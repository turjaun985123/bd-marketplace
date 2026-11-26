//! Wave 6 integration test: courier shipments — courier
//! availability, booking a shipment for a confirmed sub-order with
//! the DEV courier simulator, the one-shipment-per-sub-order rule,
//! tracking refresh progressing through the unified statuses and
//! syncing the sub-order to delivered, participant-only refresh, and
//! the honest production behaviour (no credentials, no dev mode:
//! booking is a 503). Driven through the real Axum router with tower
//! oneshot. The small helpers are copied from the Wave 5 test on
//! purpose: integration test files stay self-contained rather than
//! sharing test code across files.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.
//!
//! The courier DEV simulator is ON for the main app in this test
//! (explicit dev flag) — that is the only way a shipment can be
//! booked here, and every consignment id it mints is prefixed
//! `DEV-`. A second app instance with dev mode OFF covers the
//! production behaviour.

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
use uuid::Uuid;

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
            "delivery_phone": "01944444444",
            "delivery_address": "House 4, Road 6, Sylhet",
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

/// Book a shipment for a sub-order as the given user.
async fn book_shipment(
    app: &Router,
    token: &str,
    sub_order_id: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    call(app, Method::POST, &uri, Some(json!({})), Some(token)).await
}

/// Refresh one shipment's tracking as the given user.
async fn refresh_shipment(
    app: &Router,
    token: &str,
    shipment_id: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/shipments/{shipment_id}/refresh");
    call(app, Method::POST, &uri, None, Some(token)).await
}

