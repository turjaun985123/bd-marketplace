//! Wave 58 integration test: write validation — a bad
//! layout is refused whole, with the error naming the
//! offending section, and nothing is stored:
//! - an FAQ without its items fails at "section 2" (the
//!   first section is fine — positions are 1-based);
//! - duplicate section ids fail, naming the second use;
//! - 21 sections fail the layout cap;
//! - a future document version fails;
//! - a malformed section TYPE fails, while a well-formed
//!   UNKNOWN type is accepted, preserved in the seller
//!   view, and skipped by the public renderer (the
//!   forward-tolerance rule);
//! - after every refusal the store still has NO custom
//!   layout (a failed PUT changes nothing).
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
            eprintln!("DATABASE_URL not set — skipping Wave 58 integration tests");
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
         buyer_addresses, bulk_imports, store_layouts, storefront_events, \
         store_collections, store_collection_products, \
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
    let state = AppState::new(Some(pool.clone()), "wave58-test-secret".to_string(), true)
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

/// The ids of a layout body's sections, in order.
fn section_ids(body: &Value) -> Vec<String> {
    body["sections"]
        .as_array()
        .expect("sections array")
        .iter()
        .map(|section| section["id"].as_str().expect("section id").to_string())
        .collect()
}

/// PUT one layout document for the seller.
async fn put_layout(app: &Router, token: &str, document: Value) -> (StatusCode, Value) {
    call(
        app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(document),
        Some(token),
    )
    .await
}

/// Assert the seller still has NO custom layout (a
/// refused PUT stored nothing). A nested async fn, not a
/// closure returning an async block — the Wave 57
/// lifetime lesson.
async fn assert_still_default(app: &Router, seller: &str) {
    let (status, fetched) = call(
        app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller layout: {fetched}");
    assert_eq!(fetched["is_custom"], json!(false), "nothing was stored");
}

#[tokio::test]
async fn wave58_validation_errors_name_the_section_and_store_nothing() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w58-validation").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // 1. The second section is broken: the error names
    //    section 2 (1-based, as the seller counts blocks).
    let (status, body) = put_layout(
        &app,
        &seller,
        json!({
            "sections": [
                { "id": "hero", "type": "hero", "config": {} },
                { "id": "faq", "type": "faq", "config": {} }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "faq without items: {body}");
    let message = body["error"].as_str().expect("error message");
    assert!(message.contains("section 2"), "names the section: {message}");
    assert!(message.contains("faq"), "names the type: {message}");
    assert_still_default(&app, &seller).await;

    // 2. Duplicate ids: the second use is the offender.
    let (status, body) = put_layout(
        &app,
        &seller,
        json!({
            "sections": [
                { "id": "hero", "type": "hero", "config": {} },
                { "id": "hero", "type": "rich_text", "config": { "body": "x" } }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "duplicate ids: {body}");
    let message = body["error"].as_str().expect("error message");
    assert!(message.contains("section 2"), "names the dupe: {message}");
    assert_still_default(&app, &seller).await;

    // 3. The 20-section cap.
    let many: Vec<Value> = (0..21)
        .map(|index| json!({ "id": format!("s-{index}"), "type": "store_info", "config": {} }))
        .collect();
    let (status, body) = put_layout(&app, &seller, json!({ "sections": many })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "too many sections: {body}");
    assert_still_default(&app, &seller).await;

    // 4. A future document version is refused, never
    //    silently rewritten.
    let (status, body) = put_layout(
        &app,
        &seller,
        json!({ "version": 2, "sections": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "future version: {body}");
    assert_still_default(&app, &seller).await;

    // 5. A malformed type string is garbage, refused.
    let (status, body) = put_layout(
        &app,
        &seller,
        json!({
            "sections": [
                { "id": "weird", "type": "Bad Type!", "config": {} }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "malformed type: {body}");
    assert_still_default(&app, &seller).await;

    // 6. A well-formed UNKNOWN type is a future section:
    //    accepted, preserved verbatim in the seller view,
    //    skipped by the public renderer.
    let (status, saved) = put_layout(
        &app,
        &seller,
        json!({
            "sections": [
                { "id": "hero", "type": "hero", "config": {} },
                {
                    "id": "ar-block",
                    "type": "ar_try_on",
                    "config": { "model": "v2", "depth": 3 }
                }
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "future type accepted: {saved}");
    assert_eq!(saved["is_custom"], json!(true));
    let (status, fetched) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller layout: {fetched}");
    assert_eq!(section_ids(&fetched), vec!["hero", "ar-block"]);
    let future = &fetched["sections"][1];
    assert_eq!(future["config"]["model"], json!("v2"), "config preserved verbatim");
    assert_eq!(future["config"]["depth"], json!(3));
    let (status, public) = call(
        &app,
        Method::GET,
        "/v1/stores/w58-validation/layout",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(
        section_ids(&public),
        vec!["hero"],
        "the future section is skipped at render: {public}"
    );

    // The one accepted write is the only row.
    let stored_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_layouts WHERE store_id = $1::uuid",
    )
    .bind(&store_id)
    .fetch_one(&pool)
    .await
    .expect("count stored layouts");
    assert_eq!(stored_rows, 1);
}
