//! Wave 53 (part E) integration test: browse edge cases
//! — the boundaries a faceted endpoint most often gets
//! wrong in production.
//!
//! - Bangla text matches through `title_bn` (the
//!   marketplace is Bangla-first; a search that only
//!   matched the English title would fail its shoppers);
//! - pagination past the end is an empty page with the
//!   total intact, never an error;
//! - `limit` wins over the legacy `per_page` when both
//!   arrive, and each spelling alone echoes in both
//!   envelope fields;
//! - an exact price point (min == max) is a valid band;
//! - rating bounds 0 and 5 are inside the rule;
//! - an empty result still returns the full facet block
//!   (null price bounds, empty histogram, the OTHER
//!   groups' counts alive) — an empty page is a state,
//!   not an error shape;
//! - suggestions stay silent below two characters and
//!   suggest stores as well as products;
//! - the products list paginates through its envelope;
//! - editing a product does not bump it in newest order
//!   (created_at is the sort key, not updated_at).
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

/// Create an active product with a Bangla title; returns the id.
async fn create_product_bn(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    title_bn: &str,
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
            "title_bn": title_bn,
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

/// Run one browse; returns the results envelope.
async fn browse(app: &Router, query: &str) -> Value {
    let (status, body) = call(app, Method::GET, &format!("/v1/search?{query}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "browse {query}: {body}");
    body
}

/// Percent-encode a query value (UTF-8 bytes, unreserved
/// ASCII kept) — the test's Bangla queries need it, and a
/// hand-rolled encoder keeps the fixture honest.
fn encode_query_value(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[tokio::test]
async fn wave53e_browse_edge_cases() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 53e integration test");
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

    let state = AppState::new(Some(pool.clone()), "wave53e-test-secret".to_string(), true);
    let app = build_app(state);

    let seller = login(&app, "01753534001").await;
    let store = create_store(&app, &seller, "wave53e-shop").await;

    // Three products: two shirts (one with a Bangla title
    // that shares no Latin text with its English title),
    // one pair of shoes.
    let alpha = create_product_bn(
        &app,
        &seller,
        &store,
        "Wave53E Alpha Shirt",
        "আলফা শার্ট",
        40_000,
        5,
    )
    .await;
    patch_category(&app, &seller, &alpha, "clothing").await;
    let beta = create_product_bn(
        &app,
        &seller,
        &store,
        "Wave53E Beta Shirt",
        "বেটা শার্ট",
        60_000,
        5,
    )
    .await;
    patch_category(&app, &seller, &beta, "clothing").await;
    let gamma = create_product_bn(
        &app,
        &seller,
        &store,
        "Wave53E Gamma Shoes",
        "গামা জুতা",
        80_000,
        0,
    )
    .await;
    patch_category(&app, &seller, &gamma, "footwear").await;

    // ------------------------------------------------------------------
    // 1. Bangla text search: the Bangla word for "shirt"
    //    finds both shirts through title_bn; a word only
    //    in one Bangla title finds only that product.
    // ------------------------------------------------------------------
    let results = browse(&app, &format!("q={}", encode_query_value("শার্ট"))).await;
    assert_eq!(results["total"], 2, "Bangla shirt search: {results}");
    let results = browse(&app, &format!("q={}", encode_query_value("আলফা"))).await;
    assert_eq!(results["total"], 1, "one Bangla title: {results}");
    assert_eq!(results["items"][0]["id"], alpha);

    // ------------------------------------------------------------------
    // 2. Pagination: past-the-end pages and the two
    //    page-size spellings.
    // ------------------------------------------------------------------
    let results = browse(&app, "page=99").await;
    assert_eq!(results["total"], 3, "total survives the page: {results}");
    assert_eq!(results["items"].as_array().expect("items").len(), 0);
    assert_eq!(results["page"], 99);

    let results = browse(&app, "limit=1&per_page=2").await;
    assert_eq!(results["per_page"], 1, "limit wins: {results}");
    assert_eq!(results["limit"], 1);
    assert_eq!(results["items"].as_array().expect("items").len(), 1);
    let results = browse(&app, "per_page=2").await;
    assert_eq!(results["per_page"], 2);
    assert_eq!(results["limit"], 2, "limit echoes per_page: {results}");
    assert_eq!(results["items"].as_array().expect("items").len(), 2);

    // ------------------------------------------------------------------
    // 3. An exact price point and the rating bounds.
    // ------------------------------------------------------------------
    let results = browse(&app, "min_price_poisha=60000&max_price_poisha=60000").await;
    assert_eq!(results["total"], 1, "the exact point: {results}");
    assert_eq!(results["items"][0]["id"], beta);
    let results = browse(&app, "min_rating=0").await;
    assert_eq!(results["total"], 3, "rating 0 admits all: {results}");
    let results = browse(&app, "min_rating=5").await;
    assert_eq!(results["total"], 0, "nothing is 5-star here: {results}");

    // ------------------------------------------------------------------
    // 4. The empty result's facet block: nulls and zeros,
    //    with the other groups' counts alive.
    // ------------------------------------------------------------------
    let results = browse(&app, "category=nonexistent").await;
    assert_eq!(results["total"], 0);
    assert!(results["facets"]["price"]["min_poisha"].is_null());
    assert!(results["facets"]["price"]["max_poisha"].is_null());
    assert_eq!(
        results["facets"]["price"]["histogram"]
            .as_array()
            .expect("histogram")
            .len(),
        0,
        "no span, no histogram: {results}"
    );
    let clothing = results["facets"]["categories"]
        .as_array()
        .expect("category facets")
        .iter()
        .find(|facet| facet["category"] == "clothing")
        .expect("clothing facet survives")
        .clone();
    assert_eq!(
        clothing["count"], 2,
        "the categories facet excludes its own filter even when empty: {results}"
    );
    for facet in results["facets"]["ratings"].as_array().expect("ratings") {
        assert_eq!(facet["count"], 0, "no rated products: {facet}");
    }

    // ------------------------------------------------------------------
    // 5. Suggestions: silent below two characters; stores
    //    ride along with products.
    // ------------------------------------------------------------------
    let (status, body) = call(&app, Method::GET, "/v1/search/suggest?q=W", None, None).await;
    assert_eq!(status, StatusCode::OK, "short suggest: {body}");
    assert_eq!(body["products"].as_array().expect("products").len(), 0);
    assert_eq!(body["categories"].as_array().expect("categories").len(), 0);
    assert_eq!(body["stores"].as_array().expect("stores").len(), 0);

    let (status, body) = call(&app, Method::GET, "/v1/search/suggest?q=wave53e", None, None).await;
    assert_eq!(status, StatusCode::OK, "suggest: {body}");
    assert_eq!(
        body["stores"][0]["slug"], "wave53e-shop",
        "the store is suggested: {body}"
    );

    // ------------------------------------------------------------------
    // 6. The products list paginates through its envelope,
    //    and an edit does not bump newest order.
    // ------------------------------------------------------------------
    let (status, list) = call(&app, Method::GET, "/v1/products?limit=2&page=2", None, None).await;
    assert_eq!(status, StatusCode::OK, "list page 2: {list}");
    assert_eq!(list["total"], 3);
    assert_eq!(list["page"], 2);
    assert_eq!(list["limit"], 2);
    assert_eq!(list["items"].as_array().expect("items").len(), 1);

    // Touch the OLDEST product; newest order must not move.
    patch_category(&app, &seller, &alpha, "clothing").await;
    let results = browse(&app, "sort=newest").await;
    assert_eq!(
        results["items"][0]["id"], gamma,
        "newest is created_at, not updated_at: {results}"
    );
    assert_eq!(results["items"][2]["id"], alpha);
}
