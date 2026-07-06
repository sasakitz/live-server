pub mod session_store;

use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use time::OffsetDateTime;
use tower_sessions::cookie::Key;
use tower_sessions::Session;

use crate::config::AdminBootstrap;
use crate::error::{AppError, AppResult};
use crate::models::User;
use crate::state::AppState;

const SESSION_USER_ID_KEY: &str = "user_id";
const SESSION_KEY_FILE: &str = "session_key.bin";

/// タイミング攻撃(ユーザ名の存在有無を応答時間から推測する攻撃)を軽減するため、
/// ユーザが見つからない場合でも同程度のコストのArgon2検証を必ず行う。
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    hash_password("dummy-password-used-only-for-constant-time-login-checks")
        .expect("dummy hash generation must succeed")
});

pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default().hash_password(password.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// `data_dir/session_key.bin` からセッションCookie暗号化鍵を読み込む。無ければ新規生成して保存する。
pub fn load_or_generate_session_key(data_dir: &Path) -> Result<Key> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("データディレクトリの作成に失敗しました: {}", data_dir.display()))?;
    let key_path = data_dir.join(SESSION_KEY_FILE);

    if let Ok(bytes) = std::fs::read(&key_path) {
        return Key::try_from(bytes.as_slice())
            .with_context(|| format!("セッション鍵ファイルが不正です: {}", key_path.display()));
    }

    let key = Key::generate();
    std::fs::write(&key_path, key.master())
        .with_context(|| format!("セッション鍵の書き込みに失敗しました: {}", key_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("セッション鍵の権限設定に失敗しました: {}", key_path.display()))?;
    }
    tracing::info!(path = %key_path.display(), "セッション鍵を新規生成しました");
    Ok(key)
}

/// 起動時、設定に列挙された管理者アカウントが存在しなければ作成する。
pub async fn bootstrap_admins(pool: &SqlitePool, entries: &[AdminBootstrap]) -> Result<()> {
    for entry in entries {
        let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM users WHERE username = ?")
            .bind(&entry.username)
            .fetch_optional(pool)
            .await
            .context("管理者存在確認クエリに失敗しました")?;
        if exists.is_some() {
            continue;
        }
        let hash = hash_password(&entry.password).context("管理者パスワードのハッシュ化に失敗しました")?;
        sqlx::query(
            "INSERT INTO users (username, password_hash, role, disabled, created_at) \
             VALUES (?, ?, 'admin', 0, ?)",
        )
        .bind(&entry.username)
        .bind(&hash)
        .bind(OffsetDateTime::now_utc())
        .execute(pool)
        .await
        .context("管理者アカウントの作成に失敗しました")?;
        tracing::info!(username = %entry.username, "管理者アカウントを作成しました");
    }
    Ok(())
}

async fn fetch_user_by_id(pool: &SqlitePool, id: i64) -> AppResult<Option<User>> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(user)
}

/// セッションからログイン中ユーザを解決する共通ロジック。無効化済みユーザはNone扱いにする。
async fn load_current_user<S>(parts: &mut Parts, state: &S) -> AppResult<Option<User>>
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    let app_state = AppState::from_ref(state);
    let session = Session::from_request_parts(parts, state).await.map_err(|_| {
        AppError::Internal(anyhow::anyhow!(
            "セッションの抽出に失敗しました(SessionManagerLayerが有効か確認してください)"
        ))
    })?;
    let user_id: Option<i64> = session.get(SESSION_USER_ID_KEY).await?;
    let Some(user_id) = user_id else {
        return Ok(None);
    };
    let user = fetch_user_by_id(&app_state.db, user_id).await?;
    Ok(user.filter(|u| !u.disabled))
}

/// ログイン済みの任意ユーザ(admin/broadcaster問わず)を要求する抽出子。
pub struct CurrentUser(pub User);

impl<S> FromRequestParts<S> for CurrentUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = load_current_user(parts, state)
            .await?
            .ok_or_else(|| AppError::unauthorized("ログインが必要です"))?;
        Ok(CurrentUser(user))
    }
}

/// `Option<CurrentUser>` として使うための実装。未ログイン/無効化済みならNoneを返す
/// (視聴ページなど、ログイン任意の箇所で使う)。
impl<S> axum::extract::OptionalFromRequestParts<S> for CurrentUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Option<Self>, Self::Rejection> {
        Ok(load_current_user(parts, state).await?.map(CurrentUser))
    }
}

/// 管理者ロールを要求する抽出子。
pub struct AdminUser(pub User);

impl<S> FromRequestParts<S> for AdminUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let CurrentUser(user) = CurrentUser::from_request_parts(parts, state).await?;
        if !user.is_admin() {
            return Err(AppError::forbidden("管理者権限が必要です"));
        }
        Ok(AdminUser(user))
    }
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Serialize)]
struct UserInfo {
    id: i64,
    username: String,
    role: String,
}

impl From<&User> for UserInfo {
    fn from(u: &User) -> Self {
        UserInfo {
            id: u.id,
            username: u.username.clone(),
            role: u.role.clone(),
        }
    }
}

async fn login(
    axum::extract::State(state): axum::extract::State<AppState>,
    session: Session,
    Json(body): Json<LoginRequest>,
) -> AppResult<Json<UserInfo>> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&body.username)
        .fetch_optional(&state.db)
        .await?;

    let Some(user) = user else {
        // ユーザが存在しなくても常にArgon2検証を実行し、応答時間の差からユーザ名の
        // 存在有無が推測できないようにする。
        verify_password(&body.password, &DUMMY_HASH);
        return Err(AppError::unauthorized("ユーザ名またはパスワードが違います"));
    };

    if !verify_password(&body.password, &user.password_hash) {
        return Err(AppError::unauthorized("ユーザ名またはパスワードが違います"));
    }
    if user.disabled {
        return Err(AppError::forbidden("アカウントが無効化されています"));
    }

    // セッション固定攻撃対策: 認証成功時にセッションIDを振り直す。
    session.cycle_id().await?;
    session.insert(SESSION_USER_ID_KEY, user.id).await?;

    Ok(Json(UserInfo::from(&user)))
}

async fn logout(session: Session) -> AppResult<StatusCode> {
    session.flush().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn me(CurrentUser(user): CurrentUser) -> Json<UserInfo> {
    Json(UserInfo::from(&user))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/me", axum::routing::get(me))
}
