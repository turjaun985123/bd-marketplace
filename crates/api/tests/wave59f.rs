//! Wave 59 integration test (f): the store's Atom
//! feed — a real XML document at the store's feed URL,
//! carrying only LIVE posts (a draft, a scheduled post,
//! and an archived post are all absent), with every
//! seller-written string XML-escaped (a title holding
//! `&` and `<` arrives as entities, never as markup),
//! the feed's own updated stamp tracking the newest
//! entry, and a suspended store's feed going uniformly
//! dark.
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

/// Send one request and return (status, content-type,
/// raw text) — the feed is XML, not JSON (the wave57
/// raw-body helper pattern).
async fn call_text(app: &Router, uri: &str) -> (StatusCode, String, String) {
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .expect("request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router responds");
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    (
        status,
        content_type,
        String::from_utf8(bytes.to_vec()).expect("feed is UTF-8"),
    )
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

/// Create one post in a given state; a `published_at`
/// of None with status published stamps "now".
async fn create_post_in_state(
    app: &Router,
    token: &str,
    slug: &str,
    title: &str,
    status: &str,
    published_at: Option<&str>,
) {
    let (code, body) = call(
        app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": slug,
            "title": title,
            "body": format!("{title} body."),
            "status": status,
            "published_at": published_at
        })),
        Some(token),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "create post {slug}: {body}");
}

#[tokio::test]
async fn wave59f_atom_feed_escapes_and_lists_only_live_posts() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01717111111").await;
    let store = create_store(&app, &seller, "w59-feed").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // One live post whose title needs escaping, one
    // draft, one scheduled, one archived — only the
    // live one may appear.
    create_post_in_state(
        &app,
        &seller,
        "fish-chips",
        "Fish & Chips <ঢাকা>",
        "published",
        Some("2026-05-01T09:30:00Z"),
    )
    .await;
    create_post_in_state(&app, &seller, "draft-post", "খসড়া", "draft", None).await;
    create_post_in_state(
        &app,
        &seller,
        "future-post",
        "ভবিষ্যৎ",
        "published",
        Some("2999-01-01T00:00:00Z"),
    )
    .await;
    create_post_in_state(
        &app,
        &seller,
        "old-post",
        "পুরনো",
        "archived",
        Some("2026-01-01T00:00:00Z"),
    )
    .await;

    let (status, content_type, feed) = call_text(&app, "/v1/stores/w59-feed/feed").await;
    assert_eq!(status, StatusCode::OK, "feed reads: {feed}");
    assert!(
        content_type.starts_with("application/atom+xml"),
        "feed content type: {content_type}"
    );
    assert!(feed.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
    assert!(feed.contains("<feed xmlns=\"http://www.w3.org/2005/Atom\">"));
    // The escaping pin: the raw title must NOT appear;
    // its escaped form must.
    assert!(
        !feed.contains("Fish & Chips <ঢাকা>"),
        "raw title leaked into the feed: {feed}"
    );
    assert!(
        feed.contains("Fish &amp; Chips &lt;ঢাকা&gt;"),
        "escaped title missing: {feed}"
    );
    assert!(feed.contains("/w59-feed/blog/fish-chips"), "entry link: {feed}");
    assert!(feed.contains("<updated>2026-05-01T09:30:00"), "feed stamp: {feed}");
    for absent in ["draft-post", "future-post", "old-post"] {
        assert!(!feed.contains(absent), "{absent} must not be in the feed");
    }
    assert_eq!(feed.matches("<entry>").count(), 1, "exactly one entry: {feed}");

    // An empty blog still serves a valid feed (no
    // entries, the store's own stamp).
    let other = login(&app, "01717222222").await;
    create_store(&app, &other, "w59-feed-empty").await;
    let (status, _, feed) = call_text(&app, "/v1/stores/w59-feed-empty/feed").await;
    assert_eq!(status, StatusCode::OK, "empty feed reads: {feed}");
    assert_eq!(feed.matches("<entry>").count(), 0);

    // A missing store is the uniform 404, and a
    // suspended store's feed goes dark with it.
    let (status, _, _) = call_text(&app, "/v1/stores/no-such-store/feed").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "missing store feed");
    sqlx::query("UPDATE stores SET status = 'suspended' WHERE id = $1::uuid")
        .bind(&store_id)
        .execute(&pool)
        .await
        .expect("suspend the store");
    let (status, _, _) = call_text(&app, "/v1/stores/w59-feed/feed").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "suspended store feed is dark");
}
