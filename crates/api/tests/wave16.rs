//! Wave 16 integration test: public tracking and returns — the
//! tracking view answers only to the order's delivery phone
//! (wrong phone, unknown id and malformed id are the same 404),
//! a return can be requested only for a DELIVERED sub-order
//! (409 before delivery, 409 on a second request, 404 for a
//! stranger), the seller's approve -> received flow snapshots
//! the refund as goods minus discount (delivery never refunded),
//! moves the sub-order to `returned` and restores the product
//! stock, the refund opens as `pending` and an admin marks it
//! `done` exactly once, and the reject path is terminal. The
//! small helpers are copied from the Wave 9 test on purpose,
//! since integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so
//! a plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`.

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

/// Checkout the buyer's current cart with COD, delivering to the
/// buyer's own phone number — that number is the tracking
/// credential this wave's test exercises.
async fn checkout_cod(app: &Router, token: &str, delivery_phone: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": delivery_phone,
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

/// Buy one unit and drive the order all the way to delivered
/// through the DEV courier; returns (order id, sub-order id).
async fn buy_and_deliver(
    app: &Router,
    buyer: &str,
    buyer_phone: &str,
    seller: &str,
    product_id: &str,
) -> (String, String) {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer, buyer_phone).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
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
    (order_id, sub_order_id)
}

/// The product's current stock, read through the public view.
async fn product_stock(app: &Router, product_id: &str) -> i64 {
    let uri = format!("/v1/products/{product_id}");
    let (status, body) = call(app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "get product: {body}");
    body["stock"].as_i64().expect("stock is a number")
}

/// POST a return request for a sub-order as a buyer.
async fn request_return(
    app: &Router,
    token: &str,
    sub_order_id: &str,
    reason: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/buyer/sub-orders/{sub_order_id}/return-request");
    call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "reason": reason })),
        Some(token),
    )
    .await
}

