//! Wave 39 integration tests: the security-fix wave, driven
//! end-to-end against the real router. One test function with
//! numbered sections (the crate's canon — the shared test
//! database makes parallel test functions unsafe):
//!
//! 1. A webhook signed with the OLD public dev secret
//!    ("dev-secret", audit CR-01) is rejected 401 and changes
//!    nothing; the same callback signed with the secret the app
//!    was actually configured with settles the payment.
//! 2. With payments dev mode OFF, a DEV reference cannot be
//!    settled even with the correct secret (audit HI-50).
//! 3. The OTP attempt cap (audit CR-01 in the full list): five
//!    wrong codes exhaust the cap and the CORRECT code is then
//!    rejected too; a fresh code works again.
//! 4. The payout double-spend race (audit CR-10): two concurrent
//!    payout requests against a balance that covers only one —
//!    exactly one succeeds. Plus the approve-time half: an admin
//!    cannot approve a payout the store's current earnings no
//!    longer cover.
//! 5. Paid-order cancels (audit CR-07): buyer whole-order cancel
//!    and seller sub-order cancel on a fully paid order are 409
//!    and restock nothing; an advance-paid COD order blocks the
//!    same way; a plain COD cancel still works (regression).
//! 6. A late webhook on a cancelled order (audit CR-08): the
//!    buyer cancel voids the pending payment, and a validly
//!    signed success callback afterwards moves nothing.
//! 7. Coupon COD booking (audit CR-06 in the full list): the
//!    courier's collection amount equals the receipt's cash due
//!    — the coupon discount is not collected twice.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so
//! a plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL` for
//! the rust job — and `ci_gate.rs` makes a CI run without it
//! fail loudly instead of skipping everything (audit CR-14).

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use courier::CouriersConfig;
use payments::{sign_dev_callback, PaymentsConfig};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// The secret THIS test configures on its app state — chosen by
/// the test, like wave5/wave17 do with theirs. The point of
/// section 1 is that the old PUBLIC default ("dev-secret") no
/// longer signs anything.
const TEST_SECRET: &str = "wave39-test-dev-secret";

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

/// Checkout the buyer's current cart. `payment_method` is "cod"
/// or an online method; `advance_method` / `coupon_code` are
/// added to the body when present.
async fn checkout(
    app: &Router,
    token: &str,
    payment_method: &str,
    advance_method: Option<&str>,
    coupon_code: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = json!({
        "delivery_name": "Test Buyer",
        "delivery_phone": "01939000099",
        "delivery_address": "House 9, Road 3, Khulna",
        "delivery_zone": "inside_dhaka",
        "payment_method": payment_method
    });
    if let Some(method) = advance_method {
        body["advance_method"] = json!(method);
    }
    if let Some(code) = coupon_code {
        body["coupon_code"] = json!(code);
    }
    call(app, Method::POST, "/v1/orders", Some(body), Some(token)).await
}

/// POST one gateway webhook callback for a payment reference.
async fn webhook(
    app: &Router,
    method: &str,
    payment_ref: &str,
    amount_poisha: i64,
    callback_status: &str,
    signature: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/payments/webhook/{method}");
    call(
        app,
        Method::POST,
        &uri,
        Some(json!({
            "payment_ref": payment_ref,
            "amount_poisha": amount_poisha,
            "status": callback_status,
            "signature": signature,
        })),
        None,
    )
    .await
}

