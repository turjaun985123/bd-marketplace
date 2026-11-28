//! Admin stores remake, test C: the list search escapes
//! LIKE wildcards. The pre-remake query interpolated the
//! raw search text into the ILIKE pattern, so `%` matched
//! every store and `_` matched any single character; the
//! remake routes the text through
//! `domain::search_like_pattern`. Helpers are copied from
//! the Wave 9 test on purpose — integration test files
//! stay self-contained.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use courier::CouriersConfig;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

/// Send one JSON request through the router and return (status, body).
async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
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

/// Full dev-mode OTP login for a phone number; returns the JWT.
async fn login(app: &Router, phone: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/auth/request-otp",
        Some(json!({ "phone": phone })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "request-otp {phone}: {body}");
    let dev_otp = body["dev_otp"]
        .as_str()
        .expect("dev mode returns dev_otp")
        .to_string();
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/auth/verify-otp",
        Some(json!({ "phone": phone, "code": dev_otp })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "verify-otp {phone}: {body}");
    body["token"].as_str().expect("token issued").to_string()
}

/// Create a store for the token's user; returns the store id.
async fn create_store(app: &Router, token: &str, slug: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body["id"].as_str().expect("store id").to_string()
}

#[tokio::test]
async fn adminstores_search_escapes_like_wildcards() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin stores remake test C");
            return;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, return_requests, store_follows, \
         store_verification_events, store_verifications, platform_settings, \
         payouts, payout_accounts, review_replies, reviews, shipment_events, \
         shipments, payment_events, payments, order_items, sub_orders, orders, \
         cart_items, carts, products, api_keys, store_members, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "adminstoresc-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller_one = login(&app, "01722222222").await;
    create_store(&app, &seller_one, "cstores-pct").await;
    let seller_two = login(&app, "01733333333").await;
    create_store(&app, &seller_two, "cstores-two").await;
    let seller_three = login(&app, "01744444444").await;
    create_store(&app, &seller_three, "cstores-three").await;
    // One store whose NAME carries a literal percent sign
    // (the create helper names stores "Store {slug}", so
    // the rename is a fixture-level SQL edit).
    sqlx::query("UPDATE stores SET name = '100% Deals' WHERE slug = 'cstores-pct'")
        .execute(&pool)
        .await
        .expect("rename store");

    // Control: a plain substring finds all three, proving
    // the zeros below are escaping, not a broken filter.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/stores?q=cstores",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "control search: {body}");
    assert_eq!(body["total"], 3, "control search: {body}");

    // A bare % must match ONLY the store whose name
    // literally contains one — unescaped it matched all 3.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/stores?q=%25",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "percent search: {body}");
    assert_eq!(body["total"], 1, "percent search: {body}");
    assert_eq!(body["items"][0]["slug"], "cstores-pct");

    // A bare _ must match nothing — unescaped it matched
    // any single character, hence every store.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/stores?q=_",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "underscore search: {body}");
    assert_eq!(body["total"], 0, "underscore search: {body}");

    // The literal two-character text "100%" finds the one
    // store.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/stores?q=100%25",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "100% search: {body}");
    assert_eq!(body["total"], 1, "100% search: {body}");
    assert_eq!(body["items"][0]["slug"], "cstores-pct");
}
