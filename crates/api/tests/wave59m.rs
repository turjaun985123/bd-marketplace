//! Wave 59 integration test (m): excerpts and reading
//! time on the public surfaces — a post without a
//! hand-written excerpt gets one DERIVED from its body
//! (whitespace collapsed, cut on a word boundary, no
//! ellipsis appended), a hand-written excerpt wins
//! verbatim, editing the body re-derives (the derived
//! excerpt is never stored), and the reading time is
//! the word count at the platform's words-per-minute
//! with a one-minute floor, visible on the card and
//! the detail alike.
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

/// The public card for one post (from the blog list).
async fn public_card(app: &Router, store_slug: &str, post_slug: &str) -> Value {
    let uri = format!("/v1/stores/{store_slug}/posts");
    let (status, body) = call(app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "public list: {body}");
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["slug"] == post_slug)
        .unwrap_or_else(|| panic!("post {post_slug} listed"))
        .clone()
}

#[tokio::test]
async fn wave59m_excerpt_derivation_and_reading_time() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01724111111").await;
    create_store(&app, &seller, "w59-excerpt").await;

    // A long body of repeated 9-char words: the derived
    // excerpt must stop at a word boundary inside the
    // derivation budget and carry no ellipsis.
    let long_body = std::iter::repeat_n("abcdefghi", 600)
        .collect::<Vec<_>>()
        .join(" ");
    let (status, post) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "long-post",
            "title": "লম্বা পোস্ট",
            "body": long_body,
            "status": "published"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "long post: {post}");
    let post_id = post["id"].as_str().expect("post id").to_string();
    // 600 words at 200 wpm = 3 minutes.
    assert_eq!(post["word_count"], 600);
    let card = public_card(&app, "w59-excerpt", "long-post").await;
    let excerpt = card["excerpt"].as_str().expect("derived excerpt").to_string();
    assert!(excerpt.chars().count() <= 240, "excerpt within budget: {excerpt}");
    assert!(
        excerpt
            .split_whitespace()
            .all(|word| word == "abcdefghi"),
        "excerpt ends on a word boundary: {excerpt}"
    );
    assert!(!excerpt.contains("..."), "no ellipsis fakery: {excerpt}");
    assert_eq!(card["reading_minutes"], 3);

    // A hand-written excerpt wins verbatim on both the
    // card and the detail; the seller read keeps the
    // two apart (custom vs derived).
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/posts/{post_id}"),
        Some(json!({ "excerpt": "আমার নিজের সারাংশ।" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "manual excerpt: {body}");
    assert_eq!(body["excerpt"], "আমার নিজের সারাংশ।");
    assert_eq!(body["custom_excerpt"], "আমার নিজের সারাংশ।");
    let card = public_card(&app, "w59-excerpt", "long-post").await;
    assert_eq!(card["excerpt"], "আমার নিজের সারাংশ।");
    let (status, detail) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-excerpt/posts/long-post",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["excerpt"], "আমার নিজের সারাংশ।");
    assert_eq!(detail["reading_minutes"], 3);

    // Clearing the excerpt (blank) returns to the
    // derived one — the derivation is from the CURRENT
    // body, so a body edit changes it too.
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/posts/{post_id}"),
        Some(json!({ "excerpt": "", "body": "ছোট্ট একটি লেখা এখন।" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "clear excerpt + new body: {body}");
    assert_eq!(body["custom_excerpt"], Value::Null);
    assert_eq!(body["excerpt"], "ছোট্ট একটি লেখা এখন।");
    assert_eq!(body["word_count"], 4);
    let card = public_card(&app, "w59-excerpt", "long-post").await;
    assert_eq!(card["reading_minutes"], 1, "the one-minute floor");
}
