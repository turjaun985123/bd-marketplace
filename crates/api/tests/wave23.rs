//! Wave 23 integration test: inventory management and the
//! stock audit trail — every stock movement writes a
//! `stock_adjustments` row in the same transaction. The flow
//! walks one product through a seller's manual adjustments
//! (including a refused below-zero move that must write
//! nothing), a real COD checkout (system `order` row) and a
//! buyer cancel (system `cancel_restock` row), then checks
//! the low-stock health rule end to end: the default
//! threshold, a per-product override, the inventory summary,
//! and the dashboard alert strip following the threshold.
//! Another seller gets 404s on the product's stock endpoints
//! and never sees it in their inventory.
//!
//! The small helpers are copied from the Wave 9 test on
//! purpose, since integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test prints a skip
//! note and passes, so a plain local `cargo test` (no
//! database) still passes. CI provides a Postgres 16 service
//! and sets `DATABASE_URL` for the rust job.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
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

/// POST one manual stock adjustment for a product.
async fn adjust_stock(
    app: &Router,
    token: &str,
    product_id: &str,
    delta: i64,
    reason: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/products/{product_id}/adjust-stock");
    call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "delta": delta, "reason": reason })),
        Some(token),
    )
    .await
}

/// GET a product's full stock history (one generous page).
async fn get_history(app: &Router, token: &str, product_id: &str) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/products/{product_id}/stock-history?per_page=50");
    call(app, Method::GET, &uri, None, Some(token)).await
}

/// PUT a product's low-stock threshold (null clears it).
async fn set_threshold(
    app: &Router,
    token: &str,
    product_id: &str,
    threshold: Value,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/products/{product_id}/low-stock-threshold");
    call(
        app,
        Method::PUT,
        &uri,
        Some(json!({ "threshold": threshold })),
        Some(token),
    )
    .await
}

/// GET one product as its owner sees it.
async fn get_product(app: &Router, token: &str, product_id: &str) -> Value {
    let uri = format!("/v1/products/{product_id}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "get product: {body}");
    body
}

/// Find one history row by its reason.
fn history_row<'a>(history: &'a Value, reason: &str) -> &'a Value {
    history["items"]
        .as_array()
        .expect("history items")
        .iter()
        .find(|row| row["reason"] == reason)
        .unwrap_or_else(|| panic!("history has a '{reason}' row: {history}"))
}

