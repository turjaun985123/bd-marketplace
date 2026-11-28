//! Wave 59 integration test (p): admin content
//! moderation and the seller export — a non-admin is
//! refused the moderation queue, an admin sees every
//! store's posts and pages, an admin takedown
//! (archive) pulls a live post from public view and
//! writes the audit row, restoring it to draft hands
//! it back to the seller unpublished, a no-op admin
//! write is refused like the seller's, and the
//! seller's data export carries their pages and posts
//! in full.
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

/// Promote a user to admin by SQL (the platform's only
/// promotion path) — AFTER their first login created
/// the row.
async fn promote_admin(pool: &PgPool, phone: &str) {
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = $1")
        .bind(phone)
        .execute(pool)
        .await
        .expect("admin promotion lands");
}

#[tokio::test]
async fn wave59p_admin_moderation_and_seller_export() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01727111111").await;
    create_store(&app, &seller, "w59-mod").await;
    let (status, post) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "loud-post",
            "title": "জোরালো পোস্ট",
            "body": "বিতর্কিত লেখা।",
            "status": "published"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post: {post}");
    let post_id = post["id"].as_str().expect("post id").to_string();
    let (status, page) = call(
        &app,
        Method::POST,
        "/v1/seller/store/pages",
        Some(json!({
            "slug": "loud-page",
            "title": "জোরালো পেজ",
            "body": "পেজের লেখা।",
            "show_in_nav": true
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "page: {page}");
    let page_id = page["id"].as_str().expect("page id").to_string();

    // 1. The seller (not an admin) is refused the
    // moderation queue and its writes.
    let (status, _) = call(&app, Method::GET, "/v1/admin/content/posts", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller is not a moderator");
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/content/posts/{post_id}/status"),
        Some(json!({ "status": "archived" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller cannot moderate");

    // 2. The admin sees the post and the page, with
    // the store identity attached.
    let admin = login(&app, "01727222222").await;
    promote_admin(&pool, "+8801727222222").await;
    let (status, body) = call(&app, Method::GET, "/v1/admin/content/posts", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin post list: {body}");
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["store_slug"], "w59-mod");
    assert_eq!(body["items"][0]["status"], "published");
    let (status, body) = call(&app, Method::GET, "/v1/admin/content/pages", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin page list: {body}");
    assert_eq!(body["total"], 1);

    // 3. A no-op admin write is refused (the shared
    // machine), and a bad status is a 400.
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/content/posts/{post_id}/status"),
        Some(json!({ "status": "published" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no-op moderation refused");
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/content/posts/{post_id}/status"),
        Some(json!({ "status": "banned" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown status refused");

    // 4. The takedown: archive the post — it leaves
    // the public blog at once, and the audit log has
    // the row naming the admin's move.
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/content/posts/{post_id}/status"),
        Some(json!({ "status": "archived" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "takedown: {body}");
    assert_eq!(body["status"], "archived");
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-mod/posts/loud-post",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "taken-down post is not public");
    let (action, detail): (String, String) = sqlx::query_as(
        "SELECT action, detail::text FROM admin_audit_log \
         WHERE target_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&post_id)
    .fetch_one(&pool)
    .await
    .expect("audit row exists");
    assert_eq!(action, "content_post_status");
    assert!(detail.contains("archived"), "audit detail names the move: {detail}");

    // 5. Restore to draft: the seller has their post
    // back, unpublished, and can republish it
    // themselves.
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/content/posts/{post_id}/status"),
        Some(json!({ "status": "draft" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "restore to draft");
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/store/posts/{post_id}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller reads it back: {body}");
    assert_eq!(body["status"], "draft");
    assert_eq!(body["published_at"], Value::Null, "unpublish cleared the stamp");

    // 6. The page moderates the same way.
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/content/pages/{page_id}/status"),
        Some(json!({ "status": "published" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin publishes the page: {body}");
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-mod/pages/loud-page",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the page is public now");

    // 7. The seller export carries the content: the
    // page and the post, bodies included.
    let (status, export) = call(&app, Method::GET, "/v1/seller/export", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "seller export: {export}");
    let pages = export["pages"].as_array().expect("export pages");
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0]["slug"], "loud-page");
    assert_eq!(pages[0]["body"], "পেজের লেখা।");
    let posts = export["posts"].as_array().expect("export posts");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["slug"], "loud-post");
    assert_eq!(posts[0]["body"], "বিতর্কিত লেখা।");
}
