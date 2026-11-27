//! Wave 52 (fourth test file): the home feed's RAILS — the
//! sections the other Wave 52 tests do not pin. New arrivals
//! are newest-first; best sellers is empty until a real
//! delivered sale exists and then orders by delivered units;
//! top rated contains only products with reviews; the
//! promoted rail contains exactly the products under a live
//! paid promotion. Every rail item carries the shared view's
//! sold_count, so the sections cannot quietly disagree.
//!
//! The small helpers are copied from the Wave 52 test on
//! purpose (integration test files stay self-contained).
//!
//! Gated on `DATABASE_URL`: without it the test prints a skip
//! notice and passes. CI provides a Postgres 16 service.

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

/// Add a product to the buyer's cart.
async fn add_to_cart(app: &Router, token: &str, product_id: &str, quantity: i64) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_id, "quantity": quantity })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {body}");
}

/// Checkout the buyer's current cart with COD; returns the order.
async fn checkout_cod(app: &Router, token: &str) -> Value {
    let (status, body) = call(
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
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {body}");
    body
}

/// Move one sub-order to a new status as its seller.
async fn set_sub_order_status(
    app: &Router,
    token: &str,
    sub_order_id: &str,
    status: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/status");
    call(
        app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": status })),
        Some(token),
    )
    .await
}

/// Buy `quantity` of a product and walk its sub-order to
/// delivered (the Wave 52 status walk).
async fn buy_and_deliver(
    app: &Router,
    buyer: &str,
    seller: &str,
    product_id: &str,
    quantity: i64,
) {
    add_to_cart(app, buyer, product_id, quantity).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    for status in ["confirmed", "shipped", "delivered"] {
        let (code, body) = set_sub_order_status(app, seller, &sub_order_id, status).await;
        assert_eq!(code, StatusCode::OK, "walk to {status}: {body}");
    }
}

/// The ids of one feed rail, in feed order.
fn rail_ids(feed: &Value, rail: &str) -> Vec<String> {
    feed[rail]
        .as_array()
        .unwrap_or_else(|| panic!("{rail} is an array: {feed}"))
        .iter()
        .map(|p| p["id"].as_str().expect("product id").to_string())
        .collect()
}

#[tokio::test]
async fn wave52d_home_feed_rails() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 52d integration test");
            return;
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
         product_images, product_variants, products, store_members, stores, \
         otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave52d-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave52d-store").await;
    let buyer = login(&app, "01999999999").await;

    // Created in this order: P1 oldest, P3 newest.
    let p1 = create_product(&app, &seller, &store, "Wave52D Alpha", 200_000, 20).await;
    let p2 = create_product(&app, &seller, &store, "Wave52D Beta", 100_000, 20).await;
    let p3 = create_product(&app, &seller, &store, "Wave52D Gamma", 50_000, 20).await;

    // ------------------------------------------------------------------
    // 1. Before any sale or review: new arrivals newest-first,
    //    and the honesty rails are EMPTY — best sellers does
    //    not pad with unsold products, top rated does not
    //    invent ratings, promoted does not invent sponsors.
    // ------------------------------------------------------------------
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "feed: {feed}");
    assert_eq!(
        rail_ids(&feed, "new_arrivals"),
        vec![p3.clone(), p2.clone(), p1.clone()],
        "new arrivals newest-first: {feed}"
    );
    assert_eq!(rail_ids(&feed, "best_sellers"), Vec::<String>::new());
    assert_eq!(rail_ids(&feed, "top_rated"), Vec::<String>::new());
    assert_eq!(rail_ids(&feed, "promoted"), Vec::<String>::new());
    assert!(feed["flash_sale"].is_null(), "no sale live: {feed}");
    // The featured-stores strip is empty too: the only store
    // is unverified, and the strip lists verified stores only.
    let featured = feed["featured_stores"].as_array().expect("featured stores");
    assert!(featured.is_empty(), "unverified store not featured: {feed}");

    // ------------------------------------------------------------------
    // 2. One delivered unit of P1: best sellers is exactly
    //    [P1]; a placed-but-undelivered P2 order changes
    //    nothing (placed is not sold).
    // ------------------------------------------------------------------
    buy_and_deliver(&app, &buyer, &seller, &p1, 1).await;
    add_to_cart(&app, &buyer, &p2, 3).await;
    let _placed = checkout_cod(&app, &buyer).await;
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rail_ids(&feed, "best_sellers"), vec![p1.clone()], "feed: {feed}");
    assert_eq!(feed["best_sellers"][0]["sold_count"], 1);

    // ------------------------------------------------------------------
    // 3. Two delivered units of P2 outrank P1's one; the
    //    buyer's 5-star review of P1 opens the top-rated rail
    //    with exactly P1.
    // ------------------------------------------------------------------
    buy_and_deliver(&app, &buyer, &seller, &p2, 2).await;
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        rail_ids(&feed, "best_sellers"),
        vec![p2.clone(), p1.clone()],
        "best sellers by delivered units: {feed}"
    );
    assert_eq!(feed["best_sellers"][0]["sold_count"], 2);

    let (status, review) = call(
        &app,
        Method::POST,
        &format!("/v1/products/{p1}/reviews"),
        Some(json!({ "rating": 5, "body": "Khub bhalo — excellent." })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "review: {review}");
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rail_ids(&feed, "top_rated"), vec![p1.clone()], "feed: {feed}");
    assert_eq!(feed["top_rated"][0]["rating_count"], 1);

    // ------------------------------------------------------------------
    // 4. A paid promotion puts exactly P3 on the promoted
    //    rail (the seller's earnings cover the 1-day charge),
    //    flagged as promoted on every rail it appears in.
    // ------------------------------------------------------------------
    let (status, bought) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/products/{p3}/promote"),
        Some(json!({ "days": 1 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "promote: {bought}");
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rail_ids(&feed, "promoted"), vec![p3.clone()], "feed: {feed}");
    let arrivals = feed["new_arrivals"].as_array().expect("arrivals");
    let gamma = arrivals
        .iter()
        .find(|p| p["id"] == p3)
        .expect("P3 among new arrivals");
    assert_eq!(gamma["is_promoted"], true, "promoted flag agrees: {gamma}");
    assert_eq!(gamma["sold_count"], 0, "promoted is not sold: {gamma}");
}
