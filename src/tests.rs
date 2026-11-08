//! 実際のRouterとセッション層を通すHTTP回帰テスト(ネットワーク待受は不要)。
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::Router;
use tower::ServiceExt;
use tower_sessions::SessionManagerLayer;

use crate::state::AppState;

fn app(state: AppState) -> Router {
    let sessions = SessionManagerLayer::new(crate::auth::session_store::SqliteSessionStore::new(state.db.clone()))
        .with_private(tower_sessions::cookie::Key::generate())
        .with_secure(false);
    Router::new()
        .nest("/api/whip", crate::sfu::whip::router())
        .nest("/api/whep", crate::sfu::whep::router())
        .nest("/api", crate::api::router())
        .fallback(crate::assets::static_handler)
        .layer(sessions).with_state(state)
}

async fn request(app: &Router, method: &str, path: &str, cookie: Option<&str>, body: &str) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(path).header("content-type", "application/json");
    if let Some(cookie) = cookie { builder = builder.header("cookie", cookie); }
    app.clone().oneshot(builder.body(Body::from(body.to_owned())).unwrap()).await.unwrap()
}

async fn login(app: &Router, state: &AppState, username: &str) -> String {
    let hash = crate::auth::hash_password_async("test-password".into()).await.unwrap();
    sqlx::query("UPDATE users SET password_hash = ? WHERE username = ?")
        .bind(hash).bind(username).execute(&state.db).await.unwrap();
    let response = request(app, "POST", "/api/login", None,
        &format!(r#"{{"username":"{username}","password":"test-password"}}"#)).await;
    assert_eq!(response.status(), StatusCode::OK);
    response.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_owned()
}

#[tokio::test]
async fn canonical_routes_and_public_metadata_work_without_trailing_slash() {
    let state = crate::state::test_state().await;
    let router = app(state);
    for path in ["/", "/login", "/dashboard", "/admin", "/watch/test", "/css/styles.css", "/js/watch.js", "/api/client-config", "/api/streams"] {
        assert_eq!(request(&router, "GET", path, None, "").await.status(), StatusCode::OK, "{path}");
    }
    let response = request(&router, "GET", "/api/streams/test", None, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 10000).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("stream_key").is_none());
    assert_eq!(request(&router, "GET", "/api/admin/users", None, "").await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn authorization_logout_and_disabled_accounts_apply_to_existing_sessions() {
    let state = crate::state::test_state().await;
    let router = app(state.clone());
    let cookie = login(&router, &state, "caster").await;
    assert_eq!(request(&router, "GET", "/api/admin/users", Some(&cookie), "").await.status(), StatusCode::FORBIDDEN);
    assert_eq!(request(&router, "GET", "/api/my/streams", Some(&cookie), "").await.status(), StatusCode::OK);
    sqlx::query("UPDATE users SET disabled = 1 WHERE id = 2").execute(&state.db).await.unwrap();
    assert_eq!(request(&router, "GET", "/api/my/streams", Some(&cookie), "").await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(request(&router, "POST", "/api/whip/secret", None, "invalid SDP").await.status(), StatusCode::UNAUTHORIZED);
    sqlx::query("UPDATE users SET disabled = 0 WHERE id = 2").execute(&state.db).await.unwrap();
    request(&router, "POST", "/api/logout", Some(&cookie), "").await;
    assert_eq!(request(&router, "GET", "/api/me", Some(&cookie), "").await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn management_routes_reject_pending_key_rotation_and_clean_pending_deleted_users() {
    let state = crate::state::test_state().await;
    let router = app(state.clone());
    let cookie = login(&router, &state, "admin").await;
    assert_eq!(request(&router, "POST", "/api/admin/users", Some(&cookie),
        r#"{"username":"new-user","password":"あいう","role":"broadcaster"}"#).await.status(), StatusCode::BAD_REQUEST);
    crate::sfu::lifecycle::register_room(&state, "secret", std::sync::Arc::new(crate::sfu::room::Room::new("test".into()))).await.unwrap();
    assert_eq!(request(&router, "POST", "/api/admin/streams/test/regenerate-key", Some(&cookie), "").await.status(), StatusCode::CONFLICT);
    assert_eq!(request(&router, "DELETE", "/api/admin/users/2", Some(&cookie), "").await.status(), StatusCode::NO_CONTENT);
    assert!(state.rooms.read().await.is_empty());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM streams").fetch_one(&state.db).await.unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn private_metadata_requires_login_and_invalid_sdp_does_not_reserve_room() {
    let state = crate::state::test_state().await;
    let router = app(state.clone());
    sqlx::query("UPDATE streams SET public = 0").execute(&state.db).await.unwrap();
    assert_eq!(request(&router, "GET", "/api/streams/test", None, "").await.status(), StatusCode::UNAUTHORIZED);
    for _ in 0..2 {
        assert_eq!(request(&router, "POST", "/api/whip/secret", None, "invalid SDP").await.status(), StatusCode::BAD_REQUEST);
        assert!(state.rooms.read().await.is_empty());
    }
    assert_ne!(request(&router, "DELETE", "/api/whip/resource/test", None, "").await.status(), StatusCode::NO_CONTENT);
}
