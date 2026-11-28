//! Admin shipments remake, test F: money. The summary's
//! COD figures are the STORED booking snapshots, split
//! by what they mean: delivered-held (cash the courier
//! is presumed to hold, unreconciled — an upper bound),
//! created (booked, not yet collected). The per-courier
//! rollup's status classes partition the status set, so
//! they sum to the courier's total. And the P0 surface:
//! a buyer who PAID a COD advance (41_200 on a 206_000
//! order) whose parcel comes back by courier RTO has NO
//! return request and no refund path anywhere — the
//! summary and the detail flag the stranded population
//! read-only, inventing neither an amount owed beyond
//! the recorded advance nor a refund action. Helpers are
//! copied from the Wave 9 / 17 / admin orders tests on
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

/// Checkout the buyer's current cart with COD; returns
/// (status, order). `advance_method` rides along when
/// given (the Wave 17 shape).
async fn checkout_cod(
    app: &Router,
    auth: &str,
    advance_method: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = json!({
        "delivery_name": "Test Buyer",
        "delivery_phone": "01888888888",
        "delivery_address": "House 9, Road 3, Khulna",
        "delivery_zone": "inside_dhaka",
        "payment_method": "cod"
    });
    if let Some(method) = advance_method {
        body["advance_method"] = json!(method);
    }
    call(app, Method::POST, "/v1/orders", Some(body), Some(auth)).await
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

/// POST the admin refresh for one shipment.
async fn admin_refresh(app: &Router, auth: &str, shipment_id: &str) {
    let uri = format!("/v1/admin/shipments/{shipment_id}/refresh");
    let (status, body) = call(app, Method::POST, &uri, Some(json!({})), Some(auth)).await;
    assert_eq!(status, StatusCode::OK, "admin refresh: {body}");
    assert_eq!(body["outcome"], "advanced", "refresh advances: {body}");
}

/// Buy one product, confirm and book; returns (sub_order_id, shipment_id).
async fn buy_confirm_book(
    app: &Router,
    buyer: &str,
    seller: &str,
    product_id: &str,
    booking: Value,
) -> (String, String) {
    add_to_cart(app, buyer, product_id, 1).await;
    let (status, order) = checkout_cod(app, buyer, None).await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    let shipment = book_shipment(app, seller, &sub_order_id, booking).await;
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    (sub_order_id, shipment_id)
}

#[tokio::test]
async fn adminshipments_cod_rollup_and_stranded_advance() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin shipments remake test F");
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
    let state = AppState::new(Some(pool.clone()), "adminshipmentsf-test-secret".to_string(), true)
        .with_couriers_config(CouriersConfig {
            dev_mode: true,
            ..CouriersConfig::default()
        })
        .with_payments_config(PaymentsConfig {
            dev_mode: true,
            dev_secret: "<redacted>".to_string(),
            ..PaymentsConfig::default()
        });
    let app = build_app(state);
    // A second app over the same pool and JWT secret but
    // WITHOUT payments dev mode: Flow 1's 206_000 COD
    // order is over the advance threshold, and on the
    // dev-mode app a checkout without an advance method
    // is a 400 by design (Wave 17). On this app it
    // degrades to plain COD, so the courier still
    // collects the full 206_000.
    let plain_state = AppState::new(
        Some(pool.clone()),
        "adminshipmentsf-test-secret".to_string(),
        true,
    )
    .with_couriers_config(CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    });
    let app_plain = build_app(plain_state);

    let admin = login(&app, "01711111116").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111116'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller_a = login(&app, "01722222228").await;
    let store_a = create_store(&app, &seller_a, "fship-alpha").await;
    let pricey = create_product(&app, &seller_a, &store_a, "Ship F Pricey", 200_000, 10).await;
    let plain = create_product(&app, &seller_a, &store_a, "Ship F Plain", 150_000, 10).await;
    let seller_b = login(&app, "01722222229").await;
    let store_b = create_store(&app, &seller_b, "fship-beta").await;
    let stranded_product =
        create_product(&app, &seller_b, &store_b, "Ship F Stranded", 200_000, 10).await;
    let buyer = login(&app, "01999999994").await;

    // Flow 1: a plain COD parcel walked to delivered by
    // admin refreshes — its stored COD (200_000 goods +
    // 6_000 delivery) becomes delivered-held.
    let (_sub1, delivered_ship) =
        buy_confirm_book(&app_plain, &buyer, &seller_a, &pricey, json!({})).await;
    for _ in 0..4 {
        admin_refresh(&app, &admin, &delivered_ship).await;
    }
    // Flow 2: a second parcel left at created.
    let (_sub2, _created_ship) =
        buy_confirm_book(&app, &buyer, &seller_a, &plain, json!({})).await;

    let (status, list) = call(&app, Method::GET, "/v1/admin/shipments", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "list: {list}");
    let summary = &list["summary"];
    assert_eq!(summary["total"], 2);
    assert_eq!(summary["cod_delivered_held_poisha"], 206_000);
    assert_eq!(summary["cod_created_poisha"], 156_000);
    assert_eq!(summary["cod_in_flight_expected_poisha"], 0);
    assert_eq!(summary["by_status"]["delivered"], 1);
    assert_eq!(summary["by_status"]["created"], 1);
    assert_eq!(summary["advance_stranded"]["shipments"], 0);
    let couriers = summary["couriers"].as_array().expect("rollup");
    let steadfast = couriers
        .iter()
        .find(|c| c["courier"] == "steadfast")
        .expect("steadfast rollup");
    assert_eq!(steadfast["total"], 2);
    assert_eq!(steadfast["delivered"], 1);
    assert_eq!(steadfast["created"], 1);
    assert_eq!(steadfast["cod_delivered_held_poisha"], 206_000);
    // The status classes partition the status set: they
    // sum to the courier's total.
    let class_sum: i64 = [
        "created",
        "in_flight",
        "delivered",
        "partial_delivered",
        "return_in_transit",
        "returned",
        "cancelled",
        "unknown",
    ]
    .iter()
    .map(|key| steadfast[*key].as_i64().expect("class count"))
    .sum();
    assert_eq!(class_sum, steadfast["total"].as_i64().expect("total"));

    // Flow 3: the stranded advance. The buyer pays the
    // 41_200 advance for a 206_000 order, the parcel is
    // booked manual and walked all the way back by hand.
    add_to_cart(&app, &buyer, &stranded_product, 1).await;
    let (status, order) = checkout_cod(&app, &buyer, Some("bkash")).await;
    assert_eq!(status, StatusCode::CREATED, "advance checkout: {order}");
    assert_eq!(order["payment_status"], "advance_pending");
    assert_eq!(order["advance_required_poisha"], 41_200);
    let payment_id = order["payment"]["id"]
        .as_str()
        .expect("payment id")
        .to_string();
    let sub3 = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let simulate_uri = format!("/v1/payments/{payment_id}/dev-simulate-success");
    let (status, body) = call(
        &app,
        Method::POST,
        &simulate_uri,
        Some(json!({})),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dev simulate: {body}");
    set_sub_order_status(&app, &seller_b, &sub3, "confirmed").await;
    let stranded_ship = book_shipment(
        &app,
        &seller_b,
        &sub3,
        json!({ "courier": "manual", "manual_courier": "sundarban" }),
    )
    .await;
    let stranded_id = stranded_ship["id"]
        .as_str()
        .expect("stranded shipment id")
        .to_string();
    for step in [
        "picked_up",
        "in_transit",
        "out_for_delivery",
        "delivered",
        "return_in_transit",
        "returned",
    ] {
        manual_status(&app, &seller_b, &stranded_id, step).await;
    }

    // No return request exists for the sub-order — that
    // absence IS the gap the flag describes.
    let return_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM return_requests WHERE sub_order_id = $1::uuid",
    )
    .bind(&sub3)
    .fetch_one(&pool)
    .await
    .expect("count return requests");
    assert_eq!(return_rows, 0);

    let (status, list) = call(&app, Method::GET, "/v1/admin/shipments", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "final list: {list}");
    let stranded = &list["summary"]["advance_stranded"];
    assert_eq!(stranded["shipments"], 1, "one stranded parcel: {list}");
    assert_eq!(stranded["orders"], 1);
    assert_eq!(stranded["advance_paid_poisha"], 41_200);
    let detail_uri = format!("/v1/admin/shipments/{stranded_id}");
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "stranded detail: {detail}");
    assert_eq!(detail["status"], "returned");
    assert_eq!(detail["advance_paid_poisha"], 41_200);
    assert_eq!(detail["advance_stranded"], true);
    assert!(detail["return_request"].is_null(), "no return request: {detail}");
}
