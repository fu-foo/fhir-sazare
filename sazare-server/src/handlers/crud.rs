use axum::{
    extract::{Path, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
};
use http_body_util::BodyExt;
use sazare_core::{
    operation_outcome::IssueType,
    validation::validate_resource_all_phases,
    Meta, OperationOutcome, Resource,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::audit::{self, AuditContext};
use crate::auth::AuthUser;
use crate::compartment_check::check_compartment_access;
use crate::subscription::{self, SubscriptionManager};
use crate::{AppState, ConditionalResult};
use super::{response_with_etag, extract_version, update_search_index};

/// Extract headers and JSON body from a Request
async fn extract_body(request: Request) -> Result<(axum::http::HeaderMap, Value), (StatusCode, Json<Value>)> {
    let (parts, body) = request.into_parts();
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

    Ok((parts.headers, value))
}

/// Create resource (POST /{resource_type})
pub async fn create(
    State(state): State<Arc<AppState>>,
    Path(resource_type): Path<String>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();
    let (headers, body) = extract_body(request).await?;

    // Compartment check: patient-scoped tokens can only create resources in their compartment
    check_compartment_access(auth_user.as_ref(), &state.compartment_def, &resource_type, &body)?;

    // Conditional create: If-None-Exist header
    if let Some(if_none_exist) = headers.get("If-None-Exist").and_then(|v| v.to_str().ok()) {
        match crate::conditional_create_check(&state, &resource_type, if_none_exist).await {
            ConditionalResult::Exists(existing) => {
                return Ok(response_with_etag(StatusCode::OK, existing).into_response());
            }
            ConditionalResult::MultipleMatches => {
                return Err((
                    StatusCode::PRECONDITION_FAILED,
                    Json(json!(OperationOutcome::error(
                        IssueType::MultipleMatches,
                        "Multiple matches for If-None-Exist"
                    ))),
                ));
            }
            ConditionalResult::SearchError(e) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!(OperationOutcome::error(IssueType::Processing, e))),
                ));
            }
            ConditionalResult::NoMatch => { /* proceed */ }
        }
    }

    // Validate
    if let Err(outcome) = validate_resource_all_phases(
        &body,
        &state.profile_registry,
        &state.terminology_registry,
    ) {
        return Err((StatusCode::BAD_REQUEST, Json(json!(outcome))));
    }

    // Subscription-specific validation
    if resource_type == "Subscription"
        && let Err(e) = subscription::validate_subscription(&body, &state.search_param_registry)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e))),
        ));
    }

    let mut resource: Resource = serde_json::from_value(body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e.to_string()))),
        )
    })?;

    // Verify resourceType matches
    if resource.resource_type != resource_type {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::Invalid,
                "resourceType mismatch"
            ))),
        ));
    }

    // Generate ID
    let id = resource
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    resource.id = Some(id.clone());

    // Set metadata
    let version_id = "1".to_string();
    let now = chrono::Utc::now().to_rfc3339();
    resource.meta = Some(Meta {
        version_id: Some(version_id.clone()),
        last_updated: Some(now),
        ..Default::default()
    });

    // Save
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

    // Update search index
    let resource_value = serde_json::to_value(&resource).unwrap_or_default();
    {
        let index = state.index.lock().await;
        update_search_index(&index, &state.search_param_registry, &resource_type, &id, &resource_value);
    }

    // Audit log
    audit::log_operation_success(&audit_ctx, "CREATE", &resource_type, &id, &state.audit);

    // Subscription notification (background)
    {
        let state = state.clone();
        let rt = resource_type.clone();
        let rid = id.clone();
        let rv = resource_value.clone();
        tokio::spawn(async move {
            SubscriptionManager::notify(&state, &rt, &rid, &rv).await;
        });
    }

    Ok(response_with_etag(StatusCode::CREATED, resource_value).into_response())
}

/// Read resource (GET /{resource_type}/{id})
pub async fn read(
    State(state): State<Arc<AppState>>,
    Path((resource_type, id)): Path<(String, String)>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();

    match state.store.get(&resource_type, &id) {
        Ok(Some(data)) => {
            let resource: Value = serde_json::from_slice(&data).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!(OperationOutcome::storage_error(e.to_string()))),
                )
            })?;

            // Compartment check
            check_compartment_access(auth_user.as_ref(), &state.compartment_def, &resource_type, &resource)?;

            audit::log_operation_success(&audit_ctx, "READ", &resource_type, &id, &state.audit);
            Ok(response_with_etag(StatusCode::OK, resource).into_response())
        }
        Ok(None) => Err((
            StatusCode::NOT_FOUND,
            Json(json!(OperationOutcome::not_found(&resource_type, &id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!(OperationOutcome::storage_error(e.to_string()))),
        )),
    }
}

