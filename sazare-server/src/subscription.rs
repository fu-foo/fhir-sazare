use sazare_core::{SearchParamRegistry, SearchQuery};
use sazare_store::SearchExecutor;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, warn};

use crate::AppState;

/// Validate a Subscription resource before saving.
///
/// Checks:
/// 1. criteria format: `ResourceType?param=value` with known resource type and params
/// 2. channel.type must be "rest-hook" (only supported type)
/// 3. channel.endpoint must be present and non-empty for rest-hook
/// 4. status must be a valid Subscription status
pub fn validate_subscription(
    resource: &Value,
    registry: &SearchParamRegistry,
) -> Result<(), String> {
    // Validate status
    let status = resource
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !matches!(
        status,
        "requested" | "active" | "error" | "off" | "entered-in-error"
    ) {
        return Err(format!(
            "Invalid Subscription status: '{}'. Must be one of: requested, active, error, off, entered-in-error",
            status
        ));
    }

    // Validate criteria
    let criteria = resource
        .get("criteria")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if criteria.is_empty() {
        return Err("Subscription.criteria is required".to_string());
    }

    let (criteria_type, criteria_query) = if let Some(idx) = criteria.find('?') {
        (&criteria[..idx], &criteria[idx + 1..])
    } else {
        (criteria, "")
    };

    if !registry.has_resource_type(criteria_type) {
        return Err(format!(
            "Unknown resource type in criteria: '{}'. Not in SearchParamRegistry",
            criteria_type
        ));
    }

    // Validate search parameters in criteria
    if !criteria_query.is_empty() {
        let query = SearchQuery::parse(criteria_query)
            .map_err(|e| format!("Invalid criteria query: {}", e))?;

        for param in &query.parameters {
            if registry.lookup_param_type(criteria_type, &param.name).is_none() {
                return Err(format!(
                    "Unknown search parameter '{}' for resource type '{}' in criteria",
                    param.name, criteria_type
                ));
            }
        }
    }

    // Validate channel
    let channel = resource
        .get("channel")
        .ok_or("Subscription.channel is required")?;

    let channel_type = channel
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if channel_type != "rest-hook" {
        return Err(format!(
            "Unsupported channel type: '{}'. Only 'rest-hook' is supported",
            channel_type
        ));
    }

    let endpoint = channel
        .get("endpoint")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if endpoint.is_empty() {
        return Err("channel.endpoint is required for rest-hook".to_string());
    }

    Ok(())
}

/// Subscription notification manager.
///
/// When a resource is created/updated, checks active Subscription resources
/// and sends rest-hook notifications to matching endpoints.
pub struct SubscriptionManager;

impl SubscriptionManager {
    /// Notify matching subscriptions after a resource change.
    ///
    /// This should be spawned as a background task so it doesn't block the response.
    pub async fn notify(
        state: &Arc<AppState>,
        resource_type: &str,
        resource_id: &str,
        resource: &Value,
    ) {
        let subscriptions = match Self::get_active_subscriptions(state) {
            Ok(subs) => subs,
            Err(e) => {
                warn!("Failed to load subscriptions: {}", e);
                return;
            }
        };

        for sub in &subscriptions {
            if let Err(e) = Self::process_subscription(state, sub, resource_type, resource_id, resource).await {
                debug!("Subscription notification failed: {}", e);
                // Update subscription status to error
                Self::update_subscription_status(state, sub, "error").await;
            }
        }
    }

    /// Get all active Subscription resources.
    fn get_active_subscriptions(state: &AppState) -> Result<Vec<Value>, String> {
        let all = state
            .store
            .list_all(Some("Subscription"))
            .map_err(|e| e.to_string())?;

        let mut active = Vec::new();
        for (_rt, _id, data) in all {
            if let Ok(sub) = serde_json::from_slice::<Value>(&data) {
                let status = sub
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if status == "active" || status == "requested" {
                    active.push(sub);
                }
            }
        }
        Ok(active)
    }

