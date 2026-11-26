//! Wave 30 integration test: automatic courier status polling.
//! The worker's poll service (`refresh_stale_shipments`, the
//! same service layer the worker binary calls through the api
//! library) advances stale API-courier shipments through the
//! ONE shared refresh function the seller/buyer endpoint uses:
//! a dev-mode shipment walks created -> picked_up -> in_transit
//! -> out_for_delivery -> delivered one step per poll, the
//! sub-order syncs to delivered and the Wave 29 delivered
//! notification is queued. Staleness is respected (a fresh
//! shipment is not polled at a 15-minute threshold, a backdated
//! one is), and the exclusions hold: terminal shipments
//! (delivered) and manual shipments are never polled, however
//! old they are.
//!
//! Shared-database discipline: CI runs every suite sequentially
//! against one Postgres. This suite truncates first (the
//! established pattern), then runs ONE baseline poll at a zero
//! staleness threshold to drain anything a previous suite could
//! have left non-terminal and stale, and only then creates its
//! fixtures — so the precise `PollReport` counts asserted in
//! the exclusion phase count this suite's shipments alone.
//! Where a count could still legitimately include a foreign
//! row, the assertions use `>=` and pin the exact behaviour on
//! this suite's own shipments' stored statuses.
//!
//! The small helpers are copied from the Wave 9 test on
//! purpose, since integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.
//! CI provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job.

use api::shipments::refresh_stale_shipments;
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

/// Buy one unit of the product and confirm the sub-order;
/// returns the sub-order id.
async fn buy_and_confirm(
    app: &Router,
    buyer: &str,
    seller: &str,
    product_id: &str,
) -> String {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    sub_order_id
}

