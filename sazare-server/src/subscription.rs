use sazare_core::{SearchParamRegistry, SearchQuery};
use sazare_store::{SearchExecutor, SearchIndex};
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
        let query = SearchQuery::parse_with_registry(criteria_query, Some(criteria_type), registry)
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

    match channel_type {
        "rest-hook" => {
            let endpoint = channel
                .get("endpoint")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if endpoint.is_empty() {
                return Err("channel.endpoint is required for rest-hook".to_string());
            }
            // The notification body: none (absent payload) or JSON.
            if let Some(payload) = channel.get("payload").and_then(|v| v.as_str())
                && !payload.split(';').next().unwrap_or("").trim().to_ascii_lowercase().contains("json")
            {
                return Err(format!(
                    "channel.payload '{payload}' is not supported: notifications are sent as application/fhir+json (or leave payload out for an empty ping)"
                ));
            }
            match reqwest::Url::parse(endpoint) {
                Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() => {}
                _ => {
                    return Err(format!(
                        "channel.endpoint must be an absolute http(s) URL, got '{endpoint}'"
                    ));
                }
            }
        }
        // websocket clients connect to the server's /ws endpoint and `bind` by
        // subscription id, so no channel.endpoint is required here.
        "websocket" => {}
        other => {
            return Err(format!(
                "Unsupported channel type: '{}'. Supported: rest-hook, websocket",
                other
            ));
        }
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
        // No Subscription at all (the common case): nothing to read or match.
        if !Self::any(state) {
            return;
        }
        let subscriptions = match Self::get_active_subscriptions(state) {
            Ok(subs) => subs,
            Err(e) => {
                warn!("Failed to load subscriptions: {}", e);
                return;
            }
        };

        for sub in &subscriptions {
            if let Err(e) = Self::process_subscription(state, sub, resource_type, resource_id, resource).await {
                let sub_id = sub.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
                // Record the failure: a warning in the log AND an audit entry, so a
                // dead endpoint is visible rather than swallowed.
                warn!("Subscription {} delivery failed: {}", sub_id, e);
                let ctx = crate::audit::AuditContext::new(
                    Some("subscription".to_string()),
                    "system".to_string(),
                );
                crate::audit::log_operation_error(
                    &ctx,
                    "NOTIFY",
                    "Subscription",
                    Some(sub_id),
                    &e,
                    &state.audit,
                );
                // Mark the subscription as errored (only after retries are exhausted).
                Self::update_subscription_status(state, sub, "error").await;
            }
        }
    }

    /// Whether any Subscription is stored (a cheap check before the work).
    pub fn any(state: &AppState) -> bool {
        state.store.has_type("Subscription").unwrap_or(true)
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
        resource: &Value,
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
            let query = SearchQuery::parse_with_registry(criteria_query, Some(criteria_type), &state.search_param_registry)
                .map_err(|e| e.to_string())?;

            let matched = if query.chain_parameters.is_empty() && query.has_parameters.is_empty() {
                // Only "does this one resource match?" matters: judge it on an
                // in-memory index holding just its own entries, so the cost
                // doesn't grow with the size of the database (nor take the
                // shared index lock).
                let one = SearchIndex::open(":memory:").map_err(|e| e.to_string())?;
                let update = crate::handlers::index_update(&state.search_param_registry, resource_type, resource_id, resource);
                one.replace_many(&[update]).map_err(|e| e.to_string())?;
                let executor = SearchExecutor::new(&state.store, &one);
                executor.search(resource_type, &query)?.iter().any(|id| id == resource_id)
            } else {
                // Chains and _has look at other resources: use the real index.
                let index = state.index.lock().await;
                let executor = SearchExecutor::new(&state.store, &index);
                executor.search(resource_type, &query)?.iter().any(|id| id == resource_id)
            };
            if !matched {
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

        // websocket channel: notify bound clients with an R4 `ping <id>` frame.
        if channel_type == "websocket" {
            let sub_id = subscription.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let n = state.ws_registry.ping(sub_id).await;
            debug!(
                "websocket ping sent to {} client(s) for subscription {} ({}/{})",
                n, sub_id, resource_type, resource_id
            );
            return Ok(());
        }

        if channel_type != "rest-hook" {
            return Ok(()); // Other channel types are not delivered
        }

        let endpoint = channel
            .get("endpoint")
            .and_then(|v| v.as_str())
            .ok_or("No endpoint in channel")?;

        // Pre-fetch the payload body once (when the channel wants the full
        // resource); reused across retry attempts.
        let payload_type = channel
            .get("payload")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let body = if payload_type.to_ascii_lowercase().contains("json") {
            state.store.get(resource_type, resource_id).ok().flatten()
        } else {
            None
        };

        // Best-effort delivery: a few attempts with short backoff (500ms, 1s).
        // This is deliberately NOT durable/guaranteed delivery — no persistent
        // queue, no ordering — which belongs to a dedicated eventing tier, not a
        // lightweight single-binary server. Transient endpoint blips are smoothed
        // over; a persistently-failing endpoint exhausts the retries and is
        // recorded by the caller.
        const MAX_ATTEMPTS: u32 = 3;
        let mut last_err = String::new();
        for attempt in 1..=MAX_ATTEMPTS {
            let allow_private =
                !state.config.auth.enabled || state.config.subscription.allow_private_endpoints;
            match Self::deliver_rest_hook(channel, endpoint, body.as_deref(), allow_private).await {
                Ok(()) => {
                    debug!(
                        "Subscription notification sent to {} for {}/{} (attempt {})",
                        endpoint, resource_type, resource_id, attempt
                    );
                    return Ok(());
                }
                Err(e) => {
                    last_err = e;
                    if attempt < MAX_ATTEMPTS {
                        let backoff =
                            std::time::Duration::from_millis(500u64 * (1u64 << (attempt - 1)));
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }
        Err(format!(
            "rest-hook delivery to {} failed after {} attempts: {}",
            endpoint, MAX_ATTEMPTS, last_err
        ))
    }

    /// One rest-hook delivery attempt: POST the optional body to `endpoint` with
    /// the subscription's custom headers. Err on transport failure or non-2xx.
    ///
    /// Unless `allow_private`, the endpoint must resolve to public addresses
    /// only: IP literals are checked directly and host names go through a
    /// resolver that drops non-public addresses (so DNS can't point a public
    /// name at an internal one). Redirects are not followed.
    async fn deliver_rest_hook(
        channel: &Value,
        endpoint: &str,
        body: Option<&[u8]>,
        allow_private: bool,
    ) -> Result<(), String> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30));
        if !allow_private {
            let url = reqwest::Url::parse(endpoint).map_err(|e| format!("invalid endpoint: {e}"))?;
            // IP literals bypass the resolver, so check them here.
            let host = url.host_str().unwrap_or("").trim_start_matches('[').trim_end_matches(']');
            if let Ok(ip) = host.parse::<std::net::IpAddr>()
                && !is_public_ip(ip)
            {
                return Err(format!("endpoint {endpoint} is not a public address"));
            }
            builder = builder.dns_resolver(Arc::new(PublicOnlyResolver));
        }
        let client = builder.build().map_err(|e| format!("HTTP client: {e}"))?;
        let mut request = client.post(endpoint);

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

        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/fhir+json")
                .body(body.to_vec());
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {}", e))?;
        if !response.status().is_success() {
            return Err(format!("endpoint returned status {}", response.status()));
        }
        Ok(())
    }

    /// Update subscription status (e.g. to "error" on failure).
    async fn update_subscription_status(state: &AppState, subscription: &Value, new_status: &str) {
        // Index and store writes happen under the write gate (shared), so an index
        // switch never finds a write between its two steps.
        let _write_gate = state.write_gate.read().await;
        let id = match subscription.get("id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return,
        };

        if let Ok(Some(data)) = state.store.get("Subscription", id)
            && let Ok(mut sub) = serde_json::from_slice::<Value>(&data)
            && sub.is_object()
        {
            let current = sub
                .get("meta")
                .and_then(|m| m.get("versionId"))
                .and_then(|v| v.as_str())
                .unwrap_or("1")
                .to_string();
            let new_ver = (current.parse::<i32>().unwrap_or(1) + 1).to_string();
            {
                let obj = sub.as_object_mut().unwrap();
                obj.insert("status".to_string(), serde_json::json!(new_status));
                // versionId and lastUpdated, as every write sets them.
                crate::handlers::merge_version_meta(obj, &new_ver);
            }
            if let Ok(bytes) = serde_json::to_vec(&sub) {
                // Compare-and-swap so a concurrent user edit of the Subscription
                // isn't silently clobbered by this background status flip.
                let wrote = state
                    .store
                    .put_with_version_cas("Subscription", id, Some(&current), &new_ver, &bytes)
                    .unwrap_or(false);
                // Keep the search index in step (status is an indexed param).
                if wrote {
                    let index = state.index.lock().await;
                    crate::handlers::update_search_index(
                        &index,
                        &state.search_param_registry,
                        "Subscription",
                        id,
                        &sub,
                    );
                }
            }
        }
    }
}

