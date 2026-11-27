//! Wave 53 (part G) integration test: the saved-search
//! ROUND TRIP and the last discovery pins.
//!
//! The round trip is the saved-search feature's actual
//! promise: filters saved through the API, read back from
//! the list, rendered into a browse URL the way the web
//! page does it (lib/browse.ts's rules: repeated params
//! for the lists, `true` for the flags, poisha for the
//! bounds), return exactly the products the saver was
//! looking at. If the stored canonical form and the
//! browse validator ever drift, this test is where it
//! shows.
//!
//! Also pinned: description text is searchable (the
//! predicate covers title, Bangla title AND description),
//! the legacy `store_slug` and the new repeatable `store`
//! parameters MERGE rather than compete, and a saved
//! search's filters survive a delete-and-relist cycle
//! unchanged for the remaining entries.
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

/// Create an active product with a description; returns the id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    description: &str,
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
            "description": description,
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

/// Render stored saved-search filters into a browse query
/// string, following the web's rules (lib/browse.ts):
/// repeated params for the lists, `true` for the flags,
/// poisha integers for the bounds, the sort spelled out.
fn filters_to_query(filters: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(q) = filters["q"].as_str() {
        parts.push(format!("q={q}"));
    }
    for category in filters["categories"].as_array().expect("categories") {
        parts.push(format!(
            "category={}",
            category.as_str().expect("category")
        ));
    }
    for slug in filters["store_slugs"].as_array().expect("store slugs") {
        parts.push(format!("store={}", slug.as_str().expect("slug")));
    }
    if let Some(min) = filters["min_price_poisha"].as_i64() {
        parts.push(format!("min_price_poisha={min}"));
    }
    if let Some(max) = filters["max_price_poisha"].as_i64() {
        parts.push(format!("max_price_poisha={max}"));
    }
    if let Some(rating) = filters["min_rating"].as_f64() {
        parts.push(format!("min_rating={rating}"));
    }
    if filters["in_stock"].as_bool() == Some(true) {
        parts.push("in_stock=true".to_string());
    }
    if filters["on_sale"].as_bool() == Some(true) {
        parts.push("on_sale=true".to_string());
    }
    if let Some(sort) = filters["sort"].as_str() {
        parts.push(format!("sort={sort}"));
    }
    parts.join("&")
}

#[tokio::test]
async fn wave53g_saved_search_round_trip() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 53g integration test");
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

    let state = AppState::new(Some(pool.clone()), "wave53g-test-secret".to_string(), true);
    let app = build_app(state);

    let seller = login(&app, "01753537001").await;
    let shop = create_store(&app, &seller, "wave53g-shop").await;
    let seller2 = login(&app, "01753537002").await;
    let mart = create_store(&app, &seller2, "wave53g-mart").await;
    let buyer = login(&app, "01953537001").await;

    // The catalogue: a handwoven scarf (its searchable
    // word lives ONLY in the description), a pricier
    // scarf, and a hat in the other store.
    let scarf = create_product(
        &app,
        &seller,
        &shop,
        "Wave53G Scarf",
        "Handwoven on a wooden loom in Tangail",
        45_000,
        4,
    )
    .await;
    patch_category(&app, &seller, &scarf, "clothing").await;
    let shawl = create_product(
        &app,
        &seller,
        &shop,
        "Wave53G Shawl",
        "Machine-knit winter shawl",
        95_000,
        4,
    )
    .await;
    patch_category(&app, &seller, &shawl, "clothing").await;
    let hat = create_product(
        &app,
        &seller2,
        &mart,
        "Wave53G Hat",
        "A plain cotton hat",
        25_000,
        4,
    )
    .await;
    patch_category(&app, &seller2, &hat, "clothing").await;

    // ------------------------------------------------------------------
    // 1. Description text is searchable.
    // ------------------------------------------------------------------
    let results = browse(&app, "q=Handwoven").await;
    assert_eq!(results["total"], 1, "the description word: {results}");
    assert_eq!(results["items"][0]["id"], scarf);

    // ------------------------------------------------------------------
    // 2. The legacy and new store parameters merge (OR).
    // ------------------------------------------------------------------
    let results = browse(&app, "store_slug=wave53g-shop&store=wave53g-mart").await;
    assert_eq!(results["total"], 3, "both stores via both spellings: {results}");
    let results = browse(&app, "store_slug=wave53g-shop").await;
    assert_eq!(results["total"], 2, "the legacy spelling alone: {results}");

    // ------------------------------------------------------------------
    // 3. THE ROUND TRIP: save → list → render → browse.
    // ------------------------------------------------------------------
    let (status, saved) = call(
        &app,
        Method::POST,
        "/v1/buyer/saved-searches",
        Some(json!({
            "name": "সস্তা পোশাক",
            "filters": {
                "categories": ["clothing"],
                "max_price_poisha": 50_000,
                "in_stock": true,
                "sort": "price_asc"
            }
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "save: {saved}");
    let saved_id = saved["id"].as_str().expect("saved id").to_string();

    // A second save, deleted below; the first must survive
    // the cycle byte-identical.
    let (status, other) = call(
        &app,
        Method::POST,
        "/v1/buyer/saved-searches",
        Some(json!({ "name": "মুছে যাবে", "filters": { "q": "Hat" } })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "save other: {other}");
    let other_id = other["id"].as_str().expect("other id").to_string();
    let (status, _) = call(
        &app,
        Method::DELETE,
        &format!("/v1/buyer/saved-searches/{other_id}"),
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "delete other");

    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/buyer/saved-searches",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "list: {list}");
    assert_eq!(list["total"], 1);
    let stored = list["items"][0].clone();
    assert_eq!(stored["id"], saved_id);
    assert_eq!(
        stored["filters"], saved["filters"],
        "the stored form is stable across the cycle"
    );

    // Render the stored filters into a browse and run it:
    // clothing under ৳500 in stock, cheapest first = the
    // hat (25_000), then the scarf (45_000); the shawl is
    // priced out.
    let query = filters_to_query(&stored["filters"]);
    let results = browse(&app, &query).await;
    assert_eq!(results["total"], 2, "the round-tripped browse: {results}");
    assert_eq!(results["items"][0]["id"], hat, "cheapest first");
    assert_eq!(results["items"][1]["id"], scarf);

    // The same browse run by hand agrees exactly.
    let manual = browse(
        &app,
        "category=clothing&max_price_poisha=50000&in_stock=true&sort=price_asc",
    )
    .await;
    assert_eq!(manual["total"], results["total"]);
    assert_eq!(manual["items"][0]["id"], results["items"][0]["id"]);
}
