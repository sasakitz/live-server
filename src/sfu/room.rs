use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::RwLock;
use uuid::Uuid;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;

use crate::sfu::engine::{h264_capability, opus_capability};

pub type ViewerId = Uuid;

/// 1配信(1 stream_id)に対応するSFUの中核。
/// WHIPで受信したRTPをそのままローカルトラックへ書き込み、WHEPで接続した
/// 各視聴者PeerConnectionのsenderがそのローカルトラックを送出する(ファンアウト)。
pub struct Room {
    pub stream_id: String,
    pub video_track: Arc<TrackLocalStaticRTP>,
    pub audio_track: Arc<TrackLocalStaticRTP>,
    pub viewers: RwLock<HashMap<ViewerId, Arc<RTCPeerConnection>>>,
    pub publisher_pc: RwLock<Option<Arc<RTCPeerConnection>>>,
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
            video_track,
            audio_track,
            viewers: RwLock::new(HashMap::new()),
            publisher_pc: RwLock::new(None),
            thumbnail: RwLock::new(None),
            viewer_count: AtomicUsize::new(0),
            peak_viewers: AtomicUsize::new(0),
        }
    }

    pub async fn add_viewer(&self, id: ViewerId, pc: Arc<RTCPeerConnection>) -> usize {
        let mut viewers = self.viewers.write().await;
        viewers.insert(id, pc);
        let count = viewers.len();
        drop(viewers);
        self.viewer_count.store(count, Ordering::SeqCst);
        self.peak_viewers.fetch_max(count, Ordering::SeqCst);
        count
    }

    /// 視聴者を削除する。実際に存在した場合のみ `Some((更新後の人数, そのPeerConnection))` を返す。
    /// 呼び出し元は、自然切断由来(既にPC状態が変化している)なら返ってきたPCを再度closeする必要はないが、
    /// 明示的なDELETE(視聴終了)由来ならPCを閉じる責任を持つ。
    pub async fn remove_viewer(&self, id: &ViewerId) -> Option<(usize, Arc<RTCPeerConnection>)> {
        let mut viewers = self.viewers.write().await;
        let pc = viewers.remove(id)?;
        let count = viewers.len();
        drop(viewers);
        self.viewer_count.store(count, Ordering::SeqCst);
        Some((count, pc))
    }

    pub fn viewer_count(&self) -> usize {
        self.viewer_count.load(Ordering::SeqCst)
    }

    pub fn peak_viewers(&self) -> usize {
        self.peak_viewers.load(Ordering::SeqCst)
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
            viewers.drain().collect()
        };
        self.viewer_count.store(0, Ordering::SeqCst);
        for (_, pc) in viewers {
            let _ = pc.close().await;
        }
    }
}
