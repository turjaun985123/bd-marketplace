//! Wave 18 integration test: storefront themes & store branding.
//! A seller sets branding on their own store (theme, brand colour
//! normalized to uppercase, logo / banner URLs, a Bangla
//! announcement, a description) and the PUBLIC store view shows
//! it; invalid values are 400s that change nothing; a second
//! seller's store is untouched by the first seller's write; a
//! fresh store carries the defaults (`classic`, nulls); a blank
//! string clears a nullable field while absent fields keep their
//! values (the codebase PATCH convention). Driven through the
//! real Axum router with tower oneshot; the small helpers are
//! copied from the Wave 13 test on purpose: integration test
//! files stay self-contained rather than sharing test code
//! across files.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so
//! a plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL` for
//! the rust job.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
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

/// PUT a branding body for the token's store; returns (status, body).
async fn put_branding(
    app: &Router,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    call(
        app,
        Method::PUT,
        "/v1/seller/store/branding",
        Some(body),
        token,
    )
    .await
}

/// GET the public store view for a slug; returns the body.
async fn get_store(app: &Router, slug: &str) -> Value {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/stores/{slug}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "get store {slug}: {body}");
    body
}

#[tokio::test]
async fn wave18_store_branding() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 18 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE platform_settings, sidra_suggestions, messages, conversations, \
         return_requests, coupon_redemptions, coupons, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         products, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "wave18-test-secret".to_string(), true);
    let app = build_app(state);

    let seller_a = login(&app, "01718181818").await;
    let seller_b = login(&app, "01828282828").await;
    let buyer = login(&app, "01938383838").await;
    create_store(&app, &seller_a, "wave18-a").await;
    create_store(&app, &seller_b, "wave18-b").await;

    // 1. A fresh store carries the branding defaults: the classic
    // theme and every branding field null.
    let fresh = get_store(&app, "wave18-a").await;
    assert_eq!(fresh["theme_key"], "classic", "default theme: {fresh}");
    assert!(fresh["brand_color"].is_null(), "default colour: {fresh}");
    assert!(fresh["logo_url"].is_null(), "default logo: {fresh}");
    assert!(fresh["banner_url"].is_null(), "default banner: {fresh}");
    assert!(fresh["announcement"].is_null(), "default announcement: {fresh}");

    // 2. The seller sets the full branding; the colour comes back
    // normalized to UPPERCASE, and the public store view shows
    // exactly what was stored.
    let (status, saved) = put_branding(
        &app,
        Some(&seller_a),
        json!({
            "theme_key": "bold",
            "brand_color": "#ff5722",
            "logo_url": "https://example.com/logo.png",
            "banner_url": "https://example.com/banner.jpg",
            "announcement": "ঈদের অফার চলছে!",
            "description": "সেরা পণ্যের দোকান"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set branding: {saved}");
    assert_eq!(saved["theme_key"], "bold");
    assert_eq!(saved["brand_color"], "#FF5722", "colour uppercased: {saved}");
    assert_eq!(saved["logo_url"], "https://example.com/logo.png");
    assert_eq!(saved["banner_url"], "https://example.com/banner.jpg");
    assert_eq!(saved["announcement"], "ঈদের অফার চলছে!");
    assert_eq!(saved["description"], "সেরা পণ্যের দোকান");

    let public = get_store(&app, "wave18-a").await;
    assert_eq!(public["theme_key"], "bold", "public theme: {public}");
    assert_eq!(public["brand_color"], "#FF5722");
    assert_eq!(public["logo_url"], "https://example.com/logo.png");
    assert_eq!(public["banner_url"], "https://example.com/banner.jpg");
    assert_eq!(public["announcement"], "ঈদের অফার চলছে!");

    // 3. Invalid values are 400s — and change nothing.
    for (body, why) in [
        (json!({ "brand_color": "red" }), "colour name"),
        (json!({ "brand_color": "#12345" }), "short colour"),
        (json!({ "theme_key": "fancy" }), "unknown theme"),
        (
            json!({ "announcement": "ক".repeat(121) }),
            "121-char announcement",
        ),
        (json!({ "logo_url": "ftp://example.com/x.png" }), "ftp URL"),
        (
            json!({ "banner_url": "example.com/banner.jpg" }),
            "schemeless URL",
        ),
    ] {
        let (status, body) = put_branding(&app, Some(&seller_a), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
    }
    let after_invalid = get_store(&app, "wave18-a").await;
    assert_eq!(after_invalid["theme_key"], "bold", "400s change nothing");
    assert_eq!(after_invalid["brand_color"], "#FF5722");
    assert_eq!(after_invalid["logo_url"], "https://example.com/logo.png");

    // 4. A partial write touches only its own field; absent
    // fields keep their stored values.
    let (status, saved) = put_branding(
        &app,
        Some(&seller_a),
        json!({ "announcement": "নতুন ঘোষণা" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "partial write: {saved}");
    assert_eq!(saved["announcement"], "নতুন ঘোষণা");
    assert_eq!(saved["theme_key"], "bold", "theme kept: {saved}");
    assert_eq!(saved["brand_color"], "#FF5722", "colour kept: {saved}");
    assert_eq!(saved["logo_url"], "https://example.com/logo.png");

    // 5. A blank string CLEARS a nullable field (the documented
    // convention); everything else survives the clear.
    let (status, saved) = put_branding(
        &app,
        Some(&seller_a),
        json!({ "logo_url": "" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "clear logo: {saved}");
    assert!(saved["logo_url"].is_null(), "logo cleared: {saved}");
    assert_eq!(saved["banner_url"], "https://example.com/banner.jpg");
    assert_eq!(saved["announcement"], "নতুন ঘোষণা");

    // 6. The other seller's store is untouched by all of the
    // above, and their own write lands only on their own store.
    let store_b = get_store(&app, "wave18-b").await;
    assert_eq!(store_b["theme_key"], "classic", "store B default: {store_b}");
    assert!(store_b["brand_color"].is_null());
    assert!(store_b["announcement"].is_null());

    let (status, saved_b) = put_branding(
        &app,
        Some(&seller_b),
        json!({ "theme_key": "minimal" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller B branding: {saved_b}");
    assert_eq!(saved_b["slug"], "wave18-b");
    assert_eq!(saved_b["theme_key"], "minimal");
    let store_a = get_store(&app, "wave18-a").await;
    assert_eq!(store_a["theme_key"], "bold", "store A unaffected: {store_a}");

    // 7. A user without a store gets the same 404 every other
    // seller-store endpoint gives, and an anonymous call is a 401.
    let (status, body) = put_branding(
        &app,
        Some(&buyer),
        json!({ "theme_key": "minimal" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "storeless user: {body}");
    let (status, _) = put_branding(&app, None, json!({ "theme_key": "minimal" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous write");
}
