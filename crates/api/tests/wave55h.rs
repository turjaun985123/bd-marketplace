//! Wave 55h integration test: the buy-again rail, the
//! saved-shelf bulk operations, and the fulfillment
//! queue's district.
//!
//! The rail (GET /v1/buyer/buy-again) shows products from
//! past orders that can be bought again TODAY: an
//! archived product drops off, `in_cart` tracks the active
//! cart, and a stranger's rail is empty. The shelf bulk
//! ops (move-all / clear) move exactly the saved lines.
//! The fulfillment queue item carries the order's district
//! from a district checkout.
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

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<Router> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55h integration tests");
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
    let state = AppState::new(Some(pool), "wave55h-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some(build_app(state))
}

#[tokio::test]
async fn wave55h_buy_again_rail_bulk_saved_ops_and_fulfillment_district() {
    let Some(app) = test_app().await else {
        return;
    };
    let seller = login(&app, "01713131313").await;
    let store = create_store(&app, &seller, "wave55h-store").await;
    let keeper = create_product(&app, &seller, &store, "Rail Keeper", 10_000, 20).await;
    let goner = create_product(&app, &seller, &store, "Rail Goner", 20_000, 20).await;
    let extra = create_product(&app, &seller, &store, "Shelf Extra", 30_000, 20).await;
    let buyer = login(&app, "01913131313").await;
    let stranger = login(&app, "01914141414").await;

    // Order Keeper + Goner with a district (Tangail).
    add_to_cart(&app, &buyer, &keeper, 1).await;
    add_to_cart(&app, &buyer, &goner, 1).await;
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 1, Tangail Sadar",
            "delivery_zone": "inside_dhaka",
            "delivery_district": "Tangail",
            "payment_method": "cod"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");

    // The fulfillment queue shows the district.
    let uri = format!("/v1/seller/fulfillment?store_id={store}");
    let (status, queue) = call(&app, Method::GET, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "fulfillment: {queue}");
    assert_eq!(queue["items"][0]["delivery_district"], "Tangail");

    // Archive Goner: it must leave the rail.
    let uri = format!("/v1/products/{goner}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "archived" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "archive: {body}");

    // --- The rail. ------------------------------------------
    let (status, rail) = call(&app, Method::GET, "/v1/buyer/buy-again", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "rail: {rail}");
    let items = rail["items"].as_array().expect("rail items");
    assert_eq!(items.len(), 1, "only the keeper survives: {rail}");
    assert_eq!(items[0]["product_id"], keeper);
    assert_eq!(items[0]["times_ordered"], 1);
    assert_eq!(items[0]["unit_price_poisha"], 10_000);
    assert_eq!(items[0]["in_cart"], false);

    // The stranger's rail is empty.
    let (status, rail) = call(&app, Method::GET, "/v1/buyer/buy-again", None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rail["items"].as_array().expect("items").len(), 0);

    // Adding Keeper flips in_cart.
    add_to_cart(&app, &buyer, &keeper, 1).await;
    let (status, rail) = call(&app, Method::GET, "/v1/buyer/buy-again", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rail["items"][0]["in_cart"], true);

    // --- Shelf bulk ops. ------------------------------------
    // Cart: Keeper ×1 (active). Save it; add Extra, save it.
    let uri = format!("/v1/cart/items/{keeper}/save");
    let (status, body) = call(&app, Method::POST, &uri, Some(json!({})), Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "save keeper: {body}");
    add_to_cart(&app, &buyer, &extra, 2).await;
    let uri = format!("/v1/cart/items/{extra}/save");
    let (status, body) = call(&app, Method::POST, &uri, Some(json!({})), Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "save extra: {body}");

    // Move-all brings both back as active lines.
    let (status, cart) = call(
        &app,
        Method::POST,
        "/v1/cart/saved/move-all",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "move-all: {cart}");
    assert_eq!(cart["items"].as_array().expect("items").len(), 2);
    assert_eq!(cart["saved_items"].as_array().expect("saved").len(), 0);
    assert_eq!(cart["goods_total_poisha"], 10_000 + 2 * 30_000);

    // Save both again, then clear the shelf: both lines go,
    // and nothing else is touched.
    let uri = format!("/v1/cart/items/{keeper}/save");
    let (status, _) = call(&app, Method::POST, &uri, Some(json!({})), Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    let uri = format!("/v1/cart/items/{extra}/save");
    let (status, _) = call(&app, Method::POST, &uri, Some(json!({})), Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, cart) = call(&app, Method::DELETE, "/v1/cart/saved", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "clear shelf: {cart}");
    assert_eq!(cart["items"].as_array().expect("items").len(), 0);
    assert_eq!(cart["saved_items"].as_array().expect("saved").len(), 0);
    assert_eq!(cart["goods_total_poisha"], 0);
}
