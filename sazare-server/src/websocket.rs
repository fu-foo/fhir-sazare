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
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use crate::AppState;

/// Tracks live WebSocket clients bound to Subscription ids.
#[derive(Default)]
pub struct WsRegistry {
    /// subscription id -> outbound channel for every client bound to it
    bound: Mutex<HashMap<String, Vec<mpsc::UnboundedSender<Message>>>>,
}

impl WsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a client's outbound channel to a subscription id.
    async fn bind(&self, sub_id: &str, tx: mpsc::UnboundedSender<Message>) {
        self.bound
            .lock()
            .await
            .entry(sub_id.to_string())
            .or_default()
            .push(tx);
    }

    /// Send `ping <sub_id>` to every client bound to the subscription, pruning
    /// any whose channel has closed. Returns the number of clients notified.
    pub async fn ping(&self, sub_id: &str) -> usize {
        let mut bound = self.bound.lock().await;
        let Some(senders) = bound.get_mut(sub_id) else {
            return 0;
        };
        let msg = format!("ping {sub_id}");
        senders.retain(|tx| tx.send(Message::Text(msg.clone().into())).is_ok());
        let n = senders.len();
        if n == 0 {
            bound.remove(sub_id);
        }
        n
    }
}

/// `GET /ws` — upgrade to a FHIR R4 subscription websocket.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    // Outbound queue: pings from the registry and our handshake replies are
    // funneled here and written to the socket by the select loop below.
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    loop {
        tokio::select! {
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
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(sub_id) = text.trim().strip_prefix("bind ") {
                            let sub_id = sub_id.trim().to_string();
                            state.ws_registry.bind(&sub_id, tx.clone()).await;
                            let _ = tx.send(Message::Text(format!("bound {sub_id}").into()));
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
        let (tx, mut rx) = mpsc::unbounded_channel();
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
        let (tx, rx) = mpsc::unbounded_channel();
        reg.bind("sub-1", tx).await;
        drop(rx); // client disconnected

        assert_eq!(reg.ping("sub-1").await, 0, "closed client pruned");
        // Entry removed, so a second ping still reports zero.
        assert_eq!(reg.ping("sub-1").await, 0);
    }
}
