//! Wave 28 integration test: security hardening + the admin
//! audit log — every response carries the security header set;
//! a request body over the 1 MiB cap is a 413; OTP requests
//! are throttled per phone at the domain hourly cap (counted
//! from the append-only request log) with a 429 past it, while
//! a different number is unaffected; and the consequential
//! admin actions (payout approval, store suspension, a
//! settings update) each leave exactly one audit row, written
//! in the action's own transaction, readable only by admins,
//! with the acting admin shown as a masked phone.
//!
//! All phone numbers are FRESH to this wave: the OTP throttle
//! counts requests per phone across the whole shared test
//! database, so reusing another suite's numbers could trip
//! (or be tripped by) their logins.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.
//! CI provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
    Router,
};
use courier::CouriersConfig;
use domain::OTP_REQUEST_LIMIT_PER_HOUR;
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

/// Send one request and return (status, headers, raw body
/// text) — for the assertions that are not about a JSON body:
/// the Wave 28 security headers, and the 413 body-limit
/// response (whose body is plain text, not JSON).
async fn call_raw(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
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
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
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
            "delivery_phone": "01928282828",
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

/// Buy one unit of the product and drive the order all the way to
/// delivered through the DEV courier; returns the order id.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
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
}

/// GET the seller balance for a token.
async fn get_balance(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/seller/balance", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "balance: {body}");
    body
}

/// GET the audit log (optionally filtered to one action) as an
/// admin; asserts 200 and returns the list body.
async fn get_audit_log(app: &Router, token: &str, action: Option<&str>) -> Value {
    let uri = match action {
        Some(action) => format!("/v1/admin/audit-log?action={action}"),
        None => "/v1/admin/audit-log".to_string(),
    };
    let (status, body) = call(app, Method::GET, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "audit log {uri}: {body}");
    body
}

