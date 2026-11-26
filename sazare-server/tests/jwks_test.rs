//! JWT validation against a JWKS URL: a token naming a `kid` is verified with
//! exactly that key, never with whichever key happens to come first.

use sazare_core::{
    compartment::CompartmentDef,
    validation::{ProfileRegistry, TerminologyRegistry},
    SearchParamRegistry,
};
use sazare_server::{build_router, config::ServerConfig, AppState};
use sazare_store::{AuditLog, SearchIndex, SqliteStore};
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

const X: &str = "kUcy4sJjX12bh8URwgT8u4DW09AHvQAkT0rtNFqobDg";
const Y: &str = "q1pDYZ7v7KCO1NdeQY-4Fu7kqK1o2FjkoUTWT1crJg0";

async fn serve_jwks() -> String {
    let jwks = json!({"keys": [{"kty": "EC", "crv": "P-256", "x": X, "y": Y, "kid": "k1", "alg": "ES256", "use": "sig"}]});
    let app = axum::Router::new().route("/jwks", axum::routing::get(move || async move { axum::Json(jwks) }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}/jwks")
}

async fn start(jwk_url: &str) -> (String, TempDir) {
    let dir = TempDir::new().unwrap();
    let config: ServerConfig = serde_yaml::from_str(&format!(
        "auth:\n  enabled: true\n  jwt:\n    jwk_url: \"{jwk_url}\"\n"
    ))
    .unwrap();
    let state = Arc::new(AppState {
        store: SqliteStore::open(dir.path().join("r.sqlite")).unwrap(),
        index: Mutex::new(SearchIndex::open(dir.path().join("i.sqlite")).unwrap()),
        index_readers: sazare_store::IndexReaders::open(dir.path().join("i.sqlite"), 2).unwrap(),
        audit: Arc::new(Mutex::new(AuditLog::open(dir.path().join("a.sqlite")).unwrap())),
        config,
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
    let app = build_router(state);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await
            .unwrap();
    });
    (format!("http://{addr}"), dir)
}

fn token(kid: Option<&str>) -> String {
    let pem = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/test-key.pem")).unwrap();
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
    header.kid = kid.map(str::to_string);
    let exp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 600;
    jsonwebtoken::encode(
        &header,
        &json!({"sub": "app", "scope": "user/*.rs", "exp": exp}),
        &jsonwebtoken::EncodingKey::from_ec_pem(&pem).unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn tokens_are_verified_with_the_key_they_name() {
    let jwks = serve_jwks().await;
    let (base, _dir) = start(&jwks).await;
    let c = reqwest::Client::new();
    let status = |t: String| {
        let c = c.clone();
        let url = format!("{base}/Patient");
        async move { c.get(url).bearer_auth(t).send().await.unwrap().status().as_u16() }
    };
    assert_eq!(status(token(Some("k1"))).await, 200);
    // Same signing key, but the token names a kid the JWKS doesn't have.
    assert_eq!(status(token(Some("rotated-away"))).await, 401);
    // No kid: the set's single key is used.
    assert_eq!(status(token(None)).await, 200);
}
