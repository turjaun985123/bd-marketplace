//! Wave 57 integration test: bulk access rules (Wave 34).
//! - a staff member whose role carries manage_catalog
//!   (every member role does) CAN import into the store
//!   they serve;
//! - a stranger seller importing into the owner's store
//!   is refused (404 — the access module's convention of
//!   not confirming the store exists for outsiders);
//! - a stranger's bulk edit reports every item as an
//!   error and changes nothing;
//! - a stranger cannot read the store's import history.
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
/// return (status, parsed body). The one helper the
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
async fn wave57i_staff_can_import_and_strangers_cannot() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let owner = login(&app, "01711111111").await;
    let store = create_store(&app, &owner, "w57-access").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let uri = format!("/v1/seller/products/bulk-import?store_id={store_id}");
    let sheet = [
        HEADER.to_string(),
        row(["I-1", "Access Widget", "", "", "10000", "", "1", "", "draft", "", "", ""]),
    ]
    .join("\n");

    // Seat a staff member (the Wave 34 pattern: the
    // target logs in FIRST so their user row exists).
    let staff = login(&app, "01822222222").await;
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/stores/{store_id}/members"),
        Some(json!({ "phone": "01822222222", "role": "staff" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "seat staff: {body}");

    // Staff carries manage_catalog: the import runs.
    let (status, report) = call_csv(&app, &uri, &sheet, &staff).await;
    assert_eq!(status, StatusCode::OK, "staff import: {report}");
    assert_eq!(report["created"], json!(1));
    let product_id = report["outcomes"][0]["product_id"]
        .as_str()
        .expect("product id")
        .to_string();

    // A stranger seller is refused at the access gate.
    let stranger = login(&app, "01799999999").await;
    let (status, body) = call_csv(&app, &uri, &sheet, &stranger).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger import: {body}");

    // The stranger's bulk edit errors every item and
    // changes nothing.
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({
            "product_ids": [product_id],
            "price": { "mode": "set", "price_poisha": 1 }
        })),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stranger edit report: {report}");
    assert_eq!(report["failed"], json!(1));
    assert_eq!(report["succeeded"], json!(0));
    let price: i64 = sqlx::query_scalar("SELECT price_poisha FROM products WHERE id = $1::uuid")
        .bind(&product_id)
        .fetch_one(&pool)
        .await
        .expect("price");
    assert_eq!(price, 10_000, "the stranger changed nothing");

    // The stranger cannot read the store's import
    // history; the owner can, and sees the staff run.
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/products/import-history?store_id={store_id}"),
        None,
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger history refused");
    let (status, history) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/products/import-history?store_id={store_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "owner history: {history}");
    let entries = history.as_array().expect("history entries");
    assert_eq!(entries.len(), 1, "only the completed staff run is recorded");
    assert_eq!(entries[0]["created_count"], json!(1));
}