#[tokio::test]
async fn wave28_security_hardening_and_audit_log_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 28 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE admin_audit_log, otp_request_log, promotions, seller_charges, \
         product_questions, wishlist_items, stock_adjustments, invoices, \
         store_invoice_counters, store_verifications, return_requests, \
         sidra_suggestions, messages, conversations, coupon_redemptions, \
         coupons, platform_settings, payouts, payout_accounts, review_replies, \
         reviews, shipment_events, shipments, payment_events, payments, \
         order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode (deliveries) on.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave28-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // 1. Security headers ride on every response — checked on
    // the liveness probe, which needs no auth and no database.
    let (status, headers, _body) = call_raw(&app, Method::GET, "/health", None, None).await;
    assert_eq!(status, StatusCode::OK, "health");
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_string()
    };
    assert_eq!(header("x-content-type-options"), "nosniff");
    assert_eq!(header("x-frame-options"), "DENY");
    assert_eq!(header("referrer-policy"), "no-referrer");
    assert_eq!(
        header("content-security-policy"),
        "default-src 'none'; frame-ancestors 'none'"
    );
    assert_eq!(header("strict-transport-security"), "max-age=31536000");

    // 2. A JSON body over the 1 MiB cap is refused with 413
    // before any handler runs.
    let oversized = json!({
        "phone": "01328282828",
        "padding": "x".repeat(1_100_000)
    });
    let (status, _headers, _body) = call_raw(
        &app,
        Method::POST,
        "/v1/auth/request-otp",
        Some(oversized),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "oversized body");

    // 3. OTP throttle: a fresh number may request up to the
    // hourly cap; the next request is a 429. The cap is
    // per-phone — a different fresh number still gets a code.
    for attempt in 0..OTP_REQUEST_LIMIT_PER_HOUR {
        let (status, body) = call(
            &app,
            Method::POST,
            "/v1/auth/request-otp",
            Some(json!({ "phone": "01328282828" })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "request {attempt}: {body}");
    }
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/auth/request-otp",
        Some(json!({ "phone": "01328282828" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "over the cap: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("too many OTP requests"),
        "throttle message: {body}"
    );
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/auth/request-otp",
        Some(json!({ "phone": "01528282828" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "other number unaffected: {body}");

    // 4. The audit flow. The admin is promoted by SQL (there
    // is deliberately no endpoint for the first admin).
    let admin = login(&app, "01428282828").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801428282828'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // A seller earns 184_000 from one delivered 200_000 sale
    // (the Wave 9 numbers) and requests a 100_000 payout.
    let seller = login(&app, "01628282828").await;
    let store = create_store(&app, &seller, "wave28-store").await;
    let product = create_product(&app, &seller, &store, "Wave28 Product", 200_000, 10).await;
    let buyer = login(&app, "01928282828").await;
    buy_and_deliver(&app, &buyer, &seller, &product).await;
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["available_poisha"], 184_000, "balance: {balance}");

    let (status, account) = call(
        &app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "Wave28 Seller",
            "account_number": "01628282828"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save payout account: {account}");

    let (status, payout) = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": 100_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "request payout: {payout}");
    let payout_id = payout["id"].as_str().expect("payout id").to_string();

    // The audit log is admin-only: the seller gets a 403.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/audit-log",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "audit log as seller: {body}");

    // Admin approves the payout; exactly one payout.approve
    // row appears, pointing at the payout, with the admin
    // identified by masked phone only.
    let uri = format!("/v1/admin/payouts/{payout_id}/approve");
    let (status, approved) = call(&app, Method::POST, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "approve payout: {approved}");
    assert_eq!(approved["status"], "approved");

    let log = get_audit_log(&app, &admin, Some("payout.approve")).await;
    assert_eq!(log["total"], 1, "approve rows: {log}");
    let entry = &log["items"][0];
    assert_eq!(entry["action"], "payout.approve");
    assert_eq!(entry["target_type"], "payout");
    assert_eq!(entry["target_id"], payout_id);
    assert_eq!(entry["detail"]["from"], "requested");
    assert_eq!(entry["detail"]["to"], "approved");
    assert_eq!(
        entry["admin_phone_masked"],
        domain::mask_phone("+8801428282828")
    );
    assert!(
        !log.to_string().contains("+8801428282828"),
        "full admin phone leaked into the audit response: {log}"
    );

    // Admin suspends the store, then reactivates it: one row
    // each, in the log.
    let uri = format!("/v1/admin/stores/{store}/status");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "suspended" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suspend store: {body}");
    let log = get_audit_log(&app, &admin, Some("store.suspend")).await;
    assert_eq!(log["total"], 1, "suspend rows: {log}");
    assert_eq!(log["items"][0]["target_id"], store);

    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "active" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reactivate store: {body}");

    // Admin updates the commission setting: the audit detail
    // names the changed KEY and never the value. The settings
    // row is deleted afterwards so no other suite inherits a
    // 900-bps platform.
    let (status, settings) = call(
        &app,
        Method::PUT,
        "/v1/admin/settings",
        Some(json!({ "commission_bps": 900 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update settings: {settings}");
    assert_eq!(settings["commission_bps"], 900);

    let log = get_audit_log(&app, &admin, Some("settings.update")).await;
    assert_eq!(log["total"], 1, "settings rows: {log}");
    let changed = log["items"][0]["detail"]["changed_keys"]
        .as_array()
        .expect("changed keys list")
        .clone();
    assert_eq!(changed, vec![json!("commission_bps")], "changed keys: {log}");
    assert!(
        !log["items"][0]["detail"].to_string().contains("900"),
        "setting value leaked into the audit detail: {log}"
    );
    sqlx::query("DELETE FROM platform_settings WHERE key = 'commission_bps'")
        .execute(&pool)
        .await
        .expect("clean up the settings row");

    // The unfiltered log: pagination shape, all four actions
    // present, newest first (the settings update was last).
    let log = get_audit_log(&app, &admin, None).await;
    assert_eq!(log["page"], 1, "log page: {log}");
    assert_eq!(log["total"], 4, "all rows: {log}");
    assert_eq!(log["items"].as_array().expect("items").len(), 4);
    assert_eq!(log["items"][0]["action"], "settings.update");
}
