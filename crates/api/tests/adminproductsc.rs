//! Admin products remake, test C: the blast radius. A
//! taken-down product must vanish from EVERY public
//! surface — the product detail, the catalogue list,
//! faceted search and suggestions, the home feed, the
//! storefront list, the follows feed, the wishlist (add
//! AND list), the product's reviews and rating summary,
//! and its Q&A — while a control product in the same
//! store stays visible throughout, the store's team keeps
//! insider access, and a restore returns the product to
//! every surface (the wishlist row itself was never
//! deleted, only filtered).
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job.

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

/// Buy one unit of the product and drive the order all the way to
/// delivered through the DEV courier; returns the order id.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    let (status, body) = set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");

    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(json!({})), Some(seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();

    // Four polls walk the DEV courier created -> delivered.
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(app, Method::POST, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
}

/// Does this JSON array of product views contain the id?
fn contains_product(items: &Value, product_id: &str) -> bool {
    items
        .as_array()
        .map(|rows| rows.iter().any(|row| row["id"] == json!(product_id)))
        .unwrap_or(false)
}

#[tokio::test]
async fn adminproducts_takedown_blast_radius() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin products remake test C");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, return_requests, store_follows, \
         store_verification_events, store_verifications, platform_settings, \
         payouts, payout_accounts, review_replies, reviews, shipment_events, \
         shipments, payment_events, payments, order_items, sub_orders, orders, \
         cart_items, carts, products, api_keys, store_members, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(
        Some(pool.clone()),
        "adminproductsc-test-secret".to_string(),
        true,
    )
    .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "aprod-charlie").await;
    let target = create_product(&app, &seller, &store, "Blast Radius Widget", 120_000, 10).await;
    let control = create_product(&app, &seller, &store, "Control Widget", 80_000, 10).await;

    let buyer = login(&app, "01733333333").await;
    // The buyer follows the store, wishlists the target, asks
    // a question on it, and buys + reviews it — every
    // relationship a takedown must reach.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/aprod-charlie/follow",
        None,
        Some(&buyer),
    )
    .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "follow: {status} {body}"
    );
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/wishlist/{target}"),
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "wishlist add: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/products/{target}/questions"),
        Some(json!({ "body": "এটা কি আসল? Is this genuine?" })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "ask question: {body}");
    buy_and_deliver(&app, &buyer, &seller, &target).await;
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/products/{target}/reviews"),
        Some(json!({ "rating": 5, "body": "খুব ভালো" })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "review: {body}");

    // --- BEFORE: the target is visible on every surface. ---
    let (status, body) = call(&app, Method::GET, &format!("/v1/products/{target}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "anon detail before: {body}");
    let (status, body) = call(&app, Method::GET, "/v1/search?q=Blast%20Radius", None, None).await;
    assert_eq!(status, StatusCode::OK, "search before: {body}");
    assert!(contains_product(&body["items"], &target), "search before: {body}");
    let (status, body) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "home before: {body}");
    assert!(
        contains_product(&body["new_arrivals"], &target),
        "home new arrivals before: {body}"
    );
    let (status, body) = call(&app, Method::GET, "/v1/follows/feed", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "follows feed before: {body}");
    assert!(contains_product(&body, &target), "follows feed before: {body}");
    let (status, body) = call(&app, Method::GET, "/v1/wishlist", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "wishlist before: {body}");
    assert!(contains_product(&body["items"], &target), "wishlist before: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{target}/reviews"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reviews before: {body}");
    assert_eq!(body["total"], json!(1), "reviews before: {body}");

    // --- Take the target down. ---
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/products/{target}/takedown"),
        Some(json!({ "note": "নকল পণ্য" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "takedown: {body}");

    // --- AFTER: gone from every public surface. ---
    let (status, body) = call(&app, Method::GET, &format!("/v1/products/{target}"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "anon detail after: {body}");
    // The store's team keeps insider access to manage it.
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{target}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "owner detail after: {body}");
    assert!(body["admin_hidden_at"].is_string(), "owner sees the flag: {body}");

    let (status, body) = call(&app, Method::GET, "/v1/search?q=Blast%20Radius", None, None).await;
    assert_eq!(status, StatusCode::OK, "search after: {body}");
    assert_eq!(body["total"], json!(0), "search after: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/search/suggest?q=Blast",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suggest after: {body}");
    assert!(
        !body.to_string().contains(&target),
        "suggest after must not name the target: {body}"
    );
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/products?store=aprod-charlie",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "storefront list after: {body}");
    assert!(!contains_product(&body["items"], &target), "storefront: {body}");
    assert!(contains_product(&body["items"], &control), "control stays: {body}");
    let (status, body) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "home after: {body}");
    assert!(
        !contains_product(&body["new_arrivals"], &target),
        "home new arrivals after: {body}"
    );
    assert!(
        contains_product(&body["new_arrivals"], &control),
        "control stays on home: {body}"
    );
    let (status, body) = call(&app, Method::GET, "/v1/follows/feed", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "follows feed after: {body}");
    assert!(!contains_product(&body, &target), "follows feed after: {body}");
    assert!(contains_product(&body, &control), "control stays in feed: {body}");

    // The wishlist: the saved row is filtered out of the
    // list, and re-adding is a 404 through the public gate.
    let (status, body) = call(&app, Method::GET, "/v1/wishlist", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "wishlist after: {body}");
    assert!(!contains_product(&body["items"], &target), "wishlist after: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/wishlist/{target}"),
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "wishlist re-add after: {body}");

    // Reviews, rating summary and Q&A seal with the product.
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{target}/reviews"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "reviews after: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{target}/rating-summary"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "rating summary after: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{target}/questions"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "questions after: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{target}/related"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "related after: {body}");

    // The control product is untouched everywhere.
    let (status, body) = call(&app, Method::GET, &format!("/v1/products/{control}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "control detail: {body}");

    // --- Restore returns the target to the surfaces. ---
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/products/{target}/restore"),
        Some(json!({ "note": "প্রমাণ যাচাই হয়েছে" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "restore: {body}");
    let (status, body) = call(&app, Method::GET, &format!("/v1/products/{target}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "anon detail restored: {body}");
    let (status, body) = call(&app, Method::GET, "/v1/search?q=Blast%20Radius", None, None).await;
    assert_eq!(status, StatusCode::OK, "search restored: {body}");
    assert!(contains_product(&body["items"], &target), "search restored: {body}");
    // The wishlist row survived the takedown (filtered, not
    // deleted) and returns with the product.
    let (status, body) = call(&app, Method::GET, "/v1/wishlist", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "wishlist restored: {body}");
    assert!(contains_product(&body["items"], &target), "wishlist restored: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{target}/reviews"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reviews restored: {body}");
    assert_eq!(body["total"], json!(1), "reviews restored: {body}");
}