/// The buyer's view of one order.
async fn get_order(app: &Router, token: &str, order_id: &str) -> Value {
    let uri = format!("/v1/orders/{order_id}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "get order: {body}");
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

/// GET the seller balance for a token.
async fn get_balance(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/seller/balance", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "balance: {body}");
    body
}

/// A product's stock, from the database (ground truth).
async fn product_stock(pool: &PgPool, product_id: &str) -> i32 {
    sqlx::query_scalar("SELECT stock FROM products WHERE id = $1")
        .bind(Uuid::parse_str(product_id).expect("product id is a uuid"))
        .fetch_one(pool)
        .await
        .expect("product stock")
}

/// A payment row's status, from the database (ground truth).
async fn payment_status_of(pool: &PgPool, payment_id: &str) -> String {
    sqlx::query_scalar("SELECT status FROM payments WHERE id = $1")
        .bind(Uuid::parse_str(payment_id).expect("payment id is a uuid"))
        .fetch_one(pool)
        .await
        .expect("payment status")
}

#[tokio::test]
async fn wave39_security_fixes_end_to_end() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 39 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE api_keys, loyalty_points, notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_variants, products, store_subscriptions, store_members, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // Main app: SMS dev mode (OTP login), payments DEV simulator
    // with THIS test's own secret, courier DEV simulator.
    let state = AppState::new(Some(pool.clone()), "wave39-test-secret".to_string(), true)
        .with_payments_config(PaymentsConfig {
            dev_mode: true,
            dev_secret: TEST_SECRET.to_string(),
            ..PaymentsConfig::default()
        })
        .with_couriers_config(CouriersConfig {
            dev_mode: true,
            ..CouriersConfig::default()
        });
    let app = build_app(state);

    // Second app over the SAME database with payments dev mode
    // OFF (section 2): the same secret is configured, but DEV
    // references must be unverifiable there.
    let state_no_dev = AppState::new(Some(pool.clone()), "wave39-test-secret".to_string(), true)
        .with_payments_config(PaymentsConfig {
            dev_mode: false,
            dev_secret: TEST_SECRET.to_string(),
            ..PaymentsConfig::default()
        });
    let app_no_dev = build_app(state_no_dev);

    // Cast: a payments seller + buyer, a payout seller + buyer,
    // a coupon seller + buyer, and an admin (promoted by SQL).
    let seller = login(&app, "01739000001").await;
    let store = create_store(&app, &seller, "wave39-store-a").await;
    let product = create_product(&app, &seller, &store, "Wave39 Product", 100_000, 20).await;
    let pricey = create_product(&app, &seller, &store, "Wave39 Pricey", 200_000, 10).await;
    let buyer = login(&app, "01939000001").await;

    // ---------------------------------------------------------------
    // 1. Forged webhook with the OLD public secret (audit CR-01).
    // ---------------------------------------------------------------
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order_a) = checkout(&app, &buyer, "bkash", None, None).await;
    assert_eq!(status, StatusCode::CREATED, "bkash checkout: {order_a}");
    assert_eq!(order_a["grand_total_poisha"], 106_000);
    assert_eq!(order_a["payment_status"], "pending");
    let order_a_id = order_a["id"].as_str().expect("order id").to_string();
    let payment_a_id = order_a["payment"]["id"]
        .as_str()
        .expect("payment id")
        .to_string();
    let ref_a = order_a["payment"]["gateway_ref"]
        .as_str()
        .expect("gateway ref")
        .to_string();
    let sub_a_id = order_a["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    // Signed with the literal secret that used to be the public
    // default: rejected 401, and NOTHING moves.
    let forged_sig = sign_dev_callback("dev-secret", &ref_a, 106_000, "success");
    let (status, body) = webhook(&app, "bkash", &ref_a, 106_000, "success", &forged_sig).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "forged webhook: {body}");
    let order = get_order(&app, &buyer, &order_a_id).await;
    assert_eq!(order["payment_status"], "pending", "forgery moved nothing: {order}");
    assert_eq!(payment_status_of(&pool, &payment_a_id).await, "pending");
    // The forged attempt is still in the audit log, marked invalid.
    let invalid_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM payment_events WHERE payment_id = $1 AND signature_valid = false",
    )
    .bind(Uuid::parse_str(&payment_a_id).expect("payment id is a uuid"))
    .fetch_one(&pool)
    .await
    .expect("count invalid events");
    assert!(invalid_events >= 1, "forged webhook is audit-logged");

    // Signed with the secret the app was configured with: settles.
    let good_sig = sign_dev_callback(TEST_SECRET, &ref_a, 106_000, "success");
    let (status, payment) = webhook(&app, "bkash", &ref_a, 106_000, "success", &good_sig).await;
    assert_eq!(status, StatusCode::OK, "real-secret webhook: {payment}");
    assert_eq!(payment["status"], "paid");
    let order = get_order(&app, &buyer, &order_a_id).await;
    assert_eq!(order["payment_status"], "paid");

    // ---------------------------------------------------------------
    // 2. DEV references are unverifiable with dev mode OFF (HI-50).
    // ---------------------------------------------------------------
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order_b) = checkout(&app, &buyer, "bkash", None, None).await;
    assert_eq!(status, StatusCode::CREATED, "bkash checkout B: {order_b}");
    let order_b_id = order_b["id"].as_str().expect("order id").to_string();
    let ref_b = order_b["payment"]["gateway_ref"]
        .as_str()
        .expect("gateway ref")
        .to_string();
    let amount_b = order_b["grand_total_poisha"].as_i64().expect("grand total");
    let sig_b = sign_dev_callback(TEST_SECRET, &ref_b, amount_b, "success");
    // The CORRECT secret, but presented to an app whose dev mode
    // is off: still a 401 — a DEV reference is not a real gateway
    // callback and must never settle outside the simulator.
    let (status, body) = webhook(&app_no_dev, "bkash", &ref_b, amount_b, "success", &sig_b).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "dev-off webhook: {body}");
    let order = get_order(&app, &buyer, &order_b_id).await;
    assert_eq!(order["payment_status"], "pending");
    // Sanity: the same callback settles on the dev-mode app, so
    // order B ends paid (later sections rely on no other state).
    let (status, payment) = webhook(&app, "bkash", &ref_b, amount_b, "success", &sig_b).await;
    assert_eq!(status, StatusCode::OK, "dev-on webhook: {payment}");
    assert_eq!(payment["status"], "paid");

    // ---------------------------------------------------------------
    // 3. OTP attempt cap (audit CR-01, full list): after 5 wrong
    //    codes the CORRECT code is rejected too.
    // ---------------------------------------------------------------
    let otp_phone = "01939000003";
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/auth/request-otp",
        Some(json!({ "phone": otp_phone })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "request-otp: {body}");
    let real_code = body["dev_otp"].as_str().expect("dev otp").to_string();
    let wrong_code = if real_code == "000000" { "111111" } else { "000000" };
    for attempt in 1..=5 {
        let (status, body) = call(
            &app,
            Method::POST,
            "/v1/auth/verify-otp",
            Some(json!({ "phone": otp_phone, "code": wrong_code })),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "wrong attempt {attempt}: {body}"
        );
    }
    // The cap is exhausted: even the correct code now gets the
    // cap error (400), not a login.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/auth/verify-otp",
        Some(json!({ "phone": otp_phone, "code": real_code })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "capped correct code: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("too many OTP attempts"),
        "cap message: {body}"
    );
    // A fresh code resets the phone: request + verify succeeds.
    let recovered = login(&app, otp_phone).await;
    assert!(!recovered.is_empty(), "fresh OTP logs in after the cap");

    // ---------------------------------------------------------------
    // 4. Payout race + approve-time re-check (audit CR-10).
    // ---------------------------------------------------------------
    let seller2 = login(&app, "01739000002").await;
    let store2 = create_store(&app, &seller2, "wave39-store-b").await;
    // 150_000 goods: the COD grand total (156_000) stays under
    // the 200_000 advance threshold, so this sale needs no
    // advance and delivers straight through.
    let product2 = create_product(&app, &seller2, &store2, "Wave39 Payout Product", 150_000, 10).await;
    let buyer2 = login(&app, "01939000002").await;

    let (status, account) = call(
        &app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "Wave39 Seller",
            "account_number": "01739000002"
        })),
        Some(&seller2),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "payout account: {account}");

    // One delivered COD sale: 150_000 goods at 8% earns 138_000.
    add_to_cart(&app, &buyer2, &product2, 1).await;
    let (status, delivered_order) = checkout(&app, &buyer2, "cod", None, None).await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {delivered_order}");
    let delivered_sub = delivered_order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(&app, &seller2, &delivered_sub, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let (status, shipment) = book_shipment(&app, &seller2, &delivered_sub).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(&app, Method::POST, &uri, None, Some(&seller2)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
    let balance = get_balance(&app, &seller2).await;
    assert_eq!(balance["available_poisha"], 138_000, "earnings: {balance}");

    // Two CONCURRENT requests of 100_000 against 138_000: the
    // balance covers exactly one. Exactly one may succeed.
    let payout_body = json!({ "amount_poisha": 100_000 });
    let first = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(payout_body.clone()),
        Some(&seller2),
    );
    let second = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(payout_body),
        Some(&seller2),
    );
    let ((status1, body1), (status2, body2)) = tokio::join!(first, second);
    let created_count = [status1, status2]
        .iter()
        .filter(|status| **status == StatusCode::CREATED)
        .count();
    let rejected_count = [status1, status2]
        .iter()
        .filter(|status| **status == StatusCode::BAD_REQUEST)
        .count();
    assert_eq!(
        (created_count, rejected_count),
        (1, 1),
        "exactly one concurrent payout wins: {body1} / {body2}"
    );
    let winning = if status1 == StatusCode::CREATED { &body1 } else { &body2 };
    let payout_id = winning["id"].as_str().expect("payout id").to_string();
    let balance = get_balance(&app, &seller2).await;
    assert_eq!(balance["pending_poisha"], 100_000, "one pending: {balance}");
    assert_eq!(balance["available_poisha"], 38_000, "no double-spend: {balance}");

    // Approve-time re-check: simulate the store's earnings
    // dropping AFTER the request (the delivered sub-order's
    // payout snapshot is corrected downwards, as a return or an
    // admin correction would do). The pending 100_000 is now
    // uncovered, so approval must be refused with a 409.
    let admin = login(&app, "01739000009").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801739000009'")
        .execute(&pool)
        .await
        .expect("promote admin");
    sqlx::query("UPDATE sub_orders SET seller_payout_poisha = 50_000 WHERE id = $1")
        .bind(Uuid::parse_str(&delivered_sub).expect("sub-order id is a uuid"))
        .execute(&pool)
        .await
        .expect("simulate earnings drop");
    let approve_uri = format!("/v1/admin/payouts/{payout_id}/approve");
    let (status, body) = call(&app, Method::POST, &approve_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::CONFLICT, "approve uncovered payout: {body}");
    // Restore the earnings: the same approval now succeeds.
    sqlx::query("UPDATE sub_orders SET seller_payout_poisha = 138_000 WHERE id = $1")
        .bind(Uuid::parse_str(&delivered_sub).expect("sub-order id is a uuid"))
        .execute(&pool)
        .await
        .expect("restore earnings");
    let (status, body) = call(&app, Method::POST, &approve_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "approve covered payout: {body}");
    assert_eq!(body["status"], "approved");

    // ---------------------------------------------------------------
    // 5. Paid-order cancels are blocked (audit CR-07).
    // ---------------------------------------------------------------
    // Order A is fully PAID (section 1). Capture the product's
    // stock now (sections 1-2 each sold a unit): a blocked cancel
    // must restock NOTHING, so the figure must not move.
    let stock_before = product_stock(&pool, &product).await;
    let cancel_uri = format!("/v1/orders/{order_a_id}/cancel");
    let (status, body) = call(&app, Method::POST, &cancel_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::CONFLICT, "buyer cancel of paid order: {body}");
    assert_eq!(
        product_stock(&pool, &product).await,
        stock_before,
        "no restock on a blocked cancel"
    );
    let order = get_order(&app, &buyer, &order_a_id).await;
    assert_eq!(order["status"], "placed", "order untouched: {order}");

    // The seller cannot cancel the paid sub-order either.
    let (status, body) = set_sub_order_status(&app, &seller, &sub_a_id, "cancelled").await;
    assert_eq!(status, StatusCode::CONFLICT, "seller cancel of paid sub-order: {body}");
    assert_eq!(
        product_stock(&pool, &product).await,
        stock_before,
        "still no restock"
    );

    // An advance-PAID COD order blocks the buyer cancel too: the
    // advance money was collected online.
    add_to_cart(&app, &buyer, &pricey, 1).await;
    let (status, order_adv) = checkout(&app, &buyer, "cod", Some("bkash"), None).await;
    assert_eq!(status, StatusCode::CREATED, "advance checkout: {order_adv}");
    assert_eq!(order_adv["payment_status"], "advance_pending");
    assert_eq!(order_adv["advance_required_poisha"], 41_200);
    let order_adv_id = order_adv["id"].as_str().expect("order id").to_string();
    let ref_adv = order_adv["payment"]["gateway_ref"]
        .as_str()
        .expect("gateway ref")
        .to_string();
    let adv_sig = sign_dev_callback(TEST_SECRET, &ref_adv, 41_200, "success");
    let (status, payment) = webhook(&app, "bkash", &ref_adv, 41_200, "success", &adv_sig).await;
    assert_eq!(status, StatusCode::OK, "advance webhook: {payment}");
    let order = get_order(&app, &buyer, &order_adv_id).await;
    assert_eq!(order["payment_status"], "advance_paid");
    let pricey_stock = product_stock(&pool, &pricey).await;
    let cancel_uri = format!("/v1/orders/{order_adv_id}/cancel");
    let (status, body) = call(&app, Method::POST, &cancel_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::CONFLICT, "cancel of advance-paid order: {body}");
    assert_eq!(
        product_stock(&pool, &pricey).await,
        pricey_stock,
        "no restock on a blocked cancel"
    );

    // Regression anchor: a plain COD order (nothing collected)
    // still cancels cleanly and restocks.
    let stock_before_cod = product_stock(&pool, &product).await;
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order_cod) = checkout(&app, &buyer, "cod", None, None).await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {order_cod}");
    let order_cod_id = order_cod["id"].as_str().expect("order id").to_string();
    assert_eq!(product_stock(&pool, &product).await, stock_before_cod - 1);
    let cancel_uri = format!("/v1/orders/{order_cod_id}/cancel");
    let (status, body) = call(&app, Method::POST, &cancel_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "plain COD cancel: {body}");
    assert_eq!(body["status"], "cancelled");
    assert_eq!(
        product_stock(&pool, &product).await,
        stock_before_cod,
        "COD cancel restocks"
    );

    // ---------------------------------------------------------------
    // 6. Late webhook on a cancelled order (audit CR-08).
    // ---------------------------------------------------------------
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order_c) = checkout(&app, &buyer, "bkash", None, None).await;
    assert_eq!(status, StatusCode::CREATED, "bkash checkout C: {order_c}");
    let order_c_id = order_c["id"].as_str().expect("order id").to_string();
    let payment_c_id = order_c["payment"]["id"]
        .as_str()
        .expect("payment id")
        .to_string();
    let ref_c = order_c["payment"]["gateway_ref"]
        .as_str()
        .expect("gateway ref")
        .to_string();
    // Cancel while the payment is still pending: allowed (nothing
    // collected), and the cancel VOIDS the pending payment row.
    let cancel_uri = format!("/v1/orders/{order_c_id}/cancel");
    let (status, body) = call(&app, Method::POST, &cancel_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cancel pending online order: {body}");
    assert_eq!(
        payment_status_of(&pool, &payment_c_id).await,
        "failed",
        "the cancel voided the pending payment"
    );
    // The gateway's success callback arrives LATE, validly signed:
    // it is audit-logged and answered, but it settles NOTHING.
    let late_sig = sign_dev_callback(TEST_SECRET, &ref_c, 106_000, "success");
    let (status, payment) = webhook(&app, "bkash", &ref_c, 106_000, "success", &late_sig).await;
    assert_eq!(status, StatusCode::OK, "late webhook answered: {payment}");
    assert_eq!(payment["status"], "failed", "late webhook settled nothing: {payment}");
    let order = get_order(&app, &buyer, &order_c_id).await;
    assert_eq!(order["status"], "cancelled");
    assert_eq!(order["payment_status"], "pending", "order money state unmoved: {order}");
    assert_eq!(payment_status_of(&pool, &payment_c_id).await, "failed");

    // ---------------------------------------------------------------
    // 7. Coupon COD booking collects the receipt amount (CR-06).
    // ---------------------------------------------------------------
    let seller3 = login(&app, "01739000003").await;
    let store3 = create_store(&app, &seller3, "wave39-store-c").await;
    let product3 = create_product(&app, &seller3, &store3, "Wave39 Coupon Product", 100_000, 10).await;
    let buyer3 = login(&app, "01939000004").await;

    let (status, coupon) = call(
        &app,
        Method::POST,
        "/v1/seller/coupons",
        Some(json!({
            "code": "WAVE39SAVE",
            "discount_type": "fixed",
            "discount_value": 10_000
        })),
        Some(&seller3),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "store coupon: {coupon}");

    add_to_cart(&app, &buyer3, &product3, 1).await;
    let (status, order_d) = checkout(&app, &buyer3, "cod", None, Some("WAVE39SAVE")).await;
    assert_eq!(status, StatusCode::CREATED, "coupon checkout: {order_d}");
    // 100_000 goods − 10_000 coupon + 6_000 delivery = 96_000 due.
    assert_eq!(order_d["grand_total_poisha"], 96_000);
    let order_d_id = order_d["id"].as_str().expect("order id").to_string();
    let sub_d_id = order_d["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    let receipt_uri = format!("/v1/orders/{order_d_id}/receipt");
    let (status, receipt) = call(&app, Method::GET, &receipt_uri, None, Some(&buyer3)).await;
    assert_eq!(status, StatusCode::OK, "receipt: {receipt}");
    assert_eq!(receipt["cod_amount_due_poisha"], 96_000, "receipt due: {receipt}");

    let (status, body) = set_sub_order_status(&app, &seller3, &sub_d_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm coupon order: {body}");
    let (status, shipment) = book_shipment(&app, &seller3, &sub_d_id).await;
    assert_eq!(status, StatusCode::CREATED, "book coupon shipment: {shipment}");
    // Before the fix the courier was told to collect 106_000 —
    // the 10_000 coupon discount a second time, in cash.
    assert_eq!(
        shipment["cod_amount_poisha"], 96_000,
        "courier collects exactly the receipt amount: {shipment}"
    );
    assert_eq!(
        shipment["cod_amount_poisha"], receipt["cod_amount_due_poisha"],
        "booking and receipt agree"
    );
}
