//! Wave 35 integration test: buyer loyalty points — a delivered
//! sub-order earns 1 point per 100 taka of net goods (through
//! BOTH delivery funnels: the courier shipment sync and the
//! seller status PATCH), a repeat delivery attempt never earns
//! twice, checkout redeems points at 1 point = 100 poisha
//! clamped to half the post-coupon goods with the grand total
//! (and the advance decision) reading the reduced payable, the
//! seller payout snapshot is identical to a no-points order
//! (the platform funds the redemption), a buyer cancel refunds
//! the spent points, a received return claws the earn back,
//! and the ledger history records every movement in order.
//! The small helpers are copied from the Wave 9 test on
//! purpose, since integration test files stay self-contained.
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

/// Checkout the buyer's current cart with COD and a loyalty
/// points request; returns the raw (status, body) so the test
/// can assert rejections as well as placements.
async fn checkout_with_points(
    app: &Router,
    token: &str,
    points: i64,
) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "loyalty_points": points
        })),
        Some(token),
    )
    .await
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

/// GET the seller balance for a token.
async fn get_balance(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/seller/balance", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "balance: {body}");
    body
}

/// GET the buyer's loyalty view (balance + rules + history).
async fn get_loyalty(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/loyalty", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "loyalty: {body}");
    body
}

/// Drive one sub-order to delivered through the seller PATCH
/// funnel (confirmed -> shipped -> delivered, no courier).
async fn deliver_via_patch(app: &Router, seller: &str, sub_order_id: &str) {
    for status in ["confirmed", "shipped", "delivered"] {
        let (code, body) = set_sub_order_status(app, seller, sub_order_id, status).await;
        assert_eq!(code, StatusCode::OK, "patch to {status}: {body}");
    }
}

