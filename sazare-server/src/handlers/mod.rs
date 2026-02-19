pub mod conditional;
pub mod crud;
pub mod everything;
pub mod history;
pub mod metadata;
pub mod search;
pub mod validate;

use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json},
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

/// Build response with ETag header
pub fn response_with_etag(status: StatusCode, resource: Value) -> impl IntoResponse {
    let etag = extract_version(&resource)
        .map(|v| format!("W/\"{}\"", v))
        .unwrap_or_default();

    let mut headers = HeaderMap::new();
    if !etag.is_empty()
        && let Ok(val) = etag.parse()
    {
        headers.insert(header::ETAG, val);
    }
    headers.insert(
        header::CONTENT_TYPE,
        "application/fhir+json; charset=utf-8".parse().unwrap(),
    );

    (status, headers, Json(resource))
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
