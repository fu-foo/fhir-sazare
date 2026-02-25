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
    assert_eq!(resp.status(), 404, "GET after DELETE should return 404");
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
