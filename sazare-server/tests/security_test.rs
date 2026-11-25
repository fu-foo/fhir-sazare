//! Authorization regression tests: a server with auth enabled, an admin API
//! key, and SMART-style JWTs (HS256) — patient-scoped and system-scoped.

use sazare_core::{
    compartment::CompartmentDef,
    validation::{ProfileRegistry, TerminologyRegistry},
    SearchParamRegistry,
};
use sazare_server::{build_router, config::ServerConfig, AppState};
use sazare_store::{AuditLog, SearchIndex, SqliteStore};
use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

const ADMIN_KEY: &str = "admin-key-for-tests";
const JWT_SECRET: &str = "jwt-secret-for-tests";

fn secured_config() -> ServerConfig {
    serde_yaml::from_str(&format!(
        r#"
auth:
  enabled: true
  api_keys:
    - name: admin
      key: {ADMIN_KEY}
  jwt:
    secret: {JWT_SECRET}
"#
    ))
    .unwrap()
}

async fn start(config: ServerConfig) -> (String, TempDir) {
    let dir = TempDir::new().unwrap();
    let state = Arc::new(AppState {
        store: SqliteStore::open(dir.path().join("r.sqlite")).unwrap(),
        index: Mutex::new(SearchIndex::open(dir.path().join("i.sqlite")).unwrap()),
        audit: Arc::new(Mutex::new(AuditLog::open(dir.path().join("a.sqlite")).unwrap())),
        config,
        profile_registry: ProfileRegistry::new(),
        terminology_registry: TerminologyRegistry::new(),
        search_param_registry: SearchParamRegistry::new(),
        compartment_def: CompartmentDef::patient_compartment(),
        jwk_cache: tokio::sync::RwLock::new(sazare_server::auth::JwkCache::new()),
        plugin_names: Vec::new(),
        ws_registry: Arc::new(sazare_server::websocket::WsRegistry::new()),
        webhook: Arc::new(sazare_server::webhook::WebhookManager::new(Default::default())),
        export_jobs: Arc::new(sazare_server::bulk_export::ExportJobs::new()),
        seen_jti: std::sync::Mutex::new(std::collections::HashMap::new()),
    });
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await
            .unwrap();
    });
    (format!("http://{addr}"), dir)
}

fn jwt(scope: &str, patient: Option<&str>) -> String {
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    let mut claims = json!({"sub": "app", "scope": scope, "exp": exp});
    if let Some(p) = patient {
        claims["patient"] = json!(p);
    }
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .unwrap()
}

struct Client {
    http: reqwest::Client,
    base: String,
}

impl Client {
    async fn send(&self, method: reqwest::Method, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self
            .http
            .request(method, format!("{}{}", self.base, path))
            .bearer_auth(token);
        if let Some(b) = body {
            req = req.header("Content-Type", "application/fhir+json").body(b.to_string());
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
    async fn put(&self, path: &str, token: &str, body: Value) -> u16 {
        self.send(reqwest::Method::PUT, path, token, Some(body)).await.0
    }
    async fn get(&self, path: &str, token: &str) -> (u16, Value) {
        self.send(reqwest::Method::GET, path, token, None).await
    }
}

/// Two patients, each with an Observation; returns the client and a
/// patient-scoped token for p1.
async fn two_patients() -> (Client, String, TempDir) {
    let (base, dir) = start(secured_config()).await;
    let c = Client { http: reqwest::Client::new(), base };
    for p in ["p1", "p2"] {
        assert_eq!(c.put(&format!("/Patient/{p}"), ADMIN_KEY, json!({"resourceType": "Patient", "id": p})).await, 201);
        let o = format!("o-{p}");
        assert_eq!(
            c.put(&format!("/Observation/{o}"), ADMIN_KEY, json!({
                "resourceType": "Observation", "id": o, "status": "final",
                "code": {"text": "x"}, "subject": {"reference": format!("Patient/{p}")}
            }))
            .await,
            201
        );
    }
    (c, jwt("patient/*.read patient/*.write", Some("p1")), dir)
}

#[tokio::test]
async fn history_and_vread_respect_the_patient_compartment() {
    let (c, t, _d) = two_patients().await;
    assert_eq!(c.get("/Observation/o-p1/_history", &t).await.0, 200);
    assert_eq!(c.get("/Observation/o-p1/_history/1", &t).await.0, 200);
    assert_eq!(c.get("/Observation/o-p2/_history", &t).await.0, 403);
    assert_eq!(c.get("/Observation/o-p2/_history/1", &t).await.0, 403);
    // Admin is unaffected.
    assert_eq!(c.get("/Observation/o-p2/_history/1", ADMIN_KEY).await.0, 200);
}

#[tokio::test]
async fn writes_cannot_target_another_patients_compartment() {
    let (c, t, _d) = two_patients().await;
    let obs = |id: &str, subject: &str| json!({
        "resourceType": "Observation", "id": id, "status": "final",
        "code": {"text": "x"}, "subject": {"reference": subject}
    });
    // Update-as-create into p2.
    assert_eq!(c.put("/Observation/new", &t, obs("new", "Patient/p2")).await, 403);
    // Moving an own resource into p2.
    assert_eq!(c.put("/Observation/o-p1", &t, obs("o-p1", "Patient/p2")).await, 403);
    // Own resource stays writable.
    assert_eq!(c.put("/Observation/o-p1", &t, obs("o-p1", "Patient/p1")).await, 200);
    // PATCH that moves the resource out of the compartment.
    let (s, _) = c
        .http
        .patch(format!("{}/Observation/o-p1", c.base))
        .bearer_auth(&t)
        .header("Content-Type", "application/json-patch+json")
        .body(json!([{"op": "replace", "path": "/subject/reference", "value": "Patient/p2"}]).to_string())
        .send()
        .await
        .map(|r| (r.status().as_u16(), ()))
        .unwrap();
    assert_eq!(s, 403);
    // Conditional update whose match is p2's resource.
    let (s, _) = c
        .send(reqwest::Method::PUT, "/Observation?_id=o-p2", &t, Some(obs("o-p2", "Patient/p1")))
        .await;
    assert_eq!(s, 403);
    // p2's data is unchanged.
    let (_, o) = c.get("/Observation/o-p2", ADMIN_KEY).await;
    assert_eq!(o["subject"]["reference"], "Patient/p2");
}
