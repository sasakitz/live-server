//! 配信セッションとDB状態を同期する。古い接続の通知で新しい配信を操作しない。

use std::sync::Arc;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::models::{Stream, generate_stream_key};
use crate::sfu::room::Room;
use crate::state::{AppEvent, AppState};
use crate::webhook::{self, WebhookEventKind};

/// 登録を単一操作にし、認証後のキー再発行・所有者無効化・削除も再確認する。
pub async fn register_room(state: &AppState, stream_key: &str, room: Arc<Room>) -> AppResult<()> {
    let mut rooms = state.rooms.write().await;
    if state.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(AppError::conflict("サーバは終了処理中です"));
    }
    let public: Option<bool> = sqlx::query_scalar(
        "SELECT s.public FROM streams s JOIN users u ON u.id = s.owner_user_id \
         WHERE s.id = ? AND s.stream_key = ? AND u.disabled = 0",
    )
    .bind(&room.stream_id)
    .bind(stream_key)
    .fetch_optional(&state.db)
    .await?;
    let public = public.ok_or_else(|| AppError::unauthorized("stream_keyまたは配信者アカウントが無効です"))?;
    if rooms.contains_key(&room.stream_id) {
        return Err(AppError::conflict("この配信は既に開始されています"));
    }
    room.set_public(public);
    rooms.insert(room.stream_id.clone(), room);
    Ok(())
}

