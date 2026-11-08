use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

pub async fn init_pool(url: &str) -> Result<SqlitePool> {
    // SQLxと同じ解釈を使い、クエリ文字列やURLエンコードをパスに混入させない。
    let options = SqliteConnectOptions::from_str(url)
        .with_context(|| format!("不正なDB URLです: {url}"))?
        .create_if_missing(true)
        .foreign_keys(true);
    if let Some(parent) = options.get_filename().parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("DBディレクトリの作成に失敗しました: {}", parent.display())
            })?;
        }
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await
        .with_context(|| format!("DBへの接続に失敗しました: {url}"))?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("マイグレーションの実行に失敗しました")?;

    Ok(pool)
}

/// PeerConnectionはプロセスを越えて復元できないため、起動時に配信状態を戻す。
pub async fn reset_live_streams(pool: &SqlitePool) -> Result<()> {
    sqlx::query("UPDATE streams SET is_live = 0, started_at = NULL WHERE is_live = 1")
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_decoded_database_directory() {
        let root = std::env::temp_dir().join(format!("live-server-db-{}", uuid::Uuid::new_v4()));
        let url = format!("sqlite://{}/nested%20dir/live.db?mode=rwc", root.display());
        let pool = init_pool(&url).await.unwrap();
        assert!(root.join("nested dir/live.db").is_file());
        assert!(!root.join("nested%20dir").exists());
        pool.close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn resets_live_state_without_losing_peak_viewers() {
        let pool = init_pool("sqlite::memory:").await.unwrap();
        sqlx::query("INSERT INTO users (id, username, password_hash, role, created_at) VALUES (1, 'test', 'hash', 'admin', ?)")
            .bind(time::OffsetDateTime::now_utc())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO streams (id, owner_user_id, title, stream_key, is_live, started_at, peak_viewers, created_at) VALUES ('test', 1, 'test', 'key', 1, ?, 7, ?)")
            .bind(time::OffsetDateTime::now_utc())
            .bind(time::OffsetDateTime::now_utc())
            .execute(&pool)
            .await
            .unwrap();
        reset_live_streams(&pool).await.unwrap();
        reset_live_streams(&pool).await.unwrap();
        let row: (bool, Option<String>, i64) =
            sqlx::query_as("SELECT is_live, started_at, peak_viewers FROM streams")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row, (false, None, 7));
    }
}
