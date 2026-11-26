//! Wave 5 integration test: online payments foundation — method
//! availability, dev-simulated bKash checkout, the signed webhook
//! (wrong signature, amount mismatch, success, duplicate delivery),
//! failure + retry, dev-simulate-success, and COD staying row-free.
//! Driven through the real Axum router with tower oneshot. The small
//! helpers are copied from the Wave 4 test on purpose: integration
//! test files stay self-contained rather than sharing test code
//! across files.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.
//!
//! The payments DEV simulator is ON for the main app in this test
//! (explicit dev flag, dev secret below) — that is the only way any
//! online payment can succeed here, and every reference it mints is
//! prefixed `DEV-`. A second app instance with dev mode OFF covers
//! the honest production behaviour: methods unavailable, checkout 503.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use payments::{sign_dev_callback, PaymentsConfig};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const DEV_SECRET: &str = "wave5-test-dev-secret";

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

/// Checkout the buyer's current cart with the given payment method.
async fn checkout(
    app: &Router,
    token: &str,
    payment_method: &str,
) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01933333333",
            "delivery_address": "House 3, Road 5, Chattogram",
            "delivery_zone": "inside_dhaka",
            "payment_method": payment_method
        })),
        Some(token),
    )
    .await
}

/// POST one gateway webhook callback for a payment reference.
async fn webhook(
    app: &Router,
    method: &str,
    payment_ref: &str,
    amount_poisha: i64,
    callback_status: &str,
    signature: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/payments/webhook/{method}");
    call(
        app,
        Method::POST,
        &uri,
        Some(json!({
            "payment_ref": payment_ref,
            "amount_poisha": amount_poisha,
            "status": callback_status,
            "signature": signature,
        })),
        None,
    )
    .await
}

