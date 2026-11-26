//! Wave 11 integration test: the eCourier and Paperfly adapters —
//! the last two API couriers. With ONLY eCourier's credentials in
//! the config, `GET /v1/couriers` reports eCourier `live` and
//! Paperfly `not_configured`, and the estimate endpoint prices
//! eCourier alone and recommends it (it is the only available
//! courier, so smart routing has exactly one candidate). In DEV
//! mode, bookings under both new names mint `DEV-ecourier-…` and
//! `DEV-paperfly-…` consignment ids through the simulator. Driven
//! through the real Axum router with tower oneshot. The small
//! helpers are copied from the Wave 7 test on purpose: integration
//! test files stay self-contained rather than sharing test code
//! across files.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.
//!
//! The real eCourier/Paperfly adapters are covered by fixture unit
//! tests in the courier crate; they cannot be exercised live
//! without merchant credentials, and this test deliberately never
//! books through a credentialed adapter (that would be a real
//! network call to a real courier).

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use courier::{CouriersConfig, EcourierConfig};
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
            "delivery_phone": "01966666666",
            "delivery_address": "House 11, Road 3, Barisal",
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

/// Book a shipment for a sub-order with an explicit request body.
async fn book_shipment(
    app: &Router,
    token: &str,
    sub_order_id: &str,
    body: Value,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    call(app, Method::POST, &uri, Some(body), Some(token)).await
}

/// Place a COD order for one unit of the product and confirm its
/// sub-order; returns the sub-order id.
async fn confirmed_sub_order(
    app: &Router,
    seller: &str,
    buyer: &str,
    product: &str,
) -> String {
    add_to_cart(app, buyer, product, 1).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm sub-order: {body}");
    sub_order_id
}

/// The availability entry for one courier in a GET /v1/couriers body.
fn courier_entry<'a>(body: &'a Value, name: &str) -> &'a Value {
    body.as_array()
        .expect("couriers array")
        .iter()
        .find(|entry| entry["courier"] == name)
        .unwrap_or_else(|| panic!("courier {name} listed: {body}"))
}

#[tokio::test]
async fn wave11_ecourier_paperfly_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 11 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE platform_settings, payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, \
         order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // 1. An app whose ONLY courier credentials are eCourier's (dev
    // mode OFF): eCourier reports live, Paperfly honestly reports
    // not_configured, and the estimate endpoint prices eCourier
    // alone and recommends it — the only available courier.
    let ecourier_only = CouriersConfig {
        ecourier: EcourierConfig {
            api_key: Some("ecourier-token".to_string()),
            user_id: Some("merchant-42".to_string()),
            ..EcourierConfig::default()
        },
        ..CouriersConfig::default()
    };
    let live_state = AppState::new(Some(pool.clone()), "wave11-test-secret".to_string(), true)
        .with_couriers_config(ecourier_only);
    let live_app = build_app(live_state);

    let (status, body) = call(&live_app, Method::GET, "/v1/couriers", None, None).await;
    assert_eq!(status, StatusCode::OK, "couriers list: {body}");
    let ecourier = courier_entry(&body, "ecourier");
    assert_eq!(ecourier["available"], true, "ecourier available: {ecourier}");
    assert_eq!(ecourier["mode"], "live", "ecourier is live: {ecourier}");
    let paperfly = courier_entry(&body, "paperfly");
    assert_eq!(paperfly["available"], false, "paperfly unavailable: {paperfly}");
    assert_eq!(
        paperfly["reason"], "not_configured",
        "paperfly has no credentials: {paperfly}"
    );
    assert!(
        paperfly["mode"].is_null(),
        "an unavailable courier has no mode: {paperfly}"
    );

    let (status, body) = call(
        &live_app,
        Method::GET,
        "/v1/couriers/estimate?zone=inside_dhaka&weight_grams=1000",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "ecourier-only estimates: {body}");
    let estimates = body["estimates"].as_array().expect("estimates array");
    assert_eq!(estimates.len(), 9, "five API couriers + four manual (Wave 19): {body}");
    for estimate in estimates {
        if estimate["courier"] == "ecourier" {
            assert_eq!(estimate["available"], true, "ecourier estimate: {estimate}");
            assert_eq!(estimate["mode"], "live", "ecourier estimate: {estimate}");
            assert_eq!(
                estimate["price_poisha"], 6_000,
                "local tariff price (inside Dhaka, 1 kg): {estimate}"
            );
        } else {
            assert_eq!(estimate["available"], false, "other estimate: {estimate}");
            assert!(
                estimate["price_poisha"].is_null(),
                "no price for an unavailable courier: {estimate}"
            );
        }
    }
    assert_eq!(
        body["recommended"], "ecourier",
        "the only available courier is recommended: {body}"
    );

    // 2. DEV mode: bookings under both NEW courier names are served
    // by the simulator and mint DEV- ids under their own names.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let dev_state = AppState::new(Some(pool.clone()), "wave11-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let dev_app = build_app(dev_state);

    let seller = login(&dev_app, "01766666666").await;
    let store = create_store(&dev_app, &seller, "wave11-store").await;
    let product = create_product(&dev_app, &seller, &store, "Wave11 Product", 50_000, 10).await;
    let buyer = login(&dev_app, "01966666666").await;

    let sub_order_id = confirmed_sub_order(&dev_app, &seller, &buyer, &product).await;
    let (status, shipment) =
        book_shipment(&dev_app, &seller, &sub_order_id, json!({ "courier": "paperfly" })).await;
    assert_eq!(status, StatusCode::CREATED, "paperfly booking: {shipment}");
    assert_eq!(shipment["courier"], "paperfly");
    assert_eq!(
        shipment["consignment_id"],
        format!("DEV-paperfly-{sub_order_id}"),
        "dev paperfly consignment: {shipment}"
    );

    let sub_order_id = confirmed_sub_order(&dev_app, &seller, &buyer, &product).await;
    let (status, shipment) =
        book_shipment(&dev_app, &seller, &sub_order_id, json!({ "courier": "ecourier" })).await;
    assert_eq!(status, StatusCode::CREATED, "ecourier booking: {shipment}");
    assert_eq!(shipment["courier"], "ecourier");
    assert_eq!(
        shipment["consignment_id"],
        format!("DEV-ecourier-{sub_order_id}"),
        "dev ecourier consignment: {shipment}"
    );
}
