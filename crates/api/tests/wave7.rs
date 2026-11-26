//! Wave 7 integration test: Pathao + RedX adapters and smart
//! routing — the per-courier estimate endpoint (all couriers priced
//! by the local domain tariff, the cheapest-available recommendation
//! with the platform-priority tie-break), booking with courier
//! `auto` storing the CHOSEN courier (never the literal `auto`),
//! booking a named courier (Pathao) through the DEV simulator, and
//! the honest production behaviour (no credentials, no dev mode:
//! estimates show every courier unavailable with no recommendation,
//! and an `auto` booking is a 503). Driven through the real Axum
//! router with tower oneshot. The small helpers are copied from the
//! Wave 6 test on purpose: integration test files stay
//! self-contained rather than sharing test code across files.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.
//!
//! The courier DEV simulator is ON for the main app in this test
//! (explicit dev flag) — that is the only way a shipment can be
//! booked here, and every consignment id it mints is prefixed
//! `DEV-`. The real Pathao/RedX adapters are covered by fixture
//! unit tests in the courier crate; they cannot be exercised live
//! without merchant credentials. A second app instance with dev
//! mode OFF covers the production behaviour.

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
            "delivery_phone": "01977777777",
            "delivery_address": "House 7, Road 9, Khulna",
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

#[tokio::test]
async fn wave7_smart_routing_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 7 integration test");
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
    let state = AppState::new(Some(pool.clone()), "wave7-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // 1. Estimates with dev mode on: every known courier is
    // available (mode dev) and priced by the SAME local domain
    // tariff — inside Dhaka, 1 kg = 6,000 poisha for all five — so
    // the recommendation is the priority tie-break winner,
    // Steadfast.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/couriers/estimate?zone=inside_dhaka&weight_grams=1000",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dev estimates: {body}");
    assert_eq!(body["zone"], "inside_dhaka");
    assert_eq!(body["weight_grams"], 1_000);
    let estimates = body["estimates"].as_array().expect("estimates array");
    assert_eq!(estimates.len(), 5, "five known couriers: {body}");
    for estimate in estimates {
        assert_eq!(estimate["available"], true, "dev estimate: {estimate}");
        assert_eq!(estimate["mode"], "dev", "dev estimate: {estimate}");
        assert_eq!(
            estimate["price_poisha"], 6_000,
            "equal local prices: {estimate}"
        );
    }
    assert_eq!(
        body["recommended"], "steadfast",
        "priority tie-break recommends steadfast: {body}"
    );

    // One seller, one store, one product.
    let seller = login(&app, "01777777777").await;
    let store = create_store(&app, &seller, "wave7-store").await;
    let product = create_product(&app, &seller, &store, "Wave7 Product", 50_000, 10).await;
    let buyer = login(&app, "01977777777").await;

    // 2. Booking with courier "auto": smart routing resolves to
    // Steadfast (equal dev prices, priority wins) and the shipment
    // stores the CHOSEN courier — never the literal "auto".
    let sub_order_id = confirmed_sub_order(&app, &seller, &buyer, &product).await;
    let (status, shipment) =
        book_shipment(&app, &seller, &sub_order_id, json!({ "courier": "auto" })).await;
    assert_eq!(status, StatusCode::CREATED, "auto booking: {shipment}");
    assert_eq!(shipment["courier"], "steadfast", "auto resolved: {shipment}");
    assert_eq!(
        shipment["consignment_id"],
        format!("DEV-steadfast-{sub_order_id}"),
        "dev consignment: {shipment}"
    );

    // 3. Booking a NAMED courier (Pathao) in dev mode: the DEV
    // simulator serves it under its own name.
    let sub_order_id = confirmed_sub_order(&app, &seller, &buyer, &product).await;
    let (status, shipment) =
        book_shipment(&app, &seller, &sub_order_id, json!({ "courier": "pathao" })).await;
    assert_eq!(status, StatusCode::CREATED, "pathao booking: {shipment}");
    assert_eq!(shipment["courier"], "pathao");
    assert_eq!(
        shipment["consignment_id"],
        format!("DEV-pathao-{sub_order_id}"),
        "dev pathao consignment: {shipment}"
    );

    // 4. Second app, dev mode OFF and no courier credentials (the
    // production reality today): estimates honestly show every
    // courier unavailable with NO price and NO recommendation, and
    // an `auto` booking is a 503 — there is nothing to route to.
    let prod_state = AppState::new(Some(pool.clone()), "wave7-test-secret".to_string(), true);
    let prod_app = build_app(prod_state);
    let (status, body) = call(
        &prod_app,
        Method::GET,
        "/v1/couriers/estimate?zone=inside_dhaka&weight_grams=1000",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "prod estimates: {body}");
    let estimates = body["estimates"].as_array().expect("estimates array");
    assert_eq!(estimates.len(), 5, "five known couriers: {body}");
    for estimate in estimates {
        assert_eq!(estimate["available"], false, "prod estimate: {estimate}");
        assert_eq!(estimate["reason"], "not_configured", "prod estimate: {estimate}");
        assert!(
            estimate["price_poisha"].is_null(),
            "no price for an unavailable courier: {estimate}"
        );
    }
    assert!(
        body["recommended"].is_null(),
        "no recommendation when nothing is available: {body}"
    );

    let prod_sub_order_id = confirmed_sub_order(&prod_app, &seller, &buyer, &product).await;
    let (status, body) =
        book_shipment(&prod_app, &seller, &prod_sub_order_id, json!({ "courier": "auto" })).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "prod auto booking is a 503: {body}"
    );
}
