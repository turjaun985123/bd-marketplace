//! Wave 57 integration test: bulk edit.
//! - price percent math through the API: -10% of 100000
//!   is 90000; +10% of 105 rounds half up to 116; a -99%
//!   cut floors at 1 poisha;
//! - price set and fixed-delta adjust;
//! - stock set lands through the Wave 23 adjust path
//!   (a count_fix stock_adjustments row with the delta);
//!   stock adjust likewise (correction);
//! - a status flip draft -> active and a category assign
//!   by slug ride the shared update path;
//! - a foreign store's product id is a per-item error
//!   while the caller's own items still apply;
//! - request-shape failures are whole-request 400s:
//!   over the 200 cap, no action at all.
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

/// POST one bulk edit; returns (status, report).
async fn bulk_edit(app: &Router, token: &str, body: Value) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(body),
        Some(token),
    )
    .await
}

#[tokio::test]
async fn wave57f_bulk_edit_price_stock_status_category_and_foreign_ids() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-edit").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    let alpha = create_product(&app, &seller, &store_id, "Alpha", 100_000, 10, "draft").await;
    let beta = create_product(&app, &seller, &store_id, "Beta", 105, 3, "draft").await;
    let gamma = create_product(&app, &seller, &store_id, "Gamma", 100, 5, "active").await;

    // A foreign product (another seller's store).
    let stranger = login(&app, "01722222222").await;
    let foreign_store = create_store(&app, &stranger, "w57-foreign").await;
    let foreign_store_id = foreign_store["id"].as_str().expect("store id").to_string();
    let foreign = create_product(&app, &stranger, &foreign_store_id, "Foreign", 77_000, 2, "active").await;

    async fn price_of(pool: &PgPool, id: String) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT price_poisha FROM products WHERE id = $1::uuid")
            .bind(&id)
            .fetch_one(pool)
            .await
            .expect("price")
    }

    // Percent: -10% of 100000 = 90000.
    let (status, report) = bulk_edit(
        &app,
        &seller,
        json!({ "product_ids": [alpha], "price": { "mode": "percent", "percent": -10 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "percent edit: {report}");
    assert_eq!(report["succeeded"], json!(1));
    assert_eq!(report["outcomes"][0]["changed"], json!(["price"]));
    assert_eq!(price_of(&pool, alpha.clone()).await, 90_000);

    // Percent rounding: +10% of 105 = 115.5 -> 116.
    let (status, report) = bulk_edit(
        &app,
        &seller,
        json!({ "product_ids": [beta], "price": { "mode": "percent", "percent": 10 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "rounding edit: {report}");
    assert_eq!(price_of(&pool, beta.clone()).await, 116);

    // The floor: -99% of 100 would be 1 — and stays 1,
    // never 0.
    let (status, _) = bulk_edit(
        &app,
        &seller,
        json!({ "product_ids": [gamma], "price": { "mode": "percent", "percent": -99 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(price_of(&pool, gamma.clone()).await, 1);

    // Set + delta: set alpha to 50000, then adjust -500.
    let (status, _) = bulk_edit(
        &app,
        &seller,
        json!({ "product_ids": [alpha], "price": { "mode": "set", "price_poisha": 50_000 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = bulk_edit(
        &app,
        &seller,
        json!({ "product_ids": [alpha], "price": { "mode": "adjust", "delta_poisha": -500 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(price_of(&pool, alpha.clone()).await, 49_500);

    // Stock set: alpha 10 -> 25 lands as a +15 count_fix
    // adjustment through the audited path.
    let (status, report) = bulk_edit(
        &app,
        &seller,
        json!({ "product_ids": [alpha], "stock": { "mode": "set", "stock": 25 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stock set: {report}");
    assert_eq!(report["outcomes"][0]["changed"], json!(["stock"]));
    let (delta, reason, stock): (i64, String, i32) = sqlx::query_as(
        "SELECT a.delta::bigint, a.reason, p.stock FROM stock_adjustments a \
         JOIN products p ON p.id = a.product_id WHERE a.product_id = $1::uuid",
    )
    .bind(&alpha)
    .fetch_one(&pool)
    .await
    .expect("adjustment");
    assert_eq!((delta, reason.as_str(), stock), (15, "count_fix", 25));

    // Stock adjust: beta 3 -> 1 as a -2 correction.
    let (status, _) = bulk_edit(
        &app,
        &seller,
        json!({ "product_ids": [beta], "stock": { "mode": "adjust", "delta": -2 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let beta_stock: i32 = sqlx::query_scalar("SELECT stock FROM products WHERE id = $1::uuid")
        .bind(&beta)
        .fetch_one(&pool)
        .await
        .expect("beta stock");
    assert_eq!(beta_stock, 1);

    // Status flip + category assign by slug in one call.
    let (status, report) = bulk_edit(
        &app,
        &seller,
        json!({
            "product_ids": [alpha],
            "status": "active",
            "category": { "category_slug": "panjabi" }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "flip+assign: {report}");
    assert_eq!(report["outcomes"][0]["changed"], json!(["status", "category"]));
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{alpha}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["status"], json!("active"));
    assert_eq!(detail["category"], json!("Panjabi"), "{detail}");

    // Foreign id: per-item error; the caller's own item
    // in the same batch still applies.
    let (status, report) = bulk_edit(
        &app,
        &seller,
        json!({
            "product_ids": [foreign, beta],
            "price": { "mode": "set", "price_poisha": 200 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "foreign batch: {report}");
    assert_eq!(report["succeeded"], json!(1));
    assert_eq!(report["failed"], json!(1));
    let outcomes = report["outcomes"].as_array().expect("outcomes");
    assert_eq!(outcomes[0]["ok"], json!(false));
    assert!(outcomes[0]["error"].is_string(), "{outcomes:?}");
    assert_eq!(outcomes[1]["ok"], json!(true));
    assert_eq!(price_of(&pool, foreign.clone()).await, 77_000, "untouched");
    assert_eq!(price_of(&pool, beta.clone()).await, 200);

    // Whole-request 400s: no action, and over the cap.
    let (status, _) = bulk_edit(&app, &seller, json!({ "product_ids": [alpha] })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no action");
    let many: Vec<String> = (0..201)
        .map(|_| uuid::Uuid::new_v4().to_string())
        .collect();
    let (status, _) = bulk_edit(
        &app,
        &seller,
        json!({ "product_ids": many, "status": "draft" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "over the 200 cap");
}
