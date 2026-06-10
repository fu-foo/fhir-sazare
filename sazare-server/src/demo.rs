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
    "id": "demo-yamada",
    "name": [{"family": "山田", "given": ["太郎"], "text": "山田 太郎"}],
    "gender": "male",
    "birthDate": "1980-05-12",
    "telecom": [{"system": "phone", "value": "03-1234-5678", "use": "home"}]
  },
  {
    "resourceType": "Patient",
    "id": "demo-suzuki",
    "name": [{"family": "鈴木", "given": ["花子"], "text": "鈴木 花子"}],
    "gender": "female",
    "birthDate": "1992-11-03"
  },
  {
    "resourceType": "Observation",
    "id": "demo-bodyweight",
    "status": "final",
    "category": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/observation-category", "code": "vital-signs"}]}],
    "code": {"coding": [{"system": "http://loinc.org", "code": "29463-7", "display": "Body weight"}], "text": "体重"},
    "subject": {"reference": "Patient/demo-yamada"},
    "effectiveDateTime": "2026-01-15",
    "valueQuantity": {"value": 68.5, "unit": "kg", "system": "http://unitsofmeasure.org", "code": "kg"}
  },
  {
    "resourceType": "Observation",
    "id": "demo-heartrate",
    "status": "final",
    "category": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/observation-category", "code": "vital-signs"}]}],
    "code": {"coding": [{"system": "http://loinc.org", "code": "8867-4", "display": "Heart rate"}], "text": "心拍数"},
    "subject": {"reference": "Patient/demo-yamada"},
    "effectiveDateTime": "2026-01-15",
    "valueQuantity": {"value": 72, "unit": "beats/minute", "system": "http://unitsofmeasure.org", "code": "/min"}
  },
  {
    "resourceType": "Condition",
    "id": "demo-condition",
    "clinicalStatus": {"coding": [{"system": "http://terminology.hl7.org/CodeSystem/condition-clinical", "code": "active"}]},
    "verificationStatus": {"coding": [{"system": "http://terminology.hl7.org/CodeSystem/condition-ver-status", "code": "confirmed"}]},
    "code": {"coding": [{"system": "http://snomed.info/sct", "code": "38341003", "display": "Hypertension"}], "text": "高血圧"},
    "subject": {"reference": "Patient/demo-yamada"}
  },
  {
    "resourceType": "Encounter",
    "id": "demo-encounter",
    "status": "finished",
    "class": {"system": "http://terminology.hl7.org/CodeSystem/v3-ActCode", "code": "AMB", "display": "ambulatory"},
    "subject": {"reference": "Patient/demo-yamada"},
    "period": {"start": "2026-01-15T09:00:00+09:00", "end": "2026-01-15T09:30:00+09:00"}
  },
  {
    "resourceType": "MedicationRequest",
    "id": "demo-medication",
    "status": "active",
    "intent": "order",
    "medicationCodeableConcept": {"coding": [{"system": "http://www.nlm.nih.gov/research/umls/rxnorm", "code": "197361", "display": "Amlodipine 5 MG Oral Tablet"}], "text": "アムロジピン 5mg 錠"},
    "subject": {"reference": "Patient/demo-yamada"}
  }
]"##;

/// `POST /$demo` — load the curated sample dataset (idempotent upsert).
pub async fn load_demo(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let resources: Vec<Value> = match serde_json::from_str(DEMO_RESOURCES) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("demo data is corrupt: {e}")})),
            )
        }
    };

    let mut loaded = 0;
    let mut errors: Vec<String> = Vec::new();

    for resource in &resources {
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

    (
        StatusCode::OK,
        Json(json!({
            "loaded": loaded,
            "errors": errors,
            "message": format!("{loaded} sample resources loaded")
        })),
    )
}
