//! Admin verifications remake, part C: the decision gates.
//! Pins: approving a store with no application is a 404 (not
//! a 409 — nothing exists to transition); `verified` without
//! a trade license is a 400 naming the license; approving an
//! application with NO document image is a 400 unless the
//! reviewer passes the explicit `no_document_override` (which
//! the audit detail then records); approval on a SUSPENDED
//! store is a 409, while rejection on a suspended store still
//! works (it releases the seller — the payouts precedent).
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
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

/// Submit a verification application as the store's seller.
async fn submit(app: &Router, token: &str, body: Value) -> (StatusCode, Value) {
    call(
        app,
        Method::PUT,
        "/v1/seller/store/verification",
        Some(body),
        Some(token),
    )
    .await
}

/// The error message of a response body, for message assertions.
fn error_text(body: &Value) -> String {
    body["error"].as_str().expect("error message").to_string()
}

#[tokio::test]
async fn adminverif_decision_gates() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin verifications (gates) test");
            return;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, store_verification_events, \
         store_verifications, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         products, api_keys, store_members, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let state = AppState::new(Some(pool.clone()), "adminverifc-test-secret".to_string(), true);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // No application at all: 404, distinct from a decided
    // application's 409.
    let ghost_seller = login(&app, "01722222222").await;
    let ghost_store = create_store(&app, &ghost_seller, "adminverifc-ghost").await;
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/verifications/{ghost_store}/approve"),
        Some(json!({ "tier": "basic", "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "approve ghost: {body}");

    // License gate: document present, no license — `verified`
    // is a 400 that names the trade license, `basic` is fine.
    let seller_a = login(&app, "01733333333").await;
    let store_a = create_store(&app, &seller_a, "adminverifc-alpha").await;
    let (status, view) = submit(
        &app,
        &seller_a,
        json!({
            "nid_number": "1234567890123",
            "document_url": "https://example.com/a-nid.jpg"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "submit A: {view}");
    assert_eq!(view["version"], 1);
    let approve_a = format!("/v1/admin/verifications/{store_a}/approve");
    let (status, body) = call(
        &app,
        Method::POST,
        &approve_a,
        Some(json!({ "tier": "verified", "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "verified w/o license: {body}");
    assert!(error_text(&body).contains("trade license"), "license msg: {body}");
    let (status, view) = call(
        &app,
        Method::POST,
        &approve_a,
        Some(json!({ "tier": "basic", "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve A basic: {view}");
    assert_eq!(view["tier"], "basic");
    // Keep the store id bound to an assertion (clippy: every
    // binding must be used) — the detail view agrees.
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/verifications/{store_a}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail A: {detail}");
    assert_eq!(detail["status"], "approved");

    // Document gate: no document image at all — approval is a
    // 400 until the reviewer explicitly overrides, and the
    // override is recorded in the audit detail.
    let seller_b = login(&app, "01744444444").await;
    let store_b = create_store(&app, &seller_b, "adminverifc-beta").await;
    let (status, view) = submit(&app, &seller_b, json!({ "nid_number": "1234567890" })).await;
    assert_eq!(status, StatusCode::OK, "submit B: {view}");
    let approve_b = format!("/v1/admin/verifications/{store_b}/approve");
    let (status, body) = call(
        &app,
        Method::POST,
        &approve_b,
        Some(json!({ "tier": "basic", "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "documentless approve: {body}");
    let (status, view) = call(
        &app,
        Method::POST,
        &approve_b,
        Some(json!({
            "tier": "basic",
            "expected_version": 1,
            "no_document_override": true
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "override approve B: {view}");
    assert_eq!(view["tier"], "basic");
    let detail_text: String = sqlx::query_scalar(
        "SELECT detail::text FROM admin_audit_log \
         WHERE action = 'verification.approve' AND target_id = $1",
    )
    .bind(&store_b)
    .fetch_one(&pool)
    .await
    .expect("approve audit row");
    assert!(
        detail_text.contains("no_document_override"),
        "override recorded: {detail_text}"
    );
    assert!(
        detail_text.contains("true"),
        "override flag true: {detail_text}"
    );

    // Suspension freeze: approve is a 409 on a suspended
    // store; reject still works.
    let seller_c = login(&app, "01755555555").await;
    let store_c = create_store(&app, &seller_c, "adminverifc-gamma").await;
    let (status, view) = submit(
        &app,
        &seller_c,
        json!({
            "nid_number": "12345678901234567",
            "trade_license_number": "TL-777-111",
            "document_url": "https://example.com/c-nid.jpg"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "submit C: {view}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/stores/{store_c}/status"),
        Some(json!({ "status": "suspended", "note": "Test suspension" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suspend C: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/verifications/{store_c}/approve"),
        Some(json!({ "tier": "verified", "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "approve suspended: {body}");
    let (status, view) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/verifications/{store_c}/reject"),
        Some(json!({ "note": "স্টোর সাসপেন্ড থাকায় পরে আবার দেখা হবে", "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reject suspended: {view}");
    assert_eq!(view["status"], "rejected");
}
