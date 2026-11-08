//! ライブ状態/視聴者数のリアルタイム配信用WebSocket。
//! 一覧ページ・視聴ページ・ダッシュボードが購読し、AppEventをそのままJSONで受け取る。

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use tokio::sync::broadcast::error::RecvError;

use crate::state::{AppEvent, AppState};
use crate::auth::CurrentUser;
use crate::error::AppResult;

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>, user: Option<CurrentUser>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state, user.map(|u| u.0.id)))
}

async fn handle_socket(mut socket: WebSocket, state: AppState, user_id: Option<i64>) {
    let mut rx = state.events.subscribe();
    if state.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
        let _ = send(&mut socket, Message::Close(None)).await;
        return;
    }
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(event) => {
                        if matches!(event, AppEvent::Shutdown) {
                            let _ = send(&mut socket, Message::Close(None)).await;
                            break;
                        }
                        if !can_view_event(&state, &event, user_id).await.unwrap_or(false) { continue; }
                        let Ok(json) = serde_json::to_string(&event) else { continue };
                        if !send(&mut socket, Message::Text(json.into())).await {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        if !send(&mut socket, Message::Text("{\"type\":\"resync\"}".into())).await { break; }
                    },
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

// 読み取らないクライアントの送信待ちで、配信通知やサーバ終了を永久に待たせない。
async fn send(socket: &mut WebSocket, message: Message) -> bool {
    matches!(tokio::time::timeout(std::time::Duration::from_secs(5), socket.send(message)).await, Ok(Ok(())))
}

/// メタ情報APIと同じ認可を適用し、無効化されたユーザの既存WSにも反映する。
async fn can_view_event(state: &AppState, event: &AppEvent, user_id: Option<i64>) -> AppResult<bool> {
    let (stream_id, was_public) = match event {
        AppEvent::Shutdown => return Ok(false),
        AppEvent::StreamOnline { stream_id } | AppEvent::ViewerCount { stream_id, .. } => (stream_id, false),
        AppEvent::StreamOffline { stream_id, was_public } => (stream_id, *was_public),
    };
    let public: Option<bool> = sqlx::query_scalar("SELECT public FROM streams WHERE id = ?")
        .bind(stream_id).fetch_optional(&state.db).await?;
    if public.unwrap_or(was_public) || state.config.auth.public_viewing { return Ok(true); }
    let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = ? AND disabled = 0)")
        .bind(user_id).fetch_one(&state.db).await?;
    Ok(active)
}

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(ws_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn private_events_are_hidden_from_anonymous_and_disabled_users() {
        let state = crate::state::test_state().await;
        sqlx::query("UPDATE streams SET public = 0").execute(&state.db).await.unwrap();
        let event = AppEvent::ViewerCount { stream_id: "test".into(), count: 3 };
        assert!(!can_view_event(&state, &event, None).await.unwrap());
        assert!(can_view_event(&state, &event, Some(2)).await.unwrap());
        sqlx::query("UPDATE users SET disabled = 1 WHERE id = 2").execute(&state.db).await.unwrap();
        assert!(!can_view_event(&state, &event, Some(2)).await.unwrap());
        sqlx::query("DELETE FROM streams").execute(&state.db).await.unwrap();
        let event = AppEvent::StreamOffline { stream_id: "test".into(), was_public: true };
        assert!(can_view_event(&state, &event, None).await.unwrap());
    }
}
