//! Wave 55n integration test: the public tracking
//! surface's privacy contract + the online-payment order
//! timeline. Tracking needs BOTH the order id and the
//! delivery phone: a wrong phone is a 404 (the endpoint
//! confirms nothing to a guesser), a missing phone is a
//! 400, an unknown order is a 404. A bKash (online) order's
//! tracked timeline carries the payment event beside
//! `order_placed`, and after the courier walk the tracked
//! sub-order shows the consignment id and a
//! `shipment_booked` event naming it.
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
use payments::PaymentsConfig;
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

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<Router> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55n integration tests");
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
    // Payments DEV simulator on (the wave17 pattern): the
    // online-payment half of this test settles through the
    // dev gateway; without it bkash is honestly
    // not-configured and the checkout refuses.
    let state = AppState::new(Some(pool), "wave55n-test-secret".to_string(), true)
        .with_payments_config(PaymentsConfig {
            dev_mode: true,
            ..PaymentsConfig::default()
        })
        .with_couriers_config(dev_couriers);
    Some(build_app(state))
}

#[tokio::test]
async fn wave55n_public_tracking_privacy_and_online_payment_timeline() {
    let Some(app) = test_app().await else {
        return;
    };
    let seller = login(&app, "01721212121").await;
    let store = create_store(&app, &seller, "wave55n-store").await;
    let product = create_product(&app, &seller, &store, "Tracked Product", 20_000, 10).await;
    let buyer = login(&app, "01921212121").await;

    // An ONLINE (bKash, dev gateway) order.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 1, Road 1, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "bkash"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "online checkout: {order}");
    let order_id = order["id"].as_str().expect("order id").to_string();
    let sub_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    // --- Privacy: the phone is half the credential. ---------
    let uri = format!("/v1/track?order_id={order_id}&phone=01800000000");
    let (status, refused) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "wrong phone: {refused}");
    let uri = format!("/v1/track?order_id={order_id}");
    let (status, refused) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "missing phone: {refused}");
    let uri = "/v1/track?order_id=00000000-0000-0000-0000-000000000000&phone=01999999999";
    let (status, refused) = call(&app, Method::GET, uri, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown order: {refused}");

    // --- The tracked view: payment event + steps. -----------
    let uri = format!("/v1/track?order_id={order_id}&phone=01999999999");
    let (status, tracked) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "track: {tracked}");
    let order_kinds: Vec<String> = tracked["timeline"]
        .as_array()
        .expect("order timeline")
        .iter()
        .map(|event| event["kind"].as_str().expect("kind").to_string())
        .collect();
    assert_eq!(order_kinds[0], "order_placed");
    assert!(
        order_kinds.iter().any(|kind| kind == "payment_initiated"),
        "the online payment event is tracked: {order_kinds:?}"
    );
    assert_eq!(tracked["sub_orders"][0]["steps"][0]["state"], "current");

    // --- Settle the dev payment (the checkout page's own
    // dev path), then book: the tracked timeline gains the
    // settled event, and the sub-order's the booked one.
    let payment_id = order["payment"]["id"]
        .as_str()
        .expect("payment id on the placed order")
        .to_string();
    let uri = format!("/v1/payments/{payment_id}/dev-simulate-success");
    let (status, payment) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "dev settle: {payment}");
    let uri = format!("/v1/track?order_id={order_id}&phone=01999999999");
    let (status, tracked) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK);
    let order_kinds: Vec<String> = tracked["timeline"]
        .as_array()
        .expect("order timeline")
        .iter()
        .map(|event| event["kind"].as_str().expect("kind").to_string())
        .collect();
    assert!(
        order_kinds.iter().any(|kind| kind == "payment_paid"),
        "the settled event is tracked too: {order_kinds:?}"
    );

    let (status, body) = set_sub_order_status(&app, &seller, &sub_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub_id}/shipment");
    let (status, shipment) = call(&app, Method::POST, &uri, Some(json!({})), Some(&seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let consignment = shipment["consignment_id"]
        .as_str()
        .expect("consignment id")
        .to_string();

    let uri = format!("/v1/track?order_id={order_id}&phone=01999999999");
    let (status, tracked) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "track after booking: {tracked}");
    let sub = &tracked["sub_orders"][0];
    assert_eq!(sub["shipment"]["consignment_id"], consignment);
    let booked = sub["timeline"]
        .as_array()
        .expect("timeline")
        .iter()
        .find(|event| event["kind"] == "shipment_booked")
        .expect("booked event")
        .clone();
    assert!(
        booked["note"]
            .as_str()
            .expect("booked note")
            .contains(&consignment),
        "the booked event names the consignment: {booked}"
    );
}
