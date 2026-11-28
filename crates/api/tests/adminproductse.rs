//! Admin products remake, test E: the seller's own world
//! keeps working on a taken-down product — and cannot
//! defeat the takedown. The seller can still EDIT the
//! listing (title fix is the expected remediation) and
//! adjust its stock; a status flip back to `active` does
//! NOT clear the platform flag and fires NO follower
//! new-product notice; a bulk re-import of the product's
//! SKU is refused per-row (the export → edit → re-import
//! round-trip must not become a resurrection machine);
//! and the seller's data export still contains the
//! product (their own records are not the platform's to
//! hide).
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

/// Send one raw CSV body to the bulk-import endpoint and
/// return (status, parsed report) — the wave57 helper.
async fn call_csv(
    app: &Router,
    uri: &str,
    csv: &str,
    token: &str,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "text/csv")
        .body(Body::from(csv.to_string()))
        .expect("request builds");
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

/// GET a raw text body (the catalogue CSV export).
async fn call_text(
    app: &Router,
    uri: &str,
    token: &str,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router responds");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    (status, String::from_utf8_lossy(&bytes).to_string())
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
async fn adminproducts_seller_world_survives_takedown() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin products remake test E");
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
    let state = AppState::new(
        Some(pool.clone()),
        "adminproductse-test-secret".to_string(),
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
    let store = create_store(&app, &seller, "aprod-echo").await;

    // A follower watches the store, so a publish that
    // wrongly notified would be visible in the outbox.
    let follower = login(&app, "01744444444").await;
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/aprod-echo/follow",
        None,
        Some(&follower),
    )
    .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "follow: {status} {body}"
    );

    // The product is born through BULK IMPORT (so it owns
    // the SKU the round-trip test needs), as a DRAFT.
    let header = "sku,title,title_bn,description,price_poisha,compare_at_poisha,stock,weight_grams,status,category_slug,category_label,image_url";
    let row = |cells: [&str; 12]| cells.join(",");
    let csv = [
        header.to_string(),
        row(["E-1", "Echo Mixer", "", "", "175000", "", "6", "", "", "", "", ""]),
    ]
    .join("\n")
        + "\n";
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store}"),
        &csv,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed import: {report}");
    assert_eq!(report["created"], json!(1), "seed import: {report}");
    let product = report["outcomes"][0]["product_id"]
        .as_str()
        .expect("imported product id")
        .to_string();

    // Take the DRAFT down.
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/products/{product}/takedown"),
        Some(json!({ "note": "বর্ণনায় ভুল দাবি" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "takedown: {body}");

    // The seller can still EDIT it (the remediation path) —
    // and the edit does not clear the flag.
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/products/{product}"),
        Some(json!({ "title": "Echo Mixer (সংশোধিত)" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller edit while down: {body}");
    assert!(body["admin_hidden_at"].is_string(), "flag survives edit: {body}");
    assert_eq!(body["admin_hidden_reason"], json!("বর্ণনায় ভুল দাবি"));

    // The seller can still adjust its stock.
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/products/{product}/adjust-stock"),
        Some(json!({ "delta": 2, "reason": "count_fix" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "adjust stock while down: {body}");

    // Flipping it to active does NOT clear the flag, does
    // NOT make it public, and fires NO follower notice.
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/products/{product}"),
        Some(json!({ "status": "active" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller publish while down: {body}");
    assert!(body["admin_hidden_at"].is_string(), "flag survives publish: {body}");
    let (status, body) = call(&app, Method::GET, &format!("/v1/products/{product}"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "still hidden publicly: {body}");
    let new_product_notices: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE kind = 'store_new_product'",
    )
    .fetch_one(&pool)
    .await
    .expect("count new-product notices");
    assert_eq!(new_product_notices, 0, "no follower notice for a hidden publish");

    // The bulk round-trip refuses the row and writes nothing.
    let csv2 = [
        header.to_string(),
        row(["E-1", "Echo Mixer Hijacked", "", "", "1000", "", "99", "", "active", "", "", ""]),
    ]
    .join("\n")
        + "\n";
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store}"),
        &csv2,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-import: {report}");
    assert_eq!(report["outcomes"][0]["action"], json!("error"), "re-import: {report}");
    let message = report["outcomes"][0]["errors"][0]["message"]
        .as_str()
        .unwrap_or("");
    assert!(
        message.contains("removed by the platform"),
        "re-import refusal wording: {report}"
    );
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "owner read after re-import: {body}");
    assert_eq!(body["title"], json!("Echo Mixer (সংশোধিত)"), "title unwritten: {body}");
    assert_eq!(body["price_poisha"], json!(175_000), "price unwritten: {body}");

    // The catalogue export still carries the SKU (the
    // seller's own records), and the data export still
    // contains the product.
    let (status, csv_text) = call_text(
        &app,
        &format!("/v1/seller/products/export.csv?store_id={store}"),
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "export.csv: {csv_text}");
    assert!(csv_text.contains("E-1"), "export carries the sku: {csv_text}");
    let (status, export) = call(&app, Method::GET, "/v1/seller/export", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "seller export: {export}");
    assert!(
        export.to_string().contains(&product),
        "data export contains the hidden product: {export}"
    );
}
