//! Process lifecycle: one server per data directory, started in the background
//! with `start`, stopped cleanly with `stop`, inspected with `status`.
//!
//! - `sazare.lock` in the data directory is held (exclusively) for as long as a
//!   server runs, so a second server on the same data can't start. The OS drops
//!   the lock when the process ends, however it ends.
//! - `sazare.run.json` (readable only by the owner) says where the running
//!   server is: its URL, pid, and a loopback control port with a random token.
//!   `stop` asks for a graceful shutdown through that port; nothing is added to
//!   the public port.
//! - A background server logs to `logs/sazare.log` in the data directory.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sazare_server::config::ServerConfig;

/// Where a data directory's lock, run info and log live.
pub struct Paths {
    pub lock: PathBuf,
    pub run: PathBuf,
    pub log: PathBuf,
}

impl Paths {
    pub fn new(config: &ServerConfig) -> Self {
        let dir = &config.storage.data_dir;
        Self {
            lock: dir.join("sazare.lock"),
            run: dir.join("sazare.run.json"),
            log: dir.join("logs").join("sazare.log"),
        }
    }
}

/// What a running server publishes about itself.
#[derive(Serialize, Deserialize)]
pub struct RunInfo {
    pub pid: u32,
    pub url: String,
    /// Loopback `host:port` of the control listener.
    pub control: String,
    pub token: String,
    pub version: String,
    pub started: String,
}

/// The instance lock; held while the server runs.
pub struct InstanceLock {
    _file: std::fs::File,
}

/// Take the data directory's instance lock, or `None` if another server holds it.
pub fn try_lock(path: &Path) -> std::io::Result<Option<InstanceLock>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
        // SAFETY: flock on a file descriptor we own.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            return if err.kind() == std::io::ErrorKind::WouldBlock { Ok(None) } else { Err(err) };
        }
        Ok(Some(InstanceLock { _file: file }))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // No sharing: while one process has it open, nobody else can open it.
        match std::fs::OpenOptions::new().create(true).truncate(false).write(true).share_mode(0).open(path) {
            Ok(file) => Ok(Some(InstanceLock { _file: file })),
            Err(e) if e.raw_os_error() == Some(32) => Ok(None), // ERROR_SHARING_VIOLATION
            Err(e) => Err(e),
        }
    }
}

/// Whether a server holds the lock (probes it without keeping it). A data
/// directory that was never served has no lock file, and looking must not
/// create one (nor the directory).
pub fn is_running(paths: &Paths) -> bool {
    paths.lock.exists() && matches!(try_lock(&paths.lock), Ok(None))
}

fn read_run_info(paths: &Paths) -> Option<RunInfo> {
    let text = std::fs::read_to_string(&paths.run).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write `sazare.run.json` atomically, readable only by the owner (it holds the
/// control token). Removed again when the returned guard drops.
pub fn publish_run_info(paths: &Paths, info: &RunInfo) -> std::io::Result<RunInfoGuard> {
    let tmp = paths.run.with_extension("json.tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(serde_json::to_string_pretty(info).unwrap_or_default().as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &paths.run)?;
    Ok(RunInfoGuard { path: paths.run.clone() })
}

pub struct RunInfoGuard {
    path: PathBuf,
}

impl Drop for RunInfoGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A fresh random token for the control port.
pub fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Listen on a loopback port for `POST /shutdown` carrying the token; notify
/// `shutdown` when it comes. Returns the bound address.
pub async fn serve_control(token: String, shutdown: Arc<tokio::sync::Notify>) -> std::io::Result<SocketAddr> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else { continue };
            let token = token.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                // Read up to the end of the headers: a request can arrive in
                // several segments, and judging the first one alone refused a
                // stop whose token header hadn't arrived yet.
                let mut buf = Vec::with_capacity(1024);
                let read_headers = async {
                    let mut chunk = [0u8; 1024];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                };
                if tokio::time::timeout(Duration::from_secs(5), read_headers).await.is_err() {
                    return;
                }
                let request = String::from_utf8_lossy(&buf);
                let authorized = request.starts_with("POST /shutdown ")
                    && request.lines().any(|l| {
                        l.split_once(':').is_some_and(|(k, v)| {
                            k.trim().eq_ignore_ascii_case("x-sazare-token") && ct_eq(v.trim(), &token)
                        })
                    });
                let reply: &[u8] = if authorized {
                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                let _ = stream.write_all(reply).await;
                if authorized {
                    tracing::info!("Stop requested (sazare-server stop)");
                    shutdown.notify_one();
                }
            });
        }
    });
    Ok(addr)
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// `sazare-server start`: run the server in the background and return once it
/// accepts connections. Returns the exit code.
pub fn start(config: &ServerConfig, forwarded: &[String], open: bool) -> i32 {
    let paths = Paths::new(config);
    if is_running(&paths) {
        match read_run_info(&paths) {
            Some(info) => {
                println!("sazare is already running at {} (pid {})", info.url, info.pid);
                if open {
                    crate::open_browser(&info.url);
                }
            }
            None => println!("sazare is already running on {}", config.storage.data_dir.display()),
        }
        return 0;
    }

    let log = match open_log(&paths.log) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Cannot open log {}: {e}", paths.log.display());
            return 1;
        }
    };
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Cannot find the sazare-server executable: {e}");
            return 1;
        }
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run")
        .args(forwarded)
        // The server writes its log there itself, turning it over as it grows
        // (stdout / stderr stay on the file for anything printed directly).
        .env(LOG_FILE_ENV, &paths.log)
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().map(std::process::Stdio::from).unwrap_or_else(|_| std::process::Stdio::null()))
        .stderr(std::process::Stdio::from(log));
    detach(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to start: {e}");
            return 1;
        }
    };

    // Ready = the child published its run info (written after it bound its port).
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            eprintln!("sazare failed to start ({status}). Last lines of {}:", paths.log.display());
            print_tail(&paths.log, 20);
            return 1;
        }
        if let Some(info) = read_run_info(&paths)
            && info.pid == child.id()
        {
            println!("sazare is running at {} (pid {})", info.url, info.pid);
            println!("  log:  {}", paths.log.display());
            println!("  stop: sazare-server stop{}", config_hint(forwarded));
            if open {
                crate::open_browser(&info.url);
            }
            return 0;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!(
        "sazare (pid {}) is still starting; follow {} — `sazare-server status` shows when it is up",
        child.id(),
        paths.log.display()
    );
    0
}

