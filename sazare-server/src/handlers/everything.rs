use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use sazare_core::{
    operation_outcome::IssueType,
    OperationOutcome,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::audit::{self, AuditContext};
use crate::auth::AuthUser;
use crate::compartment_check::check_compartment_access;
use crate::AppState;

/// Patient $everything (GET /Patient/{id}/$everything)
///
/// Returns a searchset Bundle containing the Patient and all resources
/// in the Patient's compartment.
pub async fn patient_everything(
    State(state): State<Arc<AppState>>,
    Path((resource_type, patient_id)): Path<(String, String)>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();

    if resource_type != "Patient" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::NotSupported,
                format!("$everything is only supported for Patient, not {}", resource_type)
            ))),
        ));
    }

    // Load the Patient resource
    let patient_data = state.store.get("Patient", &patient_id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!(OperationOutcome::storage_error(e.to_string()))),
        )
    })?;

    let patient_data = patient_data.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!(OperationOutcome::not_found("Patient", &patient_id))),
        )
    })?;

    let patient: Value = serde_json::from_slice(&patient_data).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!(OperationOutcome::storage_error(e.to_string()))),
        )
    })?;

    // Compartment check: patient-scoped users can only access their own data
    check_compartment_access(auth_user.as_ref(), &state.compartment_def, "Patient", &patient)?;

    let mut entries: Vec<Value> = Vec::new();

    // Add the Patient itself
    entries.push(json!({
        "fullUrl": format!("Patient/{}", patient_id),
        "resource": patient,
        "search": {"mode": "match"}
    }));

    // Get all compartment resource types and their reference fields
    let index = state.index.lock().await;
    let patient_ref = format!("Patient/{}", patient_id);

    // Iterate over all compartment resource types
    let compartment_types = [
        "Observation", "Encounter", "Condition", "MedicationRequest",
        "Procedure", "AllergyIntolerance", "DiagnosticReport", "Immunization", "Task",
    ];

    for comp_type in &compartment_types {
        if let Some(ref_fields) = state.compartment_def.get_reference_fields(comp_type) {
            for field in ref_fields {
                let ids = index
                    .search_reference(comp_type, field, &patient_ref)
                    .unwrap_or_default();

                for id in &ids {
                    if let Ok(Some(data)) = state.store.get(comp_type, id)
                        && let Ok(resource) = serde_json::from_slice::<Value>(&data)
                    {
                        // Avoid duplicates (e.g. Task may match on both "for" and "owner")
                        let full_url = format!("{}/{}", comp_type, id);
                        if !entries.iter().any(|e| {
                            e.get("fullUrl").and_then(|v| v.as_str()) == Some(&full_url)
                        }) {
                            entries.push(json!({
                                "fullUrl": full_url,
                                "resource": resource,
                                "search": {"mode": "match"}
                            }));
                        }
                    }
                }
            }
        }
    }

    let total = entries.len();

    audit::log_operation_success(
        &audit_ctx,
        "$everything",
        "Patient",
        &format!("{}: {} resources", patient_id, total),
        &state.audit,
    );

    Ok(Json(json!({
        "resourceType": "Bundle",
        "type": "searchset",
        "total": total,
        "entry": entries
    })).into_response())
}
