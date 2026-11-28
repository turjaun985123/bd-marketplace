//! Wave 58 integration test: visibility — disabled
//! sections and sections outside their scheduling window
//! are dropped from the PUBLIC layout but kept, intact,
//! in the seller's own view:
//! - a live rich-text section renders;
//! - a disabled one, a not-yet-started one (window in
//!   2999), and an already-ended one (window in 2020) do
//!   not;
//! - the seller read returns all five sections with the
//!   disabled flag and the RFC 3339 windows untouched.
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

/// The ids of a layout body's sections, in order.
fn section_ids(body: &Value) -> Vec<String> {
    body["sections"]
        .as_array()
        .expect("sections array")
        .iter()
        .map(|section| section["id"].as_str().expect("section id").to_string())
        .collect()
}

/// Find one section in a layout body by id.
fn section_by_id<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["sections"]
        .as_array()?
        .iter()
        .find(|section| section["id"].as_str() == Some(id))
}

#[tokio::test]
async fn wave58_disabled_and_scheduled_sections_hide_publicly_only() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    create_store(&app, &seller, "w58-visibility").await;

    let rich = |id: &str, body_text: &str| {
        json!({
            "id": id,
            "type": "rich_text",
            "config": { "body": body_text }
        })
    };
    let mut future = rich("future-note", "ভবিষ্যতের অফার");
    future["starts_at"] = json!("2999-01-01T00:00:00Z");
    future["ends_at"] = json!("2999-01-02T00:00:00Z");
    let mut past = rich("past-note", "শেষ হয়ে যাওয়া অফার");
    past["starts_at"] = json!("2020-01-01T00:00:00Z");
    past["ends_at"] = json!("2020-01-02T00:00:00Z");
    let mut hidden = rich("hidden-note", "লুকানো নোট");
    hidden["enabled"] = json!(false);

    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "sections": [
                rich("live-note", "চলমান নোট"),
                hidden,
                future,
                past,
                { "id": "all-products", "type": "product_grid", "config": {} }
            ]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put layout: {saved}");

    // Publicly: only the live note and the grid survive.
    let (status, public) = call(
        &app,
        Method::GET,
        "/v1/stores/w58-visibility/layout",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(
        section_ids(&public),
        vec!["live-note", "all-products"],
        "disabled + out-of-window dropped: {public}"
    );
    let live = section_by_id(&public, "live-note").expect("live section");
    assert_eq!(live["config"]["body"], json!("চলমান নোট"));

    // The seller still sees the whole document, flags and
    // windows intact.
    let (status, fetched) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller layout: {fetched}");
    assert_eq!(
        section_ids(&fetched),
        vec!["live-note", "hidden-note", "future-note", "past-note", "all-products"]
    );
    let hidden = section_by_id(&fetched, "hidden-note").expect("hidden kept");
    assert_eq!(hidden["enabled"], json!(false));
    let future = section_by_id(&fetched, "future-note").expect("future kept");
    assert_eq!(future["enabled"], json!(true));
    assert!(
        future["starts_at"]
            .as_str()
            .expect("starts_at string")
            .starts_with("2999-01-01"),
        "window round-trips as RFC 3339: {future}"
    );
    assert!(
        future["ends_at"]
            .as_str()
            .expect("ends_at string")
            .starts_with("2999-01-02"),
        "window end round-trips: {future}"
    );
}