/// The buyer's view of one order.
async fn get_order(app: &Router, token: &str, order_id: &str) -> Value {
    let uri = format!("/v1/orders/{order_id}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "get order: {body}");
    body
}

#[tokio::test]
async fn wave6_courier_shipments_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 6 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE shipment_events, shipments, payment_events, payments, \
         order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // Main app: SMS dev mode (for OTP login) AND courier dev mode on.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave6-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // One seller, one store, one 500 BDT product (default 1 kg).
    let seller = login(&app, "01744444444").await;
    let store = create_store(&app, &seller, "wave6-store").await;
    let product = create_product(&app, &seller, &store, "Wave6 Product", 50_000, 10).await;
    let buyer = login(&app, "01944444444").await;
    let stranger = login(&app, "01644444444").await;
    let other_seller = login(&app, "01544444444").await;

    // 1. Courier availability with dev mode on: Steadfast is the
    // default and is honestly labelled dev; every known courier is
    // served by the simulator.
    let (status, couriers) = call(&app, Method::GET, "/v1/couriers", None, None).await;
    assert_eq!(status, StatusCode::OK, "couriers: {couriers}");
    let courier_list = couriers.as_array().expect("couriers array");
    assert_eq!(courier_list.len(), 5, "five known couriers: {couriers}");
    let steadfast = courier_list
        .iter()
        .find(|c| c["courier"] == "steadfast")
        .expect("steadfast listed");
    assert_eq!(steadfast["available"], true, "dev steadfast: {steadfast}");
    assert_eq!(steadfast["mode"], "dev");
    assert_eq!(steadfast["is_default"], true);

    // 2. COD checkout: goods 50,000 + delivery 6,000 (inside Dhaka,
    // 1 kg) = 56,000 poisha. The sub-order has no shipment yet.
    add_to_cart(&app, &buyer, &product, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
    let sub_order = &order["sub_orders"][0];
    let sub_order_id = sub_order["id"].as_str().expect("sub-order id").to_string();
    assert_eq!(sub_order["status"], "placed");
    assert!(sub_order["shipment"].is_null(), "no shipment yet: {sub_order}");

    // 3. Booking before the seller confirms is a 409.
    let (status, body) = book_shipment(&app, &seller, &sub_order_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "book while placed: {body}");

    // 4. Seller confirms; a DIFFERENT seller still cannot book for
    // this sub-order (403 — not their store).
    let (status, body) = set_sub_order_status(&app, &seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let (status, body) = book_shipment(&app, &other_seller, &sub_order_id).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "book by non-owner: {body}");

    // 5. The owner books with the default courier: 201, a DEV
    // consignment id, status created, and the COD amount is the
    // sub-order's goods + delivery share (56,000 poisha).
    let (status, shipment) = book_shipment(&app, &seller, &sub_order_id).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    assert_eq!(shipment["courier"], "steadfast");
    assert_eq!(shipment["status"], "created");
    assert_eq!(shipment["cod_amount_poisha"], 56_000);
    let expected_consignment = format!("DEV-steadfast-{sub_order_id}");
    assert_eq!(shipment["consignment_id"], expected_consignment);
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();

    // 6. One shipment per sub-order: a second booking is a 409.
    let (status, body) = book_shipment(&app, &seller, &sub_order_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "duplicate booking: {body}");

    // 7. The buyer's order detail shows the sub-order as shipped
    // with the shipment summary attached.
    let order = get_order(&app, &buyer, &order_id).await;
    let sub_order = &order["sub_orders"][0];
    assert_eq!(sub_order["status"], "shipped", "sub-order shipped: {sub_order}");
    assert_eq!(sub_order["shipment"]["id"], shipment_id);
    assert_eq!(sub_order["shipment"]["courier"], "steadfast");
    assert_eq!(sub_order["shipment"]["consignment_id"], expected_consignment);
    assert_eq!(sub_order["shipment"]["status"], "created");

    // 8. A stranger (neither seller nor buyer) cannot refresh: 404.
    let (status, body) = refresh_shipment(&app, &stranger, &shipment_id).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger refresh: {body}");

    // 9. Refreshes progress through the unified statuses — the
    // seller and the buyer may both poll — and the final delivery
    // syncs the sub-order to delivered.
    let (status, shipment) = refresh_shipment(&app, &seller, &shipment_id).await;
    assert_eq!(status, StatusCode::OK, "refresh 1: {shipment}");
    assert_eq!(shipment["status"], "picked_up");
    let (status, shipment) = refresh_shipment(&app, &buyer, &shipment_id).await;
    assert_eq!(status, StatusCode::OK, "refresh 2 (buyer): {shipment}");
    assert_eq!(shipment["status"], "in_transit");
    let (status, shipment) = refresh_shipment(&app, &seller, &shipment_id).await;
    assert_eq!(status, StatusCode::OK, "refresh 3: {shipment}");
    assert_eq!(shipment["status"], "out_for_delivery");
    let (status, shipment) = refresh_shipment(&app, &seller, &shipment_id).await;
    assert_eq!(status, StatusCode::OK, "refresh 4: {shipment}");
    assert_eq!(shipment["status"], "delivered");

    let order = get_order(&app, &buyer, &order_id).await;
    let sub_order = &order["sub_orders"][0];
    assert_eq!(sub_order["status"], "delivered", "sub-order delivered: {sub_order}");
    assert_eq!(sub_order["shipment"]["status"], "delivered");

    // The audit trail holds the create event plus one refresh event
    // per applied change.
    let shipment_uuid = Uuid::parse_str(&shipment_id).expect("shipment id is a uuid");
    let event_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM shipment_events WHERE shipment_id = $1")
            .bind(shipment_uuid)
            .fetch_one(&pool)
            .await
            .expect("count shipment events");
    assert_eq!(event_count, 5, "create + 4 refresh events");

    // 10. Second app, dev mode OFF and no Steadfast credentials (the
    // production reality today): Steadfast is honestly unavailable,
    // and booking for a freshly confirmed sub-order is a 503.
    let prod_state = AppState::new(Some(pool.clone()), "wave6-test-secret".to_string(), true);
    let prod_app = build_app(prod_state);
    let (status, couriers) = call(&prod_app, Method::GET, "/v1/couriers", None, None).await;
    assert_eq!(status, StatusCode::OK, "prod couriers: {couriers}");
    let steadfast = couriers
        .as_array()
        .expect("couriers array")
        .iter()
        .find(|c| c["courier"] == "steadfast")
        .expect("steadfast listed");
    assert_eq!(steadfast["available"], false, "prod steadfast: {steadfast}");
    assert_eq!(steadfast["reason"], "not_configured");

    add_to_cart(&prod_app, &buyer, &product, 1).await;
    let prod_order = checkout_cod(&prod_app, &buyer).await;
    let prod_sub_order_id = prod_order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) =
        set_sub_order_status(&prod_app, &seller, &prod_sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "prod confirm: {body}");
    let (status, body) = book_shipment(&prod_app, &seller, &prod_sub_order_id).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "prod booking is a 503: {body}"
    );
}
