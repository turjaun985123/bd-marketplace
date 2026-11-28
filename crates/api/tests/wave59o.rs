//! Wave 59 integration test (o): the follower notice
//! for a new blog post — a draft fires nothing, the
//! publish that makes the post live queues exactly one
//! `store_new_post` notice per follower (in the
//! follower's inbox, with the post's title in the
//! rendered body), a SCHEDULED publish fires nothing
//! (and unpublishing + republishing fires again, the
//! same rule as products), a follower who muted store
//! news hears nothing, and a non-follower hears
//! nothing.
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

/// How many `store_new_post` notices sit in a user's inbox.
async fn post_notices(app: &Router, token: &str) -> Vec<Value> {
    let (status, inbox) = call(app, Method::GET, "/v1/notifications", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "inbox: {inbox}");
    inbox["items"]
        .as_array()
        .expect("inbox items")
        .iter()
        .filter(|item| item["kind"] == "store_new_post")
        .cloned()
        .collect()
}

#[tokio::test]
async fn wave59o_new_post_follower_notices() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01726111111").await;
    create_store(&app, &seller, "w59-notice").await;
    let follower = login(&app, "01726222222").await;
    let muted = login(&app, "01726333333").await;
    let bystander = login(&app, "01726444444").await;
    for token in [&follower, &muted] {
        let (status, _) = call(
            &app,
            Method::POST,
            "/v1/stores/w59-notice/follow",
            Some(json!({})),
            Some(token),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "follow the store");
    }
    // The muted follower turns store news off.
    let (status, _) = call(
        &app,
        Method::PATCH,
        "/v1/stores/w59-notice/follow",
        Some(json!({ "notify_new_products": false })),
        Some(&muted),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mute store news");

    // 1. A draft post fires nothing.
    let (status, post) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "notice-post",
            "title": "নোটিশ পোস্ট",
            "body": "খবর।"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "draft post: {post}");
    let post_id = post["id"].as_str().expect("post id").to_string();
    assert!(post_notices(&app, &follower).await.is_empty());

    // 2. Publishing it live queues exactly one notice
    // for the follower — none for the muted follower,
    // none for the bystander — and the rendered body
    // names the post.
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/store/posts/{post_id}/status"),
        Some(json!({ "status": "published" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "publish");
    let notices = post_notices(&app, &follower).await;
    assert_eq!(notices.len(), 1, "one notice: {notices:?}");
    let body_text = notices[0]["body"].as_str().unwrap_or("");
    assert!(
        body_text.contains("নোটিশ পোস্ট"),
        "the notice body names the post: {body_text}"
    );
    assert!(post_notices(&app, &muted).await.is_empty(), "muted hears nothing");
    assert!(post_notices(&app, &bystander).await.is_empty(), "bystander hears nothing");

    // 3. A scheduled publish fires nothing now.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "future-post",
            "title": "ভবিষ্যৎ পোস্ট",
            "body": "পরে।",
            "status": "published",
            "published_at": "2999-01-01T00:00:00Z"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "scheduled post");
    assert_eq!(post_notices(&app, &follower).await.len(), 1, "still just the one");

    // 4. Unpublish + republish fires again (the post
    // becomes live anew — the product rule).
    for target in ["draft", "published"] {
        let (status, _) = call(
            &app,
            Method::POST,
            &format!("/v1/seller/store/posts/{post_id}/status"),
            Some(json!({ "status": target })),
            Some(&seller),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "move to {target}");
    }
    assert_eq!(post_notices(&app, &follower).await.len(), 2, "republish re-fires");
}
