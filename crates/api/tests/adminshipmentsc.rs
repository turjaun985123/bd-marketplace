//! Admin shipments remake, test C: the two stuck queues
//! are DIFFERENT facts and the surface keeps them apart.
//! UNREFRESHED = no status change in 15 minutes — the
//! exact population `/v1/admin/metrics` reports as
//! `stale_shipments` (pinned equal here). POLL OVERDUE =
//! no poll ATTEMPTED in 15 minutes (`last_polled_at`,
//! migration 0079): after a real poll, a shipment whose
//! status merely hasn't changed is unrefreshed but NOT
//! poll-overdue — the pin that separates a silent courier
//! from a dead worker. STATUS AGED = in one status past
//! its threshold (default 24h for `created`, overridable
//! through the `shipment_aged_*` platform settings);
//! manual shipments age but are never unrefreshed or
//! poll-overdue. An unchanged poll still stamps
//! `last_polled_at` — the stamp is how the next poll
//! knows an attempt happened. Helpers are copied from
//! the Wave 9 / admin orders tests on purpose.
//!
//! Gated on `DATABASE_URL`: without it the test returns
//! early. CI provides a Postgres 16 service.

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
    auth: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = auth {
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
async fn create_store(app: &Router, auth: &str, slug: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body["id"].as_str().expect("store id").to_string()
}

/// Create an active product; returns the product id.
async fn create_product(
    app: &Router,
    auth: &str,
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
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Add a product to the buyer's cart.
async fn add_to_cart(app: &Router, auth: &str, product_id: &str, quantity: i64) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_id, "quantity": quantity })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {body}");
}

/// Checkout the buyer's current cart with COD; returns the order.
async fn checkout_cod(app: &Router, auth: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01888888888",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {body}");
    body
}

/// Set a sub-order's seller-side status (the Wave 9 walk).
async fn set_sub_order_status(app: &Router, auth: &str, sub_order_id: &str, status: &str) {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/status");
    let (code, body) = call(
        app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": status })),
        Some(auth),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "set sub-order status {status}: {body}");
}

/// Book a shipment for a confirmed sub-order; returns the shipment.
async fn book_shipment(app: &Router, auth: &str, sub_order_id: &str, body: Value) -> Value {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(body), Some(auth)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    shipment
}

/// Refresh one shipment through the participant endpoint
/// (the shared core's seller/buyer door).
async fn refresh_participant(app: &Router, auth: &str, shipment_id: &str) -> Value {
    let uri = format!("/v1/shipments/{shipment_id}/refresh");
    let (status, body) = call(app, Method::POST, &uri, Some(json!({})), Some(auth)).await;
    assert_eq!(status, StatusCode::OK, "participant refresh: {body}");
    body
}

/// Buy one product and book its shipment; returns the shipment id.
async fn buy_and_book(
    app: &Router,
    buyer: &str,
    seller: &str,
    product_id: &str,
    booking: Value,
) -> String {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    let shipment = book_shipment(app, seller, &sub_order_id, booking).await;
    shipment["id"].as_str().expect("shipment id").to_string()
}

/// Age a shipment's booking clock, status clock and event
/// times together — "this shipment is old in every way".
async fn age_shipment_fully(pool: &PgPool, shipment_id: &str, interval: &str) {
    let sql = format!(
        "UPDATE shipments SET created_at = now() - interval '{interval}', \
         updated_at = now() - interval '{interval}' WHERE id = $1::uuid"
    );
    sqlx::query(&sql)
        .bind(shipment_id)
        .execute(pool)
        .await
        .expect("age shipment clocks");
    let sql = format!(
        "UPDATE shipment_events SET created_at = now() - interval '{interval}' \
         WHERE shipment_id = $1::uuid"
    );
    sqlx::query(&sql)
        .bind(shipment_id)
        .execute(pool)
        .await
        .expect("age shipment events");
}

/// Age ONLY a shipment's `updated_at` — its status looks
/// old while its poll stamp stays fresh.
async fn age_shipment_updated_only(pool: &PgPool, shipment_id: &str, interval: &str) {
    let sql = format!(
        "UPDATE shipments SET updated_at = now() - interval '{interval}' WHERE id = $1::uuid"
    );
    sqlx::query(&sql)
        .bind(shipment_id)
        .execute(pool)
        .await
        .expect("age shipment updated_at");
}

