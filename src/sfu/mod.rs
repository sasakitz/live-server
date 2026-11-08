pub mod engine;
pub mod lifecycle;
pub mod room;
pub mod thumbnail;
pub mod whep;
pub mod whip;

use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

use crate::error::{AppError, AppResult};

/// WHIP/WHEP共通のSDP応答生成。失敗時のPeerConnection破棄は呼び出し元が行う。
async fn negotiate(pc: &RTCPeerConnection, offer_sdp: String) -> AppResult<String> {
    let offer = RTCSessionDescription::offer(offer_sdp)
        .map_err(|_| AppError::bad_request("SDP offerが不正です"))?;
    pc.set_remote_description(offer).await
        .map_err(|_| AppError::bad_request("SDP offerを受け付けられません"))?;

    let answer = pc.create_answer(None).await?;
    let mut gather_complete = pc.gathering_complete_promise().await;
    pc.set_local_description(answer).await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), gather_complete.recv()).await
        .map_err(|_| AppError::from(anyhow::anyhow!("ICE候補収集がタイムアウトしました")))?;

    let local_desc = pc
        .local_description()
        .await
        .ok_or_else(|| AppError::from(anyhow::anyhow!("local descriptionの取得に失敗しました")))?;
    Ok(local_desc.sdp)
}

/// HTTPリクエストが中断されても、ネゴシエーション途中のPCを確実に破棄する。
struct NegotiatingPeer {
    pc: Option<std::sync::Arc<RTCPeerConnection>>,
    publisher: Option<(crate::state::AppState, String, uuid::Uuid)>,
}
impl NegotiatingPeer {
    fn new(pc: std::sync::Arc<RTCPeerConnection>) -> Self { Self { pc: Some(pc), publisher: None } }
    fn finish(mut self) { self.pc.take(); }
}
impl Drop for NegotiatingPeer {
    fn drop(&mut self) {
        let Some(pc) = self.pc.take() else { return };
        let publisher = self.publisher.take();
        tokio::spawn(async move {
            if let Some((state, stream_id, session_id)) = publisher {
                if let Err(error) = lifecycle::stop_session(&state, &stream_id, session_id).await {
                    tracing::error!(?error, stream_id, "中断した配信接続の破棄に失敗しました");
                }
            }
            let _ = pc.close().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;

    #[tokio::test]
    async fn cancelled_negotiation_closes_pc_and_removes_room() {
        let state = crate::state::test_state().await;
        let pc = Arc::new(state.webrtc_api.new_peer_connection(Default::default()).await.unwrap());
        let room = Arc::new(room::Room::new("test".into()));
        *room.publisher_pc.write().await = Some(pc.clone());
        lifecycle::register_room(&state, "secret", room.clone()).await.unwrap();
        let mut pending = NegotiatingPeer::new(pc.clone());
        pending.publisher = Some((state.clone(), "test".into(), room.session_id));
        drop(pending);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while pc.connection_state() != RTCPeerConnectionState::Closed {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert!(state.rooms.read().await.is_empty());
    }
}
