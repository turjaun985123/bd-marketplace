//! Admin shipments remake, test D: the admin refresh
//! action. It delegates to the SAME shared core the
//! worker poller and the participant endpoint run — four
//! admin refreshes walk a DEV shipment created →
//! delivered exactly like participant polls would — and
//! every ADVANCE is written to the admin audit log in
//! the refresh's own transaction (`shipment.refresh`,
//! detail {from, to, …}). An unchanged refresh writes NO
//! audit row (the platform's no-op rule), and a manual
//! shipment answers an honest `unchanged` — nothing
//! exists to poll — again with no audit row. Helpers are
//! copied from the Wave 9 / admin orders tests on
//! purpose.
//!
//! Gated on `DATABASE_URL`: without it the test returns
//! early. CI provides a Postgres 16 service.

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
    auth: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = auth {
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
async fn create_store(app: &Router, auth: &str, slug: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body["id"].as_str().expect("store id").to_string()
}

/// Create an active product; returns the product id.
async fn create_product(
    app: &Router,
    auth: &str,
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
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Add a product to the buyer's cart.
async fn add_to_cart(app: &Router, auth: &str, product_id: &str, quantity: i64) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_id, "quantity": quantity })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {body}");
}

/// Checkout the buyer's current cart with COD; returns the order.
async fn checkout_cod(app: &Router, auth: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01888888888",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {body}");
    body
}

/// Set a sub-order's seller-side status (the Wave 9 walk).
async fn set_sub_order_status(app: &Router, auth: &str, sub_order_id: &str, status: &str) {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/status");
    let (code, body) = call(
        app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": status })),
        Some(auth),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "set sub-order status {status}: {body}");
}

/// Book a shipment for a confirmed sub-order; returns the shipment.
async fn book_shipment(app: &Router, auth: &str, sub_order_id: &str, body: Value) -> Value {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(body), Some(auth)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    shipment
}

/// Buy one product and book its shipment; returns the shipment id.
async fn buy_and_book(
    app: &Router,
    buyer: &str,
    seller: &str,
    product_id: &str,
    booking: Value,
) -> String {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    let shipment = book_shipment(app, seller, &sub_order_id, booking).await;
    shipment["id"].as_str().expect("shipment id").to_string()
}

/// POST the admin refresh for one shipment.
async fn admin_refresh(app: &Router, auth: &str, shipment_id: &str) -> (StatusCode, Value) {
    let uri = format!("/v1/admin/shipments/{shipment_id}/refresh");
    call(app, Method::POST, &uri, Some(json!({})), Some(auth)).await
}

/// How many audit rows record an admin refresh of one shipment.
async fn refresh_audit_count(pool: &PgPool, shipment_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_audit_log \
         WHERE action = 'shipment.refresh' AND target_id = $1",
    )
    .bind(shipment_id)
    .fetch_one(pool)
    .await
    .expect("count refresh audit rows")
}

#[tokio::test]
async fn adminshipments_admin_refresh_advances_and_audits() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin shipments remake test D");
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
    let state = AppState::new(Some(pool.clone()), "adminshipmentsd-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111114").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111114'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01722222226").await;
    let store = create_store(&app, &seller, "dship-alpha").await;
    let product = create_product(&app, &seller, &store, "Ship D", 150_000, 20).await;
    let buyer = login(&app, "01999999996").await;

    // Four admin refreshes walk the DEV shipment to
    // delivered — the same progression participant polls
    // produce, because it is the same function.
    let ship = buy_and_book(&app, &buyer, &seller, &product, json!({})).await;
    for expected in ["picked_up", "in_transit", "out_for_delivery", "delivered"] {
        let (status, body) = admin_refresh(&app, &admin, &ship).await;
        assert_eq!(status, StatusCode::OK, "admin refresh: {body}");
        assert_eq!(body["outcome"], "advanced", "refresh advances: {body}");
        assert_eq!(body["shipment"]["status"], expected, "walk: {body}");
        assert!(
            !body["shipment"]["last_polled_at"].is_null(),
            "the advance stamps the poll: {body}"
        );
    }
    assert_eq!(refresh_audit_count(&pool, &ship).await, 4);

    // The detail shows the full history: five events, four
    // audit entries, the sub-order delivered.
    let detail_uri = format!("/v1/admin/shipments/{ship}");
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["status"], "delivered");
    assert_eq!(detail["sub_order_status"], "delivered");
    let events = detail["events"].as_array().expect("events");
    assert_eq!(events.len(), 5, "create + four refresh events: {detail}");
    assert_eq!(events[1]["source"], "refresh");
    let audit = detail["audit_entries"].as_array().expect("audit entries");
    assert_eq!(audit.len(), 4, "one entry per advance: {detail}");
    assert_eq!(audit[0]["action"], "shipment.refresh");
    // Newest first: the last advance delivered the parcel.
    assert_eq!(audit[0]["detail"]["from"], "out_for_delivery");
    assert_eq!(audit[0]["detail"]["to"], "delivered");
    assert_eq!(audit[0]["detail"]["courier"], "steadfast");

    // A fifth refresh is an honest unchanged: the courier
    // confirms delivery, nothing moves, NO audit row is
    // written — and the buyer is not notified twice.
    let (status, body) = admin_refresh(&app, &admin, &ship).await;
    assert_eq!(status, StatusCode::OK, "fifth refresh: {body}");
    assert_eq!(body["outcome"], "unchanged");
    assert_eq!(body["shipment"]["status"], "delivered");
    assert_eq!(refresh_audit_count(&pool, &ship).await, 4);
    let delivered_notices: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE kind = 'sub_order_delivered'",
    )
    .fetch_one(&pool)
    .await
    .expect("count delivered notices");
    assert_eq!(delivered_notices, 1, "delivery notified exactly once");

    // A manual shipment refreshes to an honest unchanged:
    // no state change, no audit row, no poll stamp.
    let manual = buy_and_book(
        &app,
        &buyer,
        &seller,
        &product,
        json!({ "courier": "manual", "manual_courier": "sundarban" }),
    )
    .await;
    let (status, body) = admin_refresh(&app, &admin, &manual).await;
    assert_eq!(status, StatusCode::OK, "manual refresh: {body}");
    assert_eq!(body["outcome"], "unchanged");
    assert_eq!(body["shipment"]["status"], "created");
    assert!(
        body["shipment"]["last_polled_at"].is_null(),
        "manual is never polled: {body}"
    );
    assert_eq!(refresh_audit_count(&pool, &manual).await, 0);
}
