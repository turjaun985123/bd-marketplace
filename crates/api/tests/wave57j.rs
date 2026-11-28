//! Wave 57 integration test: the import template and
//! the import history endpoints.
//! - the template downloads as CSV carrying the domain's
//!   canonical header, and its two example rows dry-run
//!   cleanly (the template can never drift from the
//!   parser's contract — it is generated from it);
//! - the history lists real and dry runs with their
//!   counts, newest first;
//! - the detail endpoint returns the run's full stored
//!   report, outcomes included.
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

const HEADER: &str = "sku,title,title_bn,description,price_poisha,compare_at_poisha,stock,weight_grams,status,category_slug,category_label,image_url";

/// One sheet row from its 12 cells (the domain's column
/// order) — no hand-counted commas.
fn row(cells: [&str; 12]) -> String {
    cells.join(",")
}

#[tokio::test]
async fn wave57j_template_dry_runs_cleanly_and_history_reopens_reports() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-history").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // The template: canonical header, generated server-side.
    let (status, template) = call_text(
        &app,
        "/v1/seller/products/import-template.csv",
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "template: {template}");
    assert!(
        template.starts_with("sku,title,title_bn,description,price_poisha,"),
        "{template}"
    );
    assert!(template.contains("PANJABI-001"), "{template}");

    // Its example rows dry-run with zero errors (the
    // template's slugs are real taxonomy nodes).
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_id}&dry_run=true"),
        &template,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "template dry run: {report}");
    assert_eq!(report["created"], json!(2), "{report}");
    assert_eq!(report["skipped"], json!(0), "{report}");

    // One real run with a broken row mixed in.
    let sheet = [
        HEADER.to_string(),
        row(["J-1", "History Widget", "", "", "10000", "", "1", "", "active", "", "", ""]),
        row(["J-2", "Broken Widget", "", "", "oops", "", "1", "", "active", "", "", ""]),
    ]
    .join("\n");
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_id}"),
        &sheet,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "real run: {report}");
    let import_id = report["import_id"].as_str().expect("import id").to_string();
    assert_eq!(report["created"], json!(1));
    assert_eq!(report["skipped"], json!(1));

    // The history lists both runs, newest (the real run)
    // first, with counts and the dry-run flag.
    let (status, history) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/products/import-history?store_id={store_id}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "history: {history}");
    let entries = history.as_array().expect("entries");
    assert_eq!(entries.len(), 2, "{history}");
    // Both runs may share a created_at second, so the
    // assertions key on the runs' identities, not their
    // positions in the list.
    let real = entries
        .iter()
        .find(|entry| entry["id"] == json!(import_id))
        .expect("the real run is listed");
    assert_eq!(real["dry_run"], json!(false));
    assert_eq!(real["created_count"], json!(1));
    assert_eq!(real["error_count"], json!(1));
    let dry = entries
        .iter()
        .find(|entry| entry["dry_run"] == json!(true))
        .expect("the dry run is listed");
    assert_eq!(dry["created_count"], json!(2));

    // The detail endpoint re-opens the full stored
    // report — outcomes, row numbers and errors included.
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/products/import-history/{import_id}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["total_rows"], json!(2));
    let outcomes = detail["outcomes"].as_array().expect("outcomes");
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[1]["action"], json!("error"));
    assert_eq!(outcomes[1]["row_number"], json!(3));
    assert_eq!(outcomes[1]["errors"][0]["field"], json!("price_poisha"));
}
