//! Wave 60 integration test: a store-scoped card NEVER
//! spills. The cart holds two stores' goods; the card
//! belongs to the store with the SMALLER basket and is
//! worth more than that basket — the redemption stops at
//! that store's post-coupon goods exactly, the other
//! store's sub-order is untouched (full commission, full
//! payout, no gift line), and the unspent remainder stays
//! on the card for a later order at its own store. This
//! is the money reason platform-wide cards are out of
//! scope for the wave: scope is what makes the snapshot
//! arithmetic exact.
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
            eprintln!("DATABASE_URL not set — skipping Wave 60r integration tests");
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
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave60-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
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

/// The public balance check for a code.
async fn balance_check(app: &Router, code: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/gift-cards/balance",
        Some(json!({ "code": code })),
        None,
    )
    .await
}

/// The seller's own sub-order list (one item in these flows).
async fn seller_sub_orders(app: &Router, token: &str) -> Value {
    let (status, body) = call(
        app,
        Method::GET,
        "/v1/seller/sub-orders",
        None,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller sub-orders: {body}");
    body
}

#[tokio::test]
async fn wave60_card_value_never_spills_across_stores() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller_a = login(&app, "01766666673").await;
    let seller_b = login(&app, "01766666674").await;
    let store_a = create_store(&app, &seller_a, "wave60r-a").await;
    let store_b = create_store(&app, &seller_b, "wave60r-b").await;
    let product_a = create_product(&app, &seller_a, &store_a, "Wave60R A Item", 100_000, 5).await;
    let product_b = create_product(&app, &seller_b, &store_b, "Wave60R B Item", 30_000, 5).await;
    // Store B's card is worth 90_000 — three times B's
    // basket in this cart.
    let (_card, code_b) = issue_card(&app, &seller_b, 90_000).await;

    let buyer = login(&app, "01966666673").await;
    add_to_cart(&app, &buyer, &product_a, 1).await;
    add_to_cart(&app, &buyer, &product_b, 1).await;
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01966666673",
            "delivery_address": "House 3, Road 5, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "gift_card_code": code_b
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "two-store checkout: {order}");
    // The order total carries ONLY B's 30_000 of redemption.
    assert_eq!(order["gift_card_discount_poisha"], 30_000);
    assert_eq!(order["goods_total_poisha"], 130_000);
    assert_eq!(order["grand_total_poisha"], 112_000, "130_000 − 30_000 + 12_000 delivery");

    // Per-sub-order truth: A untouched, B fully card-funded.
    let subs_a = seller_sub_orders(&app, &seller_a).await;
    assert_eq!(subs_a["items"][0]["gift_card_discount_poisha"], 0);
    assert_eq!(subs_a["items"][0]["commission_poisha"], 8_000, "8% of the full 100_000");
    assert_eq!(subs_a["items"][0]["seller_payout_poisha"], 92_000);
    let subs_b = seller_sub_orders(&app, &seller_b).await;
    assert_eq!(subs_b["items"][0]["gift_card_discount_poisha"], 30_000);
    assert_eq!(subs_b["items"][0]["commission_poisha"], 0, "commission on 30_000 − 30_000");
    assert_eq!(subs_b["items"][0]["seller_payout_poisha"], 0);

    // The unspent 60_000 stayed on B's card — it did not
    // leak into A's sub-order, and it spends later at B.
    let (status, balance) = balance_check(&app, &code_b).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["balance_poisha"], 60_000);
    assert_eq!(balance["status"], "active");
    add_to_cart(&app, &buyer, &product_b, 1).await;
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01966666673",
            "delivery_address": "House 3, Road 5, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "gift_card_code": code_b
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "second B-only checkout: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 30_000);
    let (status, balance) = balance_check(&app, &code_b).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["balance_poisha"], 30_000);
}
