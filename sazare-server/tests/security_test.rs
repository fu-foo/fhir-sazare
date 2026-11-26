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
    let app_names = sazare_server::apps::discover_app_names(&config);
    let state = Arc::new(AppState {
        store: SqliteStore::open(dir.path().join("r.sqlite")).unwrap(),
        index: Mutex::new(SearchIndex::open(dir.path().join("i.sqlite")).unwrap()),
        index_readers: sazare_server::Readers::new(sazare_store::IndexReaders::open(dir.path().join("i.sqlite"), 2).unwrap()),
        index_file: std::sync::RwLock::new(dir.path().join("i.sqlite")),
        write_gate: Default::default(),
        rebuild: Default::default(),
        audit: Arc::new(Mutex::new(AuditLog::open(dir.path().join("a.sqlite")).unwrap())),
        config,
        profile_registry: ProfileRegistry::new(),
        terminology_registry: TerminologyRegistry::new(),
        search_param_registry: SearchParamRegistry::new(),
        compartment_def: CompartmentDef::patient_compartment(),
        jwk_cache: tokio::sync::RwLock::new(sazare_server::auth::JwkCache::new()),
        app_names,
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
    // Accepted: a background rebuild starts (202), or one already runs (409).
    let accepted = |code: u16| code == 202 || code == 409;
    assert!(accepted(c.send(reqwest::Method::POST, "/$reindex", &system, None).await.0));
    // Metrics reveal resource counts: operators only.
    assert_eq!(c.get("/metrics", &t).await.0, 403);
    assert_eq!(c.get("/metrics", ADMIN_KEY).await.0, 200);
    assert!(accepted(c.send(reqwest::Method::POST, "/$reindex", ADMIN_KEY, None).await.0));
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
    // Accepted: 202 (rebuild started) or 409 (one already runs).
    let accepted = |code: u16| code == 202 || code == 409;
    assert!(accepted(post(Some("same-origin")).await.unwrap().status().as_u16()));
    assert!(accepted(post(None).await.unwrap().status().as_u16())); // curl & co.
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

impl Client {
    async fn post(&self, path: &str, token: &str, body: Value, headers: &[(&str, &str)]) -> (u16, Value) {
        let mut req = self
            .http
            .post(format!("{}{}", self.base, path))
            .bearer_auth(token)
            .header("Content-Type", "application/fhir+json")
            .body(body.to_string());
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

fn ids_of(bundle: &Value, rt: &str) -> Vec<String> {
    bundle["entry"]
        .as_array()
        .map(|es| {
            es.iter()
                .filter(|e| e["resource"]["resourceType"] == rt)
                .map(|e| e["resource"]["id"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn conditional_create_does_not_reveal_other_patients() {
    let (c, t, _d) = two_patients().await;
    let obs = json!({"resourceType": "Observation", "status": "final", "code": {"text": "x"},
                     "subject": {"reference": "Patient/p1"}});
    // o-p2 matches, but it isn't p1's: it must not come back (nor count).
    let (status, body) = c.post("/Observation", &t, obs.clone(), &[("If-None-Exist", "_id=o-p2")]).await;
    assert_eq!(status, 201, "{body}");
    assert_ne!(body["id"], "o-p2");
    assert_eq!(body["subject"]["reference"], "Patient/p1");
    // Two matches, one of them p1's: only p1's counts → 200 with p1's own.
    let (status, body) = c.post("/Observation", &t, obs, &[("If-None-Exist", "_id=o-p1,o-p2")]).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["id"], "o-p1");
}

#[tokio::test]
async fn patient_tokens_cannot_change_resources_outside_the_compartment() {
    let (c, t, _d) = two_patients().await;
    assert_eq!(c.put("/Practitioner/dr", ADMIN_KEY, json!({"resourceType": "Practitioner", "id": "dr"})).await, 201);
    // Readable for reference resolution...
    assert_eq!(c.get("/Practitioner/dr", &t).await.0, 200);
    // ...but not writable.
    assert_eq!(c.put("/Practitioner/dr", &t, json!({"resourceType": "Practitioner", "id": "dr"})).await, 403);
    let (status, _) = c.post("/Organization", &t, json!({"resourceType": "Organization"}), &[]).await;
    assert_eq!(status, 403);
    let (status, _) = c.send(reqwest::Method::DELETE, "/Practitioner/dr", &t, None).await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn mixed_scopes_keep_the_compartment_for_patient_types() {
    let (c, _t, _d) = two_patients().await;
    let t = jwt("patient/Observation.rs user/Practitioner.rs", Some("p1"));
    assert_eq!(c.get("/Observation/o-p1", &t).await.0, 200);
    assert_eq!(c.get("/Observation/o-p2", &t).await.0, 403);
    let (_, b) = c.get("/Observation", &t).await;
    assert_eq!(ids_of(&b, "Observation"), vec!["o-p1"]);
}

#[tokio::test]
async fn mixed_permissions_on_one_type_keep_the_compartment_for_reads() {
    let (c, _t, _d) = two_patients().await;
    // Unrestricted create, but reading only within the compartment.
    let t = jwt("patient/Observation.rs user/Observation.c", Some("p1"));
    assert_eq!(c.get("/Observation/o-p1", &t).await.0, 200);
    assert_eq!(c.get("/Observation/o-p2", &t).await.0, 403);
    let (_, b) = c.get("/Observation", &t).await;
    assert_eq!(ids_of(&b, "Observation"), vec!["o-p1"]);
    // The user/ create permission does apply to creates.
    let other = json!({"resourceType": "Observation", "status": "final", "code": {"text": "x"},
                       "subject": {"reference": "Patient/p2"}});
    assert_eq!(c.post("/Observation", &t, other, &[]).await.0, 201);
}

#[tokio::test]
async fn scopes_are_read_with_context_and_v2_permissions() {
    let (c, _t, _d) = two_patients().await;
    // v2 read+search: may search with POST, may not create.
    let t = jwt("patient/Observation.rs", Some("p1"));
    let (status, b) = c.post("/Observation/_search", &t, json!({}), &[]).await;
    assert_eq!(status, 200, "{b}");
    assert_eq!(b["total"], 1);
    let obs = json!({"resourceType": "Observation", "status": "final", "code": {"text": "x"},
                     "subject": {"reference": "Patient/p1"}});
    assert_eq!(c.post("/Observation", &t, obs.clone(), &[]).await.0, 403);
    // v2 create.
    let t = jwt("patient/Observation.c", Some("p1"));
    assert_eq!(c.post("/Observation", &t, obs, &[]).await.0, 201);
    // Bulk import needs a system/ scope; user/*.write is not enough.
    let ndjson = json!({"resourceType": "Patient", "id": "bulk"}).to_string();
    let user_write = jwt("user/*.write", None);
    let resp = c.http.post(format!("{}/$import", c.base)).bearer_auth(&user_write)
        .header("Content-Type", "application/fhir+ndjson").body(ndjson.clone()).send().await.unwrap();
    assert_eq!(resp.status(), 403);
    let system_write = jwt("system/*.write", None);
    let resp = c.http.post(format!("{}/$import", c.base)).bearer_auth(&system_write)
        .header("Content-Type", "application/fhir+ndjson").body(ndjson).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    // Granular (query) scopes grant nothing rather than everything.
    let t = jwt("user/Observation.rs?category=laboratory", None);
    assert_eq!(c.get("/Observation", &t).await.0, 403);
}

#[tokio::test]
async fn includes_and_everything_respect_resource_type_scopes() {
    let (c, _t, _d) = two_patients().await;
    let t = jwt("user/Patient.rs", None);
    let (status, b) = c.get("/Patient?_id=p1&_revinclude=Observation:subject", &t).await;
    assert_eq!(status, 200);
    assert_eq!(ids_of(&b, "Patient"), vec!["p1"]);
    assert!(ids_of(&b, "Observation").is_empty(), "{b}");
    let (status, b) = c.get("/Patient/p1/$everything", &t).await;
    assert_eq!(status, 200);
    assert!(ids_of(&b, "Observation").is_empty(), "{b}");
    // With Observation scope they're there.
    let t = jwt("user/Patient.rs user/Observation.rs", None);
    let (_, b) = c.get("/Patient/p1/$everything", &t).await;
    assert_eq!(ids_of(&b, "Observation"), vec!["o-p1"]);
}

#[tokio::test]
async fn patient_search_pages_after_the_compartment_filter() {
    let (c, t, _d) = two_patients().await;
    // Other patients' Observations sort before p1's.
    for id in ["a1", "a2", "a3"] {
        assert_eq!(
            c.put(&format!("/Observation/{id}"), ADMIN_KEY, json!({
                "resourceType": "Observation", "id": id, "status": "final",
                "code": {"text": "x"}, "subject": {"reference": "Patient/p2"}
            }))
            .await,
            201
        );
    }
    let (status, b) = c.get("/Observation?_count=1", &t).await;
    assert_eq!(status, 200);
    assert_eq!(b["total"], 1);
    assert_eq!(ids_of(&b, "Observation"), vec!["o-p1"]);
    let (_, b) = c.get("/Observation?_summary=count", &t).await;
    assert_eq!(b["total"], 1);
}

#[tokio::test]
async fn patient_tokens_cannot_listen_on_the_websocket() {
    let (c, _t, _d) = two_patients().await;
    let t = jwt("patient/*.read", Some("p1"));
    // A plain GET is enough to see the refusal before any upgrade.
    assert_eq!(c.get("/ws", &t).await.0, 403);
    let t = jwt("user/Observation.read", None);
    assert_eq!(c.get("/ws", &t).await.0, 403, "needs Subscription read");
}

#[tokio::test]
async fn browse_is_for_operators_and_the_dashboard_escapes() {
    let (c, t, _d) = two_patients().await;
    // The dashboard's data endpoints take Basic credentials (operators) only.
    assert_eq!(c.get("/$browse/Patient", &t).await.0, 401);
    let resp = c.http.get(format!("{}/$browse/Patient", c.base)).basic_auth("ops", Some("ops-password"))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let html = c.http.get(format!("{}/", c.base)).basic_auth("ops", Some("ops-password")).send().await.unwrap().text().await.unwrap();
    assert!(html.contains("function esc("));
    assert!(!html.contains("onclick=\"showResource"), "no stored values in inline handlers");
}

#[tokio::test]
async fn static_apps_are_served_under_apps_only() {
    let apps = TempDir::new().unwrap();
    std::fs::create_dir(apps.path().join("demo-app")).unwrap();
    std::fs::write(apps.path().join("demo-app/index.html"), "<h1>app</h1>").unwrap();
    std::fs::write(apps.path().join("demo-app/app.js"), "1").unwrap();
    let mut config = secured_config();
    config.apps.dir = Some(apps.path().to_path_buf());
    let (base, _d) = start(config).await;
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let get = |p: &str| http.get(format!("{base}{p}")).send();

    // App files are public (no credentials), under /apps/ only.
    let r = get("/apps/demo-app/").await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), "<h1>app</h1>");
    let r = get("/apps/demo-app/app.js").await.unwrap();
    assert_eq!(r.headers()["content-type"], "application/javascript; charset=utf-8");
    // SPA fallback.
    assert_eq!(get("/apps/demo-app/some/route").await.unwrap().text().await.unwrap(), "<h1>app</h1>");
    assert_eq!(get("/apps/demo-app").await.unwrap().status(), 308);
    // Not at the top level any more; unknown apps aren't public.
    assert_eq!(get("/demo-app/").await.unwrap().status(), 401);
    assert_eq!(get("/apps/other/").await.unwrap().status(), 401);
    // Listing.
    let list: Value = get("/$apps").await.unwrap().json().await.unwrap();
    assert_eq!(list["apps"][0]["path"], "/apps/demo-app/");
    // Path traversal stays refused.
    assert_eq!(get("/apps/demo-app/..%2F..%2Fetc%2Fpasswd").await.unwrap().status(), 400);
    // A symlink inside an app is not followed.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/hostname", apps.path().join("demo-app/leak")).unwrap();
        assert_eq!(get("/apps/demo-app/leak").await.unwrap().status(), 403);
    }
    // Unknown names aren't redirected as if they existed (with credentials: 404).
    let r = http.get(format!("{base}/apps/nope")).basic_auth("ops", Some("ops-password")).send().await.unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn no_apps_unless_configured() {
    let (base, _d) = start(secured_config()).await;
    let list: Value = reqwest::get(format!("{base}/$apps")).await.unwrap().json().await.unwrap();
    assert_eq!(list["apps"], json!([]));
}

/// A patient-scoped search narrows to the compartment in the index
/// (`_compartment`): the result must be exactly what the compartment test on
/// the resource lets through — references in arrays and in other fields
/// count; a versioned or absolute reference, or another patient's, doesn't.
#[tokio::test]
async fn patient_scoped_searches_return_exactly_the_compartment() {
    let (c, t, _d) = two_patients().await;
    let put = |path: &'static str, body: Value| {
        let c = &c;
        async move { assert_eq!(c.put(path, ADMIN_KEY, body).await, 201, "{path}") }
    };
    let obs = |id: &str, subject: &str| json!({"resourceType": "Observation", "id": id, "status": "final",
        "code": {"text": "x"}, "subject": {"reference": subject}});
    put("/Observation/o-hist", obs("o-hist", "Patient/p1/_history/1")).await;
    put("/Observation/o-abs", obs("o-abs", "http://other.example/fhir/Patient/p1")).await;
    let prov = |id: &str, targets: &[&str]| json!({"resourceType": "Provenance", "id": id, "recorded": "2024-01-01T00:00:00Z",
        "target": targets.iter().map(|t| json!({"reference": t})).collect::<Vec<_>>(),
        "agent": [{"who": {"display": "x"}}]});
    put("/Provenance/prov-both", prov("prov-both", &["Patient/p2", "Patient/p1"])).await;
    put("/Provenance/prov-p2", prov("prov-p2", &["Patient/p2"])).await;
    let task = |id: &str, f: &str, owner: Option<&str>| {
        let mut t = json!({"resourceType": "Task", "id": id, "status": "requested", "intent": "order", "for": {"reference": f}});
        if let Some(o) = owner { t["owner"] = json!({"reference": o}); }
        t
    };
    put("/Task/task-owner", task("task-owner", "Patient/p2", Some("Patient/p1"))).await;
    put("/Task/task-for", task("task-for", "Patient/p2", None)).await;
    put("/Coverage/cov-sub", json!({"resourceType": "Coverage", "id": "cov-sub", "status": "active",
        "beneficiary": {"reference": "Patient/p2"}, "subscriber": {"reference": "Patient/p1"},
        "payor": [{"display": "x"}]})).await;
    put("/Practitioner/prac", json!({"resourceType": "Practitioner", "id": "prac"})).await;

    let ids = |q: &'static str| {
        let (c, t) = (&c, &t);
        async move {
            let (s, b) = c.get(q, t).await;
            assert_eq!(s, 200, "{q}: {b}");
            let mut v: Vec<String> = b["entry"].as_array().cloned().unwrap_or_default().iter()
                .map(|e| e["resource"]["id"].as_str().unwrap().to_string()).collect();
            v.sort();
            (v, b["total"].as_u64())
        }
    };
    assert_eq!(ids("/Observation").await, (vec!["o-p1".to_string()], Some(1)));
    assert_eq!(ids("/Patient").await, (vec!["p1".to_string()], Some(1)));
    assert_eq!(ids("/Provenance").await, (vec!["prov-both".to_string()], Some(1)));
    assert_eq!(ids("/Task").await, (vec!["task-owner".to_string()], Some(1)));
    assert_eq!(ids("/Coverage").await, (vec!["cov-sub".to_string()], Some(1)));
    assert_eq!(ids("/Practitioner").await, (vec!["prac".to_string()], Some(1)), "not a compartment type");
    // With other criteria, paging and counting.
    assert_eq!(ids("/Observation?status=final&_count=1").await, (vec!["o-p1".to_string()], Some(1)));
    assert_eq!(ids("/Observation?_count=1&_offset=1").await, (vec![], Some(1)));
    let (_, b) = c.get("/Observation?_summary=count", &t).await;
    assert_eq!(b["total"], 1);
    // A count is a count: not a bound (_total=none), not a page (_count).
    let (_, b) = c.get("/Observation?_summary=count&_total=none&_count=0", &t).await;
    assert_eq!(b["total"], 1);
    let (_, b) = c.get("/Observation?status=final&_summary=count&_count=1&_total=none", ADMIN_KEY).await;
    assert_eq!(b["total"], 4, "o-p1, o-p2, o-hist, o-abs");
    // Conditional create decides on what the caller may see: of the four
    // final Observations, p1 sees one — it exists (200), no 412.
    let r = c.http.post(format!("{}/Observation", c.base)).bearer_auth(&t)
        .header("Content-Type", "application/fhir+json").header("If-None-Exist", "status=final")
        .body(obs("o-new", "Patient/p1").to_string()).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200, "the caller's one match exists");
    let b: Value = r.json().await.unwrap();
    assert_eq!(b["id"], "o-p1");
    let r = c.http.post(format!("{}/Observation", c.base)).bearer_auth(ADMIN_KEY)
        .header("Content-Type", "application/fhir+json").header("If-None-Exist", "status=final")
        .body(obs("o-new", "Patient/p1").to_string()).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 412, "several matches for an unrestricted caller");
    // Another patient's token sees the other side.
    let t2 = jwt("patient/*.read", Some("p2"));
    let (_, b) = c.get("/Task", &t2).await;
    assert_eq!(b["total"], 2, "for: p2 on both tasks");
}

/// During the upgrade from 0.16 the index serving searches has no
/// compartments (`_compartment`) yet: a patient-scoped search must fall back
/// to testing each match, not find nothing.
#[tokio::test]
async fn patient_scoped_searches_on_an_index_without_compartments() {
    let (c, t, dir) = two_patients().await;
    // Make the index look like 0.16's: no format record, no compartment rows.
    let db = rusqlite::Connection::open(dir.path().join("i.sqlite")).unwrap();
    db.execute("DELETE FROM index_meta WHERE key = 'index_format'", []).unwrap();
    let removed = db
        .execute("DELETE FROM v_ref WHERE pid IN (SELECT pid FROM param WHERE name = '_compartment')", [])
        .unwrap();
    assert!(removed > 0, "the compartment rows were there");
    drop(db);
    let (s, b) = c.get("/Observation", &t).await;
    assert_eq!(s, 200);
    assert_eq!(b["total"], 1, "{b}");
    assert_eq!(b["entry"][0]["resource"]["id"], "o-p1");
    let (_, b) = c.get("/Observation?_summary=count", &t).await;
    assert_eq!(b["total"], 1);
    // Quantities aren't in such an index either: 503, not an empty result.
    let (s, _) = c.get("/Observation?value-quantity=gt1", ADMIN_KEY).await;
    assert_eq!(s, 503);
}
