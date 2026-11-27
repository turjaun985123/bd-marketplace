//! Wave 43 integration test: observability — every response
//! carries an `X-Request-Id` (a valid inbound id is honoured
//! and echoed; an oversized or non-ASCII one is replaced by
//! a minted id), and `GET /v1/admin/metrics` is admin-only
//! and reports the point-in-time snapshot: after the
//! standard two-store COD checkout, `orders_today` shows
//! exactly 1 order whose GMV — by the Wave 27 definition,
//! SUM(goods − discount + delivery) over the day's
//! non-cancelled sub-orders — is exactly 457_000 poisha
//! (245_000 + 200_000 goods, 2 × 6_000 inside-Dhaka
//! delivery, no coupon), `orders_by_status` shows the one
//! `placed` parent order, and the notification backlog
//! shows exactly 1 pending row (the one order-placed
//! notice checkout enqueues per order).
//!
//! The small helpers are copied from the Wave 9 test on
//! purpose, since integration test files stay
//! self-contained; the one addition is
//! `call_with_request_id`, because the shared `call`
//! helper intentionally hides response headers and cannot
//! send a raw header value.
//!
//! Gated on `DATABASE_URL`: without it the test skips
//! cleanly so a plain local `cargo test` (no database)
//! still passes. CI provides a Postgres 16 service and
//! sets `DATABASE_URL` for the rust job.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
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

/// Send one bodyless request carrying a raw `X-Request-Id`
/// header value (or none) and return (status, headers, body)
/// — the Wave 43 assertions live in the response headers,
/// which `call` does not expose.
async fn call_with_request_id(
    app: &Router,
    uri: &str,
    request_id: Option<HeaderValue>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    if let Some(value) = request_id {
        builder = builder.header("x-request-id", value);
    }
    let request = builder.body(Body::empty()).expect("request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response is JSON")
    };
    (status, headers, parsed)
}

/// The `x-request-id` response header as a string, if present.
fn echoed_request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
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

/// GET the admin metrics snapshot as the given token.
async fn get_metrics(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/admin/metrics", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "admin metrics: {body}");
    body
}

