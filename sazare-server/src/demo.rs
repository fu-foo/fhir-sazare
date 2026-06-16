//! One-click sample data, for the "I just downloaded and ran it — now what?"
//! moment. `POST /$demo` upserts a small, relatable set of resources (a couple
//! of patients with vitals, a condition, an encounter, a prescription) so the
//! built-in dashboard has something to explore on first run.
//!
//! Resources use fixed ids so their references resolve, and the loader upserts
//! (idempotent) so clicking "load sample data" twice just refreshes them.

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use sazare_core::validation::validate_resource_all_phases;
use sazare_store::IndexBuilder;
use serde_json::{json, Value};

use crate::AppState;

/// Curated demo resources. Hand-written to be valid and readable, with stable
/// ids and cross-references (Observations/Condition/etc. point at the patients).
const DEMO_RESOURCES: &str = r##"[
  {
    "resourceType": "Patient",
    "id": "demo-patient-amy",
    "name": [{"family": "Shaw", "given": ["Amy"], "text": "Amy Shaw"}],
    "gender": "female",
    "birthDate": "1987-02-20",
    "telecom": [{"system": "phone", "value": "555-0142", "use": "home"}]
  },
  {
    "resourceType": "Patient",
    "id": "demo-patient-john",
    "name": [{"family": "Brown", "given": ["John"], "text": "John Brown"}],
    "gender": "male",
    "birthDate": "1965-08-09"
  },
  {
    "resourceType": "Observation",
    "id": "demo-bodyweight",
    "status": "final",
    "category": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/observation-category", "code": "vital-signs"}]}],
    "code": {"coding": [{"system": "http://loinc.org", "code": "29463-7", "display": "Body weight"}], "text": "Body weight"},
    "subject": {"reference": "Patient/demo-patient-john"},
    "effectiveDateTime": "2026-01-15",
    "valueQuantity": {"value": 80.5, "unit": "kg", "system": "http://unitsofmeasure.org", "code": "kg"}
  },
  {
    "resourceType": "Observation",
    "id": "demo-heartrate",
    "status": "final",
    "category": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/observation-category", "code": "vital-signs"}]}],
    "code": {"coding": [{"system": "http://loinc.org", "code": "8867-4", "display": "Heart rate"}], "text": "Heart rate"},
    "subject": {"reference": "Patient/demo-patient-john"},
    "effectiveDateTime": "2026-01-15",
    "valueQuantity": {"value": 72, "unit": "beats/minute", "system": "http://unitsofmeasure.org", "code": "/min"}
  },
  {
    "resourceType": "Condition",
    "id": "demo-condition",
    "clinicalStatus": {"coding": [{"system": "http://terminology.hl7.org/CodeSystem/condition-clinical", "code": "active"}]},
    "verificationStatus": {"coding": [{"system": "http://terminology.hl7.org/CodeSystem/condition-ver-status", "code": "confirmed"}]},
    "code": {"coding": [{"system": "http://snomed.info/sct", "code": "38341003", "display": "Hypertension"}], "text": "Hypertension"},
    "subject": {"reference": "Patient/demo-patient-john"}
  },
  {
    "resourceType": "Encounter",
    "id": "demo-encounter",
    "status": "finished",
    "class": {"system": "http://terminology.hl7.org/CodeSystem/v3-ActCode", "code": "AMB", "display": "ambulatory"},
    "subject": {"reference": "Patient/demo-patient-john"},
    "period": {"start": "2026-01-15T09:00:00Z", "end": "2026-01-15T09:30:00Z"}
  },
  {
    "resourceType": "MedicationRequest",
    "id": "demo-medication",
    "status": "active",
    "intent": "order",
    "medicationCodeableConcept": {"coding": [{"system": "http://www.nlm.nih.gov/research/umls/rxnorm", "code": "197361", "display": "Amlodipine 5 MG Oral Tablet"}], "text": "Amlodipine 5 mg tablet"},
    "subject": {"reference": "Patient/demo-patient-john"}
  }
]"##;

