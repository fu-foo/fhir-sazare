//! Bulk data import/export
//!
//! GET  /$export — export resources as NDJSON
//! POST /$import — import resources from NDJSON body

use crate::audit::{self, AuditContext};
use crate::auth::AuthUser;
use crate::AppState;

use axum::{
    extract::{ConnectInfo, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
};
use sazare_core::validation::validate_resource_all_phases;
use sazare_store::IndexBuilder;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

/// Query parameters for $export
#[derive(Deserialize, Default)]
pub struct ExportParams {
    /// Comma-separated resource types to export (e.g. "Patient,Observation")
    _type: Option<String>,
}

/// GET /$export — export all resources as NDJSON
pub async fn export(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<AppState>>,
    auth_user: Option<axum::extract::Extension<AuthUser>>,
    Query(params): Query<ExportParams>,
) -> impl IntoResponse {
    let user_id = auth_user.map(|u| u.user_id.clone());
    let audit_ctx = AuditContext::new(user_id, addr.ip().to_string());

    // Parse _type filter
    let type_filter: Option<Vec<String>> = params._type.as_ref().map(|t| {
        t.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });

    let mut ndjson = String::new();
    let mut count: usize = 0;

    if let Some(ref types) = type_filter {
        // Export specific resource types
        for rt in types {
            match state.store.list_all(Some(rt)) {
                Ok(resources) => {
                    for (_rt, _id, data) in resources {
                        if let Ok(line) = std::str::from_utf8(&data) {
                            ndjson.push_str(line);
                            ndjson.push('\n');
                            count += 1;
                        }
                    }
                }
                Err(e) => {
                    let outcome = json!({
                        "resourceType": "OperationOutcome",
                        "issue": [{"severity": "error", "code": "exception",
                            "diagnostics": format!("Export failed: {}", e)}]
                    });
                    return (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(outcome)).into_response();
                }
            }
        }
    } else {
        // Export all resources
        match state.store.list_all(None) {
            Ok(resources) => {
                for (_rt, _id, data) in resources {
                    if let Ok(line) = std::str::from_utf8(&data) {
                        ndjson.push_str(line);
                        ndjson.push('\n');
                        count += 1;
                    }
                }
            }
            Err(e) => {
                let outcome = json!({
                    "resourceType": "OperationOutcome",
                    "issue": [{"severity": "error", "code": "exception",
                        "diagnostics": format!("Export failed: {}", e)}]
                });
                return (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(outcome)).into_response();
            }
        }
    }

    audit::log_operation_success(
        &audit_ctx,
        "EXPORT",
        "Bundle",
        &format!("{} resources", count),
        &state.audit,
    );

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/ndjson")],
        ndjson,
    )
        .into_response()
}

/// POST /$import — import resources from NDJSON body
pub async fn import(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<AppState>>,
    auth_user: Option<axum::extract::Extension<AuthUser>>,
    body: String,
) -> impl IntoResponse {
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

        // Set id and meta
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

    let response = json!({
        "resourceType": "OperationOutcome",
        "issue": [{
            "severity": if errors.is_empty() { "information" } else { "warning" },
            "code": "informational",
            "diagnostics": format!("{} resources imported, {} errors", created, errors.len())
        }],
        "extension": [{
            "url": "http://sazare.dev/StructureDefinition/import-result",
            "extension": [
                {"url": "created", "valueInteger": created},
                {"url": "errors", "valueInteger": errors.len()}
            ]
        }],
        "details": if errors.is_empty() { Value::Null } else { json!(errors) }
    });

    let status = if !errors.is_empty() && created == 0 {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };

    (status, axum::Json(response)).into_response()
}
