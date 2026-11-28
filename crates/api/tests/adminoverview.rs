//! Admin overview remake test: the overview's money figures
//! are the platform-canonical ones — GMV is the Wave 27
//! formula (goods − discount + delivery over non-cancelled
//! sub-orders, the same number the Wave 43 metrics report
//! for today) and commission is EARNED only on delivered
//! sub-orders (the seller-earnings definition), so a placed
//! order's commission snapshot does not appear as earnings
//! until delivery. Also pins the overview/metrics agreement
//! on the pending-payout queue. Helpers are copied from
//! the Wave 9 test on purpose, since integration test
//! files stay self-contained.
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

#[tokio::test]
async fn adminoverview_canonical_money_and_payout_consistency() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin overview test");
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

    let state = AppState::new(
        Some(pool.clone()),
        "adminoverview-test-secret".to_string(),
        true,
    );
    let app = build_app(state);

    // Login BEFORE the SQL promotion (the promotion matches
    // the canonical phone the login stored).
    let admin = login(&app, "01711111111").await;
    let seller1 = login(&app, "01799999999").await;
    let seller2 = login(&app, "01599999999").await;
    let buyer = login(&app, "01999999999").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin by SQL");

    // The standard two-store cart: goods 245_000 + 200_000 =
    // 445_000, inside-Dhaka delivery 6_000 per store group,
    // grand total 457_000.
    let store1 = create_store(&app, &seller1, "aov-store-1").await;
    let store2 = create_store(&app, &seller2, "aov-store-2").await;
    let product1 = create_product(&app, &seller1, &store1, "AOV Product 1", 245_000, 10).await;
    let product2 = create_product(&app, &seller2, &store2, "AOV Product 2", 200_000, 10).await;
    add_to_cart(&app, &buyer, &product1, 1).await;
    add_to_cart(&app, &buyer, &product2, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    assert_eq!(order["grand_total_poisha"], 457_000, "seed order: {order}");

    // A non-admin is refused the overview.
    let (status, body) = call(&app, Method::GET, "/v1/admin/overview", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "buyer overview: {body}");

    // 1. While the order is merely PLACED: canonical GMV is
    // already the full 457_000 (goods − discount + delivery),
    // and NO commission is earned yet — the 8% snapshots
    // (19_600 + 16_000) sit on the sub-orders but nothing
    // has been delivered.
    let (status, overview) = call(&app, Method::GET, "/v1/admin/overview", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "overview: {overview}");
    assert_eq!(overview["users"], 4);
    assert_eq!(overview["stores"], 2);
    assert_eq!(overview["products"], 2);
    assert_eq!(overview["orders"], 1);
    assert_eq!(overview["gmv_poisha"], 457_000);
    assert_eq!(overview["commission_earned_poisha"], 0);
    assert_eq!(overview["payouts_pending"]["count"], 0);

    // The metrics snapshot agrees on today's GMV and count.
    let (status, metrics) = call(&app, Method::GET, "/v1/admin/metrics", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "metrics: {metrics}");
    assert_eq!(metrics["orders_today"]["count"], 1);
    assert_eq!(metrics["orders_today"]["gmv_poisha"], 457_000);
    assert_eq!(metrics["orders_yesterday"]["count"], 0);
    assert_eq!(metrics["orders_yesterday"]["gmv_poisha"], 0);
    assert_eq!(metrics["sub_orders_by_status"], json!({ "placed": 2 }));
    assert_eq!(
        metrics["orders_by_payment_status"],
        json!({ "pay_on_delivery": 1 })
    );

    // 2. Deliver BOTH sub-orders (each by its owning seller):
    // commission earned becomes 8% of 445_000 = 35_600, and
    // GMV does not move.
    let sub_orders = order["sub_orders"].as_array().expect("sub-orders").clone();
    assert_eq!(sub_orders.len(), 2);
    for sub in &sub_orders {
        let sub_id = sub["id"].as_str().expect("sub-order id");
        let seller = if sub["store_id"].as_str() == Some(store1.as_str()) {
            &seller1
        } else {
            &seller2
        };
        for next in ["confirmed", "shipped", "delivered"] {
            let (status, body) = set_sub_order_status(&app, seller, sub_id, next).await;
            assert_eq!(status, StatusCode::OK, "walk {next}: {body}");
        }
    }
    let (status, overview) = call(&app, Method::GET, "/v1/admin/overview", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "overview after delivery: {overview}");
    assert_eq!(overview["gmv_poisha"], 457_000);
    assert_eq!(overview["commission_earned_poisha"], 35_600);

    // 3. A payout request shows identically in both payloads
    // (the page renders the queue once, from metrics).
    let (status, account) = call(
        &app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "AOV Seller",
            "account_number": "01799999999"
        })),
        Some(&seller1),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "payout account: {account}");
    let (status, payout) = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": 100_000 })),
        Some(&seller1),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "payout request: {payout}");

    let (status, overview) = call(&app, Method::GET, "/v1/admin/overview", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "overview with payout: {overview}");
    assert_eq!(overview["payouts_pending"]["count"], 1);
    assert_eq!(overview["payouts_pending"]["amount_poisha"], 100_000);
    let (status, metrics) = call(&app, Method::GET, "/v1/admin/metrics", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "metrics with payout: {metrics}");
    assert_eq!(metrics["pending_payouts"]["count"], 1);
    assert_eq!(metrics["pending_payouts"]["amount_poisha"], 100_000);
    let oldest = metrics["pending_payouts"]["oldest_age_secs"]
        .as_i64()
        .expect("oldest payout age present");
    assert!(oldest >= 0, "oldest payout age sane: {oldest}");
}
