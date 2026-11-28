//! Wave 57 integration test: bulk import error handling.
//! - per-row errors never abort the batch: a sheet mixing
//!   broken rows (bad price, bad status, unknown category
//!   slug, zero price on an active row) with good rows
//!   lands the good rows and reports every broken row
//!   with its spreadsheet row number and field;
//! - fatal file problems refuse the whole request with a
//!   400 and write nothing: an unknown header column, a
//!   missing required column, and a sheet over the
//!   500-row cap.
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
async fn wave57c_row_errors_never_abort_and_fatals_refuse_the_file() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-errors").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let uri = format!("/v1/seller/products/bulk-import?store_id={store_id}");

    // Rows: 2 good, 3 broken in different ways, 1 good.
    // Spreadsheet numbers: header = 1, first data row = 2.
    let sheet = [
        HEADER.to_string(),
        row(["C-1", "Good One", "", "", "10000", "", "1", "", "active", "", "", ""]),
        row(["C-2", "Bad Price", "", "", "ten", "", "1", "", "active", "", "", ""]),
        row(["C-3", "Bad Status", "", "", "10000", "", "1", "", "bogus", "", "", ""]),
        row(["C-4", "Unknown Category", "", "", "10000", "", "1", "", "active", "no-such-cat", "", ""]),
        row(["C-5", "Zero Price Active", "", "", "0", "", "1", "", "active", "", "", ""]),
        row(["C-6", "Good Two", "", "", "20000", "", "2", "", "draft", "", "", ""]),
    ]
    .join("\n");
    let (status, report) = call_csv(&app, &uri, &sheet, &seller).await;
    assert_eq!(status, StatusCode::OK, "mixed sheet: {report}");
    assert_eq!(report["total_rows"], json!(6));
    assert_eq!(report["created"], json!(2), "the two good rows land: {report}");
    assert_eq!(report["skipped"], json!(4));
    let outcomes = report["outcomes"].as_array().expect("outcomes");
    assert_eq!(outcomes.len(), 6);
    // Outcomes come back in file order with sheet row numbers.
    let row_numbers: Vec<i64> = outcomes
        .iter()
        .map(|outcome| outcome["row_number"].as_i64().expect("row number"))
        .collect();
    assert_eq!(row_numbers, vec![2, 3, 4, 5, 6, 7]);
    let error_fields = |index: usize| -> Vec<String> {
        outcomes[index]["errors"]
            .as_array()
            .expect("errors")
            .iter()
            .map(|error| error["field"].as_str().expect("field").to_string())
            .collect()
    };
    assert!(error_fields(1).contains(&"price_poisha".to_string()), "{outcomes:?}");
    assert!(error_fields(2).contains(&"status".to_string()), "{outcomes:?}");
    assert!(error_fields(3).contains(&"category_slug".to_string()), "{outcomes:?}");
    assert!(error_fields(4).contains(&"price_poisha".to_string()), "{outcomes:?}");
    let product_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE store_id = $1::uuid")
            .bind(&store_id)
            .fetch_one(&pool)
            .await
            .expect("count products");
    assert_eq!(product_count, 2);

    // Fatal: an unknown header column refuses everything.
    let bad_header = "sku,title,price_poisha,colour\nX-1,Thing,100,red\n";
    let (status, body) = call_csv(&app, &uri, bad_header, &seller).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown column: {body}");
    assert!(
        body["error"].as_str().expect("error").contains("colour"),
        "{body}"
    );

    // Fatal: a missing required column.
    let missing = "sku,title\nX-1,Thing\n";
    let (status, body) = call_csv(&app, &uri, missing, &seller).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "missing column: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error")
            .contains("price_poisha"),
        "{body}"
    );

    // Fatal: over the 500-row cap.
    let mut big = String::from("sku,title,price_poisha\n");
    for index in 0..501 {
        big.push_str(&format!("BIG-{index},Thing,100\n"));
    }
    let (status, body) = call_csv(&app, &uri, &big, &seller).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "over cap: {body}");

    // None of the fatal attempts wrote anything, and none
    // was recorded in the history (there is no report).
    let product_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE store_id = $1::uuid")
            .bind(&store_id)
            .fetch_one(&pool)
            .await
            .expect("count products");
    assert_eq!(product_count, 2, "fatal files write nothing");
    let history_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM bulk_imports WHERE store_id = $1::uuid")
            .bind(&store_id)
            .fetch_one(&pool)
            .await
            .expect("count history");
    assert_eq!(history_rows, 1, "only the completed run is recorded");
}
