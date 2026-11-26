//! Wave 3 integration test: cart + split checkout (COD) across two
//! sellers, stock decrement, insufficient-stock rejection, and seller
//! sub-order status transitions — driven through the real Axum router
//! with tower oneshot (no TCP port, no network).
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

#[tokio::test]
async fn wave3_cart_split_checkout_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 3 integration test");
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
    let state = AppState::new(Some(pool), "wave3-test-secret".to_string(), true);
    let app = build_app(state);

    // Two sellers with one store and one product each, plus one buyer.
    let seller_a = login(&app, "01711111111").await;
    let store_a = create_store(&app, &seller_a, "wave3-store-a").await;
    // 2,450 BDT, stock 5.
    let product_a = create_product(&app, &seller_a, &store_a, "Wave3 Panjabi", 245_000, 5).await;

    let seller_b = login(&app, "01811111111").await;
    let store_b = create_store(&app, &seller_b, "wave3-store-b").await;
    // 2,000 BDT, stock 3.
    let product_b = create_product(&app, &seller_b, &store_b, "Wave3 Shirt", 200_000, 3).await;

    let buyer = login(&app, "01911111111").await;

    // 1. Buyer adds one item from each store; quantities accumulate.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_a, "quantity": 1 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add A: {body}");
    assert_eq!(body["goods_total_poisha"], 245_000);

    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_b, "quantity": 1 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add B: {body}");
    assert_eq!(body["goods_total_poisha"], 445_000);
    assert_eq!(body["items"].as_array().map(Vec::len), Some(2));

    // 2. Checkout COD, outside Dhaka. The worked example: goods 4,450 +
    // delivery 2 x 120 = 4,690 BDT grand total.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01911111111",
            "delivery_address": "House 1, Village Road, Rajshahi",
            "delivery_zone": "outside_dhaka",
            "payment_method": "cod"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {body}");
    assert_eq!(body["goods_total_poisha"], 445_000);
    assert_eq!(body["delivery_total_poisha"], 24_000);
    assert_eq!(body["grand_total_poisha"], 469_000);
    assert_eq!(body["status"], "placed");
    let order_id = body["id"].as_str().expect("order id").to_string();

    let sub_orders = body["sub_orders"].as_array().expect("sub_orders array");
    assert_eq!(sub_orders.len(), 2, "one sub-order per seller: {body}");
    // Sub-order order follows the (unordered) cart lock query, so find
    // each seller's sub-order by its store id rather than by position.
    let sub_a = sub_orders
        .iter()
        .find(|s| s["store_id"] == store_a)
        .expect("store A sub-order");
    assert_eq!(sub_a["goods_subtotal_poisha"], 245_000);
    assert_eq!(sub_a["delivery_charge_poisha"], 12_000);
    assert_eq!(sub_a["commission_poisha"], 19_600); // 8% of 245,000
    assert_eq!(sub_a["seller_payout_poisha"], 225_400);
    assert_eq!(sub_a["items"][0]["title_snapshot"], "Wave3 Panjabi");
    let sub_a_id = sub_a["id"].as_str().expect("sub-order id").to_string();
    let sub_b = sub_orders
        .iter()
        .find(|s| s["store_id"] == store_b)
        .expect("store B sub-order");
    assert_eq!(sub_b["goods_subtotal_poisha"], 200_000);
    assert_eq!(sub_b["commission_poisha"], 16_000);
    assert_eq!(sub_b["seller_payout_poisha"], 184_000);

    // 3. Stock decremented, cart empty after checkout.
    let uri = format!("/v1/products/{product_a}");
    let (status, body) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["stock"], 4);
    let uri = format!("/v1/products/{product_b}");
    let (_, body) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(body["stock"], 2);

    let (status, body) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["goods_total_poisha"], 0);
    assert_eq!(body["items"].as_array().map(Vec::len), Some(0));

    // 4. Buyer order history shows the order; the detail endpoint too.
    let (status, body) = call(&app, Method::GET, "/v1/orders", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer orders: {body}");
    assert_eq!(body["total"], 1);
    let uri = format!("/v1/orders/{order_id}");
    let (status, body) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["grand_total_poisha"], 469_000);
    // Another user cannot read the buyer's order.
    let (status, _) = call(&app, Method::GET, &uri, None, Some(&seller_a)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 5. Non-COD payment methods are rejected (Wave 5 will add them).
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_b, "quantity": 1 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01911111111",
            "delivery_address": "House 1, Village Road, Rajshahi",
            "delivery_zone": "outside_dhaka",
            "payment_method": "bkash"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&app, Method::DELETE, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);

    // 6. Insufficient stock: a product with stock 1, cart asks for 2.
    let scarce = create_product(&app, &seller_a, &store_a, "Wave3 Scarce", 10_000, 1).await;
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": scarce, "quantity": 2 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01911111111",
            "delivery_address": "House 1, Village Road, Rajshahi",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "insufficient stock: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("Wave3 Scarce"),
        "409 names the product: {body}"
    );
    // The failed checkout must not have decremented stock or cleared the cart.
    let uri = format!("/v1/products/{scarce}");
    let (_, body) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(body["stock"], 1);
    let (status, _) = call(&app, Method::DELETE, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);

    // 7. Seller sub-order list + status transitions.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/sub-orders",
        None,
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller sub-orders: {body}");
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["id"], sub_a_id);
    assert_eq!(body["items"][0]["delivery_zone"], "outside_dhaka");

    let status_uri = format!("/v1/seller/sub-orders/{sub_a_id}/status");
    // Illegal skip: placed -> delivered is rejected.
    let (status, _) = call(
        &app,
        Method::PATCH,
        &status_uri,
        Some(json!({ "status": "delivered" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Another seller cannot touch it.
    let (status, _) = call(
        &app,
        Method::PATCH,
        &status_uri,
        Some(json!({ "status": "confirmed" })),
        Some(&seller_b),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The legal forward path works: placed -> confirmed -> shipped -> delivered.
    for next in ["confirmed", "shipped", "delivered"] {
        let (status, body) = call(
            &app,
            Method::PATCH,
            &status_uri,
            Some(json!({ "status": next })),
            Some(&seller_a),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "transition to {next}: {body}");
        assert_eq!(body["status"], next);
    }
    // Terminal: delivered -> cancelled is rejected.
    let (status, _) = call(
        &app,
        Method::PATCH,
        &status_uri,
        Some(json!({ "status": "cancelled" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
