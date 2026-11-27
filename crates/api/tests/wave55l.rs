//! Wave 55l integration test: cart v2 edges — a VARIANT
//! line's save/unsave round trip (the variant id rides the
//! move body), removing a SAVED line outright, and a
//! reorder landing on a line that sits on the saved shelf
//! (the shelf quantity counts toward the cart cap, and
//! the re-added quantity revives the line as ACTIVE).
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

/// Create a variant on a product; returns the variant id.
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

/// Add a product (optionally a variant) to the buyer's cart.
async fn add_to_cart(
    app: &Router,
    token: &str,
    product_id: &str,
    variant_id: Option<&str>,
    quantity: i64,
) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({
            "product_id": product_id,
            "variant_id": variant_id,
            "quantity": quantity
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {body}");
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<Router> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55l integration tests");
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
    let state = AppState::new(Some(pool), "wave55l-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some(build_app(state))
}

#[tokio::test]
async fn wave55l_variant_shelf_round_trip_and_reorder_onto_saved_line() {
    let Some(app) = test_app().await else {
        return;
    };
    let seller = login(&app, "01720202020").await;
    let store = create_store(&app, &seller, "wave55l-store").await;
    let sized = create_product(&app, &seller, &store, "Sized Shelf", 10_000, 0).await;
    let small = create_variant(&app, &seller, &sized, "S", 50).await;
    let bulk = create_product(&app, &seller, &store, "Bulk Shelf", 10_000, 300).await;
    let buyer = login(&app, "01920202020").await;

    // --- Variant line: save and unsave by variant id. -------
    add_to_cart(&app, &buyer, &sized, Some(&small), 2).await;
    let uri = format!("/v1/cart/items/{sized}/save");
    let (status, cart) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "variant_id": small })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save variant line: {cart}");
    assert_eq!(cart["items"].as_array().expect("items").len(), 0);
    let saved = cart["saved_items"].as_array().expect("saved");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["variant_id"], small);
    assert_eq!(saved[0]["quantity"], 2);

    // Saving WITHOUT the variant id addresses a different
    // (nonexistent) line — a 404, and the shelf is intact.
    let (status, refused) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({})),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "wrong line: {refused}");

    let uri = format!("/v1/cart/items/{sized}/unsave");
    let (status, cart) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "variant_id": small })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unsave variant line: {cart}");
    assert_eq!(cart["items"].as_array().expect("items").len(), 1);
    assert_eq!(cart["saved_items"].as_array().expect("saved").len(), 0);

    // --- Removing a saved line outright. --------------------
    let uri = format!("/v1/cart/items/{sized}/save");
    let (status, _) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "variant_id": small })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let uri = format!("/v1/cart/items/{sized}?variant_id={small}");
    let (status, cart) = call(&app, Method::DELETE, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "remove saved line: {cart}");
    assert_eq!(cart["items"].as_array().expect("items").len(), 0);
    assert_eq!(cart["saved_items"].as_array().expect("saved").len(), 0);

    // --- Reorder onto a saved line. --------------------------
    // Past order: Bulk ×5.
    add_to_cart(&app, &buyer, &bulk, None, 5).await;
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 1, Road 1, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");
    let order_id = order["id"].as_str().expect("order id").to_string();

    // Refill the cart with Bulk ×98 and shelve the line.
    add_to_cart(&app, &buyer, &bulk, None, 98).await;
    let uri = format!("/v1/cart/items/{bulk}/save");
    let (status, _) = call(&app, Method::POST, &uri, Some(json!({})), Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);

    // Reorder: headroom over the SHELVED line is 99 − 98 =
    // 1, so exactly 1 is added — and adding revives the
    // line as ACTIVE at 99.
    let uri = format!("/v1/orders/{order_id}/reorder");
    let (status, report) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "reorder: {report}");
    assert_eq!(report["added"].as_array().expect("added").len(), 1);
    assert_eq!(report["added"][0]["quantity"], 1);
    let cart = &report["cart"];
    assert_eq!(cart["saved_items"].as_array().expect("saved").len(), 0);
    let lines = cart["items"].as_array().expect("items");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["quantity"], 99);
    assert_eq!(lines[0]["saved"], false);
}
