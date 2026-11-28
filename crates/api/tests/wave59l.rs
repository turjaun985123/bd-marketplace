//! Wave 59 integration test (l): blog discovery — the
//! platform-level recent list carries live posts from
//! every ACTIVE store newest-first (with the store's
//! slug and name riding along), the followed-stores
//! list carries only the caller's followed stores and
//! needs a sign-in, and the store's own nav list
//! reports the live blog count the storefront frame
//! uses to decide whether a Blog link exists at all.
//!
//! Gated on `DATABASE_URL`: without it the test skips
//! cleanly so a plain local `cargo test` (no database)
//! still passes. CI provides a Postgres 16 service and
//! sets `DATABASE_URL` for the rust job.

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
            eprintln!("DATABASE_URL not set — skipping Wave 59 integration tests");
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
         store_pages, store_posts, store_post_events, \
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
    let state = AppState::new(Some(pool.clone()), "wave59-test-secret".to_string(), true)
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

/// Create one post published at an explicit moment.
async fn publish_post_at(app: &Router, token: &str, slug: &str, title: &str, moment: &str) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": slug,
            "title": title,
            "body": format!("{title} body."),
            "status": "published",
            "published_at": moment
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post {slug}: {body}");
}

/// The (store_slug, post slug) pairs of a discovery list.
fn pairs(body: &Value) -> Vec<(String, String)> {
    body["items"]
        .as_array()
        .expect("discovery items")
        .iter()
        .map(|item| {
            (
                item["store_slug"].as_str().expect("store slug").to_string(),
                item["slug"].as_str().expect("post slug").to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn wave59l_discovery_recent_and_followed() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller_a = login(&app, "01723111111").await;
    create_store(&app, &seller_a, "w59-alpha").await;
    let seller_b = login(&app, "01723222222").await;
    create_store(&app, &seller_b, "w59-beta").await;
    publish_post_at(&app, &seller_a, "alpha-old", "আলফা পুরনো", "2026-01-15T10:00:00Z").await;
    publish_post_at(&app, &seller_b, "beta-new", "বিটা নতুন", "2026-03-15T10:00:00Z").await;
    publish_post_at(&app, &seller_a, "alpha-mid", "আলফা মাঝের", "2026-02-15T10:00:00Z").await;
    // A draft on beta never joins discovery.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({ "slug": "beta-draft", "title": "খসড়া", "body": "body" })),
        Some(&seller_b),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "beta draft");

    // 1. Recent across stores: newest first, store
    // identity riding along, drafts absent.
    let (status, body) = call(&app, Method::GET, "/v1/posts/recent", None, None).await;
    assert_eq!(status, StatusCode::OK, "recent: {body}");
    assert_eq!(body["total"], 3);
    assert_eq!(
        pairs(&body),
        [
            ("w59-beta".to_string(), "beta-new".to_string()),
            ("w59-alpha".to_string(), "alpha-mid".to_string()),
            ("w59-alpha".to_string(), "alpha-old".to_string()),
        ]
    );
    assert!(
        body["items"][0]["store_name"]
            .as_str()
            .expect("store name")
            .contains("w59-beta"),
        "the store name rides the discovery row: {body}"
    );

    // 2. The limit clamps the page, not the total.
    let (status, body) = call(&app, Method::GET, "/v1/posts/recent?limit=2", None, None).await;
    assert_eq!(status, StatusCode::OK, "recent limited: {body}");
    assert_eq!(body["total"], 3);
    assert_eq!(body["items"].as_array().expect("items").len(), 2);

    // 3. Followed posts: anonymous is 401; a buyer who
    // follows only alpha sees only alpha's posts.
    let (status, _) = call(&app, Method::GET, "/v1/follows/posts", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "followed posts need a sign-in");
    let buyer = login(&app, "01723333333").await;
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/w59-alpha/follow",
        Some(json!({})),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "follow alpha: {body}");
    let (status, body) = call(&app, Method::GET, "/v1/follows/posts", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "followed list: {body}");
    assert_eq!(
        pairs(&body),
        [
            ("w59-alpha".to_string(), "alpha-mid".to_string()),
            ("w59-alpha".to_string(), "alpha-old".to_string()),
        ]
    );

    // 4. The nav-list blog counts: alpha reports 2 live
    // posts, beta reports 1 (its draft does not count).
    let (status, body) = call(&app, Method::GET, "/v1/stores/w59-alpha/pages", None, None).await;
    assert_eq!(status, StatusCode::OK, "alpha nav: {body}");
    assert_eq!(body["blog_post_count"], 2);
    let (status, body) = call(&app, Method::GET, "/v1/stores/w59-beta/pages", None, None).await;
    assert_eq!(status, StatusCode::OK, "beta nav: {body}");
    assert_eq!(body["blog_post_count"], 1);
}
