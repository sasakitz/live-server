//! tower-sessions 用の自前SQLiteセッションストア。
//!
//! `tower-sessions-sqlx-store` は sqlx 0.8系に依存しており、本プロジェクトが使う
//! sqlx 0.9系のプールとは型が異なるため利用できない(SQLiteドライバの二重リンクも避けたい)。
//! セッションの永続化ロジック自体は単純なCRUDなので、本プロジェクトのプールで完結する
//! 実装を自前で用意する。

use async_trait::async_trait;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use tower_sessions::session::{Id, Record};
use tower_sessions::session_store::{self, SessionStore};

#[derive(Debug, Clone)]
pub struct SqliteSessionStore {
    pool: SqlitePool,
}

impl SqliteSessionStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 期限切れセッションの掃除。定期実行タスクから呼ぶ。
    pub async fn delete_expired(&self) -> Result<u64, sqlx::Error> {
        let result = sqlx::query("DELETE FROM sessions WHERE expiry_date <= ?")
            .bind(OffsetDateTime::now_utc())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

fn backend_err(e: sqlx::Error) -> session_store::Error {
    session_store::Error::Backend(e.to_string())
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        // IDの衝突は128bitランダム値のため実質発生しないが、規約上リトライ可能にしておく。
        loop {
            let data =
                serde_json::to_vec(&record.data).map_err(|e| session_store::Error::Encode(e.to_string()))?;
            let result = sqlx::query(
                "INSERT INTO sessions (id, data, expiry_date) VALUES (?, ?, ?) \
                 ON CONFLICT(id) DO NOTHING",
            )
            .bind(record.id.to_string())
            .bind(data)
            .bind(record.expiry_date)
            .execute(&self.pool)
            .await
            .map_err(backend_err)?;

            if result.rows_affected() > 0 {
                return Ok(());
            }
            record.id = Id::default();
        }
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        let data =
            serde_json::to_vec(&record.data).map_err(|e| session_store::Error::Encode(e.to_string()))?;
        sqlx::query(
            "INSERT INTO sessions (id, data, expiry_date) VALUES (?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET data = excluded.data, expiry_date = excluded.expiry_date",
        )
        .bind(record.id.to_string())
        .bind(data)
        .bind(record.expiry_date)
        .execute(&self.pool)
        .await
        .map_err(backend_err)?;
        Ok(())
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        let row: Option<(Vec<u8>, OffsetDateTime)> = sqlx::query_as(
            "SELECT data, expiry_date FROM sessions WHERE id = ? AND expiry_date > ?",
        )
        .bind(session_id.to_string())
        .bind(OffsetDateTime::now_utc())
        .fetch_optional(&self.pool)
        .await
        .map_err(backend_err)?;

        let Some((data, expiry_date)) = row else {
            return Ok(None);
        };
        let data = serde_json::from_slice(&data).map_err(|e| session_store::Error::Decode(e.to_string()))?;
        Ok(Some(Record {
            id: *session_id,
            data,
            expiry_date,
        }))
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend_err)?;
        Ok(())
    }
}
