//! Admin verifications remake, part A: the audited full-NID
//! view. Pins: the reveal is a POST carrying a mandatory,
//! validated reason (blank / too short / containing the NID
//! itself are all 400 and write NOTHING); a valid reveal
//! writes exactly one `verification.view_full_nid` audit row
//! whose detail carries the reason and never the number, and
//! is attributed to the revealing admin; a seller gets 403; a
//! never-applied store gets 404; and the full number appears
//! in NO other surface (queue list, detail, seller view).
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.

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
async fn adminverif_full_nid_reveal_is_audited_and_reason_gated() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin verifications (full view) test");
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
    let state = AppState::new(Some(pool.clone()), "adminverif-test-secret".to_string(), true);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let admin_id: String = sqlx::query_scalar(
        "SELECT id::text FROM users WHERE phone = '+8801711111111'",
    )
    .fetch_one(&pool)
    .await
    .expect("admin id");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "adminverif-alpha").await;
    let nid = "1234567890123";
    let (status, view) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/verification",
        Some(json!({
            "nid_number": nid,
            "document_url": "https://example.com/alpha-nid.jpg"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "submit: {view}");
    assert_eq!(view["status"], "pending");

    let full_uri = format!("/v1/admin/verifications/{store}/full");

    // Blank, junk and self-incriminating reasons are all 400 —
    // and none of them writes an audit row or returns a number.
    for bad_reason in ["", "ok", "checking 1234567890123 against the document"] {
        let (status, body) = call(
            &app,
            Method::POST,
            &full_uri,
            Some(json!({ "reason": bad_reason })),
            Some(&admin),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "reason {bad_reason:?}: {body}");
        assert!(!body.to_string().contains(nid), "no number on refusal: {body}");
    }
    assert_eq!(audit_count(&pool, "verification.view_full_nid", &store).await, 0);

    // A seller cannot reveal, even with a plausible reason.
    let (status, body) = call(
        &app,
        Method::POST,
        &full_uri,
        Some(json!({ "reason": "seller self-service attempt" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller reveal: {body}");
    assert_eq!(audit_count(&pool, "verification.view_full_nid", &store).await, 0);

    // The valid reveal: 200, the number, the snapshot version —
    // and exactly one audit row, attributed, reason-bearing,
    // number-free.
    let reason = "document comparison for queue review";
    let (status, full) = call(
        &app,
        Method::POST,
        &full_uri,
        Some(json!({ "reason": reason })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reveal: {full}");
    assert_eq!(full["nid_number"], nid);
    assert_eq!(full["version"], 1);
    assert_eq!(audit_count(&pool, "verification.view_full_nid", &store).await, 1);
    let (row_admin, detail_text): (String, String) = sqlx::query_as(
        "SELECT admin_user_id::text, detail::text FROM admin_audit_log \
         WHERE action = 'verification.view_full_nid' AND target_id = $1",
    )
    .bind(&store)
    .fetch_one(&pool)
    .await
    .expect("audit row exists");
    assert_eq!(row_admin, admin_id);
    let detail: Value = serde_json::from_str(&detail_text).expect("detail is JSON");
    assert_eq!(detail["reason"], reason);
    assert_eq!(detail["nid_last4"], "0123");
    assert!(
        !detail_text.contains(nid),
        "audit detail must never carry the full NID: {detail_text}"
    );

    // A store that never applied: 404, and no audit row.
    let other_store = create_store(&app, &seller, "adminverif-beta").await;
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/verifications/{other_store}/full"),
        Some(json!({ "reason": reason })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "never applied: {body}");
    assert_eq!(
        audit_count(&pool, "verification.view_full_nid", &other_store).await,
        0
    );

    // The number appears in NO other surface.
    let (status, queue) = call(
        &app,
        Method::GET,
        "/v1/admin/verifications?status=pending",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "queue: {queue}");
    assert!(!queue.to_string().contains(nid), "queue leaks NID: {queue}");
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/verifications/{store}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert!(!detail.to_string().contains(nid), "detail leaks NID: {detail}");
    assert_eq!(detail["nid_masked"], "*********0123");
    let (status, seller_view) = call(
        &app,
        Method::GET,
        "/v1/seller/store/verification",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller view: {seller_view}");
    assert!(
        !seller_view.to_string().contains(nid),
        "seller view leaks NID: {seller_view}"
    );
}
