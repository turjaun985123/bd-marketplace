//! Admin products remake, test B: the takedown/restore
//! transition discipline. The reason is REQUIRED on
//! takedown and the note is REQUIRED on restore — every
//! refusal shape (missing body, missing field, blank,
//! over the 500-char cap) is a 400 with NOTHING written:
//! no flag, no audit row, no notice. A valid takedown
//! writes the flag + reason on the row, exactly ONE audit
//! row whose detail carries the reason, and exactly ONE
//! `product_taken_down` notice to the store owner carrying
//! the reason. Double takedown and double restore are
//! 409s. Restore clears the row's takedown columns but
//! the restore audit detail preserves the outgoing reason
//! and hider. Takedown of a DRAFT product is allowed, and
//! restore returns it to draft — the flag is orthogonal
//! to the seller's status.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job.

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

/// Create a product with an explicit status; returns the id.
async fn create_product_with_status(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
    status: &str,
) -> String {
    let (code, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": status
        })),
        Some(token),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Count audit rows for one product and action.
async fn audit_count(pool: &PgPool, product_id: &str, action: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_audit_log \
         WHERE target_type = 'product' AND target_id = $1 AND action = $2",
    )
    .bind(product_id)
    .bind(action)
    .fetch_one(pool)
    .await
    .expect("count audit rows")
}

/// Count notifications of one kind.
async fn notice_count(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE kind = $1")
        .bind(kind)
        .fetch_one(pool)
        .await
        .expect("count notifications")
}

#[tokio::test]
async fn adminproducts_takedown_restore_discipline() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin products remake test B");
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
    let state = AppState::new(
        Some(pool.clone()),
        "adminproductsb-test-secret".to_string(),
        true,
    )
    .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "aprod-bravo").await;
    let product = create_product_with_status(
        &app,
        &seller,
        &store,
        "Bravo Gadget",
        220_000,
        8,
        "active",
    )
    .await;
    let takedown_uri = format!("/v1/admin/products/{product}/takedown");
    let restore_uri = format!("/v1/admin/products/{product}/restore");
    let detail_uri = format!("/v1/admin/products/{product}");

    // --- Every refusal shape is a 400 with NOTHING written. ---
    for body in [
        None,
        Some(json!({})),
        Some(json!({ "note": "   " })),
        Some(json!({ "note": "x".repeat(501) })),
    ] {
        let (status, response) = call(
            &app,
            Method::POST,
            &takedown_uri,
            body,
            Some(&admin),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "refused takedown: {response}");
    }
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "detail after refusals: {detail}");
    assert_eq!(detail["admin_hidden_at"], Value::Null);
    assert_eq!(audit_count(&pool, &product, "product.takedown").await, 0);
    assert_eq!(notice_count(&pool, "product_taken_down").await, 0);

    // --- A valid takedown writes flag + audit + notice, once each. ---
    let reason = "নকল পণ্য বিক্রির প্রমাণ মিলেছে";
    let (status, body) = call(
        &app,
        Method::POST,
        &takedown_uri,
        Some(json!({ "note": format!("  {reason}  ") })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "takedown: {body}");
    assert!(body["admin_hidden_at"].is_string(), "takedown: {body}");
    assert_eq!(body["admin_hidden_reason"], json!(reason));
    assert_eq!(body["status"], json!("active"), "seller status untouched: {body}");
    assert_eq!(audit_count(&pool, &product, "product.takedown").await, 1);
    assert_eq!(notice_count(&pool, "product_taken_down").await, 1);

    // The audit detail contractually carries the reason, the
    // store and the prior status; the notice payload carries
    // the reason and goes to the owner's account phone.
    let detail_json: String = sqlx::query_scalar(
        "SELECT detail::text FROM admin_audit_log \
         WHERE target_type = 'product' AND target_id = $1 AND action = 'product.takedown'",
    )
    .bind(&product)
    .fetch_one(&pool)
    .await
    .expect("takedown audit detail");
    assert!(detail_json.contains(reason), "audit detail: {detail_json}");
    assert!(detail_json.contains(&store), "audit detail: {detail_json}");
    assert!(detail_json.contains("active"), "audit detail: {detail_json}");
    let (payload, phone): (String, String) = sqlx::query_as(
        "SELECT payload::text, phone FROM notifications WHERE kind = 'product_taken_down'",
    )
    .fetch_one(&pool)
    .await
    .expect("takedown notice row");
    assert!(payload.contains(reason), "notice payload: {payload}");
    assert_eq!(phone, "+8801722222222", "owner account phone");

    // The detail's audit history shows the takedown.
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["audit_history"].as_array().map(Vec::len), Some(1));
    assert_eq!(detail["audit_history"][0]["action"], json!("product.takedown"));

    // --- Double takedown is a 409, and writes nothing more. ---
    let (status, body) = call(
        &app,
        Method::POST,
        &takedown_uri,
        Some(json!({ "note": "আবার কারণ" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "double takedown: {body}");
    assert_eq!(audit_count(&pool, &product, "product.takedown").await, 1);
    assert_eq!(notice_count(&pool, "product_taken_down").await, 1);

    // --- Restore requires its note too. ---
    let (status, body) = call(
        &app,
        Method::POST,
        &restore_uri,
        Some(json!({})),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "noteless restore: {body}");
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert!(detail["admin_hidden_at"].is_string(), "still down: {detail}");

    // --- A valid restore clears the row; the audit keeps the record. ---
    let (status, body) = call(
        &app,
        Method::POST,
        &restore_uri,
        Some(json!({ "note": "বিক্রেতা আসল পণ্যের প্রমাণ দিয়েছেন" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "restore: {body}");
    assert_eq!(body["admin_hidden_at"], Value::Null);
    assert_eq!(body["admin_hidden_reason"], Value::Null);
    assert_eq!(body["status"], json!("active"));
    assert_eq!(audit_count(&pool, &product, "product.restore").await, 1);
    assert_eq!(notice_count(&pool, "product_restored").await, 1);
    let restore_detail: String = sqlx::query_scalar(
        "SELECT detail::text FROM admin_audit_log \
         WHERE target_type = 'product' AND target_id = $1 AND action = 'product.restore'",
    )
    .bind(&product)
    .fetch_one(&pool)
    .await
    .expect("restore audit detail");
    assert!(
        restore_detail.contains(reason),
        "restore detail preserves the outgoing reason: {restore_detail}"
    );
    assert!(
        restore_detail.contains("বিক্রেতা আসল পণ্যের প্রমাণ দিয়েছেন"),
        "restore detail carries the note: {restore_detail}"
    );

    // --- Double restore is a 409. ---
    let (status, body) = call(
        &app,
        Method::POST,
        &restore_uri,
        Some(json!({ "note": "আবার নোট" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "double restore: {body}");

    // --- Takedown of a DRAFT is allowed; restore returns it to draft. ---
    let draft = create_product_with_status(
        &app,
        &seller,
        &store,
        "Bravo Draft",
        50_000,
        2,
        "draft",
    )
    .await;
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/products/{draft}/takedown"),
        Some(json!({ "note": "খসড়াতেই সমস্যা ধরা পড়েছে" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "draft takedown: {body}");
    assert_eq!(body["status"], json!("draft"));
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/products/{draft}/restore"),
        Some(json!({ "note": "ভুল বোঝাবুঝি ছিল" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "draft restore: {body}");
    assert_eq!(body["status"], json!("draft"), "restore never republishes: {body}");
    assert_eq!(body["admin_hidden_at"], Value::Null);
}