/// Book the DEV courier for a confirmed sub-order (empty JSON
/// body, the Wave 6 flow); returns the shipment id.
async fn book_dev_shipment(app: &Router, seller: &str, sub_order_id: &str) -> String {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(json!({})), Some(seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    assert_eq!(shipment["status"], "created", "shipment: {shipment}");
    shipment["id"].as_str().expect("shipment id").to_string()
}

/// Book a MANUAL courier (Wave 19) for a confirmed sub-order;
/// returns the shipment id.
async fn book_manual_shipment(app: &Router, seller: &str, sub_order_id: &str) -> String {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(
        app,
        Method::POST,
        &uri,
        Some(json!({
            "courier": "manual",
            "manual_courier": "sundarban",
            "manual_tracking_id": "SB-WAVE30"
        })),
        Some(seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "book manual shipment: {shipment}");
    assert_eq!(shipment["courier"], "manual", "shipment: {shipment}");
    shipment["id"].as_str().expect("shipment id").to_string()
}

/// Refresh one shipment through the seller endpoint (the
/// participant path — the poller must agree with it).
async fn refresh_via_endpoint(app: &Router, seller: &str, shipment_id: &str) -> Value {
    let uri = format!("/v1/shipments/{shipment_id}/refresh");
    let (status, body) = call(app, Method::POST, &uri, None, Some(seller)).await;
    assert_eq!(status, StatusCode::OK, "refresh {shipment_id}: {body}");
    body
}

/// The stored status of one shipment, read straight from the
/// database (the poller writes no HTTP response to assert on).
async fn shipment_status(pool: &PgPool, shipment_id: &str) -> String {
    sqlx::query_scalar("SELECT status FROM shipments WHERE id = $1::uuid")
        .bind(shipment_id)
        .fetch_one(pool)
        .await
        .expect("load shipment status")
}

/// The stored status of one sub-order.
async fn sub_order_status(pool: &PgPool, sub_order_id: &str) -> String {
    sqlx::query_scalar("SELECT status FROM sub_orders WHERE id = $1::uuid")
        .bind(sub_order_id)
        .fetch_one(pool)
        .await
        .expect("load sub-order status")
}

/// Age one shipment's `updated_at` by the given interval, so
/// the staleness threshold sees it as old.
async fn backdate_shipment(pool: &PgPool, shipment_id: &str, interval: &str) {
    let sql = format!(
        "UPDATE shipments SET updated_at = now() - interval '{interval}' WHERE id = $1::uuid"
    );
    sqlx::query(&sql)
        .bind(shipment_id)
        .execute(pool)
        .await
        .expect("backdate shipment");
}

/// How many outbox rows exist for one notification kind.
async fn count_kind(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE kind = $1")
        .bind(kind)
        .fetch_one(pool)
        .await
        .expect("count notifications by kind")
}

#[tokio::test]
async fn wave30_automatic_courier_polling_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 30 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode (the DEV
    // courier simulator progresses one step per poll) on. The
    // poller is called on a clone of the very state the router
    // serves — the same construction the worker uses.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave30-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let poll_state = state.clone();
    let app = build_app(state);

    // Baseline poll: drain anything a previous suite left
    // non-terminal and stale, so the counts below describe
    // this suite's fixtures (see the module doc).
    let baseline = refresh_stale_shipments(&poll_state, 0, 50)
        .await
        .expect("baseline poll");
    assert_eq!(
        baseline.checked,
        baseline.advanced + baseline.unchanged + baseline.failed,
        "report invariant: {baseline:?}"
    );

    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave30-store").await;
    let product = create_product(&app, &seller, &store, "Wave30 Product", 200_000, 10).await;
    let buyer = login(&app, "01999999999").await;

    // 1. Poll-driven progression: a zero staleness threshold
    // makes the fresh booking immediately stale, and each poll
    // advances the DEV courier exactly one step until the
    // shipment — and, through the shared sync, the sub-order —
    // is delivered and the Wave 29 notification is queued.
    let sub1 = buy_and_confirm(&app, &buyer, &seller, &product).await;
    let ship1 = book_dev_shipment(&app, &seller, &sub1).await;
    for expected in ["picked_up", "in_transit", "out_for_delivery", "delivered"] {
        let report = refresh_stale_shipments(&poll_state, 0, 50)
            .await
            .expect("poll");
        assert!(report.checked >= 1, "poll report: {report:?}");
        assert!(report.advanced >= 1, "poll report: {report:?}");
        assert_eq!(report.failed, 0, "poll report: {report:?}");
        assert_eq!(
            shipment_status(&pool, &ship1).await,
            expected,
            "after poll expecting {expected}"
        );
    }
    assert_eq!(sub_order_status(&pool, &sub1).await, "delivered");
    assert_eq!(count_kind(&pool, "sub_order_delivered").await, 1);

    // A further zero-threshold poll cannot re-deliver it:
    // delivered is terminal for the poller, and the sync's
    // guard means no duplicate notification either.
    let report = refresh_stale_shipments(&poll_state, 0, 50)
        .await
        .expect("poll after delivery");
    assert_eq!(report.checked, 0, "terminal excluded: {report:?}");
    assert_eq!(count_kind(&pool, "sub_order_delivered").await, 1);

    // 2. Staleness window: a just-booked shipment is NOT stale
    // at 15 minutes; backdated by an hour it is, and one poll
    // advances it one step.
    let sub2 = buy_and_confirm(&app, &buyer, &seller, &product).await;
    let ship2 = book_dev_shipment(&app, &seller, &sub2).await;
    let report = refresh_stale_shipments(&poll_state, 15, 50)
        .await
        .expect("poll fresh");
    assert_eq!(report.checked, 0, "fresh shipment not stale: {report:?}");
    assert_eq!(shipment_status(&pool, &ship2).await, "created");

    backdate_shipment(&pool, &ship2, "1 hour").await;
    let report = refresh_stale_shipments(&poll_state, 15, 50)
        .await
        .expect("poll backdated");
    assert_eq!(report.checked, 1, "one stale shipment: {report:?}");
    assert_eq!(report.advanced, 1, "one stale shipment: {report:?}");
    assert_eq!(shipment_status(&pool, &ship2).await, "picked_up");

    // 3. Exclusions, counted exactly: age the picked_up
    // shipment again (the positive control — the ONLY row the
    // next poll may touch), plus a delivered shipment and a
    // manual shipment, both backdated two hours. The poll must
    // check exactly one shipment.
    backdate_shipment(&pool, &ship2, "1 hour").await;

    let sub3 = buy_and_confirm(&app, &buyer, &seller, &product).await;
    let ship3 = book_dev_shipment(&app, &seller, &sub3).await;
    for _ in 0..4 {
        refresh_via_endpoint(&app, &seller, &ship3).await;
    }
    assert_eq!(shipment_status(&pool, &ship3).await, "delivered");
    backdate_shipment(&pool, &ship3, "2 hours").await;

    let sub4 = buy_and_confirm(&app, &buyer, &seller, &product).await;
    let ship4 = book_manual_shipment(&app, &seller, &sub4).await;
    backdate_shipment(&pool, &ship4, "2 hours").await;

    let report = refresh_stale_shipments(&poll_state, 15, 50)
        .await
        .expect("poll exclusions");
    assert_eq!(report.checked, 1, "only the stale API shipment: {report:?}");
    assert_eq!(report.advanced, 1, "only the stale API shipment: {report:?}");
    assert_eq!(report.failed, 0, "poll report: {report:?}");
    assert_eq!(shipment_status(&pool, &ship2).await, "in_transit");
    assert_eq!(shipment_status(&pool, &ship3).await, "delivered");
    assert_eq!(shipment_status(&pool, &ship4).await, "created");
    assert_eq!(sub_order_status(&pool, &sub4).await, "shipped");
}
