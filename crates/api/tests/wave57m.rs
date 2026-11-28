//! Wave 57 integration test: export details.
//! - an empty store exports just the header row;
//! - a manually created product (no source_ref) exports
//!   with a BLANK sku — and importing that row back
//!   errors per-row on the missing sku, exactly the
//!   documented caveat (assign a sku in the sheet first);
//! - a product whose primary image is a hotlink exports
//!   that URL in the image_url column, so the round-trip
//!   carries it.
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

/// GET a raw-text response (the export download). The
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

const HEADER: &str = "sku,title,title_bn,description,price_poisha,compare_at_poisha,stock,weight_grams,status,category_slug,category_label,image_url";

/// One sheet row from its 12 cells (the domain's column
/// order) — no hand-counted commas.
fn row(cells: [&str; 12]) -> String {
    cells.join(",")
}

#[tokio::test]
async fn wave57m_export_blank_sku_caveat_and_image_column() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-export").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let export_uri = format!("/v1/seller/products/export.csv?store_id={store_id}");

    // Empty store: the export is exactly the header.
    let (status, exported) = call_text(&app, &export_uri, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "empty export: {exported}");
    assert_eq!(exported.trim_end(), HEADER, "header only: {exported:?}");

    // A manual product (the seller form — no sku) and a
    // bulk product with a hotlinked primary image.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": "Manual Product",
            "price_poisha": 42_000,
            "stock": 3,
            "status": "active"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "manual create: {body}");
    let sheet = [
        HEADER.to_string(),
        row(["M-1", "Imaged Product", "", "", "10000", "", "1", "", "active", "", "", "https://example.com/hero.jpg"]),
    ]
    .join("\n");
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_id}"),
        &sheet,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "import: {report}");
    assert_eq!(report["created"], json!(1));

    // The export now carries both rows: the manual one
    // with a leading empty sku cell, the bulk one with
    // its sku and its image URL.
    let (status, exported) = call_text(&app, &export_uri, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "export: {exported}");
    let lines: Vec<&str> = exported.lines().collect();
    assert_eq!(lines.len(), 3, "header + two products: {exported}");
    let manual_line = lines
        .iter()
        .find(|line| line.contains("Manual Product"))
        .expect("manual row");
    assert!(
        manual_line.starts_with(','),
        "the manual product's sku cell is blank: {manual_line}"
    );
    let imaged_line = lines
        .iter()
        .find(|line| line.contains("Imaged Product"))
        .expect("imaged row");
    assert!(imaged_line.starts_with("M-1,"), "{imaged_line}");
    assert!(
        imaged_line.contains("https://example.com/hero.jpg"),
        "the primary image URL rides the export: {imaged_line}"
    );

    // The documented caveat, pinned: re-importing the
    // export into a fresh store creates the sku'd row
    // and errors the blank-sku row — visibly, per-row.
    let store_two = create_store(&app, &seller, "w57-export-two").await;
    let store_two_id = store_two["id"].as_str().expect("store id").to_string();
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_two_id}"),
        &exported,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-import: {report}");
    assert_eq!(report["created"], json!(1), "{report}");
    assert_eq!(report["skipped"], json!(1), "{report}");
    let error_outcome = report["outcomes"]
        .as_array()
        .expect("outcomes")
        .iter()
        .find(|outcome| outcome["action"] == json!("error"))
        .expect("one error outcome")
        .clone();
    assert_eq!(error_outcome["errors"][0]["field"], json!("sku"), "{error_outcome}");
    let in_store_two: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE store_id = $1::uuid")
            .bind(&store_two_id)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(in_store_two, 1, "no silent duplicate of the manual product");
}
