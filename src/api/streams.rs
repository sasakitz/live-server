//! 視聴者向けの公開API: ライブ配信一覧・単一配信メタ情報。
//! 視聴認可(ログイン要否)はWHEPと同じルールを使う。

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use time::OffsetDateTime;

use crate::auth::CurrentUser;
use crate::error::{AppError, AppResult};
use crate::models::Stream;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct StreamPublicInfo {
    pub id: String,
    pub title: String,
    pub description: String,
    pub is_live: bool,
    pub public: bool,
    pub viewer_count: usize,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
}

async fn to_public_info(state: &AppState, stream: Stream) -> StreamPublicInfo {
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
    StreamPublicInfo {
        id: stream.id,
        title: stream.title,
        description: stream.description,
        is_live: stream.is_live,
        public: stream.public,
        viewer_count,
        started_at: stream.started_at,
    }
}

fn visible(stream: &Stream, state: &AppState, logged_in: bool) -> bool {
    stream.public || state.config.auth.public_viewing || logged_in
}

/// `GET /api/streams` — 現在ライブ中の配信一覧(視聴可能なもののみ)。
async fn list_live(
    State(state): State<AppState>,
    current_user: Option<CurrentUser>,
) -> AppResult<Json<Vec<StreamPublicInfo>>> {
    let logged_in = current_user.is_some();
    let streams = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE is_live = 1 ORDER BY started_at ASC")
        .fetch_all(&state.db)
        .await?;

    let mut out = Vec::with_capacity(streams.len());
    for stream in streams {
        if visible(&stream, &state, logged_in) {
            out.push(to_public_info(&state, stream).await);
        }
    }
    Ok(Json(out))
}

/// `GET /api/streams/{id}` — 単一配信のメタ情報(視聴ページの表示用)。
async fn get_one(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    current_user: Option<CurrentUser>,
) -> AppResult<Json<StreamPublicInfo>> {
    let logged_in = current_user.is_some();
    let stream = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE id = ?")
        .bind(&stream_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::not_found("配信が見つかりません"))?;

    if !visible(&stream, &state, logged_in) {
        return Err(AppError::unauthorized("視聴にはログインが必要です"));
    }

    Ok(Json(to_public_info(&state, stream).await))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_live))
        .route("/{stream_id}", get(get_one))
}
