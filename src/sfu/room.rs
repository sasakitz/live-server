use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;

use crate::sfu::engine::{h264_capability, opus_capability};

pub type ViewerId = Uuid;

/// 1配信(1 stream_id)に対応するSFUの中核。
/// WHIPで受信したRTPをそのままローカルトラックへ書き込み、WHEPで接続した
/// 各視聴者PeerConnectionのsenderがそのローカルトラックを送出する(ファンアウト)。
pub struct Room {
    pub stream_id: String,
    pub session_id: Uuid,
    closed: AtomicBool,
    public: AtomicBool,
    pub video_track: Arc<TrackLocalStaticRTP>,
    pub audio_track: Arc<TrackLocalStaticRTP>,
    pub viewers: RwLock<HashMap<ViewerId, Arc<RTCPeerConnection>>>,
    pub publisher_pc: RwLock<Option<Arc<RTCPeerConnection>>>,
    pub publisher_video_ssrc: RwLock<Option<u32>>,
    last_keyframe_request: Mutex<Option<Instant>>,
    /// 配信一覧向けの直近サムネイル(JPEG)。`sfu::thumbnail`が定期的に更新する。
    pub thumbnail: RwLock<Option<Bytes>>,
    viewer_count: AtomicUsize,
    peak_viewers: AtomicUsize,
}

impl Room {
    pub fn new(stream_id: String) -> Self {
        let video_track = Arc::new(TrackLocalStaticRTP::new(
            h264_capability(),
            "video".to_owned(),
            stream_id.clone(),
        ));
        let audio_track = Arc::new(TrackLocalStaticRTP::new(
            opus_capability(),
            "audio".to_owned(),
            stream_id.clone(),
        ));
        Room {
            stream_id,
            session_id: Uuid::new_v4(),
            closed: AtomicBool::new(false),
            public: AtomicBool::new(false),
            video_track,
            audio_track,
            viewers: RwLock::new(HashMap::new()),
            publisher_pc: RwLock::new(None),
            publisher_video_ssrc: RwLock::new(None),
            last_keyframe_request: Mutex::new(None),
            thumbnail: RwLock::new(None),
            viewer_count: AtomicUsize::new(0),
            peak_viewers: AtomicUsize::new(0),
        }
    }

    pub async fn add_viewer(&self, id: ViewerId, pc: Arc<RTCPeerConnection>) -> Option<usize> {
        let mut viewers = self.viewers.write().await;
        if self.closed.load(Ordering::SeqCst) || matches!(
            pc.connection_state(),
            webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Disconnected
                | webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Failed
                | webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState::Closed
        ) {
            return None;
        }
        viewers.insert(id, pc);
        let count = viewers.len();
        self.viewer_count.store(count, Ordering::SeqCst);
        self.peak_viewers.fetch_max(count, Ordering::SeqCst);
        Some(count)
    }

    /// 視聴者を削除する。実際に存在した場合のみ `Some((更新後の人数, そのPeerConnection))` を返す。
    /// 呼び出し元は、自然切断由来(既にPC状態が変化している)なら返ってきたPCを再度closeする必要はないが、
    /// 明示的なDELETE(視聴終了)由来ならPCを閉じる責任を持つ。
    pub async fn remove_viewer(&self, id: &ViewerId) -> Option<(usize, Arc<RTCPeerConnection>)> {
        let mut viewers = self.viewers.write().await;
        let pc = viewers.remove(id)?;
        let count = viewers.len();
        self.viewer_count.store(count, Ordering::SeqCst);
        Some((count, pc))
    }

    pub fn set_public(&self, public: bool) { self.public.store(public, Ordering::SeqCst); }
    pub fn is_public(&self) -> bool { self.public.load(Ordering::SeqCst) }

    pub fn viewer_count(&self) -> usize {
        self.viewer_count.load(Ordering::SeqCst)
    }

    pub fn peak_viewers(&self) -> usize {
        self.peak_viewers.load(Ordering::SeqCst)
    }

    /// OBS側の映像SSRCを指定して復旧を要求する。複数視聴者からの要求は集約する。
    pub async fn request_keyframe(&self) {
        let Some(media_ssrc) = *self.publisher_video_ssrc.read().await else { return };
        let Some(pc) = self.publisher_pc.read().await.clone() else { return };
        {
            let mut last = self.last_keyframe_request.lock().await;
            let now = Instant::now();
            if last.is_some_and(|at| now.duration_since(at) < Duration::from_millis(500)) {
                return;
            }
            *last = Some(now);
        }
        if let Err(error) = pc.write_rtcp(&[Box::new(PictureLossIndication {
            sender_ssrc: 0,
            media_ssrc,
        })]).await {
            tracing::trace!(?error, "キーフレーム要求を送信できませんでした");
        }
    }

    /// 配信終了時に全視聴者PeerConnectionを閉じる。
    pub async fn close_all_viewers(&self) {
        // 書き込みロックを保持したままpc.close().awaitすると、そのclose()がその
        // 視聴者自身のon_peer_connection_state_changeコールバックを誘発し、
        // コールバック側のroom.remove_viewer()が同じロックの取得待ちでブロックする
        // ことでデッドロックする(実機で検出)。ロックはdrainの間だけ保持し、
        // 実際のclose()はロックを解放した後に行う。
        let viewers: Vec<_> = {
            let mut viewers = self.viewers.write().await;
            self.closed.store(true, Ordering::SeqCst);
            self.viewer_count.store(0, Ordering::SeqCst);
            viewers.drain().collect()
        };
        for (_, pc) in viewers {
            let _ = pc.close().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ended_rooms_reject_late_viewers() {
        let state = crate::state::test_state().await;
        let room = Room::new("test".into());
        room.close_all_viewers().await;
        let pc = Arc::new(state.webrtc_api.new_peer_connection(Default::default()).await.unwrap());
        assert!(room.add_viewer(Uuid::new_v4(), pc.clone()).await.is_none());
        assert_eq!(room.viewer_count(), 0);
        pc.close().await.unwrap();
    }
}