    /// Check if a subscription matches and send notification.
    async fn process_subscription(
        state: &Arc<AppState>,
        subscription: &Value,
        resource_type: &str,
        resource_id: &str,
        _resource: &Value,
    ) -> Result<(), String> {
        // Parse criteria (e.g. "Observation?code=85354-9")
        let criteria = subscription
            .get("criteria")
            .and_then(|v| v.as_str())
            .ok_or("No criteria in Subscription")?;

        let (criteria_type, criteria_query) = if let Some(idx) = criteria.find('?') {
            (&criteria[..idx], &criteria[idx + 1..])
        } else {
            (criteria, "")
        };

        // Check resource type matches
        if criteria_type != resource_type {
            return Ok(());
        }

        // If there are query params, check if the resource matches
        if !criteria_query.is_empty() {
            let query = SearchQuery::parse(criteria_query).map_err(|e| e.to_string())?;

            let index = state.index.lock().await;
            let executor = SearchExecutor::new(&state.store, &index);
            let ids = executor.search(resource_type, &query)?;

            if !ids.contains(&resource_id.to_string()) {
                return Ok(());
            }
        }

        // Get channel info
        let channel = subscription
            .get("channel")
            .ok_or("No channel in Subscription")?;

        let channel_type = channel
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if channel_type != "rest-hook" {
            return Ok(()); // Only rest-hook is supported
        }

        let endpoint = channel
            .get("endpoint")
            .and_then(|v| v.as_str())
            .ok_or("No endpoint in channel")?;

        // Send HTTP POST to endpoint
        let client = reqwest::Client::new();
        let mut request = client.post(endpoint);

        // Add custom headers if specified
        if let Some(headers) = channel.get("header").and_then(|v| v.as_array()) {
            for header_val in headers {
                if let Some(header_str) = header_val.as_str()
                    && let Some(colon_idx) = header_str.find(':')
                {
                    let name = header_str[..colon_idx].trim();
                    let value = header_str[colon_idx + 1..].trim();
                    if let (Ok(name), Ok(value)) = (
                        reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                        reqwest::header::HeaderValue::from_str(value),
                    ) {
                        request = request.header(name, value);
                    }
                }
            }
        }

        let payload_type = channel
            .get("payload")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Send notification based on payload content type
        if payload_type.contains("json") {
            // Full resource payload
            if let Ok(Some(data)) = state.store.get(resource_type, resource_id) {
                request = request
                    .header("Content-Type", "application/fhir+json")
                    .body(data);
            }
        }
        // Empty payload or other types: just send the POST with no body

        let response = request
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Endpoint returned status: {}", response.status()));
        }

        debug!(
            "Subscription notification sent to {} for {}/{}",
            endpoint, resource_type, resource_id
        );

        Ok(())
    }

    /// Update subscription status (e.g. to "error" on failure).
    async fn update_subscription_status(state: &AppState, subscription: &Value, new_status: &str) {
        let id = match subscription.get("id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return,
        };

        if let Ok(Some(data)) = state.store.get("Subscription", id)
            && let Ok(mut sub) = serde_json::from_slice::<Value>(&data)
            && let Some(obj) = sub.as_object_mut()
        {
            obj.insert("status".to_string(), serde_json::json!(new_status));
            if let Ok(bytes) = serde_json::to_vec(&sub) {
                let version = sub
                    .get("meta")
                    .and_then(|m| m.get("versionId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("1");
                let new_ver: i32 = version.parse().unwrap_or(1) + 1;
                let _ = state.store.put_with_version(
                    "Subscription",
                    id,
                    &new_ver.to_string(),
                    &bytes,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sazare_core::SearchParamRegistry;
    use serde_json::json;

    fn registry() -> SearchParamRegistry {
        SearchParamRegistry::new()
    }

    fn valid_subscription() -> Value {
        json!({
            "resourceType": "Subscription",
            "status": "active",
            "criteria": "Observation?code=85354-9",
            "channel": {
                "type": "rest-hook",
                "endpoint": "http://example.com/notify"
            }
        })
    }

    #[test]
    fn test_valid_subscription() {
        assert!(validate_subscription(&valid_subscription(), &registry()).is_ok());
    }

    #[test]
    fn test_invalid_status() {
        let mut sub = valid_subscription();
        sub["status"] = json!("bogus");
        let result = validate_subscription(&sub, &registry());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid Subscription status"));
    }

    #[test]
    fn test_unknown_resource_type_in_criteria() {
        let mut sub = valid_subscription();
        sub["criteria"] = json!("FakeResource?foo=bar");
        let result = validate_subscription(&sub, &registry());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown resource type"));
    }

    #[test]
    fn test_unknown_search_param_in_criteria() {
        let mut sub = valid_subscription();
        sub["criteria"] = json!("Observation?nonexistent=xyz");
        let result = validate_subscription(&sub, &registry());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown search parameter"));
    }

    #[test]
    fn test_unsupported_channel_type() {
        let mut sub = valid_subscription();
        sub["channel"]["type"] = json!("websocket");
        let result = validate_subscription(&sub, &registry());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported channel type"));
    }

    #[test]
    fn test_missing_endpoint() {
        let mut sub = valid_subscription();
        sub["channel"] = json!({"type": "rest-hook"});
        let result = validate_subscription(&sub, &registry());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("endpoint is required"));
    }

    #[test]
    fn test_criteria_without_params() {
        let mut sub = valid_subscription();
        sub["criteria"] = json!("Observation");
        assert!(validate_subscription(&sub, &registry()).is_ok());
    }
}

