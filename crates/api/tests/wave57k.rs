//! Wave 57 integration test: bulk-edit invariants.
//! - a draft -> active flip through bulk edit fires the
//!   follower notice (the shared update path's rule —
//!   the bulk path inherits it, it does not re-decide it);
//! - `changed` is honest: setting the price a product
//!   already has reports no change and succeeds;
//! - a duplicated product id applies a percent ONCE
//!   (the request dedupes, keeping first-occurrence order);
//! - a stock action on a variant-managed product is a
//!   per-item error (the Wave 23 adjust path's refusal),
//!   never a silent write to the product's stock pool;
//! - category assignment by free-text label links on the
//!   Wave 56 exact-match rule through the update path.
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
            eprintln!("DATABASE_URL not set — skipping Wave 57 integration tests");
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
         buyer_addresses, bulk_imports, \
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
    let state = AppState::new(Some(pool.clone()), "wave57-test-secret".to_string(), true)
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

/// POST one product (the wave9 shape); returns its id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i32,
    status: &str,
) -> String {
    let (code, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": status
        })),
        Some(token),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "create {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

#[tokio::test]
async fn wave57k_bulk_edit_notice_dedupe_and_variant_rules() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-edit-rules").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    let follower = login(&app, "01733333333").await;
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/stores/w57-edit-rules/follow",
        None,
        Some(&follower),
    )
    .await;
    assert!(status.is_success(), "follow: {status}");
    let follower_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE phone = '+8801733333333'")
            .fetch_one(&pool)
            .await
            .expect("follower row");
    let notice_count = |pool: &PgPool| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND kind = 'store_new_product'",
        )
        .bind(follower_id)
        .fetch_one(pool)
        .await
        .expect("count notices")
    };

    // A draft product, flipped active by bulk edit: the
    // follower notice fires exactly once.
    let draft = create_product(&app, &seller, &store_id, "Drafty", 10_000, 5, "draft").await;
    assert_eq!(notice_count(&pool).await, 0, "a draft creation notifies nobody");
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({ "product_ids": [draft], "status": "active" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "flip: {report}");
    assert_eq!(report["outcomes"][0]["changed"], json!(["status"]));
    assert_eq!(notice_count(&pool).await, 1, "the flip notified the follower");

    // Flipping to the SAME status again: no change, no
    // second notice, still a success.
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({ "product_ids": [draft], "status": "active" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-flip: {report}");
    assert_eq!(report["outcomes"][0]["ok"], json!(true));
    assert_eq!(
        report["outcomes"][0]["changed"],
        json!([]),
        "no change is reported honestly"
    );
    assert_eq!(notice_count(&pool).await, 1, "no duplicate notice");

    // Setting the price the product already has: ok,
    // changed empty.
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({
            "product_ids": [draft],
            "price": { "mode": "set", "price_poisha": 10_000 }
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "same price: {report}");
    assert_eq!(report["outcomes"][0]["changed"], json!([]));

    // A duplicated id applies its percent ONCE:
    // +10% of 10000 is 11000, not 12100.
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({
            "product_ids": [draft, draft],
            "price": { "mode": "percent", "percent": 10 }
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dedupe: {report}");
    assert_eq!(report["total"], json!(1), "the duplicate collapses");
    let price: i64 = sqlx::query_scalar("SELECT price_poisha FROM products WHERE id = $1::uuid")
        .bind(&draft)
        .fetch_one(&pool)
        .await
        .expect("price");
    assert_eq!(price, 11_000, "percent applied exactly once");

    // A variant-managed product refuses bulk stock with
    // a per-item error; its price still edits fine in a
    // separate call (the refusal is stock-specific).
    let varied = create_product(&app, &seller, &store_id, "Varied", 50_000, 0, "active").await;
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/products/{varied}/variants"),
        Some(json!({ "name": "Large", "price_delta_poisha": 5_000, "stock": 7 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "variant: {body}");
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({
            "product_ids": [varied],
            "stock": { "mode": "adjust", "delta": 3 }
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "variant stock: {report}");
    assert_eq!(report["failed"], json!(1));
    assert!(
        report["outcomes"][0]["error"]
            .as_str()
            .expect("error")
            .contains("variant"),
        "{report}"
    );
    let adjustments: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM stock_adjustments WHERE product_id = $1::uuid",
    )
    .bind(&varied)
    .fetch_one(&pool)
    .await
    .expect("adjustments");
    assert_eq!(adjustments, 0, "no adjustment row was written");

    // Category by free-text label: "Panjabi" is the
    // node's English name, so the update path links it.
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({
            "product_ids": [draft],
            "category": { "category_label": "Panjabi" }
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "label assign: {report}");
    assert_eq!(report["outcomes"][0]["changed"], json!(["category"]));
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{draft}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert!(detail["category_id"].is_string(), "linked: {detail}");
    assert_eq!(detail["category"], json!("Panjabi"));
}
