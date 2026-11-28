//! Wave 58 integration test: section analytics — the
//! anonymous beacon and the seller stats:
//! - views and clicks POSTed for a real section of the
//!   store's effective layout record (201, aggregate
//!   only — the request carries no identity at all);
//! - an event for a section the layout does not have is
//!   a 404, an unknown kind is a 400, an unknown store is
//!   a 404 — the log cannot be padded;
//! - the seller stats return one row per layout section
//!   (zeros included) with totals, today's counts and
//!   the trailing-window counts agreeing.
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

/// Find one row in a sections array by its section_id.
fn stats_by_id<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["sections"]
        .as_array()?
        .iter()
        .find(|section| section["section_id"].as_str() == Some(id))
}

#[tokio::test]
async fn wave58_section_events_and_seller_stats() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w58-events").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // The beacon is anonymous: two views and a click on
    // the default layout's hero.
    for (kind, times) in [("section_view", 2), ("section_click", 1)] {
        for _ in 0..times {
            let (status, body) = call(
                &app,
                Method::POST,
                "/v1/stores/w58-events/section-events",
                Some(json!({ "section_id": "hero", "kind": kind })),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "record {kind}: {body}");
            assert_eq!(body["recorded"], json!(true));
        }
    }

    // The log cannot be padded: unknown section, unknown
    // kind, unknown store.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/w58-events/section-events",
        Some(json!({ "section_id": "no-such-section", "kind": "section_view" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown section: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/w58-events/section-events",
        Some(json!({ "section_id": "hero", "kind": "section_hover" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown kind: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/w58-ghost/section-events",
        Some(json!({ "section_id": "hero", "kind": "section_view" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown store: {body}");

    // Exactly the three honest events were stored.
    let stored: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM storefront_events WHERE store_id = $1::uuid",
    )
    .bind(&store_id)
    .fetch_one(&pool)
    .await
    .expect("count events");
    assert_eq!(stored, 3);

    // The stats: one row per default-layout section; the
    // hero carries the counts at every horizon (the
    // events happened today, inside the window), the
    // rest are zeros.
    let (status, stats) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout/stats",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stats: {stats}");
    assert_eq!(stats["window_days"], json!(30));
    assert_eq!(
        stats["sections"].as_array().expect("stat rows").len(),
        9,
        "one row per layout section"
    );
    let hero = stats_by_id(&stats, "hero").expect("hero stats");
    assert_eq!(hero["type"], json!("hero"));
    assert_eq!(hero["views_total"], json!(2));
    assert_eq!(hero["clicks_total"], json!(1));
    assert_eq!(hero["views_today"], json!(2));
    assert_eq!(hero["clicks_today"], json!(1));
    assert_eq!(hero["views_window"], json!(2));
    assert_eq!(hero["clicks_window"], json!(1));
    let grid = stats_by_id(&stats, "all-products").expect("grid stats");
    assert_eq!(grid["views_total"], json!(0));
    assert_eq!(grid["clicks_total"], json!(0));
}
