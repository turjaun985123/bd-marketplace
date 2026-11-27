//! Wave 55e integration test: order tracking v2 — the
//! timeline. A COD order is driven through the real DEV
//! courier flow to delivered; the buyer's order view and
//! the PUBLIC tracking view must then carry the same
//! per-sub-order event sequence (placed → confirmed →
//! shipment_booked → courier statuses → delivered), with
//! timestamps only where the platform records them (the
//! seller's confirm carries `at: null` — no status-change
//! timestamp exists, and none is invented). The order-level
//! timeline on the public view starts with order_placed.
//! A return request and the seller's decision then append
//! their events with their real timestamps.
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

/// The event kinds of one timeline, in order.
fn kinds(timeline: &Value) -> Vec<String> {
    timeline
        .as_array()
        .expect("timeline array")
        .iter()
        .map(|event| event["kind"].as_str().expect("event kind").to_string())
        .collect()
}

/// Find one event in a timeline by kind.
fn event_of<'a>(timeline: &'a Value, kind: &str) -> Option<&'a Value> {
    timeline
        .as_array()
        .expect("timeline array")
        .iter()
        .find(|event| event["kind"] == kind)
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55e integration tests");
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
    let state = AppState::new(Some(pool.clone()), "wave55e-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

#[tokio::test]
async fn wave55e_tracking_timeline_flow() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01766666666").await;
    let store = create_store(&app, &seller, "wave55e-store").await;
    let product = create_product(&app, &seller, &store, "Wave55E Product", 50_000, 5).await;
    let buyer = login(&app, "01966666666").await;

    // Buy, confirm, book the DEV courier, poll to delivered
    // (the Wave 9 walk).
    add_to_cart(&app, &buyer, &product, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(&app, &seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(&app, Method::POST, &uri, Some(json!({})), Some(&seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    let consignment_id = shipment["consignment_id"]
        .as_str()
        .expect("consignment id")
        .to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }

    // 1. The buyer order view: the full sequence, in order,
    // with honest timestamps.
    let uri = format!("/v1/orders/{order_id}");
    let (status, order) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer order: {order}");
    assert_eq!(order["sub_orders"][0]["status"], "delivered");
    let timeline = &order["sub_orders"][0]["timeline"];
    let sequence = kinds(timeline);
    assert_eq!(sequence.first().map(String::as_str), Some("placed"));
    assert_eq!(sequence.last().map(String::as_str), Some("delivered"));
    let positions = |kind: &str| sequence.iter().position(|k| k == kind);
    assert!(
        positions("placed") < positions("confirmed")
            && positions("confirmed") < positions("shipment_booked")
            && positions("shipment_booked") < positions("delivered"),
        "events in logical order: {sequence:?}"
    );
    assert!(
        sequence.iter().filter(|k| *k == "shipment_status").count() >= 1,
        "the courier leg is on the timeline: {sequence:?}"
    );
    let placed = event_of(timeline, "placed").expect("placed event");
    assert!(!placed["at"].is_null(), "placement has its real time");
    let confirmed = event_of(timeline, "confirmed").expect("confirmed event");
    assert!(
        confirmed["at"].is_null(),
        "the confirm has NO recorded timestamp — null, never invented"
    );
    let booked = event_of(timeline, "shipment_booked").expect("booked event");
    assert!(!booked["at"].is_null(), "booking has its real time");
    assert!(
        booked["note"]
            .as_str()
            .expect("booking note")
            .contains(&consignment_id),
        "the booking note names the consignment: {booked}"
    );
    let delivered = event_of(timeline, "delivered").expect("delivered event");
    assert!(
        !delivered["at"].is_null(),
        "the courier reported the delivery, so its time is real"
    );

    // 2. The public tracking view carries the same
    // sub-order sequence plus the order-level timeline.
    let uri = format!("/v1/track?order_id={order_id}&phone=01999999999");
    let (status, tracked) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "public track: {tracked}");
    assert_eq!(
        kinds(&tracked["sub_orders"][0]["timeline"]),
        sequence,
        "public and buyer timelines agree"
    );
    let order_kinds = kinds(&tracked["timeline"]);
    assert_eq!(
        order_kinds.first().map(String::as_str),
        Some("order_placed"),
        "the order timeline starts at placement: {order_kinds:?}"
    );

    // 3. The return path appends its events: the request
    // (with its real time and live status), then the
    // seller's decision (with its real time).
    let uri = format!("/v1/buyer/sub-orders/{sub_order_id}/return-request");
    let (status, request) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "reason": "পণ্য ছিঁড়ে গেছে" })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "return request: {request}");
    let return_id = request["id"].as_str().expect("return id").to_string();

    let uri = format!("/v1/orders/{order_id}");
    let (status, order) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    let timeline = &order["sub_orders"][0]["timeline"];
    let requested = event_of(timeline, "return_requested").expect("return event");
    assert!(!requested["at"].is_null());
    assert_eq!(requested["status"], "requested");
    assert!(
        event_of(timeline, "return_decided").is_none(),
        "no decision yet — no decision event"
    );

    let uri = format!("/v1/seller/return-requests/{return_id}/approve");
    let (status, approved) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "note": "ফেরত পাঠিয়ে দিন" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve return: {approved}");

    let uri = format!("/v1/orders/{order_id}");
    let (status, order) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    let timeline = &order["sub_orders"][0]["timeline"];
    let decided = event_of(timeline, "return_decided").expect("decision event");
    assert!(!decided["at"].is_null(), "the decision has its real time");
    assert_eq!(decided["status"], "approved");
}
