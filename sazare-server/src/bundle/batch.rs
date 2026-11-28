//! Batch Bundle processing (each entry independent)

use super::{error_entry, BundleEntry};
use crate::audit::{self, AuditContext};
use crate::{conditional_create_check, ConditionalResult, AppState};

use axum::{
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

/// Process a batch Bundle (each entry independent).
pub(super) async fn process_batch(
    state: &Arc<AppState>,
    audit_ctx: &AuditContext,
    auth_user: Option<&crate::auth::AuthUser>,
    mut entries: Vec<BundleEntry>,
    base_url: &str,
) -> axum::response::Response {
    let mut response_entries: Vec<Value> = Vec::with_capacity(entries.len());

    for (i, entry) in entries.iter_mut().enumerate() {
        if let Some(refused) = entry.refused.take() {
            response_entries.push(refused);
            continue;
        }
        if entry.method == "GET" && entry.id.is_none() {
            response_entries.push(batch_search(state, audit_ctx, auth_user, entry, base_url).await);
            continue;
        }
        let result = process_batch_entry(state, auth_user, entry, i).await;
        response_entries.push(result);
    }

    audit::log_operation_success(
        audit_ctx, "BATCH", "Bundle",
        &format!("{} entries", response_entries.len()),
        &state.audit,
    );

    let response_bundle = json!({
        "resourceType": "Bundle",
        "type": "batch-response",
        "entry": response_entries
    });

    (StatusCode::OK, Json(response_bundle)).into_response()
}

/// Fire subscription notifications and lifecycle webhooks for a created/updated
/// resource, matching the single-resource CRUD handlers.
fn notify_change(state: &Arc<AppState>, resource_type: &str, id: &str, resource: &Value) {
    state.webhook.maybe_task_completed(resource);
    let state = state.clone();
    let rt = resource_type.to_string();
    let rid = id.to_string();
    let rv = resource.clone();
    tokio::spawn(async move {
        crate::subscription::SubscriptionManager::notify(&state, &rt, &rid, &rv).await;
    });
}

/// Process a single batch entry independently.
/// A search inside a batch (`GET Patient?gender=female`): the searchset
/// Bundle the same search answers on its own, as the entry's resource.
async fn batch_search(
    state: &Arc<AppState>,
    audit_ctx: &AuditContext,
    auth_user: Option<&crate::auth::AuthUser>,
    entry: &BundleEntry,
    base_url: &str,
) -> Value {
    let result = crate::handlers::search::do_search(
        state.clone(),
        entry.resource_type.clone(),
        entry.query.clone().unwrap_or_default(),
        auth_user.cloned(),
        audit_ctx.clone(),
        base_url.to_string(),
    )
    .await;
    let (status, body) = match result {
        Ok(response) => {
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap_or_default();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(json!({})))
        }
        Err((status, axum::Json(body))) => (status, body),
    };
    let line = format!("{} {}", status.as_u16(), status.canonical_reason().unwrap_or(""));
    if status.is_success() {
        json!({"resource": body, "response": {"status": line}})
    } else {
        json!({"response": {"status": line, "outcome": body}})
    }
}

