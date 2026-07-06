//! ライブ状態/視聴者数のリアルタイム配信用WebSocket。
//! 一覧ページ・視聴ページ・ダッシュボードが購読し、AppEventをそのままJSONで受け取る。

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use tokio::sync::broadcast::error::RecvError;

use crate::state::AppState;

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.events.subscribe();
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(event) => {
                        let Ok(json) = serde_json::to_string(&event) else { continue };
                        if socket.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    None => break,
                    Some(Err(_)) => break,
                    Some(Ok(Message::Close(_))) => break,
                    // pingやテキストメッセージなど、クライアントからの送信は現状使わないため無視する
                    // (Ping/Pongへの自動応答はaxumのWebSocket実装が行う)。
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(ws_handler))
}
