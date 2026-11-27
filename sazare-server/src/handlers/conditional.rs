use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use http_body_util::BodyExt;
use sazare_core::{
    operation_outcome::IssueType,
    validation::validate_resource_all_phases,
    OperationOutcome, Resource, SearchQuery,
};
use sazare_store::SearchExecutor;
use serde_json::{json, Value};
use std::sync::Arc;

use super::{base_url_from_headers, response_with_etag, response_with_headers, update_search_index, version_location};
use crate::audit::{self, AuditContext};
use crate::auth::AuthUser;
use crate::compartment_check::check_compartment_write;
use crate::AppState;

/// Conditional update (PUT /{resource_type}?params)
///
/// - 0 matches → create new resource (201)
/// - 1 match → update that resource (200)
/// - multiple matches → 412 Precondition Failed
pub async fn conditional_update(
    State(state): State<Arc<AppState>>,
    Path(resource_type): Path<String>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();

    let (parts, body) = request.into_parts();
    // Use the raw query string so repeated parameters (AND) survive and values
    // are decoded exactly once.
    let query_string = parts.uri.query().unwrap_or("").to_string();
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
    // Index and store writes happen under the write gate (shared), so an index
    // switch never finds a write between its two steps. Taken once the body
    // is in: a slow upload must not hold up a switch (or, behind a waiting
    // switch, every other write).
    let _write_gate = state.write_gate.read().await;

    let body_value: Value = serde_json::from_slice(&bytes).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e.to_string()))),
        )
    })?;

    if query_string.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::Invalid,
                "Conditional update requires search parameters"
            ))),
        ));
    }

    let query = SearchQuery::parse_with_registry(&query_string, Some(&resource_type), &state.search_param_registry).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e))),
        )
    })?;

    // Search for matching resources
    let (match_id, is_create) = {
        let index = state.index.lock().await;
        let executor = SearchExecutor::new(&state.store, &index);
        // 0 / 1 / many: two matches decide.
        let ids = crate::visible_upto(&state, &executor, &index, auth_user.as_ref(), &resource_type, &query, 2)
            .map_err(|e| {
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

    // Compartment check, with the id it is stored under: the match's (the body
    // may have none), or for a create the body's (none: a new one).
    let mut as_stored = body_value.clone();
    if let (Some(id), Some(obj)) = (&match_id, as_stored.as_object_mut()) {
        obj.insert("id".into(), Value::String(id.clone()));
    }
    check_compartment_write(auth_user.as_ref(), &state.compartment_def, &resource_type, &as_stored, 'u')?;

    // Validate
    if let Err(outcome) = validate_resource_all_phases(
        &body_value,
        &state.profile_registry,
        &state.terminology_registry,
    ) {
        return Err((StatusCode::BAD_REQUEST, Json(json!(outcome))));
    }

    // Same Subscription rules as every other write path.
    if resource_type == "Subscription"
        && let Err(e) = crate::subscription::validate_subscription(&body_value, &state.search_param_registry)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e))),
        ));
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
                format!(
                    "The resource in the body is a {}, but the URL is for {}.",
                    resource.resource_type, resource_type
                )
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

        // A body id can name a resource that exists but didn't match the
        // criteria: it is then an update of that resource — held to the same
        // compartment check, and its version continues instead of resetting.
        let version_id = match state.store.get(&resource_type, &id) {
            Ok(Some(data)) => {
                let existing: Value = serde_json::from_slice(&data).unwrap_or_default();
                check_compartment_write(auth_user.as_ref(), &state.compartment_def, &resource_type, &existing, 'u')?;
                let current: i32 = existing
                    .get("meta")
                    .and_then(|m| m.get("versionId"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                (current + 1).to_string()
            }
            _ => {
                // A deleted resource's id: the one it was must be the caller's.
                if let Some(was) = super::crud::deleted_content(&state, &resource_type, &id) {
                    check_compartment_write(auth_user.as_ref(), &state.compartment_def, &resource_type, &was, 'u')?;
                }
                state.store.next_version(&resource_type, &id).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!(OperationOutcome::storage_error(e.to_string())))))?
            }
        };
        // Preserve caller-provided meta (profile, source, …); only stamp the
        // server-managed versionId/lastUpdated.
        let mut meta = resource.meta.take().unwrap_or_default();
        meta.version_id = Some(version_id.clone());
        meta.last_updated = Some(chrono::Utc::now().to_rfc3339());
        resource.meta = Some(meta);

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
        super::crud::after_write(&state, &resource_type, &id, &resource_value);
        let location = version_location(&base_url_from_headers(&parts.headers), &resource_type, &id, &version_id);
        Ok(response_with_headers(StatusCode::CREATED, resource_value, Some(location)).into_response())
    } else {
        // 1 match → update
        let id = match_id.unwrap();

        let new_version = match state.store.get(&resource_type, &id) {
            Ok(Some(data)) => {
                let existing: Value = serde_json::from_slice(&data).unwrap_or_default();
                // The matched resource must be the caller's to replace.
                check_compartment_write(auth_user.as_ref(), &state.compartment_def, &resource_type, &existing, 'u')?;
                let current_ver: i32 = existing
                    .get("meta")
                    .and_then(|m| m.get("versionId"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                (current_ver + 1).to_string()
            }
            _ => state.store.next_version(&resource_type, &id).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!(OperationOutcome::storage_error(e.to_string())))))?,
        };

        resource.id = Some(id.clone());
        let mut meta = resource.meta.take().unwrap_or_default();
        meta.version_id = Some(new_version.clone());
        meta.last_updated = Some(chrono::Utc::now().to_rfc3339());
        resource.meta = Some(meta);

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
        super::crud::after_write(&state, &resource_type, &id, &resource_value);
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
    request: Request,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    // Index and store writes happen under the write gate (shared), so an index
    // switch never finds a write between its two steps.
    let _write_gate = state.write_gate.read().await;
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();

    let query_string = request.uri().query().unwrap_or("").to_string();

    if query_string.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::Invalid,
                "Conditional delete requires search parameters"
            ))),
        ));
    }

    let query = SearchQuery::parse_with_registry(&query_string, Some(&resource_type), &state.search_param_registry).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e))),
        )
    })?;

    let (ids, resource_to_check) = {
        let index = state.index.lock().await;
        let executor = SearchExecutor::new(&state.store, &index);
        // 0 / 1 / many: two matches decide.
        let ids = crate::visible_upto(&state, &executor, &index, auth_user.as_ref(), &resource_type, &query, 2)
            .map_err(|e| {
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
                check_compartment_write(auth_user.as_ref(), &state.compartment_def, &resource_type, resource, 'd')?;
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
