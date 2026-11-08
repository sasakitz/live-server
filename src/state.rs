use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use reqwest::Client;
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::{broadcast, RwLock};

use crate::config::Config;
use crate::sfu::room::Room;

pub type StreamId = String;

/// WSでダッシュボード/一覧/視聴ページへブロードキャストするリアルタイムイベント。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    Shutdown,
    StreamOnline { stream_id: StreamId },
    StreamOffline { stream_id: StreamId, #[serde(skip)] was_public: bool },
    ViewerCount { stream_id: StreamId, count: usize },
}

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub shutting_down: Arc<AtomicBool>,
    pub config: Arc<Config>,
    pub http: Client,
    pub webrtc_api: Arc<webrtc::api::API>,
    pub rooms: Arc<RwLock<HashMap<StreamId, Arc<Room>>>>,
    pub events: broadcast::Sender<AppEvent>,
}

impl AppState {
    pub fn publish(&self, event: AppEvent) {
        // 受信側(WebSocket購読者)が一人もいなければSendErrorになるが無害なので無視する。
        let _ = self.events.send(event);
    }
}

#[cfg(test)]
pub async fn test_state() -> AppState {
    let db = crate::db::init_pool("sqlite::memory:").await.unwrap();
    let now = time::OffsetDateTime::now_utc();
    for (id, name, role) in [(1, "admin", "admin"), (2, "caster", "broadcaster")] {
        sqlx::query("INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, ?, 'unused', ?, ?)")
            .bind(id).bind(name).bind(role).bind(now).execute(&db).await.unwrap();
    }
    sqlx::query("INSERT INTO streams (id, owner_user_id, title, stream_key, public, created_at) VALUES ('test', 2, 'Test', 'secret', 1, ?)")
        .bind(now).execute(&db).await.unwrap();
    let config = toml::from_str(include_str!("../config.example.toml")).unwrap();
    let (events, _) = broadcast::channel(256);
    AppState {
        db, shutting_down: Arc::new(AtomicBool::new(false)), config: Arc::new(config), http: reqwest::Client::new(),
        webrtc_api: Arc::new(crate::sfu::engine::build_api(&toml::from_str::<Config>(include_str!("../config.example.toml")).unwrap().webrtc).unwrap()),
        rooms: Arc::new(RwLock::new(HashMap::new())), events,
    }
}