#[tokio::test]
async fn wave43_observability_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 43 integration test");
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
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_variants, products, store_members, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode on (OTP login); payments stay unconfigured,
    // so the big COD checkout below takes the Wave 17 degraded
    // path (advance recorded, plain COD) exactly like Wave 40.
    let state = AppState::new(Some(pool.clone()), "wave43-test-secret".to_string(), true);
    let app = build_app(state);

    // 1. Request ids on a plain health check: no inbound id ->
    // a minted one is echoed (32 hex chars, the UUID simple
    // form); a valid inbound id is honoured verbatim.
    let (status, headers, _) = call_with_request_id(&app, "/health", None).await;
    assert_eq!(status, StatusCode::OK, "health");
    let minted = echoed_request_id(&headers).expect("health carries a request id");
    assert_eq!(minted.len(), 32, "minted id is a simple UUID: {minted}");
    assert!(
        minted.chars().all(|c| c.is_ascii_hexdigit()),
        "minted id is hex: {minted}"
    );

    let (status, headers, _) = call_with_request_id(
        &app,
        "/health",
        Some(HeaderValue::from_static("wave43-test-request-1")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "health with inbound id");
    assert_eq!(
        echoed_request_id(&headers).as_deref(),
        Some("wave43-test-request-1"),
        "a valid inbound id is echoed unchanged"
    );

    // 2. An oversized inbound id (65 chars) is NOT honoured:
    // the response carries a different, minted id instead.
    let oversized = "a".repeat(65);
    let (status, headers, _) = call_with_request_id(
        &app,
        "/health",
        Some(
            HeaderValue::from_str(&oversized).expect("65 ASCII chars are a legal header value"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "health with oversized id");
    let replaced = echoed_request_id(&headers).expect("response still carries an id");
    assert_ne!(replaced, oversized, "the oversized id must not be echoed");
    assert_eq!(replaced.len(), 32, "the replacement is a minted id: {replaced}");

    // 3. A non-ASCII inbound id (legal header bytes, but not
    // printable ASCII) is likewise replaced, never echoed.
    let unicode = HeaderValue::from_bytes("wave43-ঢাকা".as_bytes())
        .expect("UTF-8 bytes are legal header bytes");
    let (status, headers, _) = call_with_request_id(&app, "/health", Some(unicode)).await;
    assert_eq!(status, StatusCode::OK, "health with unicode id");
    let replaced = echoed_request_id(&headers).expect("response still carries an id");
    assert_eq!(replaced.len(), 32, "the replacement is a minted id: {replaced}");

    // 4. The metrics endpoint's guard: anonymous -> 401, a
    // signed-in non-admin -> 403.
    let (status, body) = call(&app, Method::GET, "/v1/admin/metrics", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "metrics anonymous: {body}");

    let buyer = login(&app, "01999999999").await;
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/metrics",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "metrics non-admin: {body}");

    // 5. The admin (promoted by SQL, the only way) sees the
    // empty-platform snapshot: every block present, every
    // number zero except the two users who have logged in.
    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin by SQL");

    let metrics = get_metrics(&app, &admin).await;
    assert_eq!(metrics["orders_today"]["count"], 0);
    assert_eq!(metrics["orders_today"]["gmv_poisha"], 0);
    assert_eq!(metrics["orders_by_status"], json!({}));
    assert_eq!(metrics["pending_payouts"]["count"], 0);
    assert_eq!(metrics["pending_payouts"]["amount_poisha"], 0);
    assert_eq!(metrics["open_returns"], 0);
    assert_eq!(metrics["notification_backlog"]["pending"], 0);
    assert_eq!(metrics["notification_backlog"]["sending"], 0);
    assert_eq!(metrics["notification_backlog"]["sent"], 0);
    assert_eq!(metrics["notification_backlog"]["failed"], 0);
    assert_eq!(metrics["stale_shipments"], 0);
    assert_eq!(metrics["users_total"], 2);
    assert_eq!(metrics["stores_total"], 0);
    assert_eq!(metrics["products_active"], 0);

    // 6. The standard two-store COD cart: 245_000 + 200_000
    // goods, 2 x 6_000 inside-Dhaka delivery (1 kg default
    // per sub-order) — grand total 457_000.
    let seller_a = login(&app, "01722222222").await;
    let store_a = create_store(&app, &seller_a, "wave43-store-a").await;
    let product_a = create_product(&app, &seller_a, &store_a, "Wave43 Product A", 245_000, 10).await;
    let seller_b = login(&app, "01733333333").await;
    let store_b = create_store(&app, &seller_b, "wave43-store-b").await;
    let product_b = create_product(&app, &seller_b, &store_b, "Wave43 Product B", 200_000, 10).await;

    add_to_cart(&app, &buyer, &product_a, 1).await;
    add_to_cart(&app, &buyer, &product_b, 1).await;
    checkout_cod(&app, &buyer).await;

    // 7. The snapshot moves by exactly the order's footprint:
    // orders_today 1 / GMV 457_000 (the Wave 27 formula over
    // the day's non-cancelled sub-orders), one `placed`
    // parent, exactly one pending notification (the single
    // order-placed notice), and the catalogue totals.
    let metrics = get_metrics(&app, &admin).await;
    assert_eq!(metrics["orders_today"]["count"], 1, "metrics: {metrics}");
    assert_eq!(metrics["orders_today"]["gmv_poisha"], 457_000, "metrics: {metrics}");
    assert_eq!(metrics["orders_by_status"], json!({ "placed": 1 }));
    assert_eq!(metrics["notification_backlog"]["pending"], 1, "metrics: {metrics}");
    assert_eq!(metrics["notification_backlog"]["sending"], 0);
    assert_eq!(metrics["notification_backlog"]["failed"], 0);
    assert_eq!(metrics["pending_payouts"]["count"], 0);
    assert_eq!(metrics["open_returns"], 0);
    assert_eq!(metrics["stale_shipments"], 0);
    assert_eq!(metrics["users_total"], 4);
    assert_eq!(metrics["stores_total"], 2);
    assert_eq!(metrics["products_active"], 2);
}