pub async fn mark_stream_started(state: &AppState, stream_id: &str, session_id: Uuid) -> AppResult<()> {
    // stop/registerとの間でDBとメモリの順序が逆転しないよう、遷移中だけロックする。
    let rooms = state.rooms.read().await;
    if !rooms.get(stream_id).is_some_and(|room| room.session_id == session_id) {
        return Ok(());
    }
    let now = OffsetDateTime::now_utc();
    let mut tx = state.db.begin().await?;
    let changed = sqlx::query(
        "UPDATE streams SET is_live = 1, started_at = ?, peak_viewers = 0 WHERE id = ? AND is_live = 0",
    )
    .bind(now)
    .bind(stream_id)
    .execute(&mut *tx)
    .await?
    .rows_affected() > 0;
    if !changed {
        return Ok(());
    }
    sqlx::query("INSERT INTO stream_events (stream_id, kind, at, peak_viewers) VALUES (?, 'start', ?, 0)")
        .bind(stream_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    let stream = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE id = ?")
        .bind(stream_id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    state.publish(AppEvent::StreamOnline { stream_id: stream_id.to_owned() });
    drop(rooms);
    webhook::notify(state, WebhookEventKind::Start, &stream).await;
    Ok(())
}

pub async fn stop_stream(state: &AppState, stream_id: &str) -> AppResult<()> {
    stop_matching(state, stream_id, None, false).await
}

/// WHIPのコールバックやDELETEは、生成元セッションだけを停止する。
pub async fn stop_session(state: &AppState, stream_id: &str, session_id: Uuid) -> AppResult<()> {
    stop_matching(state, stream_id, Some(session_id), false).await
}

/// 削除も停止と同じロック/トランザクションで処理し、削除途中の再接続を防ぐ。
pub async fn delete_stream(state: &AppState, stream_id: &str) -> AppResult<()> {
    stop_matching(state, stream_id, None, true).await
}

async fn stop_matching(state: &AppState, stream_id: &str, session_id: Option<Uuid>, delete: bool) -> AppResult<()> {
    let mut rooms = state.rooms.write().await;
    let room = rooms.get(stream_id).cloned();
    if let Some(id) = session_id {
        if !room.as_ref().is_some_and(|room| room.session_id == id) { return Ok(()); }
    }
    if room.is_none() && !delete { return Ok(()); }
    let mut tx = state.db.begin().await?;
    let changed = sqlx::query(
        "UPDATE streams SET is_live = 0, started_at = NULL, peak_viewers = ? WHERE id = ? AND is_live = 1",
    )
    .bind(room.as_ref().map(|room| room.peak_viewers() as i64).unwrap_or(0))
    .bind(stream_id)
    .execute(&mut *tx)
    .await?
    .rows_affected() > 0;
    if changed {
        sqlx::query("INSERT INTO stream_events (stream_id, kind, at, peak_viewers) VALUES (?, 'stop', ?, ?)")
            .bind(stream_id)
            .bind(OffsetDateTime::now_utc())
            .bind(room.as_ref().map(|room| room.peak_viewers() as i64).unwrap_or(0))
            .execute(&mut *tx)
            .await?;
    }
    let stream = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE id = ?")
        .bind(stream_id).fetch_optional(&mut *tx).await?;
    if delete {
        sqlx::query("DELETE FROM streams WHERE id = ?").bind(stream_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    rooms.remove(stream_id);
    if changed || room.is_some() {
        state.publish(AppEvent::StreamOffline {
            stream_id: stream_id.to_owned(),
            was_public: stream.as_ref().map(|s| s.public).unwrap_or_else(|| room.as_ref().is_some_and(|r| r.is_public())),
        });
    }
    drop(rooms);

    // PCのcloseはコールバックを呼ぶため、レジストリやpublisher_pcのロック外で行う。
    if let Some(room) = room {
        let pc = room.publisher_pc.write().await.take();
        if let Some(pc) = pc { let _ = pc.close().await; }
        room.close_all_viewers().await;
    }
    if changed {
        if let Some(stream) = stream { webhook::notify(state, WebhookEventKind::Stop, &stream).await; }
    }
    Ok(())
}

/// 管理者/配信者のどちらでも、ネゴシエーション中を含めたキー変更を禁止する。
pub async fn regenerate_key(state: &AppState, stream_id: &str) -> AppResult<()> {
    let rooms = state.rooms.read().await;
    let stream = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE id = ?")
        .bind(stream_id).fetch_optional(&state.db).await?
        .ok_or_else(|| AppError::not_found("配信が見つかりません"))?;
    if rooms.contains_key(stream_id) || stream.is_live {
        return Err(AppError::conflict("配信中はstream_keyを再発行できません。先に配信を停止してください"));
    }
    sqlx::query("UPDATE streams SET stream_key = ? WHERE id = ?")
        .bind(generate_stream_key()).bind(stream_id).execute(&state.db).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deleting_a_live_stream_emits_offline_and_prevents_reconnect() {
        let state = crate::state::test_state().await;
        let room = Arc::new(Room::new("test".into()));
        register_room(&state, "secret", room.clone()).await.unwrap();
        mark_stream_started(&state, "test", room.session_id).await.unwrap();
        let mut events = state.events.subscribe();
        delete_stream(&state, "test").await.unwrap();
        assert!(state.rooms.read().await.is_empty());
        let event = events.recv().await.unwrap();
        assert!(matches!(event, AppEvent::StreamOffline { was_public: true, .. }));
        assert!(serde_json::to_value(event).unwrap().get("was_public").is_none());
        assert!(register_room(&state, "secret", Arc::new(Room::new("test".into()))).await.is_err());
    }

    #[tokio::test]
    async fn shutdown_prevents_new_publisher_registration() {
        let state = crate::state::test_state().await;
        state.shutting_down.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(matches!(register_room(&state, "secret", Arc::new(Room::new("test".into()))).await, Err(AppError::Conflict(_))));
    }

    #[tokio::test]
    async fn concurrent_publishers_cannot_replace_each_other() {
        let state = crate::state::test_state().await;
        let (a, b) = tokio::join!(
            register_room(&state, "secret", Arc::new(Room::new("test".into()))),
            register_room(&state, "secret", Arc::new(Room::new("test".into())))
        );
        assert_ne!(a.is_ok(), b.is_ok());
        assert_eq!(state.rooms.read().await.len(), 1);
    }

    #[tokio::test]
    async fn old_callbacks_cannot_start_or_stop_a_new_session() {
        let state = crate::state::test_state().await;
        let old = Arc::new(Room::new("test".into()));
        register_room(&state, "secret", old.clone()).await.unwrap();
        mark_stream_started(&state, "test", old.session_id).await.unwrap();
        stop_session(&state, "test", old.session_id).await.unwrap();
        let new = Arc::new(Room::new("test".into()));
        register_room(&state, "secret", new.clone()).await.unwrap();
        mark_stream_started(&state, "test", old.session_id).await.unwrap();
        let live: bool = sqlx::query_scalar("SELECT is_live FROM streams WHERE id = 'test'").fetch_one(&state.db).await.unwrap();
        assert!(!live);
        mark_stream_started(&state, "test", new.session_id).await.unwrap();
        stop_session(&state, "test", old.session_id).await.unwrap();
        assert_eq!(state.rooms.read().await["test"].session_id, new.session_id);
        let live: bool = sqlx::query_scalar("SELECT is_live FROM streams WHERE id = 'test'").fetch_one(&state.db).await.unwrap();
        assert!(live);
    }

    #[tokio::test]
    async fn failed_event_insert_rolls_back_live_state() {
        let state = crate::state::test_state().await;
        let room = Arc::new(Room::new("test".into()));
        register_room(&state, "secret", room.clone()).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_event BEFORE INSERT ON stream_events BEGIN SELECT RAISE(ABORT, 'test error'); END")
            .execute(&state.db).await.unwrap();
        assert!(mark_stream_started(&state, "test", room.session_id).await.is_err());
        let live: bool = sqlx::query_scalar("SELECT is_live FROM streams WHERE id = 'test'").fetch_one(&state.db).await.unwrap();
        assert!(!live);
        sqlx::query("DROP TRIGGER reject_event").execute(&state.db).await.unwrap();
        mark_stream_started(&state, "test", room.session_id).await.unwrap();
        sqlx::query("CREATE TRIGGER reject_event BEFORE INSERT ON stream_events BEGIN SELECT RAISE(ABORT, 'test error'); END")
            .execute(&state.db).await.unwrap();
        assert!(stop_stream(&state, "test").await.is_err());
        assert!(state.rooms.read().await.contains_key("test"));
        let live: bool = sqlx::query_scalar("SELECT is_live FROM streams WHERE id = 'test'").fetch_one(&state.db).await.unwrap();
        assert!(live);
    }

    #[tokio::test]
    async fn pending_sessions_block_key_rotation_and_disabled_owners_cannot_publish() {
        let state = crate::state::test_state().await;
        register_room(&state, "secret", Arc::new(Room::new("test".into()))).await.unwrap();
        assert!(matches!(regenerate_key(&state, "test").await, Err(AppError::Conflict(_))));
        stop_stream(&state, "test").await.unwrap();
        regenerate_key(&state, "test").await.unwrap();
        assert!(register_room(&state, "secret", Arc::new(Room::new("test".into()))).await.is_err());
        sqlx::query("UPDATE users SET disabled = 1 WHERE id = 2").execute(&state.db).await.unwrap();
        let key: String = sqlx::query_scalar("SELECT stream_key FROM streams WHERE id = 'test'").fetch_one(&state.db).await.unwrap();
        assert!(register_room(&state, &key, Arc::new(Room::new("test".into()))).await.is_err());
    }
}
