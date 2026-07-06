//! 配信者自身のセルフサービスAPI(自分の配信のみ操作可能。管理者は誰の配信でも操作可能)。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::api::admin::{is_unique_violation, validate_slug};
use crate::auth::CurrentUser;
use crate::error::{AppError, AppResult};
use crate::models::{generate_stream_key, Stream};
use crate::sfu::lifecycle;
use crate::state::AppState;

#[derive(Debug, Serialize)]
struct MyStream {
    id: String,
    title: String,
    description: String,
    stream_key: String,
    is_live: bool,
    public: bool,
    viewer_count: usize,
    #[serde(with = "time::serde::rfc3339::option")]
    started_at: Option<OffsetDateTime>,
    peak_viewers: i64,
}

async fn to_my_stream(state: &AppState, stream: Stream) -> MyStream {
    let viewer_count = if stream.is_live {
        state
            .rooms
            .read()
            .await
            .get(&stream.id)
            .map(|r| r.viewer_count())
            .unwrap_or(0)
    } else {
        0
    };
    MyStream {
        id: stream.id,
        title: stream.title,
        description: stream.description,
        stream_key: stream.stream_key,
        is_live: stream.is_live,
        public: stream.public,
        viewer_count,
        started_at: stream.started_at,
        peak_viewers: stream.peak_viewers,
    }
}

async fn fetch_owned(state: &AppState, user: &crate::models::User, stream_id: &str) -> AppResult<Stream> {
    let stream = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE id = ?")
        .bind(stream_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::not_found("配信が見つかりません"))?;
    if !user.is_admin() && stream.owner_user_id != user.id {
        return Err(AppError::forbidden("この配信を操作する権限がありません"));
    }
    Ok(stream)
}

/// `GET /api/my/streams` — 自分が所有する配信一覧(stream_key込み)。
async fn list_mine(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> AppResult<Json<Vec<MyStream>>> {
    let streams = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE owner_user_id = ? ORDER BY created_at ASC")
        .bind(user.id)
        .fetch_all(&state.db)
        .await?;
    let mut out = Vec::with_capacity(streams.len());
    for stream in streams {
        out.push(to_my_stream(&state, stream).await);
    }
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
struct CreateMyStream {
    id: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    public: bool,
}

/// `POST /api/my/streams` — 自分が所有する配信を新規作成する。
async fn create_mine(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<CreateMyStream>,
) -> AppResult<(StatusCode, Json<MyStream>)> {
    validate_slug(&body.id)?;
    if body.title.trim().is_empty() {
        return Err(AppError::bad_request("タイトルは空にできません"));
    }

    let stream_key = generate_stream_key();
    let now = OffsetDateTime::now_utc();
    let result = sqlx::query(
        "INSERT INTO streams \
         (id, owner_user_id, title, description, stream_key, is_live, public, peak_viewers, created_at) \
         VALUES (?, ?, ?, ?, ?, 0, ?, 0, ?)",
    )
    .bind(&body.id)
    .bind(user.id)
    .bind(&body.title)
    .bind(&body.description)
    .bind(&stream_key)
    .bind(body.public)
    .bind(now)
    .execute(&state.db)
    .await;

    if let Err(e) = &result {
        if is_unique_violation(e) {
            return Err(AppError::conflict("その配信IDは既に使用されています"));
        }
    }
    result?;

    let created = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE id = ?")
        .bind(&body.id)
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(to_my_stream(&state, created).await)))
}

#[derive(Debug, Deserialize)]
struct UpdateMyStream {
    title: Option<String>,
    description: Option<String>,
    public: Option<bool>,
}

/// `PATCH /api/my/streams/{id}` — タイトル/説明/公開可否の変更。
async fn update_mine(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(stream_id): Path<String>,
    Json(body): Json<UpdateMyStream>,
) -> AppResult<Json<MyStream>> {
    let stream = fetch_owned(&state, &user, &stream_id).await?;

    let title = body.title.unwrap_or(stream.title);
    let description = body.description.unwrap_or(stream.description);
    let public = body.public.unwrap_or(stream.public);

    if title.trim().is_empty() {
        return Err(AppError::bad_request("タイトルは空にできません"));
    }

    sqlx::query("UPDATE streams SET title = ?, description = ?, public = ? WHERE id = ?")
        .bind(&title)
        .bind(&description)
        .bind(public)
        .bind(&stream_id)
        .execute(&state.db)
        .await?;

    let updated = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE id = ?")
        .bind(&stream_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(to_my_stream(&state, updated).await))
}

/// `DELETE /api/my/streams/{id}` — 自分が所有する配信を削除する。配信中なら先に停止する。
async fn delete_mine(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(stream_id): Path<String>,
) -> AppResult<StatusCode> {
    fetch_owned(&state, &user, &stream_id).await?;
    lifecycle::stop_stream(&state, &stream_id, true).await?;
    sqlx::query("DELETE FROM streams WHERE id = ?")
        .bind(&stream_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/my/streams/{id}/regenerate-key` — stream_keyの再発行(漏洩時などに使用)。
async fn regenerate_key(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(stream_id): Path<String>,
) -> AppResult<Json<MyStream>> {
    let stream = fetch_owned(&state, &user, &stream_id).await?;
    if stream.is_live {
        return Err(AppError::conflict(
            "配信中はstream_keyを再発行できません。先に配信を停止してください",
        ));
    }

    let new_key = generate_stream_key();
    sqlx::query("UPDATE streams SET stream_key = ? WHERE id = ?")
        .bind(&new_key)
        .bind(&stream_id)
        .execute(&state.db)
        .await?;

    let updated = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE id = ?")
        .bind(&stream_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(to_my_stream(&state, updated).await))
}

/// `POST /api/my/streams/{id}/stop` — OBS側が異常終了した場合などの強制停止。
async fn stop_mine(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(stream_id): Path<String>,
) -> AppResult<Json<MyStream>> {
    let stream = fetch_owned(&state, &user, &stream_id).await?;
    lifecycle::stop_stream(&state, &stream.id, true).await?;
    let updated = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE id = ?")
        .bind(&stream_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(to_my_stream(&state, updated).await))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/streams", get(list_mine).post(create_mine))
        .route("/streams/{id}", patch(update_mine).delete(delete_mine))
        .route("/streams/{id}/regenerate-key", post(regenerate_key))
        .route("/streams/{id}/stop", post(stop_mine))
}
