//! 配信開始/終了をDiscord(互換)Webhookへ通知する。
//!
//! ペイロードはDiscordのIncoming Webhook embed形式で送る。他サービスのURLを
//! 登録した場合も生JSONは届くが、embed形式に特化してレンダリングされるのは
//! Discordのみ(v1のスコープ)。

use serde_json::{json, Value};

use crate::models::Stream;
use crate::state::AppState;

#[derive(Clone, Copy, Debug)]
pub enum WebhookEventKind {
    Start,
    Stop,
}

fn build_payload(kind: WebhookEventKind, stream: &Stream, public_host: &str) -> Value {
    let watch_url = format!("https://{public_host}/watch/{}", stream.id);
    match kind {
        WebhookEventKind::Start => json!({
            "embeds": [{
                "title": "🔴 配信開始",
                "description": format!("**{}**\n{}", stream.title, watch_url),
                "color": 0x22c55e,
            }]
        }),
        WebhookEventKind::Stop => json!({
            "embeds": [{
                "title": "⚫ 配信終了",
                "description": format!("**{}**\nピーク視聴者数: {}人", stream.title, stream.peak_viewers),
                "color": 0x6b7280,
            }]
        }),
    }
}

/// 登録済みWebhook(有効かつ該当イベント購読)+ 設定ファイルの既定URLへ非同期送信する。
/// 送信失敗しても配信自体には影響させない(spawnしてログ警告のみ)。
pub async fn notify(state: &AppState, kind: WebhookEventKind, stream: &Stream) {
    let query = match kind {
        WebhookEventKind::Start => "SELECT * FROM webhooks WHERE enabled = 1 AND notify_start = 1",
        WebhookEventKind::Stop => "SELECT * FROM webhooks WHERE enabled = 1 AND notify_stop = 1",
    };
    let webhooks: Vec<crate::models::Webhook> = sqlx::query_as(query)
        .fetch_all(&state.db)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = ?e, "webhook一覧の取得に失敗しました");
            Vec::new()
        });

    let mut targets: Vec<String> = webhooks.into_iter().map(|w| w.url).collect();
    if let Some(default_url) = &state.config.discord.default_webhook_url {
        if !targets.contains(default_url) {
            targets.push(default_url.clone());
        }
    }
    if targets.is_empty() {
        return;
    }

    let payload = build_payload(kind, stream, &state.config.server.public_host);

    for url in targets {
        send_one(state, url, payload.clone());
    }
}

/// 管理UIの「テスト送信」用に、登録済みかどうかに関わらず単一URLへ即時送信する。
pub fn send_test(state: &AppState, url: String) {
    let payload = json!({
        "embeds": [{
            "title": "🔔 テスト通知",
            "description": "live-serverからのWebhookテスト送信です。",
            "color": 0x3b82f6,
        }]
    });
    send_one(state, url, payload);
}

fn send_one(state: &AppState, url: String, payload: Value) {
    let http = state.http.clone();
    tokio::spawn(async move {
        match http.post(&url).timeout(std::time::Duration::from_secs(10)).json(&payload).send().await {
            Ok(resp) if !resp.status().is_success() => {
                tracing::warn!(status = %resp.status(), "Webhook送信がエラー応答を返しました");
            }
            Err(e) => {
                tracing::warn!(error = ?e.without_url(), "Webhook送信に失敗しました");
            }
            _ => {}
        }
    });
}
