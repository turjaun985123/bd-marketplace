//! Admin orders remake, test B: the queue FILTERS (parent
//! status, payment status, sub-order pipeline status, the
//! two advance queues) and the whole-queue SUMMARY — whose
//! advance populations and today figures must agree with
//! the admin OVERVIEW metrics exactly (the two surfaces
//! share definitions; this test pins them against each
//! other). Fixtures: a CR-15 degraded COD order, an
//! advance-pending COD order (payments DEV mode), and a
//! cancelled order. Helpers are copied from the Wave 9
//! test on purpose — integration test files stay
//! self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test returns
//! early, so a plain local `cargo test` (no database)
//! still passes. CI provides a Postgres 16 service and
//! sets `DATABASE_URL`.

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

/// Checkout the buyer's current cart with COD; when
/// `advance_method` is present the order starts an online
/// advance collection. Returns the order.
async fn checkout_cod(
    app: &Router,
    auth: &str,
    advance_method: Option<&str>,
) -> Value {
    let mut body = json!({
        "delivery_name": "Test Buyer",
        "delivery_phone": "01888888888",
        "delivery_address": "House 9, Road 3, Khulna",
        "delivery_zone": "inside_dhaka",
        "payment_method": "cod"
    });
    if let Some(method) = advance_method {
        body["advance_method"] = json!(method);
    }
    let (status, body) = call(app, Method::POST, "/v1/orders", Some(body), Some(auth)).await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {body}");
    body
}

