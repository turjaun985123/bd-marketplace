//! Wave 59 integration test (k): post view counting —
//! the public beacon records a read ONLY for a live
//! post (a draft, a scheduled post, and a missing post
//! all 404 and record nothing), the count surfaces on
//! the seller's post list and in the stats panel
//! (all-time and trailing-window totals), and the
//! aggregate is all that exists: two reads from the
//! same anonymous caller are simply two reads.
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

/// Fire the view beacon for a post slug; returns the status.
async fn beacon(app: &Router, store_slug: &str, post_slug: &str) -> StatusCode {
    let uri = format!("/v1/stores/{store_slug}/posts/{post_slug}/view");
    let (status, _) = call(app, Method::POST, &uri, None, None).await;
    status
}

#[tokio::test]
async fn wave59k_view_beacon_and_stats() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01722111111").await;
    create_store(&app, &seller, "w59-views").await;
    let (status, live) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "live-post",
            "title": "লাইভ পোস্ট",
            "body": "পড়ার জন্য।",
            "status": "published"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "live post: {live}");
    let live_id = live["id"].as_str().expect("post id").to_string();
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "draft-post",
            "title": "খসড়া পোস্ট",
            "body": "এখনো না।"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "draft post");

    // 1. The beacon refuses what is not live — draft,
    // missing — and records nothing for them.
    assert_eq!(beacon(&app, "w59-views", "draft-post").await, StatusCode::NOT_FOUND);
    assert_eq!(beacon(&app, "w59-views", "no-such").await, StatusCode::NOT_FOUND);
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM store_post_events")
            .fetch_one(&pool)
            .await
            .expect("event count");
    assert_eq!(count, 0, "refused beacons record nothing");

    // 2. Three reads land; the seller list shows the
    // count on the post.
    for _ in 0..3 {
        assert_eq!(beacon(&app, "w59-views", "live-post").await, StatusCode::OK);
    }
    let (status, body) = call(&app, Method::GET, "/v1/seller/store/posts", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "seller list: {body}");
    let live_line = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["slug"] == "live-post")
        .expect("live post listed")
        .clone();
    assert_eq!(live_line["view_count"], 3);

    // 3. An old read (backdated past the window) counts
    // all-time but not in the trailing window.
    sqlx::query(
        "INSERT INTO store_post_events (post_id, kind, created_at) \
         VALUES ($1::uuid, 'post_view', now() - interval '40 days')",
    )
    .bind(&live_id)
    .execute(&pool)
    .await
    .expect("backdated view inserted");
    let (status, stats) = call(
        &app,
        Method::GET,
        "/v1/seller/store/post-stats",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stats: {stats}");
    assert_eq!(stats["total_views"], 4);
    assert_eq!(stats["window_views"], 3);
    assert_eq!(stats["window_days"], 30);
    let line = stats["posts"]
        .as_array()
        .expect("stat lines")
        .iter()
        .find(|item| item["slug"] == "live-post")
        .expect("live post in stats")
        .clone();
    assert_eq!(line["total_views"], 4);
    assert_eq!(line["window_views"], 3);
    // Every post has a line — the draft's reads zero.
    let lines = stats["posts"].as_array().expect("lines");
    assert_eq!(lines.len(), 2);
    let draft_line = lines
        .iter()
        .find(|item| item["slug"] == "draft-post")
        .expect("draft line present")
        .clone();
    assert_eq!(draft_line["total_views"], 0);
}
