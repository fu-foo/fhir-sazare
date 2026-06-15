//! End-to-end integration test
//!
//! Tests the full FHIR CRUD + Search flow:
//! POST (create) -> GET (read) -> GET (search) -> PUT (update) -> DELETE

use sazare_core::validation::{ProfileRegistry, TerminologyRegistry};
use sazare_core::{CompartmentDef, SearchParamRegistry};
use sazare_server::{build_router, config::ServerConfig, AppState};
use sazare_store::{AuditLog, SearchIndex, SqliteStore};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;

/// Start a test server on a random port, returns (base_url, _temp_dir)
async fn start_test_server() -> (String, TempDir) {
    let temp_dir = TempDir::new().unwrap();

    let store = SqliteStore::open(temp_dir.path().join("resources.sqlite")).unwrap();
    let index = SearchIndex::open(temp_dir.path().join("search_index.sqlite")).unwrap();
    let audit = AuditLog::open(temp_dir.path().join("audit.sqlite")).unwrap();

    let state = Arc::new(AppState {
        store,
        index: Mutex::new(index),
        audit: Arc::new(Mutex::new(audit)),
        config: ServerConfig::default(),
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
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    (format!("http://{}", addr), temp_dir)
}

/// Sink endpoint that forwards received webhook bodies to a channel.
async fn webhook_sink(
    axum::extract::State(tx): axum::extract::State<tokio::sync::mpsc::UnboundedSender<Value>>,
    axum::Json(body): axum::Json<Value>,
) -> axum::http::StatusCode {
    let _ = tx.send(body);
    axum::http::StatusCode::OK
}

#[tokio::test]
async fn test_webhook_task_completed_fires() {
    use sazare_server::config::{WebhookEndpoint, WebhookSettings};
    use tokio::sync::mpsc;

    // A sink server that records the webhook bodies it receives.
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let sink = axum::Router::new()
        .route("/sink", axum::routing::post(webhook_sink))
        .with_state(tx);
    let sink_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sink_addr = sink_listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(sink_listener, sink).await.unwrap() });
    let sink_url = format!("http://{}/sink", sink_addr);

    // A sazare server with webhooks enabled, pointing at the sink.
    let temp_dir = TempDir::new().unwrap();
    let mut config = ServerConfig::default();
    config.webhook = WebhookSettings {
        enabled: true,
        endpoints: vec![WebhookEndpoint {
            url: sink_url,
            events: vec!["TaskCompleted".to_string()],
            headers: Default::default(),
        }],
    };
    let webhook = Arc::new(sazare_server::webhook::WebhookManager::new(config.webhook.clone()));
    let state = Arc::new(AppState {
        store: SqliteStore::open(temp_dir.path().join("r.sqlite")).unwrap(),
        index: Mutex::new(SearchIndex::open(temp_dir.path().join("i.sqlite")).unwrap()),
        audit: Arc::new(Mutex::new(AuditLog::open(temp_dir.path().join("a.sqlite")).unwrap())),
        config,
        profile_registry: ProfileRegistry::new(),
        terminology_registry: TerminologyRegistry::new(),
        search_param_registry: SearchParamRegistry::new(),
        compartment_def: CompartmentDef::patient_compartment(),
        jwk_cache: tokio::sync::RwLock::new(sazare_server::auth::JwkCache::new()),
        plugin_names: Vec::new(),
        ws_registry: Arc::new(sazare_server::websocket::WsRegistry::new()),
        webhook,
        export_jobs: Arc::new(sazare_server::bulk_export::ExportJobs::new()),
        seen_jti: std::sync::Mutex::new(std::collections::HashMap::new()),
    });
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap()
    });
    let base_url = format!("http://{}", addr);

    // Creating a completed Task fires the TaskCompleted webhook.
    let client = reqwest::Client::new();
    client
        .put(format!("{}/Task/task-1", base_url))
        .json(&json!({
            "resourceType": "Task", "id": "task-1",
            "status": "completed", "intent": "order"
        }))
        .send()
        .await
        .unwrap();

    let received = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for webhook")
        .unwrap();
    assert_eq!(received["resourceType"], "Task");
    assert_eq!(received["status"], "completed");
}

