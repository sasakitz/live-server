//! 管理者専用API: ユーザ管理・配信管理(全体)・Webhook管理・サーバ状況。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::auth::{hash_password, AdminUser};
use crate::error::{AppError, AppResult};
use crate::models::{generate_stream_key, User, Webhook, ROLE_ADMIN, ROLE_BROADCASTER};
use crate::sfu::lifecycle;
use crate::state::AppState;
use crate::webhook as webhook_client;

pub(crate) fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db_err) if db_err.is_unique_violation())
}

fn valid_role(role: &str) -> bool {
    role == ROLE_ADMIN || role == ROLE_BROADCASTER
}

// ==================== users ====================

#[derive(Debug, Serialize)]
struct UserOut {
    id: i64,
    username: String,
    role: String,
    disabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

impl From<User> for UserOut {
    fn from(u: User) -> Self {
        UserOut {
            id: u.id,
            username: u.username,
            role: u.role,
            disabled: u.disabled,
            created_at: u.created_at,
        }
    }
}

async fn list_users(State(state): State<AppState>, _admin: AdminUser) -> AppResult<Json<Vec<UserOut>>> {
    let users = sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY created_at ASC")
        .fetch_all(&state.db)
        .await?;
    Ok(Json(users.into_iter().map(UserOut::from).collect()))
}

#[derive(Debug, Deserialize)]
struct CreateUser {
    username: String,
    password: String,
    role: String,
}

async fn create_user(
    State(state): State<AppState>,
    _admin: AdminUser,
    Json(body): Json<CreateUser>,
) -> AppResult<(StatusCode, Json<UserOut>)> {
    if body.username.trim().is_empty() {
        return Err(AppError::bad_request("ユーザ名は空にできません"));
    }
    if body.password.len() < 8 {
        return Err(AppError::bad_request("パスワードは8文字以上にしてください"));
    }
    if !valid_role(&body.role) {
        return Err(AppError::bad_request("roleはadminかbroadcasterのいずれかです"));
    }

    let hash = hash_password(&body.password)?;
    let now = OffsetDateTime::now_utc();
    let result = sqlx::query(
        "INSERT INTO users (username, password_hash, role, disabled, created_at) VALUES (?, ?, ?, 0, ?)",
    )
    .bind(&body.username)
    .bind(&hash)
    .bind(&body.role)
    .bind(now)
    .execute(&state.db)
    .await;

    if let Err(e) = &result {
        if is_unique_violation(e) {
            return Err(AppError::conflict("そのユーザ名は既に使用されています"));
        }
    }
    result?;

    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&body.username)
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(UserOut::from(user))))
}

#[derive(Debug, Deserialize)]
struct UpdateUser {
    role: Option<String>,
    disabled: Option<bool>,
    password: Option<String>,
}

async fn update_user(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(user_id): Path<i64>,
    Json(body): Json<UpdateUser>,
) -> AppResult<Json<UserOut>> {
    let target = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::not_found("ユーザが見つかりません"))?;

    let is_self = target.id == admin.id;

    let role = match &body.role {
        Some(r) => {
            if !valid_role(r) {
                return Err(AppError::bad_request("roleはadminかbroadcasterのいずれかです"));
            }
            if is_self && r != ROLE_ADMIN {
                return Err(AppError::bad_request("自分自身の管理者権限は外せません"));
            }
            r.clone()
        }
        None => target.role.clone(),
    };
    let disabled = match body.disabled {
        Some(true) if is_self => {
            return Err(AppError::bad_request("自分自身を無効化することはできません"));
        }
        Some(d) => d,
        None => target.disabled,
    };

    if let Some(password) = &body.password {
        if password.len() < 8 {
            return Err(AppError::bad_request("パスワードは8文字以上にしてください"));
        }
        let hash = hash_password(password)?;
        sqlx::query("UPDATE users SET role = ?, disabled = ?, password_hash = ? WHERE id = ?")
            .bind(&role)
            .bind(disabled)
            .bind(&hash)
            .bind(user_id)
            .execute(&state.db)
            .await?;
    } else {
        sqlx::query("UPDATE users SET role = ?, disabled = ? WHERE id = ?")
            .bind(&role)
            .bind(disabled)
            .bind(user_id)
            .execute(&state.db)
            .await?;
    }

    let updated = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(UserOut::from(updated)))
}

