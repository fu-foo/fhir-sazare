//! Batch Bundle processing (each entry independent)

use super::{error_entry, BundleEntry};
use crate::audit::{self, AuditContext};
use crate::{conditional_create_check, ConditionalResult, AppState};

use axum::{
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use sazare_core::validation::validate_resource_all_phases;
use sazare_store::IndexBuilder;
use serde_json::{json, Value};
use std::sync::Arc;

/// Process a batch Bundle (each entry independent).
pub(super) async fn process_batch(
    state: &Arc<AppState>,
    audit_ctx: &AuditContext,
    mut entries: Vec<BundleEntry>,
) -> axum::response::Response {
    let mut response_entries: Vec<Value> = Vec::with_capacity(entries.len());

    for (i, entry) in entries.iter_mut().enumerate() {
        let result = process_batch_entry(state, entry, i).await;
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

/// Process a single batch entry independently.
async fn process_batch_entry(
    state: &Arc<AppState>,
    entry: &mut BundleEntry,
    index: usize,
) -> Value {
    match entry.method.as_str() {
        "POST" => {
            // Check ifNoneExist (conditional create)
            if let Some(ref query) = entry.if_none_exist {
                match conditional_create_check(state, &entry.resource_type, query).await {
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

            let resource = match &mut entry.resource {
                Some(r) => r,
                None => {
                    return error_entry(
                        "400 Bad Request",
                        &format!("entry[{}].resource is required for POST", index),
                    );
                }
            };

            if let Err(outcome) = validate_resource_all_phases(
                resource,
                &state.profile_registry,
                &state.terminology_registry,
            ) {
                return json!({
                    "response": {
                        "status": "400 Bad Request",
                        "outcome": outcome
                    }
                });
            }

            let id = uuid::Uuid::new_v4().to_string();
            let version_id = "1".to_string();

            if let Some(obj) = resource.as_object_mut() {
                obj.insert("id".to_string(), json!(id));
                obj.insert(
                    "meta".to_string(),
                    json!({
                        "versionId": version_id,
                        "lastUpdated": chrono::Utc::now().to_rfc3339()
                    }),
                );
            }

            let data = serde_json::to_vec(&resource).unwrap();
            match state
                .store
                .put_with_version(&entry.resource_type, &id, &version_id, &data)
            {
                Ok(()) => {
                    // Index
                    let indices = IndexBuilder::extract_indices_with_registry(&state.search_param_registry, &entry.resource_type, resource);
                    let idx = state.index.lock().await;
                    for (param_name, param_type, value, system) in indices {
                        let _ = idx.add_index(
                            &entry.resource_type,
                            &id,
                            &param_name,
                            &param_type,
                            Some(&value),
                            system.as_deref(),
                        );
                    }

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

            let resource = match &mut entry.resource {
                Some(r) => r,
                None => {
                    return error_entry(
                        "400 Bad Request",
                        &format!("entry[{}].resource is required for PUT", index),
                    );
                }
            };

            if let Err(outcome) = validate_resource_all_phases(
                resource,
                &state.profile_registry,
                &state.terminology_registry,
            ) {
                return json!({
                    "response": {
                        "status": "400 Bad Request",
                        "outcome": outcome
                    }
                });
            }

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
                Ok(None) => (true, "1".to_string()),
                Err(e) => {
                    return error_entry("500 Internal Server Error", &e.to_string());
                }
            };

            if let Some(obj) = resource.as_object_mut() {
                obj.insert("id".to_string(), json!(id));
                obj.insert(
                    "meta".to_string(),
                    json!({
                        "versionId": version_id,
                        "lastUpdated": chrono::Utc::now().to_rfc3339()
                    }),
                );
            }

            let data = serde_json::to_vec(&resource).unwrap();
            match state
                .store
                .put_with_version(&entry.resource_type, &id, &version_id, &data)
            {
                Ok(()) => {
                    // Re-index
                    let indices = IndexBuilder::extract_indices_with_registry(&state.search_param_registry, &entry.resource_type, resource);
                    let idx = state.index.lock().await;
                    let _ = idx.remove_index(&entry.resource_type, &id);
                    for (param_name, param_type, value, system) in indices {
                        let _ = idx.add_index(
                            &entry.resource_type,
                            &id,
                            &param_name,
                            &param_type,
                            Some(&value),
                            system.as_deref(),
                        );
                    }

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
        other => error_entry(
            "400 Bad Request",
            &format!(
                "entry[{}].request.method '{}' is not supported",
                index, other
            ),
        ),
    }
}
