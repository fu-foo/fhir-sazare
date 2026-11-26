//! HTTPS: a client that opens a connection and never completes the TLS
//! handshake must not stop other clients from connecting.

use sazare_core::{
    compartment::CompartmentDef,
    validation::{ProfileRegistry, TerminologyRegistry},
    SearchParamRegistry,
};
use sazare_server::{build_router, config::ServerConfig, AppState};
use sazare_store::{AuditLog, SearchIndex, SqliteStore};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Mutex;

async fn start_https() -> (String, TempDir) {
    let dir = TempDir::new().unwrap();
    let state = Arc::new(AppState {
        store: SqliteStore::open(dir.path().join("r.sqlite")).unwrap(),
        index: Mutex::new(SearchIndex::open(dir.path().join("i.sqlite")).unwrap()),
        index_readers: sazare_server::Readers::new(sazare_store::IndexReaders::open(dir.path().join("i.sqlite"), 2).unwrap()),
        index_file: std::sync::RwLock::new(dir.path().join("i.sqlite")),
        write_gate: Default::default(),
        rebuild: Default::default(),
        audit: Arc::new(Mutex::new(AuditLog::open(dir.path().join("a.sqlite")).unwrap())),
        config: ServerConfig::default(),
        profile_registry: ProfileRegistry::new(),
        terminology_registry: TerminologyRegistry::new(),
        search_param_registry: SearchParamRegistry::new(),
        compartment_def: CompartmentDef::patient_compartment(),
        jwk_cache: tokio::sync::RwLock::new(sazare_server::auth::JwkCache::new()),
        app_names: Vec::new(),
        ws_registry: Arc::new(sazare_server::websocket::WsRegistry::new()),
        webhook: Arc::new(sazare_server::webhook::WebhookManager::new(Default::default())),
        export_jobs: Arc::new(sazare_server::bulk_export::ExportJobs::new()),
        seen_jti: std::sync::Mutex::new(std::collections::HashMap::new()),
    });
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let acceptor = sazare_server::tls::load_tls_acceptor(
        &format!("{fixtures}/test-cert.pem"),
        &format!("{fixtures}/test-key.pem"),
    )
    .unwrap();
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    let listener = sazare_server::tls::TlsListener::new(tcp, acceptor);
    let app = build_router(state).layer(axum::middleware::from_fn(sazare_server::tls::propagate_connect_info));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<sazare_server::tls::TlsConnectInfo>(),
        )
        .await
        .unwrap();
    });
    (format!("https://localhost:{}", addr.port()), dir)
}

#[tokio::test]
async fn a_stalled_handshake_does_not_block_other_clients() {
    let (base, _dir) = start_https().await;
    let port = base.rsplit(':').next().unwrap().to_string();

    // Open TCP connections and send nothing: each would sit in the handshake.
    let mut stalled = Vec::new();
    for _ in 0..5 {
        stalled.push(tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await.unwrap());
    }

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .resolve("localhost", format!("127.0.0.1:{port}").parse().unwrap())
        .build()
        .unwrap();
    let resp = tokio::time::timeout(Duration::from_secs(5), client.get(format!("{base}/health")).send())
        .await
        .expect("a real client is served while others stall")
        .unwrap();
    assert_eq!(resp.status(), 200);
    drop(stalled);
}

#[tokio::test]
async fn dropping_the_listener_releases_the_port() {
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let acceptor = sazare_server::tls::load_tls_acceptor(
        &format!("{fixtures}/test-cert.pem"),
        &format!("{fixtures}/test-key.pem"),
    )
    .unwrap();
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    drop(sazare_server::tls::TlsListener::new(tcp, acceptor));
    // The accept task notices and closes the socket; binding again succeeds.
    for _ in 0..50 {
        if tokio::net::TcpListener::bind(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("port {addr} still held after the listener was dropped");
}
