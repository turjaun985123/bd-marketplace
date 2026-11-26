//! Wave 32 integration test: the performance pass. Three
//! behaviours are pinned here:
//!
//! 1. Migration 0030's indexes exist after migrations run —
//!    the four query shapes that had no index (order_items by
//!    product, shipments by status + updated_at, sub_orders by
//!    store + status, product questions by product + hidden)
//!    now do. Asserted against `pg_indexes`, the catalogue the
//!    planner itself reads.
//! 2. Cache headers: the anonymous public catalogue reads
//!    (product list, product detail, search) carry
//!    `Cache-Control: public, max-age=30`; an authenticated
//!    catalogue read does NOT (the payload is viewer-dependent
//!    — owners see their own drafts), and an authenticated
//!    orders read never carries a public cache header.
//! 3. Gzip: a request with `Accept-Encoding: gzip` gets a
//!    `Content-Encoding: gzip` response; a request without it
//!    is untouched. The body bytes are asserted non-empty
//!    only — no gzip decoder is in the dev-dependencies, so
//!    decompression is not asserted here (the header contract
//!    is what this wave ships).
//!
//! The gzip test needs no database (compression is middleware
//! around `/health`), so it runs everywhere. The index and
//! cache-header test is gated on `DATABASE_URL`: without it
//! the test skips cleanly so a plain local `cargo test` (no
//! database) still passes. CI provides a Postgres 16 service
//! and sets `DATABASE_URL` for the rust job.
//!
//! The small helpers are copied from the Wave 9 test on
//! purpose, since integration test files stay self-contained.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
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

/// Like `call`, but sends extra request headers and returns
/// the response headers plus the raw body bytes — the shape
/// header assertions (cache-control, content-encoding) need.
async fn call_headers(
    app: &Router,
    method: Method,
    uri: &str,
    extra_headers: &[(&str, &str)],
    token: Option<&str>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::empty()).expect("request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    (status, headers, bytes.to_vec())
}

/// Read one response header as a string, if present.
fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
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

/// Create an active product; returns the product id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

#[tokio::test]
async fn wave32_gzip_compression_is_opt_in_per_request() {
    // No pool: compression is middleware and `/health` never
    // touches the database, so this half runs everywhere.
    let state = AppState::new(None, "wave32-test-secret".to_string(), true);
    let app = build_app(state);

    // With `Accept-Encoding: gzip`: a gzipped response.
    let (status, headers, bytes) = call_headers(
        &app,
        Method::GET,
        "/health",
        &[("Accept-Encoding", "gzip")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "health with gzip");
    assert_eq!(
        header_value(&headers, "content-encoding").as_deref(),
        Some("gzip"),
        "gzip negotiated: {headers:?}"
    );
    assert!(!bytes.is_empty(), "gzipped body is not empty");

    // Without the header: the plain response, no encoding.
    let (status, headers, bytes) =
        call_headers(&app, Method::GET, "/health", &[], None).await;
    assert_eq!(status, StatusCode::OK, "plain health");
    assert_eq!(
        header_value(&headers, "content-encoding"),
        None,
        "no content-encoding without Accept-Encoding: {headers:?}"
    );
    let parsed: Value = serde_json::from_slice(&bytes).expect("plain body is JSON");
    assert_eq!(parsed["status"], "ok", "plain health body: {parsed}");
}

#[tokio::test]
async fn wave32_indexes_and_cache_headers() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 32 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "wave32-test-secret".to_string(), true);
    let app = build_app(state);

    // 1. Migration 0030's four indexes are in the catalogue.
    let index_names: Vec<String> =
        sqlx::query_scalar("SELECT indexname FROM pg_indexes WHERE schemaname = 'public'")
            .fetch_all(&pool)
            .await
            .expect("read pg_indexes");
    for expected in [
        "idx_order_items_product",
        "idx_shipments_status_updated",
        "idx_sub_orders_store_status",
        "idx_product_questions_product_hidden",
    ] {
        assert!(
            index_names.iter().any(|name| name == expected),
            "missing index {expected}; catalogue: {index_names:?}"
        );
    }

    // One seller with one active product to read publicly.
    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave32-store").await;
    let product = create_product(&app, &seller, &store, "Wave32 Product", 150_000, 5).await;
    let buyer = login(&app, "01999999999").await;

    // 2. Anonymous catalogue reads carry the public cache header.
    for uri in [
        "/v1/products".to_string(),
        format!("/v1/products/{product}"),
        "/v1/search?q=Wave32".to_string(),
    ] {
        let (status, headers, _bytes) = call_headers(&app, Method::GET, &uri, &[], None).await;
        assert_eq!(status, StatusCode::OK, "GET {uri}");
        assert_eq!(
            header_value(&headers, "cache-control").as_deref(),
            Some("public, max-age=30"),
            "GET {uri} cache header: {headers:?}"
        );
    }

    // 3. The same list WITH a token: the payload is
    // viewer-dependent, so no public cache header.
    let (status, headers, _bytes) =
        call_headers(&app, Method::GET, "/v1/products", &[], Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "authenticated product list");
    let cache = header_value(&headers, "cache-control").unwrap_or_default();
    assert!(
        !cache.contains("public"),
        "authenticated list must not be publicly cacheable: {cache}"
    );

    // 4. An authenticated orders read never carries a public
    // cache header (absent entirely is the expected shape).
    let (status, headers, _bytes) =
        call_headers(&app, Method::GET, "/v1/orders", &[], Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer orders");
    let cache = header_value(&headers, "cache-control").unwrap_or_default();
    assert!(
        !cache.contains("public"),
        "orders must not be publicly cacheable: {cache}"
    );

    // 5. Gzip on a real catalogue payload: the encoding is
    // negotiated and the body is a non-empty gzip stream (not
    // asserted further — no decoder in the dev-dependencies).
    let (status, headers, bytes) = call_headers(
        &app,
        Method::GET,
        "/v1/products",
        &[("Accept-Encoding", "gzip")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "gzipped product list");
    assert_eq!(
        header_value(&headers, "content-encoding").as_deref(),
        Some("gzip"),
        "gzip negotiated on /v1/products: {headers:?}"
    );
    assert!(!bytes.is_empty(), "gzipped body is not empty");
    assert_eq!(
        &bytes[..2],
        &[0x1f, 0x8b],
        "body starts with the gzip magic bytes"
    );
}