/// GET the admin order list with a query string.
async fn list_orders(app: &Router, admin: &str, qs: &str) -> Value {
    let uri = format!("/v1/admin/orders?{qs}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(admin)).await;
    assert_eq!(status, StatusCode::OK, "list {qs}: {body}");
    body
}

#[tokio::test]
async fn adminorders_filters_and_summary_match_metrics() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin orders remake test B");
            return;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, return_requests, store_follows, \
         store_verification_events, store_verifications, platform_settings, \
         payouts, payout_accounts, review_replies, reviews, shipment_events, \
         shipments, payment_events, payments, order_items, sub_orders, orders, \
         cart_items, carts, products, api_keys, store_members, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    // Payments DEV mode so an advance collection can start.
    let state = AppState::new(Some(pool.clone()), "adminordersb-test-secret".to_string(), true)
        .with_payments_config(PaymentsConfig {
            dev_mode: true,
            dev_secret: "adminordersb-test-secret".to_string(),
            ..PaymentsConfig::default()
        })
        .with_couriers_config(CouriersConfig {
            dev_mode: true,
            ..CouriersConfig::default()
        });
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "aordersb-store").await;
    // One unit: goods 300_000 + 6_000 delivery = 306_000,
    // over the advance threshold; the advance is 20% =
    // 61_200.
    let product = create_product(&app, &seller, &store, "OrdersB Product", 300_000, 10).await;

    // Fixture 1: plain COD, no advance method — degraded.
    let buyer1 = login(&app, "01911111111").await;
    add_to_cart(&app, &buyer1, &product, 1).await;
    let degraded = checkout_cod(&app, &buyer1, None).await;
    let degraded_id = degraded["id"].as_str().expect("order id").to_string();
    assert_eq!(degraded["payment_status"], "pay_on_delivery");
    assert_eq!(degraded["advance_required_poisha"], 61_200);

    // Fixture 2: COD with an online advance — pending.
    let buyer2 = login(&app, "01922222222").await;
    add_to_cart(&app, &buyer2, &product, 1).await;
    let pending = checkout_cod(&app, &buyer2, Some("bkash")).await;
    let pending_id = pending["id"].as_str().expect("order id").to_string();
    assert_eq!(pending["payment_status"], "advance_pending");
    assert_eq!(pending["advance_required_poisha"], 61_200);

    // Fixture 3: placed then cancelled by its buyer.
    let buyer3 = login(&app, "01933333333").await;
    add_to_cart(&app, &buyer3, &product, 1).await;
    let placed = checkout_cod(&app, &buyer3, None).await;
    let cancelled_id = placed["id"].as_str().expect("order id").to_string();
    let cancel_uri = format!("/v1/orders/{cancelled_id}/cancel");
    let (status, body) = call(&app, Method::POST, &cancel_uri, None, Some(&buyer3)).await;
    assert_eq!(status, StatusCode::OK, "buyer cancel: {body}");
    assert_eq!(body["status"], "cancelled");

    // The summary over the whole queue.
    let list = list_orders(&app, &admin, "").await;
    assert_eq!(list["total"], 3);
    let summary = &list["summary"];
    assert_eq!(summary["total"], 3);
    assert_eq!(summary["placed"], 2);
    assert_eq!(summary["cancelled"], 1);
    assert_eq!(summary["payment_pay_on_delivery"], 2);
    assert_eq!(summary["payment_advance_pending"], 1);
    assert_eq!(summary["advance_pending_count"], 1);
    assert_eq!(summary["advance_outstanding_poisha"], 61_200);
    // The cancelled order was degraded too, but the
    // unfulfilled population excludes cancelled orders.
    assert_eq!(summary["advance_unfulfilled_count"], 1);
    assert_eq!(summary["advance_unfulfilled_poisha"], 61_200);
    assert_eq!(summary["orders_today"], 3);
    // Two non-cancelled sub-orders at 306_000 each.
    assert_eq!(summary["gmv_today_poisha"], 612_000);

    // The summary and the overview metrics agree — the
    // cross-surface pin.
    let (status, metrics) = call(&app, Method::GET, "/v1/admin/overview", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "overview: {metrics}");
    assert_eq!(metrics["orders_today"]["count"], summary["orders_today"]);
    assert_eq!(metrics["orders_today"]["gmv_poisha"], summary["gmv_today_poisha"]);
    assert_eq!(metrics["advance_pending"]["count"], summary["advance_pending_count"]);
    assert_eq!(
        metrics["advance_pending"]["outstanding_poisha"],
        summary["advance_outstanding_poisha"]
    );
    assert_eq!(
        metrics["advance_unfulfilled"]["count"],
        summary["advance_unfulfilled_count"]
    );
    assert_eq!(
        metrics["advance_unfulfilled"]["required_poisha"],
        summary["advance_unfulfilled_poisha"]
    );

    // Filters.
    let cancelled = list_orders(&app, &admin, "status=cancelled").await;
    assert_eq!(cancelled["total"], 1);
    assert_eq!(cancelled["items"][0]["id"], cancelled_id);
    // The summary stays whole-queue under a filter.
    assert_eq!(cancelled["summary"]["total"], 3);

    let by_payment = list_orders(&app, &admin, "payment_status=advance_pending").await;
    assert_eq!(by_payment["total"], 1);
    assert_eq!(by_payment["items"][0]["id"], pending_id);

    let advance_queue = list_orders(&app, &admin, "advance_state=pending").await;
    assert_eq!(advance_queue["total"], 1);
    assert_eq!(advance_queue["items"][0]["id"], pending_id);

    let unfulfilled = list_orders(&app, &admin, "advance_state=unfulfilled").await;
    assert_eq!(unfulfilled["total"], 1);
    assert_eq!(unfulfilled["items"][0]["id"], degraded_id);
    assert_eq!(unfulfilled["items"][0]["advance_unfulfilled"], true);

    let by_sub = list_orders(&app, &admin, "sub_status=placed").await;
    assert_eq!(by_sub["total"], 2);
    let by_sub_cancelled = list_orders(&app, &admin, "sub_status=cancelled").await;
    assert_eq!(by_sub_cancelled["total"], 1);
    assert_eq!(by_sub_cancelled["items"][0]["id"], cancelled_id);

    let by_store = list_orders(&app, &admin, &format!("store_id={store}")).await;
    assert_eq!(by_store["total"], 3);
    let other_store = list_orders(
        &app,
        &admin,
        "store_id=00000000-0000-0000-0000-000000000000",
    )
    .await;
    assert_eq!(other_store["total"], 0);
}
