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
use webrtc::track::track_local::TrackLocal;

use crate::auth::CurrentUser;
use crate::error::{AppError, AppResult};
use crate::models::Stream;
use crate::sfu::negotiate;
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

    // public_ipsで自身の公開アドレスを直接把握しているため、STUNサーバは不要
    // (詳細はsfu/engine.rsのWebRtcConfig::public_ipsのコメント参照)。
    let pc = Arc::new(
        state
            .webrtc_api
            .new_peer_connection(RTCConfiguration::default())
            .await?,
    );
    let pending = crate::sfu::NegotiatingPeer::new(pc.clone());

    // set_remote_descriptionより前にadd_trackでRoomの共有トラックを紐付けたsendrecv
    // transceiverを用意しておく。sendrecvはブラウザのrecvonlyオファーとのマッチングで
    // 優先的に再利用される(satisfy_type_and_direction)。
    // 逆に、先にrecvonlyオファーを処理させてから自動生成されたsenderにreplace_trackで
    // 初回のトラックを設定しようとすると、そのsenderのtrack_encodingsが空のままのため
    // "new track must have the same envelope as previous"エラーになる
    // (replace_trackは「既存トラックの差し替え」専用で、初回設定には使えない)。
    let negotiation = async {
        pc.add_track(room.video_track.clone() as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        pc.add_track(room.audio_track.clone() as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        negotiate(&pc, offer_sdp).await
    }
    .await;
    let sdp = match negotiation {
        Ok(sdp) => sdp,
        Err(error) => {
            let _ = pc.close().await;
            return Err(error);
        }
    };

    let viewer_id = Uuid::new_v4();

    {
        let state = state.clone();
        let room = room.clone();
        pc.on_peer_connection_state_change(Box::new(move |pc_state| {
            let state = state.clone();
            let room = room.clone();
            Box::pin(async move {
                if matches!(
                    pc_state,
                    RTCPeerConnectionState::Disconnected
                        | RTCPeerConnectionState::Failed
                        | RTCPeerConnectionState::Closed
                ) {
                    // close()は状態通知のロック外で実行する。
                    tokio::spawn(async move {
                        if let Some((_count, pc)) = room.remove_viewer(&viewer_id).await {
                            publish_viewer_count(&state, &room).await;
                            let _ = pc.close().await;
                        }
                    });
                }
            })
        }));
    }

    let rooms = state.rooms.read().await;
    if !rooms.get(&stream_id).is_some_and(|current| current.session_id == room.session_id) {
        drop(rooms);
        let _ = pc.close().await;
        return Err(AppError::conflict("配信は終了しています"));
    }
    let Some(count) = room.add_viewer(viewer_id, pc.clone()).await else {
        drop(rooms);
        let _ = pc.close().await;
        return Err(AppError::conflict("配信または視聴接続は終了しています"));
    };
    state.publish(AppEvent::ViewerCount { stream_id: stream_id.clone(), count });
    drop(rooms);

    let headers = [
        (header::CONTENT_TYPE, SDP_MIME.to_string()),
        (header::LOCATION, format!("/api/whep/resource/{viewer_id}")),
    ];

    pending.finish();
    Ok((StatusCode::CREATED, headers, sdp).into_response())
}

/// `DELETE /api/whep/resource/{viewer_id}` — 視聴終了の明示通知(ページ離脱時など)。
/// どのRoomに属するか分からないため全Roomを探索する(既定10配信程度の規模なので許容範囲)。
async fn terminate(Path(viewer_id): Path<Uuid>, State(state): State<AppState>) -> AppResult<StatusCode> {
    let rooms: Vec<_> = state.rooms.read().await.values().cloned().collect();
    for room in rooms {
        if let Some((_count, pc)) = room.remove_viewer(&viewer_id).await {
            publish_viewer_count(&state, &room).await;
            let _ = pc.close().await;
            break;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn publish_viewer_count(state: &AppState, room: &crate::sfu::room::Room) {
    let rooms = state.rooms.read().await;
    if rooms.get(&room.stream_id).is_some_and(|current| current.session_id == room.session_id) {
        state.publish(AppEvent::ViewerCount { stream_id: room.stream_id.clone(), count: room.viewer_count() });
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{stream_id}", post(watch))
        .route("/resource/{viewer_id}", delete(terminate))
}