/// `sazare-server stop`: ask the running server to shut down and wait for it.
pub fn stop(config: &ServerConfig) -> i32 {
    let paths = Paths::new(config);
    if !is_running(&paths) {
        let _ = std::fs::remove_file(&paths.run); // stale, from a crash
        println!("sazare is not running on {}", config.storage.data_dir.display());
        return 0;
    }
    let info = read_run_info(&paths);
    let asked = info.as_ref().is_some_and(|i| request_shutdown(i).is_ok());
    if asked {
        print!("Stopping sazare");
        let _ = std::io::stdout().flush();
        if wait_until_stopped(&paths, Duration::from_secs(60)) {
            println!(" — stopped");
            return 0;
        }
        println!();
    }
    // No answer from the control port: fall back to the pid.
    let Some(info) = info else {
        eprintln!("sazare is still starting (it has not published its run info yet); try again in a moment");
        return 1;
    };
    eprintln!("sazare (pid {}) did not stop when asked; terminating it", info.pid);
    terminate(info.pid);
    if wait_until_stopped(&paths, Duration::from_secs(15)) {
        println!("stopped");
        0
    } else {
        eprintln!("sazare (pid {}) is still running", info.pid);
        1
    }
}

/// `sazare-server status`: 0 when running, 3 when not (the LSB convention).
pub fn status(config: &ServerConfig) -> i32 {
    let paths = Paths::new(config);
    if !is_running(&paths) {
        println!("sazare is not running ({})", config.storage.data_dir.display());
        return 3;
    }
    match read_run_info(&paths) {
        Some(info) => {
            println!("sazare {} is running at {}", info.version, info.url);
            println!("  pid:     {}", info.pid);
            println!("  since:   {}", info.started);
            println!("  data:    {}", config.storage.data_dir.display());
            println!("  log:     {}", paths.log.display());
        }
        None => println!("sazare is running ({}), still starting", config.storage.data_dir.display()),
    }
    0
}

fn request_shutdown(info: &RunInfo) -> std::io::Result<()> {
    let addr: SocketAddr = info
        .control
        .parse()
        .map_err(|_| std::io::Error::other("bad control address"))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    // One write: the whole request in one piece (the server reads to the end of
    // the headers anyway).
    let request = format!(
        "POST /shutdown HTTP/1.1\r\nHost: {}\r\nX-Sazare-Token: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        info.control, info.token
    );
    s.write_all(request.as_bytes())?;
    let mut reply = String::new();
    let _ = s.read_to_string(&mut reply);
    if reply.starts_with("HTTP/1.1 202") {
        Ok(())
    } else {
        Err(std::io::Error::other("refused"))
    }
}

fn wait_until_stopped(paths: &Paths, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_running(paths) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
        print!(".");
        let _ = std::io::stdout().flush();
    }
    false
}

fn terminate(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: plain kill(2).
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).status();
    }
}

/// Detach the child from this terminal: Ctrl+C or closing the window here
/// must not reach it.
fn detach(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe and runs in the child before exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}

/// Append to the log, keeping one previous file once it passes 50 MB.
fn open_log(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_MAX) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path)
}

/// The log file a server writes itself, turned over as it grows: set by
/// `start` for the server it runs, or by hand for `run` under a service
/// manager that would otherwise keep stdout in one growing file.
pub const LOG_FILE_ENV: &str = "SAZARE_LOG_FILE";
/// A log file this large is turned over (`.log.1`, the one before replaced).
const LOG_MAX: u64 = 50 * 1024 * 1024;

