//! Searches run on read-only index connections: a writer holding the index
//! (a long import or rebuild batch) doesn't stop them.

use sazare_core::{
    compartment::CompartmentDef,
    validation::{ProfileRegistry, TerminologyRegistry},
    SearchParamRegistry,
};
use sazare_server::{build_router, config::ServerConfig, AppState};
use sazare_store::{AuditLog, IndexReaders, SearchIndex, SqliteStore};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Mutex;

async fn start() -> (String, Arc<AppState>, TempDir) {
    let dir = TempDir::new().unwrap();
    let state = Arc::new(AppState {
        store: SqliteStore::open(dir.path().join("r.sqlite")).unwrap(),
        index: Mutex::new(SearchIndex::open(dir.path().join("i.sqlite")).unwrap()),
        index_readers: IndexReaders::open(dir.path().join("i.sqlite"), 2).unwrap(),
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
    let app = build_router(state.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
    });
    (format!("http://{addr}"), state, dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn searches_do_not_wait_for_the_writer() {
    let (base, state, _dir) = start().await;
    let c = reqwest::Client::new();
    let r = c.put(format!("{base}/Patient/p1")).header("Content-Type", "application/fhir+json")
        .body(json!({"resourceType": "Patient", "id": "p1", "name": [{"family": "Okafor"}]}).to_string())
        .send().await.unwrap();
    assert_eq!(r.status(), 201);

    // The writer is busy (as during a large $import batch)...
    let writer = state.index.lock().await;
    // ...searches still answer, from a reader.
    let search = c.get(format!("{base}/Patient?family=okafor")).send();
    let r = tokio::time::timeout(Duration::from_secs(3), search).await.expect("search waited for the writer").unwrap();
    let b: Value = r.json().await.unwrap();
    assert_eq!(b["total"], 1);
    let count = c.get(format!("{base}/Patient?_summary=count")).send();
    let b: Value = tokio::time::timeout(Duration::from_secs(3), count).await.expect("count waited").unwrap().json().await.unwrap();
    assert_eq!(b["total"], 1);
    let everything = c.get(format!("{base}/Patient/p1/$everything")).send();
    assert_eq!(tokio::time::timeout(Duration::from_secs(3), everything).await.expect("$everything waited").unwrap().status(), 200);
    drop(writer);
}
