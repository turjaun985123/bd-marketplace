//! Admin stores remake, test E: a no-op transition is a
//! 409 and writes NOTHING. The pre-remake code updated the
//! store unconditionally, so a repeated suspend appended a
//! phantom `store.suspend` audit row every time (security
//! audit LO-93 prescribes the 409). A repeat suspend — even
//! with a DIFFERENT note — must leave the audit count, the
//! notice count, the stored note and the stamp exactly as
//! the first transition left them; a real transition after
//! the refusals still works. Helpers are copied from the
//! Wave 9 test on purpose — integration test files stay
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

/// The store row's (note, stamp) pair, as text.
async fn note_and_stamp(pool: &PgPool, store_id: &str) -> (Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT suspension_note, status_changed_at::text \
         FROM stores WHERE id = $1::uuid",
    )
    .bind(store_id)
    .fetch_one(pool)
    .await
    .expect("store note and stamp")
}

#[tokio::test]
async fn adminstores_noop_transition_is_409_and_writes_nothing() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin stores remake test E");
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
    let state = AppState::new(Some(pool.clone()), "adminstorese-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "estores-shop").await;
    let other_seller = login(&app, "01733333333").await;
    let other = create_store(&app, &other_seller, "estores-other").await;

    // The real suspend.
    let (status, body) = patch_status(
        &app,
        &admin,
        &store,
        json!({ "status": "suspended", "note": "First note" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "first suspend: {body}");
    let (note_before, stamp_before) = note_and_stamp(&pool, &store).await;
    assert_eq!(note_before.as_deref(), Some("First note"));
    assert!(stamp_before.is_some());

    // Repeat suspends — same note, then a different one —
    // are 409s naming the current status.
    for note in ["First note", "Second note"] {
        let (status, body) = patch_status(
            &app,
            &admin,
            &store,
            json!({ "status": "suspended", "note": note }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "repeat suspend ({note}): {body}");
        assert!(
            body["error"]
                .as_str()
                .expect("error message")
                .contains("already"),
            "conflict names the state: {body}"
        );
    }
    // Nothing moved: one audit row, one notice, the first
    // note and stamp untouched.
    assert_eq!(audit_count(&pool, "store.suspend", &store).await, 1);
    assert_eq!(count_kind(&pool, "store_suspended").await, 1);
    let (note_after, stamp_after) = note_and_stamp(&pool, &store).await;
    assert_eq!(note_after, note_before);
    assert_eq!(stamp_after, stamp_before);

    // Reactivating a store that was never suspended is the
    // same no-op: 409, no audit row, no notice.
    let (status, body) = patch_status(&app, &admin, &other, json!({ "status": "active" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "no-op reactivate: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("already"),
        "conflict names the state: {body}"
    );
    assert_eq!(audit_count(&pool, "store.reactivate", &other).await, 0);
    assert_eq!(count_kind(&pool, "store_reactivated").await, 0);

    // A real transition after the refusals still works —
    // and its own repeat then 409s too.
    let (status, body) = patch_status(&app, &admin, &store, json!({ "status": "active" })).await;
    assert_eq!(status, StatusCode::OK, "real reactivate: {body}");
    assert_eq!(body["status"], "active");
    let (status, body) = patch_status(&app, &admin, &store, json!({ "status": "active" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "repeat reactivate: {body}");
    assert_eq!(audit_count(&pool, "store.reactivate", &store).await, 1);
    assert_eq!(count_kind(&pool, "store_reactivated").await, 1);
}
