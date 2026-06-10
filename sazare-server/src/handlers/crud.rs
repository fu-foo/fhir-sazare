use axum::{
    extract::{Path, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Json, Response},
};
use http_body_util::BodyExt;
use sazare_core::{
    operation_outcome::IssueType,
    validation::validate_resource_all_phases,
    OperationOutcome, Resource,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::audit::{self, AuditContext};
use crate::auth::AuthUser;
use crate::compartment_check::check_compartment_access;
use crate::subscription::{self, SubscriptionManager};
use crate::{AppState, ConditionalResult};
use super::{
    base_url_from_headers, extract_version, response_with_etag, response_with_headers,
    update_search_index, version_location,
};

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

    // Assign the resource id. A client-supplied id is honoured (the server also
    // ingests pre-identified resources via bundles/import), but it must NOT
    // silently overwrite an existing resource — that destroys the current
    // version and its history. If the id already exists, reject with 409 and
    // direct the client to PUT (update) instead.
    let client_supplied_id = resource.id.clone();
    let id = client_supplied_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if client_supplied_id.is_some() {
        match state.store.get(&resource_type, &id) {
            Ok(Some(_)) => {
                return Err((
                    StatusCode::CONFLICT,
                    Json(json!(OperationOutcome::error(
                        IssueType::Conflict,
                        format!(
                            "{}/{} already exists; use PUT to update it",
                            resource_type, id
                        )
                    ))),
                ));
            }
            Ok(None) => {}
            Err(e) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!(OperationOutcome::storage_error(e.to_string()))),
                ))
            }
        }
    }
    resource.id = Some(id.clone());

    // Set metadata — preserve caller-provided fields (profile, source, tag, ...)
    let version_id = "1".to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let mut meta = resource.meta.take().unwrap_or_default();
    meta.version_id = Some(version_id.clone());
    meta.last_updated = Some(now);
    resource.meta = Some(meta);

    // Save
    let json_bytes = serde_json::to_vec(&resource).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!(OperationOutcome::storage_error(e.to_string()))),
        )
    })?;

    // Update the search index *before* persisting the resource. The store and
    // index are separate SQLite databases, so the two writes can't share a
    // transaction. Indexing first means a crash between them leaves at most a
    // stale index entry for a resource that isn't stored — harmless, since
    // search results are fetched from the store and a missing resource is
    // dropped. The reverse order risks a committed resource being invisible to
    // search until the next reindex (a false negative). reindex cleans up any
    // stale entries.
    let resource_value = serde_json::to_value(&resource).unwrap_or_default();
    {
        let index = state.index.lock().await;
        update_search_index(&index, &state.search_param_registry, &resource_type, &id, &resource_value);
    }

    state
        .store
        .put_with_version(&resource_type, &id, &version_id, &json_bytes)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e.to_string()))),
            )
        })?;

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

    // Lifecycle webhook: fire if this is a completed Task.
    state.webhook.maybe_task_completed(&resource_value);

    let location = version_location(&base_url_from_headers(&headers), &resource_type, &id, &version_id);
    Ok(response_with_headers(StatusCode::CREATED, resource_value, Some(location)).into_response())
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

    // If-Match without a matching resource → 412. Get existing resource and
    // compute the new version, tracking whether this PUT creates a new resource
    // (update-as-create) so we can return the correct 201 vs 200 status.
    let (new_version, is_create, expected_current) = match state.store.get(&resource_type, &id) {
        Ok(Some(data)) => {
            let existing: Value = serde_json::from_slice(&data).unwrap_or_default();

            // Compartment check on existing resource
            check_compartment_access(auth_user.as_ref(), &state.compartment_def, &resource_type, &existing)?;

            let current_ver_str = existing
                .get("meta")
                .and_then(|m| m.get("versionId"))
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .to_string();

            // If-Match check (precondition) → 412 Precondition Failed on mismatch.
            if let Some(ref expected) = if_match
                && expected != &current_ver_str
            {
                return Err((
                    StatusCode::PRECONDITION_FAILED,
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
            ((current_ver + 1).to_string(), false, Some(current_ver_str))
        }
        Ok(None) => {
            // If-Match supplied but nothing to match → precondition failed.
            if if_match.is_some() {
                return Err((
                    StatusCode::PRECONDITION_FAILED,
                    Json(json!(OperationOutcome::error(
                        IssueType::Conflict,
                        "If-Match supplied but resource does not exist"
                    ))),
                ));
            }
            ("1".to_string(), true, None)
        }
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e.to_string()))),
            ))
        }
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

    // Reindex before persisting (see the create handler: indexing first keeps a
    // committed resource from ever being invisible to search after a crash).
    let resource_value = serde_json::to_value(&resource).unwrap_or_default();
    {
        let index = state.index.lock().await;
        update_search_index(&index, &state.search_param_registry, &resource_type, &id, &resource_value);
    }

    // Compare-and-swap on the version we read: if a concurrent writer changed
    // the resource since, the write is refused (no lost update). On refusal,
    // restore the index to reflect the actually-stored resource before returning.
    let written = state
        .store
        .put_with_version_cas(&resource_type, &id, expected_current.as_deref(), &new_version, &json_bytes)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e.to_string()))),
            )
        })?;
    if !written {
        let index = state.index.lock().await;
        if let Ok(Some(cur)) = state.store.get(&resource_type, &id) {
            if let Ok(cur_val) = serde_json::from_slice::<Value>(&cur) {
                update_search_index(&index, &state.search_param_registry, &resource_type, &id, &cur_val);
            }
        } else {
            let _ = index.remove_index(&resource_type, &id);
        }
        return Err((
            StatusCode::CONFLICT,
            Json(json!(OperationOutcome::error(
                IssueType::Conflict,
                "Resource was modified concurrently; retry the update"
            ))),
        ));
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

    // Lifecycle webhook: fire if this is a completed Task.
    state.webhook.maybe_task_completed(&resource_value);

    let status = if is_create { StatusCode::CREATED } else { StatusCode::OK };
    let location = version_location(&base_url_from_headers(&headers), &resource_type, &id, &new_version);
    Ok(response_with_headers(status, resource_value, Some(location)).into_response())
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
            StatusCode::PRECONDITION_FAILED,
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

    // Reindex before persisting, consistent with create/update: a crash between
    // the two writes then leaves only a harmless stale index entry rather than a
    // committed resource that's invisible to search.
    {
        let index = state.index.lock().await;
        update_search_index(&index, &state.search_param_registry, &resource_type, &id, &resource);
    }

    state
        .store
        .put_with_version(&resource_type, &id, &new_version, &json_bytes)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e.to_string()))),
            )
        })?;

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

    // Lifecycle webhook: fire if this is a completed Task.
    state.webhook.maybe_task_completed(&resource);

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
