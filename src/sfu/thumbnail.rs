//! 配信中サムネイル生成。
//!
//! このSFUはRTPをデコードせずそのまま中継するだけなので(`engine.rs`参照)、
//! 常時デコードするパイプラインは持たない。代わりに、公開者からのVideo RTPを
//! 軽量にAnnex-B形式へ再構成し、キーフレームを含む短い断片が集まった時だけ
//! `ffmpeg` をサブプロセスとして都度起動して1枚だけJPEGにデコードさせる。

use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use rtp::codecs::h264::H264Packet;
use rtp::packet::Packet;
use rtp::packetizer::Depacketizer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// 前回のキャプチャ成功から、次のキャプチャを試みるまでの最短間隔。
const CAPTURE_INTERVAL: Duration = Duration::from_secs(10);
/// キーフレーム検出後、何アクセスユニット分をffmpegに渡すか。
/// SPS/PPS+IDRのみでもデコード可能なはずだが、余裕を持たせて数フレーム分渡す。
const CAPTURE_AU_COUNT: u32 = 5;
/// 異常なストリームでバッファが際限なく伸びないための安全弁。
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

/// H.264 RTPストリームからサムネイル用のAnnex-B断片を切り出す状態機械。
/// 1配信(1 Room)につき1インスタンスを、Video RTP受信ループの中で使う。
pub struct ThumbnailCapturer {
    depacketizer: H264Packet,
    au_buffer: BytesMut,
    au_input_bytes: usize,
    discarding_au: bool,
    capture_buffer: BytesMut,
    capturing: bool,
    au_count_since_start: u32,
    last_capture_at: Option<Instant>,
    /// 直近に見たSPS/PPS(Annex-Bのstart code込み)。エンコーダによっては
    /// SPS/PPSをキーフレームと同じアクセスユニットではなく、その直前の
    /// アクセスユニットで送ってくることがある(実機で確認: ffmpegのWHIP
    /// 出力はSPS/PPSを次のNAL(AUD/SEI等)とSTAP-Aで束ねて送るため、
    /// キーフレーム自身のアクセスユニットにはSPS/PPSが含まれない)。
    /// そのままキーフレームのアクセスユニットだけをffmpegに渡すとパラメータ
    /// セット欠落でデコードに失敗するため、キャプチャ開始時に直近のSPS/PPSを
    /// 先頭へ補って渡す。
    last_sps: Option<Bytes>,
    last_pps: Option<Bytes>,
}

impl ThumbnailCapturer {
    pub fn new() -> Self {
        ThumbnailCapturer {
            depacketizer: H264Packet::default(),
            au_buffer: BytesMut::new(),
            au_input_bytes: 0,
            discarding_au: false,
            capture_buffer: BytesMut::new(),
            capturing: false,
            au_count_since_start: 0,
            last_capture_at: None,
            last_sps: None,
            last_pps: None,
        }
    }

    /// 1本のRTPパケットを取り込む。アクセスユニット(1フレーム分のNAL群)が
    /// マーカービットで完結し、かつキャプチャ条件(前回から一定時間経過後の
    /// 最初のキーフレーム)を満たした場合のみ、ffmpegに渡すAnnex-B断片を返す。
    pub fn push(&mut self, packet: &Packet) -> Option<Bytes> {
        self.au_input_bytes = self.au_input_bytes.saturating_add(packet.payload.len());
        if !self.discarding_au && self.au_input_bytes <= MAX_CAPTURE_BYTES {
            if let Ok(payload) = self.depacketizer.depacketize(&packet.payload) {
                if self.au_buffer.len().saturating_add(payload.len()) <= MAX_CAPTURE_BYTES {
                    self.au_buffer.extend_from_slice(&payload);
                } else {
                    self.discarding_au = true;
                }
            }
        } else {
            self.discarding_au = true;
        }
        if self.discarding_au {
            // マーカービット欠落やFU-A断片の連続でも、内部バッファを無制限に増やさない。
            self.depacketizer = H264Packet::default();
            self.au_buffer.clear();
            self.capture_buffer.clear();
            self.capturing = false;
            if packet.header.marker {
                self.discarding_au = false;
                self.au_input_bytes = 0;
            }
            return None;
        }
        if !packet.header.marker { return None; }
        self.au_input_bytes = 0;
        // FU-Aの終端欠落があっても、未完のNALを次のアクセスユニットへ持ち越さない。
        self.depacketizer = H264Packet::default();
        let au = self.au_buffer.split().freeze();
        if au.is_empty() {
            return None;
        }

        let nals = scan_nals(&au);
        for nal in &nals {
            match nal.nal_type {
                7 => self.last_sps = Some(au.slice(nal.range.clone())),
                8 => self.last_pps = Some(au.slice(nal.range.clone())),
                _ => {}
            }
        }

        if !self.capturing {
            let due = self
                .last_capture_at
                .map(|t| t.elapsed() >= CAPTURE_INTERVAL)
                .unwrap_or(true);
            if due && nals.iter().any(|n| n.nal_type == 5) {
                self.capturing = true;
                self.capture_buffer.clear();
                self.au_count_since_start = 0;
                // このアクセスユニット自体にSPS/PPSが含まれていない構成でも
                // デコードできるよう、直近のSPS/PPSを先頭に補っておく
                // (二重になっても無害: デコーダは再宣言を無視する)。
                if let Some(sps) = &self.last_sps {
                    self.capture_buffer.extend_from_slice(sps);
                }
                if let Some(pps) = &self.last_pps {
                    self.capture_buffer.extend_from_slice(pps);
                }
            }
        }
        if !self.capturing {
            return None;
        }

        if self.capture_buffer.len().saturating_add(au.len()) > MAX_CAPTURE_BYTES {
            self.capture_buffer.clear();
            self.capturing = false;
            self.last_capture_at = Some(Instant::now());
            return None;
        }
        self.capture_buffer.extend_from_slice(&au);
        self.au_count_since_start += 1;
        if self.au_count_since_start < CAPTURE_AU_COUNT && self.capture_buffer.len() < MAX_CAPTURE_BYTES {
            return None;
        }

        self.capturing = false;
        self.last_capture_at = Some(Instant::now());
        Some(self.capture_buffer.split().freeze())
    }
}

