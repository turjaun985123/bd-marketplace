//! Admin verifications remake, part B: decisions end to end.
//! Pins: a decision without `expected_version` is a 400; a
//! blank rejection note is a 400; a rejection lands the note
//! on the seller's view AND enqueues exactly one
//! `verification_rejected` notice to the OWNER's account phone
//! whose body carries the note; a stale-version approval after
//! resubmission is a 409; a correct approval enqueues exactly
//! one `verification_approved` notice whose body names the
//! granted tier in Bangla and whose payload carries
//! `verification_tier`; re-deciding a decided application is a
//! 409 that enqueues nothing more; each decision writes
//! exactly one audit row.
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

/// Count notification rows of one kind.
async fn count_kind(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE kind = $1")
        .bind(kind)
        .fetch_one(pool)
        .await
        .expect("count notifications")
}

/// The latest notification row of one kind: (body, phone, payload text).
async fn latest_notice(pool: &PgPool, kind: &str) -> (String, String, String) {
    sqlx::query_as(
        "SELECT body, phone, payload::text FROM notifications \
         WHERE kind = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(kind)
    .fetch_one(pool)
    .await
    .expect("latest notification")
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
async fn adminverif_decisions_notify_and_version_gate() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin verifications (decisions) test");
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
    let state = AppState::new(Some(pool.clone()), "adminverifb-test-secret".to_string(), true);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "adminverifb-alpha").await;
    let application = json!({
        "nid_number": "1234567890",
        "trade_license_number": "TL-2024-9988",
        "document_url": "https://example.com/beta-nid.jpg"
    });
    let (status, view) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/verification",
        Some(application.clone()),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "submit: {view}");
    assert_eq!(view["version"], 1);

    let reject_uri = format!("/v1/admin/verifications/{store}/reject");
    let approve_uri = format!("/v1/admin/verifications/{store}/approve");

    // No version presented: 400, nothing decided, nothing sent.
    let (status, body) = call(
        &app,
        Method::POST,
        &reject_uri,
        Some(json!({ "note": "কারণ আছে" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "versionless reject: {body}");
    assert_eq!(count_kind(&pool, "verification_rejected").await, 0);

    // Blank note: 400 (the seller needs a reason they can act on).
    let (status, body) = call(
        &app,
        Method::POST,
        &reject_uri,
        Some(json!({ "note": "  ", "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "blank note: {body}");
    assert_eq!(count_kind(&pool, "verification_rejected").await, 0);

    // The real rejection: note lands on the seller view, one
    // notice to the owner's account phone carries it, one
    // audit row is written.
    let note = "ছবি ঝাপসা — পরিষ্কার ছবি দিন";
    let (status, view) = call(
        &app,
        Method::POST,
        &reject_uri,
        Some(json!({ "note": note, "expected_version": 1 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reject: {view}");
    assert_eq!(view["status"], "rejected");
    assert_eq!(view["version"], 2);
    let (status, seller_view) = call(
        &app,
        Method::GET,
        "/v1/seller/store/verification",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller view: {seller_view}");
    assert_eq!(seller_view["reviewer_note"], note);
    assert_eq!(count_kind(&pool, "verification_rejected").await, 1);
    assert_eq!(count_kind(&pool, "verification_approved").await, 0);
    let (body_text, phone, _payload) = latest_notice(&pool, "verification_rejected").await;
    assert_eq!(phone, "+8801733333333");
    assert!(body_text.contains(note), "notice carries the note: {body_text}");
    assert!(
        !body_text.contains("1234567890"),
        "notice must never carry the NID: {body_text}"
    );
    assert_eq!(audit_count(&pool, "verification.reject", &store).await, 1);

    // Resubmission bumps the version to 3 — an approval aimed
    // at the pre-rejection version is now stale: 409.
    let (status, view) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/verification",
        Some(application),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "resubmit: {view}");
    assert_eq!(view["status"], "pending");
    assert_eq!(view["version"], 3);
    let (status, body) = call(
        &app,
        Method::POST,
        &approve_uri,
        Some(json!({ "tier": "verified", "expected_version": 2 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "stale approval: {body}");
    assert_eq!(count_kind(&pool, "verification_approved").await, 0);

    // The correct approval: tier lands, one notice names the
    // tier in Bangla, the payload carries verification_tier.
    let (status, view) = call(
        &app,
        Method::POST,
        &approve_uri,
        Some(json!({ "tier": "verified", "expected_version": 3 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve: {view}");
    assert_eq!(view["status"], "approved");
    assert_eq!(view["tier"], "verified");
    assert_eq!(count_kind(&pool, "verification_approved").await, 1);
    let (body_text, phone, payload) = latest_notice(&pool, "verification_approved").await;
    assert_eq!(phone, "+8801733333333");
    assert!(body_text.contains("যাচাইকৃত ব্যবসা"), "tier in body: {body_text}");
    assert!(
        body_text.contains("প্ল্যাটফর্ম দলের পর্যালোচনায়"),
        "manual-review wording: {body_text}"
    );
    assert!(payload.contains("verification_tier"), "payload key: {payload}");
    assert!(payload.contains("verified"), "payload tier: {payload}");
    assert_eq!(audit_count(&pool, "verification.approve", &store).await, 1);

    // Re-deciding a decided application: 409, and no second
    // notice or audit row leaks out of the attempt.
    let (status, body) = call(
        &app,
        Method::POST,
        &approve_uri,
        Some(json!({ "tier": "basic", "expected_version": 4 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "re-approve: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &reject_uri,
        Some(json!({ "note": "আবার", "expected_version": 4 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "reject approved: {body}");
    assert_eq!(count_kind(&pool, "verification_approved").await, 1);
    assert_eq!(count_kind(&pool, "verification_rejected").await, 1);
    assert_eq!(audit_count(&pool, "verification.approve", &store).await, 1);
    assert_eq!(audit_count(&pool, "verification.reject", &store).await, 1);
}