async fn delete_user(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(user_id): Path<i64>,
) -> AppResult<StatusCode> {
    if user_id == admin.id {
        return Err(AppError::bad_request("自分自身のアカウントは削除できません"));
    }
    // 所有する配信がライブ中なら先に止める(FK CASCADEで配信自体は削除される)。
    let owned_live: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM streams WHERE owner_user_id = ? AND is_live = 1")
            .bind(user_id)
            .fetch_all(&state.db)
            .await?;
    for (stream_id,) in owned_live {
        lifecycle::stop_stream(&state, &stream_id, true).await?;
    }

    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(user_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ==================== streams ====================

#[derive(Debug, sqlx::FromRow)]
struct AdminStreamRow {
    id: String,
    owner_user_id: i64,
    owner_username: String,
    title: String,
    description: String,
    stream_key: String,
    is_live: bool,
    public: bool,
    started_at: Option<OffsetDateTime>,
    peak_viewers: i64,
    created_at: OffsetDateTime,
}

#[derive(Debug, Serialize)]
struct AdminStreamOut {
    id: String,
    owner_user_id: i64,
    owner_username: String,
    title: String,
    description: String,
    stream_key: String,
    is_live: bool,
    public: bool,
    viewer_count: usize,
    #[serde(with = "time::serde::rfc3339::option")]
    started_at: Option<OffsetDateTime>,
    peak_viewers: i64,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

async fn to_admin_out(state: &AppState, row: AdminStreamRow) -> AdminStreamOut {
    let viewer_count = if row.is_live {
        state
            .rooms
            .read()
            .await
            .get(&row.id)
            .map(|r| r.viewer_count())
            .unwrap_or(0)
    } else {
        0
    };
    AdminStreamOut {
        id: row.id,
        owner_user_id: row.owner_user_id,
        owner_username: row.owner_username,
        title: row.title,
        description: row.description,
        stream_key: row.stream_key,
        is_live: row.is_live,
        public: row.public,
        viewer_count,
        started_at: row.started_at,
        peak_viewers: row.peak_viewers,
        created_at: row.created_at,
    }
}

const ADMIN_STREAM_SELECT: &str =
    "SELECT streams.*, users.username AS owner_username FROM streams \
     JOIN users ON users.id = streams.owner_user_id";

async fn list_streams(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> AppResult<Json<Vec<AdminStreamOut>>> {
    let query = format!("{ADMIN_STREAM_SELECT} ORDER BY streams.created_at ASC");
    let rows = sqlx::query_as::<_, AdminStreamRow>(sqlx::AssertSqlSafe(query))
        .fetch_all(&state.db)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(to_admin_out(&state, row).await);
    }
    Ok(Json(out))
}

pub(crate) fn validate_slug(id: &str) -> AppResult<()> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(AppError::bad_request(
            "配信IDは小文字英数字とハイフンのみ、1〜64文字で指定してください",
        ))
    }
}

#[derive(Debug, Deserialize)]
struct CreateStream {
    id: String,
    owner_user_id: i64,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    public: bool,
}

async fn create_stream(
    State(state): State<AppState>,
    _admin: AdminUser,
    Json(body): Json<CreateStream>,
) -> AppResult<(StatusCode, Json<AdminStreamOut>)> {
    validate_slug(&body.id)?;
    if body.title.trim().is_empty() {
        return Err(AppError::bad_request("タイトルは空にできません"));
    }
    let owner_exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM users WHERE id = ?")
        .bind(body.owner_user_id)
        .fetch_optional(&state.db)
        .await?;
    if owner_exists.is_none() {
        return Err(AppError::bad_request("指定されたowner_user_idのユーザが存在しません"));
    }

    let stream_key = generate_stream_key();
    let now = OffsetDateTime::now_utc();
    let result = sqlx::query(
        "INSERT INTO streams \
         (id, owner_user_id, title, description, stream_key, is_live, public, peak_viewers, created_at) \
         VALUES (?, ?, ?, ?, ?, 0, ?, 0, ?)",
    )
    .bind(&body.id)
    .bind(body.owner_user_id)
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

    let query = format!("{ADMIN_STREAM_SELECT} WHERE streams.id = ?");
    let row = sqlx::query_as::<_, AdminStreamRow>(sqlx::AssertSqlSafe(query))
        .bind(&body.id)
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(to_admin_out(&state, row).await)))
}

