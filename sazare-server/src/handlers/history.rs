use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use sazare_core::{operation_outcome::IssueType, OperationOutcome};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::auth::AuthUser;
use crate::compartment_check::check_compartment_access;
use crate::AppState;
use super::{base_url_from_headers, response_with_etag};

/// Get history (GET /{resource_type}/{id}/_history)
pub async fn history(
    State(state): State<Arc<AppState>>,
    Path((resource_type, id)): Path<(String, String)>,
    auth_user: Option<axum::extract::Extension<AuthUser>>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let auth_user = auth_user.map(|axum::extract::Extension(u)| u);
    let versions = state
        .store
        .list_versions(&resource_type, &id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e.to_string()))),
            )
        })?;

    let base = base_url_from_headers(&headers);
    let mut entries = Vec::new();
    for ver in versions {
        if let Ok(Some(data)) = state.store.get_version(&resource_type, &id, &ver)
            && let Ok(resource) = serde_json::from_slice::<Value>(&data)
        {
            // Every version is held to the same compartment rule as a read:
            // a version outside the caller's compartment denies the whole
            // history (a resource's subject can change between versions).
            check_compartment_access(auth_user.as_ref(), &state.compartment_def, &resource_type, &resource)?;
            let last_updated = resource
                .get("meta")
                .and_then(|m| m.get("lastUpdated"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Version "1" was the create (POST); later versions are updates (PUT).
            // entry.response is mandatory in a history bundle (invariant bdl-4).
            let method = if ver == "1" { "POST" } else { "PUT" };
            entries.push(json!({
                "fullUrl": format!("{base}/{resource_type}/{id}"),
                "resource": resource,
                "request": {
                    "method": method,
                    "url": format!("{resource_type}/{id}")
                },
                "response": {
                    "status": "200",
                    "etag": format!("W/\"{ver}\""),
                    "lastModified": last_updated
                }
            }));
        }
    }

    let mut bundle = json!({
        "resourceType": "Bundle",
        "type": "history",
        "total": entries.len(),
        "link": [{
            "relation": "self",
            "url": format!("{base}/{resource_type}/{id}/_history")
        }]
    });
    // FHIR forbids empty arrays: leave `entry` out when there are none.
    if !entries.is_empty() {
        bundle["entry"] = json!(entries);
    }
    Ok(super::fhir_json(StatusCode::OK, bundle))
}

/// Read specific version (GET /{resource_type}/{id}/_history/{vid})
pub async fn vread(
    State(state): State<Arc<AppState>>,
    Path((resource_type, id, vid)): Path<(String, String, String)>,
    auth_user: Option<axum::extract::Extension<AuthUser>>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let auth_user = auth_user.map(|axum::extract::Extension(u)| u);
    match state.store.get_version(&resource_type, &id, &vid) {
        Ok(Some(data)) => {
            let resource: Value = serde_json::from_slice(&data).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!(OperationOutcome::storage_error(e.to_string()))),
                )
            })?;
            check_compartment_access(auth_user.as_ref(), &state.compartment_def, &resource_type, &resource)?;
            Ok(response_with_etag(StatusCode::OK, resource).into_response())
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!(OperationOutcome::error(
                IssueType::NotFound,
                format!("{}/{}/_history/{} not found", resource_type, id, vid),
            ))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!(OperationOutcome::storage_error(e.to_string()))),
        )),
    }
}