#[tokio::test]
async fn test_bulk_data_async_export() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    create(&client, &base_url, "Patient", &json!({"resourceType": "Patient"})).await;
    create(
        &client,
        &base_url,
        "Observation",
        &json!({"resourceType": "Observation", "status": "final", "code": {"text": "x"}}),
    )
    .await;

    // Kick-off with Prefer: respond-async -> 202 + Content-Location.
    let resp = client
        .get(format!("{}/$export", base_url))
        .header("Prefer", "respond-async")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let status_url = resp
        .headers()
        .get("content-location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    // Poll until the manifest is ready.
    let manifest = loop {
        let resp = client.get(&status_url).send().await.unwrap();
        if resp.status() == 200 {
            break resp.json::<Value>().await.unwrap();
        }
        assert_eq!(resp.status(), 202, "in-progress status should be 202");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert!(manifest["transactionTime"].is_string());
    assert_eq!(manifest["request"], format!("{}/$export", base_url));
    let output = manifest["output"].as_array().unwrap();
    let types: Vec<&str> = output.iter().map(|o| o["type"].as_str().unwrap()).collect();
    assert!(types.contains(&"Patient") && types.contains(&"Observation"));

    // Download one NDJSON file.
    let patient_url = output
        .iter()
        .find(|o| o["type"] == "Patient")
        .unwrap()["url"]
        .as_str()
        .unwrap();
    let resp = client.get(patient_url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.lines().next().unwrap().contains("\"resourceType\":\"Patient\""));

    // DELETE the job.
    let resp = client.delete(&status_url).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    // After deletion the status URL is gone.
    let resp = client.get(&status_url).send().await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_metadata_advertises_bulk_export_operations() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();
    let body: Value = client
        .get(format!("{}/metadata", base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let resources = body["rest"][0]["resource"].as_array().unwrap();
    let op_def = |rtype: &str| -> Option<String> {
        resources
            .iter()
            .find(|r| r["type"] == rtype)?
            .get("operation")?
            .as_array()?
            .iter()
            .find(|o| o["name"] == "export")
            .and_then(|o| o["definition"].as_str())
            .map(String::from)
    };
    assert_eq!(
        op_def("Patient").as_deref(),
        Some("http://hl7.org/fhir/uv/bulkdata/OperationDefinition/patient-export")
    );
    assert_eq!(
        op_def("Group").as_deref(),
        Some("http://hl7.org/fhir/uv/bulkdata/OperationDefinition/group-export")
    );
}

#[tokio::test]
async fn test_bulk_patient_and_group_export() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    create(&client, &base_url, "Patient", &json!({"resourceType": "Patient", "id": "p1"})).await;
    create(&client, &base_url, "Patient", &json!({"resourceType": "Patient", "id": "p2"})).await;
    create(
        &client,
        &base_url,
        "Observation",
        &json!({"resourceType": "Observation", "id": "o1", "status": "final",
                "code": {"text": "x"}, "subject": {"reference": "Patient/p1"}}),
    )
    .await;
    create(
        &client,
        &base_url,
        "Observation",
        &json!({"resourceType": "Observation", "id": "o2", "status": "final",
                "code": {"text": "y"}, "subject": {"reference": "Patient/p2"}}),
    )
    .await;
    // Organization is outside the Patient compartment.
    create(&client, &base_url, "Organization", &json!({"resourceType": "Organization", "name": "Acme"})).await;
    create(
        &client,
        &base_url,
        "Group",
        &json!({"resourceType": "Group", "id": "g1", "type": "person", "actual": true,
                "member": [{"entity": {"reference": "Patient/p1"}}]}),
    )
    .await;

    let types_of = |body: &str| -> Vec<String> {
        body.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["resourceType"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    };

    // Patient-level export excludes non-compartment resources (Organization).
    let body = client
        .get(format!("{}/Patient/$export", base_url))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let types = types_of(&body);
    assert!(types.contains(&"Patient".to_string()) && types.contains(&"Observation".to_string()));
    assert!(!types.contains(&"Organization".to_string()), "Organization is outside the Patient compartment");

    // Group export returns only the member patient's compartment.
    let body = client
        .get(format!("{}/Group/g1/$export", base_url))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let ids: Vec<String> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"p1".to_string()) && ids.contains(&"o1".to_string()));
    assert!(!ids.contains(&"p2".to_string()) && !ids.contains(&"o2".to_string()), "non-member data excluded");

    // Unknown group -> 404.
    let resp = client.get(format!("{}/Group/nope/$export", base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_bulk_export_sync_fallback_and_bad_format() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();
    create(&client, &base_url, "Patient", &json!({"resourceType": "Patient"})).await;

    // No Prefer header -> synchronous NDJSON.
    let resp = client.get(format!("{}/$export", base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["content-type"],
        "application/fhir+ndjson"
    );

    // Unsupported _outputFormat is rejected.
    let resp = client
        .get(format!("{}/$export?_outputFormat=csv", base_url))
        .header("Prefer", "respond-async")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// Test RSA key (RS384) for SMART Backend Services: the private PEM signs the
// client assertion; the JWKS is registered with the server to verify it.
const SMART_TEST_PRIV_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDa8PTmPplmNc/D
5nyVz/rEXB+kUYo6O3d3oj5sY0JxwD7JAYZ73TZgxj7VkDC4p/Gvphr9N0McJNje
HQ8YO4BhjLOTKKqWVyppGZLnzGFqeVRoMgPBCFdfvKvd4L605BKp2Ji+yuQzG0gg
Zheld577B+Blgn0EdBFj+ss9+BRX/oYzuCDdmXtLFnCkBcEX0tynYcJfIKIsN9nf
ebXkyRIoCrDSXBzpikvoYn3AGZw/NpYHNxiizWJTzZffhudU0++vJY000ceZzj5e
ZSPiemw2ap5cS4t62rJMNmxUDVXoItNPQyEQ/KZbsVCKfaqmCW73rFOLfP3gCMWt
I8C3LM29AgMBAAECggEABdMWOM/ehd0Vn2kQ1G15huLKhrUSn9T4KysaTQjg+iS6
pt9PBBmQ+kcb4dOZsDfrkpgxLJ5dPzDtwg702Viv5Ch5mIci7qf3/V7MTTVfPBVp
PPJjri16IZZj3h2+BRLyOimEesLsCj4GD7KXm3PQu23sttafW9B4q/e+k3H3nJWg
vynl80tP5BpEQK5t//gUw1rEqqMlqKQzYA+ECu6cfDqybLJ/Vf072XeeOFugpbRi
86llLKvTXsY6igXSiuylNcT9++R10Q3Z2RZn7dVxqg0Dq/0O8rUh3HNeMwRhOtXo
ir6oHRlnqu1asIHcxCOxElmKAd6tYttViETRJnm2cwKBgQD66qBkHqSThEF6CsnD
b8+LZ/63iKr/bJikje6ODMMTy3gmRx5Lp1R3N//RhSl1Bb4hftt5kX+es8OXPAM3
e27bL4QaAEhnZSCK7ikKHc+mJq1qIiz4o/jBcNFBU/c+PGwZz7fIWJ1J57Wn3cPr
3Gdf+X8ux3fiSfV2J/4K90HahwKBgQDfYH2x2ifBUcjMLAnlE7OgnodhQv67hUb1
2WDaZem9oexhFGSmPTUxmoMrGIdQp88i0GaoYbGabKI4ZcrRJMPW/SQ9QwH3g1r5
2ls59mNXNGuuGK56NMTuftujfft60tLvJHrZToEqoOj+hI9FW6mprGAKpKT4Hxwj
A+J5I5ASmwKBgBG1FyaZpwGmFrzXIBADxwDOkQJAydiGwGr0fT5XFjlCZbNF15R9
NY/ISuxtWlSg2B8jBbgZlYkMpKP6nzt+IX+kZc0z8egiEd7cGddV7T6Lgjd4O/Y8
FCUKoinbFBq3RuW3a//eiRMbIA29r8LoZf8260vQ1dwJXiI9hkcd1e7nAoGAVvVw
WVFtEpQtEFbAj15JGmk8mL+E/SlYQ5MZYkw0X29u3ygaOqEvWR0mnRtSajK5r3rj
y/8kLdLGoB/4gczHNKWdev/f/yQywXug8gKaD7PHi6HuIoLye0oFZTNBOEJkfVZ9
QKWQITE9IwAl6kFR3kzNorcPxcwOnw7/Flui1asCgYEAvoxDAQm0JYPHFuUaseSg
uLWo8sIgz41CDC3de0WijcIBnhfW5rLugsScL88RdKh5nRZCx+a4bK6SJCwkmTWk
H4PmuZYdT4hGcpoCrnEz3EkE+o1YbJyc9rr3JYwToTrPb2XxOawOxVb9O0/JBlaF
RBZvtZSgEHvY9JvTeEu+Pog=
-----END PRIVATE KEY-----";

const SMART_TEST_JWKS: &str = r#"{"keys":[{"kty":"RSA","alg":"RS384","use":"sig","kid":"test-key-1","n":"2vD05j6ZZjXPw-Z8lc_6xFwfpFGKOjt3d6I-bGNCccA-yQGGe902YMY-1ZAwuKfxr6Ya_TdDHCTY3h0PGDuAYYyzkyiqllcqaRmS58xhanlUaDIDwQhXX7yr3eC-tOQSqdiYvsrkMxtIIGYXpXee-wfgZYJ9BHQRY_rLPfgUV_6GM7gg3Zl7SxZwpAXBF9Lcp2HCXyCiLDfZ33m15MkSKAqw0lwc6YpL6GJ9wBmcPzaWBzcYos1iU82X34bnVNPvryWNNNHHmc4-XmUj4npsNmqeXEuLetqyTDZsVA1V6CLTT0MhEPymW7FQin2qpglu96xTi3z94AjFrSPAtyzNvQ","e":"AQAB"}]}"#;

#[tokio::test]
async fn test_smart_backend_services_token_flow() {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    use sazare_server::config::{BackendClient, JwtSettings, SmartSettings};

    let temp_dir = TempDir::new().unwrap();
    let mut config = ServerConfig::default();
    config.auth.enabled = true;
    config.auth.jwt = Some(JwtSettings {
        issuer: Some("https://sazare.test".into()),
        audience: None,
        secret: Some("backend-services-test-secret".into()),
        public_key_file: None,
        jwk_url: None,
    });
    config.auth.smart = Some(SmartSettings {
        token_endpoint: Some("https://sazare.test/token".into()),
        token_ttl_secs: Some(300),
        backend_clients: vec![BackendClient {
            client_id: "inferno-client".into(),
            jwks_url: None,
            jwks: Some(serde_json::from_str(SMART_TEST_JWKS).unwrap()),
            allowed_scopes: vec![],
        }],
    });

    let state = Arc::new(AppState {
        store: SqliteStore::open(temp_dir.path().join("r.sqlite")).unwrap(),
        index: Mutex::new(SearchIndex::open(temp_dir.path().join("i.sqlite")).unwrap()),
        audit: Arc::new(Mutex::new(AuditLog::open(temp_dir.path().join("a.sqlite")).unwrap())),
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
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap()
    });
    let base_url = format!("http://{}", addr);
    let client = reqwest::Client::new();

    // Auth is enabled: an unauthenticated request is rejected.
    let resp = client.get(format!("{}/Patient", base_url)).send().await.unwrap();
    assert_eq!(resp.status(), 401, "auth enabled rejects anonymous");

    // Build a client assertion signed with the client's private key.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let assertion_claims = json!({
        "iss": "inferno-client",
        "sub": "inferno-client",
        "aud": "https://sazare.test/token",
        "exp": now + 300,
        "iat": now,
        "jti": "unique-jti-1"
    });
    let mut header = Header::new(Algorithm::RS384);
    header.kid = Some("test-key-1".into());
    let assertion = encode(
        &header,
        &assertion_claims,
        &EncodingKey::from_rsa_pem(SMART_TEST_PRIV_PEM.as_bytes()).unwrap(),
    )
    .unwrap();

    // Exchange the assertion for an access token.
    let resp = client
        .post(format!("{}/token", base_url))
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_assertion_type", "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"),
            ("client_assertion", &assertion),
            ("scope", "system/*.read"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "token exchange succeeds");
    let token_resp: Value = resp.json().await.unwrap();
    assert_eq!(token_resp["token_type"], "bearer");
    let access_token = token_resp["access_token"].as_str().unwrap();

    // The issued token authorizes a request.
    let resp = client
        .get(format!("{}/Patient", base_url))
        .bearer_auth(access_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "issued bearer token is accepted");

    // A bad grant type is rejected.
    let resp = client
        .post(format!("{}/token", base_url))
        .form(&[("grant_type", "password")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_health_check() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/health", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["fhirVersion"], "4.0.1");
}

#[tokio::test]
async fn test_metadata() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/metadata", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["resourceType"], "CapabilityStatement");
    assert_eq!(body["fhirVersion"], "4.0.1");
}

#[tokio::test]
async fn test_patient_crud_and_search() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    // 1. POST /Patient — Create
    let patient = json!({
        "resourceType": "Patient",
        "name": [{"family": "Doe", "given": ["Jane"]}],
        "gender": "female"
    });

    let resp = client
        .post(format!("{}/Patient", base_url))
        .header("Content-Type", "application/fhir+json")
        .json(&patient)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "POST should return 201 Created");

    let created: Value = resp.json().await.unwrap();
    let id = created["id"].as_str().expect("Created resource should have id");
    assert_eq!(created["resourceType"], "Patient");
    assert_eq!(created["name"][0]["family"], "Doe");
    assert_eq!(created["meta"]["versionId"], "1");

    // 2. GET /Patient/{id} — Read
    let resp = client
        .get(format!("{}/Patient/{}", base_url, id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "GET should return 200 OK");

    let read: Value = resp.json().await.unwrap();
    assert_eq!(read["id"], id);
    assert_eq!(read["name"][0]["family"], "Doe");
    assert_eq!(read["name"][0]["given"][0], "Jane");

    // 3. GET /Patient?family=Doe — Search
    let resp = client
        .get(format!("{}/Patient?family=Doe", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "Search should return 200 OK");

    let bundle: Value = resp.json().await.unwrap();
    assert_eq!(bundle["resourceType"], "Bundle");
    assert_eq!(bundle["type"], "searchset");
    assert_eq!(bundle["total"], 1);
    assert_eq!(bundle["entry"][0]["resource"]["id"], id);

    // 4. PUT /Patient/{id} — Update
    let updated_patient = json!({
        "resourceType": "Patient",
        "name": [{"family": "Doe", "given": ["Jane", "M"]}],
        "gender": "female"
    });

    let resp = client
        .put(format!("{}/Patient/{}", base_url, id))
        .header("Content-Type", "application/fhir+json")
        .json(&updated_patient)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "PUT should return 200 OK");

    let updated: Value = resp.json().await.unwrap();
    assert_eq!(updated["meta"]["versionId"], "2");
    assert_eq!(updated["name"][0]["given"][1], "M");

    // 5. GET /Patient/{id}/_history — History
    let resp = client
        .get(format!("{}/Patient/{}/_history", base_url, id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "History should return 200 OK");

    let history: Value = resp.json().await.unwrap();
    assert_eq!(history["resourceType"], "Bundle");
    assert_eq!(history["type"], "history");
    assert!(history["total"].as_u64().unwrap() >= 1);

    // 6. DELETE /Patient/{id}
    let resp = client
        .delete(format!("{}/Patient/{}", base_url, id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204, "DELETE should return 204 No Content");

    // 7. Verify deleted — GET should return 404
    let resp = client
        .get(format!("{}/Patient/{}", base_url, id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 410, "GET after DELETE should return 410 Gone");
}

#[tokio::test]
async fn test_observation_create_and_search() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    // Create an Observation
    let observation = json!({
        "resourceType": "Observation",
        "status": "final",
        "code": {
            "coding": [{
                "system": "http://loinc.org",
                "code": "85354-9",
                "display": "Blood pressure"
            }]
        },
        "subject": {
            "reference": "Patient/test-123"
        },
        "valueQuantity": {
            "value": 120,
            "unit": "mmHg"
        }
    });

    let resp = client
        .post(format!("{}/Observation", base_url))
        .json(&observation)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let created: Value = resp.json().await.unwrap();
    let id = created["id"].as_str().unwrap();

    // Search by code
    let resp = client
        .get(format!("{}/Observation?code=85354-9", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let bundle: Value = resp.json().await.unwrap();
    assert_eq!(bundle["total"], 1);
    assert_eq!(bundle["entry"][0]["resource"]["id"], id);
}

#[tokio::test]
async fn test_resource_not_found() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/Patient/nonexistent-id", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["resourceType"], "OperationOutcome");
}

#[tokio::test]
async fn test_invalid_resource_rejected() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    // Missing resourceType
    let invalid = json!({
        "name": [{"family": "Test"}]
    });

    let resp = client
        .post(format!("{}/Patient", base_url))
        .json(&invalid)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_bundle_transaction() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let bundle = json!({
        "resourceType": "Bundle",
        "type": "transaction",
        "entry": [
            {
                "fullUrl": "urn:uuid:patient-1",
                "resource": {
                    "resourceType": "Patient",
                    "name": [{"family": "Smith"}],
                    "gender": "female"
                },
                "request": {
                    "method": "POST",
                    "url": "Patient"
                }
            },
            {
                "fullUrl": "urn:uuid:obs-1",
                "resource": {
                    "resourceType": "Observation",
                    "status": "final",
                    "code": {"coding": [{"system": "http://loinc.org", "code": "29463-7"}]},
                    "subject": {"reference": "urn:uuid:patient-1"}
                },
                "request": {
                    "method": "POST",
                    "url": "Observation"
                }
            }
        ]
    });

    let resp = client
        .post(&base_url)
        .json(&bundle)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let result: Value = resp.json().await.unwrap();
    assert_eq!(result["resourceType"], "Bundle");
    assert_eq!(result["type"], "transaction-response");

    let entries = result["entry"].as_array().unwrap();
    assert_eq!(entries.len(), 2);

    // First entry should be 201 Created
    assert!(entries[0]["response"]["status"].as_str().unwrap().contains("201"));
    // Second entry should be 201 Created
    assert!(entries[1]["response"]["status"].as_str().unwrap().contains("201"));
}

/// Helper: POST a resource and return its server-assigned id.
async fn create(client: &reqwest::Client, base_url: &str, type_: &str, body: &Value) -> String {
    let resp = client
        .post(format!("{}/{}", base_url, type_))
        .json(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create {} should 201", type_);
    let created: Value = resp.json().await.unwrap();
    created["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn test_demo_loads_sample_data() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    // Load the curated sample dataset.
    let resp = client.post(format!("{base_url}/$demo")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    // Every demo resource must be valid (no load errors) and several must load.
    assert_eq!(body["errors"].as_array().unwrap().len(), 0, "demo data should be valid: {body}");
    assert!(body["loaded"].as_u64().unwrap() >= 5, "expected several demo resources, got {body}");

    // The sample patients are now searchable.
    let bundle: Value = client
        .get(format!("{base_url}/Patient"))
        .send().await.unwrap().json().await.unwrap();
    assert!(bundle["total"].as_u64().unwrap() >= 2, "demo patients should be searchable");

    // Idempotent: loading again doesn't error or duplicate.
    let resp2 = client.post(format!("{base_url}/$demo")).send().await.unwrap();
    assert_eq!(resp2.status(), 200);
    let bundle2: Value = client
        .get(format!("{base_url}/Patient"))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(
        bundle["total"], bundle2["total"],
        "re-running $demo must not duplicate resources"
    );
}

#[tokio::test]
async fn test_post_with_existing_id_conflicts() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    // First create with an explicit id succeeds.
    let body = json!({"resourceType": "Patient", "id": "fixed-1", "name": [{"family": "One"}]});
    let r = client.post(format!("{base_url}/Patient")).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 201);

    // A second POST reusing that id must NOT clobber it — expect 409.
    let body2 = json!({"resourceType": "Patient", "id": "fixed-1", "name": [{"family": "Two"}]});
    let r2 = client.post(format!("{base_url}/Patient")).json(&body2).send().await.unwrap();
    assert_eq!(r2.status(), 409, "re-POST of an existing id must conflict, not overwrite");

    // The original survives.
    let got: Value = client.get(format!("{base_url}/Patient/fixed-1")).send().await.unwrap().json().await.unwrap();
    assert_eq!(got["name"][0]["family"], "One");
}

#[tokio::test]
async fn test_repeated_date_params_are_anded() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    // Two Observations on different dates.
    create(&client, &base_url, "Observation", &json!({
        "resourceType": "Observation", "status": "final",
        "code": {"coding": [{"code": "x"}]},
        "effectiveDateTime": "2024-06-15"
    })).await;
    create(&client, &base_url, "Observation", &json!({
        "resourceType": "Observation", "status": "final",
        "code": {"coding": [{"code": "x"}]},
        "effectiveDateTime": "2023-01-01"
    })).await;

    // A bounded range (AND of two date params) must return only the 2024 one.
    let bundle: Value = client
        .get(format!("{base_url}/Observation?date=ge2024-01-01&date=le2024-12-31"))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(bundle["total"], 1, "repeated date params must AND, not last-wins");
}

#[tokio::test]
async fn test_if_match_conflict_is_412() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();
    let id = create(&client, &base_url, "Patient", &json!({"resourceType": "Patient", "name": [{"family": "M"}]})).await;

    // Update with a stale If-Match version → 412 Precondition Failed.
    let r = client
        .put(format!("{base_url}/Patient/{id}"))
        .header("If-Match", "W/\"99\"")
        .json(&json!({"resourceType": "Patient", "id": id, "name": [{"family": "M2"}]}))
        .send().await.unwrap();
    assert_eq!(r.status(), 412, "stale If-Match must be 412");
}

#[tokio::test]
async fn test_conditional_create_if_none_exist() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let patient = json!({"resourceType": "Patient", "name": [{"family": "Unique"}]});
    let id = create(&client, &base_url, "Patient", &patient).await;

    // Conditional create with a matching criterion must NOT create a duplicate;
    // it returns the existing resource (200).
    let resp = client
        .post(format!("{}/Patient", base_url))
        .header("If-None-Exist", "family=Unique")
        .json(&patient)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "matching If-None-Exist should return existing (200)");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"], id, "should return the existing resource, not a new one");

    // Only one Patient should exist.
    let resp = client
        .get(format!("{}/Patient?family=Unique", base_url))
        .send()
        .await
        .unwrap();
    let bundle: Value = resp.json().await.unwrap();
    assert_eq!(bundle["total"], 1, "no duplicate should be created");
}

#[tokio::test]
async fn test_conditional_update() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    // 0 matches -> conditional update creates a new resource (201).
    let resp = client
        .put(format!("{}/Patient?family=Cond", base_url))
        .json(&json!({"resourceType": "Patient", "name": [{"family": "Cond"}], "gender": "male"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "conditional update with no match should create");
    let created: Value = resp.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    // 1 match -> conditional update updates the existing resource (200).
    let resp = client
        .put(format!("{}/Patient?family=Cond", base_url))
        .json(&json!({"resourceType": "Patient", "name": [{"family": "Cond"}], "gender": "female"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "conditional update with one match should update");
    let updated: Value = resp.json().await.unwrap();
    assert_eq!(updated["id"], id, "should update the same resource");
    assert_eq!(updated["gender"], "female");
}

#[tokio::test]
async fn test_conditional_delete() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let id = create(
        &client,
        &base_url,
        "Patient",
        &json!({"resourceType": "Patient", "name": [{"family": "DelMe"}]}),
    )
    .await;

    let resp = client
        .delete(format!("{}/Patient?family=DelMe", base_url))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "conditional delete should succeed");

    // Resource is gone.
    let resp = client
        .get(format!("{}/Patient/{}", base_url, id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 410, "deleted resource should be 410 Gone");
}

#[tokio::test]
async fn test_bulk_export_import_roundtrip() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    create(&client, &base_url, "Patient", &json!({"resourceType": "Patient", "name": [{"family": "Exp1"}]})).await;
    create(&client, &base_url, "Patient", &json!({"resourceType": "Patient", "name": [{"family": "Exp2"}]})).await;

    // $export -> NDJSON
    let resp = client
        .get(format!("{}/$export?_type=Patient", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "$export should return 200");
    let ndjson = resp.text().await.unwrap();
    let lines: Vec<&str> = ndjson.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "two Patients should be exported");
    for line in &lines {
        let v: Value = serde_json::from_str(line).expect("each line is a JSON resource");
        assert_eq!(v["resourceType"], "Patient");
    }

    // $import the same NDJSON into a fresh server, then verify it lands.
    let (base2, _dir2) = start_test_server().await;
    let resp = client
        .post(format!("{}/$import", base2))
        .header("Content-Type", "application/fhir+ndjson")
        .body(ndjson)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "$import should return 200");

    let resp = client
        .get(format!("{}/Patient?family=Exp1", base2))
        .send()
        .await
        .unwrap();
    let bundle: Value = resp.json().await.unwrap();
    assert_eq!(bundle["total"], 1, "imported Patient should be searchable");
}

#[tokio::test]
async fn test_search_include_and_revinclude() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let pid = create(
        &client,
        &base_url,
        "Patient",
        &json!({"resourceType": "Patient", "name": [{"family": "Incl"}]}),
    )
    .await;
    create(
        &client,
        &base_url,
        "Observation",
        &json!({
            "resourceType": "Observation",
            "status": "final",
            "code": {"coding": [{"system": "http://loinc.org", "code": "1234-5"}]},
            "subject": {"reference": format!("Patient/{}", pid)}
        }),
    )
    .await;

    // _include: searching Observations pulls in the referenced Patient.
    let resp = client
        .get(format!("{}/Observation?code=1234-5&_include=Observation:subject", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bundle: Value = resp.json().await.unwrap();
    let types: Vec<&str> = bundle["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["resource"]["resourceType"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"Observation"), "_include result has the Observation");
    assert!(types.contains(&"Patient"), "_include should pull in the referenced Patient");

    // _revinclude: searching the Patient pulls in Observations referencing it.
    let resp = client
        .get(format!("{}/Patient?family=Incl&_revinclude=Observation:subject", base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bundle: Value = resp.json().await.unwrap();
    let types: Vec<&str> = bundle["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["resource"]["resourceType"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"Patient"), "_revinclude result has the Patient");
    assert!(types.contains(&"Observation"), "_revinclude should pull in the referencing Observation");
}

#[tokio::test]
async fn test_websocket_subscription_ping() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    // Create a websocket-channel Subscription (no endpoint needed).
    let sub_id = create(
        &client,
        &base_url,
        "Subscription",
        &json!({
            "resourceType": "Subscription",
            "status": "active",
            "criteria": "Observation?status=final",
            "channel": {"type": "websocket"}
        }),
    )
    .await;

    // Connect and bind to the subscription.
    let ws_url = format!("{}/ws", base_url.replace("http://", "ws://"));
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    ws.send(Message::text(format!("bind {sub_id}"))).await.unwrap();

    // Helper: read the next non-empty text frame, with a timeout.
    async fn next_text(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> String {
        let fut = async {
            while let Some(Ok(msg)) = ws.next().await {
                if let Ok(t) = msg.to_text()
                    && !t.is_empty()
                {
                    return t.to_string();
                }
            }
            String::new()
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), fut)
            .await
            .expect("timed out waiting for ws frame")
    }

    assert_eq!(next_text(&mut ws).await, format!("bound {sub_id}"));

    // A matching create triggers a ping.
    create(
        &client,
        &base_url,
        "Observation",
        &json!({
            "resourceType": "Observation",
            "status": "final",
            "code": {"coding": [{"system": "http://loinc.org", "code": "9999-9"}]}
        }),
    )
    .await;

    assert_eq!(
        next_text(&mut ws).await,
        format!("ping {sub_id}"),
        "matching create should ping the bound websocket client"
    );
}

#[tokio::test]
async fn test_search_multi_level_chain() {
    // Condition -> encounter -> Encounter -> subject -> Patient.name
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let pid = create(
        &client,
        &base_url,
        "Patient",
        &json!({"resourceType": "Patient", "name": [{"family": "Zelda"}]}),
    )
    .await;
    let eid = create(
        &client,
        &base_url,
        "Encounter",
        &json!({
            "resourceType": "Encounter",
            "status": "finished",
            "class": {"code": "AMB"},
            "subject": {"reference": format!("Patient/{}", pid)}
        }),
    )
    .await;
    create(
        &client,
        &base_url,
        "Condition",
        &json!({
            "resourceType": "Condition",
            "subject": {"reference": format!("Patient/{}", pid)},
            "encounter": {"reference": format!("Encounter/{}", eid)}
        }),
    )
    .await;

    let total = |bundle: &Value| bundle["entry"].as_array().map(|a| a.len()).unwrap_or(0);

    // Two-level chain resolves through Encounter to the Patient's name.
    let resp = client
        .get(format!(
            "{}/Condition?encounter:Encounter.subject:Patient.name=Zelda",
            base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bundle: Value = resp.json().await.unwrap();
    assert_eq!(total(&bundle), 1, "multi-level chain should match the Condition");

    // Negative control: a name that doesn't exist matches nothing.
    let resp = client
        .get(format!(
            "{}/Condition?encounter:Encounter.subject:Patient.name=Nobody",
            base_url
        ))
        .send()
        .await
        .unwrap();
    let bundle: Value = resp.json().await.unwrap();
    assert_eq!(total(&bundle), 0, "non-matching terminal value yields nothing");

    // One-level chains still work.
    let resp = client
        .get(format!("{}/Condition?subject:Patient.name=Zelda", base_url))
        .send()
        .await
        .unwrap();
    let bundle: Value = resp.json().await.unwrap();
    assert_eq!(total(&bundle), 1, "single-level chain still works");
}

#[tokio::test]
async fn test_search_include_choice_type_medication_reference() {
    // Regression: `_include=MedicationRequest:medication` must resolve the
    // choice-type `medicationReference` element (not a bare `medication` field).
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let med_id = create(
        &client,
        &base_url,
        "Medication",
        &json!({
            "resourceType": "Medication",
            "code": {"coding": [{"system": "http://www.nlm.nih.gov/research/umls/rxnorm", "code": "860975"}]}
        }),
    )
    .await;
    create(
        &client,
        &base_url,
        "MedicationRequest",
        &json!({
            "resourceType": "MedicationRequest",
            "status": "active",
            "intent": "order",
            "medicationReference": {"reference": format!("Medication/{}", med_id)},
            "subject": {"reference": "Patient/example"}
        }),
    )
    .await;

    let resp = client
        .get(format!(
            "{}/MedicationRequest?_include=MedicationRequest:medication",
            base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bundle: Value = resp.json().await.unwrap();
    let types: Vec<&str> = bundle["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["resource"]["resourceType"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"MedicationRequest"), "result has the MedicationRequest");
    assert!(
        types.contains(&"Medication"),
        "_include should resolve the choice-type medicationReference and pull in the Medication, got {types:?}"
    );
}

#[tokio::test]
async fn test_json_patch() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let id = create(
        &client,
        &base_url,
        "Patient",
        &json!({"resourceType": "Patient", "name": [{"family": "Patch"}], "gender": "male"}),
    )
    .await;

    let patch = json!([{"op": "replace", "path": "/gender", "value": "female"}]);
    let resp = client
        .patch(format!("{}/Patient/{}", base_url, id))
        .header("Content-Type", "application/json-patch+json")
        .json(&patch)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "PATCH should return 200");

    let resp = client
        .get(format!("{}/Patient/{}", base_url, id))
        .send()
        .await
        .unwrap();
    let read: Value = resp.json().await.unwrap();
    assert_eq!(read["gender"], "female", "patch should have applied");
}

#[tokio::test]
async fn test_patient_everything() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let pid = create(
        &client,
        &base_url,
        "Patient",
        &json!({"resourceType": "Patient", "name": [{"family": "Every"}]}),
    )
    .await;
    create(
        &client,
        &base_url,
        "Observation",
        &json!({
            "resourceType": "Observation",
            "status": "final",
            "code": {"coding": [{"system": "http://loinc.org", "code": "9999-9"}]},
            "subject": {"reference": format!("Patient/{}", pid)}
        }),
    )
    .await;

    let resp = client
        .get(format!("{}/Patient/{}/$everything", base_url, pid))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "$everything should return 200");
    let bundle: Value = resp.json().await.unwrap();
    assert_eq!(bundle["resourceType"], "Bundle");
    let types: Vec<&str> = bundle["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["resource"]["resourceType"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"Patient"), "$everything includes the Patient");
    assert!(types.contains(&"Observation"), "$everything includes compartment members");
}

#[tokio::test]
async fn test_vread_specific_version() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    let id = create(
        &client,
        &base_url,
        "Patient",
        &json!({"resourceType": "Patient", "name": [{"family": "Ver"}], "gender": "male"}),
    )
    .await;

    // Update -> version 2
    client
        .put(format!("{}/Patient/{}", base_url, id))
        .json(&json!({"resourceType": "Patient", "name": [{"family": "Ver"}], "gender": "female"}))
        .send()
        .await
        .unwrap();

    // vread version 1 still has the original value.
    let resp = client
        .get(format!("{}/Patient/{}/_history/1", base_url, id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "vread of v1 should return 200");
    let v1: Value = resp.json().await.unwrap();
    assert_eq!(v1["meta"]["versionId"], "1");
    assert_eq!(v1["gender"], "male", "v1 should retain the original value");
}

#[tokio::test]
async fn test_bundle_batch() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    // Pre-create a resource so the batch can read it.
    let id = create(
        &client,
        &base_url,
        "Patient",
        &json!({"resourceType": "Patient", "name": [{"family": "Batch"}]}),
    )
    .await;

    // Mixed batch: a GET hit, a GET miss, and a POST — entries are independent,
    // so each resolves on its own and the bundle as a whole returns 200.
    let bundle = json!({
        "resourceType": "Bundle",
        "type": "batch",
        "entry": [
            { "request": {"method": "GET", "url": format!("Patient/{}", id)} },
            { "request": {"method": "GET", "url": "Patient/does-not-exist"} },
            {
                "resource": {"resourceType": "Patient", "name": [{"family": "BatchNew"}]},
                "request": {"method": "POST", "url": "Patient"}
            }
        ]
    });

    let resp = client.post(&base_url).json(&bundle).send().await.unwrap();
    assert_eq!(resp.status(), 200, "batch itself returns 200 even with a failing entry");
    let result: Value = resp.json().await.unwrap();
    assert_eq!(result["type"], "batch-response");
    let entries = result["entry"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    // GET hit: 200 with the resource body.
    assert!(entries[0]["response"]["status"].as_str().unwrap().contains("200"));
    assert_eq!(entries[0]["resource"]["id"], id);
    // GET miss: 404, independently of the others.
    assert!(entries[1]["response"]["status"].as_str().unwrap().contains("404"));
    // POST still creates.
    assert!(entries[2]["response"]["status"].as_str().unwrap().contains("201"));
}

#[tokio::test]
async fn test_search_by_profile() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();
    let jp_patient = "http://jpfhir.jp/fhir/core/StructureDefinition/JP_Patient";

    let pid = create(&client, &base_url, "Patient", &json!({
        "resourceType": "Patient",
        "meta": {"profile": [jp_patient]},
        "identifier": [{"system": "urn:oid:1.2.392.100495.20.3.51.1", "value": "1"}],
        "name": [{"family": "山田"}]
    })).await;
    // A non-JP patient that must not match.
    create(&client, &base_url, "Patient", &json!({"resourceType": "Patient", "name": [{"family": "Doe"}]})).await;

    let resp = client
        .get(format!("{}/Patient", base_url))
        .query(&[("_profile", jp_patient)])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bundle: Value = resp.json().await.unwrap();
    assert_eq!(bundle["total"], 1, "_profile search should return only the JP_Patient");
    assert_eq!(bundle["entry"][0]["resource"]["id"], pid);
}
