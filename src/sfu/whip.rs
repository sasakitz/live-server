//! WHIP (WebRTC-HTTP Ingestion Protocol) 受信エンドポイント。OBSからの配信を受け付ける。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, post};
use axum::Router;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::track::track_local::TrackLocalWriter;

use crate::error::{AppError, AppResult};
use crate::models::Stream;
use crate::sfu::lifecycle;
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

    let stream = sqlx::query_as::<_, Stream>("SELECT * FROM streams WHERE stream_key = ?")
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
            lifecycle::stop_stream(&state, &stream.id, true).await?;
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

    let room = Arc::new(Room::new(stream.id.clone()));
    *room.publisher_pc.write().await = Some(pc.clone());
    state
        .rooms
        .write()
        .await
        .insert(stream.id.clone(), room.clone());

    // OBSから届いたRTPをそのままRoomのローカルトラックへ書き込む(=視聴者へのファンアウト元)。
    // トランスコードは行わない。
    {
        let room = room.clone();
        pc.on_track(Box::new(move |track, _receiver, _transceiver| {
            let room = room.clone();
            Box::pin(async move {
                let is_video = track.kind() == RTPCodecType::Video;
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
                                if local_track.write_rtp(&packet).await.is_err() {
                                    break;
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
                match pc_state {
                    RTCPeerConnectionState::Connected => {
                        if let Err(e) = lifecycle::mark_stream_started(&state, &stream_id).await {
                            tracing::error!(error = ?e, stream_id, "配信開始処理に失敗しました");
                        }
                    }
                    RTCPeerConnectionState::Disconnected
                    | RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Closed => {
                        // このコールバックはPeerConnection内部でon_peer_connection_state_change_handler
                        // をロックした状態で呼ばれるため、ここでpc.close()を呼ぶと同じロックの
                        // 再取得でデッドロックする。PCは既に閉じかけているのでclose_publisher_pc=false。
                        if let Err(e) = lifecycle::stop_stream(&state, &stream_id, false).await {
                            tracing::error!(error = ?e, stream_id, "配信終了処理に失敗しました");
                        }
                    }
                    _ => {}
                }
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
            state.rooms.write().await.remove(&stream.id);
            let _ = pc.close().await;
            return Err(e);
        }
    };

    let headers = [
        (header::CONTENT_TYPE, SDP_MIME.to_string()),
        (
            header::LOCATION,
            format!("/api/whip/resource/{}", stream.id),
        ),
    ];

    Ok((StatusCode::CREATED, headers, sdp).into_response())
}

async fn negotiate(pc: &RTCPeerConnection, offer_sdp: String) -> AppResult<String> {
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
    Ok(local_desc.sdp)
}

/// `DELETE /api/whip/resource/{stream_id}` — OBS側の「配信終了」操作、または異常切断時に呼ばれる。
/// on_peer_connection_state_changeのコールバック外(通常のリクエストハンドラ)なので
/// 明示的にPeerConnectionを閉じる(close_publisher_pc=true)。
async fn terminate(Path(stream_id): Path<String>, State(state): State<AppState>) -> AppResult<StatusCode> {
    lifecycle::stop_stream(&state, &stream_id, true).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{stream_key}", post(publish))
        .route("/resource/{stream_id}", delete(terminate))
}
