//! Wave 9 integration test: seller dashboard, earnings and payout
//! requests — earnings come only from DELIVERED sub-orders (goods
//! minus the commission snapshot; a merely-confirmed sub-order adds
//! nothing), the payout account must exist and validate before a
//! request, requests respect the 500-taka minimum and the available
//! balance, a requested payout moves its amount from available to
//! pending, sellers see only their own numbers, and a user without
//! a store gets a 404 on the dashboard. The delivered order is
//! produced by driving the real Wave 6 flow (COD checkout -> seller
//! confirms -> DEV courier shipment -> refresh until delivered);
//! the small helpers are copied from the Wave 8 test on purpose,
//! since integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a
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

/// Buy one unit of the product and drive the order all the way to
/// delivered through the DEV courier; returns the order id.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    let (status, body) = set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");

    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(json!({})), Some(seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();

    // Four polls walk the DEV courier created -> delivered.
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(app, Method::POST, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
}

/// Buy one unit and leave the sub-order merely CONFIRMED.
async fn buy_and_confirm(app: &Router, buyer: &str, seller: &str, product_id: &str) {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
}

/// GET the seller balance for a token.
async fn get_balance(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/seller/balance", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "balance: {body}");
    body
}

#[tokio::test]
async fn wave9_seller_dashboard_earnings_payouts_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 9 integration test");
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

    // SMS dev mode (OTP login) AND courier dev mode (deliveries) on.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave9-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // The earning seller, a second seller with a store but no sales,
    // and a plain buyer with no store at all.
    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave9-store").await;
    // 200_000 poisha of goods: 8% commission is 16_000, so a
    // delivered unit earns exactly 184_000.
    let product = create_product(&app, &seller, &store, "Wave9 Product", 200_000, 10).await;
    let other_seller = login(&app, "01599999999").await;
    create_store(&app, &other_seller, "wave9-other-store").await;
    let buyer = login(&app, "01999999999").await;

    // 1. A user without a store gets a 404 on the dashboard.
    let (status, body) = call(&app, Method::GET, "/v1/seller/dashboard", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "dashboard without store: {body}");

    // 2. Before any sale: the dashboard shows the store, one product,
    // zeroed counts and zeroed earnings; the balance agrees.
    let (status, dashboard) =
        call(&app, Method::GET, "/v1/seller/dashboard", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "dashboard: {dashboard}");
    assert_eq!(dashboard["store"]["id"], store);
    assert_eq!(dashboard["store"]["slug"], "wave9-store");
    assert_eq!(dashboard["counts"]["products"], 1);
    assert_eq!(dashboard["counts"]["sub_orders_by_status"]["delivered"], 0);
    assert_eq!(dashboard["earnings"]["earned_poisha"], 0);
    assert_eq!(dashboard["earnings"]["available_poisha"], 0);
    assert_eq!(dashboard["recent_sub_orders"].as_array().map(Vec::len), Some(0));

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 0);
    assert_eq!(balance["paid_out_poisha"], 0);
    assert_eq!(balance["pending_poisha"], 0);
    assert_eq!(balance["available_poisha"], 0);

    // 3. One DELIVERED sale earns 184_000; a second sale that is only
    // CONFIRMED adds nothing.
    buy_and_deliver(&app, &buyer, &seller, &product).await;
    buy_and_confirm(&app, &buyer, &seller, &product).await;

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 184_000, "balance: {balance}");
    assert_eq!(balance["paid_out_poisha"], 0);
    assert_eq!(balance["pending_poisha"], 0);
    assert_eq!(balance["available_poisha"], 184_000);

    let (status, dashboard) =
        call(&app, Method::GET, "/v1/seller/dashboard", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "dashboard: {dashboard}");
    assert_eq!(dashboard["counts"]["sub_orders_by_status"]["delivered"], 1);
    assert_eq!(dashboard["counts"]["sub_orders_by_status"]["confirmed"], 1);
    assert_eq!(dashboard["earnings"]["available_poisha"], 184_000);
    assert_eq!(dashboard["recent_sub_orders"].as_array().map(Vec::len), Some(2));

    // 4. No payout account yet: GET is null and a payout request is
    // a 400 telling the seller to add the account first.
    let (status, account) = call(
        &app,
        Method::GET,
        "/v1/seller/payout-account",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "payout account: {account}");
    assert!(account.is_null(), "no account yet: {account}");

    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": 100_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "payout without account: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("payout account first"),
        "account-first message: {body}"
    );

    // 5. A bKash account with an invalid mobile number is a 400;
    // the valid one saves.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "Wave9 Seller",
            "account_number": "01299999999"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad bkash number: {body}");

    let (status, account) = call(
        &app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "Wave9 Seller",
            "account_number": "01799999999"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save payout account: {account}");
    assert_eq!(account["method"], "bkash");
    assert_eq!(account["account_number"], "01799999999");

    let (status, account) = call(
        &app,
        Method::GET,
        "/v1/seller/payout-account",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "payout account: {account}");
    assert_eq!(account["method"], "bkash");

    // 6. Below the 500-taka minimum (50_000 poisha): a 400.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": 40_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "below minimum: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("minimum"),
        "minimum message: {body}"
    );

    // 7. A valid request: 201, `requested`, account fields snapshotted.
    // The amount moves from available to pending.
    let (status, payout) = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": 100_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "request payout: {payout}");
    assert_eq!(payout["amount_poisha"], 100_000);
    assert_eq!(payout["status"], "requested");
    assert_eq!(payout["method"], "bkash");
    assert_eq!(payout["account_name"], "Wave9 Seller");
    assert_eq!(payout["account_number"], "01799999999");
    assert!(payout["processed_at"].is_null());

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 184_000, "balance: {balance}");
    assert_eq!(balance["pending_poisha"], 100_000);
    assert_eq!(balance["available_poisha"], 84_000);

    // 8. A second request beyond the remaining available balance is
    // a 400 (90_000 > 84_000 available).
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": 90_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "exceeds available: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("exceeds the available balance"),
        "exceeds message: {body}"
    );

    // 9. The payout list shows exactly the one request.
    let (status, list) = call(&app, Method::GET, "/v1/seller/payouts", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "payout list: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["amount_poisha"], 100_000);
    assert_eq!(list["items"][0]["status"], "requested");

    // 10. The other seller sees only their own (empty) numbers:
    // dashboard and balance are zeroed, and they have no payout
    // account.
    let (status, dashboard) = call(
        &app,
        Method::GET,
        "/v1/seller/dashboard",
        None,
        Some(&other_seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "other dashboard: {dashboard}");
    assert_eq!(dashboard["store"]["slug"], "wave9-other-store");
    assert_eq!(dashboard["counts"]["products"], 0);
    assert_eq!(dashboard["earnings"]["earned_poisha"], 0);
    assert_eq!(dashboard["earnings"]["available_poisha"], 0);

    let balance = get_balance(&app, &other_seller).await;
    assert_eq!(balance["earned_poisha"], 0);
    assert_eq!(balance["available_poisha"], 0);

    let (status, account) = call(
        &app,
        Method::GET,
        "/v1/seller/payout-account",
        None,
        Some(&other_seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "other payout account: {account}");
    assert!(account.is_null(), "other seller has no account: {account}");

    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/seller/payouts",
        None,
        Some(&other_seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "other payout list: {list}");
    assert_eq!(list["total"], 0);
}
