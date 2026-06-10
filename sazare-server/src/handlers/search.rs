use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{Json, Response},
};
use sazare_core::{
    operation_outcome::IssueType,
    resource_filter::{apply_elements, apply_summary},
    OperationOutcome, SearchQuery,
};
use sazare_store::SearchExecutor;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::audit::{self, AuditContext};
use crate::auth::AuthUser;
use crate::compartment_check::filter_by_compartment;
use crate::AppState;

/// Default page size per FHIR spec
const DEFAULT_COUNT: usize = 100;

/// Reconstruct the externally-visible base URL (scheme + authority).
/// Honors `X-Forwarded-Proto` and `X-Forwarded-Host` so reverse-proxied deploys
/// (Fly.io, nginx) emit absolute Bundle.link / entry.fullUrl URLs that clients
/// can resolve back to the same scheme — preventing http→https 301 redirects
/// when downstream tooling follows the link.
fn external_base_url(request: &Request) -> String {
    let headers = request.headers();
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        .unwrap_or_else(|| {
            request
                .uri()
                .scheme_str()
                .unwrap_or("http")
                .to_string()
        });
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("{}://{}", scheme, host)
}

/// Search (GET /{resource_type}?...)
pub async fn search(
    State(state): State<Arc<AppState>>,
    Path(resource_type): Path<String>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();
    let base_url = external_base_url(&request);
    // Use the raw (still percent-encoded) query string so that repeated
    // parameters (AND semantics, e.g. `date=ge..&date=le..`) are preserved and
    // values are decoded exactly once by the parser. Going through a
    // HashMap<String,String> would collapse duplicates (last-wins) and a
    // pre-decoded map would be decoded a second time.
    let raw_query = request.uri().query().unwrap_or("").to_string();
    do_search(state, resource_type, raw_query, auth_user, audit_ctx, base_url).await
}

/// Search via POST (POST /{resource_type}/_search) — FHIR alternative to GET search.
/// Body is `application/x-www-form-urlencoded` with the same params as the query string.
pub async fn search_post(
    State(state): State<Arc<AppState>>,
    Path(resource_type): Path<String>,
    request: Request,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let audit_ctx = AuditContext::from_request(&request);
    let auth_user = request.extensions().get::<AuthUser>().cloned();
    let base_url = external_base_url(&request);

    let body = request.into_body();
    let bytes = axum::body::to_bytes(body, 16 * 1024 * 1024).await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::Invalid,
                format!("Failed to read body: {e}")
            ))),
        )
    })?;
    // The form-encoded body has the same grammar as a query string; pass it
    // through verbatim to preserve repeated parameters and single-decode values.
    let raw_query = String::from_utf8(bytes.to_vec()).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(
                IssueType::Invalid,
                format!("Body is not valid UTF-8: {e}")
            ))),
        )
    })?;

    do_search(state, resource_type, raw_query, auth_user, audit_ctx, base_url).await
}

/// Reconstruct the query string without `_count`/`_offset`, preserving the
/// original percent-encoding and the order/multiplicity of every other
/// parameter (so pagination links round-trip exactly).
fn base_params_without_paging(raw_query: &str) -> String {
    raw_query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            // `_count`/`_offset` are never percent-encoded by clients, so a raw
            // key comparison is sufficient and avoids a decode dependency.
            let key = pair.split('=').next().unwrap_or("");
            key != "_count" && key != "_offset"
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Shared search execution path used by both GET and POST search handlers.
async fn do_search(
    state: Arc<AppState>,
    resource_type: String,
    raw_query: String,
    auth_user: Option<AuthUser>,
    audit_ctx: AuditContext,
    base_url: String,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let query = SearchQuery::parse_for_resource(&raw_query, Some(&resource_type)).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!(OperationOutcome::error(IssueType::Invalid, e))),
        )
    })?;

    // Reject unknown search parameters (FHIR strict handling) rather than
    // silently returning an empty set — but only for resource types we have an
    // explicit parameter registry for, so unmodelled types stay lenient.
    // Underscore result params (`_id`, `_lastUpdated`, …) are already validated
    // by the parser and pass through here.
    if state.search_param_registry.has_resource_type(&resource_type) {
        for p in &query.parameters {
            if p.name.starts_with('_') {
                continue;
            }
            if state
                .search_param_registry
                .lookup_param_type(&resource_type, &p.name)
                .is_none()
            {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!(OperationOutcome::error(
                        IssueType::NotSupported,
                        format!(
                            "'{}' is not a search parameter this server understands for {}. \
                             Check the spelling, or open GET /metadata to see the search \
                             parameters {} supports.",
                            p.name, resource_type, resource_type
                        )
                    ))),
                ));
            }
        }
    }

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
            return Ok(super::fhir_json(StatusCode::OK, json!({
                "resourceType": "Bundle",
                "type": "searchset",
                "total": filtered.len()
            })));
        }

        return Ok(super::fhir_json(StatusCode::OK, json!({
            "resourceType": "Bundle",
            "type": "searchset",
            "total": ids.len()
        })));
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
            .process_includes(&resources, &query.include, &state.search_param_registry)
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
                "{}/{}/{}",
                base_url,
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
            "fullUrl": format!("{}/{}/{}", base_url, inc_type, inc_id),
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
            "fullUrl": format!("{}/{}/{}", base_url, inc_type, inc_id),
            "resource": inc,
            "search": {"mode": "include"}
        }));
    }

    // Pagination links
    let count = query.count.unwrap_or(DEFAULT_COUNT);
    let offset = query.offset.unwrap_or(0);
    let mut links: Vec<Value> = Vec::new();

    // Build base query without _count and _offset (encoding preserved).
    let base_params = base_params_without_paging(&raw_query);

    let base = if base_params.is_empty() {
        format!("{}/{}?_count={}", base_url, resource_type, count)
    } else {
        format!("{base_url}/{resource_type}?{base_params}&_count={count}")
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

    // Omit `entry` entirely when empty — FHIR JSON forbids empty arrays.
    let mut bundle = json!({
        "resourceType": "Bundle",
        "type": "searchset",
        "total": total,
        "link": links,
    });
    if !entries.is_empty() {
        bundle["entry"] = json!(entries);
    }
    Ok(super::fhir_json(StatusCode::OK, bundle))
}
