//! Wave 60 integration test: two lifecycle edges.
//! (1) A card spent to zero reads `depleted` — and a
//! cancel restore brings it back to `active` with its
//! full balance: depletion is a derived state, not a
//! stored death. (2) Disabling a card AFTER an order has
//! redeemed it freezes only FUTURE spending: the placed
//! order keeps its snapshot, confirms, and books with the
//! card-funded slice still out of the courier's COD — a
//! freeze is not a clawback.
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
            eprintln!("DATABASE_URL not set — skipping Wave 60m integration tests");
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

/// COD checkout presenting a gift-card code.
async fn checkout_with_card(
    app: &Router,
    token: &str,
    code: &str,
) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01966666666",
            "delivery_address": "House 3, Road 5, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "gift_card_code": code
        })),
        Some(token),
    )
    .await
}

#[tokio::test]
async fn wave60_depleted_revives_on_restore_and_disable_never_claws_back() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01766666665").await;
    let store = create_store(&app, &seller, "wave60m-store").await;
    let pricey = create_product(&app, &seller, &store, "Wave60M Pricey", 50_000, 10).await;
    let cheap = create_product(&app, &seller, &store, "Wave60M Cheap", 30_000, 10).await;
    let buyer = login(&app, "01966666665").await;

    // (1) Spend a 50_000 card to zero → depleted. Cancel
    // the order → the restore revives it to active.
    let (_card, code) = issue_card(&app, &seller, 50_000).await;
    add_to_cart(&app, &buyer, &pricey, 1).await;
    let (status, order) = checkout_with_card(&app, &buyer, &code).await;
    assert_eq!(status, StatusCode::CREATED, "drain checkout: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 50_000);
    let order_id = order["id"].as_str().expect("order id").to_string();
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["status"], "depleted");
    assert_eq!(balance["balance_poisha"], 0);

    let uri = format!("/v1/orders/{order_id}/cancel");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer cancel: {body}");
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["status"], "active", "restore revives a depleted card");
    assert_eq!(balance["balance_poisha"], 50_000);

    // (2) A fresh order redeems 30_000 from an 80_000 card;
    // the seller then disables the card. The placed order
    // is untouched: it confirms and books with the COD
    // still net of the redemption (30_000 − 30_000 + 6_000
    // delivery = 6_000) — but a NEW checkout with the
    // frozen card is refused.
    let (card2, code2) = issue_card(&app, &seller, 80_000).await;
    let card2_id = card2["id"].as_str().expect("card2 id").to_string();
    add_to_cart(&app, &buyer, &cheap, 1).await;
    let (status, order) = checkout_with_card(&app, &buyer, &code2).await;
    assert_eq!(status, StatusCode::CREATED, "second checkout: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 30_000);
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    let uri = format!("/v1/seller/gift-cards/{card2_id}/disable");
    let (status, view) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "disable mid-flight: {view}");
    assert_eq!(view["status"], "disabled");
    assert_eq!(view["balance_poisha"], 50_000);

    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/status");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "confirmed" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "confirm placed order: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({})),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "book placed order: {shipment}");
    assert_eq!(
        shipment["cod_amount_poisha"], 6_000,
        "the freeze never claws back a placed redemption"
    );

    add_to_cart(&app, &buyer, &cheap, 1).await;
    let (status, body) = checkout_with_card(&app, &buyer, &code2).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "frozen card refused for new spending: {body}"
    );
}
