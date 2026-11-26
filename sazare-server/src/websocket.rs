//! FHIR R4 Subscription WebSocket channel.
//!
//! Implements the simple R4 websocket notification protocol: a client opens a
//! WebSocket to `/ws`, sends the text frame `bind <subscription-id>`, and from
//! then on receives `ping <subscription-id>` whenever a resource matching that
//! Subscription's criteria changes. The client reacts to a ping by running the
//! Subscription's search to fetch the new data (the ping carries no payload).

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    response::Response,
};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use crate::AppState;

/// Messages queued for one client. A ping carries no data (it means "search
/// again"), so a client this far behind loses nothing when more are dropped.
const QUEUE: usize = 64;
/// How often an idle connection is pinged; one that answered nothing within
/// two of these is gone (a dropped NAT mapping, a sleeping laptop).
const KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(30);

/// Tracks live WebSocket clients bound to Subscription ids.
#[derive(Default)]
pub struct WsRegistry {
    /// subscription id -> outbound channel for every client bound to it
    bound: Mutex<HashMap<String, Vec<mpsc::Sender<Message>>>>,
}

impl WsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a client's outbound channel to a subscription id.
    async fn bind(&self, sub_id: &str, tx: mpsc::Sender<Message>) {
        self.bound
            .lock()
            .await
            .entry(sub_id.to_string())
            .or_default()
            .push(tx);
    }

    /// Send `ping <sub_id>` to every client bound to the subscription, pruning
    /// any whose channel has closed (a full queue skips the ping). Returns the
    /// number of clients still bound.
    pub async fn ping(&self, sub_id: &str) -> usize {
        let mut bound = self.bound.lock().await;
        let Some(senders) = bound.get_mut(sub_id) else {
            return 0;
        };
        let msg = format!("ping {sub_id}");
        senders.retain(|tx| !matches!(tx.try_send(Message::Text(msg.clone().into())), Err(TrySendError::Closed(_))));
        let n = senders.len();
        if n == 0 {
            bound.remove(sub_id);
        }
        n
    }
}

/// `GET /ws` — upgrade to a FHIR R4 subscription websocket.
///
/// Notifications fire for every patient's data, so a patient-scoped token may
/// not listen (the same rule as for managing Subscriptions).
pub async fn ws_handler(
    State(state): State<Arc<AppState>>,
    auth_user: Option<axum::extract::Extension<crate::auth::AuthUser>>,
    // Refuse by authorization before judging the upgrade request itself.
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    use axum::response::IntoResponse;
    if auth_user.is_some_and(|axum::extract::Extension(u)| u.compartment_restricted("Subscription", 'r')) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(sazare_core::OperationOutcome::forbidden(
                "Patient-scoped tokens may not listen to Subscription notifications",
            )),
        )
            .into_response();
    }
    match ws {
        Ok(ws) => ws.on_upgrade(move |socket| handle_socket(socket, state)),
        Err(rejection) => rejection.into_response(),
    }
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    // Outbound queue: pings from the registry and our handshake replies are
    // funneled here and written to the socket by the select loop below.
    let (tx, mut rx) = mpsc::channel::<Message>(QUEUE);
    let mut keepalive = tokio::time::interval(KEEPALIVE);
    keepalive.tick().await; // the first tick is immediate
    let mut heard = true;

    loop {
        tokio::select! {
            _ = keepalive.tick() => {
                if !heard {
                    debug!("websocket client silent for {}s, closing", 2 * KEEPALIVE.as_secs());
                    break;
                }
                heard = false;
                if socket.send(Message::Ping(Default::default())).await.is_err() {
                    break;
                }
            }
            outbound = rx.recv() => {
                match outbound {
                    Some(msg) => {
                        if socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            incoming = socket.recv() => {
                heard = true;
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(sub_id) = text.trim().strip_prefix("bind ") {
                            let sub_id = sub_id.trim().to_string();
                            // Only an existing websocket-channel Subscription
                            // can be bound — not an arbitrary id.
                            let is_ws_subscription = state
                                .store
                                .get("Subscription", &sub_id)
                                .ok()
                                .flatten()
                                .and_then(|d| serde_json::from_slice::<serde_json::Value>(&d).ok())
                                .is_some_and(|s| {
                                    s.pointer("/channel/type").and_then(|t| t.as_str()) == Some("websocket")
                                });
                            if !is_ws_subscription {
                                let _ = tx.try_send(Message::Text(
                                    format!("error {sub_id}: no websocket Subscription with this id").into(),
                                ));
                                continue;
                            }
                            state.ws_registry.bind(&sub_id, tx.clone()).await;
                            let _ = tx.try_send(Message::Text(format!("bound {sub_id}").into()));
                            debug!("websocket bound to subscription {sub_id}");
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // ignore binary/ping/pong frames
                    Some(Err(_)) => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ping_no_clients() {
        let reg = WsRegistry::new();
        assert_eq!(reg.ping("sub-1").await, 0);
    }

    #[tokio::test]
    async fn test_bind_then_ping_delivers() {
        let reg = WsRegistry::new();
        let (tx, mut rx) = mpsc::channel(QUEUE);
        reg.bind("sub-1", tx).await;

        assert_eq!(reg.ping("sub-1").await, 1);
        let msg = rx.recv().await.unwrap();
        match msg {
            Message::Text(t) => assert_eq!(t.as_str(), "ping sub-1"),
            _ => panic!("expected text ping"),
        }
        // A different subscription id reaches no one.
        assert_eq!(reg.ping("sub-2").await, 0);
    }

    #[tokio::test]
    async fn test_ping_prunes_closed_clients() {
        let reg = WsRegistry::new();
        let (tx, rx) = mpsc::channel(QUEUE);
        reg.bind("sub-1", tx).await;
        drop(rx); // client disconnected

        assert_eq!(reg.ping("sub-1").await, 0, "closed client pruned");
        // Entry removed, so a second ping still reports zero.
        assert_eq!(reg.ping("sub-1").await, 0);
    }

    #[tokio::test]
    async fn a_client_that_reads_nothing_holds_a_bounded_queue() {
        let reg = WsRegistry::new();
        let (tx, mut rx) = mpsc::channel(QUEUE);
        reg.bind("sub-1", tx).await;
        for _ in 0..QUEUE * 3 {
            // Still bound: a full queue skips the ping, it doesn't drop the client.
            assert_eq!(reg.ping("sub-1").await, 1);
        }
        let mut queued = 0;
        while rx.try_recv().is_ok() {
            queued += 1;
        }
        assert_eq!(queued, QUEUE);
    }
}
