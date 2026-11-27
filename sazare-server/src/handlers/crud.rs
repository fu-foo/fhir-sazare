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
use crate::compartment_check::{check_compartment_access, check_compartment_write};
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
    // Index and store writes happen under the write gate (shared), so an index
    // switch never finds a write between its two steps. Taken once the body
    // is in: a slow upload must not hold up a switch (or, behind a waiting
    // switch, every other write).
    let _write_gate = state.write_gate.read().await;

    // Compartment check: patient-scoped tokens can only create resources in their compartment
    check_compartment_write(auth_user.as_ref(), &state.compartment_def, &resource_type, &body, 'c')?;

    // Conditional create: If-None-Exist header
    if let Some(if_none_exist) = headers.get("If-None-Exist").and_then(|v| v.to_str().ok()) {
        match crate::conditional_create_check(&state, auth_user.as_ref(), &resource_type, if_none_exist).await {
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
                format!(
                    "The resource in the body is a {}, but the URL is for {}. \
                     Post to /{} instead, or change the body's resourceType.",
                    resource.resource_type, resource_type, resource.resource_type
                )
            ))),
        ));
    }

    // The server assigns the id: one in the body is ignored, as R4 specifies
    // (and as transaction and batch entries always did). To choose the id,
    // PUT to it.
    let id = uuid::Uuid::new_v4().to_string();
    resource.id = Some(id.clone());
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

    // Compare-and-swap on "absent": two creates racing for the same
    // client-chosen id can't both succeed.
    let created = state
        .store
        .put_with_version_cas(&resource_type, &id, None, &version_id, &json_bytes)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e.to_string()))),
            )
        })?;
    if !created {
        // Put the index back to what is stored (or nothing, if the winner of
        // the race was since deleted).
        restore_index(&state, &resource_type, &id).await;
        return Err((
            StatusCode::CONFLICT,
            Json(json!(OperationOutcome::error(
                IssueType::Conflict,
                format!("{}/{} already exists; use PUT to update it", resource_type, id)
            ))),
        ));
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
        Ok(None) => {
            // A previously-deleted resource is Gone (410), not merely Not Found.
            if state.store.is_deleted(&resource_type, &id).unwrap_or(false) {
                return Err((
                    StatusCode::GONE,
                    Json(json!(OperationOutcome::error(
                        IssueType::Deleted,
                        format!("{}/{} has been deleted", resource_type, id)
                    ))),
                ));
            }
            Err((
                StatusCode::NOT_FOUND,
                Json(json!(OperationOutcome::not_found(&resource_type, &id))),
            ))
        }
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
    // Index and store writes happen under the write gate (shared), so an index
    // switch never finds a write between its two steps. Taken once the body
    // is in: a slow upload must not hold up a switch (or, behind a waiting
    // switch, every other write).
    let _write_gate = state.write_gate.read().await;

    // The new content must be in the caller's compartment too (the existing
    // version is checked below) — this also covers update-as-create.
    check_compartment_write(auth_user.as_ref(), &state.compartment_def, &resource_type, &body, 'u')?;

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

    let resource: Resource = serde_json::from_value(body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e.to_string()))),
        )
    })?;

    // The body must be the resource the URL names (FHIR: 400 otherwise).
    if resource.resource_type != resource_type {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::Invalid,
                format!(
                    "The resource in the body is a {}, but the URL is for {}",
                    resource.resource_type, resource_type
                )
            ))),
        ));
    }
    match resource.id.as_deref() {
        Some(body_id) if body_id != id => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!(OperationOutcome::error(
                    IssueType::Invalid,
                    format!("The body's id ({body_id}) differs from the URL's ({id})")
                ))),
            ));
        }
        _ => {}
    }

    // If-Match header (optimistic locking)
    let if_match = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').trim_start_matches("W/\"").trim_end_matches('"').to_string());

    // Without If-Match the last write wins: a write that loses the race to
    // another (compare-and-swap below) reads the new current version and
    // tries again.
    const MAX_ATTEMPTS: usize = 20;
    let _one_at_a_time = state.resource_locks.lock(&resource_type, &id).await;
    let body = resource;
    let mut attempt = 0;
    let (new_version, is_create, resource_value) = loop {
        attempt += 1;
        let mut resource = body.clone();
        // If-Match without a matching resource → 412. Get existing resource and
        // compute the new version, tracking whether this PUT creates a new resource
        // (update-as-create) so we can return the correct 201 vs 200 status.
        let (new_version, is_create, expected_current) = match state.store.get(&resource_type, &id) {
            Ok(Some(data)) => {
                let existing: Value = serde_json::from_slice(&data).unwrap_or_default();

                // Compartment check on existing resource
                check_compartment_write(auth_user.as_ref(), &state.compartment_def, &resource_type, &existing, 'u')?;

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
                // Continue the history of a deleted resource rather than
                // overwriting its version 1.
                let next = state.store.next_version(&resource_type, &id).map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!(OperationOutcome::storage_error(e.to_string()))),
                    )
                })?;
                (next, true, None)
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
        if written {
            break (new_version, is_create, resource_value);
        }
        // A concurrent write got there first: put the index back to what is
        // stored, then read again (an If-Match named a version no longer
        // current: 412).
        restore_index(&state, &resource_type, &id).await;
        if if_match.is_some() {
            return Err((
                StatusCode::PRECONDITION_FAILED,
                Json(json!(OperationOutcome::error(IssueType::Conflict, "Version conflict: the resource changed meanwhile"))),
            ));
        }
        if attempt >= MAX_ATTEMPTS {
            return Err((
                StatusCode::CONFLICT,
                Json(json!(OperationOutcome::error(
                    IssueType::Conflict,
                    "Resource was modified concurrently; retry the update"
                ))),
            ));
        }
    };

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
    // JSON Patch only: a FHIRPath Patch (a Parameters resource,
    // application/fhir+json) would fail as "invalid JSON Patch".
    let media = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase());
    if let Some(media) = media
        && media != "application/json-patch+json"
        && media != "application/json"
    {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(json!(OperationOutcome::error(
                IssueType::NotSupported,
                format!("PATCH takes a JSON Patch (Content-Type: application/json-patch+json), not {media}; FHIRPath Patch is not supported")
            ))),
        ));
    }
    // Index and store writes happen under the write gate (shared), so an index
    // switch never finds a write between its two steps. Taken once the body
    // is in: a slow upload must not hold up a switch (or, behind a waiting
    // switch, every other write).
    let _write_gate = state.write_gate.read().await;

    // A patch applies to the current version: one that loses the race to
    // another write (compare-and-swap below) is applied again to the new one.
    const MAX_ATTEMPTS: usize = 20;
    let _one_at_a_time = state.resource_locks.lock(&resource_type, &id).await;
    let mut attempt = 0;
    let resource = loop {
        attempt += 1;
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
        check_compartment_write(auth_user.as_ref(), &state.compartment_def, &resource_type, &resource, 'u')?;

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
        let patch_ops: json_patch::Patch = serde_json::from_value(patch_body.clone()).map_err(|e| {
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

        // A patch may change content, not identity.
        if resource.get("resourceType").and_then(|v| v.as_str()) != Some(resource_type.as_str())
            || resource.get("id").and_then(|v| v.as_str()) != Some(id.as_str())
        {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!(OperationOutcome::error(
                    IssueType::Processing,
                    "A patch may not change the resource's resourceType or id"
                ))),
            ));
        }

        // The patched result must stay in the caller's compartment.
        check_compartment_write(auth_user.as_ref(), &state.compartment_def, &resource_type, &resource, 'u')?;

        // Same rules for a patched Subscription as for a PUT one.
        if resource_type == "Subscription"
            && let Err(e) = subscription::validate_subscription(&resource, &state.search_param_registry)
        {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!(OperationOutcome::error(IssueType::Invalid, e))),
            ));
        }

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

        // Stamp meta even when the patch removed it.
        if let Some(obj) = resource.as_object_mut() {
            super::merge_version_meta(obj, &new_version);
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

        // Compare-and-swap against the version the patch was applied to: a
        // concurrent update in between makes this a 409, not a lost update.
        let written = state
            .store
            .put_with_version_cas(&resource_type, &id, Some(&current_ver_str), &new_version, &json_bytes)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!(OperationOutcome::storage_error(e.to_string()))),
                )
            })?;
        if written {
            break resource;
        }
        // A concurrent write got there first: put the index back to what is
        // stored, then apply the patch to the new current version (an If-Match
        // named one no longer current: 412).
        restore_index(&state, &resource_type, &id).await;
        if if_match.is_some() {
            return Err((
                StatusCode::PRECONDITION_FAILED,
                Json(json!(OperationOutcome::error(IssueType::Conflict, "Version conflict: the resource changed meanwhile"))),
            ));
        }
        if attempt >= MAX_ATTEMPTS {
            return Err((
                StatusCode::CONFLICT,
                Json(json!(OperationOutcome::error(
                    IssueType::Conflict,
                    "The resource changed while the patch was applied; retry"
                ))),
            ));
        }
    };

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
    // Index and store writes happen under the write gate (shared), so an index
    // switch never finds a write between its two steps.
    let _write_gate = state.write_gate.read().await;
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();

    // Compartment check: load existing resource first
    if let Ok(Some(data)) = state.store.get(&resource_type, &id)
        && let Ok(resource) = serde_json::from_slice::<Value>(&data)
    {
        check_compartment_write(auth_user.as_ref(), &state.compartment_def, &resource_type, &resource, 'd')?;
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

/// After a lost compare-and-swap: make the index match what is stored again —
/// the stored resource's entries, or none if it no longer exists.
/// After a resource was written: Subscription notifications (in the
/// background) and the lifecycle webhook. Every write path calls it.
pub(crate) fn after_write(state: &Arc<AppState>, resource_type: &str, id: &str, resource: &Value) {
    {
        let state = state.clone();
        let rt = resource_type.to_string();
        let rid = id.to_string();
        let rv = resource.clone();
        tokio::spawn(async move {
            SubscriptionManager::notify(&state, &rt, &rid, &rv).await;
        });
    }
    state.webhook.maybe_task_completed(resource);
}

pub(crate) async fn restore_index(state: &Arc<AppState>, resource_type: &str, id: &str) {
    let stored = state
        .store
        .get(resource_type, id)
        .ok()
        .flatten()
        .and_then(|data| serde_json::from_slice::<Value>(&data).ok());
    let index = state.index.lock().await;
    match stored {
        Some(current) => update_search_index(&index, &state.search_param_registry, resource_type, id, &current),
        None => {
            let _ = index.remove_index(resource_type, id);
        }
    }
}