/// True for addresses a rest-hook may reach when private endpoints are not
/// allowed: not loopback, private, link-local, CGNAT, unspecified, broadcast,
/// multicast or documentation ranges (IPv4-mapped IPv6 judged as IPv4).
pub fn is_public_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1])) // 100.64.0.0/10 (CGNAT)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 (protocol assignments)
                || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15 (benchmarking)
                || o[0] >= 240) // 240.0.0.0/4 (reserved)
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            let embedded = |hi: u16, lo: u16| IpAddr::V4(std::net::Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8));
            // IPv4 inside IPv6: mapped (::ffff:a.b.c.d), compatible (::a.b.c.d),
            // NAT64 (64:ff9b::/96) and 6to4 (2002:aabb:ccdd::/16) all reach
            // that IPv4 address: judge it.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            if seg[..6] == [0, 0, 0, 0, 0, 0] && !v6.is_loopback() && !v6.is_unspecified() {
                return is_public_ip(embedded(seg[6], seg[7]));
            }
            if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return is_public_ip(embedded(seg[6], seg[7]));
            }
            if seg[0] == 0x2002 {
                return is_public_ip(embedded(seg[1], seg[2]));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
                || (seg[0] & 0xffc0) == 0xfec0 // fec0::/10 site-local (deprecated)
                || (seg[0] == 0x2001 && seg[1] == 0x0db8)) // 2001:db8::/32 documentation
        }
    }
}

