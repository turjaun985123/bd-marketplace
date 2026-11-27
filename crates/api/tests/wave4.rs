//! Wave 4 integration test: weight-based delivery charges, buyer
//! order cancellation with stock restore, seller-cancel restock, and
//! the buyer receipt — driven through the real Axum router with tower
//! oneshot (no TCP port, no network). The small helpers are copied
//! from the Wave 3 test on purpose: integration test files stay
//! self-contained rather than sharing test code across files.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a plain
//! local `cargo test` (no database) still passes. CI provides a
//! Postgres 16 service and sets `DATABASE_URL` for the rust job.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use domain::{calculate_delivery_charge, DeliveryZone};
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

/// Create an active product, optionally with an explicit weight in
/// grams; returns the full product JSON.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
    weight_grams: Option<i64>,
) -> Value {
    let mut payload = json!({
        "store_id": store_id,
        "title": title,
        "price_poisha": price_poisha,
        "stock": stock,
        "status": "active"
    });
    if let Some(weight) = weight_grams {
        payload["weight_grams"] = json!(weight);
    }
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(payload),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body
}

/// Current stock of a product, read through the public detail endpoint.
async fn stock_of(app: &Router, product_id: &str) -> i64 {
    let uri = format!("/v1/products/{product_id}");
    let (status, body) = call(app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "product detail: {body}");
    body["stock"].as_i64().expect("stock field")
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

/// COD checkout for the buyer's current cart in the given zone.
async fn checkout(app: &Router, token: &str, zone: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01922222222",
            "delivery_address": "House 2, Lake Road, Sylhet",
            "delivery_zone": zone,
            "payment_method": "cod"
        })),
        Some(token),
    )
    .await
}

/// Find a sub-order JSON object by its store id inside an order body.
fn sub_for_store<'a>(order: &'a Value, store_id: &str) -> &'a Value {
    order["sub_orders"]
        .as_array()
        .expect("sub_orders array")
        .iter()
        .find(|s| s["store_id"] == store_id)
        .expect("sub-order for store")
}

