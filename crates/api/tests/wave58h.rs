//! Wave 58 integration test: the layout and the
//! branding are INDEPENDENT surfaces:
//! - the store info section reads its texts from the
//!   branding-owned store row — editing branding changes
//!   what the section renders WITHOUT touching the
//!   layout, and the layout document never stores a copy;
//! - writing a new layout does not disturb branding
//!   (theme, about) — the public store view still shows
//!   the branding the seller set;
//! - the seller layout read is byte-stable across
//!   branding writes.
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

/// Find one section in a layout body by id.
fn section_by_id<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["sections"]
        .as_array()?
        .iter()
        .find(|section| section["id"].as_str() == Some(id))
}

#[tokio::test]
async fn wave58_layout_and_branding_are_independent_surfaces() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    create_store(&app, &seller, "w58-independent").await;

    // A layout whose store-info section carries NO text
    // of its own — the texts live on the store row.
    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "sections": [
                {
                    "id": "note",
                    "type": "rich_text",
                    "config": { "body": "লেআউটের নিজের লেখা" }
                },
                { "id": "store-info", "type": "store_info", "config": {} }
            ]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put layout: {saved}");
    let info = section_by_id(&saved, "store-info").expect("info section");
    assert_eq!(info["config"], json!({}), "store info stores no text copy");

    // Before any branding text exists, the info section
    // resolves empty and drops publicly.
    let (status, public) = call(
        &app,
        Method::GET,
        "/v1/stores/w58-independent/layout",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert!(section_by_id(&public, "store-info").is_none(), "no texts, no section");
    assert!(section_by_id(&public, "note").is_some());

    // Write branding: theme + about. The LAYOUT read must
    // not change; the public resolution must pick the
    // text up.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/branding",
        Some(json!({ "theme_key": "bold", "about": "প্রথম পরিচিতি" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "branding: {body}");
    let (status, fetched) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller layout: {fetched}");
    assert_eq!(fetched, saved, "branding writes never touch the layout");
    let (status, public) = call(
        &app,
        Method::GET,
        "/v1/stores/w58-independent/layout",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    let info = section_by_id(&public, "store-info").expect("info now renders");
    assert_eq!(info["store_info"]["about"], json!("প্রথম পরিচিতি"));

    // Edit the about text again: the section follows the
    // branding, still without any layout write.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/branding",
        Some(json!({ "about": "দ্বিতীয় পরিচিতি" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "branding 2: {body}");
    let (status, public) = call(
        &app,
        Method::GET,
        "/v1/stores/w58-independent/layout",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    let info = section_by_id(&public, "store-info").expect("info renders");
    assert_eq!(info["store_info"]["about"], json!("দ্বিতীয় পরিচিতি"));

    // Now rewrite the layout entirely: branding must be
    // exactly where the seller left it.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "sections": [
                { "id": "hero", "type": "hero", "config": {} }
            ]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put layout 2: {body}");
    let (status, store_view) = call(
        &app,
        Method::GET,
        "/v1/stores/w58-independent",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "store view: {store_view}");
    assert_eq!(store_view["theme_key"], json!("bold"));
    assert_eq!(store_view["about"], json!("দ্বিতীয় পরিচিতি"));
}
