//! Admin stores remake, test F: reactivation reverses
//! suspension completely — the note is cleared, the
//! `store_reactivated` notice fires once, the products are
//! visible again, and the cart that checkout refused while
//! suspended checks out. While suspended, the Wave 10
//! canon holds: product detail 404s, the listing empties,
//! checkout refuses with a 409 naming the suspension.
//! Helpers are copied from the Wave 9 test on purpose —
//! integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`.

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

/// Checkout the buyer's current cart with COD; asserts 201
/// and returns the order.
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

/// PATCH a store's status as the admin with a raw body.
async fn patch_status(
    app: &Router,
    admin: &str,
    store_id: &str,
    body: Value,
) -> (StatusCode, Value) {
    let uri = format!("/v1/admin/stores/{store_id}/status");
    call(app, Method::PATCH, &uri, Some(body), Some(admin)).await
}

/// Count notification rows of one kind.
async fn count_kind(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE kind = $1")
        .bind(kind)
        .fetch_one(pool)
        .await
        .expect("count notifications")
}

/// Count audit rows for one action + target.
async fn audit_count(pool: &PgPool, action: &str, target_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_audit_log WHERE action = $1 AND target_id = $2",
    )
    .bind(action)
    .bind(target_id)
    .fetch_one(pool)
    .await
    .expect("count audit rows")
}

#[tokio::test]
async fn adminstores_reactivate_restores_visibility_and_checkout() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin stores remake test F");
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
    let state = AppState::new(Some(pool.clone()), "adminstoresf-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "fstores-shop").await;
    let product = create_product(&app, &seller, &store, "F Product", 100_000, 10).await;
    // The buyer carts the product BEFORE the suspension —
    // the cart survives it (suspension never rewrites carts).
    let buyer = login(&app, "01855555555").await;
    add_to_cart(&app, &buyer, &product, 1).await;

    let (status, body) = patch_status(
        &app,
        &admin,
        &store,
        json!({ "status": "suspended", "note": "Under review" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suspend: {body}");

    // While suspended: detail 404s, the listing empties,
    // checkout refuses with the suspension 409.
    let uri = format!("/v1/products/{product}");
    let (status, body) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "detail hidden: {body}");
    let (status, listing) = call(&app, Method::GET, "/v1/products", None, None).await;
    assert_eq!(status, StatusCode::OK, "listing: {listing}");
    assert_eq!(listing["total"], 0, "listing empty: {listing}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "checkout refused: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("suspended"),
        "suspension message: {body}"
    );

    // Reactivate (no note — reactivation takes none).
    let (status, body) = patch_status(&app, &admin, &store, json!({ "status": "active" })).await;
    assert_eq!(status, StatusCode::OK, "reactivate: {body}");
    assert_eq!(body["status"], "active");

    // The note is cleared everywhere it was visible.
    let uri = format!("/v1/admin/stores/{store}");
    let (status, detail) = call(&app, Method::GET, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert!(detail["suspension_note"].is_null(), "note cleared: {detail}");
    assert!(detail["status_changed_at"].is_string(), "stamp updated: {detail}");
    let (status, dashboard) = call(
        &app,
        Method::GET,
        "/v1/seller/dashboard",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dashboard: {dashboard}");
    assert!(dashboard["suspension_note"].is_null(), "dashboard note cleared: {dashboard}");

    // One reactivation notice + one audit row; the suspend
    // artefacts are untouched.
    assert_eq!(count_kind(&pool, "store_reactivated").await, 1);
    let phone: String = sqlx::query_scalar(
        "SELECT phone FROM notifications WHERE kind = 'store_reactivated'",
    )
    .fetch_one(&pool)
    .await
    .expect("reactivation notice phone");
    assert_eq!(phone, "+8801722222222");
    assert_eq!(audit_count(&pool, "store.reactivate", &store).await, 1);
    assert_eq!(audit_count(&pool, "store.suspend", &store).await, 1);
    assert_eq!(count_kind(&pool, "store_suspended").await, 1);

    // Visibility and checkout are fully restored.
    let uri = format!("/v1/products/{product}");
    let (status, body) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "detail restored: {body}");
    let (status, listing) = call(&app, Method::GET, "/v1/products", None, None).await;
    assert_eq!(status, StatusCode::OK, "listing: {listing}");
    assert_eq!(listing["total"], 1, "listing restored: {listing}");
    let order = checkout_cod(&app, &buyer).await;
    assert_eq!(order["sub_orders"][0]["store_id"], store, "order: {order}");
}
