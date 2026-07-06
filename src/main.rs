mod api;
mod assets;
mod auth;
mod config;
mod db;
mod error;
mod models;
mod sfu;
mod state;
mod webhook;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::Router;
use tokio::sync::{broadcast, RwLock};
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tower_sessions::cookie::SameSite;
use tower_sessions::{Expiry, SessionManagerLayer};
use tracing_subscriber::EnvFilter;

use crate::auth::session_store::SqliteSessionStore;
use crate::config::Config;
use crate::state::AppState;

fn config_path_from_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    for i in 0..args.len() {
        if args[i] == "--config" {
            if let Some(value) = args.get(i + 1) {
                return PathBuf::from(value);
            }
        }
        if let Some(value) = args[i].strip_prefix("--config=") {
            return PathBuf::from(value);
        }
    }
    PathBuf::from("config.toml")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    // rustls 0.23はプロセス全体で使うCryptoProviderを自動決定できないため明示的にインストールする。
    // (reqwest/webrtcなど複数の依存クレートがそれぞれ異なるrustls機能を有効化しうるため、
    // "ring"と"aws-lc-rs"の両方がビルドに含まれることがあり、その場合は起動時にpanicする)
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("rustls CryptoProviderのインストールに失敗しました");

    let config_path = config_path_from_args();
    let config = Config::load(&config_path)?;
    let config = Arc::new(config);
    tracing::info!(path = %config_path.display(), "設定を読み込みました");

    let db = db::init_pool(&config.db.url).await?;

    auth::bootstrap_admins(&db, &config.auth.admin_bootstrap).await?;

    let webrtc_api = Arc::new(sfu::engine::build_api(&config.webrtc)?);

    let (events_tx, _) = broadcast::channel(256);

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        http: reqwest::Client::new(),
        webrtc_api,
        rooms: Arc::new(RwLock::new(HashMap::new())),
        events: events_tx,
    };

    let session_key = auth::load_or_generate_session_key(&config.server.data_dir)?;
    let session_store = SqliteSessionStore::new(db.clone());

    // 期限切れセッションの定期掃除。
    {
        let session_store = session_store.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                if let Err(e) = session_store.delete_expired().await {
                    tracing::warn!(error = ?e, "期限切れセッションの削除に失敗しました");
                }
            }
        });
    }

    let session_layer = SessionManagerLayer::new(session_store)
        .with_private(session_key)
        .with_secure(config.server.secure_cookies)
        .with_same_site(SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(time::Duration::days(30)));

    let app = Router::new()
        .nest("/api/whip", sfu::whip::router())
        .nest("/api/whep", sfu::whep::router())
        .nest("/api", api::router())
        .fallback(assets::static_handler)
        .layer(session_layer)
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.server.bind).await?;
    tracing::info!(addr = %config.server.bind, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Ctrl+Cハンドラの登録に失敗しました");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERMハンドラの登録に失敗しました")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("シャットダウンシグナルを受信しました");
}
