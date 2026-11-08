//! WHIP (WebRTC-HTTP Ingestion Protocol) 受信エンドポイント。OBSからの配信を受け付ける。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, post};
use axum::Router;
use uuid::Uuid;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::track::track_local::TrackLocalWriter;

use crate::error::{AppError, AppResult};
use crate::models::Stream;
use crate::sfu::{lifecycle, negotiate};
use crate::sfu::room::Room;
use crate::sfu::thumbnail::{self, ThumbnailCapturer};
use crate::state::AppState;

const SDP_MIME: &str = "application/sdp";

/// `Authorization: Bearer <token>` からstream_keyを取り出す。OBSの実装によっては
/// Bearer Token欄が空だと接続に失敗する個体があるため、URLパスとBearer Tokenの
/// どちらでもstream_keyを受け付けられるようにしている(Bearerを優先)。
fn bearer_stream_key(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_owned())
    }
}

/// `POST /api/whip/{stream_key}` — OBSはこのURLに対しSDP offerをPOSTする。
/// stream_keyはURLパスに含めるのが既定だが、Authorization: Bearerが設定されて
/// いればそちらを優先する(空欄でも動くはずだが、OBS側の個体差を考慮)。
async fn publish(
    State(state): State<AppState>,
    Path(stream_key): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let stream_key = bearer_stream_key(&headers).unwrap_or(stream_key);
    let offer_sdp =
        String::from_utf8(body.to_vec()).map_err(|_| AppError::bad_request("SDPがUTF-8として不正です"))?;

    let stream = sqlx::query_as::<_, Stream>("SELECT s.* FROM streams s JOIN users u ON u.id = s.owner_user_id WHERE s.stream_key = ? AND u.disabled = 0")
        .bind(&stream_key)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::unauthorized("stream_keyが無効です"))?;

    let existing_room = {
        let rooms = state.rooms.read().await;
        rooms.get(&stream.id).cloned()
    };
    if let Some(existing) = existing_room {
        // OBSがWHIPのDELETEを送らずに切断した場合、ICEの切断検知には数秒かかることがある。
        // その間に再配信しようとすると本来は不要な409を返してしまうため、既存の
        // publisher_pcが既に非アクティブ(Disconnected/Failed/Closed)であれば、
        // ここで能動的に古いRoomを片付けてから新規配信を受け付ける。
        let is_stale = match existing.publisher_pc.read().await.as_ref() {
            Some(pc) => matches!(
                pc.connection_state(),
                RTCPeerConnectionState::Disconnected
                    | RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Closed
            ),
            None => true,
        };
        if is_stale {
            tracing::info!(stream_id = %stream.id, "未クリーンアップの旧セッションを検出したため破棄して再配信を受け付けます");
            lifecycle::stop_session(&state, &stream.id, existing.session_id).await?;
        } else {
            return Err(AppError::conflict("この配信は既に開始されています"));
        }
    }

    // public_ipsで自身の公開アドレスを直接把握しているため、STUNサーバは不要
    // (詳細はsfu/engine.rsのWebRtcConfig::public_ipsのコメント参照)。
    let pc = Arc::new(
        state
            .webrtc_api
            .new_peer_connection(RTCConfiguration::default())
            .await?,
    );
    let mut pending = crate::sfu::NegotiatingPeer::new(pc.clone());

    let room = Arc::new(Room::new(stream.id.clone()));
    *room.publisher_pc.write().await = Some(pc.clone());
    if let Err(error) = lifecycle::register_room(&state, &stream_key, room.clone()).await {
        let _ = pc.close().await;
        return Err(error);
    }
    let session_id = room.session_id;
    pending.publisher = Some((state.clone(), stream.id.clone(), session_id));


    // OBSから届いたRTPをそのままRoomのローカルトラックへ書き込む(=視聴者へのファンアウト元)。
    // トランスコードは行わない。
    {
        let room = room.clone();
        pc.on_track(Box::new(move |track, _receiver, _transceiver| {
            let room = room.clone();
            Box::pin(async move {
                let is_video = track.kind() == RTPCodecType::Video;
                if is_video {
                    *room.publisher_video_ssrc.write().await = Some(track.ssrc());
                }
                let local_track = if is_video {
                    room.video_track.clone()
                } else {
                    room.audio_track.clone()
                };
                let room = room.clone();
                tokio::spawn(async move {
                    // 配信一覧のサムネイル用に、Video RTPだけ並行してAnnex-B断片へ
                    // 再構成する(詳細はsfu/thumbnail.rs参照)。中継のホットパス自体は
                    // write_rtpのみで、ffmpeg起動は別タスクに逃がして遅延させない。
                    let mut capturer = is_video.then(ThumbnailCapturer::new);
                    loop {
                        match track.read_rtp().await {
                            Ok((packet, _attributes)) => {
                                // 1視聴者の送信失敗で、全視聴者への中継を終了させない。
                                if let Err(error) = local_track.write_rtp(&packet).await {
                                    tracing::trace!(?error, "RTP送信の一部が失敗しました");
                                }
                                if let Some(capturer) = capturer.as_mut() {
                                    if let Some(annexb) = capturer.push(&packet) {
                                        let room = room.clone();
                                        tokio::spawn(async move {
                                            if let Some(jpeg) = thumbnail::encode_jpeg(annexb).await {
                                                *room.thumbnail.write().await = Some(jpeg);
                                            }
                                        });
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
            })
        }));
    }

    {
        let state = state.clone();
        let stream_id = stream.id.clone();
        pc.on_peer_connection_state_change(Box::new(move |pc_state| {
            let state = state.clone();
            let stream_id = stream_id.clone();
            Box::pin(async move {
                // close()が再びこのハンドラを呼ぶため、処理を別タスクへ渡して直ちに戻る。
                tokio::spawn(async move {
                    let result = match pc_state {
                        RTCPeerConnectionState::Connected => lifecycle::mark_stream_started(&state, &stream_id, session_id).await,
                        RTCPeerConnectionState::Disconnected | RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => lifecycle::stop_session(&state, &stream_id, session_id).await,
                        _ => Ok(()),
                    };
                    if let Err(error) = result { tracing::error!(?error, stream_id, "配信状態の更新に失敗しました"); }
                });
            })
        }));
    }

    // set_remote_description以降のSDPネゴシエーションが失敗すると、それ以前に
    // state.roomsへ登録済みのRoomが誰にも片付けられないまま残ってしまい
    // (publisher_pcが一度も接続状態に至らないため、既存の再接続時の自己修復判定
    // にも引っかからない)、以後そのstream_idへの配信が永久に409になる不具合が
    // あった。ネゴシエーション処理を切り出し、失敗時はここでRoomを明示的に破棄する。
    let sdp = match negotiate(&pc, offer_sdp).await {
        Ok(sdp) => sdp,
        Err(e) => {
            lifecycle::stop_session(&state, &stream.id, session_id).await?;
            let _ = pc.close().await;
            return Err(e);
        }
    };

    let headers = [
        (header::CONTENT_TYPE, SDP_MIME.to_string()),
        (
            header::LOCATION,
            format!("/api/whip/resource/{session_id}"),
        ),
    ];

    pending.finish();
    Ok((StatusCode::CREATED, headers, sdp).into_response())
}

/// Locationは接続ごとに生成する推測困難なID。古いDELETEで再配信を止めない。
async fn terminate(Path(session_id): Path<Uuid>, State(state): State<AppState>) -> AppResult<StatusCode> {
    let stream_id = state.rooms.read().await.values()
        .find(|room| room.session_id == session_id).map(|room| room.stream_id.clone());
    if let Some(stream_id) = stream_id {
        lifecycle::stop_session(&state, &stream_id, session_id).await?;
    }
    // OBSのWHIP実装はDELETE成功を200で判定する。
    Ok(StatusCode::OK)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{stream_key}", post(publish))
        .route("/resource/{session_id}", delete(terminate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn old_resource_delete_cannot_stop_reconnected_publisher() {
        let state = crate::state::test_state().await;
        let old = Arc::new(Room::new("test".into()));
        lifecycle::register_room(&state, "secret", old.clone()).await.unwrap();
        assert_eq!(terminate(Path(old.session_id), State(state.clone())).await.unwrap(), StatusCode::OK);
        let new = Arc::new(Room::new("test".into()));
        lifecycle::register_room(&state, "secret", new.clone()).await.unwrap();
        assert_eq!(terminate(Path(old.session_id), State(state.clone())).await.unwrap(), StatusCode::OK);
        assert_eq!(state.rooms.read().await["test"].session_id, new.session_id);
        assert_eq!(terminate(Path(new.session_id), State(state.clone())).await.unwrap(), StatusCode::OK);
        assert!(state.rooms.read().await.is_empty());
    }
}
