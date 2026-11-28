//! Admin stores remake, test D: suspension REQUIRES a
//! note. A missing, empty, or whitespace-only note is a
//! 400 and writes NOTHING (no status change, no stamp, no
//! audit row, no notice). A valid suspend stores the note
//! on the store row, stamps the transition, writes ONE
//! audit row whose detail carries the note, enqueues ONE
//! `store_suspended` notice to the owner's account phone,
//! and surfaces the note + stamp on the seller dashboard —
//! while the PUBLIC store view never carries the note.
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
async fn adminstores_suspend_requires_note_and_notifies() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin stores remake test D");
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
    let state = AppState::new(Some(pool.clone()), "adminstoresd-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "dstores-shop").await;
    create_product(&app, &seller, &store, "D Product", 50_000, 5).await;

    let uri = format!("/v1/admin/stores/{store}/status");
    // Missing, empty and whitespace-only notes all refuse
    // with the same 400 — and write nothing.
    for body in [
        json!({ "status": "suspended" }),
        json!({ "status": "suspended", "note": "" }),
        json!({ "status": "suspended", "note": "   " }),
    ] {
        let (status, refused) = call(&app, Method::PATCH, &uri, Some(body), Some(&admin)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "note-less suspend: {refused}");
    }
    let (row_status, row_note, row_stamp): (String, Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT status, suspension_note, status_changed_at::text \
             FROM stores WHERE id = $1::uuid",
        )
        .bind(&store)
        .fetch_one(&pool)
        .await
        .expect("store row after refusals");
    assert_eq!(row_status, "active");
    assert!(row_note.is_none(), "no note stored: {row_note:?}");
    assert!(row_stamp.is_none(), "no stamp written: {row_stamp:?}");
    assert_eq!(audit_count(&pool, "store.suspend", &store).await, 0);
    assert_eq!(count_kind(&pool, "store_suspended").await, 0);

    // A valid suspend.
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "suspended", "note": "Fraud reports under review" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suspend: {body}");
    assert_eq!(body["status"], "suspended");

    // The admin list row carries the note and the stamp.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/stores?q=dstores-shop",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "list: {list}");
    assert_eq!(list["items"][0]["suspension_note"], "Fraud reports under review");
    assert!(list["items"][0]["status_changed_at"].is_string());

    // The seller dashboard shows the reason and the stamp —
    // the seller-visible half of the decision.
    let (status, dashboard) = call(
        &app,
        Method::GET,
        "/v1/seller/dashboard",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dashboard: {dashboard}");
    assert_eq!(dashboard["store"]["status"], "suspended");
    assert_eq!(dashboard["suspension_note"], "Fraud reports under review");
    assert!(dashboard["status_changed_at"].is_string());

    // Exactly one notice, to the owner's account phone,
    // honestly worded and carrying the reason.
    assert_eq!(count_kind(&pool, "store_suspended").await, 1);
    let (phone, notice_body, payload): (String, String, String) = sqlx::query_as(
        "SELECT phone, body, payload::text FROM notifications \
         WHERE kind = 'store_suspended'",
    )
    .fetch_one(&pool)
    .await
    .expect("suspension notice row");
    assert_eq!(phone, "+8801722222222");
    assert!(notice_body.contains("সাময়িকভাবে"), "notice body: {notice_body}");
    assert!(
        notice_body.contains("Fraud reports under review"),
        "notice body carries the note: {notice_body}"
    );
    assert!(
        payload.contains("Fraud reports under review"),
        "notice payload carries the note: {payload}"
    );

    // Exactly one audit row, its detail carrying the note.
    assert_eq!(audit_count(&pool, "store.suspend", &store).await, 1);
    let detail: String = sqlx::query_scalar(
        "SELECT detail::text FROM admin_audit_log \
         WHERE action = 'store.suspend' AND target_id = $1",
    )
    .bind(&store)
    .fetch_one(&pool)
    .await
    .expect("audit detail");
    assert!(detail.contains("Fraud reports under review"), "audit detail: {detail}");

    // The PUBLIC store view never carries the admin's note.
    let (status, public) = call(&app, Method::GET, "/v1/stores/dstores-shop", None, None).await;
    assert_eq!(status, StatusCode::OK, "public store: {public}");
    assert!(
        !public.to_string().contains("Fraud reports"),
        "suspension note leaked into the public store view: {public}"
    );
}