#[tokio::test]
async fn wave23_inventory_and_stock_audit_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 23 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, \
         order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode on (OTP login); no courier or payment dev
    // modes are needed — the flow never ships anything.
    let state = AppState::new(Some(pool.clone()), "wave23-test-secret".to_string(), true);
    let app = build_app(state);

    let seller = login(&app, "01799999999").await;
    let store_id = create_store(&app, &seller, "wave23-store").await;
    let product_id = create_product(&app, &seller, &store_id, "Wave23 Widget", 50_000, 10).await;
    let other_seller = login(&app, "01599999999").await;
    create_store(&app, &other_seller, "wave23-other-store").await;
    let buyer = login(&app, "01999999999").await;

    // Creation writes NO audit row: the history starts empty,
    // at the creation stock of 10.
    let (status, history) = get_history(&app, &seller, &product_id).await;
    assert_eq!(status, StatusCode::OK, "empty history: {history}");
    assert_eq!(history["total"], 0);

    // 1. Manual adjustment +5 (restock): stock 15, and the
    // response carries the audit row it wrote.
    let (status, body) = adjust_stock(&app, &seller, &product_id, 5, "restock").await;
    assert_eq!(status, StatusCode::OK, "adjust +5: {body}");
    assert_eq!(body["product"]["stock"], 15);
    assert_eq!(body["product"]["stock_status"], "in_stock");
    assert_eq!(body["adjustment"]["actor"], "seller");
    assert_eq!(body["adjustment"]["delta"], 5);
    assert_eq!(body["adjustment"]["new_stock"], 15);
    assert_eq!(body["adjustment"]["reason"], "restock");

    // 2. A move that would drive stock negative is a 400 and
    // writes NOTHING — stock and history are untouched.
    let (status, body) = adjust_stock(&app, &seller, &product_id, -20, "correction").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "adjust -20: {body}");
    let product = get_product(&app, &seller, &product_id).await;
    assert_eq!(product["stock"], 15, "refused move leaves stock alone");
    let (_, history) = get_history(&app, &seller, &product_id).await;
    assert_eq!(history["total"], 1, "refused move writes no row");

    // Domain validation reaches the API: zero delta, a forged
    // system reason, and an out-of-range threshold are 400s.
    let (status, _) = adjust_stock(&app, &seller, &product_id, 0, "restock").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "zero delta refused");
    let (status, _) = adjust_stock(&app, &seller, &product_id, 1, "order").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "system reason refused");
    let (status, _) = set_threshold(&app, &seller, &product_id, json!(-1)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "negative threshold refused");
    let (status, _) = set_threshold(&app, &seller, &product_id, json!(10_001)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "huge threshold refused");

    // 3. Adjust -15 (correction): the shelf is empty and the
    // product reports out_of_stock.
    let (status, body) = adjust_stock(&app, &seller, &product_id, -15, "correction").await;
    assert_eq!(status, StatusCode::OK, "adjust -15: {body}");
    assert_eq!(body["product"]["stock"], 0);
    assert_eq!(body["product"]["stock_status"], "out_of_stock");

    // 4. Restock +7, then a buyer buys 2 through a real COD
    // checkout: the system writes its own `order` row.
    let (status, body) = adjust_stock(&app, &seller, &product_id, 7, "restock").await;
    assert_eq!(status, StatusCode::OK, "adjust +7: {body}");
    assert_eq!(body["product"]["stock"], 7);

    add_to_cart(&app, &buyer, &product_id, 2).await;
    let order = checkout_cod(&app, &buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
    let product = get_product(&app, &seller, &product_id).await;
    assert_eq!(product["stock"], 5, "checkout decremented stock");
    let (_, history) = get_history(&app, &seller, &product_id).await;
    let order_row = history_row(&history, "order");
    assert_eq!(order_row["actor"], "system");
    assert_eq!(order_row["delta"], -2);
    assert_eq!(order_row["new_stock"], 5);
    assert!(order_row["actor_user_id"].is_null(), "system rows have no author");

    // 5. The buyer cancels the still-placed order: the shared
    // restock helper puts the units back under
    // `cancel_restock`.
    let cancel_uri = format!("/v1/orders/{order_id}/cancel");
    let (status, body) = call(&app, Method::POST, &cancel_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer cancel: {body}");
    let product = get_product(&app, &seller, &product_id).await;
    assert_eq!(product["stock"], 7, "cancel restocked the units");
    assert_eq!(product["stock_status"], "in_stock");
    let (_, history) = get_history(&app, &seller, &product_id).await;
    let cancel_row = history_row(&history, "cancel_restock");
    assert_eq!(cancel_row["actor"], "system");
    assert_eq!(cancel_row["delta"], 2);
    assert_eq!(cancel_row["new_stock"], 7);
    assert_eq!(history["total"], 5, "five movements so far");

    // 6. Down to exactly the default threshold: stock 5 is
    // `low` at the default of 5.
    let (status, body) = adjust_stock(&app, &seller, &product_id, -2, "damage").await;
    assert_eq!(status, StatusCode::OK, "adjust -2: {body}");
    assert_eq!(body["product"]["stock"], 5);
    assert_eq!(body["product"]["stock_status"], "low");

    // 7. A per-product threshold of 3 makes the same stock
    // healthy again — in the product view, the inventory
    // summary, and the dashboard alerts alike.
    let (status, body) = set_threshold(&app, &seller, &product_id, json!(3)).await;
    assert_eq!(status, StatusCode::OK, "set threshold 3: {body}");
    assert_eq!(body["low_stock_threshold"], 3);
    assert_eq!(body["stock_status"], "in_stock");

    let (status, inventory) = call(
        &app,
        Method::GET,
        "/v1/seller/inventory",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "inventory: {inventory}");
    assert_eq!(inventory["summary"]["products"], 1);
    assert_eq!(inventory["summary"]["low_stock"], 0);
    assert_eq!(inventory["summary"]["out_of_stock"], 0);
    let item = &inventory["items"][0];
    assert_eq!(item["product_id"], product_id);
    assert_eq!(item["stock"], 5);
    assert_eq!(item["low_stock_threshold"], 3);
    assert_eq!(item["effective_threshold"], 3);
    assert_eq!(item["stock_status"], "in_stock");

    let (status, dashboard) = call(
        &app,
        Method::GET,
        "/v1/seller/dashboard",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dashboard: {dashboard}");
    let alerts = dashboard["low_stock_items"].as_array().expect("alert list");
    assert!(
        alerts.iter().all(|row| row["product_id"] != product_id),
        "no alert while the custom threshold says healthy: {dashboard}"
    );

    // 8. Clearing the threshold restores the default: stock 5
    // is low again, the summary counts it, and the dashboard
    // alert strip names it.
    let (status, body) = set_threshold(&app, &seller, &product_id, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "clear threshold: {body}");
    assert!(body["low_stock_threshold"].is_null());
    assert_eq!(body["stock_status"], "low");

    let (status, inventory) = call(
        &app,
        Method::GET,
        "/v1/seller/inventory",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "inventory again: {inventory}");
    assert_eq!(inventory["summary"]["low_stock"], 1);
    assert_eq!(inventory["summary"]["out_of_stock"], 0);
    assert_eq!(inventory["items"][0]["effective_threshold"], 5);
    assert_eq!(inventory["items"][0]["stock_status"], "low");

    let (status, dashboard) = call(
        &app,
        Method::GET,
        "/v1/seller/dashboard",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dashboard again: {dashboard}");
    let alerts = dashboard["low_stock_items"].as_array().expect("alert list");
    let alert = alerts
        .iter()
        .find(|row| row["product_id"] == product_id)
        .expect("the low product alerts on the dashboard");
    assert_eq!(alert["stock"], 5);
    assert_eq!(alert["stock_status"], "low");

    // 9. Another seller: every product-scoped stock endpoint
    // is a 404, and their inventory never lists the product.
    let (status, _) = adjust_stock(&app, &other_seller, &product_id, 1, "restock").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign adjust-stock is a 404");
    let (status, _) = get_history(&app, &other_seller, &product_id).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign history is a 404");
    let (status, _) = set_threshold(&app, &other_seller, &product_id, json!(3)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign threshold is a 404");
    let (status, inventory) = call(
        &app,
        Method::GET,
        "/v1/seller/inventory",
        None,
        Some(&other_seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "foreign inventory: {inventory}");
    assert_eq!(inventory["summary"]["products"], 0);
    assert!(
        inventory["items"]
            .as_array()
            .expect("items")
            .iter()
            .all(|row| row["product_id"] != product_id),
        "the product never appears in another seller's inventory"
    );
}
