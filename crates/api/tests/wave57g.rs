//! Wave 57 integration test: the export -> import
//! round-trip. A store's catalogue exports in the
//! import's own column format; importing that sheet into
//! a SECOND store recreates the catalogue there (same
//! skus, titles, prices, category links), and the second
//! store's export carries the same product set. A
//! stranger cannot export a store they do not manage.
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
async fn call_text(
    app: &Router,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, String) {
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

/// The (sku, title, price) set of one store, from the DB.
async fn catalogue_set(pool: &PgPool, store_id: &str) -> Vec<(String, String, i64)> {
    sqlx::query_as(
        "SELECT source_ref, title, price_poisha FROM products \
         WHERE store_id = $1::uuid ORDER BY source_ref",
    )
    .bind(store_id)
    .fetch_all(pool)
    .await
    .expect("catalogue set")
}

#[tokio::test]
async fn wave57g_export_import_round_trip_recreates_the_catalogue() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store_one = create_store(&app, &seller, "w57-round-one").await;
    let store_one_id = store_one["id"].as_str().expect("store id").to_string();
    let store_two = create_store(&app, &seller, "w57-round-two").await;
    let store_two_id = store_two["id"].as_str().expect("store id").to_string();

    // Seed store one through the import itself (one
    // slug-linked product, one labelled, one plain).
    let sheet = [
        HEADER.to_string(),
        row(["G-1", "Round Trip Panjabi", "পাঞ্জাবি", "Soft cotton", "185000", "220000", "25", "400", "active", "panjabi", "", ""]),
        row(["G-2", "Round Trip Rice", "", "", "62000", "", "120", "5000", "active", "", "Rice & Grains", ""]),
        row(["G-3", "Round Trip Plain", "", "", "10000", "", "1", "", "draft", "", "", ""]),
    ]
    .join("\n");
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_one_id}"),
        &sheet,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seed import: {report}");
    assert_eq!(report["created"], json!(3));

    // Export store one: the sheet carries the header and
    // all three skus.
    let (status, exported) = call_text(
        &app,
        &format!("/v1/seller/products/export.csv?store_id={store_one_id}"),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "export: {exported}");
    assert!(exported.starts_with("sku,title,"), "{exported}");
    for sku in ["G-1", "G-2", "G-3"] {
        assert!(exported.contains(sku), "export carries {sku}: {exported}");
    }
    assert!(exported.contains("panjabi"), "the slug column rides: {exported}");

    // Import the export into store two: everything is a
    // creation there, and the catalogue sets match.
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_two_id}"),
        &exported,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "round-trip import: {report}");
    assert_eq!(report["created"], json!(3), "{report}");
    assert_eq!(report["skipped"], json!(0), "{report}");
    assert_eq!(
        catalogue_set(&pool, &store_one_id).await,
        catalogue_set(&pool, &store_two_id).await,
        "same sku/title/price set after the round-trip"
    );

    // The category links survived the trip: the
    // slug-linked panjabi AND the label-linked rice
    // ("Rice & Grains" is the rice-grains node's English
    // name, so it linked on import and exported with its
    // slug) are linked in store two; the plain row is not.
    let linked: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM products WHERE store_id = $1::uuid AND category_id IS NOT NULL",
    )
    .bind(&store_two_id)
    .fetch_one(&pool)
    .await
    .expect("linked count");
    assert_eq!(linked, 2, "both linked products keep their links");

    // A stranger cannot export someone else's catalogue.
    let stranger = login(&app, "01722222222").await;
    let (status, _) = call_text(
        &app,
        &format!("/v1/seller/products/export.csv?store_id={store_one_id}"),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger export refused");
}
