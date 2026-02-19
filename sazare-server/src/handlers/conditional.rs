use axum::{
    extract::{Path, Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use http_body_util::BodyExt;
use sazare_core::{
    operation_outcome::IssueType,
    validation::validate_resource_all_phases,
    Meta, OperationOutcome, Resource, SearchQuery,
};
use sazare_store::SearchExecutor;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{response_with_etag, update_search_index};
use crate::audit::{self, AuditContext};
use crate::auth::AuthUser;
use crate::compartment_check::check_compartment_access;
use crate::handlers::search::SearchParams;
use crate::AppState;

/// Conditional update (PUT /{resource_type}?params)
///
/// - 0 matches → create new resource (201)
/// - 1 match → update that resource (200)
/// - multiple matches → 412 Precondition Failed
pub async fn conditional_update(
    State(state): State<Arc<AppState>>,
    Path(resource_type): Path<String>,
    Query(params): Query<SearchParams>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();

    let (_parts, body) = request.into_parts();
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

    let body_value: Value = serde_json::from_slice(&bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e.to_string()))),
        )
    })?;

    // Build search query from params
    let query_string: String = params
        .params
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&");

    if query_string.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::Invalid,
                "Conditional update requires search parameters"
            ))),
        ));
    }

    let query = SearchQuery::parse(&query_string).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e))),
        )
    })?;

    // Search for matching resources
    let (match_id, is_create) = {
        let index = state.index.lock().await;
        let executor = SearchExecutor::new(&state.store, &index);
        let ids = executor.search(&resource_type, &query).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e))),
            )
        })?;

        match ids.len() {
            0 => (None, true),
            1 => (Some(ids.into_iter().next().unwrap()), false),
            _ => {
                return Err((
                    StatusCode::PRECONDITION_FAILED,
                    Json(json!(OperationOutcome::error(
                        IssueType::MultipleMatches,
                        "Multiple matches found for conditional update"
                    ))),
                ));
            }
        }
    };

    // Compartment check
    check_compartment_access(auth_user.as_ref(), &state.compartment_def, &resource_type, &body_value)?;

    // Validate
    if let Err(outcome) = validate_resource_all_phases(
        &body_value,
        &state.profile_registry,
        &state.terminology_registry,
    ) {
        return Err((StatusCode::BAD_REQUEST, Json(json!(outcome))));
    }

    let mut resource: Resource = serde_json::from_value(body_value).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e.to_string()))),
        )
    })?;

    if resource.resource_type != resource_type {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::Invalid,
                "resourceType mismatch"
            ))),
        ));
    }

    if is_create {
        // 0 matches → create
        let id = resource
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        resource.id = Some(id.clone());

        let version_id = "1".to_string();
        resource.meta = Some(Meta {
            version_id: Some(version_id.clone()),
            last_updated: Some(chrono::Utc::now().to_rfc3339()),
            ..Default::default()
        });

        let json_bytes = serde_json::to_vec(&resource).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e.to_string()))),
            )
        })?;

        state
            .store
            .put_with_version(&resource_type, &id, &version_id, &json_bytes)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!(OperationOutcome::storage_error(e.to_string()))),
                )
            })?;

        let resource_value = serde_json::to_value(&resource).unwrap_or_default();
        {
            let index = state.index.lock().await;
            update_search_index(&index, &state.search_param_registry, &resource_type, &id, &resource_value);
        }

        audit::log_operation_success(&audit_ctx, "CREATE", &resource_type, &id, &state.audit);
        Ok(response_with_etag(StatusCode::CREATED, resource_value).into_response())
    } else {
        // 1 match → update
        let id = match_id.unwrap();

        let new_version = match state.store.get(&resource_type, &id) {
            Ok(Some(data)) => {
                let existing: Value = serde_json::from_slice(&data).unwrap_or_default();
                let current_ver: i32 = existing
                    .get("meta")
                    .and_then(|m| m.get("versionId"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                (current_ver + 1).to_string()
            }
            _ => "1".to_string(),
        };

        resource.id = Some(id.clone());
        resource.meta = Some(Meta {
            version_id: Some(new_version.clone()),
            last_updated: Some(chrono::Utc::now().to_rfc3339()),
            ..Default::default()
        });

        let json_bytes = serde_json::to_vec(&resource).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e.to_string()))),
            )
        })?;

        state
            .store
            .put_with_version(&resource_type, &id, &new_version, &json_bytes)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!(OperationOutcome::storage_error(e.to_string()))),
                )
            })?;

        let resource_value = serde_json::to_value(&resource).unwrap_or_default();
        {
            let index = state.index.lock().await;
            update_search_index(&index, &state.search_param_registry, &resource_type, &id, &resource_value);
        }

        audit::log_operation_success(&audit_ctx, "UPDATE", &resource_type, &id, &state.audit);
        Ok(response_with_etag(StatusCode::OK, resource_value).into_response())
    }
}

/// Conditional delete (DELETE /{resource_type}?params)
///
/// - 0 matches → 204 No Content (success, nothing to delete)
/// - 1 match → delete + 204 No Content
/// - multiple matches → 412 Precondition Failed
pub async fn conditional_delete(
    State(state): State<Arc<AppState>>,
    Path(resource_type): Path<String>,
    Query(params): Query<SearchParams>,
    request: Request,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();

    let query_string: String = params
        .params
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&");

    if query_string.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::Invalid,
                "Conditional delete requires search parameters"
            ))),
        ));
    }

    let query = SearchQuery::parse(&query_string).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e))),
        )
    })?;

    let (ids, resource_to_check) = {
        let index = state.index.lock().await;
        let executor = SearchExecutor::new(&state.store, &index);
        let ids = executor.search(&resource_type, &query).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e))),
            )
        })?;

        // Load the resource for compartment check if exactly 1 match
        let resource = if ids.len() == 1 {
            executor.load_resources(&resource_type, &ids).ok().and_then(|r| r.into_iter().next())
        } else {
            None
        };

        (ids, resource)
    };

    match ids.len() {
        0 => Ok(StatusCode::NO_CONTENT),
        1 => {
            let id = &ids[0];

            // Compartment check
            if let Some(ref resource) = resource_to_check {
                check_compartment_access(auth_user.as_ref(), &state.compartment_def, &resource_type, resource)?;
            }

            state.store.delete(&resource_type, id).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!(OperationOutcome::storage_error(e.to_string()))),
                )
            })?;

            // Remove from index
            let index = state.index.lock().await;
            let _ = index.remove_index(&resource_type, id);

            audit::log_operation_success(&audit_ctx, "DELETE", &resource_type, id, &state.audit);
            Ok(StatusCode::NO_CONTENT)
        }
        _ => Err((
            StatusCode::PRECONDITION_FAILED,
            Json(json!(OperationOutcome::error(
                IssueType::MultipleMatches,
                "Multiple matches found for conditional delete"
            ))),
        )),
    }
}
