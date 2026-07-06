//! フロントエンド一式(frontend/)をバイナリに埋め込み、静的配信する。
//! マルチページ構成(ビルド不要のバニラJS/CSS)なので、既知のページパスを
//! 対応するhtmlファイルへ手動でマッピングする(SPAのような汎用フォールバックはしない)。

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "frontend/"]
struct Frontend;

fn page_alias(path: &str) -> Option<&'static str> {
    match path {
        "/" => Some("index.html"),
        "/login" => Some("login.html"),
        "/dashboard" => Some("dashboard.html"),
        "/admin" => Some("admin.html"),
        p if p.starts_with("/watch/") => Some("watch.html"),
        _ => None,
    }
}

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path();
    let asset_path = match page_alias(path) {
        Some(alias) => alias.to_string(),
        None => path.trim_start_matches('/').to_string(),
    };

    match Frontend::get(&asset_path) {
        Some(file) => {
            let mime = mime_guess::from_path(&asset_path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.essence_str().to_string())],
                file.data.into_owned(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "404 Not Found").into_response(),
    }
}
