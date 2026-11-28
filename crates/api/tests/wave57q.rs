//! Wave 57 integration test: the plan cap in a dry run.
//! A dry run SIMULATES the cap budget: at 48 of 50, a
//! five-row dry run reports two would_create and three
//! cap errors — and spends nothing, so the identical
//! real run right after lands exactly the same two
//! creations (the dry run's prediction is the real
//! run's result, which is the point of a dry run).
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
async fn wave57q_dry_run_simulates_the_plan_cap_without_spending_it() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-cap-dry").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // Fill to 48 of the free plan's 50 in one import.
    let mut filler = vec![HEADER.to_string()];
    for index in 0..48 {
        let sku = format!("F-{index}");
        filler.push(row([
            &sku, "Filler", "", "", "10000", "", "1", "", "draft", "", "", "",
        ]));
    }
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_id}"),
        &filler.join("\n"),
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "filler import: {report}");
    assert_eq!(report["created"], json!(48));

    let sheet = [
        HEADER.to_string(),
        row(["Q-1", "New One", "", "", "10000", "", "1", "", "draft", "", "", ""]),
        row(["Q-2", "New Two", "", "", "10000", "", "1", "", "draft", "", "", ""]),
        row(["Q-3", "New Three", "", "", "10000", "", "1", "", "draft", "", "", ""]),
        row(["Q-4", "New Four", "", "", "10000", "", "1", "", "draft", "", "", ""]),
        row(["Q-5", "New Five", "", "", "10000", "", "1", "", "draft", "", "", ""]),
    ]
    .join("\n");
    let uri = format!("/v1/seller/products/bulk-import?store_id={store_id}");

    // The dry run predicts: 2 would-create, 3 cap errors.
    let (status, dry) = call_csv(&app, &format!("{uri}&dry_run=true"), &sheet, &seller).await;
    assert_eq!(status, StatusCode::OK, "dry run: {dry}");
    assert_eq!(dry["created"], json!(2), "{dry}");
    assert_eq!(dry["skipped"], json!(3), "{dry}");
    let dry_actions: Vec<&str> = dry["outcomes"]
        .as_array()
        .expect("outcomes")
        .iter()
        .map(|outcome| outcome["action"].as_str().expect("action"))
        .collect();
    assert_eq!(
        dry_actions,
        vec!["would_create", "would_create", "error", "error", "error"]
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE store_id = $1::uuid")
        .bind(&store_id)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count, 48, "the dry run spent nothing");

    // The real run lands exactly the prediction.
    let (status, real) = call_csv(&app, &uri, &sheet, &seller).await;
    assert_eq!(status, StatusCode::OK, "real run: {real}");
    assert_eq!(real["created"], json!(2));
    assert_eq!(real["skipped"], json!(3));
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE store_id = $1::uuid")
        .bind(&store_id)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count, 50, "the store sits at the cap, as predicted");
}