async fn process_batch_entry(
    state: &Arc<AppState>,
    auth_user: Option<&crate::auth::AuthUser>,
    entry: &mut BundleEntry,
    index: usize,
) -> Value {
    match entry.method.as_str() {
        "POST" => {
            // Check ifNoneExist (conditional create)
            if let Some(ref query) = entry.if_none_exist {
                match conditional_create_check(state, auth_user, &entry.resource_type, query).await {
                    ConditionalResult::Exists(existing) => {
                        let existing_id = existing.get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        return json!({
                            "response": {
                                "status": "200 OK",
                                "location": format!("{}/{}", entry.resource_type, existing_id)
                            }
                        });
                    }
                    ConditionalResult::MultipleMatches => {
                        return error_entry(
                            "412 Precondition Failed",
                            &format!("entry[{}]: Multiple matches for ifNoneExist: {}", index, query),
                        );
                    }
                    ConditionalResult::SearchError(e) => {
                        return error_entry(
                            "400 Bad Request",
                            &format!("entry[{}]: ifNoneExist search failed: {}", index, e),
                        );
                    }
                    ConditionalResult::NoMatch => { /* proceed to create */ }
                }
            }

            let (method, url_type, url_id) = (entry.method.clone(), entry.resource_type.clone(), entry.id.clone());
            let resource = match &mut entry.resource {
                Some(r) => r,
                None => {
                    return error_entry(
                        "400 Bad Request",
                        &format!("entry[{}].resource is required for POST", index),
                    );
                }
            };

            if let Err(e) = super::check_entry_body(state, &method, &url_type, url_id.as_deref(), resource) {
                return error_entry("400 Bad Request", &format!("entry[{index}]: {e}"));
            }

            // Validated (with its conditional references resolved) by
            // `write::prepare` before the entries were authorized.

            let id = uuid::Uuid::new_v4().to_string();
            let version_id = "1".to_string();

            if let Some(obj) = resource.as_object_mut() {
                obj.insert("id".to_string(), json!(id));
                crate::handlers::merge_version_meta(obj, &version_id);
            }

            let data = serde_json::to_vec(&resource).unwrap();
            match state
                .store
                .put_with_version(&entry.resource_type, &id, &version_id, &data, "POST")
            {
                Ok(()) => {
                    // Index
                    {
                        let idx = state.index.lock().await;
                        crate::handlers::update_search_index(
                            &idx,
                            &state.search_param_registry,
                            &entry.resource_type,
                            &id,
                            resource,
                        );
                    }

                    notify_change(state, &entry.resource_type, &id, resource);
                    json!({
                        "response": {
                            "status": "201 Created",
                            "location": format!("{}/{}/_history/1", entry.resource_type, id)
                        }
                    })
                }
                Err(e) => error_entry("500 Internal Server Error", &e.to_string()),
            }
        }
        "PUT" => {
            let id = match &entry.id {
                Some(id) => id.clone(),
                None => {
                    return error_entry(
                        "400 Bad Request",
                        &format!(
                            "entry[{}].request.url must include id for PUT",
                            index
                        ),
                    );
                }
            };

            let (method, url_type, url_id) = (entry.method.clone(), entry.resource_type.clone(), entry.id.clone());
            let resource = match &mut entry.resource {
                Some(r) => r,
                None => {
                    return error_entry(
                        "400 Bad Request",
                        &format!("entry[{}].resource is required for PUT", index),
                    );
                }
            };

            if let Err(e) = super::check_entry_body(state, &method, &url_type, url_id.as_deref(), resource) {
                return error_entry("400 Bad Request", &format!("entry[{index}]: {e}"));
            }

            // Validated (with its conditional references resolved) by
            // `write::prepare` before the entries were authorized.

            // Determine version
            let (is_create, version_id) = match state.store.get(&entry.resource_type, &id) {
                Ok(Some(existing)) => {
                    let existing: Value = serde_json::from_slice(&existing).unwrap_or(json!({}));
                    let current: i64 = existing
                        .get("meta")
                        .and_then(|m| m.get("versionId"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    (false, (current + 1).to_string())
                }
                Ok(None) => match state.store.next_version(&entry.resource_type, &id) {
                    Ok(v) => (true, v),
                    Err(e) => return error_entry("500 Internal Server Error", &e.to_string()),
                },
                Err(e) => {
                    return error_entry("500 Internal Server Error", &e.to_string());
                }
            };

            if let Some(obj) = resource.as_object_mut() {
                obj.insert("id".to_string(), json!(id));
                crate::handlers::merge_version_meta(obj, &version_id);
            }

            let data = serde_json::to_vec(&resource).unwrap();
            // `ifMatch`: replace only that version (compare-and-swap).
            let written = match &entry.if_match {
                Some(want) => match state.store.put_with_version_cas(&entry.resource_type, &id, Some(want), &version_id, &data, "PUT") {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        return error_entry(
                            "412 Precondition Failed",
                            &format!("entry[{index}]: {}/{id} is not at version {want}", entry.resource_type),
                        );
                    }
                    Err(e) => Err(e),
                },
                None => state.store.put_with_version(&entry.resource_type, &id, &version_id, &data, "PUT"),
            };
            match written {
                Ok(()) => {
                    // Re-index
                    {
                        let idx = state.index.lock().await;
                        crate::handlers::update_search_index(
                            &idx,
                            &state.search_param_registry,
                            &entry.resource_type,
                            &id,
                            resource,
                        );
                    }

                    notify_change(state, &entry.resource_type, &id, resource);
                    let status = if is_create {
                        "201 Created"
                    } else {
                        "200 OK"
                    };
                    json!({
                        "response": {
                            "status": status,
                            "location": format!("{}/{}/_history/{}", entry.resource_type, id, version_id)
                        }
                    })
                }
                Err(e) => error_entry("500 Internal Server Error", &e.to_string()),
            }
        }
        "DELETE" => {
            let id = match &entry.id {
                Some(id) => id.clone(),
                None => {
                    return error_entry(
                        "400 Bad Request",
                        &format!(
                            "entry[{}].request.url must include id for DELETE",
                            index
                        ),
                    );
                }
            };

            if let Some(want) = &entry.if_match {
                let current = state.store.get(&entry.resource_type, &id).ok().flatten();
                if current.as_deref().and_then(super::stored_version).as_deref() != Some(want.as_str()) {
                    return error_entry(
                        "412 Precondition Failed",
                        &format!("entry[{index}]: {}/{id} is not at version {want}", entry.resource_type),
                    );
                }
            }
            match state.store.delete(&entry.resource_type, &id) {
                Ok(_) => {
                    // Remove indices
                    let idx = state.index.lock().await;
                    let _ = idx.remove_index(&entry.resource_type, &id);

                    json!({
                        "response": { "status": "204 No Content" }
                    })
                }
                Err(e) => error_entry("500 Internal Server Error", &e.to_string()),
            }
        }
        "GET" => {
            // Read by id (Type/id). FHIR also allows search GETs in a batch;
            // those are not supported yet and fall through to the id check.
            let id = match &entry.id {
                Some(id) => id.clone(),
                None => {
                    return error_entry(
                        "400 Bad Request",
                        &format!("entry[{}].request.url must include id for GET", index),
                    );
                }
            };

            match state.store.get(&entry.resource_type, &id) {
                Ok(Some(data)) => {
                    let resource: Value = serde_json::from_slice(&data).unwrap_or(json!({}));
                    // Another patient's resource: as if there were none.
                    if crate::compartment_check::check_compartment_access(auth_user, &state.compartment_def, &entry.resource_type, &resource).is_err() {
                        return json!({"response": { "status": "404 Not Found" }});
                    }
                    json!({
                        "resource": resource,
                        "response": { "status": "200 OK" }
                    })
                }
                Ok(None) => json!({
                    "response": { "status": "404 Not Found" }
                }),
                Err(e) => error_entry("500 Internal Server Error", &e.to_string()),
            }
        }
        other => error_entry(
            "400 Bad Request",
            &format!(
                "entry[{}].request.method '{}' is not supported",
                index, other
            ),
        ),
    }
}