#[tokio::test]
async fn wave16_tracking_and_returns_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 16 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE return_requests, sidra_suggestions, messages, conversations, \
         platform_settings, coupon_redemptions, coupons, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode (deliveries) on.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave16-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // Cast: an admin (promoted by SQL, the only way), one seller,
    // the buyer, and a stranger who owns nothing.
    let admin = login(&app, "01511111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801511111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave16-store").await;
    // 200_000 poisha of goods, no coupon: a received return must
    // refund exactly 200_000 — the 6_000 inside-Dhaka delivery
    // charge is never part of a refund.
    let product = create_product(&app, &seller, &store, "Wave16 Product", 200_000, 10).await;
    let buyer = login(&app, "01811111111").await;
    let stranger = login(&app, "01611111111").await;

    // 1. Buy one unit but do NOT deliver yet: the order exists in
    // `placed`, stock dropped to 9.
    add_to_cart(&app, &buyer, &product, 1).await;
    let order = checkout_cod(&app, &buyer, "01811111111").await;
    let order_id = order["id"].as_str().expect("order id").to_string();
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    assert_eq!(product_stock(&app, &product).await, 9);

    // 2. Tracking: wrong phone, unknown id and malformed id are
    // all the same indistinguishable 404.
    let uri = format!("/v1/track?order_id={order_id}&phone=01822222222");
    let (status, body) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "wrong phone: {body}");

    let uri = "/v1/track?order_id=00000000-0000-0000-0000-000000000000&phone=01811111111";
    let (status, body) = call(&app, Method::GET, uri, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown order: {body}");

    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/track?order_id=not-a-uuid&phone=01811111111",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "malformed id: {body}");

    // The right phone (local form) opens the timeline: no shipment
    // yet, sub-order placed, grand total = goods + delivery.
    let uri = format!("/v1/track?order_id={order_id}&phone=01811111111");
    let (status, tracked) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "track: {tracked}");
    assert_eq!(tracked["order_id"], order_id);
    assert_eq!(tracked["payment_status"], "pay_on_delivery");
    assert_eq!(tracked["payment_method"], "cod");
    assert_eq!(tracked["grand_total_poisha"], 206_000);
    assert_eq!(tracked["sub_orders"][0]["store_slug"], "wave16-store");
    assert_eq!(tracked["sub_orders"][0]["status"], "placed");
    assert!(tracked["sub_orders"][0]["shipment"].is_null());

    // The international form of the same number matches too.
    let uri = format!("/v1/track?order_id={order_id}&phone=%2B8801811111111");
    let (status, _) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "track with +880 phone");

    // 3. A return for a merely-PLACED sub-order is a 409.
    let (status, body) = request_return(&app, &buyer, &sub_order_id, "ছিঁড়ে গেছে").await;
    assert_eq!(status, StatusCode::CONFLICT, "return before delivery: {body}");

    // 4. Deliver the parcel through the DEV courier (the cart was
    // consumed by the checkout above, so drive this same order:
    // confirm, book, refresh until delivered).
    let (status, body) = set_sub_order_status(&app, &seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(&app, Method::POST, &uri, Some(json!({})), Some(&seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }

    // Tracking now shows the shipment with its event history.
    let uri = format!("/v1/track?order_id={order_id}&phone=01811111111");
    let (status, tracked) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "track delivered: {tracked}");
    assert_eq!(tracked["sub_orders"][0]["status"], "delivered");
    let shipment = &tracked["sub_orders"][0]["shipment"];
    assert_eq!(shipment["status"], "delivered");
    let events = shipment["events"].as_array().expect("shipment events");
    assert!(events.len() >= 2, "event history present: {events:?}");
    assert_eq!(events[events.len() - 1]["status"], "delivered");

    // 5. A stranger cannot request a return on the buyer's
    // sub-order (404 — the sub-order is not theirs to see).
    let (status, body) = request_return(&app, &stranger, &sub_order_id, "আমার নয়").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger return: {body}");

    // 6. The buyer requests the return: an empty reason is a 400,
    // a real one creates the request.
    let (status, body) = request_return(&app, &buyer, &sub_order_id, "   ").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty reason: {body}");

    let (status, created) =
        request_return(&app, &buyer, &sub_order_id, "পণ্যটি ছিঁড়ে গেছে").await;
    assert_eq!(status, StatusCode::CREATED, "create return: {created}");
    assert_eq!(created["status"], "requested");
    assert_eq!(created["refund_status"], "none");
    assert!(created["refund_poisha"].is_null());
    let return_id = created["id"].as_str().expect("return id").to_string();

    // A second request for the same sub-order is a 409.
    let (status, body) = request_return(&app, &buyer, &sub_order_id, "আবার").await;
    assert_eq!(status, StatusCode::CONFLICT, "second return: {body}");

    // The buyer's list shows it, and the order view summarises it.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/buyer/return-requests",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "buyer returns: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], return_id);

    let uri = format!("/v1/orders/{order_id}");
    let (status, order_view) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer order: {order_view}");
    assert_eq!(order_view["sub_orders"][0]["return_status"], "requested");

    // 7. The seller cannot mark it received before approving.
    let uri = format!("/v1/seller/return-requests/{return_id}/received");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::CONFLICT, "received before approve: {body}");

    // The seller's list carries the masked buyer phone + items.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/seller/return-requests",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller returns: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["buyer_phone_masked"], "018****111");
    assert_eq!(list["items"][0]["items"][0]["title_snapshot"], "Wave16 Product");

    // Approve, then approve again (illegal) -> 409.
    let uri = format!("/v1/seller/return-requests/{return_id}/approve");
    let (status, approved) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "note": "ফেরত পাঠিয়ে দিন" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve: {approved}");
    assert_eq!(approved["status"], "approved");
    assert_eq!(approved["seller_note"], "ফেরত পাঠিয়ে দিন");

    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({})),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "approve twice: {body}");

    // 8. Received: refund snapshot = full goods (no coupon),
    // refund pending, sub-order returned, stock restored to 10.
    let uri = format!("/v1/seller/return-requests/{return_id}/received");
    let (status, received) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "received: {received}");
    assert_eq!(received["status"], "received");
    assert_eq!(received["refund_poisha"], 200_000);
    assert_eq!(received["refund_status"], "pending");
    assert_eq!(product_stock(&app, &product).await, 10);

    let uri = format!("/v1/orders/{order_id}");
    let (status, order_view) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer order after return: {order_view}");
    assert_eq!(order_view["sub_orders"][0]["status"], "returned");
    assert_eq!(order_view["sub_orders"][0]["return_status"], "received");

    // 9. The admin refund queue: a non-admin is a 403; the queue
    // filtered to pending holds exactly this refund; marking it
    // done works once and only once.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/return-requests?refund_status=pending",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "admin list as seller: {body}");

    let (status, queue) = call(
        &app,
        Method::GET,
        "/v1/admin/return-requests?refund_status=pending",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin queue: {queue}");
    assert_eq!(queue["total"], 1);
    assert_eq!(queue["items"][0]["id"], return_id);

    let uri = format!("/v1/admin/return-requests/{return_id}/refund-done");
    let (status, done) = call(&app, Method::POST, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "refund done: {done}");
    assert_eq!(done["refund_status"], "done");

    let (status, body) = call(&app, Method::POST, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::CONFLICT, "refund done twice: {body}");

    // 10. Reject path: a second delivered order, a return the
    // seller rejects — terminal for approve and received alike.
    let (_, sub2) = buy_and_deliver(&app, &buyer, "01811111111", &seller, &product).await;
    let (status, created) = request_return(&app, &buyer, &sub2, "ভুল রং এসেছে").await;
    assert_eq!(status, StatusCode::CREATED, "create return 2: {created}");
    let return2_id = created["id"].as_str().expect("return 2 id").to_string();

    let uri = format!("/v1/seller/return-requests/{return2_id}/reject");
    let (status, rejected) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "note": "ব্যবহৃত পণ্য ফেরত নেওয়া হয় না" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reject: {rejected}");
    assert_eq!(rejected["status"], "rejected");
    assert_eq!(rejected["refund_status"], "none");
    assert!(rejected["refund_poisha"].is_null());

    let uri = format!("/v1/seller/return-requests/{return2_id}/approve");
    let (status, body) = call(&app, Method::POST, &uri, Some(json!({})), Some(&seller)).await;
    assert_eq!(status, StatusCode::CONFLICT, "approve after reject: {body}");

    let uri = format!("/v1/seller/return-requests/{return2_id}/received");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::CONFLICT, "received after reject: {body}");
}
