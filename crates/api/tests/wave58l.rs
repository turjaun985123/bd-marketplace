//! Wave 58 integration test: write semantics and the
//! suspended store:
//! - a second PUT REPLACES the document whole (the old
//!   sections are gone, the row count stays one — an
//!   upsert, never an append);
//! - the catalogue grid resolves even on a store with
//!   zero products (total 0) — it is the one section
//!   that never drops;
//! - a SUSPENDED store's public layout and event beacon
//!   are 404 (its storefront is hidden with the store),
//!   while the seller can still read and edit the layout
//!   (suspension is a public-surface state, not a lock
//!   on the seller's own tools).
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
async fn wave58_upsert_replaces_and_suspension_hides_publicly() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w58-upsert").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // The catalogue grid on an empty store: present,
    // total zero — the one section that never drops.
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-upsert/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    let grid = section_by_id(&public, "all-products").expect("grid on empty store");
    assert_eq!(grid["total_products"], json!(0));

    // First PUT: two sections.
    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "sections": [
                { "id": "note", "type": "rich_text", "config": { "body": "প্রথম সংস্করণ" } },
                { "id": "faq", "type": "faq", "config": { "items": [
                    { "question": "প্রশ্ন?", "answer": "উত্তর।" }
                ] } }
            ]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "first put: {saved}");
    assert_eq!(section_ids(&saved), vec!["note", "faq"]);

    // Second PUT: a different, smaller document REPLACES
    // the first — no trace of the old sections, and the
    // store still has exactly one layout row.
    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "sections": [
                { "id": "hero", "type": "hero", "config": { "headline": "নতুন সাজ" } }
            ]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "second put: {saved}");
    assert_eq!(section_ids(&saved), vec!["hero"]);
    let stored_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_layouts WHERE store_id = $1::uuid",
    )
    .bind(&store_id)
    .fetch_one(&pool)
    .await
    .expect("count stored layouts");
    assert_eq!(stored_rows, 1, "an upsert, never an append");
    let (status, fetched) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller layout: {fetched}");
    assert_eq!(section_ids(&fetched), vec!["hero"]);

    // Suspend the store (the admin surface's state, set
    // here by SQL): the public layout and the beacon go
    // dark; the seller's own layout tools keep working.
    sqlx::query("UPDATE stores SET status = 'suspended' WHERE id = $1::uuid")
        .bind(&store_id)
        .execute(&pool)
        .await
        .expect("suspend store");
    let (status, body) = call(&app, Method::GET, "/v1/stores/w58-upsert/layout", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "suspended layout: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/w58-upsert/section-events",
        Some(json!({ "section_id": "hero", "kind": "section_view" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "suspended beacon: {body}");
    let (status, fetched) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller still reads: {fetched}");
    assert_eq!(section_ids(&fetched), vec!["hero"]);

    // Reactivate: the layout is back, unchanged.
    sqlx::query("UPDATE stores SET status = 'active' WHERE id = $1::uuid")
        .bind(&store_id)
        .execute(&pool)
        .await
        .expect("reactivate store");
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-upsert/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(section_ids(&public), vec!["hero"]);
}
