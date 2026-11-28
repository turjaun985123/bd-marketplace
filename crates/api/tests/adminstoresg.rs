//! Admin stores remake, test G: the suspension blast
//! radius holes the remake closes. Before it, a suspended
//! store's product could still be ADDED to a cart (the cart
//! read only the product's own status), a reorder reported
//! the line with the wrong reason, and the buy-again rail
//! kept offering the product. Now: cart add refuses with
//! the cart module's bilingual 400, reorder skips the line
//! with the dedicated `store_suspended` reason, and the
//! rail excludes the store's products — while another
//! store's product carts normally. Helpers are copied from
//! the Wave 9 test on purpose — integration test files
//! stay self-contained.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`.

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

/// Add a product to the buyer's cart; asserts 201.
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

/// Checkout the buyer's current cart with COD; asserts 201
/// and returns the order.
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

/// Suspend a store with a note; asserts success.
async fn suspend_store(app: &Router, admin: &str, store_id: &str) {
    let uri = format!("/v1/admin/stores/{store_id}/status");
    let (status, body) = call(
        app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "suspended", "note": "Blast radius fixture" })),
        Some(admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suspend store: {body}");
}

#[tokio::test]
async fn adminstores_suspension_blocks_cart_reorder_buyagain() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin stores remake test G");
            return;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, return_requests, store_follows, \
         store_verification_events, store_verifications, platform_settings, \
         payouts, payout_accounts, review_replies, reviews, shipment_events, \
         shipments, payment_events, payments, order_items, sub_orders, orders, \
         cart_items, carts, products, api_keys, store_members, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "adminstoresg-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "gstores-shop").await;
    let product_p = create_product(&app, &seller, &store, "G Product P", 100_000, 20).await;
    let other_seller = login(&app, "01733333333").await;
    let other_store = create_store(&app, &other_seller, "gstores-other").await;
    let product_q = create_product(&app, &other_seller, &other_store, "G Product Q", 80_000, 5).await;

    // The buyer buys P once (the order stays `placed` —
    // reorder refuses only cancelled orders), emptying the
    // cart through checkout.
    let buyer = login(&app, "01933333333").await;
    add_to_cart(&app, &buyer, &product_p, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();

    suspend_store(&app, &admin, &store).await;

    // Cart add of the suspended store's product: the cart
    // module's bilingual 400, and no cart line is created.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_p, "quantity": 1 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "cart add refused: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("স্থগিত"),
        "suspension wording: {body}"
    );
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cart view: {cart}");
    assert!(
        cart["items"]
            .as_array()
            .expect("cart items")
            .iter()
            .all(|item| item["product_id"] != product_p.as_str()),
        "no suspended-store line in the cart: {cart}"
    );

    // Reorder: honest partial success — the line is SKIPPED
    // with the dedicated store_suspended reason.
    let uri = format!("/v1/orders/{order_id}/reorder");
    let (status, report) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "reorder report: {report}");
    assert_eq!(report["added"].as_array().expect("added").len(), 0);
    let skipped = report["skipped"].as_array().expect("skipped");
    assert_eq!(skipped.len(), 1, "one skipped line: {report}");
    assert_eq!(skipped[0]["reason"], "store_suspended");
    assert_eq!(skipped[0]["title"], "G Product P");

    // Buy-again: the suspended store's product is gone
    // from the rail.
    let (status, rail) = call(&app, Method::GET, "/v1/buyer/buy-again", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buy-again: {rail}");
    assert!(
        rail["items"]
            .as_array()
            .expect("rail items")
            .iter()
            .all(|item| item["product_id"] != product_p.as_str()),
        "suspended-store product off the rail: {rail}"
    );

    // Control: the other store's product still carts —
    // the guard is store-scoped, not a broken cart.
    add_to_cart(&app, &buyer, &product_q, 1).await;
}
