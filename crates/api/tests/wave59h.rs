//! Wave 59 integration test (h): a suspended store's
//! content goes dark with the rest of the store —
//! pages, posts, the nav list and the discovery
//! surfaces all answer the uniform 404, exactly like
//! its products do — while the SELLER keeps full
//! access to their own editor (suspension is a public
//! visibility state, not a lockout), and unsuspending
//! restores the content untouched.
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

/// Set a store's status directly (admin suspension is
/// an API action; the row state is what visibility reads).
async fn set_store_status(pool: &PgPool, store_id: &str, status: &str) {
    sqlx::query("UPDATE stores SET status = $2 WHERE id = $1::uuid")
        .bind(store_id)
        .bind(status)
        .execute(pool)
        .await
        .expect("store status updated");
}

#[tokio::test]
async fn wave59h_suspended_store_content_goes_dark() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01719111111").await;
    let store = create_store(&app, &seller, "w59-dark").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // Published page + post, both public (the page is
    // born a draft; the status action publishes it).
    let (status, page) = call(
        &app,
        Method::POST,
        "/v1/seller/store/pages",
        Some(json!({
            "slug": "about",
            "title": "সম্পর্কে",
            "body": "স্টোর সম্পর্কে।",
            "show_in_nav": true
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "page created");
    let page_id = page["id"].as_str().expect("page id").to_string();
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/store/pages/{page_id}/status"),
        Some(json!({ "status": "published" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "page published");
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "hello",
            "title": "হ্যালো",
            "body": "প্রথম পোস্ট।",
            "status": "published"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post created");
    for uri in [
        "/v1/stores/w59-dark/pages",
        "/v1/stores/w59-dark/pages/about",
        "/v1/stores/w59-dark/posts",
        "/v1/stores/w59-dark/posts/hello",
    ] {
        let (status, _) = call(&app, Method::GET, uri, None, None).await;
        assert_eq!(status, StatusCode::OK, "live before suspension: {uri}");
    }
    let (status, body) = call(&app, Method::GET, "/v1/posts/recent", None, None).await;
    assert_eq!(status, StatusCode::OK, "recent before: {body}");
    assert_eq!(body["total"], 1);

    // Suspend: every public content surface 404s, and
    // the discovery list drops the post.
    set_store_status(&pool, &store_id, "suspended").await;
    for uri in [
        "/v1/stores/w59-dark/pages",
        "/v1/stores/w59-dark/pages/about",
        "/v1/stores/w59-dark/posts",
        "/v1/stores/w59-dark/posts/hello",
        "/v1/stores/w59-dark/feed",
    ] {
        let (status, _) = call(&app, Method::GET, uri, None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "dark while suspended: {uri}");
    }
    let (status, body) = call(&app, Method::GET, "/v1/posts/recent", None, None).await;
    assert_eq!(status, StatusCode::OK, "recent during: {body}");
    assert_eq!(body["total"], 0, "suspended store leaves discovery");

    // The seller's editor still works — they can read
    // and even keep writing while suspended.
    let (status, body) = call(&app, Method::GET, "/v1/seller/store/pages", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "seller pages during suspension: {body}");
    assert_eq!(body.as_array().expect("pages").len(), 1);
    let (status, body) = call(&app, Method::GET, "/v1/seller/store/posts", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "seller posts during suspension: {body}");
    assert_eq!(body["total"], 1);

    // Unsuspend: the content returns exactly as it was.
    set_store_status(&pool, &store_id, "active").await;
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-dark/posts/hello",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "restored post: {body}");
    assert_eq!(body["title"], "হ্যালো");
}
