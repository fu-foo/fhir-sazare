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
  basic_auth:
    - username: ops
      password: ops-password
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

fn subscription(endpoint: &str) -> Value {
    json!({
        "resourceType": "Subscription", "status": "requested", "reason": "test",
        "criteria": "Observation?",
        "channel": {"type": "rest-hook", "endpoint": endpoint, "payload": "application/fhir+json"}
    })
}

#[tokio::test]
async fn patient_tokens_cannot_create_subscriptions() {
    let (c, t, _d) = two_patients().await;
    let sub = subscription("https://example.org/hook");
    let (s, _) = c.send(reqwest::Method::POST, "/Subscription", &t, Some(sub.clone())).await;
    assert_eq!(s, 403);
    let mut with_id = sub.clone();
    with_id["id"] = json!("s1");
    assert_eq!(c.put("/Subscription/s1", &t, with_id).await, 403);
    // A system-scoped client may.
    let system = jwt("system/*.read system/*.write", None);
    let (s, _) = c.send(reqwest::Method::POST, "/Subscription", &system, Some(sub)).await;
    assert_eq!(s, 201);
}

#[tokio::test]
async fn subscription_endpoint_must_be_http_url() {
    let (c, _t, _d) = two_patients().await;
    for bad in ["file:///etc/passwd", "gopher://x/", "not a url", "/relative"] {
        let (s, _) = c.send(reqwest::Method::POST, "/Subscription", ADMIN_KEY, Some(subscription(bad))).await;
        assert_eq!(s, 400, "{bad}");
    }
}

#[tokio::test]
async fn admin_operations_require_an_operator() {
    let (c, t, _d) = two_patients().await;
    for path in ["/$reindex", "/$demo"] {
        assert_eq!(c.send(reqwest::Method::POST, path, &t, None).await.0, 403, "{path} patient");
        let user = jwt("user/*.read user/*.write", None);
        assert_eq!(c.send(reqwest::Method::POST, path, &user, None).await.0, 403, "{path} user");
    }
    let system = jwt("system/*.write", None);
    assert_eq!(c.send(reqwest::Method::POST, "/$reindex", &system, None).await.0, 200);
    assert_eq!(c.send(reqwest::Method::POST, "/$reindex", ADMIN_KEY, None).await.0, 200);
}

#[tokio::test]
async fn cross_site_basic_auth_writes_are_refused() {
    let (c, _t, _d) = two_patients().await;
    let post = |site: Option<&'static str>| {
        let mut r = c
            .http
            .post(format!("{}/$reindex", c.base))
            .basic_auth("ops", Some("ops-password"));
        if let Some(s) = site {
            r = r.header("Sec-Fetch-Site", s);
        }
        r.send()
    };
    assert_eq!(post(Some("cross-site")).await.unwrap().status().as_u16(), 403);
    assert_eq!(post(Some("same-origin")).await.unwrap().status().as_u16(), 200);
    assert_eq!(post(None).await.unwrap().status().as_u16(), 200); // curl & co.
    // Reads are unaffected.
    let r = c.http.get(format!("{}/Patient/p1", c.base)).basic_auth("ops", Some("ops-password"))
        .header("Sec-Fetch-Site", "cross-site").send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
}

fn bundle(kind: &str, entries: Vec<Value>) -> Value {
    json!({"resourceType": "Bundle", "type": kind, "entry": entries})
}

#[tokio::test]
async fn bundles_accept_api_credentials_and_authorize_each_entry() {
    let (c, t, _d) = two_patients().await;
    let get = |id: &str| json!({"request": {"method": "GET", "url": format!("Observation/{id}")}});
    let post_obs = |subject: &str| json!({
        "resource": {"resourceType": "Observation", "status": "final", "code": {"text": "x"},
                     "subject": {"reference": subject}},
        "request": {"method": "POST", "url": "Observation"}
    });

    // Bearer credentials work on POST / (it used to accept only Basic).
    let (s, b) = c.send(reqwest::Method::POST, "/", ADMIN_KEY, Some(bundle("batch", vec![get("o-p2")]))).await;
    assert_eq!(s, 200, "{b}");

    // Patient token: own data passes, anything else refuses the whole Bundle.
    let ok = |b: Value| async { c.send(reqwest::Method::POST, "/", &t, Some(b)).await.0 };
    assert_eq!(ok(bundle("batch", vec![get("o-p1")])).await, 200);
    assert_eq!(ok(bundle("batch", vec![get("o-p1"), get("o-p2")])).await, 403);
    assert_eq!(ok(bundle("transaction", vec![post_obs("Patient/p1")])).await, 200);
    assert_eq!(ok(bundle("transaction", vec![post_obs("Patient/p2")])).await, 403);
    let del = json!({"request": {"method": "DELETE", "url": "Observation/o-p2"}});
    assert_eq!(ok(bundle("batch", vec![del])).await, 403);
    let sub = json!({"resource": subscription("https://example.org/h"), "request": {"method": "POST", "url": "Subscription"}});
    assert_eq!(ok(bundle("transaction", vec![sub])).await, 403);
    // Scopes apply per entry too.
    let read_only = jwt("patient/Observation.read", Some("p1"));
    let (s, _) = c.send(reqwest::Method::POST, "/", &read_only,
        Some(bundle("transaction", vec![post_obs("Patient/p1")]))).await;
    assert_eq!(s, 403);
    // p2's Observation still exists.
    assert_eq!(c.get("/Observation/o-p2", ADMIN_KEY).await.0, 200);
}
