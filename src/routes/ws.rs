use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::Response,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{broadcast, Mutex};
use futures::{SinkExt, StreamExt};

pub type WsRegistry = Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>;

pub fn new_registry() -> WsRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Deserialize)]
pub struct WsQuery {
    pub pubkey: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RideMessage {
    pub to: String,
    pub from: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub payload: serde_json::Value,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsQuery>,
    State(state): State<crate::AppState>,
) -> Response {
    let pubkey = params.pubkey.clone();
    ws.on_upgrade(move |socket| handle_socket(socket, pubkey, state))
}

async fn handle_socket(socket: WebSocket, pubkey: String, state: crate::AppState) {
    let registry = state.ws.clone();
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = broadcast::channel::<String>(64);
    {
        let mut reg = registry.lock().await;
        reg.insert(pubkey.clone(), tx);
    }
    tracing::info!("[ws] {} connected", &pubkey[..8.min(pubkey.len())]);

    // Server-side keepalive: ping client every 30s to prevent Railway proxy timeout
    let ping_tx = {
        let reg = registry.lock().await;
        reg.get(&pubkey).cloned()
    };
    if let Some(tx) = ping_tx {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                if tx.send(r#"{"type":"ping"}"#.to_string()).is_err() { break; }
            }
        });
    }

    let send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            if sender.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    let reg_clone = registry.clone();
    let state_clone = state.clone();
    let pk = pubkey.clone();
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            if let Message::Text(text) = msg {
                // Handle client keepalive pings
                if text.contains("\"ping\"") {
                    let reg = reg_clone.lock().await;
                    if let Some(tx) = reg.get(&pk) {
                        let _ = tx.send(r#"{"type":"pong"}"#.to_string());
                    }
                    continue;
                }
                if let Ok(mut envelope) = serde_json::from_str::<RideMessage>(&text) {
                    envelope.from = pk.clone();
                    let out = serde_json::to_string(&envelope).unwrap_or_default();
                    let is_call_offer = envelope.msg_type == "ulendo-call-offer";
                    let to_pubkey = envelope.to.clone();
                    let caller_name = envelope.payload
                        .get("callerName").and_then(|v| v.as_str())
                        .unwrap_or("Someone").to_string();
                    {
                        let reg = reg_clone.lock().await;
                        if let Some(tx) = reg.get(&envelope.to) {
                            let _ = tx.send(out);
                        } else if is_call_offer {
                            // Recipient offline — send Web Push
                            let state_clone = state_clone.clone();
                            let caller = caller_name.clone();
                            let to = to_pubkey.clone();
                            tokio::spawn(async move {
                                send_call_push(&state_clone, &to, &caller).await;
                            });
                        }
                        if let Some(tx) = reg.get(&pk) {
                            let confirm = serde_json::json!({"type":"delivered","to":envelope.to});
                            let _ = tx.send(confirm.to_string());
                        }
                    }
                }
            } else if let Message::Close(_) = msg {
                break;
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }

    registry.lock().await.remove(&pubkey);
    tracing::info!("[ws] {} disconnected", &pubkey[..8.min(pubkey.len())]);
}

async fn send_call_push(state: &crate::AppState, to_pubkey: &str, caller_name: &str) {
    // Query push subscriptions via identities table (hex pubkey → npub → push subs)
    let subs = sqlx::query_as::<_, crate::db::PushSubscription>(
        "SELECT ps.* FROM push_subscriptions ps
         JOIN identities i ON i.npub = ps.npub
         WHERE i.public_key = ?1"
    )
    .bind(to_pubkey)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    if subs.is_empty() {
        tracing::debug!("[push] no subscriptions for pubkey {}", &to_pubkey[..8.min(to_pubkey.len())]);
        return;
    }

    let payload = serde_json::json!({
        "title": "📞 Incoming Ulendo Call",
        "body":  format!("{} is calling you", caller_name),
        "icon":  "/logo-icon.svg",
        "badge": "/logo-icon.svg",
        "tag":   "ulendo-call",
        "requireInteraction": true,
        "data":  { "type": "incoming_call", "from": to_pubkey }
    });

    for sub in &subs {
        match state.push.send(sub, payload.to_string()).await {
            Ok(_)  => tracing::info!("[push] call notification sent to {}", &to_pubkey[..8.min(to_pubkey.len())]),
            Err(e) => tracing::warn!("[push] failed to send call notification: {e}"),
        }
    }
}

pub async fn online_drivers(
    State(state): State<crate::AppState>,
) -> axum::Json<Vec<String>> {
    let reg = state.ws.lock().await;
    let keys: Vec<String> = reg.keys().cloned().collect();
    axum::Json(keys)
}