/// GET the admin shipment list with a query string.
async fn list_shipments(app: &Router, auth: &str, qs: &str) -> Value {
    let uri = format!("/v1/admin/shipments?{qs}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(auth)).await;
    assert_eq!(status, StatusCode::OK, "list {qs}: {body}");
    body
}

#[tokio::test]
async fn adminshipments_stuck_queues_are_distinct() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin shipments remake test C");
            return;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, return_requests, store_follows, \
         store_verification_events, store_verifications, platform_settings, \
         payouts, payout_accounts, review_replies, reviews, shipment_events, \
         shipments, payment_events, payments, order_items, sub_orders, orders, \
         cart_items, carts, products, api_keys, store_members, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "adminshipmentsc-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111113").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111113'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01722222225").await;
    let store = create_store(&app, &seller, "cship-alpha").await;
    let product = create_product(&app, &seller, &store, "Ship C", 150_000, 20).await;
    let buyer = login(&app, "01999999997").await;

    // ship1: a fresh DEV booking — in no queue.
    let _ship1 = buy_and_book(&app, &buyer, &seller, &product, json!({})).await;
    // ship2: a DEV booking aged an hour in every clock,
    // never polled.
    let ship2 = buy_and_book(&app, &buyer, &seller, &product, json!({})).await;
    age_shipment_fully(&pool, &ship2, "1 hour").await;

    // The unrefreshed queue IS the metrics population.
    let (status, metrics) = call(&app, Method::GET, "/v1/admin/metrics", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "metrics: {metrics}");
    assert_eq!(metrics["stale_shipments"], 1, "metrics stale: {metrics}");
    let list = list_shipments(&app, &admin, "").await;
    assert_eq!(list["summary"]["unrefreshed"], 1);
    assert_eq!(
        list["summary"]["unrefreshed"], metrics["stale_shipments"],
        "the queue and the overview count are one population"
    );
    assert_eq!(list["summary"]["poll_overdue"], 1);
    let queue = list_shipments(&app, &admin, "queue=unrefreshed").await;
    assert_eq!(queue["total"], 1);
    assert_eq!(queue["items"][0]["id"], ship2);
    let queue = list_shipments(&app, &admin, "queue=poll_overdue").await;
    assert_eq!(queue["total"], 1);
    assert_eq!(queue["items"][0]["id"], ship2);

    // Poll ship2 for real (it advances to picked_up and
    // stamps last_polled_at), then age ONLY its
    // updated_at: now it is unrefreshed (status old) but
    // NOT poll-overdue (an attempt just happened) — the
    // two queues separate.
    let refreshed = refresh_participant(&app, &seller, &ship2).await;
    assert_eq!(refreshed["status"], "picked_up");
    age_shipment_updated_only(&pool, &ship2, "1 hour").await;
    let list = list_shipments(&app, &admin, "").await;
    let row2 = list["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|row| row["id"] == ship2)
        .expect("ship2 row")
        .clone();
    assert_eq!(row2["unrefreshed"], true, "status is old: {row2}");
    assert_eq!(row2["poll_overdue"], false, "but a poll just happened: {row2}");
    assert!(!row2["last_polled_at"].is_null(), "poll stamped: {row2}");
    let queue = list_shipments(&app, &admin, "queue=poll_overdue").await;
    assert_eq!(queue["total"], 0, "nobody is poll-overdue now: {queue}");
    let queue = list_shipments(&app, &admin, "queue=unrefreshed").await;
    assert_eq!(queue["total"], 1);
    assert_eq!(queue["items"][0]["id"], ship2);

    // ship3: a manual booking aged three days — aged,
    // but never unrefreshed or poll-overdue (manual is
    // excluded from both poll queues by construction).
    let ship3 = buy_and_book(
        &app,
        &buyer,
        &seller,
        &product,
        json!({ "courier": "manual", "manual_courier": "sundarban" }),
    )
    .await;
    age_shipment_fully(&pool, &ship3, "3 days").await;
    let queue = list_shipments(&app, &admin, "queue=status_aged").await;
    assert_eq!(queue["total"], 1, "only the manual parcel is aged: {queue}");
    assert_eq!(queue["items"][0]["id"], ship3);
    let queue = list_shipments(&app, &admin, "queue=unrefreshed").await;
    assert_eq!(queue["total"], 1, "manual never joins unrefreshed: {queue}");
    assert_eq!(queue["items"][0]["id"], ship2);

    // ship4: a manual booking aged two hours — under the
    // default 24h created threshold it is NOT aged...
    let ship4 = buy_and_book(
        &app,
        &buyer,
        &seller,
        &product,
        json!({ "courier": "manual", "manual_courier": "karatoa" }),
    )
    .await;
    age_shipment_fully(&pool, &ship4, "2 hours").await;
    let queue = list_shipments(&app, &admin, "queue=status_aged").await;
    assert_eq!(queue["total"], 1, "two hours is under the default: {queue}");
    // ...until the platform setting lowers the bar to 1h.
    sqlx::query(
        "INSERT INTO platform_settings (key, value) \
         VALUES ('shipment_aged_created_hours', '1')",
    )
    .execute(&pool)
    .await
    .expect("set aged threshold");
    let queue = list_shipments(&app, &admin, "queue=status_aged").await;
    assert_eq!(queue["total"], 2, "the setting re-draws the queue: {queue}");
    assert_eq!(queue["summary"]["aged_thresholds"]["created"], 1);
    sqlx::query("DELETE FROM platform_settings WHERE key LIKE 'shipment_aged_%'")
        .execute(&pool)
        .await
        .expect("clean aged settings");

    // ship5: walked to delivered by participant polls,
    // then polled once more — an UNCHANGED poll, which
    // writes no event but DOES stamp last_polled_at.
    let ship5 = buy_and_book(&app, &buyer, &seller, &product, json!({})).await;
    for _ in 0..4 {
        refresh_participant(&app, &seller, &ship5).await;
    }
    age_shipment_updated_only(&pool, &ship5, "1 hour").await;
    let again = refresh_participant(&app, &seller, &ship5).await;
    assert_eq!(again["status"], "delivered");
    let detail_uri = format!("/v1/admin/shipments/{ship5}");
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "ship5 detail: {detail}");
    assert!(
        !detail["last_polled_at"].is_null(),
        "the unchanged poll stamped the attempt: {detail}"
    );
    assert_eq!(
        detail["events"].as_array().expect("events").len(),
        5,
        "but it wrote no sixth event: {detail}"
    );
    // Delivered is terminal: ship5 joins no stuck queue
    // even with an old updated_at.
    let queue = list_shipments(&app, &admin, "queue=unrefreshed").await;
    assert_eq!(queue["total"], 1);
    assert_eq!(queue["items"][0]["id"], ship2);
}
