use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use http_body_util::BodyExt;
use sazare_core::{
    operation_outcome::IssueType,
    validation::validate_resource_all_phases,
    OperationOutcome,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

/// $validate operation (POST /{resource_type}/$validate)
///
/// Always returns 200 OK with an OperationOutcome.
/// Success: severity=information, Failure: severity=error.
pub async fn validate(
    State(state): State<Arc<AppState>>,
    Path(resource_type): Path<String>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let body = request
        .into_body();
    let bytes = body
        .collect()
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!(OperationOutcome::error(IssueType::Invalid, e.to_string()))),
            )
        })?
        .to_bytes();

    let value: Value = serde_json::from_slice(&bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e.to_string()))),
        )
    })?;

    // If wrapped in Parameters, extract the resource parameter
    let resource = if value.get("resourceType").and_then(|v| v.as_str()) == Some("Parameters") {
        extract_resource_from_parameters(&value).unwrap_or(value)
    } else {
        value
    };

    // Check resourceType matches the URL
    let body_type = resource
        .get("resourceType")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !body_type.is_empty() && body_type != resource_type {
        let outcome = json!({
            "resourceType": "OperationOutcome",
            "issue": [{
                "severity": "error",
                "code": "invalid",
                "diagnostics": format!(
                    "Resource type in body ({}) does not match URL ({})",
                    body_type, resource_type
                )
            }]
        });
        return Ok((StatusCode::OK, Json(outcome)).into_response());
    }

    // Run validation
    match validate_resource_all_phases(
        &resource,
        &state.profile_registry,
        &state.terminology_registry,
    ) {
        Ok(()) => {
            let outcome = json!({
                "resourceType": "OperationOutcome",
                "issue": [{
                    "severity": "information",
                    "code": "informational",
                    "diagnostics": "Validation successful"
                }]
            });
            Ok((StatusCode::OK, Json(outcome)).into_response())
        }
        Err(outcome) => {
            // $validate always returns 200 OK, even on validation failure
            Ok((StatusCode::OK, Json(json!(outcome))).into_response())
        }
    }
}

/// Extract a resource from a FHIR Parameters wrapper.
/// Looks for parameter with name "resource".
fn extract_resource_from_parameters(params: &Value) -> Option<Value> {
    params
        .get("parameter")
        .and_then(|p| p.as_array())
        .and_then(|arr| {
            arr.iter().find(|p| {
                p.get("name").and_then(|n| n.as_str()) == Some("resource")
            })
        })
        .and_then(|p| p.get("resource"))
        .cloned()
}
