//! Wave 53 (part I) integration test: the last sort and
//! facet pins — list-endpoint sorts, explicit sorts
//! staying pure under a text query, the price histogram's
//! stability under its own filter, combined flag
//! intersection, and the filter-value cap at the API.
//!
//! The histogram pin is the subtle one: the histogram is
//! computed over the exclude-price set, so picking a
//! price band must NOT collapse the histogram into that
//! band — the shopper still sees the whole distribution
//! and can widen. If the histogram ever moved under the
//! price filter, the facet convention would be broken in
//! the one place the brief names explicitly.
//!
//! Delivery here uses the PATCH status walk
//! (confirmed → shipped → delivered) — the sub-order
//! state machine the seller endpoints expose; the DEV
//! courier flow is pinned by the other parts.
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

/// PATCH a product as its seller.
async fn patch_product(app: &Router, token: &str, product_id: &str, patch: Value) {
    let (status, body) = call(
        app,
        Method::PATCH,
        &format!("/v1/products/{product_id}"),
        Some(patch),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch product: {body}");
}

/// Buy one unit and walk the sub-order to delivered via
/// the PATCH status machine; returns nothing.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_id, "quantity": 1 })),
        Some(buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {body}");
    let (status, order) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    for next in ["confirmed", "shipped", "delivered"] {
        let (status, body) = call(
            app,
            Method::PATCH,
            &format!("/v1/seller/sub-orders/{sub_order_id}/status"),
            Some(json!({ "status": next })),
            Some(seller),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "walk to {next}: {body}");
    }
}

/// Review a delivered product as a buyer (Wave 8 shape).
async fn review(app: &Router, token: &str, product_id: &str, rating: i64) {
    let (status, body) = call(
        app,
        Method::POST,
        &format!("/v1/products/{product_id}/reviews"),
        Some(json!({ "rating": rating })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "review: {body}");
}

/// Run one browse; returns the results envelope.
async fn browse(app: &Router, query: &str) -> Value {
    let (status, body) = call(app, Method::GET, &format!("/v1/search?{query}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "browse {query}: {body}");
    body
}

#[tokio::test]
async fn wave53i_sorts_histogram_stability_and_caps() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 53i integration test");
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

    let state = AppState::new(Some(pool.clone()), "wave53i-test-secret".to_string(), true);
    let app = build_app(state);

    let seller = login(&app, "01753539001").await;
    let store = create_store(&app, &seller, "wave53i-shop").await;
    let buyer = login(&app, "01953539001").await;

    let alpha = create_product(&app, &seller, &store, "Wave53I Alpha", 30_000, 5).await;
    patch_product(&app, &seller, &alpha, json!({ "category": "clothing" })).await;
    let beta = create_product(&app, &seller, &store, "Wave53I Beta", 90_000, 5).await;
    patch_product(
        &app,
        &seller,
        &beta,
        json!({ "category": "clothing", "compare_at_poisha": 120_000 }),
    )
    .await;
    let gamma = create_product(&app, &seller, &store, "Wave53I Gamma", 60_000, 5).await;
    patch_product(&app, &seller, &gamma, json!({ "category": "electronics" })).await;

    // Alpha is bought, delivered and rated 5 stars.
    buy_and_deliver(&app, &buyer, &seller, &alpha).await;
    review(&app, &buyer, &alpha, 5).await;

    // ------------------------------------------------------------------
    // 1. The products list's remaining sorts.
    // ------------------------------------------------------------------
    let (status, list) = call(&app, Method::GET, "/v1/products?sort=rating", None, None).await;
    assert_eq!(status, StatusCode::OK, "list rating: {list}");
    assert_eq!(list["items"][0]["id"], alpha, "the 5-star product leads");
    let (status, list) = call(&app, Method::GET, "/v1/products?sort=price_desc", None, None).await;
    assert_eq!(status, StatusCode::OK, "list price_desc: {list}");
    assert_eq!(list["items"][0]["id"], beta, "the dearest leads");

    // ------------------------------------------------------------------
    // 2. An explicit sort stays pure under a text query:
    //    price order, not relevance order.
    // ------------------------------------------------------------------
    let results = browse(&app, "q=Wave53I&sort=price_asc").await;
    assert_eq!(results["total"], 3);
    assert_eq!(results["items"][0]["id"], alpha, "cheapest first: {results}");
    assert_eq!(results["items"][2]["id"], beta, "dearest last: {results}");

    // ------------------------------------------------------------------
    // 3. The histogram does not move under its own filter.
    // ------------------------------------------------------------------
    let unfiltered = browse(&app, "").await;
    let histogram = unfiltered["facets"]["price"]["histogram"].clone();
    assert_eq!(
        histogram
            .as_array()
            .expect("histogram")
            .iter()
            .map(|bucket| bucket["count"].as_i64().expect("count"))
            .sum::<i64>(),
        3,
        "the whole set is banded: {unfiltered}"
    );
    let band = histogram
        .as_array()
        .expect("histogram")
        .iter()
        .find(|bucket| {
            bucket["min_poisha"].as_i64().expect("min") <= 60_000
                && 60_000 <= bucket["max_poisha"].as_i64().expect("max")
        })
        .expect("a band holds the gamma price")
        .clone();
    let filtered = browse(
        &app,
        &format!(
            "min_price_poisha={}&max_price_poisha={}",
            band["min_poisha"], band["max_poisha"]
        ),
    )
    .await;
    assert_eq!(
        filtered["facets"]["price"]["histogram"], histogram,
        "picking a band leaves the distribution intact"
    );
    assert_eq!(
        filtered["facets"]["price"]["min_poisha"],
        unfiltered["facets"]["price"]["min_poisha"],
        "and the span intact"
    );

    // ------------------------------------------------------------------
    // 4. Flag intersection: on-sale AND rated is empty;
    //    each alone finds its product.
    // ------------------------------------------------------------------
    let results = browse(&app, "on_sale=true").await;
    assert_eq!(results["total"], 1, "only beta is on sale: {results}");
    assert_eq!(results["items"][0]["id"], beta);
    let results = browse(&app, "min_rating=4").await;
    assert_eq!(results["total"], 1, "only alpha is rated: {results}");
    let results = browse(&app, "on_sale=true&min_rating=4").await;
    assert_eq!(results["total"], 0, "the intersection is empty: {results}");

    // ------------------------------------------------------------------
    // 5. The filter-value cap at the API: 21 categories
    //    is a 400, not a truncation.
    // ------------------------------------------------------------------
    let many = (0..21)
        .map(|index| format!("category=cat-{index}"))
        .collect::<Vec<_>>()
        .join("&");
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/search?{many}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "21 categories: {body}");
}
