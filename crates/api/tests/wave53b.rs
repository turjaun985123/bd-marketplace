//! Wave 53 (part B) integration test: saved searches —
//! the buyer's own browse states, stored canonically and
//! applied back to the faceted browse.
//!
//! Pinned here:
//! - creating a saved search validates the name and the
//!   filters through the SAME browse validator and stores
//!   the canonical form (deduplicated lists, the sort
//!   spelled out, flags as booleans);
//! - the list returns only the caller's searches, newest
//!   first, with the total;
//! - the per-user cap is a bilingual 400;
//! - deleting another user's search is a 404 (ownership
//!   never leaks), deleting your own is a 204;
//! - the endpoints are login-gated (401);
//! - a saved search applied to `/v1/search` returns the
//!   filtered set it describes.
//!
//! Helpers follow the Wave 9 test (self-contained file);
//! gated on `DATABASE_URL` like every integration test.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
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

/// Create a store for the token's user; returns the store id.
async fn create_store(app: &Router, token: &str, slug: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body["id"].as_str().expect("store id").to_string()
}

/// Create an active product; returns the product id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// PATCH a product's category as its seller.
async fn patch_category(app: &Router, token: &str, product_id: &str, category: &str) {
    let (status, body) = call(
        app,
        Method::PATCH,
        &format!("/v1/products/{product_id}"),
        Some(json!({ "category": category })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch category: {body}");
}

/// Save one search for the token's user; returns (status, body).
async fn save_search(app: &Router, token: &str, name: &str, filters: Value) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/buyer/saved-searches",
        Some(json!({ "name": name, "filters": filters })),
        Some(token),
    )
    .await
}

#[tokio::test]
async fn wave53b_saved_searches_end_to_end() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 53b integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate (the Wave 52 truncate canon plus this
    // wave's tables).
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_images, product_variants, products, store_members, stores, \
         otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "wave53b-test-secret".to_string(), true);
    let app = build_app(state);

    // A tiny catalogue so an applied saved search has
    // something exact to find: two clothing products and
    // one electronics product in one store.
    let seller = login(&app, "01753531001").await;
    let store = create_store(&app, &seller, "wave53b-shop").await;
    let shirt = create_product(&app, &seller, &store, "Wave53B Shirt", 40_000, 5).await;
    patch_category(&app, &seller, &shirt, "clothing").await;
    let jeans = create_product(&app, &seller, &store, "Wave53B Jeans", 70_000, 5).await;
    patch_category(&app, &seller, &jeans, "clothing").await;
    let radio = create_product(&app, &seller, &store, "Wave53B Radio", 60_000, 5).await;
    patch_category(&app, &seller, &radio, "electronics").await;

    let buyer = login(&app, "01953531001").await;
    let other = login(&app, "01953531002").await;

    // Login-gated.
    let (status, body) = call(&app, Method::GET, "/v1/buyer/saved-searches", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous list: {body}");

    // Create: duplicates in the category list collapse,
    // the sort is stored spelled out, the name is trimmed.
    let (status, saved) = save_search(
        &app,
        &buyer,
        "  সস্তা পোশাক  ",
        json!({
            "categories": ["clothing", "clothing"],
            "max_price_poisha": 50_000,
            "in_stock": true,
            "sort": "price_asc"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "save search: {saved}");
    assert_eq!(saved["name"], "সস্তা পোশাক", "name trimmed: {saved}");
    assert_eq!(
        saved["filters"]["categories"],
        json!(["clothing"]),
        "categories deduplicated: {saved}"
    );
    assert_eq!(saved["filters"]["sort"], "price_asc");
    assert_eq!(saved["filters"]["in_stock"], true);
    assert_eq!(saved["filters"]["max_price_poisha"], 50_000);
    let saved_id = saved["id"].as_str().expect("saved id").to_string();

    // A second, plainer save (defaults canonicalise too:
    // no sort given + no query text = newest).
    let (status, second) = save_search(&app, &buyer, "সব ইলেকট্রনিক্স", json!({
        "categories": ["electronics"]
    }))
    .await;
    assert_eq!(status, StatusCode::CREATED, "save second: {second}");
    assert_eq!(second["filters"]["sort"], "newest");

    // The list is the caller's own, newest first.
    let (status, list) = call(&app, Method::GET, "/v1/buyer/saved-searches", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "list: {list}");
    assert_eq!(list["total"], 2);
    assert_eq!(list["items"][0]["id"], second["id"], "newest first: {list}");
    let (status, list) = call(&app, Method::GET, "/v1/buyer/saved-searches", None, Some(&other)).await;
    assert_eq!(status, StatusCode::OK, "other list: {list}");
    assert_eq!(list["total"], 0, "nobody else's searches: {list}");

    // Applying the first saved search to the browse
    // endpoint returns exactly the shirt (clothing under
    // ৳500, in stock).
    let (status, results) = call(
        &app,
        Method::GET,
        "/v1/search?category=clothing&max_price_poisha=50000&in_stock=true&sort=price_asc",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "applied browse: {results}");
    assert_eq!(results["total"], 1, "applied filters: {results}");
    assert_eq!(results["items"][0]["id"], shirt);

    // A save with EVERY field populated stores them all —
    // the canonical form is the whole filter, not a subset.
    let (status, full) = save_search(
        &app,
        &buyer,
        "সবকিছুসহ খোঁজ",
        json!({
            "q": "Wave53B",
            "categories": ["clothing"],
            "store_slugs": ["wave53b-shop"],
            "min_price_poisha": 10_000,
            "max_price_poisha": 80_000,
            "min_rating": 1.0,
            "in_stock": true,
            "on_sale": true,
            "sort": "rating"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "full save: {full}");
    assert_eq!(full["filters"]["q"], "Wave53B");
    assert_eq!(full["filters"]["store_slugs"], json!(["wave53b-shop"]));
    assert_eq!(full["filters"]["min_price_poisha"], 10_000);
    assert_eq!(full["filters"]["max_price_poisha"], 80_000);
    assert_eq!(full["filters"]["min_rating"].as_f64(), Some(1.0));
    assert_eq!(full["filters"]["in_stock"], true);
    assert_eq!(full["filters"]["on_sale"], true);
    assert_eq!(full["filters"]["sort"], "rating");

    // Invalid saves are 400s: bad filter, bad name.
    let (status, body) = save_search(&app, &buyer, "খারাপ", json!({ "min_rating": 9.0 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad filter: {body}");
    let (status, body) = save_search(&app, &buyer, "   ", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "blank name: {body}");
    let (status, body) = save_search(&app, &buyer, &"x".repeat(61), json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "long name: {body}");

    // The cap: the buyer already holds 3; 17 more reach
    // the limit of 20, and the 21st is refused.
    for index in 0..17 {
        let (status, body) = save_search(
            &app,
            &buyer,
            &format!("ভর্তি {index}"),
            json!({ "q": format!("filler {index}") }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "filler {index}: {body}");
    }
    let (status, body) = save_search(&app, &buyer, "একটি বেশি", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "over the cap: {body}");

    // Ownership: another user cannot delete it (404, no
    // leak); the owner can (204); a second delete is 404.
    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/buyer/saved-searches/{saved_id}"),
        None,
        Some(&other),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign delete: {body}");
    let (status, _) = call(
        &app,
        Method::DELETE,
        &format!("/v1/buyer/saved-searches/{saved_id}"),
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "own delete");
    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/buyer/saved-searches/{saved_id}"),
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "second delete: {body}");
}
