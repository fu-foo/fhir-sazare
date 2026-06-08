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
    assert_eq!(resp.status(), 404, "deleted resource should be 404");
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
async fn test_jp_kana_name_search() {
    let (base_url, _dir) = start_test_server().await;
    let client = reqwest::Client::new();

    // JP Core Patient with both kanji (IDE) and kana (SYL) name representations.
    let rep = "http://hl7.org/fhir/StructureDefinition/iso21090-EN-representation";
    let patient = json!({
        "resourceType": "Patient",
        "name": [
            {"extension": [{"url": rep, "valueCode": "IDE"}], "use": "usual",
             "text": "山田 太郎", "family": "山田", "given": ["太郎"]},
            {"extension": [{"url": rep, "valueCode": "SYL"}], "use": "usual",
             "text": "ヤマダ タロウ", "family": "ヤマダ", "given": ["タロウ"]}
        ],
        "gender": "male"
    });
    create(&client, &base_url, "Patient", &patient).await;

    async fn total(client: &reqwest::Client, base: &str, param: &str, value: &str) -> i64 {
        let resp = client
            .get(format!("{}/Patient", base))
            .query(&[(param, value)])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let bundle: Value = resp.json().await.unwrap();
        bundle["total"].as_i64().unwrap_or(0)
    }

    // Plain `name` indexes every representation, so it matches kanji and kana alike.
    assert_eq!(total(&client, &base_url, "name", "山田").await, 1, "name should match kanji");
    assert_eq!(total(&client, &base_url, "name", "ヤマダ").await, 1, "name should match kana too");

    // `name-kana` matches only the SYL (kana) representation.
    assert_eq!(total(&client, &base_url, "name-kana", "ヤマダ").await, 1, "name-kana matches kana");
    assert_eq!(total(&client, &base_url, "name-kana", "タロウ").await, 1, "name-kana matches kana given");
    assert_eq!(total(&client, &base_url, "name-kana", "山田").await, 0, "name-kana must NOT match kanji");
}
