//! Admin products remake, test A: the surface exists and is
//! admin-only. Before the remake there was NO admin products
//! API at all — an admin could not see the catalogue
//! cross-store, and the only lever on a bad listing was
//! suspending the seller's whole store. This file pins the
//! gates (a non-admin gets 403 on list, detail and takedown;
//! an API key gets 403 on all of them — admin routes are
//! JWT-only by the Wave 37 central rule), the list envelope
//! with its whole-queue summary, and the detail's
//! composition (store summary, empty gallery/variants, the
//! sales block, empty audit history).
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job.

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

/// Create a product with an explicit status; returns the id.
async fn create_product_with_status(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
    status: &str,
) -> String {
    let (code, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": status
        })),
        Some(token),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Mint an API key with a JWT; returns the full key (shown once).
async fn create_key(app: &Router, jwt: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/api-keys",
        Some(json!({ "name": "products-test", "scopes": ["read", "write"], "expires_in_days": null })),
        Some(jwt),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create api key: {body}");
    body["key"].as_str().expect("full key once").to_string()
}

#[tokio::test]
async fn adminproducts_gates_envelope_and_detail() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin products remake test A");
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
    let state = AppState::new(
        Some(pool.clone()),
        "adminproducts-test-secret".to_string(),
        true,
    )
    .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // Login BEFORE the SQL admin promotion (the canon).
    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "aprod-alpha").await;
    let active = create_product_with_status(
        &app,
        &seller,
        &store,
        "Alpha Widget",
        150_000,
        10,
        "active",
    )
    .await;
    let draft = create_product_with_status(
        &app,
        &seller,
        &store,
        "Alpha Draft",
        90_000,
        4,
        "draft",
    )
    .await;

    // --- Gates: a non-admin is refused everywhere. ---
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/products",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller list: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/products/{active}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller detail: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/products/{active}/takedown"),
        Some(json!({ "note": "কারণ" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller takedown: {body}");

    // --- Gates: an API key is refused everywhere (JWT-only). ---
    let key = create_key(&app, &seller).await;
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/products",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "api-key list: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/products/{active}/takedown"),
        Some(json!({ "note": "কারণ" })),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "api-key takedown: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/products/{active}/restore"),
        Some(json!({ "note": "নোট" })),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "api-key restore: {body}");

    // --- The list envelope + whole-queue summary. ---
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/products",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin list: {list}");
    assert_eq!(list["total"], json!(2), "list: {list}");
    assert_eq!(list["summary"]["total"], json!(2), "list: {list}");
    assert_eq!(list["summary"]["active"], json!(1), "list: {list}");
    assert_eq!(list["summary"]["draft"], json!(1), "list: {list}");
    assert_eq!(list["summary"]["archived"], json!(0), "list: {list}");
    assert_eq!(list["summary"]["taken_down"], json!(0), "list: {list}");
    assert_eq!(list["summary"]["no_image"], json!(2), "list: {list}");
    let items = list["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2, "list: {list}");
    let alpha = items
        .iter()
        .find(|item| item["id"] == json!(active))
        .expect("active product listed");
    assert_eq!(alpha["store_name"], json!("Store aprod-alpha"));
    assert_eq!(alpha["store_status"], json!("active"));
    assert_eq!(alpha["status"], json!("active"));
    assert_eq!(alpha["source"], json!("manual"));
    assert_eq!(alpha["admin_hidden_at"], Value::Null);
    assert_eq!(alpha["admin_hidden_reason"], Value::Null);
    assert_eq!(alpha["sold_count"], json!(0));

    // --- The detail composes. ---
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/products/{draft}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin detail: {detail}");
    assert_eq!(detail["id"], json!(draft));
    assert_eq!(detail["status"], json!("draft"));
    assert_eq!(detail["store"]["slug"], json!("aprod-alpha"));
    assert_eq!(detail["store"]["status"], json!("active"));
    assert_eq!(detail["store"]["verified_tier"], json!("unverified"));
    assert_eq!(detail["images"], json!([]));
    assert_eq!(detail["variants"], json!([]));
    assert_eq!(detail["sales"]["goods_poisha"], json!(0));
    assert_eq!(detail["sales"]["units_ordered"], json!(0));
    assert_eq!(detail["audit_history"], json!([]));

    // Unknown product: 404.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/products/00000000-0000-0000-0000-000000000000",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown detail: {body}");
}
