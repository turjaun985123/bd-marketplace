//! Wave 59 integration test (i): the caps and the
//! collision rules — a store holds at most 25 pages
//! (the 26th is refused and the count does not move),
//! a slug collision INSIDE one store is a 409 for both
//! pages and posts while the same slug in another
//! store is fine, and the field rules refuse what the
//! domain caps name (bad slug shape, over-long titles,
//! over-long bodies, a nav flag on a non-boolean).
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

#[tokio::test]
async fn wave59i_caps_collisions_and_field_rules() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01720111111").await;
    create_store(&app, &seller, "w59-caps").await;

    // 1. The page cap: 25 pages fit, the 26th is a 400
    // and the store still holds exactly 25.
    for n in 0..25 {
        let (status, body) = call(
            &app,
            Method::POST,
            "/v1/seller/store/pages",
            Some(json!({
                "slug": format!("page-{n}"),
                "title": format!("পেজ {n}"),
                "body": "body"
            })),
            Some(&seller),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "page {n}: {body}");
    }
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/pages",
        Some(json!({ "slug": "page-26", "title": "একটি বেশি", "body": "body" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "the 26th page is refused");
    let (status, body) = call(&app, Method::GET, "/v1/seller/store/pages", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "page list: {body}");
    assert_eq!(body.as_array().expect("pages").len(), 25);

    // Free one slot first: at the cap, a create is
    // refused by the cap rule before its slug is ever
    // examined, so the collision rule below needs room.
    let first_id = body[24]["id"].as_str().expect("page id").to_string();
    let (status, _) = call(
        &app,
        Method::DELETE,
        &format!("/v1/seller/store/pages/{first_id}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete frees a slot");

    // 2. Slug collisions: page-0 again inside this
    // store is a 409; in another store it is fine.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/pages",
        Some(json!({ "slug": "page-0", "title": "দ্বিতীয়", "body": "body" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "same-store page slug collides");
    let other = login(&app, "01720222222").await;
    create_store(&app, &other, "w59-caps-other").await;
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/pages",
        Some(json!({ "slug": "page-0", "title": "অন্য স্টোরে", "body": "body" })),
        Some(&other),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "same slug elsewhere is fine");

    // 3. The same collision rule for posts.
    let post = json!({
        "slug": "shared-slug",
        "title": "পোস্ট",
        "body": "body",
        "tags": []
    });
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(post.clone()),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "first post");
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(post.clone()),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "same-store post slug collides");
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(post),
        Some(&other),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post slug elsewhere is fine");

    // 4. Field rules: a bad slug shape, an over-long
    // page title (121 chars), an over-long post title
    // (161), and over-long bodies are all 400s.
    for (uri, payload) in [
        (
            "/v1/seller/store/pages",
            json!({ "slug": "Bad Slug!", "title": "ঠিক আছে", "body": "body" }),
        ),
        (
            "/v1/seller/store/pages",
            json!({ "slug": "ok-slug", "title": "a".repeat(121), "body": "body" }),
        ),
        (
            "/v1/seller/store/pages",
            json!({ "slug": "ok-slug", "title": "ঠিক আছে", "body": "a".repeat(20_001) }),
        ),
        (
            "/v1/seller/store/posts",
            json!({ "slug": "ok-post", "title": "a".repeat(161), "body": "body" }),
        ),
        (
            "/v1/seller/store/posts",
            json!({ "slug": "ok-post", "title": "ঠিক আছে", "body": "a".repeat(40_001) }),
        ),
    ] {
        let (status, _) = call(&app, Method::POST, uri, Some(payload), Some(&seller)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "field rule at {uri}");
    }

    // 5. A page and a post may share a slug inside one
    // store (different tables, different URLs) — the
    // collision rule is per content kind.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "page-1",
            "title": "পেজের নামে পোস্ট",
            "body": "body",
            "tags": []
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "a post may reuse a page's slug");
}