/// The buyer's view of one order (payment_status + payment summary).
async fn get_order(app: &Router, token: &str, order_id: &str) -> Value {
    let uri = format!("/v1/orders/{order_id}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "get order: {body}");
    body
}

#[tokio::test]
async fn wave5_online_payments_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 5 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE payment_events, payments, order_items, sub_orders, orders, \
         cart_items, carts, products, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // Main app: SMS dev mode (for OTP login) AND payments dev mode on.
    let dev_config = PaymentsConfig {
        dev_mode: true,
        dev_secret: DEV_SECRET.to_string(),
        ..PaymentsConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave5-test-secret".to_string(), true)
        .with_payments_config(dev_config);
    let app = build_app(state);

    // One seller, one store, one 1,000 BDT product (default 1 kg).
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "wave5-store").await;
    let product = create_product(&app, &seller, &store, "Wave5 Product", 100_000, 20).await;
    let buyer = login(&app, "01933333333").await;

    // 1. Method availability with dev mode on: COD always available;
    // bKash available and honestly labelled as dev.
    let (status, methods) = call(&app, Method::GET, "/v1/payments/methods", None, None).await;
    assert_eq!(status, StatusCode::OK, "methods: {methods}");
    let method_list = methods.as_array().expect("methods array");
    let bkash = method_list
        .iter()
        .find(|m| m["method"] == "bkash")
        .expect("bkash listed");
    assert_eq!(bkash["available"], true, "dev bkash available: {bkash}");
    assert_eq!(bkash["mode"], "dev", "dev bkash labelled dev: {bkash}");
    let cod = method_list
        .iter()
        .find(|m| m["method"] == "cod")
        .expect("cod listed");
    assert_eq!(cod["available"], true);

    // 2. bKash checkout in dev mode: order pending, payment row
    // pending, DEV reference and dev redirect present. Inside Dhaka,
    // 1 kg: goods 100,000 + delivery 6,000 = 106,000 poisha.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order_a) = checkout(&app, &buyer, "bkash").await;
    assert_eq!(status, StatusCode::CREATED, "bkash checkout: {order_a}");
    assert_eq!(order_a["grand_total_poisha"], 106_000);
    assert_eq!(order_a["payment_status"], "pending");
    assert_eq!(order_a["payment"]["method"], "bkash");
    assert_eq!(order_a["payment"]["status"], "pending");
    let order_a_id = order_a["id"].as_str().expect("order id").to_string();
    let payment_a_id = order_a["payment"]["id"]
        .as_str()
        .expect("payment id")
        .to_string();
    let ref_a = order_a["payment"]["gateway_ref"]
        .as_str()
        .expect("gateway ref")
        .to_string();
    assert_eq!(ref_a, format!("DEV-bkash-{order_a_id}"));
    assert_eq!(
        order_a["payment"]["dev_redirect_url"],
        format!("/dev-pay/{ref_a}")
    );
    let amount_a = order_a["grand_total_poisha"].as_i64().expect("grand total");

    // 3. Webhook with a WRONG signature: 401, payment still pending,
    // and the rejected attempt is in the audit log as invalid.
    let (status, body) = webhook(&app, "bkash", &ref_a, amount_a, "success", "deadbeef").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "wrong signature: {body}");
    let order = get_order(&app, &buyer, &order_a_id).await;
    assert_eq!(order["payment_status"], "pending");
    assert_eq!(order["payment"]["status"], "pending");
    let payment_a_uuid = Uuid::parse_str(&payment_a_id).expect("payment id is a uuid");
    let invalid_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM payment_events WHERE payment_id = $1 AND signature_valid = false",
    )
    .bind(payment_a_uuid)
    .fetch_one(&pool)
    .await
    .expect("count invalid events");
    assert!(invalid_events >= 1, "rejected webhook is audit-logged");

    // 4. Webhook with a signature that is valid FOR THE WRONG AMOUNT:
    // the signature verifies over its own payload, but the amount
    // does not match the payment, so 400 and still pending.
    let wrong_amount = amount_a + 100;
    let wrong_amount_sig = sign_dev_callback(DEV_SECRET, &ref_a, wrong_amount, "success");
    let (status, body) = webhook(
        &app,
        "bkash",
        &ref_a,
        wrong_amount,
        "success",
        &wrong_amount_sig,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "amount mismatch: {body}");
    let order = get_order(&app, &buyer, &order_a_id).await;
    assert_eq!(order["payment_status"], "pending");

    // 5. Correctly signed success webhook: payment paid, order paid.
    // The exact same webhook delivered AGAIN is a 200 no-op.
    let good_sig = sign_dev_callback(DEV_SECRET, &ref_a, amount_a, "success");
    let (status, payment) = webhook(&app, "bkash", &ref_a, amount_a, "success", &good_sig).await;
    assert_eq!(status, StatusCode::OK, "success webhook: {payment}");
    assert_eq!(payment["status"], "paid");
    assert!(payment["paid_at"].is_string(), "paid_at set: {payment}");
    let order = get_order(&app, &buyer, &order_a_id).await;
    assert_eq!(order["payment_status"], "paid");
    assert_eq!(order["payment"]["status"], "paid");

    let (status, payment) = webhook(&app, "bkash", &ref_a, amount_a, "success", &good_sig).await;
    assert_eq!(status, StatusCode::OK, "duplicate webhook: {payment}");
    assert_eq!(payment["status"], "paid");

    // 6. Second bKash order: a signed FAILED webhook marks payment
    // and order failed; the buyer's retry returns it to pending.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order_b) = checkout(&app, &buyer, "bkash").await;
    assert_eq!(status, StatusCode::CREATED, "bkash checkout B: {order_b}");
    let order_b_id = order_b["id"].as_str().expect("order id").to_string();
    let payment_b_id = order_b["payment"]["id"]
        .as_str()
        .expect("payment id")
        .to_string();
    let ref_b = order_b["payment"]["gateway_ref"]
        .as_str()
        .expect("gateway ref")
        .to_string();
    let amount_b = order_b["grand_total_poisha"].as_i64().expect("grand total");

    let failed_sig = sign_dev_callback(DEV_SECRET, &ref_b, amount_b, "failed");
    let (status, payment) = webhook(&app, "bkash", &ref_b, amount_b, "failed", &failed_sig).await;
    assert_eq!(status, StatusCode::OK, "failed webhook: {payment}");
    assert_eq!(payment["status"], "failed");
    let order = get_order(&app, &buyer, &order_b_id).await;
    assert_eq!(order["payment_status"], "failed");

    let retry_uri = format!("/v1/payments/{payment_b_id}/retry");
    let (status, payment) = call(&app, Method::POST, &retry_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "retry: {payment}");
    assert_eq!(payment["status"], "pending");
    let order = get_order(&app, &buyer, &order_b_id).await;
    assert_eq!(order["payment_status"], "pending");

    // 7. Third bKash order settled through the dev-simulate helper:
    // the same apply path as the webhook, driven by the API itself.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order_c) = checkout(&app, &buyer, "bkash").await;
    assert_eq!(status, StatusCode::CREATED, "bkash checkout C: {order_c}");
    let order_c_id = order_c["id"].as_str().expect("order id").to_string();
    let payment_c_id = order_c["payment"]["id"]
        .as_str()
        .expect("payment id")
        .to_string();
    let simulate_uri = format!("/v1/payments/{payment_c_id}/dev-simulate-success");
    let (status, payment) = call(&app, Method::POST, &simulate_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "dev simulate: {payment}");
    assert_eq!(payment["status"], "paid");
    let order = get_order(&app, &buyer, &order_c_id).await;
    assert_eq!(order["payment_status"], "paid");

    // 8. COD checkout: pay_on_delivery, no payment summary, and NO
    // payment row in the database.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order_d) = checkout(&app, &buyer, "cod").await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {order_d}");
    assert_eq!(order_d["payment_status"], "pay_on_delivery");
    assert!(order_d["payment"].is_null(), "COD has no payment: {order_d}");
    let order_d_uuid = Uuid::parse_str(order_d["id"].as_str().expect("order id"))
        .expect("order id is a uuid");
    let cod_payment_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM payments WHERE order_id = $1")
            .bind(order_d_uuid)
            .fetch_one(&pool)
            .await
            .expect("count COD payment rows");
    assert_eq!(cod_payment_rows, 0, "COD orders have no payments row");

    // 9. Second app, dev mode OFF and no credentials (the production
    // reality today): bKash is honestly unavailable, the simulate
    // route 404s, and a bKash checkout is a 503 that leaves the
    // buyer's cart intact.
    let prod_state = AppState::new(Some(pool.clone()), "wave5-test-secret".to_string(), true);
    let prod_app = build_app(prod_state);
    let (status, methods) = call(&prod_app, Method::GET, "/v1/payments/methods", None, None).await;
    assert_eq!(status, StatusCode::OK, "prod methods: {methods}");
    let bkash = methods
        .as_array()
        .expect("methods array")
        .iter()
        .find(|m| m["method"] == "bkash")
        .expect("bkash listed");
    assert_eq!(bkash["available"], false, "prod bkash unavailable: {bkash}");
    assert_eq!(bkash["reason"], "not_configured");

    let simulate_uri = format!("/v1/payments/{payment_c_id}/dev-simulate-success");
    let (status, _) = call(&prod_app, Method::POST, &simulate_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "simulate route hidden without dev mode");

    add_to_cart(&prod_app, &buyer, &product, 1).await;
    let (status, body) = checkout(&prod_app, &buyer, "bkash").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "prod bkash checkout is a 503: {body}"
    );
    let (status, cart) = call(&prod_app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cart after 503: {cart}");
    assert_eq!(
        cart["items"].as_array().expect("cart items").len(),
        1,
        "failed checkout leaves the cart intact: {cart}"
    );
}
