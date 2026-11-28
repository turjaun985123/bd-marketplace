//! Wave 59 integration test (n): the seller's post
//! list tooling — the status filter narrows to exactly
//! one state, the text search matches title and slug
//! case-insensitively, an unknown status filter is a
//! 400 (not a silent empty list), and pagination
//! reports the true total while slicing in newest-
//! first order.
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

/// Create one post in a state; returns nothing.
async fn create_post(app: &Router, token: &str, slug: &str, title: &str, status: &str) {
    let (code, body) = call(
        app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": slug,
            "title": title,
            "body": format!("{title} body."),
            "status": status
        })),
        Some(token),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "post {slug}: {body}");
}

/// The seller list under a query string: (total, slugs).
async fn seller_list(app: &Router, token: &str, query: &str) -> (i64, Vec<String>) {
    let uri = format!("/v1/seller/store/posts{query}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "seller list {query}: {body}");
    let slugs = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["slug"].as_str().expect("slug").to_string())
        .collect();
    (body["total"].as_i64().expect("total"), slugs)
}

#[tokio::test]
async fn wave59n_seller_post_list_filters_search_and_pages() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01725111111").await;
    create_store(&app, &seller, "w59-list").await;
    create_post(&app, &seller, "eid-guide", "Eid Shopping Guide", "published").await;
    create_post(&app, &seller, "eid-recipes", "ঈদের রেসিপি", "draft").await;
    create_post(&app, &seller, "winter-care", "শীতের যত্ন", "published").await;
    create_post(&app, &seller, "old-news", "পুরনো খবর", "archived").await;

    // 1. Unfiltered: all four, newest first (old-news
    // was created last).
    let (total, slugs) = seller_list(&app, &seller, "").await;
    assert_eq!(total, 4);
    assert_eq!(slugs[0], "old-news");

    // 2. The status filter narrows exactly.
    let (total, slugs) = seller_list(&app, &seller, "?status=draft").await;
    assert_eq!((total, slugs.len()), (1, 1));
    assert_eq!(slugs[0], "eid-recipes");
    let (total, _) = seller_list(&app, &seller, "?status=published").await;
    assert_eq!(total, 2);
    let (total, _) = seller_list(&app, &seller, "?status=archived").await;
    assert_eq!(total, 1);

    // 3. An unknown status is a 400, never a silent
    // empty list.
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/seller/store/posts?status=live",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown status filter");

    // 4. Search matches the title (either script) and
    // the slug, case-insensitively.
    let (total, slugs) = seller_list(&app, &seller, "?q=EID").await;
    assert_eq!(total, 2, "EID matches title + slug: {slugs:?}");
    let (total, slugs) = seller_list(&app, &seller, "?q=winter").await;
    assert_eq!((total, slugs.len()), (1, 1));
    assert_eq!(slugs[0], "winter-care");
    let (total, _) = seller_list(&app, &seller, "?q=zzz-no-match").await;
    assert_eq!(total, 0);

    // 5. Pagination slices with the true total.
    let (total, page1) = seller_list(&app, &seller, "?limit=3&page=1").await;
    assert_eq!(total, 4);
    assert_eq!(page1.len(), 3);
    let (total, page2) = seller_list(&app, &seller, "?limit=3&page=2").await;
    assert_eq!(total, 4);
    assert_eq!(page2.len(), 1);
    assert!(page1.iter().all(|slug| !page2.contains(slug)), "pages do not overlap");
}
