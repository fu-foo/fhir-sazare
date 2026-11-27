//! Stopping the server while it rebuilds the search index in the background:
//! the process must exit promptly (the build stops at its next page), not
//! when the build is done — an hour at 10M resources.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_sazare-server");

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[tokio::test]
async fn stop_during_a_background_rebuild_exits_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let cfg = dir.path().join("config.yaml");
    std::fs::write(
        &cfg,
        format!(
            "server:\n  host: 127.0.0.1\n  port: {port}\nauth:\n  enabled: false\nstorage:\n  data_dir: {}\nlog:\n  level: info\n",
            dir.path().join("data").display()
        ),
    )
    .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    let log_path = dir.path().join("server.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut server = Command::new(BIN)
        .args(["run", "--config", cfg.to_str().unwrap()])
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let mut up = false;
    for _ in 0..200 {
        if client.get(format!("{base}/health")).send().await.is_ok() {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(up, "server did not start");

    // Enough resources that a rebuild takes a while.
    for chunk in 0..8 {
        let ndjson: String = (0..5000)
            .map(|i| json!({"resourceType": "Patient", "id": format!("p{chunk}-{i}"), "name": [{"family": "Stop"}]}).to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let r = client.post(format!("{base}/$import")).header("Content-Type", "application/fhir+ndjson").body(ndjson).send().await.unwrap();
        assert_eq!(r.status(), 200);
    }
    let r = client.post(format!("{base}/$reindex")).send().await.unwrap();
    assert_eq!(r.status(), 202);
    let health: Value = client.get(format!("{base}/health")).send().await.unwrap().json().await.unwrap();
    assert_eq!(health["index"]["state"], "rebuilding", "{health}");

    let started = Instant::now();
    let stop = Command::new(BIN).args(["stop", "--config", cfg.to_str().unwrap()]).output().unwrap();
    assert!(stop.status.success(), "stop: {}", String::from_utf8_lossy(&stop.stderr));
    let mut exited = false;
    for _ in 0..200 {
        if server.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !exited {
        let _ = server.kill();
    }
    assert!(exited, "the server was still running 10 s after stop (it waited for the rebuild)");
    assert!(started.elapsed() < Duration::from_secs(10));

    // The build was stopped, not finished (a quick machine can finish a small
    // build within the deadline above; the log tells them apart).
    let log = std::fs::read_to_string(&log_path).unwrap_or_default()
        + &std::fs::read_to_string(dir.path().join("data/logs/sazare.log")).unwrap_or_default();
    assert!(log.contains("Index rebuild stopped"), "the rebuild was not stopped:\n{log}");
    assert!(!log.contains("Switched to search index generation"), "the rebuild ran to the end before exiting");

    // The unfinished generation is gone.
    let left: Vec<String> = std::fs::read_dir(dir.path().join("data"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("search_index.g"))
        .collect();
    assert!(left.is_empty(), "partial generation left: {left:?}");
}

/// A request still running at a stop gets 30 s, then the server stops
/// without it (here: a body announced and never sent).
#[tokio::test]
async fn stop_does_not_wait_for_a_request_forever() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let cfg = dir.path().join("config.yaml");
    std::fs::write(
        &cfg,
        format!(
            "server:\n  host: 127.0.0.1\n  port: {port}\nauth:\n  enabled: false\nstorage:\n  data_dir: {}\n",
            dir.path().join("data").display()
        ),
    )
    .unwrap();
    let mut server = Command::new(BIN)
        .args(["run", "--config", cfg.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let mut up = false;
    for _ in 0..200 {
        if reqwest::get(format!("{base}/health")).await.is_ok() {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(up, "server did not start");

    let mut slow = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(slow, "PUT /Patient/p HTTP/1.1\r\nHost: x\r\nContent-Type: application/fhir+json\r\nContent-Length: 100\r\n\r\n{{").unwrap();
    std::thread::sleep(Duration::from_millis(300));

    let started = Instant::now();
    let stop = std::thread::spawn({
        let cfg = cfg.clone();
        move || Command::new(BIN).args(["stop", "--config", cfg.to_str().unwrap()]).output().unwrap()
    });
    let mut exited = false;
    for _ in 0..1000 {
        if server.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !exited {
        let _ = server.kill();
        let _ = server.wait();
    }
    assert!(exited, "the server was still waiting for the request after 50 s");
    let took = started.elapsed();
    assert!(took >= Duration::from_secs(25), "the request got its grace period ({took:?})");
    assert!(stop.join().unwrap().status.success());
    drop(slow);
}
