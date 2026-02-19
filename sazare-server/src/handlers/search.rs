use axum::{
    extract::{Path, Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use sazare_core::{
    operation_outcome::IssueType,
    resource_filter::{apply_elements, apply_summary},
    OperationOutcome, SearchQuery,
};
use sazare_store::SearchExecutor;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::audit::{self, AuditContext};
use crate::auth::AuthUser;
use crate::compartment_check::filter_by_compartment;
use crate::AppState;

/// Default page size per FHIR spec
const DEFAULT_COUNT: usize = 100;

/// Search query parameters
#[derive(Deserialize, Default)]
pub struct SearchParams {
    #[serde(flatten)]
    pub params: std::collections::HashMap<String, String>,
}

/// Search (GET /{resource_type}?...)
pub async fn search(
    State(state): State<Arc<AppState>>,
    Path(resource_type): Path<String>,
    Query(params): Query<SearchParams>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();

    // Reconstruct query string
    let query_string: String = params
        .params
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&");

    let query = SearchQuery::parse(&query_string).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e))),
        )
    })?;

    // If _summary=count, return only the count
    if query.summary == Some(sazare_core::SummaryMode::Count) {
        let index = state.index.lock().await;
        let executor = SearchExecutor::new(&state.store, &index);
        let ids = executor.search(&resource_type, &query).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!(OperationOutcome::storage_error(e))),
            )
        })?;

        // For count mode with compartment filtering, we need to load and filter
        if auth_user.as_ref().is_some_and(|u| u.is_patient_scoped()) {
            let resources = executor.load_resources(&resource_type, &ids).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!(OperationOutcome::storage_error(e))),
                )
            })?;
            let filtered = filter_by_compartment(auth_user.as_ref(), &state.compartment_def, &resource_type, resources);
            return Ok(Json(json!({
                "resourceType": "Bundle",
                "type": "searchset",
                "total": filtered.len()
            })).into_response());
        }

        return Ok(Json(json!({
            "resourceType": "Bundle",
            "type": "searchset",
            "total": ids.len()
        })).into_response());
    }

    let index = state.index.lock().await;
    let executor = SearchExecutor::new(&state.store, &index);

    let (ids, total) = executor.search_with_total(&resource_type, &query).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!(OperationOutcome::storage_error(e))),
        )
    })?;

    let resources = executor.load_resources(&resource_type, &ids).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!(OperationOutcome::storage_error(e))),
        )
    })?;

    // Compartment filtering
    let mut resources = filter_by_compartment(auth_user.as_ref(), &state.compartment_def, &resource_type, resources);

    let total = if auth_user.as_ref().is_some_and(|u| u.is_patient_scoped()) {
        // If compartment-filtered, total is the filtered count
        resources.len()
    } else {
        total
    };

    // Process _include
    let included = if !query.include.is_empty() {
        executor
            .process_includes(&resources, &query.include)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // Process _revinclude
    let revincluded = if !query.revinclude.is_empty() {
        executor
            .process_revincludes(&resources, &resource_type, &query.revinclude)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // Apply _summary / _elements filtering
    for resource in &mut resources {
        if let Some(ref mode) = query.summary {
            apply_summary(resource, mode);
        }
        if !query.elements.is_empty() {
            apply_elements(resource, &query.elements);
        }
    }

    // Build Bundle
    let mut entries: Vec<Value> = resources
        .into_iter()
        .map(|r| {
            let full_url = format!(
                "{}/{}",
                resource_type,
                r.get("id").and_then(|v| v.as_str()).unwrap_or("")
            );
            json!({
                "fullUrl": full_url,
                "resource": r,
                "search": {"mode": "match"}
            })
        })
        .collect();

    // Include entries
    for inc in included {
        let inc_type = inc
            .get("resourceType")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let inc_id = inc.get("id").and_then(|v| v.as_str()).unwrap_or("");
        entries.push(json!({
            "fullUrl": format!("{}/{}", inc_type, inc_id),
            "resource": inc,
            "search": {"mode": "include"}
        }));
    }

    // Revinclude entries
    for inc in revincluded {
        let inc_type = inc
            .get("resourceType")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let inc_id = inc.get("id").and_then(|v| v.as_str()).unwrap_or("");
        entries.push(json!({
            "fullUrl": format!("{}/{}", inc_type, inc_id),
            "resource": inc,
            "search": {"mode": "include"}
        }));
    }

    // Pagination links
    let count = query.count.unwrap_or(DEFAULT_COUNT);
    let offset = query.offset.unwrap_or(0);
    let mut links: Vec<Value> = Vec::new();

    // Build base query without _count and _offset
    let base_params: String = params
        .params
        .iter()
        .filter(|(k, _)| k.as_str() != "_count" && k.as_str() != "_offset")
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&");

    let base = if base_params.is_empty() {
        format!("/{}?_count={}", resource_type, count)
    } else {
        format!("/{resource_type}?{base_params}&_count={count}")
    };

    // self link
    links.push(json!({
        "relation": "self",
        "url": format!("{}&_offset={}", base, offset)
    }));

    // next link
    if offset + count < total {
        links.push(json!({
            "relation": "next",
            "url": format!("{}&_offset={}", base, offset + count)
        }));
    }

    // previous link
    if offset > 0 {
        let prev_offset = offset.saturating_sub(count);
        links.push(json!({
            "relation": "previous",
            "url": format!("{}&_offset={}", base, prev_offset)
        }));
    }

    audit::log_operation_success(
        &audit_ctx,
        "SEARCH",
        &resource_type,
        &format!("{} results", total),
        &state.audit,
    );

    Ok(Json(json!({
        "resourceType": "Bundle",
        "type": "searchset",
        "total": total,
        "link": links,
        "entry": entries
    })).into_response())
}
