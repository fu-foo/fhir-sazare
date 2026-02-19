//! Transaction Bundle processing (all-or-nothing)

use super::{resolve_references, BundleEntry};
use crate::audit::{self, AuditContext};
use crate::{conditional_create_check, ConditionalResult, AppState};

use axum::{
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use sazare_core::{
    operation_outcome::IssueType,
    validation::validate_resource_all_phases,
    OperationOutcome,
};
use sazare_store::IndexBuilder;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Process a transaction Bundle (all-or-nothing).
pub(super) async fn process_transaction(
    state: &Arc<AppState>,
    audit_ctx: &AuditContext,
    mut entries: Vec<BundleEntry>,
) -> axum::response::Response {
    // Phase 1: Validate all resources that will be created/updated
    for (i, entry) in entries.iter().enumerate() {
        match entry.method.as_str() {
            "POST" | "PUT" => {
                let resource = match &entry.resource {
                    Some(r) => r,
                    None => {
                        let outcome = OperationOutcome::error(
                            IssueType::Required,
                            format!("entry[{}].resource is required for {}", i, entry.method),
                        );
                        audit::log_operation_error(
                            audit_ctx, "TRANSACTION", "Bundle", None,
                            "Missing resource in entry", &state.audit,
                        );
                        return (StatusCode::BAD_REQUEST, Json(json!(outcome))).into_response();
                    }
                };
                if let Err(outcome) = validate_resource_all_phases(
                    resource,
                    &state.profile_registry,
                    &state.terminology_registry,
                ) {
                    audit::log_operation_error(
                        audit_ctx, "TRANSACTION", "Bundle", None,
                        "Validation failed", &state.audit,
                    );
                    return (StatusCode::BAD_REQUEST, Json(json!(outcome))).into_response();
                }
            }
            "DELETE" => {}
            _ => {
                let outcome = OperationOutcome::error(
                    IssueType::NotSupported,
                    format!(
                        "entry[{}].request.method '{}' is not supported (use POST, PUT, or DELETE)",
                        i, entry.method
                    ),
                );
                return (StatusCode::BAD_REQUEST, Json(json!(outcome))).into_response();
            }
        }
    }

    // Phase 2: Assign IDs for POST entries and build reference map
    let mut ref_map: HashMap<String, String> = HashMap::new();
    let mut assigned: Vec<(String, String)> = Vec::with_capacity(entries.len());
    let mut conditional_existing: Vec<Option<Value>> = vec![None; entries.len()];

    for (i, entry) in entries.iter_mut().enumerate() {
        let id = match entry.method.as_str() {
            "POST" => {
                // Check ifNoneExist before assigning a new ID
                if let Some(ref query) = entry.if_none_exist {
                    match conditional_create_check(state, &entry.resource_type, query).await {
                        ConditionalResult::Exists(existing) => {
                            let existing_id = existing.get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if let Some(ref full_url) = entry.full_url {
                                ref_map.insert(
                                    full_url.clone(),
                                    format!("{}/{}", entry.resource_type, existing_id),
                                );
                            }
                            conditional_existing[i] = Some(existing);
                            assigned.push((entry.resource_type.clone(), existing_id));
                            continue;
                        }
                        ConditionalResult::MultipleMatches => {
                            let outcome = OperationOutcome::error(
                                IssueType::MultipleMatches,
                                format!(
                                    "entry[{}]: Multiple matches for ifNoneExist: {}",
                                    i, query
                                ),
                            );
                            audit::log_operation_error(
                                audit_ctx, "TRANSACTION", "Bundle", None,
                                "Multiple matches for ifNoneExist", &state.audit,
                            );
                            return (StatusCode::PRECONDITION_FAILED, Json(json!(outcome))).into_response();
                        }
                        ConditionalResult::SearchError(e) => {
                            let outcome = OperationOutcome::error(
                                IssueType::Processing,
                                format!("entry[{}]: ifNoneExist search failed: {}", i, e),
                            );
                            return (StatusCode::BAD_REQUEST, Json(json!(outcome))).into_response();
                        }
                        ConditionalResult::NoMatch => { /* proceed to create */ }
                    }
                }

                let new_id = uuid::Uuid::new_v4().to_string();
                if let Some(ref full_url) = entry.full_url {
                    ref_map.insert(
                        full_url.clone(),
                        format!("{}/{}", entry.resource_type, new_id),
                    );
                }
                new_id
            }
            "PUT" | "DELETE" => match &entry.id {
                Some(id) => id.clone(),
                None => {
                    let outcome = OperationOutcome::error(
                        IssueType::Required,
                        format!(
                            "request.url must include resource id for {} (e.g. 'Patient/123')",
                            entry.method
                        ),
                    );
                    return (StatusCode::BAD_REQUEST, Json(json!(outcome))).into_response();
                }
            },
            _ => unreachable!(),
        };
        assigned.push((entry.resource_type.clone(), id));
    }

    // Phase 3: Resolve urn:uuid references in all resources
    for entry in entries.iter_mut() {
        if let Some(ref mut resource) = entry.resource {
            resolve_references(resource, &ref_map);
        }
    }

    // Phase 4: Execute all operations in a single SQLite transaction
    let mut resources_for_index: Vec<(String, String, Value)> = Vec::new();
    let mut response_entries: Vec<Value> = Vec::with_capacity(entries.len());

    let tx_result = state.store.in_transaction(|ops| {
        for (i, entry) in entries.iter_mut().enumerate() {
            // Skip conditional-existing entries (ifNoneExist matched)
            if conditional_existing[i].is_some() {
                let (ref resource_type, ref id) = assigned[i];
                response_entries.push(json!({
                    "response": {
                        "status": "200 OK",
                        "location": format!("{}/{}", resource_type, id)
                    }
                }));
                continue;
            }

            let (ref resource_type, ref id) = assigned[i];
            match entry.method.as_str() {
                "POST" => {
                    let resource = entry.resource.as_mut().unwrap();
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
                    ops.put_with_version(resource_type, id, &version_id, &data)?;

                    resources_for_index.push((
                        resource_type.clone(),
                        id.clone(),
                        resource.clone(),
                    ));
                    response_entries.push(json!({
                        "response": {
                            "status": "201 Created",
                            "location": format!("{}/{}/_history/1", resource_type, id)
                        }
                    }));
                }
                "PUT" => {
                    let resource = entry.resource.as_mut().unwrap();

                    // Determine version from existing resource
                    let version_id = match ops.get(resource_type, id)? {
                        Some(existing) => {
                            let existing: Value =
                                serde_json::from_slice(&existing).unwrap_or(json!({}));
                            let current: i64 = existing
                                .get("meta")
                                .and_then(|m| m.get("versionId"))
                                .and_then(|v| v.as_str())
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(0);
                            (current + 1).to_string()
                        }
                        None => "1".to_string(),
                    };

                    let is_create = version_id == "1";

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
                    ops.put_with_version(resource_type, id, &version_id, &data)?;

                    resources_for_index.push((
                        resource_type.clone(),
                        id.clone(),
                        resource.clone(),
                    ));

                    let status = if is_create {
                        "201 Created"
                    } else {
                        "200 OK"
                    };
                    response_entries.push(json!({
                        "response": {
                            "status": status,
                            "location": format!("{}/{}/_history/{}", resource_type, id, version_id)
                        }
                    }));
                }
                "DELETE" => {
                    let _existed = ops.delete(resource_type, id)?;
                    response_entries.push(json!({
                        "response": { "status": "204 No Content" }
                    }));
                }
                _ => unreachable!(),
            }
        }
        Ok(())
    });

    if let Err(e) = tx_result {
        let outcome = OperationOutcome::storage_error(format!("Transaction failed: {}", e));
        audit::log_operation_error(
            audit_ctx, "TRANSACTION", "Bundle", None,
            &e.to_string(), &state.audit,
        );
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!(outcome))).into_response();
    }

    // Phase 5: Update indices (outside SQLite transaction — separate DB)
    {
        let index = state.index.lock().await;
        for (resource_type, id, resource) in &resources_for_index {
            let _ = index.remove_index(resource_type, id);
            let indices = IndexBuilder::extract_indices_with_registry(&state.search_param_registry, resource_type, resource);
            for (param_name, param_type, value, system) in indices {
                let _ = index.add_index(resource_type, id, &param_name, &param_type, Some(&value), system.as_deref());
            }
        }
    }

    audit::log_operation_success(
        audit_ctx, "TRANSACTION", "Bundle",
        &format!("{} entries", response_entries.len()),
        &state.audit,
    );

    let response_bundle = json!({
        "resourceType": "Bundle",
        "type": "transaction-response",
        "entry": response_entries
    });

    (StatusCode::OK, Json(response_bundle)).into_response()
}
