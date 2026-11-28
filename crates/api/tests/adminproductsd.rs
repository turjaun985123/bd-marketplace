//! Admin products remake, test D: the purchase paths. A
//! taken-down product cannot enter a cart (400 at the
//! door, bilingual platform wording); a product carted
//! BEFORE its takedown is refused at checkout — checkout
//! is the re-validator — with a 400 naming the product,
//! the whole checkout aborts, and the cart survives
//! intact, so a restore makes the same cart check out.
//! Reorder reports the line with the distinct
//! `product_taken_down` reason (not the seller-withdrawal
//! `product_unavailable`), and the buy-again rail stops
//! offering the product while it is down.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job.

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

/// Add a product to the buyer's cart; returns the raw result.
async fn add_to_cart_raw(
    app: &Router,
    token: &str,
    product_id: &str,
    quantity: i64,
) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_id, "quantity": quantity })),
        Some(token),
    )
    .await
}

/// Checkout the buyer's current cart with COD; returns the raw result.
async fn checkout_cod_raw(app: &Router, token: &str) -> (StatusCode, Value) {
    call(
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
    .await
}

#[tokio::test]
async fn adminproducts_purchase_paths_block_taken_down() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin products remake test D");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, return_requests, store_follows, \
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
    let state = AppState::new(
        Some(pool.clone()),
        "adminproductsd-test-secret".to_string(),
        true,
    )
    .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "aprod-delta").await;
    // 100_000 poisha of goods stays under the COD advance
    // threshold, so checkout is plain COD on this app.
    let target = create_product(&app, &seller, &store, "Delta Kettle", 100_000, 10).await;

    let buyer = login(&app, "01733333333").await;

    // The buyer carts the target and checks out once, while
    // it stands — this order feeds reorder + buy-again.
    let (status, body) = add_to_cart_raw(&app, &buyer, &target, 1).await;
    assert_eq!(status, StatusCode::CREATED, "cart before: {body}");
    let (status, order) = checkout_cod_raw(&app, &buyer).await;
    assert_eq!(status, StatusCode::CREATED, "checkout before: {order}");
    let order_id = order["id"].as_str().expect("order id").to_string();

    // Buy-again offers it while it stands.
    let (status, rail) = call(&app, Method::GET, "/v1/buyer/buy-again", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buy-again before: {rail}");
    assert!(
        rail.to_string().contains(&target),
        "buy-again before offers the target: {rail}"
    );

    // Cart the target again, THEN take it down: the cart
    // now holds a line checkout must refuse.
    let (status, body) = add_to_cart_raw(&app, &buyer, &target, 1).await;
    assert_eq!(status, StatusCode::CREATED, "re-cart: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/products/{target}/takedown"),
        Some(json!({ "note": "বিপজ্জনক পণ্য" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "takedown: {body}");

    // Cart add is refused at the door with the platform wording.
    let (status, body) = add_to_cart_raw(&app, &buyer, &target, 1).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "cart add while down: {body}");
    assert!(
        body["error"].as_str().unwrap_or("").contains("সরিয়ে নেওয়া হয়েছে"),
        "cart add wording: {body}"
    );

    // Checkout refuses the whole order, naming the product,
    // and the cart survives intact.
    let (status, body) = checkout_cod_raw(&app, &buyer).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "checkout while down: {body}");
    let message = body["error"].as_str().unwrap_or("");
    assert!(message.contains("সরিয়ে নেওয়া হয়েছে"), "checkout wording: {body}");
    assert!(message.contains("Delta Kettle"), "checkout names the product: {body}");
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cart read: {cart}");
    assert_eq!(
        cart["items"].as_array().map(Vec::len),
        Some(1),
        "cart intact after refused checkout: {cart}"
    );

    // Reorder reports the distinct takedown reason.
    let (status, report) = call(
        &app,
        Method::POST,
        &format!("/v1/orders/{order_id}/reorder"),
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reorder while down: {report}");
    assert_eq!(report["added"], json!([]), "reorder while down: {report}");
    assert_eq!(
        report["skipped"][0]["reason"],
        json!("product_taken_down"),
        "reorder reason: {report}"
    );

    // Buy-again stops offering it.
    let (status, rail) = call(&app, Method::GET, "/v1/buyer/buy-again", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buy-again while down: {rail}");
    assert!(
        !rail.to_string().contains(&target),
        "buy-again while down must not offer the target: {rail}"
    );

    // Restore: the surviving cart line checks out, and
    // reorder adds the line again.
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/products/{target}/restore"),
        Some(json!({ "note": "নিরাপদ প্রমাণিত" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "restore: {body}");
    let (status, body) = checkout_cod_raw(&app, &buyer).await;
    assert_eq!(status, StatusCode::CREATED, "checkout after restore: {body}");
    let (status, report) = call(
        &app,
        Method::POST,
        &format!("/v1/orders/{order_id}/reorder"),
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reorder after restore: {report}");
    assert_eq!(
        report["added"].as_array().map(Vec::len),
        Some(1),
        "reorder after restore adds the line: {report}"
    );
}
