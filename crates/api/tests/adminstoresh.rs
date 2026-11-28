//! Admin stores remake, test H: the follower-notice
//! funnel guard. Before the remake, all three follower
//! funnels (new product, new post, flash sale) ignored the
//! store's status, so a SUSPENDED store's publishes pinged
//! every un-muted follower about pages that publicly 404.
//! Publishing itself stays allowed (the seller keeps
//! catalogue management while suspended) — only the
//! fan-out is silent, and nothing retro-fires when the
//! store is reactivated. Helpers are copied from the Wave
//! 9 test on purpose — integration test files stay
//! self-contained.
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

/// Create a product with an explicit status; returns the id.
async fn create_product_with_status(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    status: &str,
) -> String {
    let (code, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": 50_000,
            "stock": 5,
            "status": status
        })),
        Some(token),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Count one follower's new-product notices.
async fn follower_product_notices(pool: &PgPool, follower_phone: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications n \
         JOIN users u ON u.id = n.user_id \
         WHERE n.kind = 'store_new_product' AND u.phone = $1",
    )
    .bind(follower_phone)
    .fetch_one(pool)
    .await
    .expect("count follower notices")
}

/// PATCH a store's status as the admin with a raw body;
/// asserts success.
async fn patch_status_ok(app: &Router, admin: &str, store_id: &str, body: Value) {
    let uri = format!("/v1/admin/stores/{store_id}/status");
    let (status, response) = call(app, Method::PATCH, &uri, Some(body), Some(admin)).await;
    assert_eq!(status, StatusCode::OK, "store status: {response}");
}

#[tokio::test]
async fn adminstores_suspended_store_publishes_notify_nobody() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin stores remake test H");
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
    let state = AppState::new(Some(pool.clone()), "adminstoresh-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "hstores-shop").await;
    // The follower follows BEFORE the suspension.
    let follower = login(&app, "01855555555").await;
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/hstores-shop/follow",
        None,
        Some(&follower),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "follow: {body}");
    assert_eq!(body["following"], true, "follow: {body}");

    patch_status_ok(
        &app,
        &admin,
        &store,
        json!({ "status": "suspended", "note": "Funnel fixture" }),
    )
    .await;

    // Publish path 1: a product created already active
    // while the store is suspended. The create SUCCEEDS
    // (management access survives) but notifies nobody.
    create_product_with_status(&app, &seller, &store, "H Product One", "active").await;
    assert_eq!(follower_product_notices(&pool, "+8801855555555").await, 0);

    // Publish path 2: a draft moved INTO active while
    // still suspended — the second funnel call site.
    let draft = create_product_with_status(&app, &seller, &store, "H Product Two", "draft").await;
    let uri = format!("/v1/products/{draft}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "active" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "activate draft: {body}");
    assert_eq!(follower_product_notices(&pool, "+8801855555555").await, 0);

    // After reactivation the funnel fires again — exactly
    // once, for the NEW publish only (no retro-fire for
    // the two suspended-era products).
    patch_status_ok(&app, &admin, &store, json!({ "status": "active" })).await;
    let third = create_product_with_status(&app, &seller, &store, "H Product Three", "active").await;
    assert_eq!(follower_product_notices(&pool, "+8801855555555").await, 1);
    let payload: String = sqlx::query_scalar(
        "SELECT payload::text FROM notifications WHERE kind = 'store_new_product'",
    )
    .fetch_one(&pool)
    .await
    .expect("notice payload");
    assert!(payload.contains(&third), "payload names the new product: {payload}");
    let phone: String = sqlx::query_scalar(
        "SELECT phone FROM notifications WHERE kind = 'store_new_product'",
    )
    .fetch_one(&pool)
    .await
    .expect("notice phone");
    assert_eq!(phone, "+8801855555555");
}
