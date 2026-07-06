use serde::Serialize;
use time::OffsetDateTime;

pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_BROADCASTER: &str = "broadcaster";

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub role: String,
    pub disabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == ROLE_ADMIN
    }
}

/// WHIPのBearer/URLシークレットとして使うランダムなstream_keyを生成する(64桁の16進文字列)。
/// UUIDv4を2つ連結することで、専用のrand依存を増やさずにOS RNG由来の乱数を得る。
pub fn generate_stream_key() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Stream {
    pub id: String,
    pub owner_user_id: i64,
    pub title: String,
    pub description: String,
    #[serde(skip_serializing)]
    pub stream_key: String,
    pub is_live: bool,
    pub public: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    pub peak_viewers: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Webhook {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub notify_start: bool,
    pub notify_stop: bool,
    pub enabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}