/// Update resource (PUT /{resource_type}/{id})
pub async fn update(
    State(state): State<Arc<AppState>>,
    Path((resource_type, id)): Path<(String, String)>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();
    let (headers, body) = extract_body(request).await?;

    // Validate
    if let Err(outcome) = validate_resource_all_phases(
        &body,
        &state.profile_registry,
        &state.terminology_registry,
    ) {
        return Err((StatusCode::BAD_REQUEST, Json(json!(outcome))));
    }

    // Subscription-specific validation
    if resource_type == "Subscription"
        && let Err(e) = subscription::validate_subscription(&body, &state.search_param_registry)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e))),
        ));
    }

    let mut resource: Resource = serde_json::from_value(body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e.to_string()))),
        )
    })?;

    // If-Match header (optimistic locking)
    let if_match = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').trim_start_matches("W/\"").trim_end_matches('"').to_string());

    // Get existing resource and compute new version
    let new_version = match state.store.get(&resource_type, &id) {
        Ok(Some(data)) => {
            let existing: Value = serde_json::from_slice(&data).unwrap_or_default();

            // Compartment check on existing resource
            check_compartment_access(auth_user.as_ref(), &state.compartment_def, &resource_type, &existing)?;

            let current_ver_str = existing
                .get("meta")
                .and_then(|m| m.get("versionId"))
                .and_then(|v| v.as_str())
                .unwrap_or("0");

            // If-Match check
            if let Some(ref expected) = if_match
                && expected != current_ver_str
            {
                return Err((
                    StatusCode::CONFLICT,
                    Json(json!(OperationOutcome::error(
                        IssueType::Conflict,
                        format!(
                            "Version conflict: expected {}, current is {}",
                            expected, current_ver_str
                        )
                    ))),
                ));
            }

            let current_ver: i32 = current_ver_str.parse().unwrap_or(0);
            (current_ver + 1).to_string()
        }
        Ok(None) => "1".to_string(),
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e.to_string()))),
            ))
        }
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

    // Update search index
    let resource_value = serde_json::to_value(&resource).unwrap_or_default();
    {
        let index = state.index.lock().await;
        update_search_index(&index, &state.search_param_registry, &resource_type, &id, &resource_value);
    }

    audit::log_operation_success(&audit_ctx, "UPDATE", &resource_type, &id, &state.audit);

    // Subscription notification (background)
    {
        let state = state.clone();
        let rt = resource_type.clone();
        let rid = id.clone();
        let rv = resource_value.clone();
        tokio::spawn(async move {
            SubscriptionManager::notify(&state, &rt, &rid, &rv).await;
        });
    }

    Ok(response_with_etag(StatusCode::OK, resource_value).into_response())
}

/// JSON PATCH (PATCH /{resource_type}/{id})
pub async fn patch_resource(
    State(state): State<Arc<AppState>>,
    Path((resource_type, id)): Path<(String, String)>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();
    let (headers, patch_body) = extract_body(request).await?;

    // Get existing resource
    let data = match state.store.get(&resource_type, &id) {
        Ok(Some(data)) => data,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!(OperationOutcome::not_found(&resource_type, &id))),
            ))
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e.to_string()))),
            ))
        }
    };

    let mut resource: Value = serde_json::from_slice(&data).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!(OperationOutcome::storage_error(e.to_string()))),
        )
    })?;

    // Compartment check on existing resource
    check_compartment_access(auth_user.as_ref(), &state.compartment_def, &resource_type, &resource)?;

    // If-Match check
    let if_match = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').trim_start_matches("W/\"").trim_end_matches('"').to_string());

    let current_ver_str = extract_version(&resource).unwrap_or_else(|| "0".to_string());

    if let Some(ref expected) = if_match
        && expected != &current_ver_str
    {
        return Err((
            StatusCode::CONFLICT,
            Json(json!(OperationOutcome::error(
                IssueType::Conflict,
                format!("Version conflict: expected {}, current is {}", expected, current_ver_str)
            ))),
        ));
    }

    // Apply JSON Patch
    let patch_ops: json_patch::Patch = serde_json::from_value(patch_body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::Invalid,
                format!("Invalid JSON Patch: {}", e)
            ))),
        )
    })?;

    json_patch::patch(&mut resource, &patch_ops).map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!(OperationOutcome::error(
                IssueType::Processing,
                format!("Patch failed: {}", e)
            ))),
        )
    })?;

    // Validate patched resource
    if let Err(outcome) = validate_resource_all_phases(
        &resource,
        &state.profile_registry,
        &state.terminology_registry,
    ) {
        return Err((StatusCode::BAD_REQUEST, Json(json!(outcome))));
    }

    // Update version
    let current_ver: i32 = current_ver_str.parse().unwrap_or(0);
    let new_version = (current_ver + 1).to_string();

    if let Some(meta) = resource.get_mut("meta").and_then(|m| m.as_object_mut()) {
        meta.insert("versionId".to_string(), json!(new_version));
        meta.insert(
            "lastUpdated".to_string(),
            json!(chrono::Utc::now().to_rfc3339()),
        );
    }

    // Save
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

    // Update search index
    {
        let index = state.index.lock().await;
        update_search_index(&index, &state.search_param_registry, &resource_type, &id, &resource);
    }

    audit::log_operation_success(&audit_ctx, "PATCH", &resource_type, &id, &state.audit);

    // Subscription notification (background)
    {
        let state = state.clone();
        let rt = resource_type.clone();
        let rid = id.clone();
        let rv = resource.clone();
        tokio::spawn(async move {
            SubscriptionManager::notify(&state, &rt, &rid, &rv).await;
        });
    }

    Ok(response_with_etag(StatusCode::OK, resource).into_response())
}

/// Delete resource (DELETE /{resource_type}/{id})
pub async fn delete_resource(
    State(state): State<Arc<AppState>>,
    Path((resource_type, id)): Path<(String, String)>,
    request: Request,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();

    // Compartment check: load existing resource first
    if let Ok(Some(data)) = state.store.get(&resource_type, &id)
        && let Ok(resource) = serde_json::from_slice::<Value>(&data)
    {
        check_compartment_access(auth_user.as_ref(), &state.compartment_def, &resource_type, &resource)?;
    }

    match state.store.delete(&resource_type, &id) {
        Ok(true) => {
            // Remove search index
            let index = state.index.lock().await;
            let _ = index.remove_index(&resource_type, &id);
            drop(index);

            audit::log_operation_success(&audit_ctx, "DELETE", &resource_type, &id, &state.audit);
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(json!(OperationOutcome::not_found(&resource_type, &id))),
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!(OperationOutcome::storage_error(e.to_string()))),
        )),
    }
}