/// DNS resolver that only returns public addresses (see [`is_public_ip`]).
struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| is_public_ip(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} does not resolve to a public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
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
        sub["channel"]["type"] = json!("email");
        let result = validate_subscription(&sub, &registry());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported channel type"));
    }

    #[test]
    fn test_websocket_channel_accepted_without_endpoint() {
        // websocket clients connect to /ws and bind by id, so no endpoint needed.
        let mut sub = valid_subscription();
        sub["channel"] = json!({"type": "websocket"});
        assert!(validate_subscription(&sub, &registry()).is_ok());
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

    #[test]
    fn test_payload_is_json_or_absent() {
        // Notifications are JSON (or an empty ping): XML was accepted and
        // then sent as an empty ping.
        let mut sub = valid_subscription();
        for ok in ["application/fhir+json", "application/json; charset=utf-8", "Application/FHIR+JSON"] {
            sub["channel"]["payload"] = json!(ok);
            assert!(validate_subscription(&sub, &registry()).is_ok(), "{ok}");
        }
        sub["channel"]["payload"] = json!("application/fhir+xml");
        assert!(validate_subscription(&sub, &registry()).unwrap_err().contains("payload"));
    }
}


#[cfg(test)]
mod delivery_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A throwaway HTTP server that accepts one connection and replies with the
    /// given status line, so we can exercise the rest-hook delivery primitive.
    async fn one_shot_server(status_line: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let resp = format!("HTTP/1.1 {status_line}\r\nContent-Length: 0\r\n\r\n");
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn deliver_ok_on_2xx() {
        let url = one_shot_server("200 OK").await;
        let channel = serde_json::json!({});
        assert!(SubscriptionManager::deliver_rest_hook(&channel, &url, None, true).await.is_ok());
    }

    #[tokio::test]
    async fn deliver_err_on_5xx() {
        let url = one_shot_server("500 Internal Server Error").await;
        let channel = serde_json::json!({});
        let r = SubscriptionManager::deliver_rest_hook(&channel, &url, None, true).await;
        assert!(r.is_err(), "non-2xx must be an error so the retry loop kicks in");
    }

    #[tokio::test]
    async fn deliver_refuses_private_endpoints_unless_allowed() {
        let url = one_shot_server("200 OK").await; // http://127.0.0.1:port/
        let channel = serde_json::json!({});
        let r = SubscriptionManager::deliver_rest_hook(&channel, &url, None, false).await;
        assert!(r.unwrap_err().contains("not a public address"));
        // A host name resolving only to loopback is refused too.
        let port = url.rsplit(':').next().unwrap().trim_end_matches('/');
        let named = format!("http://localhost:{port}/");
        assert!(SubscriptionManager::deliver_rest_hook(&channel, &named, None, false).await.is_err());
    }

    #[test]
    fn public_ip_classification() {
        for private in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.169.254",
                        "100.64.0.1", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:10.0.0.1"] {
            assert!(!is_public_ip(private.parse().unwrap()), "{private}");
        }
        for public in ["8.8.8.8", "203.0.114.1", "2001:4860:4860::8888"] {
            assert!(is_public_ip(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn ipv4_inside_ipv6_and_special_ranges_are_not_public() {
        use std::net::IpAddr;
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        for private in [
            "::ffff:127.0.0.1", "::127.0.0.1", "::10.0.0.1", "64:ff9b::10.0.0.1", "64:ff9b::7f00:1",
            "2002:7f00:1::", "2002:c0a8:101::1", "fec0::1", "2001:db8::1",
            "198.18.0.1", "198.19.255.255", "240.0.0.1", "255.255.255.255", "192.0.0.1", "169.254.169.254", "100.64.0.1",
        ] {
            assert!(!is_public_ip(ip(private)), "{private}");
        }
        for public in ["8.8.8.8", "::ffff:8.8.8.8", "64:ff9b::808:808", "2002:808:808::1", "2606:4700::1111", "198.20.0.1"] {
            assert!(is_public_ip(ip(public)), "{public}");
        }
    }
}
