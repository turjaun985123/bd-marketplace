//! Wave 60 integration test: the gift card meets the Wave
//! 17 COD advance. The advance is measured against the
//! payable grand total — goods net of the gift-card
//! redemption (and of loyalty) plus delivery — so a card
//! that pulls an order's payable down also pulls its
//! required advance down, poisha for poisha: a ৳3,000
//! order with a ৳1,000 card owes the advance on ৳2,060,
//! not on ৳3,060. Pinned against the identical cart
//! without a card as the control.
//!
//! Gated on `DATABASE_URL`: skips cleanly so a plain local
//! `cargo test` (no database) still passes. CI provides a
//! Postgres 16 service and sets `DATABASE_URL`.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use courier::CouriersConfig;
use payments::PaymentsConfig;
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

/// Build the test app (payments + couriers DEV simulators
/// on, so an advance can actually be collected), or None
/// when no database is configured. Truncates every table
/// EXCEPT `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 60l integration tests");
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
         gift_card_ledger, gift_cards, \
         stores, otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let state = AppState::new(Some(pool.clone()), "wave60-test-secret".to_string(), true)
        .with_payments_config(PaymentsConfig {
            dev_mode: true,
            dev_secret: "wave60-test-dev-secret".to_string(),
            ..PaymentsConfig::default()
        })
        .with_couriers_config(CouriersConfig {
            dev_mode: true,
            ..CouriersConfig::default()
        });
    Some((build_app(state), pool))
}

/// POST a store for the token's user; returns the store id.
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

/// POST one active product; returns its id.
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

/// Issue one card for the caller's store; returns (view, code).
async fn issue_card(app: &Router, token: &str, amount_poisha: i64) -> (Value, String) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": amount_poisha })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "issue card: {body}");
    let card = body["cards"][0].clone();
    let code = card["code"]
        .as_str()
        .expect("full code shown once")
        .to_string();
    (card, code)
}

/// COD checkout naming the bkash advance method, with an
/// optional gift-card code.
async fn checkout_cod(
    app: &Router,
    token: &str,
    gift_card_code: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = json!({
        "delivery_name": "Test Buyer",
        "delivery_phone": "01966666666",
        "delivery_address": "House 3, Road 5, Dhaka",
        "delivery_zone": "inside_dhaka",
        "payment_method": "cod",
        "advance_method": "bkash"
    });
    if let Some(code) = gift_card_code {
        body["gift_card_code"] = json!(code);
    }
    call(app, Method::POST, "/v1/orders", Some(body), Some(token)).await
}

#[tokio::test]
async fn wave60_gift_card_reduces_the_cod_advance_base() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01766666662").await;
    let store = create_store(&app, &seller, "wave60l-store").await;
    // ৳3,000 product, 1 kg → inside-Dhaka delivery 6_000.
    let product = create_product(&app, &seller, &store, "Wave60L Pricey", 300_000, 10).await;
    let (_card, code) = issue_card(&app, &seller, 100_000).await;

    // With the card: payable = 300_000 − 100_000 + 6_000 =
    // 206_000, still over the 200_000 threshold, so the
    // order is advance_pending — for 20% of 206_000.
    let buyer = login(&app, "01966666662").await;
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = checkout_cod(&app, &buyer, Some(&code)).await;
    assert_eq!(status, StatusCode::CREATED, "card checkout: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 100_000);
    assert_eq!(order["grand_total_poisha"], 206_000);
    assert_eq!(order["payment_status"], "advance_pending");
    assert_eq!(
        order["advance_required_poisha"], 41_200,
        "advance is 20% of the card-reduced payable 206_000"
    );

    // Control: the identical cart without a card owes the
    // advance on the full 306_000.
    let buyer2 = login(&app, "01966666663").await;
    add_to_cart(&app, &buyer2, &product, 1).await;
    let (status, order) = checkout_cod(&app, &buyer2, None).await;
    assert_eq!(status, StatusCode::CREATED, "control checkout: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 0);
    assert_eq!(order["grand_total_poisha"], 306_000);
    assert_eq!(order["payment_status"], "advance_pending");
    assert_eq!(
        order["advance_required_poisha"], 61_200,
        "control advance is 20% of the unreduced 306_000"
    );

    // And the receipt for the card order carries the same
    // reduced advance (the receipt is the money authority).
    let buyer3 = login(&app, "01966666664").await;
    add_to_cart(&app, &buyer3, &product, 1).await;
    let (_card2, code2) = issue_card(&app, &seller, 250_000).await;
    let (status, order) = checkout_cod(&app, &buyer3, Some(&code2)).await;
    assert_eq!(status, StatusCode::CREATED, "big card checkout: {order}");
    // A ৳2,500 card against ৳3,000 of goods: payable =
    // 50_000 + 6_000 = 56_000 — BELOW the threshold, so no
    // advance is required at all and the order is plain COD.
    assert_eq!(order["gift_card_discount_poisha"], 250_000);
    assert_eq!(order["grand_total_poisha"], 56_000);
    assert_eq!(order["advance_required_poisha"], 0);
    assert_eq!(order["payment_status"], "pay_on_delivery");
    let order_id = order["id"].as_str().expect("order id").to_string();
    let uri = format!("/v1/orders/{order_id}/receipt");
    let (status, receipt) = call(&app, Method::GET, &uri, None, Some(&buyer3)).await;
    assert_eq!(status, StatusCode::OK, "receipt: {receipt}");
    assert_eq!(receipt["gift_card_discount_poisha"], 250_000);
    assert_eq!(receipt["advance_required_poisha"], 0);
    assert_eq!(receipt["cod_amount_due_poisha"], 56_000);
}
