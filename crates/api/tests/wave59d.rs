//! Wave 59 integration test (d): post tags — the API
//! normalises what the seller typed (case folded,
//! spaces become hyphens, duplicates collapse, the
//! order is first-seen), the public tag filter matches
//! the normalised form from either spelling, a tag
//! that can never exist yields an honest empty list
//! (not an error, not the unfiltered blog), and a
//! ninth tag or an illegal character is refused at
//! write time.
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

/// Create one PUBLISHED post with the given raw tags;
/// returns the post view.
async fn create_tagged_post(
    app: &Router,
    token: &str,
    slug: &str,
    title: &str,
    tags: Value,
) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": slug,
            "title": title,
            "body": format!("{title} body."),
            "tags": tags,
            "status": "published"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create post {slug}: {body}");
    body
}

/// The public list's slugs under a raw tag filter.
async fn slugs_for_tag(app: &Router, store_slug: &str, tag: &str) -> Vec<String> {
    let uri = format!("/v1/stores/{store_slug}/posts?tag={tag}");
    let (status, body) = call(app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "tag list {tag}: {body}");
    body["items"]
        .as_array()
        .expect("post items")
        .iter()
        .map(|item| item["slug"].as_str().expect("slug").to_string())
        .collect()
}

#[tokio::test]
async fn wave59d_post_tags_normalise_and_filter() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01715111111").await;
    create_store(&app, &seller, "w59-tags").await;

    // 1. Normalisation is visible in the write response:
    // "Eid Collection" folds to eid-collection, "EID"
    // folds to eid, the duplicate eid collapses, and
    // first-seen order is kept.
    let eid = create_tagged_post(
        &app,
        &seller,
        "eid-post",
        "ঈদের পোস্ট",
        json!(["Eid Collection", "EID", "eid", "Guide"]),
    )
    .await;
    assert_eq!(eid["tags"], json!(["eid-collection", "eid", "guide"]));
    create_tagged_post(
        &app,
        &seller,
        "plain-post",
        "সাধারণ পোস্ট",
        json!(["guide"]),
    )
    .await;
    create_tagged_post(
        &app,
        &seller,
        "untagged-post",
        "ট্যাগহীন পোস্ট",
        json!([]),
    )
    .await;

    // 2. The filter matches the normalised form from
    // EITHER spelling: the raw typed form and the
    // stored form find the same post.
    assert_eq!(
        slugs_for_tag(&app, "w59-tags", "Eid%20Collection").await,
        ["eid-post"]
    );
    assert_eq!(slugs_for_tag(&app, "w59-tags", "eid-collection").await, ["eid-post"]);
    let guide = slugs_for_tag(&app, "w59-tags", "guide").await;
    assert_eq!(guide.len(), 2, "both guide posts match: {guide:?}");
    assert!(guide.contains(&"eid-post".to_string()));
    assert!(guide.contains(&"plain-post".to_string()));

    // 3. A tag nobody has is an empty list; a tag that
    // can never be normalised (illegal characters) is
    // ALSO an honest empty list — never an error and
    // never the whole blog.
    assert!(slugs_for_tag(&app, "w59-tags", "nonexistent").await.is_empty());
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-tags/posts?tag=!!!",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "illegal tag filter: {body}");
    assert_eq!(body["total"], 0);

    // 4. Write-time refusals: nine tags, an illegal
    // character, and an over-long tag all 400 with
    // nothing stored.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "too-many-tags",
            "title": "অনেক ট্যাগ",
            "body": "body",
            "tags": ["a", "b", "c", "d", "e", "f", "g", "h", "i"]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "nine tags refused");
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "bad-char-tag",
            "title": "খারাপ ট্যাগ",
            "body": "body",
            "tags": ["eid!"]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "illegal tag character refused");
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "long-tag",
            "title": "লম্বা ট্যাগ",
            "body": "body",
            "tags": ["a".repeat(31)]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "over-long tag refused");
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/store/posts",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller list: {body}");
    assert_eq!(body["total"], 3, "only the three good posts exist");
}
