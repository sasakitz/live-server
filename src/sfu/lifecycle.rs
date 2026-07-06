//! 配信の開始/終了に伴うDB更新・WebSocket通知・Webhook送信をまとめた関数群。
//! WHIPハンドラの接続状態変化コールバックと、管理API(強制停止)の両方から呼ばれる。

use time::OffsetDateTime;

use crate::error::AppResult;
use crate::models::Stream;
use crate::state::{AppEvent, AppState};
use crate::webhook::{self, WebhookEventKind};

async fn fetch_stream(state: &AppState, stream_id: &str) -> AppResult<Option<Stream>> {
    let stream = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE id = ?")
        .bind(stream_id)
        .fetch_optional(&state.db)
        .await?;
    Ok(stream)
}

/// WHIPのPeerConnectionが実際にConnected状態になったタイミングで呼ぶ。
/// 既にlive状態なら何もしない(状態変化コールバックの重複発火に対する防御)。
pub async fn mark_stream_started(state: &AppState, stream_id: &str) -> AppResult<()> {
    let now = OffsetDateTime::now_utc();
    let result = sqlx::query(
        "UPDATE streams SET is_live = 1, started_at = ?, peak_viewers = 0 \
         WHERE id = ? AND is_live = 0",
    )
    .bind(now)
    .bind(stream_id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO stream_events (stream_id, kind, at, peak_viewers) VALUES (?, 'start', ?, 0)",
    )
    .bind(stream_id)
    .bind(now)
    .execute(&state.db)
    .await?;

    state.publish(AppEvent::StreamOnline {
        stream_id: stream_id.to_owned(),
    });

    if let Some(stream) = fetch_stream(state, stream_id).await? {
        webhook::notify(state, WebhookEventKind::Start, &stream).await;
    }

    tracing::info!(stream_id, "配信を開始しました");
    Ok(())
}

/// 配信を停止する。Room(視聴者/publisher PeerConnection含む)を破棄し、
/// DBのis_live/peak_viewersを確定させ、WS通知とWebhookを送る。
/// WHIP切断コールバック・管理APIの強制停止のどちらから呼んでも安全(冪等)。
///
/// `close_publisher_pc` は publisher 側の `RTCPeerConnection::close()` を
/// このタイミングで呼ぶかどうかを制御する。webrtc-rsは
/// `on_peer_connection_state_change` のコールバックを、そのハンドラ自身を
/// 保持するMutexをロックした状態で実行するため、**その状態変化コールバック
/// (自然切断検知)からこの関数を呼ぶ場合は必ず `false` を渡すこと**。
/// `close()` を再度呼ぶと同じMutexの再ロックとなりデッドロックする
/// (PeerConnectionは既に閉じかけているため、そもそも呼ぶ必要がない)。
/// 管理APIの強制停止やDELETEなど、コールバック外から能動的に止める場合は
/// `true` を渡してPeerConnectionを実際に閉じる。
pub async fn stop_stream(state: &AppState, stream_id: &str, close_publisher_pc: bool) -> AppResult<()> {
    let room = {
        let mut rooms = state.rooms.write().await;
        rooms.remove(stream_id)
    };

    let peak_viewers: i64 = if let Some(room) = &room {
        if close_publisher_pc {
            if let Some(pc) = room.publisher_pc.write().await.take() {
                let _ = pc.close().await;
            }
        }
        room.close_all_viewers().await;
        room.peak_viewers() as i64
    } else {
        0
    };

    let now = OffsetDateTime::now_utc();
    let result = sqlx::query(
        "UPDATE streams SET is_live = 0, started_at = NULL, peak_viewers = ? \
         WHERE id = ? AND is_live = 1",
    )
    .bind(peak_viewers)
    .bind(stream_id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO stream_events (stream_id, kind, at, peak_viewers) VALUES (?, 'stop', ?, ?)",
    )
    .bind(stream_id)
    .bind(now)
    .bind(peak_viewers)
    .execute(&state.db)
    .await?;

    state.publish(AppEvent::StreamOffline {
        stream_id: stream_id.to_owned(),
    });

    if let Some(stream) = fetch_stream(state, stream_id).await? {
        webhook::notify(state, WebhookEventKind::Stop, &stream).await;
    }

    tracing::info!(stream_id, peak_viewers, "配信を終了しました");
    Ok(())
}
