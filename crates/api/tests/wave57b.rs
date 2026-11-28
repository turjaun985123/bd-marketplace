//! Wave 57 integration test: bulk import dry-run and
//! re-import idempotency.
//! - a dry run reports would_create/would_update, writes
//!   NO products, but IS recorded in the import history;
//! - re-importing the same sku UPDATES price and stock
//!   instead of duplicating; the stock move lands a
//!   stock_adjustments row (reason count_fix) in the same
//!   flow — the Wave 23 single stock path;
//! - an update does NOT re-notify followers (a re-import
//!   is a sync, not a publish);
//! - a sku claimed by a facebook-source product is a
//!   per-row error, never a hijack;
//! - a duplicated sku inside one file errors the later
//!   row at the domain layer (also covered there) — here
//!   the report shape is what matters.
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
async fn wave57b_dry_run_writes_nothing_and_reimport_updates_in_place() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-reimport").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let follower = login(&app, "01733333333").await;
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/stores/w57-reimport/follow",
        None,
        Some(&follower),
    )
    .await;
    assert!(status.is_success(), "follow: {status}");

    let sheet_v1 = [
        HEADER.to_string(),
        row(["B-1", "Widget One", "", "", "100000", "", "10", "", "active", "", "", ""]),
        row(["B-2", "Widget Two", "", "", "50000", "", "4", "", "draft", "", "", ""]),
    ]
    .join("\n");

    // --- Dry run: full report, zero writes, history row.
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_id}&dry_run=true"),
        &sheet_v1,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dry run: {report}");
    assert_eq!(report["dry_run"], json!(true));
    assert_eq!(report["created"], json!(2), "would-create counts as created: {report}");
    let actions: Vec<&str> = report["outcomes"]
        .as_array()
        .expect("outcomes")
        .iter()
        .map(|outcome| outcome["action"].as_str().expect("action"))
        .collect();
    assert_eq!(actions, vec!["would_create", "would_create"]);
    let product_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE store_id = $1::uuid")
            .bind(&store_id)
            .fetch_one(&pool)
            .await
            .expect("count products");
    assert_eq!(product_count, 0, "a dry run writes no products");
    let history_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM bulk_imports WHERE store_id = $1::uuid AND dry_run")
            .bind(&store_id)
            .fetch_one(&pool)
            .await
            .expect("count history");
    assert_eq!(history_rows, 1, "the dry run is recorded");

    // --- Real run: creates both.
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_id}"),
        &sheet_v1,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "real run: {report}");
    assert_eq!(report["created"], json!(2));
    let widget_one = report["outcomes"][0]["product_id"]
        .as_str()
        .expect("product id")
        .to_string();

    // The follower got exactly one notice (Widget One is
    // active; Widget Two is a draft).
    let follower_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE phone = '+8801733333333'")
            .fetch_one(&pool)
            .await
            .expect("follower row");
    async fn notice_count(pool: &PgPool, follower_id: uuid::Uuid) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND kind = 'store_new_product'",
        )
        .bind(follower_id)
        .fetch_one(pool)
        .await
        .expect("count notices")
    }
    assert_eq!(notice_count(&pool, follower_id).await, 1);

    // --- Re-import with a new price and stock: updates.
    let sheet_v2 = [
        HEADER.to_string(),
        row(["B-1", "Widget One", "", "", "120000", "", "17", "", "active", "", "", ""]),
        row(["B-2", "Widget Two", "", "", "50000", "", "4", "", "draft", "", "", ""]),
    ]
    .join("\n");
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_id}"),
        &sheet_v2,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-import: {report}");
    assert_eq!(report["created"], json!(0));
    assert_eq!(report["updated"], json!(2));
    assert_eq!(
        report["outcomes"][0]["product_id"],
        json!(widget_one),
        "the same product is updated, not duplicated"
    );
    let product_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE store_id = $1::uuid")
            .bind(&store_id)
            .fetch_one(&pool)
            .await
            .expect("count products");
    assert_eq!(product_count, 2, "still exactly two products");

    let (price, stock): (i64, i32) = sqlx::query_as(
        "SELECT price_poisha, stock FROM products WHERE id = $1::uuid",
    )
    .bind(&widget_one)
    .fetch_one(&pool)
    .await
    .expect("updated row");
    assert_eq!(price, 120_000);
    assert_eq!(stock, 17);

    // The stock move (10 -> 17) wrote its audit row
    // through the Wave 23 writer: delta +7, count_fix.
    let (delta, reason): (i64, String) = sqlx::query_as(
        "SELECT delta, reason FROM stock_adjustments WHERE product_id = $1::uuid",
    )
    .bind(&widget_one)
    .fetch_one(&pool)
    .await
    .expect("adjustment row");
    assert_eq!(delta, 7);
    assert_eq!(reason, "count_fix");

    // The update did NOT re-notify the follower.
    assert_eq!(notice_count(&pool, follower_id).await, 1, "updates never notify");

    // --- A sku claimed by another source errors per-row.
    // Seed a facebook-source product directly (the
    // import's own shape: source + source_ref on the row).
    sqlx::query(
        "INSERT INTO products (store_id, title, price_poisha, stock, status, \
         weight_grams, source, source_ref) \
         VALUES ($1::uuid, 'FB Product', 1000, 1, 'active', 1000, 'facebook', 'FB-9')",
    )
    .bind(&store_id)
    .execute(&pool)
    .await
    .expect("seed facebook product");
    let sheet_v3 = [
        HEADER.to_string(),
        row(["FB-9", "Hijack Attempt", "", "", "99000", "", "1", "", "active", "", "", ""]),
        row(["B-3", "Widget Three", "", "", "30000", "", "2", "", "active", "", "", ""]),
    ]
    .join("\n");
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_id}"),
        &sheet_v3,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "collision run: {report}");
    assert_eq!(report["created"], json!(1), "the clean row still lands");
    assert_eq!(report["skipped"], json!(1));
    let hijack = &report["outcomes"][0];
    assert_eq!(hijack["action"], json!("error"));
    assert!(
        hijack["errors"][0]["message"]
            .as_str()
            .expect("message")
            .contains("already used"),
        "{hijack}"
    );
    // The facebook product is untouched.
    let fb_price: i64 = sqlx::query_scalar(
        "SELECT price_poisha FROM products WHERE store_id = $1::uuid AND source_ref = 'FB-9'",
    )
    .bind(&store_id)
    .fetch_one(&pool)
    .await
    .expect("fb product");
    assert_eq!(fb_price, 1000);
}
