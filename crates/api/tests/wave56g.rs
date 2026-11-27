//! Wave 56 integration test: taxonomy import edge cases.
//! - the whole export re-imports as pure updates (the
//!   round-trip is lossless at the report level);
//! - a document that would push a branch past depth 3
//!   aborts with NOTHING written;
//! - a duplicate slug inside one document aborts;
//! - an unknown icon key and an invalid slug abort;
//! - every abort leaves the taxonomy exactly as it was
//!   (the validate-before-write rule).
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

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

/// Login, THEN promote by SQL (login creates the user row).
async fn login_admin(app: &Router, pool: &PgPool, phone: &str) -> String {
    let token = login(app, phone).await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = $1")
        .bind(format!("+880{}", &phone[1..]))
        .execute(pool)
        .await
        .expect("promote admin");
    token
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 56 integration tests");
            return None;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         buyer_addresses, \
         product_images, product_variants, products, store_members, store_follows, \
         stores, otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave56-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// Find a category's admin view by slug in the admin list.
async fn admin_category(app: &Router, admin: &str, slug: &str) -> Option<Value> {
    let (status, list) = call(app, Method::GET, "/v1/admin/categories", None, Some(admin)).await;
    assert_eq!(status, StatusCode::OK, "admin list: {list}");
    list["items"]
        .as_array()
        .expect("admin items")
        .iter()
        .find(|item| item["slug"] == slug)
        .cloned()
}

/// POST an import document; returns (status, body).
async fn import(app: &Router, admin: &str, document: Value) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/admin/categories/import",
        Some(document),
        Some(admin),
    )
    .await
}

#[tokio::test]
async fn wave56_import_edge_cases() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let admin = login_admin(&app, &pool, "01711111111").await;

    // 1. The round-trip: exporting the whole taxonomy
    // and importing it straight back creates NOTHING and
    // updates every row — the document format is a
    // lossless description of the current tree.
    let (status, export) = call(
        &app,
        Method::GET,
        "/v1/admin/categories/export",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "export: {export}");
    let row_count = export["categories"].as_array().expect("export rows").len();
    assert!(row_count >= 53, "the seed alone is 53 rows: {row_count}");
    let (status, report) = import(&app, &admin, export).await;
    assert_eq!(status, StatusCode::OK, "round-trip import: {report}");
    assert_eq!(report["created"], 0, "round-trip creates nothing");
    assert_eq!(
        report["updated"].as_i64().expect("updated"),
        row_count as i64,
        "every exported row updates in place"
    );

    // 2. Depth: panjabi already sits at depth 3, so a
    // document hanging a child under it aborts — and the
    // child is NOT left behind.
    let (status, err) = import(
        &app,
        &admin,
        json!({
            "categories": [
                { "slug": "w56g-too-deep", "name_en": "W56G Too Deep", "parent_slug": "panjabi" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "depth import: {err}");
    assert!(admin_category(&app, &admin, "w56g-too-deep").await.is_none());

    // 3. A duplicate slug inside ONE document aborts,
    // even though each row alone would be valid.
    let (status, err) = import(
        &app,
        &admin,
        json!({
            "categories": [
                { "slug": "w56g-dupe", "name_en": "W56G Dupe One" },
                { "slug": "w56g-dupe", "name_en": "W56G Dupe Two" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "duplicate import: {err}");
    assert!(admin_category(&app, &admin, "w56g-dupe").await.is_none());

    // 4. An unknown icon key aborts (icons are keys into
    // the built-in set, validated like the CRUD path).
    let (status, err) = import(
        &app,
        &admin,
        json!({
            "categories": [
                { "slug": "w56g-icon", "name_en": "W56G Icon", "icon_key": "rocket" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "icon import: {err}");
    assert!(admin_category(&app, &admin, "w56g-icon").await.is_none());

    // 5. An invalid slug aborts.
    let (status, err) = import(
        &app,
        &admin,
        json!({
            "categories": [
                { "slug": "W56G Bad Slug", "name_en": "W56G Bad" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "slug import: {err}");

    // 6. A document mixing one valid row with one broken
    // row writes NOTHING — the valid row does not land.
    let (status, err) = import(
        &app,
        &admin,
        json!({
            "categories": [
                { "slug": "w56g-valid", "name_en": "W56G Valid" },
                { "slug": "w56g-broken", "name_en": "W56G Broken", "parent_slug": "w56g-ghost" }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "mixed import: {err}");
    assert!(admin_category(&app, &admin, "w56g-valid").await.is_none());
    assert!(admin_category(&app, &admin, "w56g-broken").await.is_none());

    // 7. The import is audited like every admin write.
    let (status, log) = call(
        &app,
        Method::GET,
        "/v1/admin/audit-log?action=category.import",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "audit log: {log}");
    assert!(
        log["items"]
            .as_array()
            .expect("audit items")
            .iter()
            .any(|entry| entry["action"] == "category.import"),
        "the round-trip import was audited: {log}"
    );
}
