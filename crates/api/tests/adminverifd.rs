//! Admin verifications remake, part D: who may touch KYC.
//! Pins: submission and reading are OWNER-only since the
//! remake (the new `manage_verification` domain action) — a
//! store MANAGER who could previously submit under the shared
//! store-settings action now gets 403 on both the scoped PUT
//! and the scoped GET; the owner submitting with an explicit
//! `?store_id=` for their own store still works (the matrix
//! array entry exists); and a write-scoped API key — which
//! acts as the owner — is refused the PUT by the JWT-only
//! denylist (the route is a PUT: a POST-only clause would
//! protect nothing), while a key GET stays allowed.
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

#[tokio::test]
async fn adminverif_kyc_is_owner_only_and_jwt_only() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin verifications (access) test");
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
    let state = AppState::new(Some(pool.clone()), "adminverifd-test-secret".to_string(), true);
    let app = build_app(state);

    let owner = login(&app, "01766666666").await;
    let store = create_store(&app, &owner, "adminverifd-alpha").await;

    // Seat a manager (their account must exist first).
    let manager = login(&app, "01777777777").await;
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/stores/{store}/members"),
        Some(json!({ "phone": "01777777777", "role": "manager" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "seat manager: {body}");
    assert_eq!(body["role"], "manager");

    let scoped = format!("/v1/seller/store/verification?store_id={store}");
    let application = json!({ "nid_number": "1234567890123" });

    // The manager can neither read nor replace the owner's
    // KYC file for a store they serve.
    let (status, body) = call(&app, Method::GET, &scoped, None, Some(&manager)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "manager read: {body}");
    let (status, body) = call(
        &app,
        Method::PUT,
        &scoped,
        Some(application.clone()),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "manager submit: {body}");

    // Nothing was filed by the attempts above.
    let (status, view) = call(
        &app,
        Method::GET,
        "/v1/seller/store/verification",
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "owner view: {view}");
    assert_eq!(view["status"], "none");

    // The owner submitting with an explicit store_id works —
    // the scoped path consults the matrix, so this pins the
    // new action's STORE_ACTIONS entry.
    let (status, view) = call(
        &app,
        Method::PUT,
        &scoped,
        Some(application.clone()),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "owner scoped submit: {view}");
    assert_eq!(view["status"], "pending");
    assert_eq!(view["version"], 1);

    // A write-scoped API key acts as the owner everywhere
    // EXCEPT the JWT-only set: the verification PUT refuses
    // it (403), the GET stays open.
    let (status, created) = call(
        &app,
        Method::POST,
        "/v1/api-keys",
        Some(json!({ "name": "verif key", "scopes": ["write"], "expires_in_days": 30 })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create key: {created}");
    let key = created["key"].as_str().expect("full key shown once").to_string();
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/verification",
        Some(application),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "key submit: {body}");
    let (status, view) = call(
        &app,
        Method::GET,
        "/v1/seller/store/verification",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "key read: {view}");
    assert_eq!(view["status"], "pending");
}
