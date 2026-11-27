//! Wave 55g integration test: reorder CLAMPS — the
//! per-item report never promises more than reality.
//! Partial stock re-adds exactly the available quantity
//! (not the ordered quantity, not a skip); a line already
//! at the 99-unit cart cap skips with `cart_limit`; a
//! product that GAINED variants after a variant-less
//! purchase skips with `variant_unavailable` (the buyer
//! must choose a variant — the platform never guesses).
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

/// Create an active product; returns the product id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Add a product to the buyer's cart.
async fn add_to_cart(app: &Router, token: &str, product_id: &str, quantity: i64) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_id, "quantity": quantity })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {body}");
}

/// COD-checkout the cart with the standard test address;
/// returns the created order.
async fn checkout(app: &Router, token: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 1, Road 1, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {body}");
    body
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<Router> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55g integration tests");
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
         buyer_addresses, \
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
    let state = AppState::new(Some(pool), "wave55g-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some(build_app(state))
}

#[tokio::test]
async fn wave55g_reorder_clamps_to_stock_cart_cap_and_variant_rules() {
    let Some(app) = test_app().await else {
        return;
    };
    let seller = login(&app, "01712121212").await;
    let store = create_store(&app, &seller, "wave55g-store").await;
    // Partial: stock 5, order takes 4 (stock 1), restock +2
    // (stock 3) — a reorder of 4 must add exactly 3.
    let partial = create_product(&app, &seller, &store, "Partial Stock", 10_000, 5).await;
    // Full: stock 200 — the order takes 1, then the cart is
    // pre-filled to the 99 cap before the reorder.
    let full = create_product(&app, &seller, &store, "Full Cart", 10_000, 200).await;
    // Plain: bought variant-less; gains a variant later.
    let plain = create_product(&app, &seller, &store, "Gains Variants", 10_000, 10).await;
    let buyer = login(&app, "01912121212").await;

    add_to_cart(&app, &buyer, &partial, 4).await;
    add_to_cart(&app, &buyer, &full, 1).await;
    add_to_cart(&app, &buyer, &plain, 1).await;
    let order = checkout(&app, &buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();

    // Restock Partial to 3 (was 1 after the order).
    let uri = format!("/v1/seller/products/{partial}/adjust-stock");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "delta": 2, "reason": "restock" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "restock: {body}");

    // Plain gains a variant AFTER the purchase.
    let uri = format!("/v1/seller/products/{plain}/variants");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "name": "S", "price_delta_poisha": 0, "stock": 5, "sort": 0 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create variant: {body}");

    // Pre-fill the cart's Full line to the 99 cap.
    add_to_cart(&app, &buyer, &full, 99).await;

    // --- Reorder: one clamped add, two skips. ---------------
    let uri = format!("/v1/orders/{order_id}/reorder");
    let (status, report) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "reorder: {report}");

    let added = report["added"].as_array().expect("added array");
    assert_eq!(added.len(), 1, "only the clamped line adds: {report}");
    assert_eq!(added[0]["product_id"], partial);
    assert_eq!(
        added[0]["quantity"], 3,
        "clamped to the 3 in stock, not the 4 ordered"
    );

    let skipped = report["skipped"].as_array().expect("skipped array");
    assert_eq!(skipped.len(), 2, "{report}");
    let by_reason: std::collections::HashMap<String, String> = skipped
        .iter()
        .map(|entry| {
            (
                entry["title"].as_str().expect("title").to_string(),
                entry["reason"].as_str().expect("reason").to_string(),
            )
        })
        .collect();
    assert_eq!(
        by_reason.get("Full Cart").map(String::as_str),
        Some("cart_limit")
    );
    assert_eq!(
        by_reason.get("Gains Variants").map(String::as_str),
        Some("variant_unavailable")
    );

    // The cart view agrees: Partial ×3, Full ×99, Plain absent.
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    let lines = cart["items"].as_array().expect("cart items");
    assert_eq!(lines.len(), 2, "{cart}");
    let partial_line = lines
        .iter()
        .find(|line| line["product_id"] == partial)
        .expect("partial line");
    assert_eq!(partial_line["quantity"], 3);
    let full_line = lines
        .iter()
        .find(|line| line["product_id"] == full)
        .expect("full line");
    assert_eq!(full_line["quantity"], 99);
}
