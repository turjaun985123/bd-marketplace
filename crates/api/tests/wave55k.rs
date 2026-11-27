//! Wave 55k integration test: timeline edge branches — a
//! seller-cancelled order's timeline + steps (placed done,
//! cancelled alert, nothing invented after), an
//! advance-due COD order's ORDER-level timeline carrying
//! the advance payment event, and the seller-PATCH
//! funnel's steps (confirmed current, the rest upcoming)
//! with the confirm event's honest `at: null`.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

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

/// COD-checkout the cart (delivery phone 01999999999);
/// returns the created order.
async fn checkout_cod(app: &Router, token: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 1, Road 1, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {body}");
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

/// The kinds of a timeline array, in order.
fn kinds(timeline: &Value) -> Vec<String> {
    timeline
        .as_array()
        .expect("timeline array")
        .iter()
        .map(|event| event["kind"].as_str().expect("kind").to_string())
        .collect()
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<Router> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55k integration tests");
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
         buyer_addresses, \
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
    let state = AppState::new(Some(pool), "wave55k-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some(build_app(state))
}

#[tokio::test]
async fn wave55k_timeline_cancelled_advance_and_patch_funnel() {
    let Some(app) = test_app().await else {
        return;
    };
    let seller = login(&app, "01719191919").await;
    let store = create_store(&app, &seller, "wave55k-store").await;
    let cheap = create_product(&app, &seller, &store, "Timeline Cheap", 10_000, 10).await;
    let pricey = create_product(&app, &seller, &store, "Timeline Pricey", 300_000, 5).await;
    let buyer = login(&app, "01919191919").await;

    // --- Cancelled: the story ends at the alert step. -------
    add_to_cart(&app, &buyer, &cheap, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
    let sub_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(&app, &seller, &sub_id, "cancelled").await;
    assert_eq!(status, StatusCode::OK, "seller cancel: {body}");

    let uri = format!("/v1/orders/{order_id}");
    let (status, fetched) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    let sub = &fetched["sub_orders"][0];
    assert_eq!(kinds(&sub["timeline"]), vec!["placed", "cancelled"]);
    let steps = sub["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["kind"], "placed");
    assert_eq!(steps[0]["state"], "done");
    assert_eq!(steps[1]["kind"], "cancelled");
    assert_eq!(steps[1]["state"], "alert");
    // The public track tells the same story.
    let uri = format!("/v1/track?order_id={order_id}&phone=01999999999");
    let (status, tracked) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "track cancelled: {tracked}");
    assert_eq!(
        kinds(&tracked["sub_orders"][0]["timeline"]),
        vec!["placed", "cancelled"]
    );

    // --- Advance-due: the order timeline carries the
    // advance payment event. --------------------------------
    add_to_cart(&app, &buyer, &pricey, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    assert_eq!(order["payment_status"], "advance_pending", "{order}");
    let order_id = order["id"].as_str().expect("order id").to_string();
    let uri = format!("/v1/track?order_id={order_id}&phone=01999999999");
    let (status, tracked) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "track advance: {tracked}");
    let order_kinds = kinds(&tracked["timeline"]);
    assert_eq!(order_kinds[0], "order_placed");
    assert!(
        order_kinds.iter().any(|kind| kind == "advance_initiated"),
        "the advance event is on the order timeline: {order_kinds:?}"
    );

    // --- PATCH funnel: confirmed is current, and its
    // event honestly carries no timestamp. ------------------
    add_to_cart(&app, &buyer, &cheap, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
    let sub_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(&app, &seller, &sub_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/orders/{order_id}");
    let (status, fetched) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    let sub = &fetched["sub_orders"][0];
    let steps = sub["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 4);
    assert_eq!(steps[0]["state"], "done");
    assert_eq!(steps[1]["kind"], "confirmed");
    assert_eq!(steps[1]["state"], "current");
    assert_eq!(steps[2]["state"], "upcoming");
    assert_eq!(steps[3]["state"], "upcoming");
    let confirmed = sub["timeline"]
        .as_array()
        .expect("timeline")
        .iter()
        .find(|event| event["kind"] == "confirmed")
        .expect("confirmed event")
        .clone();
    assert!(
        confirmed["at"].is_null(),
        "a seller confirm records no timestamp: {confirmed}"
    );
}
