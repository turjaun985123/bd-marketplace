//! Wave 60 integration test: card lifecycle at checkout —
//! an expired card is refused (expiry is time-derived; the
//! test backdates the stored expiry the way the passage of
//! time would), a disabled card is refused, re-enabling
//! revives it, and disabling a partially-spent card
//! mid-life freezes exactly the remaining balance.
//!
//! Gated on `DATABASE_URL`: skips cleanly without one.

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
            eprintln!("DATABASE_URL not set — skipping Wave 60 integration tests");
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

/// Create one active product; returns the product id.
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

/// Issue one card with an explicit validity; returns (view, code).
async fn issue_card_with_validity(
    app: &Router,
    token: &str,
    amount_poisha: i64,
    validity_days: i64,
) -> (Value, String) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": amount_poisha, "validity_days": validity_days })),
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

/// Place a COD order with the given gift-card code.
async fn checkout_with_card(
    app: &Router,
    token: &str,
    gift_card_code: &str,
) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "gift_card_code": gift_card_code
        })),
        Some(token),
    )
    .await
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

#[tokio::test]
async fn wave60_expired_and_disabled_cards_are_refused() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let buyer = login(&app, "01733333333").await;
    let store_id = create_store(&app, &seller, "w60d-store").await;
    let product = create_product(&app, &seller, &store_id, "Thing", 60_000, 20).await;

    // 1. A card issued with 1-day validity, then aged past
    // its expiry in the database (exactly what the passage
    // of a day does to the derived status — no status flip
    // exists to set).
    let (card, code) = issue_card_with_validity(&app, &seller, 50_000, 1).await;
    let card_id = card["id"].as_str().expect("card id").to_string();
    sqlx::query("UPDATE gift_cards SET expires_at = now() - interval '1 hour' WHERE id = $1")
        .bind(sqlx::types::Uuid::parse_str(&card_id).expect("uuid"))
        .execute(&pool)
        .await
        .expect("age the card");
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["status"], "expired");
    assert_eq!(balance["balance_poisha"], 50_000, "expiry strands but never burns value silently");
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, body) = checkout_with_card(&app, &buyer, &code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "expired card refused: {body}");

    // 2. A live card the seller disables mid-life: refused
    // while disabled (balance intact), spendable again
    // after re-enable. The cart from step 1 is still
    // holding its product — every failed checkout left it
    // intact, and the successful one below consumes it.
    let (card2, code2) = issue_card_with_validity(&app, &seller, 80_000, 365).await;
    let card2_id = card2["id"].as_str().expect("card id").to_string();
    // Spend 60_000 of it first (the cart's product), leaving
    // 20_000 — the disable must freeze the REMAINDER.
    let (status, order) = checkout_with_card(&app, &buyer, &code2).await;
    assert_eq!(status, StatusCode::CREATED, "first spend: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 60_000);
    let uri = format!("/v1/seller/gift-cards/{card2_id}/disable");
    let (status, view) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "disable: {view}");
    assert_eq!(view["status"], "disabled");
    assert_eq!(view["balance_poisha"], 20_000);
    let (status, balance) = balance_check(&app, &code2).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["status"], "disabled");
    assert_eq!(balance["balance_poisha"], 20_000);
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, body) = checkout_with_card(&app, &buyer, &code2).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "disabled card refused: {body}");
    // Re-enable revives exactly the frozen remainder.
    let uri = format!("/v1/seller/gift-cards/{card2_id}/enable");
    let (status, view) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "enable: {view}");
    assert_eq!(view["status"], "active");
    let (status, order) = checkout_with_card(&app, &buyer, &code2).await;
    assert_eq!(status, StatusCode::CREATED, "spend after enable: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 20_000);
    let (status, balance) = balance_check(&app, &code2).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["balance_poisha"], 0);
    assert_eq!(balance["status"], "depleted");
}
