//! Wave 55o integration test: the MANUAL district path
//! in depth — checking out with `delivery_district` (no
//! address book) derives the zone from the district and
//! stores the district's CANONICAL name however it was
//! typed (case, surrounding spaces, Bangla name), while
//! the typed delivery address text stays exactly the
//! buyer's own words (composition is an address-book-only
//! behaviour). A 500-char order note is accepted; the
//! note rides to the seller view verbatim.
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

/// Manual checkout with an explicit district + zone field
/// (the zone deliberately disagrees in some calls — the
/// district must win). Returns (status, body).
async fn checkout_with_district(
    app: &Router,
    token: &str,
    district: &str,
    zone: &str,
    note: Option<&str>,
) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "বাসা ৭, রোড ২, সদর",
            "delivery_zone": zone,
            "delivery_district": district,
            "payment_method": "cod",
            "order_note": note
        })),
        Some(token),
    )
    .await
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<Router> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55o integration tests");
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
    let state = AppState::new(Some(pool), "wave55o-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some(build_app(state))
}

#[tokio::test]
async fn wave55o_manual_district_canonicalisation_and_note_boundary() {
    let Some(app) = test_app().await else {
        return;
    };
    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "wave55o-store").await;
    let product = create_product(&app, &seller, &store, "District Product", 10_000, 30).await;
    let buyer = login(&app, "01922222222").await;

    // Typed sloppily (" gazipur ") with a disagreeing zone:
    // the stored district is canonical, the zone is the
    // district's, and the delivery charge is the SUBURB
    // first-kg rate (10_000) — not the typed zone's.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = checkout_with_district(&app, &buyer, " gazipur ", "inside_dhaka", None).await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");
    assert_eq!(order["delivery_district"], "Gazipur");
    assert_eq!(order["delivery_zone"], "dhaka_suburb");
    assert_eq!(order["delivery_total_poisha"], 10_000);
    // The manual path keeps the buyer's own address words —
    // composition happens only for address-book checkouts.
    assert_eq!(order["delivery_address"], "বাসা ৭, রোড ২, সদর");

    // The Bangla district name works the same way.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = checkout_with_district(&app, &buyer, "গাজীপুর", "outside_dhaka", None).await;
    assert_eq!(status, StatusCode::CREATED, "bangla district checkout: {order}");
    assert_eq!(order["delivery_district"], "Gazipur");
    assert_eq!(order["delivery_zone"], "dhaka_suburb");

    // A note of exactly 500 characters is accepted and
    // reaches the seller view verbatim.
    let note = "ক".repeat(500);
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) =
        checkout_with_district(&app, &buyer, "Dhaka", "inside_dhaka", Some(&note)).await;
    assert_eq!(status, StatusCode::CREATED, "500-char note checkout: {order}");
    assert_eq!(order["order_note"], note);
    let (status, seller_list) = call(
        &app,
        Method::GET,
        "/v1/seller/sub-orders?limit=10",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let with_note = seller_list["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|row| row["order_note"] == note)
        .expect("the note reached the seller list")
        .clone();
    assert_eq!(with_note["delivery_district"], "Dhaka");

    // The fulfillment queue (Wave 40 surface) carries the
    // same note + district for the packing bench.
    let uri = format!("/v1/seller/fulfillment?store_id={store}");
    let (status, queue) = call(&app, Method::GET, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "fulfillment queue: {queue}");
    let queued = queue["items"]
        .as_array()
        .expect("queue items")
        .iter()
        .find(|row| row["order_note"] == note)
        .expect("the note reached the fulfillment queue")
        .clone();
    assert_eq!(queued["delivery_district"], "Dhaka");
}
