pub mod admin;
pub mod mine;
pub mod streams;
pub mod ws;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize)]
struct ClientConfig {
    stun_servers: Vec<String>,
}

/// ブラウザがRTCPeerConnection構築に使う公開設定(認証不要)。
async fn client_config(State(state): State<AppState>) -> Json<ClientConfig> {
    Json(ClientConfig {
        stun_servers: state.config.webrtc.stun_servers.clone(),
    })
}

/// `/api` 配下(WHIP/WHEPを除く)のJSON APIをまとめる。ログイン/ログアウト/自分の情報は
/// authモジュール側のルータをそのままmergeする。
pub fn router() -> Router<AppState> {
    Router::new()
        .merge(crate::auth::router())
        .route("/client-config", get(client_config))
        .nest("/streams", streams::router())
        .nest("/my", mine::router())
        .nest("/admin", admin::router())
        .nest("/ws", ws::router())
}
