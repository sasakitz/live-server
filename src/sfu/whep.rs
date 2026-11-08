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
use webrtc::rtcp::payload_feedbacks::{
    full_intra_request::FullIntraRequest, picture_loss_indication::PictureLossIndication,
};
use webrtc::rtp_transceiver::rtp_sender::RTCRtpSender;
use webrtc::track::track_local::TrackLocal;

use crate::auth::CurrentUser;
use crate::error::{AppError, AppResult};
use crate::models::Stream;
use crate::sfu::negotiate;
use crate::state::{AppEvent, AppState};

const SDP_MIME: &str = "application/sdp";

/// read_rtcpを回して初めてwebrtc-rsのNACK interceptorが再送要求を処理する。
/// PLI/FIRは配信元へ届け、その他のRTCPは各PeerConnection内で処理する。
fn read_viewer_feedback(sender: Arc<RTCRtpSender>, room: Option<Arc<crate::sfu::room::Room>>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok((packets, _)) = sender.read_rtcp().await {
            if let Some(room) = &room {
                if packets.iter().any(|packet| {
                    packet.as_any().is::<PictureLossIndication>()
                        || packet.as_any().is::<FullIntraRequest>()
                }) {
                    room.request_keyframe().await;
                }
            }
        }
    })
}

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
        let video_sender = pc.add_track(room.video_track.clone() as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        let audio_sender = pc.add_track(room.audio_track.clone() as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        read_viewer_feedback(video_sender, Some(room.clone()));
        read_viewer_feedback(audio_sender, None);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use webrtc::api::{APIBuilder,
        media_engine::MediaEngine, setting_engine::SettingEngine};
    use webrtc::rtcp::payload_feedbacks::full_intra_request::FirEntry;
    use webrtc::rtcp::transport_feedbacks::transport_layer_nack::{NackPair, TransportLayerNack};
    use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecParameters, RTPCodecType};
    use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;

    async fn peer() -> Arc<webrtc::peer_connection::RTCPeerConnection> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut media = MediaEngine::default();
        media.register_codec(RTCRtpCodecParameters {
            capability: crate::sfu::engine::h264_capability(),
            payload_type: 102,
            ..Default::default()
        }, RTPCodecType::Video).unwrap();
        let registry = crate::sfu::engine::register_interceptors(&mut media).unwrap();
        let mut settings = SettingEngine::default();
        settings.set_include_loopback_candidate(true);
        // 再送を観測するテスト用。既に受信済みのパケットを再送要求するため。
        settings.disable_srtp_replay_protection(true);
        settings.set_ip_filter(Box::new(|ip| ip.is_loopback()));
        Arc::new(APIBuilder::new().with_media_engine(media)
            .with_interceptor_registry(registry).with_setting_engine(settings)
            .build().new_peer_connection(Default::default()).await.unwrap())
    }

    async fn connect(sender: &webrtc::peer_connection::RTCPeerConnection,
        receiver: &webrtc::peer_connection::RTCPeerConnection) {
        let offer = sender.create_offer(None).await.unwrap();
        let mut gathering = sender.gathering_complete_promise().await;
        sender.set_local_description(offer).await.unwrap();
        gathering.recv().await;
        receiver.set_remote_description(sender.local_description().await.unwrap()).await.unwrap();
        let answer = receiver.create_answer(None).await.unwrap();
        let mut gathering = receiver.gathering_complete_promise().await;
        receiver.set_local_description(answer).await.unwrap();
        gathering.recv().await;
        sender.set_remote_description(receiver.local_description().await.unwrap()).await.unwrap();
        while sender.connection_state() != RTCPeerConnectionState::Connected
            || receiver.connection_state() != RTCPeerConnectionState::Connected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn viewer_feedback_retransmits_rtp_requests_keyframes_and_stops_on_close() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let publisher = peer().await;
            let ingest = peer().await;
            let egress = peer().await;
            let viewer = peer().await;
            let room = Arc::new(crate::sfu::room::Room::new("feedback-test".into()));
            let publisher_track = Arc::new(TrackLocalStaticRTP::new(
                crate::sfu::engine::h264_capability(), "video".into(), "publisher".into()));
            let publisher_sender = publisher.add_track(publisher_track).await.unwrap();
            connect(&publisher, &ingest).await;
            let publisher_ssrc = publisher_sender.get_parameters().await.encodings[0].ssrc;
            *room.publisher_video_ssrc.write().await = Some(publisher_ssrc);
            *room.publisher_pc.write().await = Some(ingest.clone());
            let video_sender = egress.add_track(room.video_track.clone()).await.unwrap();
            let feedback_task = read_viewer_feedback(video_sender.clone(), Some(room.clone()));
            let (rtp_tx, mut rtp_rx) = mpsc::unbounded_channel();
            viewer.on_track(Box::new(move |track, _, _| {
                let tx = rtp_tx.clone();
                Box::pin(async move {
                    tokio::spawn(async move {
                        while let Ok((packet, _)) = track.read_rtp().await {
                            if tx.send(packet).is_err() { break; }
                        }
                    });
                })
            }));
            connect(&egress, &viewer).await;
            let viewer_ssrc = video_sender.get_parameters().await.encodings[0].ssrc;
            use webrtc::track::track_local::TrackLocalWriter;
            let payload = bytes::Bytes::from_static(&[0x65, 1, 2, 3]);
            let mut packet = rtp::packet::Packet {
                header: rtp::header::Header {
                    version: 2, sequence_number: 100, timestamp: 90000,
                    marker: true, ..Default::default()
                },
                payload: payload.clone(),
            };
            // OBS側の拡張番号と値をそのまま転送してはいけない。
            packet.header.set_extension(7, bytes::Bytes::from_static(&[0xfe, 0xdc])).unwrap();
            crate::sfu::whip::prepare_relay_packet(&mut packet);
            room.video_track.write_rtp(&packet).await.unwrap();
            let received = rtp_rx.recv().await.unwrap();
            assert_eq!(received.header.sequence_number, 100);
            assert!(received.header.get_extension(7).is_none());
            let params = video_sender.get_parameters().await;
            let twcc_id = params.rtp_parameters.header_extensions.iter()
                .find(|ext| ext.uri.ends_with("transport-wide-cc-extensions-01")).unwrap().id as u8;
            let first_twcc = received.header.get_extension(twcc_id).unwrap();
            assert_eq!(first_twcc.len(), 2);
            assert_ne!(first_twcc.as_ref(), &[0xfe, 0xdc]);
            viewer.write_rtcp(&[Box::new(TransportLayerNack {
                sender_ssrc: 0, media_ssrc: viewer_ssrc,
                nacks: vec![NackPair { packet_id: 100, lost_packets: 0 }],
            })]).await.unwrap();
            let resent = rtp_rx.recv().await.unwrap();
            assert_eq!(resent.header.sequence_number, 100);
            assert_eq!(resent.payload, payload);
            let next_twcc = resent.header.get_extension(twcc_id).unwrap();
            assert_eq!(u16::from_be_bytes([next_twcc[0], next_twcc[1]]),
                u16::from_be_bytes([first_twcc[0], first_twcc[1]]).wrapping_add(1));
            let (tx, mut rx) = mpsc::unbounded_channel();
            let publisher_feedback = tokio::spawn(async move {
                while let Ok((packets, _)) = publisher_sender.read_rtcp().await {
                    for packet in packets {
                        if let Some(pli) = packet.as_any().downcast_ref::<PictureLossIndication>() {
                            tx.send(pli.media_ssrc).unwrap();
                        }
                    }
                }
            });
            viewer.write_rtcp(&[Box::new(PictureLossIndication {
                sender_ssrc: 0, media_ssrc: viewer_ssrc,
            })]).await.unwrap();
            assert_eq!(rx.recv().await.unwrap(), publisher_ssrc);
            // 同じ配信の同時要求を集約する。
            room.request_keyframe().await;
            assert!(tokio::time::timeout(Duration::from_millis(100), rx.recv()).await.is_err());
            tokio::time::sleep(Duration::from_millis(500)).await;
            // FIRもOBS側のSSRCに置き換えたPLIとして届ける。
            viewer.write_rtcp(&[Box::new(FullIntraRequest {
                sender_ssrc: 0, media_ssrc: 0,
                fir: vec![FirEntry { ssrc: viewer_ssrc, sequence_number: 1 }],
            })]).await.unwrap();
            assert_eq!(rx.recv().await.unwrap(), publisher_ssrc);
            egress.close().await.unwrap();
            feedback_task.await.unwrap();
            publisher.close().await.unwrap();
            publisher_feedback.await.unwrap();
            ingest.close().await.unwrap();
            viewer.close().await.unwrap();
        }).await.unwrap();
    }
}
