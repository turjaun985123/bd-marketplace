//! Admin verifications remake, part E: submission semantics.
//! Pins: a Bengali-digit NID is accepted and stored in
//! canonical ASCII (the full view returns the ASCII string —
//! the transliteration policy change); an IDENTICAL resubmit
//! of a pending application is a pure no-op (version and
//! `submitted_at` unmoved, no second event); a CHANGED edit
//! of a pending application bumps the version, keeps the
//! original `submitted_at`, and appends a `resubmitted`
//! event; duplicate flags on the admin detail count only
//! DIFFERENT-owner stores carrying the same NID (the same
//! proprietor's second store is legitimate and not counted).
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

/// The submission-event kinds recorded for one store, in order.
async fn event_kinds(pool: &PgPool, store_id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT kind FROM store_verification_events \
         WHERE store_id = $1 ORDER BY id ASC",
    )
    .bind(store_id)
    .fetch_all(pool)
    .await
    .expect("event kinds")
}

#[tokio::test]
async fn adminverif_submit_semantics_and_duplicate_flags() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin verifications (submit) test");
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
    let state = AppState::new(Some(pool.clone()), "adminverife-test-secret".to_string(), true);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // Bengali digits in, canonical ASCII stored.
    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "adminverife-alpha").await;
    let (status, view) = submit(
        &app,
        &seller,
        json!({
            "nid_number": "১২৩৪৫৬৭৮৯০১২৩",
            "document_url": "https://example.com/e-nid.jpg"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bengali submit: {view}");
    assert_eq!(view["nid_masked"], "*********0123");
    assert_eq!(view["version"], 1);
    let submitted_at = view["submitted_at"].as_str().expect("submitted_at").to_string();
    let (status, full) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/verifications/{store}/full"),
        Some(json!({ "reason": "canonical form check" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "full view: {full}");
    assert_eq!(full["nid_number"], "1234567890123");
    assert_eq!(event_kinds(&pool, &store).await, ["submitted"]);

    // Identical resubmit (now typed in ASCII — the canonical
    // form makes the two spellings the same payload): a pure
    // no-op. Version, submitted_at and the event log unmoved.
    let (status, view) = submit(
        &app,
        &seller,
        json!({
            "nid_number": "1234567890123",
            "document_url": "https://example.com/e-nid.jpg"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "identical resubmit: {view}");
    assert_eq!(view["version"], 1);
    assert_eq!(view["submitted_at"], submitted_at);
    assert_eq!(event_kinds(&pool, &store).await, ["submitted"]);

    // A changed edit: version bumps, submitted_at (queue
    // seniority) is preserved, a `resubmitted` event lands.
    let (status, view) = submit(
        &app,
        &seller,
        json!({
            "nid_number": "1234567890123",
            "document_url": "https://example.com/e-nid-clearer.jpg",
            "applicant_name": "মোঃ রফিকুল ইসলাম"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "changed edit: {view}");
    assert_eq!(view["version"], 2);
    assert_eq!(view["submitted_at"], submitted_at);
    assert_eq!(view["applicant_name"], "মোঃ রফিকুল ইসলাম");
    assert_eq!(event_kinds(&pool, &store).await, ["submitted", "resubmitted"]);

    // Duplicate flags: a DIFFERENT owner filing the same NID
    // flags the first store; the SAME owner's second store
    // does not.
    let other = login(&app, "01733333333").await;
    let other_store = create_store(&app, &other, "adminverife-beta").await;
    let (status, view) = submit(
        &app,
        &other,
        json!({ "nid_number": "1234567890123" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "other submit: {view}");
    assert_eq!(view["status"], "pending");
    let detail_uri = format!("/v1/admin/verifications/{store}");
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["duplicate_nid_stores"], 1);
    assert_eq!(detail["duplicate_license_stores"], 0);
    // The history on the detail shows both submission events.
    let kinds: Vec<&str> = detail["history"]
        .as_array()
        .expect("history list")
        .iter()
        .filter_map(|entry| entry["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"submitted"), "history kinds: {kinds:?}");
    assert!(kinds.contains(&"resubmitted"), "history kinds: {kinds:?}");
    // The other store's own detail flags the first store back.
    let (status, other_detail) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/verifications/{other_store}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "other detail: {other_detail}");
    assert_eq!(other_detail["duplicate_nid_stores"], 1);

    // Same owner, second store, same NID: legitimate reuse —
    // the count for the first store stays at 1 (the stranger).
    let second = create_store(&app, &seller, "adminverife-gamma").await;
    let (status, view) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/verification?store_id={second}"),
        Some(json!({ "nid_number": "1234567890123" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "second store submit: {view}");
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "detail after: {detail}");
    assert_eq!(detail["duplicate_nid_stores"], 1);
    assert_eq!(detail["owner_stores"].as_array().map(|rows| rows.len()), Some(2));
}
