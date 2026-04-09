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
    ws.on_upgrade(move |socket| handle_socket(socket, pubkey, state.ws))
}

async fn handle_socket(socket: WebSocket, pubkey: String, registry: WsRegistry) {
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
                    let reg = reg_clone.lock().await;
                    if let Some(tx) = reg.get(&envelope.to) {
                        let _ = tx.send(out);
                    }
                    if let Some(tx) = reg.get(&pk) {
                        let confirm = serde_json::json!({"type":"delivered","to":envelope.to});
                        let _ = tx.send(confirm.to_string());
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

pub async fn online_drivers(
    State(state): State<crate::AppState>,
) -> axum::Json<Vec<String>> {
    let reg = state.ws.lock().await;
    let keys: Vec<String> = reg.keys().cloned().collect();
    axum::Json(keys)
}