/// `POST /$demo` — load the curated sample dataset (idempotent upsert).
pub async fn load_demo(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match load_demo_into(&state).await {
        Ok((loaded, errors)) => (
            StatusCode::OK,
            Json(json!({
                "loaded": loaded,
                "errors": errors,
                "message": format!("{loaded} sample resources loaded")
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        ),
    }
}

/// Load the curated sample dataset into the store + index (idempotent upsert).
/// Shared by the `POST /$demo` endpoint and the `--demo` startup flag. Returns
/// `(loaded_count, per_resource_errors)`.
pub async fn load_demo_into(state: &AppState) -> Result<(usize, Vec<String>), String> {
    let resources: Vec<Value> =
        serde_json::from_str(DEMO_RESOURCES).map_err(|e| format!("demo data is corrupt: {e}"))?;
    Ok(load_resources_into(state, &resources).await)
}

/// Seed the server from an external dataset file *only if the store is empty*.
/// Wired to the `SAZARE_SEED_ON_EMPTY=<path>` env var so `docker run` (or a bare
/// binary) can come up already populated — without baking the data into the
/// binary. The file may be a FHIR transaction/collection Bundle, a JSON array of
/// resources, or a single resource; references are upserted by fixed id, so the
/// dataset should use stable ids (as `examples/demo/cohort.json` does).
///
/// Returns `Ok(None)` if the store already had data (seeding skipped), or
/// `Ok(Some((loaded, errors)))` after a seed attempt.
pub async fn seed_from_file_if_empty(
    state: &AppState,
    path: &str,
) -> Result<Option<(usize, Vec<String>)>, String> {
    let non_empty = state
        .store
        .count_by_type()
        .map_err(|e| format!("cannot read store: {e}"))?
        .iter()
        .any(|(_, n)| *n > 0);
    if non_empty {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|e| format!("{path} is not JSON: {e}"))?;
    let resources = resources_from_dataset(parsed);
    if resources.is_empty() {
        return Err(format!("{path} contained no resources"));
    }
    Ok(Some(load_resources_into(state, &resources).await))
}

/// Extract the resources to load from a seed dataset value: a Bundle's
/// `entry[].resource`, a top-level JSON array, or a single resource object.
fn resources_from_dataset(value: Value) -> Vec<Value> {
    if value.get("resourceType").and_then(|v| v.as_str()) == Some("Bundle") {
        return value
            .get("entry")
            .and_then(|e| e.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| e.get("resource").cloned())
                    .collect()
            })
            .unwrap_or_default();
    }
    match value {
        Value::Array(items) => items,
        obj @ Value::Object(_) if obj.get("resourceType").is_some() => vec![obj],
        _ => vec![],
    }
}

/// Validate, upsert (fixed version 1), and index each resource. Shared by the
/// embedded `--demo` set and the `SAZARE_SEED_ON_EMPTY` file loader. Returns
/// `(loaded_count, per_resource_errors)`.
async fn load_resources_into(state: &AppState, resources: &[Value]) -> (usize, Vec<String>) {
    let mut loaded = 0;
    let mut errors: Vec<String> = Vec::new();

    for resource in resources {
        let (rt, id) = match (
            resource.get("resourceType").and_then(|v| v.as_str()),
            resource.get("id").and_then(|v| v.as_str()),
        ) {
            (Some(rt), Some(id)) => (rt.to_string(), id.to_string()),
            _ => continue,
        };

        // The demo set is curated to be valid; surface a failure loudly so a
        // regression in the sample data is caught (the e2e test asserts none).
        if let Err(outcome) = validate_resource_all_phases(
            resource,
            &state.profile_registry,
            &state.terminology_registry,
        ) {
            let diag = outcome
                .issue
                .first()
                .and_then(|i| i.diagnostics.as_deref())
                .unwrap_or("validation failed")
                .to_string();
            errors.push(format!("{rt}/{id}: {diag}"));
            continue;
        }

        // Stamp meta and upsert (fixed version so repeated loads just refresh).
        let mut stored = resource.clone();
        if let Some(obj) = stored.as_object_mut() {
            crate::handlers::merge_version_meta(obj, "1");
        }
        let data = match serde_json::to_vec(&stored) {
            Ok(d) => d,
            Err(e) => {
                errors.push(format!("{rt}/{id}: {e}"));
                continue;
            }
        };
        if let Err(e) = state.store.put_with_version(&rt, &id, "1", &data) {
            errors.push(format!("{rt}/{id}: {e}"));
            continue;
        }

        let indices = IndexBuilder::extract_indices_with_registry(&state.search_param_registry, &rt, &stored);
        let index = state.index.lock().await;
        let _ = index.remove_index(&rt, &id);
        for (param_name, param_type, value, system) in indices {
            let _ = index.add_index(&rt, &id, &param_name, &param_type, Some(&value), system.as_deref());
        }
        drop(index);
        loaded += 1;
    }

    (loaded, errors)
}
