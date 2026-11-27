//! Wave 44 integration test: payout `Idempotency-Key` — a
//! keyed payout request is safely retryable (same key + same
//! amount returns the ORIGINAL payout with 200 and creates
//! no second row; the balance's pending total moves once),
//! the same key with a different amount is a 409, different
//! keys and keyless requests keep the pre-Wave-44 semantics
//! (one row per request), two CONCURRENT same-key requests
//! land exactly one row, and a malformed key is a 400 before
//! anything is written. The seller's balance comes from the
//! real Wave 9 earning flow (delivered COD sales), copied
//! here since integration test files stay self-contained.
//!
//! The one helper beyond the Wave 9 set is `payout_request`,
//! because the shared `call` helper cannot send an
//! `Idempotency-Key` header (the Wave 43 test added its own
//! header helper for the same reason).
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.
//! CI provides a Postgres 16 service and sets `DATABASE_URL`
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

/// Move one sub-order to a new status as its seller.
async fn set_sub_order_status(
    app: &Router,
    token: &str,
    sub_order_id: &str,
    status: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/status");
    call(
        app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": status })),
        Some(token),
    )
    .await
}

/// Buy one unit of the product and drive the order all the way to
/// delivered through the DEV courier; returns the order id.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    let (status, body) = set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");

    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(json!({})), Some(seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();

    // Four polls walk the DEV courier created -> delivered.
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(app, Method::POST, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
}

/// GET the seller balance for a token.
async fn get_balance(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/seller/balance", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "balance: {body}");
    body
}

/// POST a payout request, carrying an `Idempotency-Key` header
/// when `key` is `Some`. (The one helper beyond the Wave 9 set:
/// the shared `call` cannot send headers.)
async fn payout_request(
    app: &Router,
    token: &str,
    amount_poisha: i64,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/v1/seller/payouts")
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");
    if let Some(key) = key {
        builder = builder.header("Idempotency-Key", key);
    }
    let request = builder
        .body(Body::from(
            json!({ "amount_poisha": amount_poisha }).to_string(),
        ))
        .expect("request builds");
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

/// The seller's payout-list total (the row count the API reports).
async fn payout_total(app: &Router, token: &str) -> i64 {
    let (status, list) = call(app, Method::GET, "/v1/seller/payouts", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "payout list: {list}");
    list["total"].as_i64().expect("list total")
}

#[tokio::test]
async fn wave44_payout_idempotency_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 44 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_variants, products, store_members, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode (deliveries) on.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave44-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // The earning seller and a plain buyer. Three delivered units
    // of the 200_000-poisha product earn 3 x 184_000 = 552_000
    // (8% commission), enough headroom for every step below.
    let seller = login(&app, "01744000001").await;
    let store = create_store(&app, &seller, "wave44-store").await;
    let product = create_product(&app, &seller, &store, "Wave44 Product", 200_000, 10).await;
    let buyer = login(&app, "01944000001").await;

    buy_and_deliver(&app, &buyer, &seller, &product).await;
    buy_and_deliver(&app, &buyer, &seller, &product).await;
    buy_and_deliver(&app, &buyer, &seller, &product).await;

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 552_000, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 552_000);

    // The payout account every request below snapshots.
    let (status, account) = call(
        &app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "Wave44 Seller",
            "account_number": "01744000001"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save payout account: {account}");

    // 1. A malformed key (200 characters, over the 128 cap) is a
    // 400 BEFORE anything is written: no payout row exists.
    let oversized = "k".repeat(200);
    let (status, body) = payout_request(&app, &seller, 100_000, Some(&oversized)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "oversized key: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("Idempotency-Key"),
        "key message: {body}"
    );
    assert_eq!(payout_total(&app, &seller).await, 0);

    // 2. First keyed request: 201, the key is stored and echoed.
    let (status, payout) = payout_request(&app, &seller, 100_000, Some("wave44-key-a")).await;
    assert_eq!(status, StatusCode::CREATED, "first keyed payout: {payout}");
    assert_eq!(payout["amount_poisha"], 100_000);
    assert_eq!(payout["idempotency_key"], "wave44-key-a");
    let payout_a = payout["id"].as_str().expect("payout id").to_string();

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["pending_poisha"], 100_000, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 452_000);

    // 3. The retry — same key, same amount: 200 with the SAME
    // payout id, still exactly one row, and the pending total
    // has NOT moved a second time.
    let (status, replay) = payout_request(&app, &seller, 100_000, Some("wave44-key-a")).await;
    assert_eq!(status, StatusCode::OK, "idempotent replay: {replay}");
    assert_eq!(replay["id"], payout_a, "replay returns the original: {replay}");
    assert_eq!(replay["idempotency_key"], "wave44-key-a");
    assert_eq!(payout_total(&app, &seller).await, 1);

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["pending_poisha"], 100_000, "pending moved once: {balance}");
    assert_eq!(balance["available_poisha"], 452_000);

    // 4. Same key, DIFFERENT amount: a 409, and still one row.
    let (status, body) = payout_request(&app, &seller, 60_000, Some("wave44-key-a")).await;
    assert_eq!(status, StatusCode::CONFLICT, "key reuse, other amount: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("Idempotency-Key"),
        "conflict message: {body}"
    );
    assert_eq!(payout_total(&app, &seller).await, 1);

    // 5. A different key is a genuinely new request: 201, a
    // distinct row.
    let (status, payout_b) = payout_request(&app, &seller, 100_000, Some("wave44-key-b")).await;
    assert_eq!(status, StatusCode::CREATED, "second key: {payout_b}");
    assert_ne!(payout_b["id"], payout_a);
    assert_eq!(payout_total(&app, &seller).await, 2);

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["pending_poisha"], 200_000, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 352_000);

    // 6. No key at all, twice: today's behaviour is preserved —
    // two separate rows, and the key field reads back null.
    let (status, plain1) = payout_request(&app, &seller, 100_000, None).await;
    assert_eq!(status, StatusCode::CREATED, "keyless 1: {plain1}");
    assert!(plain1["idempotency_key"].is_null(), "keyless echo: {plain1}");
    let (status, plain2) = payout_request(&app, &seller, 100_000, None).await;
    assert_eq!(status, StatusCode::CREATED, "keyless 2: {plain2}");
    assert_ne!(plain1["id"], plain2["id"]);
    assert_eq!(payout_total(&app, &seller).await, 4);

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["pending_poisha"], 400_000, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 152_000);

    // 7. Two CONCURRENT requests with the same fresh key:
    // exactly one row lands, one caller sees 201 and the other
    // the 200 replay, and both bodies carry that row's id.
    let first = payout_request(&app, &seller, 100_000, Some("wave44-key-c"));
    let second = payout_request(&app, &seller, 100_000, Some("wave44-key-c"));
    let ((status1, body1), (status2, body2)) = tokio::join!(first, second);
    let created_count = [status1, status2]
        .iter()
        .filter(|status| **status == StatusCode::CREATED)
        .count();
    let replay_count = [status1, status2]
        .iter()
        .filter(|status| **status == StatusCode::OK)
        .count();
    assert_eq!(
        (created_count, replay_count),
        (1, 1),
        "one create + one replay: {body1} / {body2}"
    );
    assert_eq!(body1["id"], body2["id"], "both name the one row: {body1} / {body2}");
    assert_eq!(payout_total(&app, &seller).await, 5);

    let keyed_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM payouts WHERE idempotency_key = 'wave44-key-c'",
    )
    .fetch_one(&pool)
    .await
    .expect("count keyed rows");
    assert_eq!(keyed_rows, 1, "exactly one row carries the raced key");

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["pending_poisha"], 500_000, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 52_000);
}
