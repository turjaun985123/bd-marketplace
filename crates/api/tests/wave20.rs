//! Wave 20 integration test: deployment readiness — the
//! `/health/ready` readiness probe (200 with a live database,
//! 503 without a pool, while `/health` liveness stays 200) and
//! the configured CORS layer (a preflight from the default
//! local dev origin is answered with that origin; an unlisted
//! origin gets no allow-origin header; a state built with
//! `with_cors_origins` answers exactly its own list — the
//! default is replaced, never merged).
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;

/// Send one JSON request through the router and return (status, body).
async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(value) => builder
            .header("Content-Type", "application/json")
            .body(Body::from(value.to_string()))
            .expect("request builds"),
        None => builder.body(Body::empty()).expect("request builds"),
    };
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router responds");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response is JSON")
    };
    (status, parsed)
}

/// Send one CORS preflight (OPTIONS with an Origin and a
/// requested method) and return the response status plus the
/// `access-control-allow-origin` header value, when present.
async fn preflight(app: &Router, origin: &str) -> (StatusCode, Option<String>) {
    let request = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/products")
        .header("Origin", origin)
        .header("Access-Control-Request-Method", "GET")
        .body(Body::empty())
        .expect("request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router responds");
    let status = response.status();
    let allow_origin = response
        .headers()
        .get("access-control-allow-origin")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (status, allow_origin)
}

#[tokio::test]
async fn wave20_readiness_and_cors() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set; skipping wave20 integration test");
            return;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("run migrations");

    // App 1: default state (default CORS origins) with a database.
    let app = build_app(AppState::new(
        Some(pool.clone()),
        "wave20-test-secret".to_string(),
        true,
    ));

    // Liveness and readiness both answer, and readiness is real.
    let (status, body) = call(&app, Method::GET, "/health", None).await;
    assert_eq!(status, StatusCode::OK, "health: {body}");
    assert_eq!(body["status"], "ok");
    let (status, body) = call(&app, Method::GET, "/health/ready", None).await;
    assert_eq!(status, StatusCode::OK, "ready with database: {body}");
    assert_eq!(body["status"], "ready", "ready body: {body}");
    assert_eq!(body["database"], "ok", "ready body: {body}");

    // The default origin (local Next.js dev) is allowed...
    let (status, allow_origin) = preflight(&app, "http://localhost:3000").await;
    assert_eq!(status, StatusCode::OK, "preflight from the dev origin");
    assert_eq!(
        allow_origin.as_deref(),
        Some("http://localhost:3000"),
        "the dev origin is echoed back"
    );
    // ...and an unlisted origin gets no allow-origin header at all.
    let (_, allow_origin) = preflight(&app, "https://evil.example.com").await;
    assert_eq!(
        allow_origin, None,
        "an unlisted origin must not be allowed"
    );

    // App 2: an explicit origin list REPLACES the default.
    let deployed = build_app(
        AppState::new(Some(pool.clone()), "wave20-test-secret".to_string(), true)
            .with_cors_origins(vec!["https://shop.example.com".to_string()]),
    );
    let (status, allow_origin) = preflight(&deployed, "https://shop.example.com").await;
    assert_eq!(status, StatusCode::OK, "preflight from the deployed origin");
    assert_eq!(
        allow_origin.as_deref(),
        Some("https://shop.example.com"),
        "the deployed origin is echoed back"
    );
    let (_, allow_origin) = preflight(&deployed, "http://localhost:3000").await;
    assert_eq!(
        allow_origin, None,
        "the default origin is replaced, not merged"
    );

    // App 3: no pool — liveness still answers, readiness is a 503.
    let db_less = build_app(AppState::new(None, "wave20-test-secret".to_string(), true));
    let (status, _) = call(&db_less, Method::GET, "/health", None).await;
    assert_eq!(status, StatusCode::OK, "liveness needs no database");
    let (status, body) = call(&db_less, Method::GET, "/health/ready", None).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "readiness without a pool: {body}"
    );
    assert_eq!(body["status"], "not ready", "not-ready body: {body}");
}
