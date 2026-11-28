//! Admin shipments remake, test E: manual shipments.
//! A manual booking's receipt number becomes its
//! consignment id; without one the platform generates a
//! `MANUAL-…` placeholder the admin can recognise (and
//! search) as "no receipt recorded". Seller-entered
//! status moves land in the admin detail as `manual`
//! events — visibly distinct from courier-polled ones,
//! because a manual move is seller self-attestation.
//! Manual shipments are never polled: `last_polled_at`
//! stays null, they join neither poll queue even when
//! ancient — but they DO age. Helpers are copied from
//! the Wave 9 / admin orders tests on purpose.
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

/// Buy one product and book its shipment; returns the shipment.
async fn buy_and_book(
    app: &Router,
    buyer: &str,
    seller: &str,
    product_id: &str,
    booking: Value,
) -> Value {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    book_shipment(app, seller, &sub_order_id, booking).await
}

/// Move a manual shipment by seller entry.
async fn manual_status(app: &Router, auth: &str, shipment_id: &str, status: &str) {
    let uri = format!("/v1/seller/shipments/{shipment_id}/manual-status");
    let (code, body) = call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "status": status })),
        Some(auth),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "manual status {status}: {body}");
}

/// Age a shipment's clocks and events together.
async fn age_shipment_fully(pool: &PgPool, shipment_id: &str, interval: &str) {
    let sql = format!(
        "UPDATE shipments SET created_at = now() - interval '{interval}', \
         updated_at = now() - interval '{interval}' WHERE id = $1::uuid"
    );
    sqlx::query(&sql)
        .bind(shipment_id)
        .execute(pool)
        .await
        .expect("age shipment clocks");
    let sql = format!(
        "UPDATE shipment_events SET created_at = now() - interval '{interval}' \
         WHERE shipment_id = $1::uuid"
    );
    sqlx::query(&sql)
        .bind(shipment_id)
        .execute(pool)
        .await
        .expect("age shipment events");
}

#[tokio::test]
async fn adminshipments_manual_shipments_render_and_age() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin shipments remake test E");
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
    let state = AppState::new(Some(pool.clone()), "adminshipmentse-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111115").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111115'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01722222227").await;
    let store = create_store(&app, &seller, "eship-alpha").await;
    let product = create_product(&app, &seller, &store, "Ship E", 150_000, 20).await;
    let buyer = login(&app, "01999999995").await;

    // One manual booking WITH a counter receipt, one
    // without.
    let with_receipt = buy_and_book(
        &app,
        &buyer,
        &seller,
        &product,
        json!({ "courier": "manual", "manual_courier": "sundarban", "manual_tracking_id": "SB-12345" }),
    )
    .await;
    let with_id = with_receipt["id"].as_str().expect("shipment id").to_string();
    assert_eq!(with_receipt["courier"], "manual");
    assert_eq!(with_receipt["manual_courier_name"], "Sundarban Courier");
    assert_eq!(with_receipt["consignment_id"], "SB-12345");
    let without_receipt = buy_and_book(
        &app,
        &buyer,
        &seller,
        &product,
        json!({ "courier": "manual", "manual_courier": "karatoa" }),
    )
    .await;
    let without_id = without_receipt["id"]
        .as_str()
        .expect("shipment id")
        .to_string();
    assert_eq!(without_receipt["manual_courier_name"], "Karatoa Courier");
    assert!(without_receipt["manual_tracking_id"].is_null());
    assert!(
        without_receipt["consignment_id"]
            .as_str()
            .expect("consignment")
            .starts_with("MANUAL-"),
        "placeholder consignment: {without_receipt}"
    );

    // The seller walks the first parcel by hand; the
    // admin detail shows the moves as MANUAL events.
    manual_status(&app, &seller, &with_id, "picked_up").await;
    manual_status(&app, &seller, &with_id, "in_transit").await;
    let detail_uri = format!("/v1/admin/shipments/{with_id}");
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "manual detail: {detail}");
    assert_eq!(detail["status"], "in_transit");
    assert_eq!(detail["manual_courier_name"], "Sundarban Courier");
    assert!(detail["last_polled_at"].is_null(), "manual never polled: {detail}");
    let events = detail["events"].as_array().expect("events");
    assert_eq!(events.len(), 3, "create + two manual moves: {detail}");
    assert_eq!(events[0]["source"], "create");
    assert_eq!(events[1]["source"], "manual");
    assert_eq!(events[1]["status"], "picked_up");
    assert_eq!(events[2]["source"], "manual");
    assert_eq!(events[2]["status"], "in_transit");

    // The list and the rollup carry both manual couriers
    // as their own counterparties.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/shipments?courier=manual",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "manual list: {list}");
    assert_eq!(list["total"], 2);
    let couriers = list["summary"]["couriers"].as_array().expect("rollup");
    let sundarban = couriers
        .iter()
        .find(|c| c["display_name"] == "Sundarban Courier")
        .expect("sundarban rollup");
    assert_eq!(sundarban["total"], 1);
    assert_eq!(sundarban["in_flight"], 1);
    assert_eq!(sundarban["mode"], "manual");
    let karatoa = couriers
        .iter()
        .find(|c| c["display_name"] == "Karatoa Courier")
        .expect("karatoa rollup");
    assert_eq!(karatoa["total"], 1);
    assert_eq!(karatoa["created"], 1);

    // Aged four days, the manual parcels join the aged
    // queue — and still neither poll queue.
    age_shipment_fully(&pool, &with_id, "4 days").await;
    age_shipment_fully(&pool, &without_id, "4 days").await;
    let (status, queue) = call(
        &app,
        Method::GET,
        "/v1/admin/shipments?queue=status_aged",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "aged queue: {queue}");
    assert_eq!(queue["total"], 2, "manual parcels age: {queue}");
    for qs in ["queue=unrefreshed", "queue=poll_overdue"] {
        let uri = format!("/v1/admin/shipments?{qs}");
        let (status, queue) = call(&app, Method::GET, &uri, None, Some(&admin)).await;
        assert_eq!(status, StatusCode::OK, "{qs}: {queue}");
        assert_eq!(queue["total"], 0, "manual never joins {qs}: {queue}");
    }
    let (status, list) = call(&app, Method::GET, "/v1/admin/shipments", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "final list: {list}");
    assert_eq!(list["summary"]["unrefreshed"], 0);
    assert_eq!(list["summary"]["poll_overdue"], 0);
    assert_eq!(list["summary"]["status_aged"], 2);
}
