//! A crash between the store and the index write (they are separate
//! databases): the next start puts the index back in step with the store.
//! The two half-done writes are made by hand on a stopped server's files.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_sazare-server");

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Server {
    child: Child,
    base: String,
}

/// A server is never left running by a test that fails half way.
impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

// The child is waited for by `stop` or the test (after a kill).
#[allow(clippy::zombie_processes)]
async fn start(dir: &Path, port: u16) -> Server {
    let cfg = dir.join("config.yaml");
    std::fs::write(
        &cfg,
        format!(
            "server:\n  host: 127.0.0.1\n  port: {port}\nauth:\n  enabled: false\nstorage:\n  data_dir: {}\nlog:\n  level: info\n",
            dir.join("data").display()
        ),
    )
    .unwrap();
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("server.log")).unwrap();
    let child = Command::new(BIN)
        .args(["run", "--config", cfg.to_str().unwrap()])
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    for _ in 0..200 {
        if reqwest::get(format!("{base}/health")).await.is_ok() {
            return Server { child, base };
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("server did not start");
}

fn stop(dir: &Path, mut s: Server) {
    let cfg = dir.join("config.yaml");
    let out = Command::new(BIN).args(["stop", "--config", cfg.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success(), "stop: {}", String::from_utf8_lossy(&out.stderr));
    for _ in 0..200 {
        if s.child.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = s.child.kill();
    let _ = s.child.wait();
    panic!("server did not stop");
}

async fn put(base: &str, id: &str, family: &str) {
    let r = reqwest::Client::new()
        .put(format!("{base}/Patient/{id}"))
        .header("Content-Type", "application/fhir+json")
        .body(json!({"resourceType": "Patient", "id": id, "name": [{"family": family}]}).to_string())
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
}

async fn total(base: &str, q: &str) -> u64 {
    let b: Value = reqwest::get(format!("{base}/Patient?{q}")).await.unwrap().json().await.unwrap();
    b["total"].as_u64().unwrap()
}

fn patient(id: &str, family: &str) -> String {
    json!({"resourceType": "Patient", "id": id, "meta": {"versionId": "9", "lastUpdated": "2026-01-01T00:00:00Z"},
           "name": [{"family": family}]})
    .to_string()
}

#[tokio::test]
async fn a_restart_after_a_crash_puts_the_index_back_in_step_with_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let store = dir.path().join("data/resources.sqlite");

    let s = start(dir.path(), port).await;
    put(&s.base, "a", "Alpha").await;
    put(&s.base, "b", "Bravo").await;
    stop(dir.path(), s);

    // Store writes whose index step never ran (the index is as before).
    {
        let db = rusqlite::Connection::open(&store).unwrap();
        db.execute("UPDATE resources SET value = ?1 WHERE resource_type = 'Patient' AND id = 'a'", [patient("a", "Changed")])
            .unwrap();
        db.execute("DELETE FROM resources WHERE resource_type = 'Patient' AND id = 'b'", []).unwrap();
        db.execute("INSERT INTO resources (resource_type, id, value) VALUES ('Patient', 'c', ?1)", [patient("c", "Charlie")])
            .unwrap();
    }
    let s = start(dir.path(), port).await;
    assert_eq!(total(&s.base, "family=Changed").await, 1);
    assert_eq!(total(&s.base, "family=Alpha").await, 0, "the old values are gone");
    assert_eq!(total(&s.base, "_id=b").await, 0, "a deleted resource is gone");
    assert_eq!(total(&s.base, "family=Charlie").await, 1);
    assert_eq!(total(&s.base, "").await, 2);

    // An index write whose store step never ran: written, then the process
    // dies before the minute is up, and the store never had it.
    put(&s.base, "d", "Delta").await;
    let mut s = s;
    s.child.kill().unwrap();
    s.child.wait().unwrap();
    drop(s);
    {
        let db = rusqlite::Connection::open(&store).unwrap();
        db.execute("DELETE FROM resources WHERE resource_type = 'Patient' AND id = 'd'", []).unwrap();
        db.execute("DELETE FROM resource_history WHERE resource_type = 'Patient' AND id = 'd'", []).unwrap();
        db.execute("DELETE FROM changes WHERE id = 'd'", []).unwrap();
    }
    let s = start(dir.path(), port).await;
    assert_eq!(total(&s.base, "family=Delta").await, 0, "the index no longer has it");
    assert_eq!(total(&s.base, "").await, 2);

    // A clean stop leaves nothing to catch up on.
    stop(dir.path(), s);
    let log_before = std::fs::read_to_string(dir.path().join("server.log")).unwrap();
    let s = start(dir.path(), port).await;
    let log = std::fs::read_to_string(dir.path().join("server.log")).unwrap();
    let after_start = &log[log_before.len()..];
    assert!(!after_start.contains("indexed again from the store"), "{after_start}");
    stop(dir.path(), s);
    // The two crashes did catch up.
    assert_eq!(log_before.matches("indexed again from the store").count(), 2, "{log_before}");
}
