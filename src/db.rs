use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

/// "sqlite://..." URLからファイルシステムパスを取り出す(ディレクトリを事前作成するため)。
/// 解釈できない形式(例: "sqlite::memory:")の場合はNoneを返す。
fn db_path_from_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("sqlite://")?;
    if rest.starts_with(':') {
        return None; // sqlite::memory: など
    }
    Some(PathBuf::from(rest))
}

pub async fn init_pool(url: &str) -> Result<SqlitePool> {
    if let Some(path) = db_path_from_url(url) {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("DBディレクトリの作成に失敗しました: {}", parent.display())
                })?;
            }
        }
    }

    let options = SqliteConnectOptions::from_str(url)
        .with_context(|| format!("不正なDB URLです: {url}"))?
        .create_if_missing(true)
        .foreign_keys(true);

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