#[derive(Debug, Deserialize)]
struct UpdateStream {
    title: Option<String>,
    description: Option<String>,
    public: Option<bool>,
    owner_user_id: Option<i64>,
}

async fn update_stream(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(stream_id): Path<String>,
    Json(body): Json<UpdateStream>,
) -> AppResult<Json<AdminStreamOut>> {
    let query = format!("{ADMIN_STREAM_SELECT} WHERE streams.id = ?");
    let current = sqlx::query_as::<_, AdminStreamRow>(sqlx::AssertSqlSafe(query))
        .bind(&stream_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::not_found("配信が見つかりません"))?;

    if let Some(owner_id) = body.owner_user_id {
        let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM users WHERE id = ?")
            .bind(owner_id)
            .fetch_optional(&state.db)
            .await?;
        if exists.is_none() {
            return Err(AppError::bad_request("指定されたowner_user_idのユーザが存在しません"));
        }
    }

    let title = body.title.unwrap_or(current.title);
    let description = body.description.unwrap_or(current.description);
    let public = body.public.unwrap_or(current.public);
    let owner_user_id = body.owner_user_id.unwrap_or(current.owner_user_id);

    if title.trim().is_empty() {
        return Err(AppError::bad_request("タイトルは空にできません"));
    }

    sqlx::query("UPDATE streams SET title = ?, description = ?, public = ?, owner_user_id = ? WHERE id = ?")
        .bind(&title)
        .bind(&description)
        .bind(public)
        .bind(owner_user_id)
        .bind(&stream_id)
        .execute(&state.db)
        .await?;

    let query = format!("{ADMIN_STREAM_SELECT} WHERE streams.id = ?");
    let row = sqlx::query_as::<_, AdminStreamRow>(sqlx::AssertSqlSafe(query))
        .bind(&stream_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(to_admin_out(&state, row).await))
}

