//! WHEP (WebRTC-HTTP Egress Protocol) 配信エンドポイント。ブラウザ視聴者向け。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, post};
use axum::Router;
use uuid::Uuid;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::track::track_local::TrackLocal;

use crate::auth::CurrentUser;
use crate::error::{AppError, AppResult};
use crate::models::Stream;
use crate::sfu::engine;
use crate::state::{AppEvent, AppState};

const SDP_MIME: &str = "application/sdp";

/// `POST /api/whep/{stream_id}` — ブラウザはこのURLにSDP offer(recvonly)をPOSTする。
/// 視聴認可: 配信がpublicか、site-wideでpublic_viewingが有効か、ログイン済みのいずれか。
async fn watch(
    State(state): State<AppState>,
    Path(stream_id): Path<String>,
    current_user: Option<CurrentUser>,
    body: Bytes,
) -> AppResult<Response> {
    let offer_sdp =
        String::from_utf8(body.to_vec()).map_err(|_| AppError::bad_request("SDPがUTF-8として不正です"))?;

    let stream = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE id = ?")
        .bind(&stream_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::not_found("配信が見つかりません"))?;

    let publicly_viewable = stream.public || state.config.auth.public_viewing;
    if !publicly_viewable && current_user.is_none() {
        return Err(AppError::unauthorized("視聴にはログインが必要です"));
    }

    let room = {
        let rooms = state.rooms.read().await;
        rooms.get(&stream_id).cloned()
    }
    .ok_or_else(|| AppError::conflict("配信は現在オフラインです"))?;

    let config = RTCConfiguration {
        ice_servers: engine::ice_servers(&state.config.webrtc),
        ..Default::default()
    };
    let pc = Arc::new(state.webrtc_api.new_peer_connection(config).await?);

    // set_remote_descriptionより前にadd_trackでRoomの共有トラックを紐付けたsendrecv
    // transceiverを用意しておく。sendrecvはブラウザのrecvonlyオファーとのマッチングで
    // 優先的に再利用される(satisfy_type_and_direction)。
    // 逆に、先にrecvonlyオファーを処理させてから自動生成されたsenderにreplace_trackで
    // 初回のトラックを設定しようとすると、そのsenderのtrack_encodingsが空のままのため
    // "new track must have the same envelope as previous"エラーになる
    // (replace_trackは「既存トラックの差し替え」専用で、初回設定には使えない)。
    pc.add_track(room.video_track.clone() as Arc<dyn TrackLocal + Send + Sync>)
        .await?;
    pc.add_track(room.audio_track.clone() as Arc<dyn TrackLocal + Send + Sync>)
        .await?;

    let offer = RTCSessionDescription::offer(offer_sdp)?;
    pc.set_remote_description(offer).await?;

    let answer = pc.create_answer(None).await?;
    let mut gather_complete = pc.gathering_complete_promise().await;
    pc.set_local_description(answer).await?;
    let _ = gather_complete.recv().await;

    let local_desc = pc
        .local_description()
        .await
        .ok_or_else(|| AppError::from(anyhow::anyhow!("local descriptionの取得に失敗しました")))?;

    let viewer_id = Uuid::new_v4();
    let count = room.add_viewer(viewer_id, pc.clone()).await;
    state.publish(AppEvent::ViewerCount {
        stream_id: stream_id.clone(),
        count,
    });

    {
        let state = state.clone();
        let room = room.clone();
        let stream_id = stream_id.clone();
        pc.on_peer_connection_state_change(Box::new(move |pc_state| {
            let state = state.clone();
            let room = room.clone();
            let stream_id = stream_id.clone();
            Box::pin(async move {
                if matches!(
                    pc_state,
                    RTCPeerConnectionState::Disconnected
                        | RTCPeerConnectionState::Failed
                        | RTCPeerConnectionState::Closed
                ) {
                    // PCは既に切断/終了処理中なので、ここではRoomからの除去とカウント通知のみ行う。
                    if let Some((count, _pc)) = room.remove_viewer(&viewer_id).await {
                        state.publish(AppEvent::ViewerCount {
                            stream_id: stream_id.clone(),
                            count,
                        });
                    }
                }
            })
        }));
    }

    let headers = [
        (header::CONTENT_TYPE, SDP_MIME.to_string()),
        (header::LOCATION, format!("/api/whep/resource/{viewer_id}")),
    ];

    Ok((StatusCode::CREATED, headers, local_desc.sdp).into_response())
}

/// `DELETE /api/whep/resource/{viewer_id}` — 視聴終了の明示通知(ページ離脱時など)。
/// どのRoomに属するか分からないため全Roomを探索する(既定10配信程度の規模なので許容範囲)。
async fn terminate(Path(viewer_id): Path<Uuid>, State(state): State<AppState>) -> AppResult<StatusCode> {
    let rooms: Vec<_> = state.rooms.read().await.values().cloned().collect();
    for room in rooms {
        if let Some((count, pc)) = room.remove_viewer(&viewer_id).await {
            let _ = pc.close().await;
            state.publish(AppEvent::ViewerCount {
                stream_id: room.stream_id.clone(),
                count,
            });
            break;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{stream_id}", post(watch))
        .route("/resource/{viewer_id}", delete(terminate))
}
