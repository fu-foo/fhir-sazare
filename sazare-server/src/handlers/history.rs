use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use sazare_core::{operation_outcome::IssueType, OperationOutcome};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;
use super::response_with_etag;

/// Get history (GET /{resource_type}/{id}/_history)
pub async fn history(
    State(state): State<Arc<AppState>>,
    Path((resource_type, id)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let versions = state
        .store
        .list_versions(&resource_type, &id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e.to_string()))),
            )
        })?;

    let mut entries = Vec::new();
    for ver in versions {
        if let Ok(Some(data)) = state.store.get_version(&resource_type, &id, &ver)
            && let Ok(resource) = serde_json::from_slice::<Value>(&data)
        {
            entries.push(json!({
                "resource": resource,
                "request": {
                    "method": "GET",
                    "url": format!("{}/{}", resource_type, id)
                }
            }));
        }
    }

    Ok(Json(json!({
        "resourceType": "Bundle",
        "type": "history",
        "total": entries.len(),
        "entry": entries
    })))
}

/// Read specific version (GET /{resource_type}/{id}/_history/{vid})
pub async fn vread(
    State(state): State<Arc<AppState>>,
    Path((resource_type, id, vid)): Path<(String, String, String)>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    match state.store.get_version(&resource_type, &id, &vid) {
        Ok(Some(data)) => {
            let resource: Value = serde_json::from_slice(&data).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!(OperationOutcome::storage_error(e.to_string()))),
                )
            })?;
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
