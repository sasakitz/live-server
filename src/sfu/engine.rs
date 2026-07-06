//! WebRTC の `API` (MediaEngine + InterceptorRegistry + SettingEngine) 初期化。
//!
//! モバイル(iOS Safari)を含めた互換性のため、コーデックは H.264 + Opus のみを登録する。
//! VP8/VP9は登録しない(=SDP上の選択肢に出さない)ことで、OBS/ブラウザ双方に
//! H.264(+Opus)を強制する。

use anyhow::{Context, Result};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264, MIME_TYPE_OPUS};
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::{APIBuilder, API};
use webrtc::ice::udp_network::{EphemeralUDP, UDPNetwork};
use webrtc::ice_transport::ice_candidate_type::RTCIceCandidateType;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType};
use webrtc::rtp_transceiver::RTCPFeedback;

use crate::config::WebRtcConfig;

/// 視聴者向けローカルトラック(Room)が名乗るH.264 capability。
/// OBS/ブラウザ側とのネゴシエーションで実際に選ばれた具体的なfmtpとは独立に、
/// この1種類の代表値でRoomのトラックを作成する(WHIP/WHEP双方でこの1種のみを登録するため)。
pub fn h264_capability() -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_H264.to_owned(),
        clock_rate: 90000,
        channels: 0,
        sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
            .to_owned(),
        rtcp_feedback: video_rtcp_feedback(),
    }
}

pub fn opus_capability() -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: 48000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
        rtcp_feedback: vec![],
    }
}

fn video_rtcp_feedback() -> Vec<RTCPFeedback> {
    vec![
        RTCPFeedback {
            typ: "goog-remb".to_owned(),
            parameter: "".to_owned(),
        },
        RTCPFeedback {
            typ: "transport-cc".to_owned(),
            parameter: "".to_owned(),
        },
        RTCPFeedback {
            typ: "ccm".to_owned(),
            parameter: "fir".to_owned(),
        },
        RTCPFeedback {
            typ: "nack".to_owned(),
            parameter: "".to_owned(),
        },
        RTCPFeedback {
            typ: "nack".to_owned(),
            parameter: "pli".to_owned(),
        },
    ]
}

/// H.264はfmtpの完全一致でネゴシエーションされるため、送信側(OBSやブラウザ)が
/// どのプロファイルをオファーしてもなるべく一致するよう、複数の代表的なバリアントを
/// 異なるペイロードタイプで登録しておく。
fn register_h264_variants(m: &mut MediaEngine) -> Result<()> {
    let variants: &[(u8, &str)] = &[
        (
            102,
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
        ),
        (
            104,
            "level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42e01f",
        ),
        (
            106,
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f",
        ),
        (
            108,
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=640c1f",
        ),
    ];

    for (payload_type, fmtp) in variants {
        m.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: MIME_TYPE_H264.to_owned(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line: (*fmtp).to_owned(),
                    rtcp_feedback: video_rtcp_feedback(),
                },
                payload_type: *payload_type,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;
    }
    Ok(())
}

fn build_media_engine() -> Result<MediaEngine> {
    let mut m = MediaEngine::default();
    register_h264_variants(&mut m).context("H.264コーデックの登録に失敗しました")?;
    m.register_codec(
        RTCRtpCodecParameters {
            capability: opus_capability(),
            payload_type: 111,
            ..Default::default()
        },
        RTPCodecType::Audio,
    )
    .context("Opusコーデックの登録に失敗しました")?;
    Ok(m)
}

/// SFU全体で共有する webrtc `API` を構築する。
pub fn build_api(cfg: &WebRtcConfig) -> Result<API> {
    let mut media_engine = build_media_engine()?;

    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut media_engine)
        .context("インターセプタの登録に失敗しました")?;

    let mut setting_engine = SettingEngine::default();
    let ephemeral = EphemeralUDP::new(cfg.udp_port_min, cfg.udp_port_max)
        .context("UDPポート範囲の設定に失敗しました(min > maxになっていないか確認)")?;
    setting_engine.set_udp_network(UDPNetwork::Ephemeral(ephemeral));
    // webrtc-iceの1:1 NATマッピングはIPv4/IPv6を別々に保持するため、両方を同時に
    // 渡しておけば、デュアルスタック環境でもそれぞれのアドレスファミリのICE候補に
    // 正しく適用される(例: ["203.0.113.10", "2001:db8::1"])。
    let public_ips: Vec<String> = cfg.public_ips.iter().map(|ip| ip.to_string()).collect();
    setting_engine.set_nat_1to1_ips(public_ips, RTCIceCandidateType::Host);
    // サーバは固定の公開IPを持つため、ブラウザ向けプライバシー機能であるmDNS候補は不要。
    // 無効化してICE candidate gatheringを単純化・高速化する。
    setting_engine.set_ice_multicast_dns_mode(webrtc::ice::mdns::MulticastDnsMode::Disabled);
    // 注意: set_network_types(vec![Udp4])のようにアドレスファミリを絞り込むと、
    // 環境によっては(マルチインターフェース+1:1 NATマッピングの組み合わせで)候補が
    // 1件も生成されずICE接続不能になる現象を実機検証で確認したため、意図的に
    // 設定していない。public_ipsに含めなかった側のアドレスファミリの
    // ローカルインターフェースがある場合、1:1マッピング対象が見つからない旨の
    // 警告ログが出るが、そのアドレスはマッピングなしでそのまま候補として
    // 使われるため実際の接続性には影響しない(無害な警告)。

    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_setting_engine(setting_engine)
        .build();

    Ok(api)
}

/// ブラウザ/OBS側のICEに使わせるSTUNサーバ一覧。WHIP/WHEP双方の`RTCConfiguration`で共有する。
pub fn ice_servers(cfg: &WebRtcConfig) -> Vec<RTCIceServer> {
    cfg.stun_servers
        .iter()
        .map(|url| RTCIceServer {
            urls: vec![url.clone()],
            ..Default::default()
        })
        .collect()
}
