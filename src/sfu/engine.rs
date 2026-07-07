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

fn build_media_engine() -> Result<MediaEngine> {
    let mut m = MediaEngine::default();
    // H.264はfmtpの完全一致でネゴシエーションされ、かつこのSFUはRTPをトランスコード
    // せずそのまま中継する(TrackLocalStaticRTP)。そのため登録するプロファイルは
    // 必ず1種類のみにすること。以前はOBS/ブラウザ双方の互換性を広げる意図で
    // packetization-mode違いなどの複数バリアントを別ペイロードタイプで登録していたが、
    // WHIP側(OBS)とWHEP側(視聴者)が「たまたま異なるバリアント」を選んでしまうと、
    // 中継されるRTPペイロードの実体(NALのパケタイズ形式)と視聴側が受け取るSDP上の
    // fmtp(packetization-mode等)が食い違い、映像がデコードされない不具合が実機で
    // 発生した。1種類のみ登録すればOBS/視聴者は必ず同じプロファイルに合意するため、
    // この不整合は構造的に発生しない。
    m.register_codec(
        RTCRtpCodecParameters {
            capability: h264_capability(),
            payload_type: 102,
            ..Default::default()
        },
        RTPCodecType::Video,
    )
    .context("H.264コーデックの登録に失敗しました")?;
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
    // ローカル候補の収集をpublic_ipsに列挙したアドレスのみに絞る。Docker/Tailscale等の
    // 仮想インターフェースやIPv6リンクローカルアドレスが多数存在する環境では、それら
    // 全てに対してエフェメラルポート範囲(数百ポート)を1つずつ総当たりでbindしようと
    // した結果、本来使うべきアドレスに到達する前にICE gathering自体が中断されてしまい
    // 接続不能になる現象を実機で確認した。1:1 NATマッピング対象外のアドレスから
    // 候補を収集しても(公開IPとして広告できないため)どのみち使い物にならないので、
    // このフィルタで絞ることに副作用はない。
    // 注意: set_network_types(vec![Udp4])のようにアドレス"ファミリ"単位で絞り込むと、
    // 環境によっては候補が1件も生成されずICE接続不能になる現象を別途確認したため、
    // そちらは意図的に使っていない(アドレス単位のset_ip_filterのみを用いる)。
    // bind_ipsが未指定ならpublic_ipsをそのまま使う(public_ipsがNICに直接付いている
    // クラウドVM等の構成)。自宅サーバ等、public_ipsがルータ経由のグローバルIPで
    // NICには存在しない構成では、bind_ipsにサーバ機自身のLAN側IPを指定させる
    // (WebRtcConfig::bind_ipsのコメント参照)。
    let bind_ips = if cfg.bind_ips.is_empty() {
        &cfg.public_ips
    } else {
        &cfg.bind_ips
    };
    let allowed_ips: std::collections::HashSet<std::net::IpAddr> = bind_ips.iter().copied().collect();
    setting_engine.set_ip_filter(Box::new(move |ip| allowed_ips.contains(&ip)));

    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_setting_engine(setting_engine)
        .build();

    Ok(api)
}
