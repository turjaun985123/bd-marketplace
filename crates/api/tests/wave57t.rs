//! Wave 57 integration test: template auth and the
//! history list's filters.
//! - the template endpoint requires a signed-in seller
//!   (401 anonymously — it is a seller tool, not a public
//!   download);
//! - the history list honours `limit` and the `dry_run`
//!   filter (dry runs and real runs list separately);
//! - an unknown import id on the detail endpoint is a
//!   clean 404.
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

/// Send one raw CSV body to the bulk-import endpoint and
/// return (status, parsed report). The one helper the
/// shared `call` cannot express (it JSON-encodes its
/// body); the Wave 51 raw-body helper precedent.
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

/// GET a raw-text response (the template download). The
/// text sibling of `call_csv`, same precedent.
async fn call_text(app: &Router, uri: &str, token: Option<&str>) -> (StatusCode, String) {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let request = builder.body(Body::empty()).expect("request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router responds");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    (status, String::from_utf8(bytes.to_vec()).expect("utf8 body"))
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

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 57 integration tests");
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
         buyer_addresses, bulk_imports, \
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
    let state = AppState::new(Some(pool.clone()), "wave57-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// POST a store for the token's user; returns the store view.
async fn create_store(app: &Router, token: &str, slug: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body
}

/// GET the history list for a store and return its
/// length (the filter assertions only need the counts).
async fn history_len(app: &Router, store_id: &str, suffix: &str, token: &str) -> usize {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/seller/products/import-history?store_id={store_id}{suffix}"),
        None,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "history {suffix}: {body}");
    body.as_array().expect("entries").len()
}

const HEADER: &str = "sku,title,title_bn,description,price_poisha,compare_at_poisha,stock,weight_grams,status,category_slug,category_label,image_url";

/// One sheet row from its 12 cells (the domain's column
/// order) — no hand-counted commas.
fn row(cells: [&str; 12]) -> String {
    cells.join(",")
}

#[tokio::test]
async fn wave57t_template_auth_and_history_filters() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    // Anonymous template fetch: 401.
    let (status, _) = call_text(&app, "/v1/seller/products/import-template.csv", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "template needs a login");

    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-filters").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let uri = format!("/v1/seller/products/bulk-import?store_id={store_id}");
    let sheet = [
        HEADER.to_string(),
        row(["T-1", "Filter Widget", "", "", "10000", "", "1", "", "draft", "", "", ""]),
    ]
    .join("\n");

    // Two dry runs and one real run.
    for _ in 0..2 {
        let (status, _) = call_csv(&app, &format!("{uri}&dry_run=true"), &sheet, &seller).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _) = call_csv(&app, &uri, &sheet, &seller).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(history_len(&app, &store_id, "", &seller).await, 3, "all runs");
    assert_eq!(
        history_len(&app, &store_id, "&dry_run=true", &seller).await,
        2,
        "dry runs only"
    );
    assert_eq!(
        history_len(&app, &store_id, "&dry_run=false", &seller).await,
        1,
        "real runs only"
    );
    assert_eq!(
        history_len(&app, &store_id, "&limit=2", &seller).await,
        2,
        "the limit clamps the list"
    );

    // An unknown import id is a clean 404.
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/products/import-history/{}", uuid::Uuid::new_v4()),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown import id");
}
