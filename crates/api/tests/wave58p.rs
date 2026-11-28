//! Wave 58 integration test: the stats horizons —
//! totals, today, and the trailing 30-day window are
//! three different numbers, computed on Asia/Dhaka
//! calendar days:
//! - events backdated by SQL to 40 days ago count in the
//!   TOTAL but in neither the window nor today;
//! - events backdated to 10 days ago count in the total
//!   and the window, but not today;
//! - a fresh event counts everywhere.
//! (The events are inserted directly because the beacon
//! can only ever write "now" — ageing is a database
//! fact, staged here the way the analytics tests stage
//! theirs.)
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

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

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 58 integration tests");
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
         buyer_addresses, bulk_imports, store_layouts, storefront_events, \
         store_collections, store_collection_products, \
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
    let state = AppState::new(Some(pool.clone()), "wave58-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// POST a store for the token's user; returns the store view.
async fn create_store(app: &Router, token: &str, slug: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body
}

/// Insert one backdated section event directly (the
/// beacon only writes "now"; ageing is staged in SQL).
async fn insert_event_days_ago(pool: &PgPool, store_id: &str, days: i64) {
    sqlx::query(
        "INSERT INTO storefront_events (store_id, section_id, kind, created_at) \
         VALUES ($1::uuid, 'hero', 'section_view', now() - ($2::int * interval '1 day'))",
    )
    .bind(store_id)
    .bind(days)
    .execute(pool)
    .await
    .expect("insert backdated event");
}

#[tokio::test]
async fn wave58_stats_horizons_total_window_today() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w58-horizons").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // Three hero views at three ages, plus one fresh
    // click through the real beacon.
    insert_event_days_ago(&pool, &store_id, 40).await;
    insert_event_days_ago(&pool, &store_id, 40).await;
    insert_event_days_ago(&pool, &store_id, 10).await;
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/w58-horizons/section-events",
        Some(json!({ "section_id": "hero", "kind": "section_view" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "fresh view: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/w58-horizons/section-events",
        Some(json!({ "section_id": "hero", "kind": "section_click" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "fresh click: {body}");

    let (status, stats) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout/stats",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stats: {stats}");
    let hero = stats["sections"]
        .as_array()
        .expect("stat rows")
        .iter()
        .find(|row| row["section_id"] == json!("hero"))
        .expect("hero stats row")
        .clone();
    // Views: 2 at 40 days (total only), 1 at 10 days
    // (total + window), 1 fresh (all three horizons).
    assert_eq!(hero["views_total"], json!(4));
    assert_eq!(hero["views_window"], json!(2));
    assert_eq!(hero["views_today"], json!(1));
    assert_eq!(hero["clicks_total"], json!(1));
    assert_eq!(hero["clicks_window"], json!(1));
    assert_eq!(hero["clicks_today"], json!(1));
}