/// The log of a background server: appended to, and turned over when it
/// reaches [`LOG_MAX`] — so a server that runs for months keeps at most two
/// files' worth, not one that grows without end.
pub struct RotatingLog {
    path: std::path::PathBuf,
    max: u64,
    file: std::sync::Mutex<(std::fs::File, u64)>,
}

impl RotatingLog {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Self::with_max(path, LOG_MAX)
    }

    fn with_max(path: &Path, max: u64) -> std::io::Result<Self> {
        let file = open_log(path)?;
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self { path: path.to_path_buf(), max, file: std::sync::Mutex::new((file, len)) })
    }

    fn turn_over(&self, file: &mut (std::fs::File, u64)) -> std::io::Result<()> {
        let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        *file = (std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?, 0);
        Ok(())
    }
}

impl std::io::Write for &RotatingLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if file.1 > 0 && file.1 + buf.len() as u64 > self.max {
            self.turn_over(&mut file)?;
        }
        file.0.write_all(buf)?;
        file.1 += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.lock().unwrap_or_else(|e| e.into_inner()).0.flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RotatingLog {
    type Writer = &'a RotatingLog;
    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

fn print_tail(path: &Path, lines: usize) {
    if let Ok(text) = std::fs::read_to_string(path) {
        let all: Vec<&str> = text.lines().collect();
        for l in &all[all.len().saturating_sub(lines)..] {
            eprintln!("  {l}");
        }
    }
}

fn config_hint(forwarded: &[String]) -> String {
    forwarded
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| forwarded.get(i + 1))
        .map(|p| format!(" --config {p}"))
        .or_else(|| forwarded.iter().find(|a| a.starts_with("--config=")).map(|a| format!(" {a}")))
        .unwrap_or_default()
}

/// `sazare-server logs [-n <lines>] [-f]`: the background server's log.
pub fn logs(config: &ServerConfig, args: &[String]) -> i32 {
    let paths = Paths::new(config);
    let follow = args.iter().any(|a| a == "-f" || a == "--follow");
    let lines = args
        .iter()
        .position(|a| a == "-n")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(50usize);
    let Ok(text) = std::fs::read_to_string(&paths.log) else {
        eprintln!("No log at {} (the server logs there when started with `start`).", paths.log.display());
        return 1;
    };
    let all: Vec<&str> = text.lines().collect();
    for l in &all[all.len().saturating_sub(lines)..] {
        println!("{l}");
    }
    if !follow {
        return 0;
    }
    // Follow: poll for appended bytes; start over if the file was rotated.
    let mut pos = text.len() as u64;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let Ok(meta) = std::fs::metadata(&paths.log) else { continue };
        if meta.len() < pos {
            pos = 0;
        }
        if meta.len() > pos
            && let Ok(mut f) = std::fs::File::open(&paths.log)
        {
            use std::io::Seek;
            if f.seek(std::io::SeekFrom::Start(pos)).is_ok() {
                let mut chunk = String::new();
                if f.read_to_string(&mut chunk).is_ok() {
                    print!("{chunk}");
                    let _ = std::io::stdout().flush();
                    pos += chunk.len() as u64;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stop request split into small pieces with pauses between them (as a
    /// slow or busy client may send it) is still accepted; a wrong token isn't.
    #[tokio::test]
    async fn control_accepts_a_request_in_pieces() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let stop = Arc::new(tokio::sync::Notify::new());
        let addr = serve_control("tok123".into(), stop.clone()).await.unwrap();
        let send = |token: &'static str| async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = format!("POST /shutdown HTTP/1.1\r\nHost: x\r\nX-Sazare-Token: {token}\r\n\r\n");
            for piece in req.as_bytes().chunks(7) {
                s.write_all(piece).await.unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let mut reply = String::new();
            let _ = s.read_to_string(&mut reply).await;
            reply
        };
        assert!(send("wrong").await.starts_with("HTTP/1.1 403"));
        assert!(send("tok123").await.starts_with("HTTP/1.1 202"));
        tokio::time::timeout(Duration::from_secs(2), stop.notified()).await.expect("stop notified");
    }
}

#[cfg(test)]
mod rotating_log_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn the_log_is_turned_over_as_it_grows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs/sazare.log");
        let log = RotatingLog::with_max(&path, 100).unwrap();
        let line = [b'x'; 39];
        for _ in 0..10 {
            (&log).write_all(&line).unwrap();
        }
        let now = std::fs::metadata(&path).unwrap().len();
        let before = std::fs::metadata(path.with_extension("log.1")).unwrap().len();
        assert!(now <= 100 && before <= 100, "{now} {before}");
        assert_eq!((now + before) % 39, 0, "whole writes in each file");
        // Only two files, ever.
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 2);
    }
}
