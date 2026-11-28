//! Wave 59 integration test (b): page reordering and
//! the merge update — a reorder must name the store's
//! EXACT current page set (a partial list, a duplicate,
//! or a stranger's page id are all refused and nothing
//! moves), the landed order drives the public nav, an
//! edit merges (only the named fields change, a blank
//! title is refused), a slug can move but never onto a
//! sibling, and a delete removes the page everywhere.
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

/// Create one published nav page (pages are born
/// drafts — publishing is the status action); returns
/// the page view.
async fn create_published_page(app: &Router, token: &str, slug: &str, title: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/seller/store/pages",
        Some(json!({
            "slug": slug,
            "title": title,
            "body": format!("{title} body."),
            "show_in_nav": true
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create page {slug}: {body}");
    let page_id = body["id"].as_str().expect("page id").to_string();
    let (status, body) = call(
        app,
        Method::POST,
        &format!("/v1/seller/store/pages/{page_id}/status"),
        Some(json!({ "status": "published" })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "publish page {slug}: {body}");
    body
}

/// The public nav slugs for a store, in served order.
async fn nav_slugs(app: &Router, store_slug: &str) -> Vec<String> {
    let uri = format!("/v1/stores/{store_slug}/pages");
    let (status, body) = call(app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "nav list: {body}");
    body["items"]
        .as_array()
        .expect("nav items")
        .iter()
        .map(|item| item["slug"].as_str().expect("slug").to_string())
        .collect()
}

#[tokio::test]
async fn wave59b_page_reorder_merge_update_and_delete() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01713111111").await;
    create_store(&app, &seller, "w59-reorder").await;
    let a = create_published_page(&app, &seller, "alpha", "আলফা").await;
    let b = create_published_page(&app, &seller, "beta", "বিটা").await;
    let c = create_published_page(&app, &seller, "gamma", "গামা").await;
    let id = |page: &Value| page["id"].as_str().expect("page id").to_string();
    let (a_id, b_id, c_id) = (id(&a), id(&b), id(&c));
    assert_eq!(nav_slugs(&app, "w59-reorder").await, ["alpha", "beta", "gamma"]);

    // 1. A partial list is refused and nothing moves.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/pages",
        Some(json!({ "page_ids": [c_id, a_id] })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "partial reorder: {body}");
    assert_eq!(nav_slugs(&app, "w59-reorder").await, ["alpha", "beta", "gamma"]);

    // 2. A duplicate is refused too.
    let (status, _) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/pages",
        Some(json!({ "page_ids": [c_id, a_id, a_id] })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "duplicate reorder refused");

    // 3. A stranger's page id makes the set wrong: a
    // second store's page cannot be smuggled in.
    let other = login(&app, "01713222222").await;
    create_store(&app, &other, "w59-reorder-other").await;
    let foreign = create_published_page(&app, &other, "delta", "ডেল্টা").await;
    let foreign_id = id(&foreign);
    let (status, _) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/pages",
        Some(json!({ "page_ids": [c_id, b_id, foreign_id] })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "foreign id in reorder refused");
    assert_eq!(nav_slugs(&app, "w59-reorder").await, ["alpha", "beta", "gamma"]);

    // 4. The full reversed set lands, and the nav (and
    // the seller list) follow it.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/pages",
        Some(json!({ "page_ids": [c_id, b_id, a_id] })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reorder: {body}");
    assert_eq!(nav_slugs(&app, "w59-reorder").await, ["gamma", "beta", "alpha"]);

    // 5. Merge update: only the named fields change —
    // the title moves, the slug and body stay.
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/pages/{b_id}"),
        Some(json!({ "title": "বিটা (সংশোধিত)" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "merge update: {body}");
    assert_eq!(body["title"], "বিটা (সংশোধিত)");
    assert_eq!(body["slug"], "beta");
    assert_eq!(body["body"], "বিটা body.");
    assert_eq!(body["position"], 1, "position survives a merge update");

    // 6. A blank title is refused; a slug move onto a
    // sibling is a 409; a free slug move works.
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/pages/{b_id}"),
        Some(json!({ "title": "   " })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "blank title refused");
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/pages/{b_id}"),
        Some(json!({ "slug": "alpha" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "slug onto sibling refused");
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/pages/{b_id}"),
        Some(json!({ "slug": "beta-v2" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "slug move: {body}");
    assert_eq!(nav_slugs(&app, "w59-reorder").await, ["gamma", "beta-v2", "alpha"]);

    // 7. A stranger cannot touch the page at all (the
    // scoped-store read is a 404), and neither can the
    // OTHER store's owner.
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/pages/{b_id}"),
        Some(json!({ "title": "হ্যাক" })),
        Some(&other),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign owner gets 404");
    let (status, _) = call(
        &app,
        Method::DELETE,
        &format!("/v1/seller/store/pages/{b_id}"),
        None,
        Some(&other),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign delete gets 404");

    // 8. Delete removes the page from the seller list,
    // the nav, and the direct public read.
    let (status, _) = call(
        &app,
        Method::DELETE,
        &format!("/v1/seller/store/pages/{b_id}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete page");
    assert_eq!(nav_slugs(&app, "w59-reorder").await, ["gamma", "alpha"]);
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-reorder/pages/beta-v2",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "deleted page is gone publicly");
    let (status, _) = call(
        &app,
        Method::DELETE,
        &format!("/v1/seller/store/pages/{b_id}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "second delete is a 404");
}
