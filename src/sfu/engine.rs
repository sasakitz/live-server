//! WebRTC の `API` (MediaEngine + InterceptorRegistry + SettingEngine) 初期化。
//!
//! モバイル(iOS Safari)を含めた互換性のため、コーデックは H.264 + Opus のみを登録する。
//! VP8/VP9は登録しない(=SDP上の選択肢に出さない)ことで、OBS/ブラウザ双方に
//! H.264(+Opus)を強制する。

use std::collections::HashSet;
use std::net::IpAddr;

use anyhow::{Context, Result};
use webrtc::api::interceptor_registry::{configure_twcc_sender_only, register_default_interceptors};
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264, MIME_TYPE_OPUS};
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::{APIBuilder, API};
use webrtc::dtls_transport::dtls_role::DTLSRole;
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

/// 公開IPがNICに直接付いている場合と、NAT配下のローカルIPの両方を収集対象にする。
/// IPv4のNAT用bind_ipsを指定しても、public_ipsのIPv6候補を除外しない。
fn candidate_ips(cfg: &WebRtcConfig) -> HashSet<IpAddr> {
    cfg.public_ips.iter().chain(&cfg.bind_ips).copied().collect()
}

/// 再送も含め、接続ごとのTWCC送信連番を付ける。
pub(super) fn register_interceptors(media_engine: &mut MediaEngine) -> Result<Registry> {
    // 既定ではTWCC受信側のみ。視聴者向けには接続ごとの送信連番も生成する。
    // NACK responderの内側になるよう先に登録し、再送時も新しい連番を付ける。
    let registry = configure_twcc_sender_only(Registry::new(), media_engine)
        .context("TWCC送信インターセプタの登録に失敗しました")?;
    register_default_interceptors(registry, media_engine)
        .context("インターセプタの登録に失敗しました")
}

/// SFU全体で共有する webrtc `API` を構築する。
pub fn build_api(cfg: &WebRtcConfig) -> Result<API> {
    let mut media_engine = build_media_engine()?;
    let registry = register_interceptors(&mut media_engine)?;

    let mut setting_engine = SettingEngine::default();
    // 配信サーバがDTLSの接続を受け、OBS/ブラウザがClientHelloを送る。
    // 既定のClient役ではwebrtc-dtlsが相手のIPv6アドレスをSNIに入れ、
    // libdatachannelのGnuTLSバックエンドに拒否されるため、Server役を明示する。
    setting_engine.set_answering_dtls_role(DTLSRole::Server)
        .context("DTLSサーバロールの設定に失敗しました")?;
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
    // 仮想NIC等から不要な候補を集めないよう、設定されたIPに絞る。
    // public_ipsのNIC直結アドレスと、NAT用bind_ipsを併用する。
    // ネットワーク種別は既定のUDP IPv4/IPv6両方を利用する。
    let allowed_ips = candidate_ips(cfg);
    setting_engine.set_ip_filter(Box::new(move |ip| allowed_ips.contains(&ip)));

    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_setting_engine(setting_engine)
        .build();

    Ok(api)
}

#[cfg(test)]
mod tests {
    use super::*;
    use webrtc::rtp_transceiver::RTCRtpTransceiverInit;
    use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;

    fn config(public_ips: &[&str], bind_ips: &[&str]) -> WebRtcConfig {
        WebRtcConfig {
            public_ips: public_ips.iter().map(|ip| ip.parse().unwrap()).collect(),
            bind_ips: bind_ips.iter().map(|ip| ip.parse().unwrap()).collect(),
            udp_port_min: 5000,
            udp_port_max: 5200,
            stun_servers: vec![],
        }
    }

    #[test]
    fn ipv4_nat_binding_preserves_public_ipv6_candidate() {
        let cfg = config(&["203.0.113.10", "2001:db8::10"], &["192.168.1.7"]);
        let ips = candidate_ips(&cfg);
        assert!(ips.contains(&"192.168.1.7".parse::<IpAddr>().unwrap()));
        assert!(ips.contains(&"2001:db8::10".parse::<IpAddr>().unwrap()));
        assert!(!ips.contains(&"2001:db8::99".parse::<IpAddr>().unwrap()));
        build_api(&cfg).unwrap();
    }

    #[test]
    fn ipv6_only_configuration_is_supported() {
        let cfg = config(&["2001:db8::10"], &[]);
        let ips = candidate_ips(&cfg);
        assert_eq!(ips.len(), 1);
        assert!(ips.contains(&"2001:db8::10".parse::<IpAddr>().unwrap()));
        build_api(&cfg).unwrap();
    }

    #[tokio::test]
    async fn answers_accept_dtls_connections_from_publishers_and_viewers() {
        let client_api = APIBuilder::new()
            .with_media_engine(build_media_engine().unwrap())
            .build();
        let server_api = build_api(&config(&["2001:db8::10"], &[])).unwrap();
        for direction in [RTCRtpTransceiverDirection::Sendonly, RTCRtpTransceiverDirection::Recvonly] {
            let client = client_api.new_peer_connection(Default::default()).await.unwrap();
            client.add_transceiver_from_kind(RTPCodecType::Video, Some(RTCRtpTransceiverInit {
                direction,
                send_encodings: vec![],
            })).await.unwrap();
            let offer = client.create_offer(None).await.unwrap();
            assert!(offer.sdp.contains("a=setup:actpass"));
            let server = server_api.new_peer_connection(Default::default()).await.unwrap();
            server.set_remote_description(offer).await.unwrap();
            let answer = server.create_answer(None).await.unwrap();
            assert!(answer.sdp.contains("a=setup:passive"));
            assert!(!answer.sdp.contains("a=setup:active"));
            server.close().await.unwrap();
            client.close().await.unwrap();
        }
    }
}
