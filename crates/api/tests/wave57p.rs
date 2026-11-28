//! Wave 57 integration test: the CSV grammar end to end.
//! A sheet as a real spreadsheet would save it — a UTF-8
//! BOM, CRLF line endings, quoted fields carrying commas
//! and an embedded newline, doubled quotes — imports
//! cleanly, and the stored values are the unescaped
//! originals. A second sheet with its columns shuffled
//! maps by header name, not position.
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

#[tokio::test]
async fn wave57p_spreadsheet_grammar_survives_the_import() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-grammar").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let uri = format!("/v1/seller/products/bulk-import?store_id={store_id}");

    // BOM + CRLF + quoted commas, a quoted embedded
    // newline and a doubled quote — as Excel saves it.
    let sheet = concat!(
        "\u{feff}sku,title,title_bn,description,price_poisha,compare_at_poisha,stock,weight_grams,status,category_slug,category_label,image_url\r\n",
        "P-1,\"Panjabi, Premium\",,\"Line one\nLine two, with comma\",185000,,25,400,active,,,\r\n",
        "P-2,\"He said \"\"soft\"\" cotton\",,,99000,,3,,draft,,,\r\n",
    );
    let (status, report) = call_csv(&app, &uri, sheet, &seller).await;
    assert_eq!(status, StatusCode::OK, "grammar sheet: {report}");
    assert_eq!(report["created"], json!(2), "{report}");
    assert_eq!(report["skipped"], json!(0), "{report}");

    let first_id = report["outcomes"][0]["product_id"]
        .as_str()
        .expect("product id")
        .to_string();
    let (title, description): (String, Option<String>) = sqlx::query_as(
        "SELECT title, description FROM products WHERE id = $1::uuid",
    )
    .bind(&first_id)
    .fetch_one(&pool)
    .await
    .expect("first product");
    assert_eq!(title, "Panjabi, Premium", "the quoted comma survived");
    assert_eq!(
        description.as_deref(),
        Some("Line one\nLine two, with comma"),
        "the embedded newline survived"
    );
    let second_id = report["outcomes"][1]["product_id"]
        .as_str()
        .expect("product id")
        .to_string();
    let title: String = sqlx::query_scalar("SELECT title FROM products WHERE id = $1::uuid")
        .bind(&second_id)
        .fetch_one(&pool)
        .await
        .expect("second product");
    assert_eq!(title, "He said \"soft\" cotton", "the doubled quote unescaped");

    // Shuffled columns map by header name.
    let shuffled = "status,price_poisha,title,sku\nactive,77000,Shuffled In, P-3\n";
    let (status, report) = call_csv(&app, &uri, shuffled, &seller).await;
    assert_eq!(status, StatusCode::OK, "shuffled: {report}");
    assert_eq!(report["created"], json!(1), "{report}");
    let third_id = report["outcomes"][0]["product_id"]
        .as_str()
        .expect("product id")
        .to_string();
    let (title, price): (String, i64) = sqlx::query_as(
        "SELECT title, price_poisha FROM products WHERE id = $1::uuid",
    )
    .bind(&third_id)
    .fetch_one(&pool)
    .await
    .expect("third product");
    assert_eq!(title, "Shuffled In");
    assert_eq!(price, 77_000);
}
