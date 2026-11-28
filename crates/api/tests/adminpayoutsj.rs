//! Admin payouts remake, part J: the seller data export
//! carries the new decision fields, and the queue's
//! possible-duplicate flag follows the twin's lifecycle.
//!
//! The export's payout rows ride the shared `Payout`
//! projection: the remake's columns (per-stage stamps and
//! the payment reference) had to be added to its SELECT or
//! the export would have failed at runtime — this test
//! creates a fully-decided payout and reads it back from
//! the export. The duplicate flag is pinned at both ends:
//! two identical in-flight requests flag each other, and
//! once one is paid the other stands alone.
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

#[tokio::test]
async fn admin_payouts_export_and_duplicate_flag() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin payouts remake test J");
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
        "adminpayoutsj-test-secret".to_string(),
        true,
    )
    .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let seller = login(&app, "01710101010").await;
    let store = create_store(&app, &seller, "adminpayoutsj-store").await;
    let product = create_product(&app, &seller, &store, "Remake J Product", 200_000, 10).await;
    let buyer = login(&app, "01910101010").await;
    let admin = make_admin(&app, &pool, "01710000010", "+8801710000010").await;

    buy_and_deliver(&app, &buyer, &seller, &product).await;
    save_account(&app, &seller, "01710101010").await;

    // Twin requests: same store, same amount, both in flight.
    let (status, payout) = request_payout(&app, &seller, 60_000).await;
    assert_eq!(status, StatusCode::CREATED, "request 1: {payout}");
    let payout1 = payout["id"].as_str().expect("payout id").to_string();
    let (status, payout) = request_payout(&app, &seller, 60_000).await;
    assert_eq!(status, StatusCode::CREATED, "request 2: {payout}");
    let payout2 = payout["id"].as_str().expect("payout id").to_string();

    let (status, queue) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?status=requested",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "queue: {queue}");
    assert_eq!(queue["total"], 2, "queue: {queue}");
    for item in queue["items"].as_array().expect("items") {
        assert_eq!(item["possible_duplicate"], true, "twins flag: {item}");
    }

    // Decide the first one fully.
    let uri = format!("/v1/admin/payouts/{payout1}/approve");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "approve: {body}");
    let uri = format!("/v1/admin/payouts/{payout1}/mark-paid");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "payment_reference": "EXPORT-TXN-1" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mark-paid: {body}");

    // The second twin now stands alone.
    let (status, queue) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?status=requested",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "queue: {queue}");
    assert_eq!(queue["total"], 1, "queue: {queue}");
    assert_eq!(queue["items"][0]["id"], payout2, "queue: {queue}");
    assert_eq!(queue["items"][0]["possible_duplicate"], false, "queue: {queue}");

    // The seller export carries the decided payout with its
    // reference and stamps.
    let (status, export) = call(&app, Method::GET, "/v1/seller/export", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "export: {export}");
    let exported = export["payouts"]
        .as_array()
        .expect("exported payouts")
        .iter()
        .find(|p| p["id"] == json!(payout1))
        .expect("paid payout exported")
        .clone();
    assert_eq!(exported["status"], "paid", "exported: {exported}");
    assert_eq!(exported["payment_reference"], "EXPORT-TXN-1", "exported: {exported}");
    assert!(exported["approved_at"].is_string(), "exported: {exported}");
    assert!(exported["paid_at"].is_string(), "exported: {exported}");
    assert!(exported["rejected_at"].is_null(), "exported: {exported}");
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
