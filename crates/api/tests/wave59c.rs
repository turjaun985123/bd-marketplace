//! Wave 59 integration test (c): the blog post
//! lifecycle — a draft is invisible, publishing with a
//! FUTURE moment schedules it (still invisible, in the
//! list and at its URL), the moment arriving makes it
//! live, unpublishing pulls it back and clears the
//! stamp, archiving keeps the stamp but hides the
//! post, and re-publishing an archived post restores
//! it. A reschedule — re-publishing a live post with a
//! NEW future moment — is the one same-status write
//! the state machine allows, and it hides the post
//! until the new moment arrives.
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

/// Create one draft post; returns the post view.
async fn create_post(app: &Router, token: &str, slug: &str, title: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": slug,
            "title": title,
            "body": format!("{title} — মূল লেখা।\n\nআরও বিস্তারিত।"),
            "tags": []
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create post {slug}: {body}");
    body
}

/// Move one post to a status (optionally naming the
/// publish moment); returns (status, body) unasserted.
async fn post_status(
    app: &Router,
    token: &str,
    post_id: &str,
    status: &str,
    published_at: Option<&str>,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/store/posts/{post_id}/status");
    let mut payload = json!({ "status": status });
    if let Some(moment) = published_at {
        payload["published_at"] = json!(moment);
    }
    call(app, Method::POST, &uri, Some(payload), Some(token)).await
}

/// Whether a post slug is publicly readable right now.
async fn is_public(app: &Router, store_slug: &str, post_slug: &str) -> bool {
    let uri = format!("/v1/stores/{store_slug}/posts/{post_slug}");
    let (status, _) = call(app, Method::GET, &uri, None, None).await;
    status == StatusCode::OK
}

/// The public list's slugs for a store, in served order.
async fn public_slugs(app: &Router, store_slug: &str) -> Vec<String> {
    let uri = format!("/v1/stores/{store_slug}/posts");
    let (status, body) = call(app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "public posts: {body}");
    body["items"]
        .as_array()
        .expect("post items")
        .iter()
        .map(|item| item["slug"].as_str().expect("slug").to_string())
        .collect()
}

#[tokio::test]
async fn wave59c_post_lifecycle_and_scheduling() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01714111111").await;
    create_store(&app, &seller, "w59-life").await;
    let post = create_post(&app, &seller, "eid-guide", "ঈদ গাইড").await;
    let post_id = post["id"].as_str().expect("post id").to_string();
    assert_eq!(post["status"], "draft");
    assert_eq!(post["published_at"], Value::Null);

    // 1. A draft is invisible everywhere public.
    assert!(!is_public(&app, "w59-life", "eid-guide").await);
    assert!(public_slugs(&app, "w59-life").await.is_empty());

    // 2. Publish with a FUTURE moment: the status is
    // published but the post is still not live — the
    // schedule is real, not a label.
    let future = "2999-01-01T00:00:00Z";
    let (status, body) = post_status(&app, &seller, &post_id, "published", Some(future)).await;
    assert_eq!(status, StatusCode::OK, "schedule: {body}");
    assert_eq!(body["published_at"], future);
    assert!(!is_public(&app, "w59-life", "eid-guide").await);
    assert!(public_slugs(&app, "w59-life").await.is_empty());

    // 3. The moment arrives (the test moves the stamp
    // into the past, the way the clock would): the
    // post is live in the list and at its URL.
    sqlx::query("UPDATE store_posts SET published_at = now() - interval '1 hour' WHERE id = $1::uuid")
        .bind(&post_id)
        .execute(&pool)
        .await
        .expect("backdate the publish moment");
    assert!(is_public(&app, "w59-life", "eid-guide").await);
    assert_eq!(public_slugs(&app, "w59-life").await, ["eid-guide"]);

    // 4. A same-status write with NO new moment is the
    // refused no-op; WITH a new future moment it is a
    // reschedule and the post hides until then.
    let (status, _) = post_status(&app, &seller, &post_id, "published", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no-op publish refused");
    let (status, body) = post_status(&app, &seller, &post_id, "published", Some(future)).await;
    assert_eq!(status, StatusCode::OK, "reschedule: {body}");
    assert!(!is_public(&app, "w59-life", "eid-guide").await);

    // 5. Unpublish: back to draft and the stamp is
    // cleared — a later publish starts a fresh moment.
    let (status, body) = post_status(&app, &seller, &post_id, "draft", None).await;
    assert_eq!(status, StatusCode::OK, "unpublish: {body}");
    assert_eq!(body["status"], "draft");
    assert_eq!(body["published_at"], Value::Null);
    let (status, body) = post_status(&app, &seller, &post_id, "published", None).await;
    assert_eq!(status, StatusCode::OK, "publish now: {body}");
    assert!(body["published_at"].is_string(), "publish-now stamps the moment");
    assert!(is_public(&app, "w59-life", "eid-guide").await);

    // 6. Archive keeps the stamp but hides the post;
    // re-publishing the archived post restores it with
    // the SAME stamp (no moment supplied).
    let stamp = body["published_at"].as_str().expect("stamp").to_string();
    let (status, body) = post_status(&app, &seller, &post_id, "archived", None).await;
    assert_eq!(status, StatusCode::OK, "archive: {body}");
    assert_eq!(body["published_at"], stamp);
    assert!(!is_public(&app, "w59-life", "eid-guide").await);
    let (status, body) = post_status(&app, &seller, &post_id, "published", None).await;
    assert_eq!(status, StatusCode::OK, "restore: {body}");
    assert_eq!(body["published_at"], stamp);
    assert!(is_public(&app, "w59-life", "eid-guide").await);

    // 7. Creating directly as published stamps the
    // moment at write time.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "instant-post",
            "title": "তাৎক্ষণিক পোস্ট",
            "body": "সরাসরি প্রকাশিত।",
            "status": "published"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "born published: {body}");
    assert!(body["published_at"].is_string());
    assert!(is_public(&app, "w59-life", "instant-post").await);
    // Newest first: the instant post leads the list.
    assert_eq!(
        public_slugs(&app, "w59-life").await,
        ["instant-post", "eid-guide"]
    );
}
