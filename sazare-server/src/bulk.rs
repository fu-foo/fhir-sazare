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
    // Index and store writes happen under the write gate (shared), so an index
    // switch never finds a write between its two steps. Held per batch (from
    // its first store write to its index flush), not for the whole import: a
    // switch — and the writes queued behind it — wait for one batch at most.
    let mut write_gate = state.write_gate.read().await;
    // $import writes the whole dataset — require system-level write scope.
    if let Err(resp) = crate::bulk_export::authorize_bulk(&auth_user, "write") {
        return resp;
    }
    let user_id = auth_user.map(|u| u.user_id.clone());
    let audit_ctx = AuditContext::new(user_id, addr.ip().to_string());

    let mut created: usize = 0;
    let mut errors: Vec<Value> = Vec::new();
    // Writes are batched: the store and the index each take one transaction
    // per BATCH resources instead of one per resource (a store commit waits
    // for the disk: one per resource held $import to ~170/s on an SSD).
    const BATCH: usize = 1000;
    let mut staged: Vec<Staged> = Vec::new();
    // Versions given in the batch not yet written: the same id twice in a
    // file gets 1, then 2.
    let mut staged_versions: std::collections::HashMap<(String, String), i64> = std::collections::HashMap::new();

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

        // One resource per line: a JSON array (a whole list on one line) or
        // any other non-object isn't one.
        if !resource.is_object() {
            let what = if resource.is_array() { "a JSON array" } else { "not a JSON object" };
            errors.push(json!({
                "line": line_num + 1,
                "error": format!("{what}; NDJSON has one resource (a JSON object) per line")
            }));
            continue;
        }

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

        if resource_type == "Subscription"
            && let Err(e) = crate::subscription::validate_subscription(&resource, &state.search_param_registry)
        {
            errors.push(json!({"line": line_num + 1, "resourceType": resource_type, "error": e}));
            continue;
        }

        // Use existing id or assign new one
        let id = resource
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Determine version: one past a version given earlier in this batch,
        // or past the stored one.
        let staged_key = (resource_type.clone(), id.clone());
        let version_id = match staged_versions.get(&staged_key) {
            Some(v) => (v + 1).to_string(),
            None => match state.store.get(&resource_type, &id) {
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
            _ => match state.store.next_version(&resource_type, &id) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(json!({"line": line_num + 1, "resourceType": resource_type, "id": id,
                                       "error": format!("Storage error: {}", e)}));
                    continue;
                }
            },
            },
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
        staged_versions.insert(staged_key, version_id.parse().unwrap_or(1));
        staged.push(Staged { line: line_num + 1, resource_type, id, version_id, data, resource });
        if staged.len() >= BATCH {
            flush(&state, &mut staged, &mut created, &mut errors).await;
            staged_versions.clear();
            drop(write_gate);
            write_gate = state.write_gate.read().await;
        }
    }
    flush(&state, &mut staged, &mut created, &mut errors).await;
    drop(write_gate);
    // Storage errors are found when their batch is written: back in line order.
    errors.sort_by_key(|e| e.get("line").and_then(|v| v.as_u64()).unwrap_or(0));

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
        "diagnostics": format!(
            "{created} resource{} imported, {} error{}",
            if created == 1 { "" } else { "s" },
            errors.len(),
            if errors.len() == 1 { "" } else { "s" }
        )
    })];
    // The first errors in full; a file with millions of bad lines would make a
    // response as large as the file. The count above is exact.
    const LISTED: usize = 100;
    for err in errors.iter().take(LISTED) {
        // `line 3 (Patient p1): …` — a sentence, not JSON in a string.
        let what = match (err.get("resourceType").and_then(|v| v.as_str()), err.get("id").and_then(|v| v.as_str())) {
            (Some(t), Some(id)) => format!(" ({t} {id})"),
            (Some(t), None) => format!(" ({t})"),
            _ => String::new(),
        };
        let diag = format!(
            "line {}{what}: {}",
            err.get("line").and_then(|v| v.as_u64()).unwrap_or(0),
            err.get("error").and_then(|v| v.as_str()).unwrap_or("import error")
        );
        issues.push(json!({
            "severity": "error",
            "code": "processing",
            "diagnostics": diag,
        }));
    }
    if errors.len() > LISTED {
        issues.push(json!({
            "severity": "error",
            "code": "too-costly",
            "diagnostics": format!("{} more errors not listed (the first {LISTED} are)", errors.len() - LISTED),
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

/// A resource ready to be written by [`flush`].
struct Staged {
    line: usize,
    resource_type: String,
    id: String,
    version_id: String,
    data: Vec<u8>,
    resource: Value,
}

/// Write `staged` to the store in one transaction, then index what was
/// written (one transaction) and notify subscriptions once it is searchable.
async fn flush(state: &Arc<AppState>, staged: &mut Vec<Staged>, created: &mut usize, errors: &mut Vec<Value>) {
    if staged.is_empty() {
        return;
    }
    let rows: Vec<(&str, &str, &str, &[u8])> = staged
        .iter()
        .map(|s| (s.resource_type.as_str(), s.id.as_str(), s.version_id.as_str(), s.data.as_slice()))
        .collect();
    let results = match state.store.put_versions(&rows) {
        Ok(r) => r.into_iter().map(|r| r.map_err(|e| e.to_string())).collect::<Vec<_>>(),
        Err(e) => vec![Err(e.to_string()); rows.len()],
    };
    drop(rows);
    let mut pending: Vec<(sazare_store::IndexUpdate, Value)> = Vec::new();
    for (s, result) in staged.drain(..).zip(results) {
        match result {
            Ok(()) => {
                state.webhook.maybe_task_completed(&s.resource);
                let update = crate::handlers::index_update(&state.search_param_registry, &s.resource_type, &s.id, &s.resource);
                pending.push((update, s.resource));
                *created += 1;
            }
            Err(e) => errors.push(json!({
                "line": s.line,
                "resourceType": s.resource_type,
                "id": s.id,
                "error": format!("Storage error: {e}")
            })),
        }
    }
    {
        let updates: Vec<sazare_store::IndexUpdate> = pending.iter().map(|(u, _)| u.clone()).collect();
        let index = state.index.lock().await;
        if let Err(e) = index.replace_many(&updates) {
            tracing::warn!("search index update failed for an $import batch: {e}");
        }
    }
    // Subscription criteria are evaluated by searching, so notifications go
    // out only after their batch is indexed.
    if crate::subscription::SubscriptionManager::any(state) {
        for (u, resource) in pending {
            let state = state.clone();
            tokio::spawn(async move {
                crate::subscription::SubscriptionManager::notify(&state, &u.resource_type, &u.resource_id, &resource).await;
            });
        }
    }
}

