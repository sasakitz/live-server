use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

fn default_udp_port_min() -> u16 {
    5000
}

fn default_udp_port_max() -> u16 {
    5200
}

fn default_stun_servers() -> Vec<String> {
    vec!["stun:stun.l.google.com:19302".to_string()]
}

fn default_data_dir() -> PathBuf {
    PathBuf::from(".")
}

fn default_true() -> bool {
    true
}

fn deserialize_public_ips<'de, D>(deserializer: D) -> std::result::Result<Vec<IpAddr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let ips = Vec::<IpAddr>::deserialize(deserializer)?;
    if ips.is_empty() {
        return Err(serde::de::Error::custom(
            "webrtc.public_ipsには少なくとも1つのIPアドレスを指定してください",
        ));
    }
    Ok(ips)
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub webrtc: WebRtcConfig,
    pub auth: AuthConfig,
    #[serde(default)]
    pub discord: DiscordConfig,
    pub db: DbConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// アプリが内部でバインドするアドレス(通常はCaddy等のリバースプロキシからのみ到達可能な127.0.0.1)
    pub bind: SocketAddr,
    /// 外部公開URLに使うホスト名(視聴URL生成やCookieのdomain判定に使用)
    pub public_host: String,
    /// 実行時ステート(セッション鍵など)を保存するディレクトリ
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// TLS終端リバースプロキシ(Caddy等)配下で動かす場合はtrue(既定)。
    /// セッションCookieにSecure属性を付与するかどうかを制御する。
    /// Caddyなしでhttp://localhostに直接繋いでローカル動作確認する場合のみfalseにする。
    #[serde(default = "default_true")]
    pub secure_cookies: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebRtcConfig {
    /// このサーバの公開IPアドレス。ICE候補として広告するために必須。
    /// IPv4とIPv6を両方持つ(デュアルスタック)サーバの場合は両方列挙できる
    /// (例: `["203.0.113.10", "2001:db8::1"]`)。各アドレスは自分と同じ
    /// アドレスファミリのICE候補にのみ1:1 NATマッピングとして適用される。
    #[serde(deserialize_with = "deserialize_public_ips")]
    pub public_ips: Vec<IpAddr>,
    /// public_ipsに加えてICE候補を収集するローカルNICアドレス。
    /// public_ipsがNICに直接付いている場合は指定不要。
    /// IPv4 NAT配下ではサーバ機のLAN側IPv4を追加する(例: `["192.168.1.7"]`)。
    /// public_ipsに指定したNIC直結のIPv6は、bind_ipsの指定にかかわらず収集対象になる。
    #[serde(default)]
    pub bind_ips: Vec<IpAddr>,
    /// WebRTCメディア(SRTP/DTLS)に使うUDPポート範囲の下限。ファイアウォールはこの範囲を開放する。
    #[serde(default = "default_udp_port_min")]
    pub udp_port_min: u16,
    /// UDPポート範囲の上限。
    #[serde(default = "default_udp_port_max")]
    pub udp_port_max: u16,
    /// ブラウザ側のICEに使わせるSTUNサーバ(`/api/client-config`経由で配布)。
    /// サーバ側はpublic_ipsで到達先を明示するため、STUNはブラウザ側にのみ配布する。
    /// IPv4/IPv6とも、サーバ側の候補はpublic_ipsとbind_ipsに基づいて収集する。
    #[serde(default = "default_stun_servers")]
    pub stun_servers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    /// trueの場合、配信ごとのpublic設定にかかわらず、未ログインでの全配信の視聴を許可する。
    #[serde(default)]
    pub public_viewing: bool,
    /// 初回起動時に存在しなければ作成する管理者アカウント。
    #[serde(default)]
    pub admin_bootstrap: Vec<AdminBootstrap>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdminBootstrap {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DiscordConfig {
    /// 個別のWebhook管理UIとは別に、常に通知したい既定のWebhook URL(任意)。
    #[serde(default)]
    pub default_webhook_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DbConfig {
    /// sqlx接続URL。例: sqlite:///var/lib/live-server/live.db
    pub url: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("設定ファイルの読み込みに失敗しました: {}", path.display()))?;
        let config: Config = toml::from_str(&raw)
            .with_context(|| format!("設定ファイルの解析に失敗しました: {}", path.display()))?;
        Ok(config)
    }
}
