//! Bulk data import
//!
//! POST /$import — import resources from NDJSON body
//!
//! (Bulk Data IG `$export` lives in [`crate::bulk_export`].)

use crate::audit::{self, AuditContext};
use crate::auth::AuthUser;
use crate::handlers::merge_version_meta;
use crate::AppState;

use axum::{
    extract::{ConnectInfo, State},
    http::StatusCode,
    response::IntoResponse,
};
use sazare_core::validation::validate_resource_all_phases;
use sazare_store::IndexBuilder;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

/// POST /$import — import resources from NDJSON body
pub async fn import(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<AppState>>,
    auth_user: Option<axum::extract::Extension<AuthUser>>,
    body: String,
) -> axum::response::Response {
    // $import writes the whole dataset — require system-level write scope.
    if let Err(resp) = crate::bulk_export::authorize_bulk(&auth_user, "write") {
        return resp;
    }
    let user_id = auth_user.map(|u| u.user_id.clone());
    let audit_ctx = AuditContext::new(user_id, addr.ip().to_string());

    let mut created: usize = 0;
    let mut errors: Vec<Value> = Vec::new();

    for (line_num, line) in body.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse JSON
        let mut resource: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                errors.push(json!({
                    "line": line_num + 1,
                    "error": format!("Invalid JSON: {}", e)
                }));
                continue;
            }
        };

        // Extract resourceType
        let resource_type = match resource.get("resourceType").and_then(|v| v.as_str()) {
            Some(rt) => rt.to_string(),
            None => {
                errors.push(json!({
                    "line": line_num + 1,
                    "error": "Missing resourceType"
                }));
                continue;
            }
        };

        // Validate
        if let Err(outcome) = validate_resource_all_phases(
            &resource,
            &state.profile_registry,
            &state.terminology_registry,
        ) {
            let diag = outcome
                .issue
                .first()
                .and_then(|i| i.diagnostics.as_deref())
                .unwrap_or("Validation failed")
                .to_string();
            errors.push(json!({
                "line": line_num + 1,
                "resourceType": resource_type,
                "error": diag
            }));
            continue;
        }

        // Use existing id or assign new one
        let id = resource
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Determine version: check if resource already exists
        let version_id = match state.store.get(&resource_type, &id) {
            Ok(Some(existing)) => {
                let existing: Value = serde_json::from_slice(&existing).unwrap_or(json!({}));
                let current: i64 = existing
                    .get("meta")
                    .and_then(|m| m.get("versionId"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                (current + 1).to_string()
            }
            _ => "1".to_string(),
        };

        // Set id and meta (preserve caller-provided meta fields)
        if let Some(obj) = resource.as_object_mut() {
            obj.insert("id".to_string(), json!(id));
            merge_version_meta(obj, &version_id);
        }

        let data = match serde_json::to_vec(&resource) {
            Ok(d) => d,
            Err(e) => {
                errors.push(json!({
                    "line": line_num + 1,
                    "resourceType": resource_type,
                    "id": id,
                    "error": format!("Serialization error: {}", e)
                }));
                continue;
            }
        };
        match state
            .store
            .put_with_version(&resource_type, &id, &version_id, &data)
        {
            Ok(()) => {
                // Index
                let indices = IndexBuilder::extract_indices_with_registry(&state.search_param_registry, &resource_type, &resource);
                let index = state.index.lock().await;
                let _ = index.remove_index(&resource_type, &id);
                for (param_name, param_type, value, system) in indices {
                    let _ = index.add_index(
                        &resource_type,
                        &id,
                        &param_name,
                        &param_type,
                        Some(&value),
                        system.as_deref(),
                    );
                }
                drop(index);
                // Fire subscriptions/webhooks for imported resources too.
                state.webhook.maybe_task_completed(&resource);
                {
                    let state = state.clone();
                    let rt = resource_type.clone();
                    let rid = id.clone();
                    let rv = resource.clone();
                    tokio::spawn(async move {
                        crate::subscription::SubscriptionManager::notify(&state, &rt, &rid, &rv).await;
                    });
                }
                created += 1;
            }
            Err(e) => {
                errors.push(json!({
                    "line": line_num + 1,
                    "resourceType": resource_type,
                    "id": id,
                    "error": format!("Storage error: {}", e)
                }));
            }
        }
    }

    audit::log_operation_success(
        &audit_ctx,
        "IMPORT",
        "Bundle",
        &format!("{} created, {} errors", created, errors.len()),
        &state.audit,
    );

    // Build a spec-valid OperationOutcome: a summary issue followed by one
    // issue per failed line (the previous root-level `details` element is not a
    // valid OperationOutcome field).
    let mut issues: Vec<Value> = vec![json!({
        "severity": if errors.is_empty() { "information" } else { "warning" },
        "code": "informational",
        "diagnostics": format!("{} resources imported, {} errors", created, errors.len())
    })];
    for err in &errors {
        let diag = serde_json::to_string(err).unwrap_or_else(|_| "import error".to_string());
        issues.push(json!({
            "severity": "error",
            "code": "processing",
            "diagnostics": diag,
        }));
    }
    let response = json!({
        "resourceType": "OperationOutcome",
        "issue": issues,
        "extension": [{
            "url": "http://sazare.dev/StructureDefinition/import-result",
            "extension": [
                {"url": "created", "valueInteger": created},
                {"url": "errors", "valueInteger": errors.len()}
            ]
        }]
    });

    let status = if !errors.is_empty() && created == 0 {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };

    (status, axum::Json(response)).into_response()
}