async fn delete_stream(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(stream_id): Path<String>,
) -> AppResult<StatusCode> {
    lifecycle::stop_stream(&state, &stream_id, true).await?;
    sqlx::query("DELETE FROM streams WHERE id = ?")
        .bind(&stream_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn regenerate_key(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(stream_id): Path<String>,
) -> AppResult<Json<AdminStreamOut>> {
    let new_key = generate_stream_key();
    let result = sqlx::query("UPDATE streams SET stream_key = ? WHERE id = ?")
        .bind(&new_key)
        .bind(&stream_id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::not_found("配信が見つかりません"));
    }

    let query = format!("{ADMIN_STREAM_SELECT} WHERE streams.id = ?");
    let row = sqlx::query_as::<_, AdminStreamRow>(sqlx::AssertSqlSafe(query))
        .bind(&stream_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(to_admin_out(&state, row).await))
}

async fn stop_stream_handler(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(stream_id): Path<String>,
) -> AppResult<StatusCode> {
    lifecycle::stop_stream(&state, &stream_id, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ==================== webhooks ====================

#[derive(Debug, Deserialize)]
struct UpsertWebhook {
    name: String,
    url: String,
    #[serde(default = "default_true")]
    notify_start: bool,
    #[serde(default = "default_true")]
    notify_stop: bool,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

async fn list_webhooks(State(state): State<AppState>, _admin: AdminUser) -> AppResult<Json<Vec<Webhook>>> {
    let webhooks = sqlx::query_as::<_, Webhook>("SELECT * FROM webhooks ORDER BY created_at ASC")
        .fetch_all(&state.db)
        .await?;
    Ok(Json(webhooks))
}

async fn create_webhook(
    State(state): State<AppState>,
    _admin: AdminUser,
    Json(body): Json<UpsertWebhook>,
) -> AppResult<(StatusCode, Json<Webhook>)> {
    if body.name.trim().is_empty() || body.url.trim().is_empty() {
        return Err(AppError::bad_request("nameとurlは必須です"));
    }
    let now = OffsetDateTime::now_utc();
    let result = sqlx::query(
        "INSERT INTO webhooks (name, url, notify_start, notify_stop, enabled, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&body.name)
    .bind(&body.url)
    .bind(body.notify_start)
    .bind(body.notify_stop)
    .bind(body.enabled)
    .bind(now)
    .execute(&state.db)
    .await?;

    let webhook = sqlx::query_as::<_, Webhook>("SELECT * FROM webhooks WHERE id = ?")
        .bind(result.last_insert_rowid())
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(webhook)))
}

async fn update_webhook(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(webhook_id): Path<i64>,
    Json(body): Json<UpsertWebhook>,
) -> AppResult<Json<Webhook>> {
    if body.name.trim().is_empty() || body.url.trim().is_empty() {
        return Err(AppError::bad_request("nameとurlは必須です"));
    }
    let result = sqlx::query(
        "UPDATE webhooks SET name = ?, url = ?, notify_start = ?, notify_stop = ?, enabled = ? WHERE id = ?",
    )
    .bind(&body.name)
    .bind(&body.url)
    .bind(body.notify_start)
    .bind(body.notify_stop)
    .bind(body.enabled)
    .bind(webhook_id)
    .execute(&state.db)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::not_found("Webhookが見つかりません"));
    }
    let webhook = sqlx::query_as::<_, Webhook>("SELECT * FROM webhooks WHERE id = ?")
        .bind(webhook_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(webhook))
}

async fn delete_webhook(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(webhook_id): Path<i64>,
) -> AppResult<StatusCode> {
    sqlx::query("DELETE FROM webhooks WHERE id = ?")
        .bind(webhook_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct TestWebhook {
    url: String,
}

async fn test_webhook(_admin: AdminUser, State(state): State<AppState>, Json(body): Json<TestWebhook>) -> StatusCode {
    webhook_client::send_test(&state, body.url);
    StatusCode::ACCEPTED
}

// ==================== overview ====================

#[derive(Debug, Serialize)]
struct Overview {
    active_streams: usize,
    total_viewers: usize,
    total_users: i64,
    total_streams: i64,
}

async fn overview(State(state): State<AppState>, _admin: AdminUser) -> AppResult<Json<Overview>> {
    let rooms = state.rooms.read().await;
    let active_streams = rooms.len();
    let total_viewers = rooms.values().map(|r| r.viewer_count()).sum();
    drop(rooms);

    let total_users: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&state.db)
        .await?;
    let total_streams: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM streams")
        .fetch_one(&state.db)
        .await?;

    Ok(Json(Overview {
        active_streams,
        total_viewers,
        total_users: total_users.0,
        total_streams: total_streams.0,
    }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/overview", get(overview))
        .route("/users", get(list_users).post(create_user))
        .route("/users/{id}", patch(update_user).delete(delete_user))
        .route("/streams", get(list_streams).post(create_stream))
        .route("/streams/{id}", patch(update_stream).delete(delete_stream))
        .route("/streams/{id}/regenerate-key", post(regenerate_key))
        .route("/streams/{id}/stop", post(stop_stream_handler))
        .route("/webhooks", get(list_webhooks).post(create_webhook))
        .route(
            "/webhooks/{id}",
            patch(update_webhook).delete(delete_webhook),
        )
        .route("/webhooks/{id}/test", post(test_webhook))
}
