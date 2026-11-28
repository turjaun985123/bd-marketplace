//! Wave 59 integration test (e): post-to-post
//! navigation — the detail read carries the previous
//! (older) and next (newer) live neighbours by publish
//! moment, drafts and scheduled posts never appear as
//! neighbours, and the related rail ranks live posts
//! by shared-tag count (more shared tags first, then
//! newest) while a tagless post gets NO related rail
//! rather than unrelated filler.
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

/// Create one post published at an explicit moment
/// (the status action sets the stamp); returns the id.
async fn create_post_at(
    app: &Router,
    token: &str,
    slug: &str,
    title: &str,
    tags: Value,
    moment: &str,
) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": slug,
            "title": title,
            "body": format!("{title} body."),
            "tags": tags
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create post {slug}: {body}");
    let id = body["id"].as_str().expect("post id").to_string();
    let uri = format!("/v1/seller/store/posts/{id}/status");
    let (status, body) = call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "status": "published", "published_at": moment })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "publish {slug}: {body}");
    id
}

/// The public detail read for one post.
async fn detail(app: &Router, store_slug: &str, post_slug: &str) -> Value {
    let uri = format!("/v1/stores/{store_slug}/posts/{post_slug}");
    let (status, body) = call(app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "detail {post_slug}: {body}");
    body
}

#[tokio::test]
async fn wave59e_prev_next_and_related_ranking() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01716111111").await;
    create_store(&app, &seller, "w59-nav").await;

    // A dated chain: oldest → middle → newest, plus a
    // DRAFT that shares every tag (it must never leak
    // into navigation or related), and a tagless post.
    create_post_at(
        &app,
        &seller,
        "oldest",
        "সবচেয়ে পুরনো",
        json!(["eid", "guide"]),
        "2026-01-01T10:00:00Z",
    )
    .await;
    create_post_at(
        &app,
        &seller,
        "middle",
        "মাঝের পোস্ট",
        json!(["eid", "guide"]),
        "2026-02-01T10:00:00Z",
    )
    .await;
    create_post_at(
        &app,
        &seller,
        "newest",
        "সবচেয়ে নতুন",
        json!(["eid"]),
        "2026-03-01T10:00:00Z",
    )
    .await;
    create_post_at(
        &app,
        &seller,
        "solo",
        "একা পোস্ট",
        json!([]),
        "2026-04-01T10:00:00Z",
    )
    .await;
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "draft-twin",
            "title": "খসড়া জমজ",
            "body": "draft body",
            "tags": ["eid", "guide"]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "draft twin created");

    // 1. The middle post: previous = oldest, next =
    // newest (solo is NEWER still but comes after
    // newest in the chain — next is the IMMEDIATE
    // neighbour, so newest, not solo).
    let middle = detail(&app, "w59-nav", "middle").await;
    assert_eq!(middle["previous"]["slug"], "oldest");
    assert_eq!(middle["next"]["slug"], "newest");

    // 2. The ends of the chain: oldest has no previous,
    // solo (the newest live post) has no next.
    let oldest = detail(&app, "w59-nav", "oldest").await;
    assert_eq!(oldest["previous"], Value::Null);
    assert_eq!(oldest["next"]["slug"], "middle");
    let solo = detail(&app, "w59-nav", "solo").await;
    assert_eq!(solo["next"], Value::Null);
    assert_eq!(solo["previous"]["slug"], "newest");

    // 3. Related ranking for "middle" (tags eid+guide):
    // oldest shares BOTH tags and outranks newest,
    // which shares one — even though newest is newer.
    // The draft twin (both tags) is absent.
    let related = middle["related"].as_array().expect("related rail");
    let related_slugs: Vec<&str> = related
        .iter()
        .map(|item| item["slug"].as_str().expect("slug"))
        .collect();
    assert_eq!(related_slugs, ["oldest", "newest"]);

    // 4. A tagless post has NO related rail — no filler.
    let solo_related = solo["related"].as_array().expect("related rail");
    assert!(solo_related.is_empty(), "tagless post: {solo_related:?}");
}
