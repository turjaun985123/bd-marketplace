//! Wave 58 integration test: reset — POSTing the reset
//! deletes the stored row and the store falls back to
//! the platform default composition:
//! - after a custom PUT the row exists and both reads
//!   report `is_custom: true`;
//! - the reset response IS the default document
//!   (`is_custom: false`, the 9 default sections);
//! - the row is gone from store_layouts afterwards, and
//!   the public read renders the default again;
//! - resetting a store that never customised is a
//!   harmless no-op landing on the same default.
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

/// How many layout rows a store has stored.
async fn stored_layout_rows(pool: &PgPool, store_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM store_layouts WHERE store_id = $1::uuid")
        .bind(store_id)
        .fetch_one(pool)
        .await
        .expect("count stored layouts")
}

#[tokio::test]
async fn wave58_reset_deletes_the_row_back_to_default() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w58-reset").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // Resetting a never-customised store: a no-op that
    // still answers the default document.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/store/layout/reset",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "early reset: {body}");
    assert_eq!(body["is_custom"], json!(false));
    assert_eq!(stored_layout_rows(&pool, &store_id).await, 0);

    // Customise: one rich-text section only.
    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "sections": [
                { "id": "note", "type": "rich_text", "config": { "body": "শুধু এই নোট" } }
            ]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put layout: {saved}");
    assert_eq!(saved["is_custom"], json!(true));
    assert_eq!(stored_layout_rows(&pool, &store_id).await, 1);
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-reset/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(section_ids(&public), vec!["note"]);

    // Reset: the response is the default document...
    let (status, reset) = call(
        &app,
        Method::POST,
        "/v1/seller/store/layout/reset",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reset: {reset}");
    assert_eq!(reset["is_custom"], json!(false));
    assert_eq!(reset["updated_at"], Value::Null);
    assert_eq!(
        section_ids(&reset),
        vec![
            "hero",
            "rail-on-sale",
            "rail-best-sellers",
            "rail-new-arrivals",
            "collections",
            "specialties",
            "categories",
            "all-products",
            "store-info",
        ]
    );
    // ...the row is gone...
    assert_eq!(stored_layout_rows(&pool, &store_id).await, 0);
    // ...and both reads agree the store is default again.
    let (status, fetched) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller layout: {fetched}");
    assert_eq!(fetched["is_custom"], json!(false));
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-reset/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(public["is_custom"], json!(false));
    // The default composition on an empty store resolves
    // to the hero and the (empty) catalogue grid.
    assert_eq!(section_ids(&public), vec!["hero", "all-products"]);
}
