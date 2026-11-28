//! Admin stores remake, test A: the admin store LIST masks
//! the owner's phone (the pre-remake list leaked the full
//! number — the same leak class the payouts remake removed
//! from its list), carries the decision aggregates and the
//! whole-queue summary, and the new DETAIL endpoint stays
//! masked too (no payout-style reveal: no store decision is
//! executed by phone). Helpers are copied from the Wave 9
//! test on purpose — integration test files stay
//! self-contained.
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

#[tokio::test]
async fn adminstores_list_masks_owner_phone_and_summarizes() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin stores remake test A");
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
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "adminstores-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller_a = login(&app, "01722222222").await;
    let alpha = create_store(&app, &seller_a, "astores-alpha").await;
    create_product(&app, &seller_a, &alpha, "Alpha One", 50_000, 5).await;
    create_product(&app, &seller_a, &alpha, "Alpha Two", 60_000, 5).await;
    let seller_b = login(&app, "01733333333").await;
    let beta = create_store(&app, &seller_b, "astores-beta").await;
    create_product(&app, &seller_b, &beta, "Beta One", 70_000, 5).await;
    // Deterministic default order: alpha is the older store.
    sqlx::query("UPDATE stores SET created_at = now() - interval '1 day' WHERE id = $1::uuid")
        .bind(&alpha)
        .execute(&pool)
        .await
        .expect("backdate alpha");

    // The list is admin-only.
    let (status, body) = call(&app, Method::GET, "/v1/admin/stores", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no token: {body}");
    let (status, body) = call(&app, Method::GET, "/v1/admin/stores", None, Some(&seller_a)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller token: {body}");

    let (status, list) = call(&app, Method::GET, "/v1/admin/stores", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin list: {list}");
    assert_eq!(list["total"], 2);
    assert_eq!(list["page"], 1);
    assert_eq!(list["items"][0]["id"], beta, "newest first: {list}");
    assert_eq!(list["items"][1]["id"], alpha);

    // Masking: the masked field is present and correct, the
    // full-phone field is GONE, and neither spelling of the
    // full number appears anywhere in the raw body.
    let raw = list.to_string();
    assert!(
        !raw.contains("+8801722222222"),
        "full phone (canonical) leaked into the list: {raw}"
    );
    assert!(
        !raw.contains("01722222222"),
        "full phone (local) leaked into the list: {raw}"
    );
    for item in list["items"].as_array().expect("items array") {
        assert!(
            item.get("owner_phone").is_none(),
            "owner_phone key must not exist: {item}"
        );
        assert!(
            item["owner_phone_masked"].is_string(),
            "masked field present: {item}"
        );
    }
    let alpha_row = &list["items"][1];
    assert_eq!(alpha_row["owner_phone_masked"], "017****222");
    assert_eq!(alpha_row["product_count"], 2);
    assert_eq!(alpha_row["status"], "active");
    assert_eq!(alpha_row["verified_tier"], "unverified");
    assert_eq!(alpha_row["plan"], "free");
    assert_eq!(alpha_row["follower_count"], 0);
    assert_eq!(alpha_row["order_count"], 0);
    assert_eq!(alpha_row["delivered_gmv_poisha"], 0);
    assert_eq!(alpha_row["pending_payout_count"], 0);
    assert_eq!(alpha_row["pending_payout_poisha"], 0);
    assert_eq!(alpha_row["open_returns_count"], 0);
    assert!(alpha_row["suspension_note"].is_null());
    assert!(alpha_row["status_changed_at"].is_null());

    // The whole-queue summary.
    assert_eq!(list["summary"]["total"], 2);
    assert_eq!(list["summary"]["active"], 2);
    assert_eq!(list["summary"]["suspended"], 0);
    assert_eq!(list["summary"]["unverified"], 2);
    assert_eq!(list["summary"]["basic"], 0);
    assert_eq!(list["summary"]["verified"], 0);

    // The detail endpoint: same masking discipline (a
    // deliberate divergence from the payout detail, which
    // carries the full phone because money is sent from it).
    let uri = format!("/v1/admin/stores/{alpha}");
    let (status, detail) = call(&app, Method::GET, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin detail: {detail}");
    assert_eq!(detail["id"], alpha);
    assert_eq!(detail["owner_phone_masked"], "017****222");
    assert!(detail.get("owner_phone").is_none());
    let raw_detail = detail.to_string();
    assert!(
        !raw_detail.contains("+8801722222222") && !raw_detail.contains("01722222222"),
        "full phone leaked into the detail: {raw_detail}"
    );
    assert_eq!(detail["earnings"]["earned_poisha"], 0);
    assert_eq!(detail["subscription"]["plan"], "free");
    assert_eq!(detail["verification"]["status"], "none");
    let owner_stores = detail["owner_stores"].as_array().expect("owner stores");
    assert_eq!(owner_stores.len(), 1, "owner stores: {detail}");
    assert_eq!(owner_stores[0]["store_id"], alpha);
    assert_eq!(owner_stores[0]["status"], "active");
    assert!(detail["audit_history"].as_array().expect("audit history").is_empty());
}