struct NalRef {
    range: std::ops::Range<usize>,
    nal_type: u8,
}

/// `au`(depacketizeで再構成したAnnex-B断片)中の各NALユニットを走査する。
/// このSFUが自前で挿入するstart codeは常に4バイト(`ANNEXB_NALUSTART_CODE`)だが、
/// 念のため3バイト形式も許容する。
fn scan_nals(au: &[u8]) -> Vec<NalRef> {
    let mut starts = vec![];
    let mut i = 0;
    while i + 2 < au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            let code_begin = if i > 0 && au[i - 1] == 0 { i - 1 } else { i };
            starts.push(code_begin);
            i += 3;
        } else {
            i += 1;
        }
    }

    let mut out = Vec::with_capacity(starts.len());
    for (idx, &code_begin) in starts.iter().enumerate() {
        let content_start = code_begin + if au.get(code_begin + 2) == Some(&1) { 3 } else { 4 };
        let Some(&nal_byte) = au.get(content_start) else { continue };
        let end = starts.get(idx + 1).copied().unwrap_or(au.len());
        out.push(NalRef {
            range: code_begin..end,
            nal_type: nal_byte & 0x1F,
        });
    }
    out
}

/// Annex-B H.264断片を`ffmpeg`でデコードし、JPEGバイト列を返す。
/// `ffmpeg`が未インストール・デコード失敗の場合は`None`(サムネイル生成をスキップするだけで、
/// 配信自体には影響しない)。
pub async fn encode_jpeg(annexb: Bytes) -> Option<Bytes> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "h264",
            "-i",
            "pipe:0",
            "-frames:v",
            "1",
            "-vf",
            "scale=320:-2",
            "-f",
            "mjpeg",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .inspect_err(|e| tracing::warn!(error = ?e, "ffmpegの起動に失敗しました(サムネイル生成をスキップ)"))
        .ok()?;

    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;

    // 入出力を並行して処理し、ffmpegの停止・不正入力でタスクが残り続けないようにする。
    tokio::time::timeout(Duration::from_secs(10), async {
        let write = async {
            let result = stdin.write_all(&annexb).await;
            drop(stdin);
            result
        };
        let mut out = Vec::new();
        let (_write_result, read_result) = tokio::join!(write, stdout.read_to_end(&mut out));
        let status = child.wait().await.ok()?;
        if read_result.is_err() || !status.success() || out.is_empty() {
            return None;
        }
        Some(Bytes::from(out))
    }).await.ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_rtp_cannot_grow_access_unit_buffers_forever() {
        let mut capturer = ThumbnailCapturer::new();
        let mut packet = Packet::default();
        packet.payload = Bytes::from(vec![0x61; 65536]);
        for _ in 0..100 { assert!(capturer.push(&packet).is_none()); }
        assert!(capturer.au_buffer.len() <= MAX_CAPTURE_BYTES);
        assert!(capturer.discarding_au);
        packet.header.marker = true;
        capturer.push(&packet);
        assert!(!capturer.discarding_au);
        assert!(capturer.au_buffer.is_empty());
    }
}