/// Ask to return one delivered sub-order as its buyer.
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
async fn wave35_loyalty_earn_redeem_reverse_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 35 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE loyalty_points, notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_variants, products, store_members, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode (deliveries) on.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave35-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave35-store").await;
    // The standard product: 200_000 poisha of goods, inside-Dhaka
    // delivery 6_000, 8% commission 16_000, payout 184_000.
    let product = create_product(&app, &seller, &store, "Wave35 Product", 200_000, 100).await;
    // A big-ticket product whose delivery earns 2_000 points —
    // the balance the cap step needs.
    let big_product =
        create_product(&app, &seller, &store, "Wave35 Big Product", 20_000_000, 2).await;
    let buyer = login(&app, "01999999999").await;

    // 1. The loyalty endpoint needs auth, and a fresh buyer is
    // zeroed with an empty history.
    let (status, body) = call(&app, Method::GET, "/v1/loyalty", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "loyalty without auth: {body}");
    let loyalty = get_loyalty(&app, &buyer).await;
    assert_eq!(loyalty["available_points"], 0);
    assert_eq!(loyalty["lifetime_earned"], 0);
    assert_eq!(loyalty["lifetime_redeemed"], 0);
    assert_eq!(loyalty["earn_poisha_per_point"], 10_000);
    assert_eq!(loyalty["point_value_poisha"], 100);
    assert_eq!(loyalty["history"].as_array().map(Vec::len), Some(0));

    // 2. Order 1, delivered through the DEV courier (the
    // shipment-sync funnel): earns 200_000 / 10_000 = 20 points.
    add_to_cart(&app, &buyer, &product, 1).await;
    let order1 = checkout_cod(&app, &buyer).await;
    assert_eq!(order1["loyalty_points_used"], 0);
    assert_eq!(order1["grand_total_poisha"], 206_000);
    let sub1 = order1["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let payout1 = order1["sub_orders"][0]["seller_payout_poisha"]
        .as_i64()
        .expect("payout snapshot");
    assert_eq!(payout1, 184_000);

    let (status, body) = set_sub_order_status(&app, &seller, &sub1, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub1}/shipment");
    let (status, shipment) = call(&app, Method::POST, &uri, Some(json!({})), Some(&seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment1 = shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment1}/refresh");
        let (status, _) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }

    let loyalty = get_loyalty(&app, &buyer).await;
    assert_eq!(loyalty["available_points"], 20, "loyalty: {loyalty}");
    assert_eq!(loyalty["lifetime_earned"], 20);
    assert_eq!(loyalty["history"][0]["reason"], "earn");
    assert_eq!(loyalty["history"][0]["delta"], 20);

    // 3. No double earn: re-delivering is an illegal transition
    // (400) and another courier refresh changes nothing.
    let (status, body) = set_sub_order_status(&app, &seller, &sub1, "delivered").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "re-deliver: {body}");
    let uri = format!("/v1/shipments/{shipment1}/refresh");
    let (status, _) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "refresh after delivered");
    let loyalty = get_loyalty(&app, &buyer).await;
    assert_eq!(loyalty["available_points"], 20, "still 20: {loyalty}");

    // 4. A negative points request is a 400 and the cart
    // survives it (order 2 below checks out the same cart).
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, body) = checkout_with_points(&app, &buyer, -5).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "negative points: {body}");

    // 5. Order 2 redeems 15 of the 20 points: discount 1_500,
    // grand 206_000 - 1_500 = 204_500, and the seller payout
    // snapshot is EXACTLY the no-points order 1 figure — the
    // platform funds the redemption.
    let (status, order2) = checkout_with_points(&app, &buyer, 15).await;
    assert_eq!(status, StatusCode::CREATED, "order 2: {order2}");
    assert_eq!(order2["loyalty_points_used"], 15);
    assert_eq!(order2["loyalty_discount_poisha"], 1_500);
    assert_eq!(order2["grand_total_poisha"], 204_500);
    assert_eq!(order2["sub_orders"][0]["loyalty_discount_poisha"], 1_500);
    assert_eq!(
        order2["sub_orders"][0]["seller_payout_poisha"], payout1,
        "platform-funded: payout unchanged: {order2}"
    );
    let order2_id = order2["id"].as_str().expect("order 2 id").to_string();
    let sub2 = order2["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order 2 id")
        .to_string();

    let loyalty = get_loyalty(&app, &buyer).await;
    assert_eq!(loyalty["available_points"], 5);
    assert_eq!(loyalty["lifetime_redeemed"], 15);

    // The receipt carries the loyalty line and its cash-due is
    // the reduced payable.
    let uri = format!("/v1/orders/{order2_id}/receipt");
    let (status, receipt) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "receipt: {receipt}");
    assert_eq!(receipt["loyalty_points_used"], 15);
    assert_eq!(receipt["loyalty_discount_poisha"], 1_500);
    assert_eq!(receipt["cod_amount_due_poisha"], 204_500);

    // 6. Deliver order 2 through the seller PATCH funnel: the
    // second earn lands (+20 -> 25), and the seller's earnings
    // are the two full no-discount payouts — loyalty never
    // touched the seller side.
    deliver_via_patch(&app, &seller, &sub2).await;
    let loyalty = get_loyalty(&app, &buyer).await;
    assert_eq!(loyalty["available_points"], 25, "loyalty: {loyalty}");
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 368_000, "balance: {balance}");

    // 7. The big order earns 2_000 points on delivery: balance
    // 2_025, enough that the cap — not the balance — binds next.
    add_to_cart(&app, &buyer, &big_product, 1).await;
    let big_order = checkout_cod(&app, &buyer).await;
    let big_sub = big_order["sub_orders"][0]["id"]
        .as_str()
        .expect("big sub-order id")
        .to_string();
    deliver_via_patch(&app, &seller, &big_sub).await;
    let loyalty = get_loyalty(&app, &buyer).await;
    assert_eq!(loyalty["available_points"], 2_025, "loyalty: {loyalty}");

    // 8. The cap: requesting 10_000 points on a 200_000-goods
    // order clamps to half the goods = 100_000 poisha = 1_000
    // points. The advance decision reads the REDUCED payable
    // (106_000 < the 200_000 threshold), so no advance is
    // required even though the pre-points total (206_000)
    // crosses the threshold.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order4) = checkout_with_points(&app, &buyer, 10_000).await;
    assert_eq!(status, StatusCode::CREATED, "cap order: {order4}");
    assert_eq!(order4["loyalty_points_used"], 1_000);
    assert_eq!(order4["loyalty_discount_poisha"], 100_000);
    assert_eq!(order4["grand_total_poisha"], 106_000);
    assert_eq!(
        order4["advance_required_poisha"], 0,
        "advance reads the reduced payable: {order4}"
    );
    let order4_id = order4["id"].as_str().expect("order 4 id").to_string();
    let loyalty = get_loyalty(&app, &buyer).await;
    assert_eq!(loyalty["available_points"], 1_025);

    // 9. The buyer cancels the still-placed cap order: its
    // 1_000 points come back as an adjust row.
    let uri = format!("/v1/orders/{order4_id}/cancel");
    let (status, cancelled) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cancel cap order: {cancelled}");
    assert_eq!(cancelled["status"], "cancelled");
    let loyalty = get_loyalty(&app, &buyer).await;
    assert_eq!(loyalty["available_points"], 2_025, "refunded: {loyalty}");

    // 10. Return order 2 and have it received: the earn it
    // produced (+20) is clawed back (2_025 - 20 = 2_005).
    let (status, ret) = request_return(&app, &buyer, &sub2, "Damaged item").await;
    assert_eq!(status, StatusCode::CREATED, "return request: {ret}");
    let return_id = ret["id"].as_str().expect("return id").to_string();
    let uri = format!("/v1/seller/return-requests/{return_id}/approve");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "note": "ফেরত পাঠিয়ে দিন" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve return: {body}");
    let uri = format!("/v1/seller/return-requests/{return_id}/received");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "receive return: {body}");
    let loyalty = get_loyalty(&app, &buyer).await;
    assert_eq!(loyalty["available_points"], 2_005, "clawed back: {loyalty}");

    // 11. Courier collection netting: a new order spending 10
    // points books a parcel that collects goods + delivery
    // minus the 1_000-poisha points slice.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order6) = checkout_with_points(&app, &buyer, 10).await;
    assert_eq!(status, StatusCode::CREATED, "order 6: {order6}");
    assert_eq!(order6["loyalty_points_used"], 10);
    assert_eq!(order6["grand_total_poisha"], 205_000);
    let sub6 = order6["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order 6 id")
        .to_string();
    let (status, body) = set_sub_order_status(&app, &seller, &sub6, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm order 6: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub6}/shipment");
    let (status, shipment6) = call(&app, Method::POST, &uri, Some(json!({})), Some(&seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment 6: {shipment6}");
    assert_eq!(
        shipment6["cod_amount_poisha"], 205_000,
        "courier collects net of points: {shipment6}"
    );
    let shipment6_id = shipment6["id"].as_str().expect("shipment 6 id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment6_id}/refresh");
        let (status, _) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh order 6");
    }

    // 12. Final position + the full ledger, newest first:
    // every movement of the flow above, in order.
    let loyalty = get_loyalty(&app, &buyer).await;
    assert_eq!(loyalty["available_points"], 2_015, "final: {loyalty}");
    assert_eq!(loyalty["lifetime_earned"], 2_060);
    assert_eq!(loyalty["lifetime_redeemed"], 1_025);
    let history: Vec<(String, i64)> = loyalty["history"]
        .as_array()
        .expect("history list")
        .iter()
        .map(|entry| {
            (
                entry["reason"].as_str().expect("reason").to_string(),
                entry["delta"].as_i64().expect("delta"),
            )
        })
        .collect();
    assert_eq!(
        history,
        vec![
            ("earn".to_string(), 20),
            ("redeem".to_string(), -10),
            ("adjust".to_string(), -20),
            ("adjust".to_string(), 1_000),
            ("redeem".to_string(), -1_000),
            ("earn".to_string(), 2_000),
            ("earn".to_string(), 20),
            ("redeem".to_string(), -15),
            ("earn".to_string(), 20),
        ],
        "ledger order: {loyalty}"
    );
}
