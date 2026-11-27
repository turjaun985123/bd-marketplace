//! Wave 41 integration tests: security hardening tranche 2 —
//! the audit's highest-value High findings, driven end-to-end
//! against the real router and the payment service functions.
//! One test function with numbered sections (the crate's canon
//! — the shared test database makes parallel test functions
//! unsafe):
//!
//! 1. HI-51, full payment: a payment row whose amount was
//!    tampered away from the order's grand total cannot settle
//!    — the correctly-signed callback for the TAMPERED amount
//!    (which passes the Wave 5 row-vs-callback check) is
//!    rejected 400 and nothing moves; the honest callback for
//!    the restored row settles.
//! 2. HI-51, COD advance: a drifted advance row is rejected
//!    the same way — a COD order can only ever settle its
//!    recognised advance, never fall through to a generic
//!    `paid`; the honest advance settles to `advance_paid`.
//! 3. HI-53: `insert_pending_payment` refuses a duplicate
//!    gateway reference with a typed 409, refuses a second
//!    pending attempt for one order, and accepts a new attempt
//!    once the previous one failed (migration 0036's two
//!    partial unique indexes).
//! 4. OTP request throttle (HI-04/F04 + HI-06/F06): with 17 of
//!    the hourly 20 requests already logged, SIX concurrent
//!    request-otp calls for the same phone produce exactly 3
//!    successes and 3 refusals, exactly 20 log rows, and
//!    exactly ONE live code — the count check, the log insert
//!    and the code replacement are one locked transaction.
//! 5. HI-52: on a two-store online-paid order, each admin
//!    refund-done folds its return's refund into the payment
//!    record in the same transaction — a partial refund
//!    accumulates on a still-`paid` payment, and the final one
//!    flips the payment AND the order to `refunded` with
//!    `refunded_at` stamped. A COD return's refund-done still
//!    works and touches no payment (there is none).
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.
//! CI provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job — and `ci_gate.rs` makes a CI run without
//! it fail loudly instead of skipping everything (audit CR-14).

use api::payments::insert_pending_payment;
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

