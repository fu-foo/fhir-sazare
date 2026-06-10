pub mod conditional;
pub mod crud;
pub mod everything;
pub mod history;
pub mod metadata;
pub mod reindex;
pub mod search;
pub mod validate;

use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde_json::Value;
use sazare_core::SearchParamRegistry;
use sazare_store::{IndexBuilder, SearchIndex};

/// Extract version from meta for ETag
pub fn extract_version(resource: &Value) -> Option<String> {
    resource
        .get("meta")
        .and_then(|m| m.get("versionId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Build response with ETag + Last-Modified headers.
pub fn response_with_etag(status: StatusCode, resource: Value) -> impl IntoResponse {
    response_with_headers(status, resource, None)
}

/// Convert an RFC3339 `meta.lastUpdated` into an HTTP-date for `Last-Modified`.
fn http_date(resource: &Value) -> Option<String> {
    let raw = resource
        .get("meta")
        .and_then(|m| m.get("lastUpdated"))
        .and_then(|v| v.as_str())?;
    let dt = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    Some(dt.with_timezone(&chrono::Utc).format("%a, %d %b %Y %H:%M:%S GMT").to_string())
}

/// Build a response carrying `ETag`, `Last-Modified`, FHIR content type, and an
/// optional `Location` header (used on create/update so clients learn the
/// server-assigned id and version URL).
pub fn response_with_headers(
    status: StatusCode,
    resource: Value,
    location: Option<String>,
) -> impl IntoResponse {
    let mut headers = HeaderMap::new();

    if let Some(etag) = extract_version(&resource).map(|v| format!("W/\"{}\"", v))
        && let Ok(val) = etag.parse()
    {
        headers.insert(header::ETAG, val);
    }
    if let Some(lm) = http_date(&resource)
        && let Ok(val) = lm.parse()
    {
        headers.insert(header::LAST_MODIFIED, val);
    }
    if let Some(loc) = location
        && let Ok(val) = loc.parse()
    {
        headers.insert(header::LOCATION, val);
    }
    headers.insert(
        header::CONTENT_TYPE,
        "application/fhir+json; charset=utf-8".parse().unwrap(),
    );

    (status, headers, Json(resource))
}

/// Build the `Location`/`Content-Location` URL for a versioned resource.
pub fn version_location(base_url: &str, resource_type: &str, id: &str, version_id: &str) -> String {
    format!("{base_url}/{resource_type}/{id}/_history/{version_id}")
}

/// Reconstruct the externally-visible base URL (scheme + authority) from request
/// headers, honoring `X-Forwarded-Proto`/`X-Forwarded-Host`.
pub fn base_url_from_headers(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        .unwrap_or_else(|| "http".to_string());
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}

/// Merge versionId + lastUpdated into a resource's `meta` object, preserving
/// any caller-provided fields (profile, tag, security, source, extension).
pub fn merge_version_meta(obj: &mut serde_json::Map<String, Value>, version_id: &str) {
    let now = chrono::Utc::now().to_rfc3339();
    let meta_entry = obj
        .entry("meta".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !meta_entry.is_object() {
        *meta_entry = Value::Object(serde_json::Map::new());
    }
    let meta = meta_entry.as_object_mut().unwrap();
    meta.insert("versionId".to_string(), Value::String(version_id.to_string()));
    meta.insert("lastUpdated".to_string(), Value::String(now));
}

/// Wrap a JSON body (Bundle, OperationOutcome, …) in a response carrying the
/// FHIR media type `application/fhir+json` rather than bare `application/json`.
pub fn fhir_json(status: StatusCode, body: Value) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/fhir+json; charset=utf-8".parse().unwrap(),
    );
    (status, headers, Json(body)).into_response()
}

/// Update search index for a resource (synchronous — must not be async)
pub fn update_search_index(
    index: &SearchIndex,
    registry: &SearchParamRegistry,
    resource_type: &str,
    id: &str,
    resource: &Value,
) {
    let _ = index.remove_index(resource_type, id);
    let indices = IndexBuilder::extract_indices_with_registry(registry, resource_type, resource);
    for (param_name, param_type, value, system) in indices {
        let _ = index.add_index(
            resource_type,
            id,
            &param_name,
            &param_type,
            Some(&value),
            system.as_deref(),
        );
    }
}
