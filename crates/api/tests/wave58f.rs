//! Wave 58 integration test: layout access follows the
//! BRANDING rule exactly (Wave 34 `manage_marketing`):
//! - anonymous reads of the seller layout are a 401;
//! - a stranger seller naming the store via `?store_id=`
//!   gets the scoped 404 (the store is not theirs to
//!   know about);
//! - a STAFF member is refused (403) on read and write —
//!   staff run catalog/orders/questions, not marketing;
//! - a MANAGER member can read AND write the layout of
//!   the store they serve;
//! - the owner reads their own layout with no scope, as
//!   always.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

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

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 58 integration tests");
            return None;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         buyer_addresses, bulk_imports, store_layouts, storefront_events, \
         store_collections, store_collection_products, \
         product_images, product_variants, products, store_members, store_follows, \
         stores, otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave58-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// POST a store for the token's user; returns the store view.
async fn create_store(app: &Router, token: &str, slug: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body
}

/// The id of the user a phone login created (E.164 form).
async fn user_id(pool: &PgPool, phone_e164: &str) -> String {
    sqlx::query_scalar("SELECT id::text FROM users WHERE phone = $1")
        .bind(phone_e164)
        .fetch_one(pool)
        .await
        .expect("user row exists")
}

/// Enrol a user in a store's team with a role (the team
/// surface's own writes are Wave 34's tests' business;
/// this wave only reads the role matrix back).
async fn add_member(pool: &PgPool, store_id: &str, member_id: &str, role: &str) {
    sqlx::query(
        "INSERT INTO store_members (store_id, user_id, role) \
         VALUES ($1::uuid, $2::uuid, $3)",
    )
    .bind(store_id)
    .bind(member_id)
    .bind(role)
    .execute(pool)
    .await
    .expect("insert store member");
}

#[tokio::test]
async fn wave58_layout_access_matches_the_branding_rule() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let owner = login(&app, "01711111111").await;
    let store = create_store(&app, &owner, "w58-access").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let scoped = format!("/v1/seller/store/layout?store_id={store_id}");

    // Anonymous: 401 before anything else.
    let (status, body) = call(&app, Method::GET, "/v1/seller/store/layout", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous: {body}");

    // The owner, unscoped: their own store's default.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "owner read: {body}");
    assert_eq!(body["store_id"], json!(store_id));

    // A stranger seller (with a store of their own) naming
    // this store: the scoped 404.
    let stranger = login(&app, "01722222222").await;
    create_store(&app, &stranger, "w58-stranger").await;
    let (status, body) = call(&app, Method::GET, &scoped, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger read: {body}");
    let (status, body) = call(
        &app,
        Method::PUT,
        &scoped,
        Some(json!({ "sections": [] })),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger write: {body}");
    // The stranger's own unscoped read is their own store.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stranger own read: {body}");
    assert_ne!(body["store_id"], json!(store_id));

    // Staff: enrolled, but the role lacks manage_marketing
    // — 403 on read and on write.
    let staff = login(&app, "01733333333").await;
    let staff_id = user_id(&pool, "+8801733333333").await;
    add_member(&pool, &store_id, &staff_id, "staff").await;
    let (status, body) = call(&app, Method::GET, &scoped, None, Some(&staff)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "staff read: {body}");
    let (status, body) = call(
        &app,
        Method::PUT,
        &scoped,
        Some(json!({ "sections": [] })),
        Some(&staff),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "staff write: {body}");

    // Manager: the role carries manage_marketing — read
    // and write both work, and the write is real (the
    // owner's own read sees the custom layout).
    let manager = login(&app, "01744444444").await;
    let manager_id = user_id(&pool, "+8801744444444").await;
    add_member(&pool, &store_id, &manager_id, "manager").await;
    let (status, body) = call(&app, Method::GET, &scoped, None, Some(&manager)).await;
    assert_eq!(status, StatusCode::OK, "manager read: {body}");
    assert_eq!(body["store_id"], json!(store_id));
    let (status, body) = call(
        &app,
        Method::PUT,
        &scoped,
        Some(json!({
            "sections": [
                { "id": "note", "type": "rich_text", "config": { "body": "ম্যানেজারের নোট" } }
            ]
        })),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "manager write: {body}");
    assert_eq!(body["is_custom"], json!(true));
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "owner re-read: {body}");
    assert_eq!(body["is_custom"], json!(true), "the manager's write landed");
}
