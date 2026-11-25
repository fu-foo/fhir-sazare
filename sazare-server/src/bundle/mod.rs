//! Bundle (transaction/batch) processing
//!
//! POST / — accepts a Bundle of type "transaction" or "batch" and processes
//! each entry according to FHIR R4 rules.

mod batch;
mod transaction;

use crate::audit::AuditContext;
use crate::auth::AuthUser;
use crate::AppState;

use axum::{
    extract::{ConnectInfo, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use sazare_core::{
    operation_outcome::IssueType,
    OperationOutcome,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

/// Parsed entry from a Bundle
pub(crate) struct BundleEntry {
    pub method: String,
    pub resource_type: String,
    pub id: Option<String>,
    pub full_url: Option<String>,
    pub resource: Option<Value>,
    pub if_none_exist: Option<String>,
}

/// Parse request.url to extract resource type and optional id.
/// "Patient" -> ("Patient", None)
/// "Patient/123" -> ("Patient", Some("123"))
fn parse_request_url(url: &str) -> Option<(String, Option<String>)> {
    let url = url.trim_start_matches('/');
    if url.is_empty() {
        return None;
    }
    let parts: Vec<&str> = url.splitn(2, '/').collect();
    let resource_type = parts[0].to_string();
    let id = parts.get(1).map(|s| s.to_string());
    Some((resource_type, id))
}

/// Parse all entries from a Bundle value.
fn parse_entries(bundle: &Value) -> Result<Vec<BundleEntry>, OperationOutcome> {
    let entries = bundle
        .get("entry")
        .and_then(|e| e.as_array())
        .ok_or_else(|| {
            OperationOutcome::error(IssueType::Invalid, "Bundle.entry is missing or not an array")
        })?;

    let mut parsed = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let request = entry.get("request").ok_or_else(|| {
            OperationOutcome::error(
                IssueType::Required,
                format!("entry[{}].request is required", i),
            )
        })?;

        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .ok_or_else(|| {
                OperationOutcome::error(
                    IssueType::Required,
                    format!("entry[{}].request.method is required", i),
                )
            })?
            .to_string();

        let url = request
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| {
                OperationOutcome::error(
                    IssueType::Required,
                    format!("entry[{}].request.url is required", i),
                )
            })?;

        let (resource_type, id) = parse_request_url(url).ok_or_else(|| {
            OperationOutcome::error(
                IssueType::Invalid,
                format!("entry[{}].request.url is invalid: '{}'", i, url),
            )
        })?;

        let full_url = entry.get("fullUrl").and_then(|f| f.as_str()).map(|s| s.to_string());
        let resource = entry.get("resource").cloned();
        let if_none_exist = request.get("ifNoneExist").and_then(|v| v.as_str()).map(|s| s.to_string());

        parsed.push(BundleEntry {
            method,
            resource_type,
            id,
            full_url,
            resource,
            if_none_exist,
        });
    }
    Ok(parsed)
}

/// The checks a REST create/update applies to its body, for a Bundle entry:
/// the body is of the URL's type, a PUT's body id (if any) is the URL's id,
/// and a Subscription passes the Subscription rules.
pub(crate) fn check_entry_body(
    state: &AppState,
    method: &str,
    url_type: &str,
    url_id: Option<&str>,
    resource: &Value,
) -> Result<(), String> {
    let body_type = resource.get("resourceType").and_then(|v| v.as_str()).unwrap_or("");
    if body_type != url_type {
        return Err(format!(
            "resourceType '{}' does not match request URL type '{}'",
            body_type, url_type
        ));
    }
    if method == "PUT"
        && let (Some(url_id), Some(body_id)) = (url_id, resource.get("id").and_then(|v| v.as_str()))
        && url_id != body_id
    {
        return Err(format!("body id '{body_id}' differs from the request URL id '{url_id}'"));
    }
    if url_type == "Subscription" {
        crate::subscription::validate_subscription(resource, &state.search_param_registry)?;
    }
    Ok(())
}

/// Recursively resolve urn:uuid references in a JSON value.
/// Apply the per-request authorization rules to each Bundle entry: JWT scopes
/// (and the patient-token restrictions), then the patient compartment for the
/// entry's body and for the existing resource it reads, replaces or deletes.
fn authorize_entries(state: &AppState, user: &AuthUser, entries: &[BundleEntry]) -> Result<(), OperationOutcome> {
    use crate::auth::patient_scope_may;
    use crate::compartment_check::{check_compartment_access, check_compartment_write};

    let deny = |i: usize, why: String| {
        OperationOutcome::error(IssueType::Forbidden, format!("entry[{i}]: {why}"))
    };
    for (i, e) in entries.iter().enumerate() {
        // `Type?query` (conditional/search URLs) authorizes as `Type`.
        let rt = e.resource_type.split('?').next().unwrap_or("");
        let perm = match e.method.as_str() {
            "GET" | "HEAD" if e.id.is_some() => 'r',
            "GET" | "HEAD" => 's',
            "POST" => 'c',
            "PUT" | "PATCH" => 'u',
            "DELETE" => 'd',
            _ => 'c',
        };
        if user.grant(rt, perm).is_none() {
            return Err(deny(i, format!("insufficient scope: {rt} needs the '{perm}' permission")));
        }
        patient_scope_may(user, rt, perm).map_err(|m| deny(i, m))?;
        let writes = !matches!(perm, 'r' | 's');

        let to_resp = |_| deny(i, "resource is not in the patient compartment".to_string());
        if let Some(body) = &e.resource
            && matches!(e.method.as_str(), "POST" | "PUT")
        {
            check_compartment_write(Some(user), &state.compartment_def, rt, body, perm).map_err(to_resp)?;
        }
        if let Some(id) = &e.id
            && let Ok(Some(data)) = state.store.get(rt, id)
            && let Ok(existing) = serde_json::from_slice::<Value>(&data)
        {
            if writes {
                check_compartment_write(Some(user), &state.compartment_def, rt, &existing, perm).map_err(to_resp)?;
            } else {
                check_compartment_access(Some(user), &state.compartment_def, rt, &existing).map_err(to_resp)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn resolve_references(value: &mut Value, ref_map: &HashMap<String, String>) {
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get_mut("reference")
                && let Some(ref_str) = reference.as_str()
                && let Some(resolved) = ref_map.get(ref_str)
            {
                *reference = Value::String(resolved.clone());
            }
            for v in map.values_mut() {
                resolve_references(v, ref_map);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                resolve_references(item, ref_map);
            }
        }
        _ => {}
    }
}

/// Build an error response entry for batch-response.
pub(crate) fn error_entry(status: &str, message: &str) -> Value {
    json!({
        "response": {
            "status": status,
            "outcome": OperationOutcome::error(IssueType::Processing, message)
        }
    })
}

/// POST / — process a Bundle (transaction or batch)
pub async fn process_bundle(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<AppState>>,
    auth_user: Option<axum::extract::Extension<AuthUser>>,
    Json(bundle): Json<Value>,
) -> impl IntoResponse {
    let auth_user = auth_user.map(|axum::extract::Extension(u)| u);
    let user_id = auth_user.as_ref().map(|u| u.user_id.clone());
    let audit_ctx = AuditContext::new(user_id, addr.ip().to_string());

    // Validate top-level structure
    let rt = bundle.get("resourceType").and_then(|v| v.as_str());
    if rt != Some("Bundle") {
        let outcome =
            OperationOutcome::error(IssueType::Invalid, "resourceType must be 'Bundle'");
        return (StatusCode::BAD_REQUEST, Json(json!(outcome))).into_response();
    }

    let bundle_type = match bundle.get("type").and_then(|v| v.as_str()) {
        Some(t @ ("transaction" | "batch")) => t.to_string(),
        _ => {
            let outcome = OperationOutcome::error(
                IssueType::Invalid,
                "Bundle.type must be 'transaction' or 'batch'",
            );
            return (StatusCode::BAD_REQUEST, Json(json!(outcome))).into_response();
        }
    };

    // Parse entries
    let entries = match parse_entries(&bundle) {
        Ok(e) => e,
        Err(outcome) => {
            return (StatusCode::BAD_REQUEST, Json(json!(outcome))).into_response();
        }
    };

    // Every entry is authorized up front, exactly like the equivalent direct
    // request; one refused entry refuses the whole Bundle.
    if let Some(user) = auth_user.as_ref()
        && let Err(outcome) = authorize_entries(&state, user, &entries)
    {
        return (StatusCode::FORBIDDEN, Json(json!(outcome))).into_response();
    }

    let response = if bundle_type == "transaction" {
        transaction::process_transaction(&state, &audit_ctx, auth_user.as_ref(), entries).await
    } else {
        batch::process_batch(&state, &audit_ctx, auth_user.as_ref(), entries).await
    };

    // Fire the BundleCreated lifecycle webhook on a successful bundle.
    if response.status().is_success() {
        state
            .webhook
            .trigger(crate::webhook::WebhookEvent::BundleCreated, bundle);
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_references_basic() {
        let mut resource = json!({
            "resourceType": "Observation",
            "subject": {
                "reference": "urn:uuid:abc-123"
            }
        });

        let mut ref_map = HashMap::new();
        ref_map.insert(
            "urn:uuid:abc-123".to_string(),
            "Patient/xyz-456".to_string(),
        );

        resolve_references(&mut resource, &ref_map);

        assert_eq!(
            resource["subject"]["reference"].as_str().unwrap(),
            "Patient/xyz-456"
        );
    }

    #[test]
    fn test_resolve_references_nested() {
        let mut resource = json!({
            "resourceType": "Encounter",
            "subject": {
                "reference": "urn:uuid:p1"
            },
            "contained": [{
                "resourceType": "Condition",
                "subject": {
                    "reference": "urn:uuid:p1"
                }
            }]
        });

        let mut ref_map = HashMap::new();
        ref_map.insert("urn:uuid:p1".to_string(), "Patient/real-id".to_string());

        resolve_references(&mut resource, &ref_map);

        assert_eq!(
            resource["subject"]["reference"].as_str().unwrap(),
            "Patient/real-id"
        );
        assert_eq!(
            resource["contained"][0]["subject"]["reference"]
                .as_str()
                .unwrap(),
            "Patient/real-id"
        );
    }

    #[test]
    fn test_resolve_references_no_match() {
        let mut resource = json!({
            "subject": {
                "reference": "Patient/existing-id"
            }
        });

        let ref_map = HashMap::new();
        resolve_references(&mut resource, &ref_map);

        assert_eq!(
            resource["subject"]["reference"].as_str().unwrap(),
            "Patient/existing-id"
        );
    }

    #[test]
    fn test_parse_request_url_post() {
        let (rt, id) = parse_request_url("Patient").unwrap();
        assert_eq!(rt, "Patient");
        assert_eq!(id, None);
    }

    #[test]
    fn test_parse_request_url_put() {
        let (rt, id) = parse_request_url("Patient/123").unwrap();
        assert_eq!(rt, "Patient");
        assert_eq!(id, Some("123".to_string()));
    }

    #[test]
    fn test_parse_request_url_empty() {
        assert!(parse_request_url("").is_none());
    }
}
