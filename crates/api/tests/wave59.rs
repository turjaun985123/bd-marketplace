//! Wave 59 integration test: store pages — the seller
//! creates pages as drafts, the public sees nothing of
//! them until they publish, the nav list carries only
//! published + nav-flagged pages in the seller's
//! arrangement (with the live blog-post count riding
//! along), an archived page disappears again, and a
//! second store's page under the same slug is a
//! different page entirely.
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

/// Create one page (a draft) for the caller's store;
/// returns the page view.
async fn create_page(
    app: &Router,
    token: &str,
    slug: &str,
    title: &str,
    show_in_nav: bool,
) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/seller/store/pages",
        Some(json!({
            "slug": slug,
            "title": title,
            "body": format!("{title} — বিস্তারিত লেখা।\n\nদ্বিতীয় অনুচ্ছেদ।"),
            "show_in_nav": show_in_nav
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create page {slug}: {body}");
    body
}

/// Move one page to a new status; returns the page view.
async fn set_page_status(app: &Router, token: &str, page_id: &str, status: &str) -> Value {
    let uri = format!("/v1/seller/store/pages/{page_id}/status");
    let (code, body) = call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "status": status })),
        Some(token),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "set page status {status}: {body}");
    body
}

#[tokio::test]
async fn wave59_pages_crud_nav_and_public_visibility() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    create_store(&app, &seller, "w59-pages").await;

    // 1. Two pages as drafts (one nav-flagged, one not)
    // plus a third that will never leave the nav off.
    let shipping = create_page(&app, &seller, "shipping-info", "ডেলিভারি তথ্য", true).await;
    assert_eq!(shipping["status"], "draft");
    assert_eq!(shipping["position"], 0);
    let about = create_page(&app, &seller, "about-us", "আমাদের কথা", true).await;
    assert_eq!(about["position"], 1);
    let hidden = create_page(&app, &seller, "wholesale", "পাইকারি", false).await;

    // 2. While all are drafts the public sees nothing:
    // the nav list is empty and a direct read is the
    // uniform 404.
    let (status, body) = call(&app, Method::GET, "/v1/stores/w59-pages/pages", None, None).await;
    assert_eq!(status, StatusCode::OK, "public page list: {body}");
    assert_eq!(body["total"], 0);
    assert_eq!(body["blog_post_count"], 0);
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-pages/pages/shipping-info",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "draft page is not public");

    // 3. Publish all three: the nav list carries the two
    // flagged ones IN CREATION ORDER (position), the
    // unflagged one is reachable directly but unlisted.
    for page in [&shipping, &about, &hidden] {
        let id = page["id"].as_str().expect("page id");
        set_page_status(&app, &seller, id, "published").await;
    }
    let (status, body) = call(&app, Method::GET, "/v1/stores/w59-pages/pages", None, None).await;
    assert_eq!(status, StatusCode::OK, "nav list: {body}");
    assert_eq!(body["total"], 2);
    assert_eq!(body["items"][0]["slug"], "shipping-info");
    assert_eq!(body["items"][1]["slug"], "about-us");
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-pages/pages/wholesale",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unflagged page reads: {body}");
    assert_eq!(body["title"], "পাইকারি");
    assert!(
        body["body"]
            .as_str()
            .expect("page body")
            .contains("দ্বিতীয় অনুচ্ছেদ"),
        "the full body rides the public read: {body}"
    );

    // 4. A no-op status write is refused (the state
    // machine's rule), and so is an unknown status.
    let shipping_id = shipping["id"].as_str().expect("page id").to_string();
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/store/pages/{shipping_id}/status"),
        Some(json!({ "status": "published" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no-op refused: {body}");
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/store/pages/{shipping_id}/status"),
        Some(json!({ "status": "live" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown status refused");

    // 5. Archiving removes the page from every public
    // surface again; the seller still sees it.
    set_page_status(&app, &seller, &shipping_id, "archived").await;
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-pages/pages/shipping-info",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "archived page is not public");
    let (status, body) = call(&app, Method::GET, "/v1/seller/store/pages", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "seller list: {body}");
    let seller_pages = body.as_array().expect("seller pages list");
    assert_eq!(seller_pages.len(), 3);
    assert_eq!(seller_pages[0]["status"], "archived");

    // 6. The same slug in a DIFFERENT store is a
    // different page: the other store's "about-us" is
    // its own, and this store's is untouched.
    let other = login(&app, "01722222222").await;
    create_store(&app, &other, "w59-other").await;
    let other_about = create_page(&app, &other, "about-us", "অন্য স্টোরের কথা", true).await;
    let other_id = other_about["id"].as_str().expect("page id").to_string();
    set_page_status(&app, &other, &other_id, "published").await;
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-other/pages/about-us",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "other store page: {body}");
    assert_eq!(body["title"], "অন্য স্টোরের কথা");
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-pages/pages/about-us",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "own page unchanged: {body}");
    assert_eq!(body["title"], "আমাদের কথা");
}
