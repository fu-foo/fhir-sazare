use crate::config::WebhookSettings;
use serde_json::Value;

/// Webhook event types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookEvent {
    BundleCreated,
    TaskCompleted,
}

impl WebhookEvent {
    pub fn as_str(&self) -> &str {
        match self {
            WebhookEvent::BundleCreated => "BundleCreated",
            WebhookEvent::TaskCompleted => "TaskCompleted",
        }
    }
}

/// Webhook manager
pub struct WebhookManager {
    settings: WebhookSettings,
    client: reqwest::Client,
}

impl WebhookManager {
    pub fn new(settings: WebhookSettings) -> Self {
        Self {
            settings,
            client: reqwest::Client::new(),
        }
    }

    /// Trigger webhook for an event
    pub fn trigger(&self, event: WebhookEvent, resource: Value) {
        if !self.settings.enabled {
            return;
        }

        // Find matching endpoints for this event
        let endpoints: Vec<_> = self
            .settings
            .endpoints
            .iter()
            .filter(|ep| ep.events.contains(&event.as_str().to_string()))
            .cloned()
            .collect();

        if endpoints.is_empty() {
            return;
        }

        // Spawn async task to send webhooks (non-blocking)
        let client = self.client.clone();
        tokio::spawn(async move {
            for endpoint in endpoints {
                let mut request = client.post(&endpoint.url).json(&resource);

                // Add custom headers
                for (key, value) in &endpoint.headers {
                    request = request.header(key, value);
                }

                match request.send().await {
                    Ok(response) => {
                        if response.status().is_success() {
                            tracing::info!(
                                url = %endpoint.url,
                                event = event.as_str(),
                                status = %response.status(),
                                "Webhook sent successfully"
                            );
                        } else {
                            tracing::warn!(
                                url = %endpoint.url,
                                event = event.as_str(),
                                status = %response.status(),
                                "Webhook failed with non-success status"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            url = %endpoint.url,
                            event = event.as_str(),
                            error = %e,
                            "Failed to send webhook"
                        );
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WebhookEndpoint;

    #[test]
    fn test_webhook_event_as_str() {
        assert_eq!(WebhookEvent::BundleCreated.as_str(), "BundleCreated");
        assert_eq!(WebhookEvent::TaskCompleted.as_str(), "TaskCompleted");
    }

    #[test]
    fn test_webhook_manager_disabled() {
        let settings = WebhookSettings {
            enabled: false,
            endpoints: vec![],
        };
        let manager = WebhookManager::new(settings);

        // This should not panic when webhooks are disabled
        manager.trigger(WebhookEvent::BundleCreated, serde_json::json!({}));
    }

    #[test]
    fn test_webhook_manager_no_matching_endpoints() {
        let settings = WebhookSettings {
            enabled: true,
            endpoints: vec![WebhookEndpoint {
                url: "http://example.com".to_string(),
                events: vec!["TaskCompleted".to_string()],
                headers: Default::default(),
            }],
        };
        let manager = WebhookManager::new(settings);

        // This should not panic when no endpoints match
        manager.trigger(WebhookEvent::BundleCreated, serde_json::json!({}));
    }
}