/// The dev-payments secret THIS test configures on its app
/// state — chosen by the test, like wave5/wave17/wave39 do
/// with theirs.
const TEST_SECRET: &str = "wave41-test-dev-secret";

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
/// or an online method; `advance_method` is added to the body
/// when present.
async fn checkout(
    app: &Router,
    token: &str,
    payment_method: &str,
    advance_method: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = json!({
        "delivery_name": "Test Buyer",
        "delivery_phone": "01941000099",
        "delivery_address": "House 9, Road 3, Khulna",
        "delivery_zone": "inside_dhaka",
        "payment_method": payment_method
    });
    if let Some(method) = advance_method {
        body["advance_method"] = json!(method);
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

/// Drive one confirmed sub-order to delivered through the DEV
/// courier (confirm -> book -> four refresh polls).
async fn deliver_sub_order(app: &Router, seller: &str, sub_order_id: &str) {
    let (status, body) = set_sub_order_status(app, seller, sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm sub-order: {body}");
    let (status, shipment) = book_shipment(app, seller, sub_order_id).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, body) = call(app, Method::POST, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh shipment: {body}");
    }
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

/// Drive one return all the way: seller approves, seller marks
/// received, admin marks the refund done. Returns the received
/// view (with the refund snapshot).
async fn drive_return_to_refund_done(
    app: &Router,
    seller: &str,
    admin: &str,
    return_id: &str,
) -> Value {
    let uri = format!("/v1/seller/return-requests/{return_id}/approve");
    let (status, body) = call(
        app,
        Method::POST,
        &uri,
        Some(json!({})),
        Some(seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve return: {body}");
    let uri = format!("/v1/seller/return-requests/{return_id}/received");
    let (status, received) = call(app, Method::POST, &uri, None, Some(seller)).await;
    assert_eq!(status, StatusCode::OK, "receive return: {received}");
    assert_eq!(received["refund_status"], "pending");
    let uri = format!("/v1/admin/return-requests/{return_id}/refund-done");
    let (status, done) = call(app, Method::POST, &uri, None, Some(admin)).await;
    assert_eq!(status, StatusCode::OK, "refund done: {done}");
    assert_eq!(done["refund_status"], "done");
    received
}

/// A payment row's (status, refund_amount_poisha, refunded_at set?)
/// from the database (ground truth).
async fn payment_row(pool: &PgPool, payment_id: &str) -> (String, i64, bool) {
    sqlx::query_as(
        "SELECT status, refund_amount_poisha, refunded_at IS NOT NULL FROM payments WHERE id = $1",
    )
    .bind(Uuid::parse_str(payment_id).expect("payment id is a uuid"))
    .fetch_one(pool)
    .await
    .expect("payment row")
}

/// A payment row's status, from the database (ground truth).
async fn payment_status_of(pool: &PgPool, payment_id: &str) -> String {
    sqlx::query_scalar("SELECT status FROM payments WHERE id = $1")
        .bind(Uuid::parse_str(payment_id).expect("payment id is a uuid"))
        .fetch_one(pool)
        .await
        .expect("payment status")
}

/// Set a payment row's amount directly — the tamper the HI-51
/// sections defend against (a drifted row, however it got there).
async fn set_payment_amount(pool: &PgPool, payment_id: &str, amount_poisha: i64) {
    sqlx::query("UPDATE payments SET amount_poisha = $2 WHERE id = $1")
        .bind(Uuid::parse_str(payment_id).expect("payment id is a uuid"))
        .bind(amount_poisha)
        .execute(pool)
        .await
        .expect("tamper payment amount");
}

#[tokio::test]
async fn wave41_security_hardening_end_to_end() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 41 integration test");
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
    let state = AppState::new(Some(pool.clone()), "wave41-test-secret".to_string(), true)
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

    // Cast: two sellers, one buyer, one admin (promoted by SQL).
    let admin = login(&app, "01541000001").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801541000001'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01741000001").await;
    let store = create_store(&app, &seller, "wave41-store-a").await;
    let product = create_product(&app, &seller, &store, "Wave41 Product", 100_000, 20).await;
    let pricey = create_product(&app, &seller, &store, "Wave41 Pricey", 200_000, 10).await;
    let seller_b = login(&app, "01741000002").await;
    let store_b = create_store(&app, &seller_b, "wave41-store-b").await;
    let product_b = create_product(&app, &seller_b, &store_b, "Wave41 Product B", 200_000, 10).await;
    let buyer = login(&app, "01941000001").await;

    // ---------------------------------------------------------------
    // 1. HI-51: a tampered payment row cannot settle a full
    //    online payment — the order total is the expectation.
    // ---------------------------------------------------------------
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order_a) = checkout(&app, &buyer, "bkash", None).await;
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

    // The row drifts DOWN (a bug, a manual edit — however it
    // happened). A callback for the drifted amount is signed
    // correctly and matches the ROW, so the Wave 5 check passes
    // it; only the order-derived expectation can catch it.
    set_payment_amount(&pool, &payment_a_id, 90_000).await;
    let drifted_sig = sign_dev_callback(TEST_SECRET, &ref_a, 90_000, "success");
    let (status, body) = webhook(&app, "bkash", &ref_a, 90_000, "success", &drifted_sig).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "drifted callback: {body}");
    assert_eq!(payment_status_of(&pool, &payment_a_id).await, "pending");
    let order = get_order(&app, &buyer, &order_a_id).await;
    assert_eq!(order["payment_status"], "pending", "drift settled nothing: {order}");

    // Restore the row; the honest callback settles as before.
    set_payment_amount(&pool, &payment_a_id, 106_000).await;
    let good_sig = sign_dev_callback(TEST_SECRET, &ref_a, 106_000, "success");
    let (status, payment) = webhook(&app, "bkash", &ref_a, 106_000, "success", &good_sig).await;
    assert_eq!(status, StatusCode::OK, "honest webhook: {payment}");
    assert_eq!(payment["status"], "paid");
    let order = get_order(&app, &buyer, &order_a_id).await;
    assert_eq!(order["payment_status"], "paid");

    // ---------------------------------------------------------------
    // 2. HI-51: a COD order can only settle its recognised
    //    advance — a drifted advance row never becomes `paid`.
    // ---------------------------------------------------------------
    add_to_cart(&app, &buyer, &pricey, 1).await;
    let (status, order_adv) = checkout(&app, &buyer, "cod", Some("bkash")).await;
    assert_eq!(status, StatusCode::CREATED, "advance checkout: {order_adv}");
    assert_eq!(order_adv["payment_status"], "advance_pending");
    let advance_required = order_adv["advance_required_poisha"]
        .as_i64()
        .expect("advance required");
    assert_eq!(advance_required, 41_200);
    let order_adv_id = order_adv["id"].as_str().expect("order id").to_string();
    let payment_adv_id = order_adv["payment"]["id"]
        .as_str()
        .expect("payment id")
        .to_string();
    let ref_adv = order_adv["payment"]["gateway_ref"]
        .as_str()
        .expect("gateway ref")
        .to_string();
    let row_amount: i64 = sqlx::query_scalar("SELECT amount_poisha FROM payments WHERE id = $1")
        .bind(Uuid::parse_str(&payment_adv_id).expect("payment id is a uuid"))
        .fetch_one(&pool)
        .await
        .expect("advance row amount");
    assert_eq!(row_amount, advance_required, "advance row snapshots the advance");

    // Drift the advance row UP; the signed callback for the
    // drifted amount matches the row but not the order's
    // required advance — rejected, and the order does NOT fall
    // through to a generic `paid`.
    set_payment_amount(&pool, &payment_adv_id, advance_required + 10_000).await;
    let drifted_sig = sign_dev_callback(
        TEST_SECRET,
        &ref_adv,
        advance_required + 10_000,
        "success",
    );
    let (status, body) = webhook(
        &app,
        "bkash",
        &ref_adv,
        advance_required + 10_000,
        "success",
        &drifted_sig,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "drifted advance callback: {body}");
    assert_eq!(payment_status_of(&pool, &payment_adv_id).await, "pending");
    let order = get_order(&app, &buyer, &order_adv_id).await;
    assert_eq!(order["payment_status"], "advance_pending");
    assert_eq!(order["advance_paid_poisha"], 0);

    // Restore; the honest advance settles to `advance_paid`,
    // never `paid` (the balance is still COD).
    set_payment_amount(&pool, &payment_adv_id, advance_required).await;
    let good_sig = sign_dev_callback(TEST_SECRET, &ref_adv, advance_required, "success");
    let (status, payment) = webhook(
        &app,
        "bkash",
        &ref_adv,
        advance_required,
        "success",
        &good_sig,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "honest advance webhook: {payment}");
    assert_eq!(payment["status"], "paid");
    let order = get_order(&app, &buyer, &order_adv_id).await;
    assert_eq!(order["payment_status"], "advance_paid");
    assert_eq!(order["advance_paid_poisha"], advance_required);

    // ---------------------------------------------------------------
    // 3. HI-53: duplicate gateway references and duplicate
    //    pending attempts are typed conflicts, never rows.
    // ---------------------------------------------------------------
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order_cod) = checkout(&app, &buyer, "cod", None).await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {order_cod}");
    // Below the advance threshold the COD order carries NO
    // payment row — the clean order to insert attempts against.
    let cod_order_id = order_cod["id"].as_str().expect("order id").to_string();
    let cod_order_uuid = Uuid::parse_str(&cod_order_id).expect("order id is a uuid");
    let payment_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM payments WHERE order_id = $1")
            .bind(cod_order_uuid)
            .fetch_one(&pool)
            .await
            .expect("count cod payments");
    assert_eq!(payment_count, 0, "plain COD order has no payment row");

    let mut conn = pool.acquire().await.expect("acquire connection");
    // Reusing order A's reference (anywhere in the table) is a
    // conflict — the reference namespace is unique.
    let err = insert_pending_payment(&mut conn, cod_order_uuid, "bkash", 1_000, &ref_a)
        .await
        .expect_err("duplicate gateway_ref must be refused");
    assert_eq!(err.status, StatusCode::CONFLICT, "duplicate ref is a 409");
    // A fresh reference inserts fine.
    let inserted = insert_pending_payment(
        &mut conn,
        cod_order_uuid,
        "bkash",
        1_000,
        "WAVE41-UNIQUE-REF",
    )
    .await
    .expect("fresh reference inserts");
    // A second PENDING attempt for the same order — even with
    // a fresh reference — is a conflict: one settleable row
    // per order at a time.
    let err = insert_pending_payment(
        &mut conn,
        cod_order_uuid,
        "nagad",
        2_000,
        "WAVE41-OTHER-REF",
    )
    .await
    .expect_err("second pending attempt must be refused");
    assert_eq!(err.status, StatusCode::CONFLICT, "second pending is a 409");
    // Once that attempt has failed, the pending slot is free
    // again (the index is status-scoped, not order-scoped).
    sqlx::query("UPDATE payments SET status = 'failed' WHERE id = $1")
        .bind(inserted)
        .execute(&pool)
        .await
        .expect("fail the inserted attempt");
    insert_pending_payment(
        &mut conn,
        cod_order_uuid,
        "nagad",
        2_000,
        "WAVE41-OTHER-REF",
    )
    .await
    .expect("new attempt after failure inserts");
    drop(conn);

    // ---------------------------------------------------------------
    // 4. OTP request throttle: concurrent requests cannot burst
    //    past the hourly cap (HI-04/F04 + HI-06/F06).
    // ---------------------------------------------------------------
    let otp_phone_raw = "01741000009";
    let otp_phone = "+8801741000009";
    // 17 of the 20 hourly requests are already used up.
    sqlx::query("INSERT INTO otp_request_log (phone) SELECT $1 FROM generate_series(1, 17)")
        .bind(otp_phone)
        .execute(&pool)
        .await
        .expect("seed OTP request log");
    // Six requests race for the three remaining slots.
    let (r1, r2, r3, r4, r5, r6) = tokio::join!(
        call(
            &app,
            Method::POST,
            "/v1/auth/request-otp",
            Some(json!({ "phone": otp_phone_raw })),
            None
        ),
        call(
            &app,
            Method::POST,
            "/v1/auth/request-otp",
            Some(json!({ "phone": otp_phone_raw })),
            None
        ),
        call(
            &app,
            Method::POST,
            "/v1/auth/request-otp",
            Some(json!({ "phone": otp_phone_raw })),
            None
        ),
        call(
            &app,
            Method::POST,
            "/v1/auth/request-otp",
            Some(json!({ "phone": otp_phone_raw })),
            None
        ),
        call(
            &app,
            Method::POST,
            "/v1/auth/request-otp",
            Some(json!({ "phone": otp_phone_raw })),
            None
        ),
        call(
            &app,
            Method::POST,
            "/v1/auth/request-otp",
            Some(json!({ "phone": otp_phone_raw })),
            None
        ),
    );
    let results = [r1, r2, r3, r4, r5, r6];
    let succeeded = results
        .iter()
        .filter(|(status, _)| *status == StatusCode::OK)
        .count();
    let refused = results
        .iter()
        .filter(|(status, _)| *status == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!(succeeded, 3, "exactly the remaining slots succeed: {results:?}");
    assert_eq!(refused, 3, "the rest are refused 429: {results:?}");
    let log_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM otp_request_log WHERE phone = $1",
    )
    .bind(otp_phone)
    .fetch_one(&pool)
    .await
    .expect("count OTP log");
    assert_eq!(log_count, 20, "the cap is exact under concurrency");
    let code_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM otp_codes WHERE phone = $1")
            .bind(otp_phone)
            .fetch_one(&pool)
            .await
            .expect("count OTP codes");
    assert_eq!(code_count, 1, "exactly one live code survives the race");

    // ---------------------------------------------------------------
    // 5. HI-52: refund-done settles the payment record in the
    //    same transaction — partial, then complete.
    // ---------------------------------------------------------------
    add_to_cart(&app, &buyer, &product, 1).await;
    add_to_cart(&app, &buyer, &product_b, 1).await;
    let (status, order_r) = checkout(&app, &buyer, "bkash", None).await;
    assert_eq!(status, StatusCode::CREATED, "two-store checkout: {order_r}");
    assert_eq!(order_r["grand_total_poisha"], 312_000);
    let order_r_id = order_r["id"].as_str().expect("order id").to_string();
    let payment_r_id = order_r["payment"]["id"]
        .as_str()
        .expect("payment id")
        .to_string();
    let ref_r = order_r["payment"]["gateway_ref"]
        .as_str()
        .expect("gateway ref")
        .to_string();
    let sig_r = sign_dev_callback(TEST_SECRET, &ref_r, 312_000, "success");
    let (status, payment) = webhook(&app, "bkash", &ref_r, 312_000, "success", &sig_r).await;
    assert_eq!(status, StatusCode::OK, "settle two-store order: {payment}");
    assert_eq!(payment["status"], "paid");

    // Map the two sub-orders to their stores (response order is
    // not the contract; the store id is).
    let sub_rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, store_id FROM sub_orders WHERE order_id = $1",
    )
    .bind(Uuid::parse_str(&order_r_id).expect("order id is a uuid"))
    .fetch_all(&pool)
    .await
    .expect("sub-order rows");
    let store_a_uuid = Uuid::parse_str(&store).expect("store id is a uuid");
    let mut sub_a = String::new();
    let mut sub_b = String::new();
    for (sub_id, store_id) in &sub_rows {
        if *store_id == store_a_uuid {
            sub_a = sub_id.to_string();
        } else {
            sub_b = sub_id.to_string();
        }
    }
    assert!(!sub_a.is_empty() && !sub_b.is_empty(), "both sub-orders found");

    deliver_sub_order(&app, &seller, &sub_a).await;
    deliver_sub_order(&app, &seller_b, &sub_b).await;

    // Return A (100_000 of goods): refund-done accumulates the
    // partial amount; the payment stays `paid` — one sub-order
    // is still the buyer's.
    let (status, created) = request_return(&app, &buyer, &sub_a, "কাপড় ছেঁড়া ছিল").await;
    assert_eq!(status, StatusCode::CREATED, "return A: {created}");
    let return_a = created["id"].as_str().expect("return id").to_string();
    let received = drive_return_to_refund_done(&app, &seller, &admin, &return_a).await;
    assert_eq!(received["refund_poisha"], 100_000);
    let (status, refunded, refunded_at_set) = payment_row(&pool, &payment_r_id).await;
    assert_eq!(status, "paid", "partial refund keeps the payment paid");
    assert_eq!(refunded, 100_000, "partial refund accumulated");
    assert!(!refunded_at_set, "refunded_at only stamps at completion");
    let order = get_order(&app, &buyer, &order_r_id).await;
    assert_eq!(order["payment_status"], "paid");

    // Return B (200_000 of goods): the order is now fully
    // refunded — payment AND order flip to `refunded`, the
    // accumulated figure is the full goods value (the 12_000
    // delivery is never refunded, Wave 16).
    let (status, created) = request_return(&app, &buyer, &sub_b, "ভুল মডেল এসেছে").await;
    assert_eq!(status, StatusCode::CREATED, "return B: {created}");
    let return_b = created["id"].as_str().expect("return id").to_string();
    let received = drive_return_to_refund_done(&app, &seller_b, &admin, &return_b).await;
    assert_eq!(received["refund_poisha"], 200_000);
    let (status, refunded, refunded_at_set) = payment_row(&pool, &payment_r_id).await;
    assert_eq!(status, "refunded", "complete refund flips the payment");
    assert_eq!(refunded, 300_000, "both refunds accumulated");
    assert!(refunded_at_set, "refunded_at stamped at completion");
    let order = get_order(&app, &buyer, &order_r_id).await;
    assert_eq!(order["payment_status"], "refunded");

    // COD regression: a plain COD return's refund-done still
    // works end-to-end and touches no payment (none exists).
    // A FRESH buyer drives it: the main buyer now has two
    // returned sub-orders and no delivered ones, so the Wave 17
    // risk shield would force an advance onto their next COD
    // checkout — correct product behaviour, wrong fixture.
    let buyer2 = login(&app, "01941000002").await;
    add_to_cart(&app, &buyer2, &product, 1).await;
    let (status, order_cod2) = checkout(&app, &buyer2, "cod", None).await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout 2: {order_cod2}");
    let order_cod2_id = order_cod2["id"].as_str().expect("order id").to_string();
    let sub_cod2 = order_cod2["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    deliver_sub_order(&app, &seller, &sub_cod2).await;
    let (status, created) = request_return(&app, &buyer2, &sub_cod2, "মন বদলেছে").await;
    assert_eq!(status, StatusCode::CREATED, "cod return: {created}");
    let return_cod = created["id"].as_str().expect("return id").to_string();
    drive_return_to_refund_done(&app, &seller, &admin, &return_cod).await;
    let cod_payments: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM payments WHERE order_id = $1")
            .bind(Uuid::parse_str(&order_cod2_id).expect("order id is a uuid"))
            .fetch_one(&pool)
            .await
            .expect("count cod2 payments");
    assert_eq!(cod_payments, 0, "COD refund-done created no payment state");
}