#[tokio::test]
async fn wave4_weight_cancel_receipt_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 4 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE order_items, sub_orders, orders, cart_items, carts, \
         products, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // Dev mode on: OTPs are log-sent and echoed in the response.
    let state = AppState::new(Some(pool), "wave4-test-secret".to_string(), true);
    let app = build_app(state);

    // Two sellers, one store each. Heavy: 1,000 BDT, stock 10, 2,500 g.
    // Light: 500 BDT, stock 7, 500 g.
    let seller_a = login(&app, "01722222222").await;
    let store_a = create_store(&app, &seller_a, "wave4-store-a").await;
    let heavy = create_product(
        &app,
        &seller_a,
        &store_a,
        "Wave4 Heavy",
        100_000,
        10,
        Some(2_500),
    )
    .await;
    assert_eq!(heavy["weight_grams"], 2_500, "weight stored: {heavy}");
    let heavy_id = heavy["id"].as_str().expect("product id").to_string();

    // A product created without a weight falls back to 1,000 g.
    let defaulted = create_product(
        &app,
        &seller_a,
        &store_a,
        "Wave4 Default Weight",
        10_000,
        1,
        None,
    )
    .await;
    assert_eq!(defaulted["weight_grams"], 1_000);

    let seller_b = login(&app, "01822222222").await;
    let store_b = create_store(&app, &seller_b, "wave4-store-b").await;
    let light = create_product(
        &app,
        &seller_b,
        &store_b,
        "Wave4 Light",
        50_000,
        7,
        Some(500),
    )
    .await;
    let light_id = light["id"].as_str().expect("product id").to_string();

    let buyer = login(&app, "01922222222").await;

    // 1. Order 1: 2 x Heavy (2 x 2,500 g = 5 kg) + 1 x Light (500 g),
    // outside Dhaka. Store A's delivery must be the domain charge for
    // a 5 kg parcel; store B's 500 g parcel bills as 1 kg.
    add_to_cart(&app, &buyer, &heavy_id, 2).await;
    add_to_cart(&app, &buyer, &light_id, 1).await;
    let (status, order1) = checkout(&app, &buyer, "outside_dhaka").await;
    assert_eq!(status, StatusCode::CREATED, "checkout 1: {order1}");
    // Wave 39: the domain rule returns a Result now (audit
    // CR-11/CR-12) — same value, unwrapped.
    let expected_a =
        calculate_delivery_charge(DeliveryZone::OutsideDhaka, 5, 200_000).expect("delivery charge");
    assert_eq!(expected_a, 28_000); // 120 + 4*40 BDT, pins the domain rule
    let sub_a1 = sub_for_store(&order1, &store_a);
    assert_eq!(sub_a1["delivery_charge_poisha"], expected_a, "5 kg charge: {order1}");
    let sub_b1 = sub_for_store(&order1, &store_b);
    assert_eq!(sub_b1["delivery_charge_poisha"], 12_000, "500 g bills as 1 kg");
    assert_eq!(order1["goods_total_poisha"], 250_000);
    assert_eq!(order1["delivery_total_poisha"], 40_000);
    assert_eq!(order1["grand_total_poisha"], 290_000);
    let order1_id = order1["id"].as_str().expect("order id").to_string();
    assert_eq!(stock_of(&app, &heavy_id).await, 8);
    assert_eq!(stock_of(&app, &light_id).await, 6);

    // 2. Receipt: buyer sees totals and the COD amount due; commission
    // and seller payout must NOT appear anywhere in the receipt body.
    let receipt_uri = format!("/v1/orders/{order1_id}/receipt");
    let (status, receipt) = call(&app, Method::GET, &receipt_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "receipt: {receipt}");
    assert_eq!(receipt["platform_name"], "BD Marketplace");
    assert_eq!(receipt["order_id"], order1_id);
    assert_eq!(receipt["grand_total_poisha"], 290_000);
    assert_eq!(receipt["cod_amount_due_poisha"], 290_000);
    assert_eq!(receipt["delivery"]["name"], "Test Buyer");
    assert_eq!(receipt["delivery"]["zone"], "outside_dhaka");
    let receipt_subs = receipt["sub_orders"].as_array().expect("receipt sub_orders");
    assert_eq!(receipt_subs.len(), 2);
    let receipt_a = receipt_subs
        .iter()
        .find(|s| s["store_slug"] == "wave4-store-a")
        .expect("receipt sub-order for store A");
    assert_eq!(receipt_a["store_name"], "Store wave4-store-a");
    assert_eq!(receipt_a["delivery_charge_poisha"], 28_000);
    assert_eq!(receipt_a["items"][0]["title_snapshot"], "Wave4 Heavy");
    let receipt_text = receipt.to_string();
    assert!(
        !receipt_text.contains("commission"),
        "buyer receipt must not contain commission: {receipt_text}"
    );
    assert!(
        !receipt_text.contains("seller_payout"),
        "buyer receipt must not contain seller payout: {receipt_text}"
    );
    // Another user cannot read the buyer's receipt.
    let (status, _) = call(&app, Method::GET, &receipt_uri, None, Some(&seller_a)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 3. Buyer cancels order 1 while everything is still placed:
    // order + sub-orders become cancelled and stock is fully restored.
    let cancel_uri = format!("/v1/orders/{order1_id}/cancel");
    let (status, cancelled) = call(&app, Method::POST, &cancel_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer cancel: {cancelled}");
    assert_eq!(cancelled["status"], "cancelled");
    for sub in cancelled["sub_orders"].as_array().expect("sub_orders") {
        assert_eq!(sub["status"], "cancelled", "every sub-order cancelled: {sub}");
    }
    assert_eq!(stock_of(&app, &heavy_id).await, 10, "heavy restocked");
    assert_eq!(stock_of(&app, &light_id).await, 7, "light restocked");

    // 4. Order 2: one of each, inside Dhaka. Seller A confirms their
    // sub-order, so the buyer can no longer cancel (409) and a failed
    // cancel changes no stock.
    add_to_cart(&app, &buyer, &heavy_id, 1).await;
    add_to_cart(&app, &buyer, &light_id, 1).await;
    let (status, order2) = checkout(&app, &buyer, "inside_dhaka").await;
    assert_eq!(status, StatusCode::CREATED, "checkout 2: {order2}");
    let order2_id = order2["id"].as_str().expect("order id").to_string();
    let sub_a2_id = sub_for_store(&order2, &store_a)["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let sub_b2_id = sub_for_store(&order2, &store_b)["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    assert_eq!(stock_of(&app, &heavy_id).await, 9);
    assert_eq!(stock_of(&app, &light_id).await, 6);

    let status_uri_a = format!("/v1/seller/sub-orders/{sub_a2_id}/status");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &status_uri_a,
        Some(json!({ "status": "confirmed" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller A confirms: {body}");

    let cancel2_uri = format!("/v1/orders/{order2_id}/cancel");
    let (status, body) = call(&app, Method::POST, &cancel2_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::CONFLICT, "cancel after confirm: {body}");
    assert_eq!(stock_of(&app, &heavy_id).await, 9);
    assert_eq!(stock_of(&app, &light_id).await, 6);

    // 5. Seller B cancels their own (still placed) sub-order: only
    // that sub-order's items are restocked; store A's stock is
    // untouched and store A's sub-order stays confirmed.
    let status_uri_b = format!("/v1/seller/sub-orders/{sub_b2_id}/status");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &status_uri_b,
        Some(json!({ "status": "cancelled" })),
        Some(&seller_b),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller B cancels: {body}");
    assert_eq!(body["status"], "cancelled");
    assert_eq!(stock_of(&app, &light_id).await, 7, "seller-cancel restocks light");
    assert_eq!(stock_of(&app, &heavy_id).await, 9, "other store untouched");

    let order2_uri = format!("/v1/orders/{order2_id}");
    let (status, order2_after) = call(&app, Method::GET, &order2_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sub_for_store(&order2_after, &store_a)["status"],
        "confirmed",
        "store A sub-order unaffected: {order2_after}"
    );
    assert_eq!(
        sub_for_store(&order2_after, &store_b)["status"],
        "cancelled",
        "store B sub-order cancelled: {order2_after}"
    );
}
