//! Wave 17 integration test: partial advance for COD + the COD
//! fraud shield — below-threshold COD unchanged, above-threshold
//! COD demanding an advance method (400 without one), the advance
//! settling through dev-simulate into `advance_paid` (receipt
//! cash-due netted, shipment booking unblocked, courier collects
//! the balance), a risk-forced advance for a seeded high-risk
//! buyer, the seller buyer-risk endpoint, the admin threshold
//! switch, and the deliberate degradation on an app with NO
//! gateway available (order proceeds as flagged plain COD — the
//! path the Wave 3 / Wave 14 test apps take).
//! Driven through the real Axum router with tower oneshot. The
//! small helpers are copied from the Wave 5 / Wave 9 tests on
//! purpose: integration test files stay self-contained rather
//! than sharing test code across files.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so
//! a plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL` for
//! the rust job.
//!
//! The payments DEV simulator and the courier DEV simulator are
//! ON for the main app (explicit dev flags) — the only way an
//! advance can be collected here; every reference the payment
//! simulator mints is prefixed `DEV-`. A second app instance
//! with payments dev mode OFF covers the no-gateway degradation.

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
use uuid::Uuid;

const DEV_SECRET: &str = "wave17-test-dev-secret";

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

/// Checkout the buyer's current cart with COD, optionally naming
/// an advance payment method (Wave 17).
async fn checkout_cod(
    app: &Router,
    token: &str,
    advance_method: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = json!({
        "delivery_name": "Test Buyer",
        "delivery_phone": "01944444444",
        "delivery_address": "House 3, Road 5, Chattogram",
        "delivery_zone": "inside_dhaka",
        "payment_method": "cod"
    });
    if let Some(method) = advance_method {
        body["advance_method"] = json!(method);
    }
    call(app, Method::POST, "/v1/orders", Some(body), Some(token)).await
}

