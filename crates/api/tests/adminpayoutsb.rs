//! Admin payouts remake, part B: a rejection REQUIRES a
//! reason.
//!
//! Before the remake the reason was optional at every
//! layer: an empty body stored an empty note, the seller
//! could not see it, and the web's `window.prompt` fired
//! the rejection even when the admin pressed Cancel. Now
//! the transition validates the reason before any database
//! work: no body, a missing field, a blank or whitespace
//! reason, and an over-long reason are all the same 400,
//! and NOTHING is written — the payout stays `requested`,
//! no audit row, no notification, the seller's hold
//! untouched. A valid reason is trimmed, stored, readable
//! by the seller, audited, and carried in the
//! `payout_rejected` notice.
//!
//! Helpers are copied verbatim from the Wave 9 test, since
//! integration test files stay self-contained. Gated on
//! `DATABASE_URL`, like every integration test here.

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

/// Count notifications of one kind (Wave 29 helper).
async fn count_kind(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE kind = $1")
        .bind(kind)
        .fetch_one(pool)
        .await
        .expect("count notifications by kind")
}

/// The latest notification body of one kind (Wave 29 helper).
async fn latest_notice_body(pool: &PgPool, kind: &str) -> String {
    sqlx::query_scalar(
        "SELECT body FROM notifications WHERE kind = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(kind)
    .fetch_one(pool)
    .await
    .expect("load latest notification body")
}

#[tokio::test]
async fn admin_payouts_reject_requires_a_reason() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin payouts remake test B");
            return;
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
         payments, order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
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
        "adminpayoutsb-test-secret".to_string(),
        true,
    )
    .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "adminpayoutsb-store").await;
    let product = create_product(&app, &seller, &store, "Remake B Product", 200_000, 10).await;
    let buyer = login(&app, "01922222222").await;
    let admin = make_admin(&app, &pool, "01720000002", "+8801720000002").await;

    buy_and_deliver(&app, &buyer, &seller, &product).await;
    save_account(&app, &seller, "01722222222").await;

    let (status, payout) = request_payout(&app, &seller, 100_000).await;
    assert_eq!(status, StatusCode::CREATED, "request payout: {payout}");
    let payout_id = payout["id"].as_str().expect("payout id").to_string();
    let reject_uri = format!("/v1/admin/payouts/{payout_id}/reject");

    // Every shape of "no reason" is the same 400.
    let (status, body) = call(&app, Method::POST, &reject_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "reject, no body: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &reject_uri,
        Some(json!({})),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "reject, no note field: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &reject_uri,
        Some(json!({ "note": "" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "reject, empty note: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &reject_uri,
        Some(json!({ "note": "   " })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "reject, whitespace note: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &reject_uri,
        Some(json!({ "note": "x".repeat(501) })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "reject, over-long note: {body}");

    // Nothing was written by any of those: the payout is
    // still requested, the hold is intact, no audit row,
    // no notification.
    let (status, queue) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?status=requested",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "queue: {queue}");
    assert_eq!(queue["total"], 1, "payout still queued: {queue}");
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["pending_poisha"], 100_000, "hold intact: {balance}");
    let audit_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_audit_log WHERE action = 'payout.reject'",
    )
    .fetch_one(&pool)
    .await
    .expect("count reject audit rows");
    assert_eq!(audit_rows, 0, "no audit row for refused rejections");
    assert_eq!(count_kind(&pool, "payout_rejected").await, 0);

    // A real reason: trimmed, stored, seller-readable,
    // audited, and carried in the notice.
    let (status, body) = call(
        &app,
        Method::POST,
        &reject_uri,
        Some(json!({ "note": "  অ্যাকাউন্ট নম্বর মিলছে না  " })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reject with reason: {body}");
    assert_eq!(body["status"], "rejected");
    assert_eq!(body["note"], "অ্যাকাউন্ট নম্বর মিলছে না", "trimmed: {body}");
    assert!(body["rejected_at"].is_string(), "rejected_at: {body}");

    let (status, mine) = call(&app, Method::GET, "/v1/seller/payouts", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "seller payouts: {mine}");
    assert_eq!(mine["items"][0]["note"], "অ্যাকাউন্ট নম্বর মিলছে না");

    let audit_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_audit_log WHERE action = 'payout.reject' AND target_id = $1",
    )
    .bind(&payout_id)
    .fetch_one(&pool)
    .await
    .expect("count reject audit rows");
    assert_eq!(audit_rows, 1, "the rejection is audited");

    assert_eq!(count_kind(&pool, "payout_rejected").await, 1);
    let notice = latest_notice_body(&pool, "payout_rejected").await;
    assert!(
        notice.contains("অ্যাকাউন্ট নম্বর মিলছে না"),
        "notice carries the reason: {notice}"
    );
    assert!(
        notice.contains("ফেরত গেছে"),
        "notice says the balance was freed: {notice}"
    );

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["pending_poisha"], 0, "hold freed: {balance}");
    assert_eq!(balance["available_poisha"], 184_000, "balance: {balance}");
}

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
/// Log in, then promote to admin by SQL (login FIRST — the
/// promotion must land on the already-created user row).
async fn make_admin(app: &Router, pool: &PgPool, phone_local: &str, phone_canonical: &str) -> String {
    let token = login(app, phone_local).await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = $1")
        .bind(phone_canonical)
        .execute(pool)
        .await
        .expect("promote admin");
    token
}
/// Save the seller's bKash payout account.
async fn save_account(app: &Router, seller: &str, number: &str) {
    let (status, body) = call(
        app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "Remake Seller",
            "account_number": number
        })),
        Some(seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save payout account: {body}");
}
/// Request a payout as the seller.
async fn request_payout(app: &Router, seller: &str, amount: i64) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": amount })),
        Some(seller),
    )
    .await
}
