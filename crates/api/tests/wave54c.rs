//! Wave 54 integration tests (part 3): the delivery
//! estimate endpoint (the Wave 1 charge rule + the zone
//! day ranges, defaults and the admin override through
//! platform_settings), the store profile texts
//! (about / return_policy: branding round-trip, the
//! length cap, blank clears, public visibility), and the
//! admin store list's follower count.
//!
//! The platform_settings discipline: the settings test
//! DELETES the rows it wrote before finishing, because
//! every test file shares one database sequentially.
//!
//! The small helpers are copied from the Wave 9 test on
//! purpose, since integration test files stay
//! self-contained.
//!
//! Gated on `DATABASE_URL`: skipped (with a printed note)
//! when it is unset, so a plain local `cargo test` (no
//! database) still passes. CI provides a Postgres 16
//! service and sets `DATABASE_URL` for the rust job.

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

/// The shared setup: migrated, truncated database + a
/// dev-mode app. Returns the app and the pool (admin
/// promotion + settings cleanup go through SQL).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 54c integration tests");
            return None;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_images, product_variants, products, store_members, store_follows, \
         stores, otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave54-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// GET one delivery estimate; asserts 200 and returns the body.
async fn estimate(app: &Router, zone: &str, weight_grams: i64) -> Value {
    let uri = format!("/v1/delivery-estimate?zone={zone}&weight_grams={weight_grams}");
    let (status, body) = call(app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "estimate {zone}/{weight_grams}: {body}");
    body
}

#[tokio::test]
async fn wave54_delivery_estimate_defaults_and_admin_override() {
    let Some((app, pool)) = test_app().await else {
        return;
    };

    // Defaults: the Wave 1 charge rule (inside Dhaka
    // 6000 for the first kg + 2000 per started extra kg)
    // and the built-in day ranges.
    let body = estimate(&app, "inside_dhaka", 1000).await;
    assert_eq!(body["delivery_charge_poisha"], 6_000);
    assert_eq!(body["min_days"], 1);
    assert_eq!(body["max_days"], 2);
    let body = estimate(&app, "inside_dhaka", 2500).await;
    assert_eq!(body["delivery_charge_poisha"], 10_000, "3 started kg: {body}");
    let body = estimate(&app, "dhaka_suburb", 2000).await;
    assert_eq!(body["delivery_charge_poisha"], 13_000);
    assert_eq!(body["min_days"], 2);
    assert_eq!(body["max_days"], 3);
    let body = estimate(&app, "outside_dhaka", 1000).await;
    assert_eq!(body["delivery_charge_poisha"], 12_000);
    assert_eq!(body["min_days"], 3);
    assert_eq!(body["max_days"], 5);

    // Bad input is a bilingual 400, never a guess.
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/delivery-estimate?zone=chittagong&weight_grams=1000",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown zone");
    let (status, _) = call(&app, Method::GET, "/v1/delivery-estimate", None, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "missing zone");
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/delivery-estimate?zone=inside_dhaka&weight_grams=0",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "zero weight");

    // The admin override: settings carry the ranges, the
    // estimate follows them, and the charges do NOT move
    // (the ranges are display promises, not pricing).
    // Admin is SQL-only (the Wave 10 rule); require_admin
    // reads the flag fresh from the row, so the token
    // issued before promotion works.
    let admin = login(&app, "01710000031").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801710000031'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let (status, settings) = call(
        &app,
        Method::PUT,
        "/v1/admin/settings",
        Some(json!({ "delivery_days_inside_dhaka": [2, 4] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save day range: {settings}");
    assert_eq!(settings["delivery_days_inside_dhaka"], json!([2, 4]));
    let body = estimate(&app, "inside_dhaka", 1000).await;
    assert_eq!(body["min_days"], 2);
    assert_eq!(body["max_days"], 4);
    assert_eq!(body["delivery_charge_poisha"], 6_000, "charge unmoved: {body}");

    // An inverted range is refused.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/admin/settings",
        Some(json!({ "delivery_days_outside_dhaka": [5, 2] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "inverted range: {body}");

    // Cleanup: the shared database must not keep this
    // test's settings for the next file's tests.
    sqlx::query("DELETE FROM platform_settings")
        .execute(&pool)
        .await
        .expect("settings cleanup");
    let body = estimate(&app, "inside_dhaka", 1000).await;
    assert_eq!(body["min_days"], 1, "defaults restored: {body}");
    assert_eq!(body["max_days"], 2);
}

#[tokio::test]
async fn wave54_store_profile_texts_and_admin_follower_count() {
    let Some((app, pool)) = test_app().await else {
        return;
    };

    let seller = login(&app, "01710000041").await;
    let _store_id = create_store(&app, &seller, "wave54-profile").await;

    // Fresh store: both texts are null in the public view.
    let (status, view) = call(&app, Method::GET, "/v1/stores/wave54-profile", None, None).await;
    assert_eq!(status, StatusCode::OK, "public store: {view}");
    assert!(view["about"].is_null());
    assert!(view["return_policy"].is_null());

    // The branding write round-trips both texts, and the
    // public view shows them.
    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/branding",
        Some(json!({
            "about": "আমরা ২০১৫ সাল থেকে হাতে বোনা পণ্য বিক্রি করি।",
            "return_policy": "৭ দিনের মধ্যে অপরিবর্তিত পণ্য ফেরত নেওয়া হয়।"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "branding save: {saved}");
    assert_eq!(saved["about"], "আমরা ২০১৫ সাল থেকে হাতে বোনা পণ্য বিক্রি করি।");
    let (status, view) = call(&app, Method::GET, "/v1/stores/wave54-profile", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        view["return_policy"],
        "৭ দিনের মধ্যে অপরিবর্তিত পণ্য ফেরত নেওয়া হয়।"
    );

    // Over the 2000-character cap is a 400 and changes
    // nothing.
    let too_long = "a".repeat(2001);
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/branding",
        Some(json!({ "about": too_long })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "over-cap about: {body}");
    let (status, view) = call(&app, Method::GET, "/v1/stores/wave54-profile", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["about"], "আমরা ২০১৫ সাল থেকে হাতে বোনা পণ্য বিক্রি করি।");

    // A blank value clears the text (the form convention).
    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/branding",
        Some(json!({ "about": "   " })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "clear about: {saved}");
    assert!(saved["about"].is_null());

    // The admin store list carries the follower count.
    let buyer = login(&app, "01710000042").await;
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/stores/wave54-profile/follow",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "follow for admin count");
    let admin = login(&app, "01710000043").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801710000043'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let (status, stores) = call(
        &app,
        Method::GET,
        "/v1/admin/stores?q=wave54-profile",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin stores: {stores}");
    assert_eq!(stores["items"][0]["slug"], "wave54-profile");
    assert_eq!(stores["items"][0]["follower_count"], 1);
}