/// The buyer's view of one order.
async fn get_order(app: &Router, token: &str, order_id: &str) -> Value {
    let uri = format!("/v1/orders/{order_id}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "get order: {body}");
    body
}

/// Move one of the seller's sub-orders to a new status.
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

/// Book a shipment for a sub-order with an EMPTY body (courier
/// dev mode: the configured default is served by the simulator).
async fn book_shipment(
    app: &Router,
    token: &str,
    sub_order_id: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    call(app, Method::POST, &uri, Some(json!({})), Some(token)).await
}

/// How many payment rows an order has (SQL ground truth).
async fn payment_row_count(pool: &PgPool, order_id: &str) -> i64 {
    let order_uuid = Uuid::parse_str(order_id).expect("order id is a uuid");
    sqlx::query_scalar("SELECT COUNT(*) FROM payments WHERE order_id = $1")
        .bind(order_uuid)
        .fetch_one(pool)
        .await
        .expect("count payment rows")
}

#[tokio::test]
async fn wave17_cod_advance_and_fraud_shield() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 17 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE platform_settings, sidra_suggestions, messages, conversations, \
         return_requests, coupon_redemptions, coupons, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         products, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // Main app: SMS dev mode (OTP login), payments DEV simulator
    // (advances can be collected) and courier DEV simulator
    // (shipments can be booked) all on.
    let state = AppState::new(Some(pool.clone()), "wave17-test-secret".to_string(), true)
        .with_payments_config(PaymentsConfig {
            dev_mode: true,
            dev_secret: DEV_SECRET.to_string(),
            ..PaymentsConfig::default()
        })
        .with_couriers_config(CouriersConfig {
            dev_mode: true,
            ..CouriersConfig::default()
        });
    let app = build_app(state);

    // One seller, one store, two products (default 1 kg each):
    // a 1,000-taka one (grand 106,000 — below the 200,000
    // threshold) and a 2,000-taka one (grand 206,000 — above).
    let seller = login(&app, "01744444444").await;
    let store = create_store(&app, &seller, "wave17-store").await;
    let cheap = create_product(&app, &seller, &store, "Wave17 Cheap", 100_000, 50).await;
    let pricey = create_product(&app, &seller, &store, "Wave17 Pricey", 200_000, 50).await;

    // An admin for the settings phase, promoted by SQL only.
    let admin = login(&app, "01755555555").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801755555555'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let buyer = login(&app, "01944444444").await;

    // The public policy endpoint reports the defaults.
    let (status, policy) = call(&app, Method::GET, "/v1/payments/advance-policy", None, None).await;
    assert_eq!(status, StatusCode::OK, "advance policy: {policy}");
    assert_eq!(policy["percent_bps"], 2_000);
    assert_eq!(policy["threshold_poisha"], 200_000);

    // 1. Below-threshold COD: unchanged Wave 3 behaviour —
    // pay_on_delivery, no advance fields set, NO payment row.
    add_to_cart(&app, &buyer, &cheap, 1).await;
    let (status, order) = checkout_cod(&app, &buyer, None).await;
    assert_eq!(status, StatusCode::CREATED, "below-threshold COD: {order}");
    assert_eq!(order["grand_total_poisha"], 106_000);
    assert_eq!(order["payment_status"], "pay_on_delivery");
    assert_eq!(order["advance_required_poisha"], 0);
    assert_eq!(order["advance_paid_poisha"], 0);
    assert!(order["payment"].is_null(), "no payment row: {order}");
    let order_id = order["id"].as_str().expect("order id").to_string();
    assert_eq!(payment_row_count(&pool, &order_id).await, 0);

    // 2. Above-threshold COD without an advance method: 400
    // naming the advance, and the cart survives.
    add_to_cart(&app, &buyer, &pricey, 1).await;
    let (status, body) = checkout_cod(&app, &buyer, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no advance method: {body}");
    let error = body["error"].as_str().expect("error message");
    assert!(
        error.contains("advance"),
        "the 400 tells the buyer to choose an advance method: {error}"
    );
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cart after 400: {cart}");
    assert_eq!(
        cart["items"].as_array().expect("cart items").len(),
        1,
        "rejected checkout leaves the cart intact: {cart}"
    );

    // 3. The same cart with advance_method bkash: the order is
    // advance_pending for exactly 20% of 206,000 = 41,200, with
    // one pending DEV payment row of that amount.
    let (status, order) = checkout_cod(&app, &buyer, Some("bkash")).await;
    assert_eq!(status, StatusCode::CREATED, "advance checkout: {order}");
    assert_eq!(order["grand_total_poisha"], 206_000);
    assert_eq!(order["payment_status"], "advance_pending");
    assert_eq!(order["advance_required_poisha"], 41_200);
    assert_eq!(order["advance_paid_poisha"], 0);
    assert_eq!(order["payment"]["method"], "bkash");
    assert_eq!(order["payment"]["status"], "pending");
    let order_id = order["id"].as_str().expect("order id").to_string();
    let payment_id = order["payment"]["id"]
        .as_str()
        .expect("payment id")
        .to_string();
    let ref_expected = format!("DEV-bkash-{order_id}");
    assert_eq!(order["payment"]["gateway_ref"], ref_expected);
    assert_eq!(payment_row_count(&pool, &order_id).await, 1);
    let payment_amount: i64 = sqlx::query_scalar(
        "SELECT amount_poisha FROM payments WHERE order_id = $1",
    )
    .bind(Uuid::parse_str(&order_id).expect("order uuid"))
    .fetch_one(&pool)
    .await
    .expect("advance payment amount");
    assert_eq!(payment_amount, 41_200, "the payment is the ADVANCE, not the total");
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    // Confirming is allowed while the advance is unpaid...
    let (status, body) = set_sub_order_status(&app, &seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm advance order: {body}");
    // ...but booking a courier is a 409 until the advance lands.
    let (status, body) = book_shipment(&app, &seller, &sub_order_id).await;
    assert_eq!(status, StatusCode::CONFLICT, "booking blocked: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error")
            .contains("advance payment pending"),
        "the guard names the unpaid advance: {body}"
    );

    // 4. Settle the advance through dev-simulate (the shared
    // callback path): the order becomes advance_paid — NOT paid —
    // with the collected amount recorded.
    let simulate_uri = format!("/v1/payments/{payment_id}/dev-simulate-success");
    let (status, payment) = call(&app, Method::POST, &simulate_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "dev simulate advance: {payment}");
    assert_eq!(payment["status"], "paid");
    let order = get_order(&app, &buyer, &order_id).await;
    assert_eq!(order["payment_status"], "advance_paid");
    assert_eq!(order["advance_paid_poisha"], 41_200);

    // The receipt nets the advance out of the cash due:
    // 206,000 − 41,200 = 164,800 still to hand over on delivery.
    let receipt_uri = format!("/v1/orders/{order_id}/receipt");
    let (status, receipt) = call(&app, Method::GET, &receipt_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "receipt: {receipt}");
    assert_eq!(receipt["advance_required_poisha"], 41_200);
    assert_eq!(receipt["advance_paid_poisha"], 41_200);
    assert_eq!(receipt["cod_amount_due_poisha"], 164_800);

    // Booking works now, and the courier collects exactly the
    // remaining balance.
    let (status, shipment) = book_shipment(&app, &seller, &sub_order_id).await;
    assert_eq!(status, StatusCode::CREATED, "booking after advance: {shipment}");
    assert_eq!(shipment["cod_amount_poisha"], 164_800);

    // The seller sub-order list carries the risk verdict and the
    // advance state for this order.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/seller/sub-orders?limit=50",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller sub-orders: {list}");
    let listed = list["items"]
        .as_array()
        .expect("sub-order list")
        .iter()
        .find(|sub| sub["id"] == sub_order_id)
        .expect("the advance sub-order is listed")
        .clone();
    assert_eq!(listed["order_payment_status"], "advance_paid");
    assert_eq!(listed["advance_required_poisha"], 41_200);
    assert_eq!(listed["advance_paid_poisha"], 41_200);
    assert_eq!(listed["buyer_risk_level"], "low");

    // 5. A high-risk buyer: two seeded returned sub-orders (the
    // fixture is direct SQL — driving two full RTO flows would
    // take the whole test; the aggregate reads statuses only).
    // Every completed sub-order came back, so the shield forces
    // an advance even on a below-threshold order.
    let risky = login(&app, "01966666666").await;
    let risky_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE phone = '+8801966666666'")
        .fetch_one(&pool)
        .await
        .expect("risky buyer id");
    let store_uuid = Uuid::parse_str(&store).expect("store uuid");
    for _ in 0..2 {
        let seeded_order: Uuid = sqlx::query_scalar(
            "INSERT INTO orders (buyer_user_id, delivery_name, delivery_phone, \
             delivery_address, delivery_zone, goods_total_poisha, \
             delivery_total_poisha, grand_total_poisha) \
             VALUES ($1, 'Risk Fixture', '+8801966666666', 'Fixture address', \
             'inside_dhaka', 100000, 6000, 106000) RETURNING id",
        )
        .bind(risky_id)
        .fetch_one(&pool)
        .await
        .expect("seed fixture order");
        sqlx::query(
            "INSERT INTO sub_orders (order_id, store_id, status, \
             goods_subtotal_poisha, delivery_charge_poisha, commission_poisha, \
             seller_payout_poisha) VALUES ($1, $2, 'returned', 100000, 6000, 8000, 92000)",
        )
        .bind(seeded_order)
        .bind(store_uuid)
        .execute(&pool)
        .await
        .expect("seed fixture sub-order");
    }

    add_to_cart(&app, &risky, &cheap, 1).await;
    let (status, body) = checkout_cod(&app, &risky, None).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "risk-forced advance still needs a method: {body}"
    );
    let (status, order) = checkout_cod(&app, &risky, Some("bkash")).await;
    assert_eq!(status, StatusCode::CREATED, "risk-forced checkout: {order}");
    assert_eq!(order["grand_total_poisha"], 106_000);
    assert_eq!(order["payment_status"], "advance_pending");
    assert_eq!(
        order["advance_required_poisha"], 21_200,
        "20% of 106,000, forced by risk despite the small total"
    );

    // 6. The seller buyer-risk endpoint agrees with the seeded
    // history (2 fixture orders + the 1 real checkout = 3 total).
    let (status, risk) = call(
        &app,
        Method::GET,
        "/v1/seller/buyer-risk?phone=01966666666",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "buyer risk: {risk}");
    assert_eq!(risk["found"], true);
    assert_eq!(risk["total_orders"], 3);
    assert_eq!(risk["delivered"], 0);
    assert_eq!(risk["returned_rto"], 2);
    assert_eq!(risk["buyer_cancelled"], 0);
    assert_eq!(risk["risk_level"], "high");
    assert_eq!(risk["requires_advance"], true);

    // The clean buyer reads low with their real counts (3 orders:
    // below-threshold, advance, and — not yet — the flagged one).
    let (status, risk) = call(
        &app,
        Method::GET,
        "/v1/seller/buyer-risk?phone=01944444444",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "clean buyer risk: {risk}");
    assert_eq!(risk["found"], true);
    assert_eq!(risk["total_orders"], 2);
    assert_eq!(risk["risk_level"], "low");
    assert_eq!(risk["requires_advance"], false);

    // Unknown phone: found false, zeroed, low — no oracle.
    let (status, risk) = call(
        &app,
        Method::GET,
        "/v1/seller/buyer-risk?phone=01977777777",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unknown buyer risk: {risk}");
    assert_eq!(risk["found"], false);
    assert_eq!(risk["total_orders"], 0);
    assert_eq!(risk["risk_level"], "low");

    // A non-seller (the clean buyer owns no store) is refused.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/buyer-risk?phone=01966666666",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "non-seller lookup: {body}");

    // 7. Admin turns the threshold off: the settings view shows
    // the advance fields, the policy endpoint follows, and a new
    // above-threshold COD order from the clean buyer needs no
    // advance at all.
    let (status, settings) = call(
        &app,
        Method::PUT,
        "/v1/admin/settings",
        Some(json!({ "advance_threshold_poisha": 0 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update settings: {settings}");
    assert_eq!(settings["advance_threshold_poisha"], 0);
    assert_eq!(settings["advance_percent_bps"], 2_000);
    let (status, policy) = call(&app, Method::GET, "/v1/payments/advance-policy", None, None).await;
    assert_eq!(status, StatusCode::OK, "policy after update: {policy}");
    assert_eq!(policy["threshold_poisha"], 0);

    add_to_cart(&app, &buyer, &pricey, 1).await;
    let (status, order) = checkout_cod(&app, &buyer, None).await;
    assert_eq!(status, StatusCode::CREATED, "threshold off checkout: {order}");
    assert_eq!(order["payment_status"], "pay_on_delivery");
    assert_eq!(order["advance_required_poisha"], 0);

    // Settings are global (the Wave 10 lesson): remove the rows
    // this test wrote so later suites see the defaults.
    sqlx::query("DELETE FROM platform_settings")
        .execute(&pool)
        .await
        .expect("clean platform_settings");
    let (status, policy) = call(&app, Method::GET, "/v1/payments/advance-policy", None, None).await;
    assert_eq!(status, StatusCode::OK, "policy after cleanup: {policy}");
    assert_eq!(policy["threshold_poisha"], 200_000);

    // 8. The degradation path: a second app with NO gateway
    // available (payments dev mode off, no credentials — the
    // Wave 3 / Wave 14 test apps' shape). The same above-threshold
    // COD checkout proceeds as plain COD with the required
    // advance recorded as a flag: totals unchanged, payment
    // status pay_on_delivery, no payment row, nothing collected.
    let bare_state = AppState::new(Some(pool.clone()), "wave17-test-secret".to_string(), true);
    let bare_app = build_app(bare_state);
    add_to_cart(&bare_app, &buyer, &pricey, 1).await;
    let (status, order) = checkout_cod(&bare_app, &buyer, None).await;
    assert_eq!(status, StatusCode::CREATED, "flagged checkout: {order}");
    assert_eq!(order["grand_total_poisha"], 206_000);
    assert_eq!(order["payment_status"], "pay_on_delivery");
    assert_eq!(order["advance_required_poisha"], 41_200);
    assert_eq!(order["advance_paid_poisha"], 0);
    assert!(order["payment"].is_null(), "no payment row: {order}");
    let order_id = order["id"].as_str().expect("order id").to_string();
    assert_eq!(payment_row_count(&pool, &order_id).await, 0);
}
