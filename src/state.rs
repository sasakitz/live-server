use std::collections::HashMap;
use std::sync::Arc;

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
    StreamOnline { stream_id: StreamId },
    StreamOffline { stream_id: StreamId },
    ViewerCount { stream_id: StreamId, count: usize },
}

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
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
