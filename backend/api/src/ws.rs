use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use futures::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::state::AppState;

#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum WsRequest {
    #[serde(rename = "subscribe")]
    Subscribe { channel: String },
    #[serde(rename = "unsubscribe")]
    Unsubscribe { channel: String },
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Serialize)]
#[serde(tag = "type")]
pub enum WsResponse {
    #[serde(rename = "subscribed")]
    Subscribed { channel: String },
    #[serde(rename = "unsubscribed")]
    Unsubscribed { channel: String },
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "price")]
    Price { price: u64, timestamp: i64 },
    #[serde(rename = "orderbook")]
    OrderBook {
        bids: Vec<(u64, u64)>,
        asks: Vec<(u64, u64)>,
        timestamp: i64,
    },
    #[serde(rename = "trade")]
    Trade {
        side: String,
        price: u64,
        size: u64,
        timestamp: i64,
    },
    #[serde(rename = "position")]
    PositionUpdate {
        address: String,
        unrealized_pnl: i64,
        margin_ratio: u32,
    },
    #[serde(rename = "error")]
    Error { message: String },
}

pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();

    // Handle incoming messages
    while let Some(msg) = receiver.next().await {
        if let Ok(msg) = msg {
            match msg {
                Message::Text(text) => {
                    if let Ok(request) = serde_json::from_str::<WsRequest>(&text) {
                        let response = handle_request(request, &state).await;
                        if let Ok(json) = serde_json::to_string(&response) {
                            if sender.send(Message::Text(json)).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    }
}

async fn handle_request(request: WsRequest, _state: &AppState) -> WsResponse {
    match request {
        WsRequest::Subscribe { channel } => {
            // TODO: Add to subscription list
            WsResponse::Subscribed { channel }
        }
        WsRequest::Unsubscribe { channel } => {
            // TODO: Remove from subscription list
            WsResponse::Unsubscribed { channel }
        }
        WsRequest::Ping => WsResponse::Pong,
    }
}
