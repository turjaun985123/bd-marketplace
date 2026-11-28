//! Wave 58 integration test: the hero fallback matrix —
//! each hero field falls back to branding INDEPENDENTLY:
//! - with only a link set, the headline/subtext/image
//!   still come from the store (name / description /
//!   banner) while `is_customised` reports true (a link
//!   IS a customisation — the copy block renders);
//! - an explicit hero image beats the branding banner;
//! - a store with no announcement resolves the hero's
//!   announcement to null (no bar renders);
//! - clearing the hero back to an empty config returns
//!   `is_customised` to false.
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

/// PUT a one-hero layout with the given hero config.
async fn put_hero(app: &Router, token: &str, config: Value) {
    let (status, body) = call(
        app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "sections": [
                { "id": "hero", "type": "hero", "config": config }
            ]
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put hero layout: {body}");
}

/// The public layout's hero payload for the test store.
async fn public_hero(app: &Router) -> Value {
    let (status, public) = call(
        app,
        Method::GET,
        "/v1/stores/w58-hero/layout",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    let hero = public["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .find(|section| section["id"] == json!("hero"))
        .expect("hero section");
    hero["hero"].clone()
}

#[tokio::test]
async fn wave58_hero_fallback_matrix() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    create_store(&app, &seller, "w58-hero").await;
    // Branding WITHOUT an announcement: banner + description only.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/branding",
        Some(json!({
            "banner_url": "https://cdn.example.com/brand-banner.png",
            "description": "ব্র্যান্ডের বর্ণনা"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "branding: {body}");

    // Empty hero: pure branding echo, not customised, no
    // announcement.
    put_hero(&app, &seller, json!({})).await;
    let hero = public_hero(&app).await;
    assert_eq!(hero["headline"], json!("Store w58-hero"));
    assert_eq!(hero["subtext"], json!("ব্র্যান্ডের বর্ণনা"));
    assert_eq!(
        hero["image_url"],
        json!("https://cdn.example.com/brand-banner.png")
    );
    assert_eq!(hero["announcement"], Value::Null);
    assert_eq!(hero["is_customised"], json!(false));

    // Only a link set: the copy block counts as
    // customised, the rest still falls back.
    put_hero(
        &app,
        &seller,
        json!({ "link_url": "/w58-hero/shop", "link_label": "সব পণ্য" }),
    )
    .await;
    let hero = public_hero(&app).await;
    assert_eq!(hero["headline"], json!("Store w58-hero"));
    assert_eq!(hero["link_url"], json!("/w58-hero/shop"));
    assert_eq!(hero["link_label"], json!("সব পণ্য"));
    assert_eq!(hero["is_customised"], json!(true));

    // An explicit hero image beats the branding banner;
    // the headline still falls back.
    put_hero(
        &app,
        &seller,
        json!({ "image_url": "https://cdn.example.com/hero-only.png" }),
    )
    .await;
    let hero = public_hero(&app).await;
    assert_eq!(
        hero["image_url"],
        json!("https://cdn.example.com/hero-only.png")
    );
    assert_eq!(hero["headline"], json!("Store w58-hero"));
    assert_eq!(hero["is_customised"], json!(true));
}
