//! Wave 55d integration test: reorder ("buy again") — a
//! past order's lines come back at CURRENT prices and
//! availability, never the snapshot's. After the order:
//! one product's price RISES (the cart must show the new
//! price), one product is archived (skip:
//! product_unavailable), one bought variant is deactivated
//! (skip: variant_unavailable), one product sells out
//! (skip: out_of_stock), and one variant line is untouched
//! (added with its variant). A cancelled order is a 409 and
//! another buyer's order is a 404.
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

/// Create one variant on a product; returns the variant id.
async fn create_variant(
    app: &Router,
    token: &str,
    product_id: &str,
    name: &str,
    stock: i64,
) -> String {
    let uri = format!("/v1/seller/products/{product_id}/variants");
    let (status, body) = call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "name": name, "price_delta_poisha": 0, "stock": stock, "sort": 0 })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create variant {name}: {body}");
    body["id"].as_str().expect("variant id").to_string()
}

/// Add a line to the buyer's cart (variant optional).
async fn add_to_cart(
    app: &Router,
    token: &str,
    product_id: &str,
    quantity: i64,
    variant_id: Option<&str>,
) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({
            "product_id": product_id,
            "quantity": quantity,
            "variant_id": variant_id
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {body}");
}

/// Checkout the buyer's current cart with COD; returns the order.
async fn checkout_cod(app: &Router, token: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {body}");
    body
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55d integration tests");
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
    let state = AppState::new(Some(pool.clone()), "wave55d-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

#[tokio::test]
async fn wave55d_reorder_current_prices_and_skips_flow() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01755555555").await;
    let store = create_store(&app, &seller, "wave55d-store").await;
    let keep = create_product(&app, &seller, &store, "Wave55D Keep", 10_000, 10).await;
    let goner = create_product(&app, &seller, &store, "Wave55D Goner", 20_000, 5).await;
    let empty = create_product(&app, &seller, &store, "Wave55D Empty", 40_000, 1).await;
    let sized = create_product(&app, &seller, &store, "Wave55D Sized", 30_000, 0).await;
    let small = create_variant(&app, &seller, &sized, "S", 5).await;
    let sized2 = create_product(&app, &seller, &store, "Wave55D Sized Two", 25_000, 0).await;
    let large = create_variant(&app, &seller, &sized2, "L", 5).await;
    let buyer = login(&app, "01944444444").await;
    let stranger = login(&app, "01955555555").await;

    // The original order: one of every case.
    add_to_cart(&app, &buyer, &keep, 2, None).await;
    add_to_cart(&app, &buyer, &goner, 1, None).await;
    add_to_cart(&app, &buyer, &sized, 1, Some(&small)).await;
    add_to_cart(&app, &buyer, &empty, 1, None).await;
    add_to_cart(&app, &buyer, &sized2, 2, Some(&large)).await;
    let order = checkout_cod(&app, &buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();

    // The world moves on: Keep's price rises, Goner is
    // archived, S is deactivated, Empty sold out at
    // checkout (its only unit is in this very order).
    let uri = format!("/v1/products/{keep}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "price_poisha": 15_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "raise price: {body}");
    let uri = format!("/v1/products/{goner}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "archived" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "archive goner: {body}");
    let uri = format!("/v1/seller/products/{sized}/variants/{small}");
    let (status, body) = call(&app, Method::DELETE, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "deactivate S: {body}");
    assert_eq!(body["is_active"], false);

    // 1. Reorder: honest partial success at today's facts.
    let uri = format!("/v1/orders/{order_id}/reorder");
    let (status, report) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "reorder: {report}");
    assert_eq!(report["order_id"], order_id);

    let added = report["added"].as_array().expect("added list");
    assert_eq!(added.len(), 2, "two lines come back: {report}");
    let added_keep = added
        .iter()
        .find(|row| row["product_id"] == keep)
        .expect("keep added");
    assert_eq!(added_keep["quantity"], 2);
    let added_large = added
        .iter()
        .find(|row| row["product_id"] == sized2)
        .expect("sized two added");
    assert_eq!(added_large["variant_id"], large);
    assert_eq!(added_large["quantity"], 2);

    let skipped = report["skipped"].as_array().expect("skipped list");
    assert_eq!(skipped.len(), 3, "three lines skip: {report}");
    let reason_of = |title: &str| {
        skipped
            .iter()
            .find(|row| row["title"] == title)
            .unwrap_or_else(|| panic!("{title} skipped"))
            ["reason"]
            .as_str()
            .expect("reason")
            .to_string()
    };
    assert_eq!(reason_of("Wave55D Goner"), "product_unavailable");
    assert_eq!(reason_of("Wave55D Sized"), "variant_unavailable");
    assert_eq!(reason_of("Wave55D Empty"), "out_of_stock");

    // The cart in the report prices Keep at TODAY's 15_000
    // — the 10_000 snapshot price is never resurrected.
    let cart = &report["cart"];
    let keep_line = cart["items"]
        .as_array()
        .expect("cart items")
        .iter()
        .find(|row| row["product_id"] == keep)
        .expect("keep in cart");
    assert_eq!(keep_line["unit_price_poisha"], 15_000);
    assert_eq!(keep_line["line_total_poisha"], 30_000);
    assert_eq!(
        cart["goods_total_poisha"], 80_000,
        "2 x 15_000 (new price) + 2 x 25_000: {cart}"
    );

    // 2. Another buyer cannot reorder this order (404),
    // and neither can anyone reorder a phantom order.
    let (status, refused) = call(&app, Method::POST, &uri, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger reorder: {refused}");
    let (status, refused) = call(
        &app,
        Method::POST,
        "/v1/orders/00000000-0000-0000-0000-000000000000/reorder",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "phantom reorder: {refused}");

    // 3. A cancelled order cannot be reordered (409).
    add_to_cart(&app, &buyer, &keep, 1, None).await;
    let second = checkout_cod(&app, &buyer).await;
    let second_id = second["id"].as_str().expect("order id").to_string();
    let uri = format!("/v1/orders/{second_id}/cancel");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cancel second order: {body}");
    let uri = format!("/v1/orders/{second_id}/reorder");
    let (status, refused) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::CONFLICT, "cancelled reorder: {refused}");
}
