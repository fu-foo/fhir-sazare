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
    // The `profile` to validate against may arrive as a query parameter
    // (`?profile=URL`) — capture it before the body is consumed.
    let query_profile = request
        .uri()
        .query()
        .and_then(|q| {
            q.split('&')
                .filter_map(|p| p.split_once('='))
                .find(|(k, _)| *k == "profile")
                .map(|(_, v)| v.to_string())
        })
        .and_then(|v| urldecode(&v));

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

    // The profile may also be supplied as a Parameters `profile` parameter.
    let param_profile = if value.get("resourceType").and_then(|v| v.as_str()) == Some("Parameters") {
        extract_profile_from_parameters(&value)
    } else {
        None
    };

    // If wrapped in Parameters, extract the resource parameter
    let mut resource = if value.get("resourceType").and_then(|v| v.as_str()) == Some("Parameters") {
        extract_resource_from_parameters(&value).unwrap_or(value)
    } else {
        value
    };

    // If a profile was requested, assert it on meta.profile so the profile-driven
    // phase-2 validation runs against it (a client validating against a specific
    // profile must not get a misleading "success" from auto-matching).
    if let Some(profile) = query_profile.or(param_profile)
        && let Some(obj) = resource.as_object_mut()
    {
        let meta = obj
            .entry("meta".to_string())
            .or_insert_with(|| json!({}));
        if let Some(meta_obj) = meta.as_object_mut() {
            let profiles = meta_obj
                .entry("profile".to_string())
                .or_insert_with(|| json!([]));
            if let Some(arr) = profiles.as_array_mut()
                && !arr.iter().any(|p| p.as_str() == Some(profile.as_str()))
            {
                arr.push(json!(profile));
            }
        }
    }

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
        Ok(result) => {
            let mut issues = vec![json!({
                "severity": "information",
                "code": "informational",
                "diagnostics": "Validation successful"
            })];

            // Append any warnings from profile validation
            for warning in &result.warnings {
                issues.push(json!(warning));
            }

            let outcome = json!({
                "resourceType": "OperationOutcome",
                "issue": issues
            });
            Ok((StatusCode::OK, Json(outcome)).into_response())
        }
        Err(outcome) => {
            // $validate always returns 200 OK, even on validation failure
            Ok((StatusCode::OK, Json(json!(outcome))).into_response())
        }
    }
}

/// Minimal percent-decoding for the `profile` query value (covers `%XX` escapes).
fn urldecode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16)?;
                let lo = (bytes[i + 2] as char).to_digit(16)?;
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Extract a `profile` (valueUri/valueCanonical) from a Parameters wrapper.
fn extract_profile_from_parameters(params: &Value) -> Option<String> {
    params
        .get("parameter")
        .and_then(|p| p.as_array())?
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("profile"))
        .and_then(|p| {
            p.get("valueUri")
                .or_else(|| p.get("valueCanonical"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
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
