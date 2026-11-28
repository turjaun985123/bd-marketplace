//! Wave 60 integration test: cancellation restores the
//! redemption — a buyer whole-order cancel and a seller
//! sub-order cancel each return the FULL redeemed amount
//! to the card in the cancel transaction (ledger row +
//! balance agree), and a restore that lands after the
//! card's expiry returns the balance WITHOUT extending
//! its life: the value is back, the card stays expired
//! and unredeemable.
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

/// Buy one unit of the product paying with the card;
/// returns the placed order.
async fn buy_with_card(app: &Router, buyer: &str, product_id: &str, code: &str) -> Value {
    add_to_cart(app, buyer, product_id, 1).await;
    let (status, order) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "gift_card_code": code
        })),
        Some(buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");
    order
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

/// The card's ledger kinds + the sum invariant, from SQL.
async fn ledger_kinds_and_sum(pool: &PgPool, card_id: &str) -> (Vec<String>, i64, i64) {
    let uuid = sqlx::types::Uuid::parse_str(card_id).expect("uuid");
    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM gift_card_ledger WHERE gift_card_id = $1 ORDER BY created_at, id",
    )
    .bind(uuid)
    .fetch_all(pool)
    .await
    .expect("ledger kinds");
    let (sum, balance): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(delta_poisha), 0)::bigint, \
         (SELECT balance_poisha FROM gift_cards WHERE id = $1) \
         FROM gift_card_ledger WHERE gift_card_id = $1",
    )
    .bind(uuid)
    .fetch_one(pool)
    .await
    .expect("ledger sum");
    (kinds, sum, balance)
}

#[tokio::test]
async fn wave60_cancels_restore_the_redemption_in_full() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let buyer = login(&app, "01733333333").await;
    let store_id = create_store(&app, &seller, "w60e-store").await;
    let product = create_product(&app, &seller, &store_id, "Thing", 200_000, 20).await;
    let (card, code) = issue_card(&app, &seller, 50_000).await;
    let card_id = card["id"].as_str().expect("card id").to_string();

    // 1. Buyer cancel: spend 50_000, cancel the order, the
    // full redemption returns — balance, status and ledger
    // all agree.
    let order = buy_with_card(&app, &buyer, &product, &code).await;
    assert_eq!(order["gift_card_discount_poisha"], 50_000);
    let order_id = order["id"].as_str().expect("order id").to_string();
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(balance["balance_poisha"], 0);
    let uri = format!("/v1/orders/{order_id}/cancel");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer cancel: {body}");
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(balance["balance_poisha"], 50_000);
    assert_eq!(balance["status"], "active");
    let (kinds, sum, balance) = ledger_kinds_and_sum(&pool, &card_id).await;
    assert_eq!(kinds, ["issue", "redeem", "cancel_restore"]);
    assert_eq!(sum, balance);
    assert_eq!(sum, 50_000);

    // 2. Seller cancel of the sub-order: same rule through
    // the other funnel.
    let order = buy_with_card(&app, &buyer, &product, &code).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/status");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "cancelled" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller cancel: {body}");
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(balance["balance_poisha"], 50_000);
    let (kinds, sum, _) = ledger_kinds_and_sum(&pool, &card_id).await;
    assert_eq!(
        kinds,
        ["issue", "redeem", "cancel_restore", "redeem", "cancel_restore"]
    );
    assert_eq!(sum, 50_000);

    // 3. Restore past expiry: spend again, age the card
    // past its expiry, then cancel. The balance returns in
    // full — but the expiry STANDS: the card reports
    // expired and checkout still refuses it. Restoring
    // value never rewrites the card's terms.
    let order = buy_with_card(&app, &buyer, &product, &code).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
    let uuid = sqlx::types::Uuid::parse_str(&card_id).expect("uuid");
    sqlx::query("UPDATE gift_cards SET expires_at = now() - interval '1 hour' WHERE id = $1")
        .bind(uuid)
        .execute(&pool)
        .await
        .expect("age the card");
    let uri = format!("/v1/orders/{order_id}/cancel");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cancel past expiry: {body}");
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(balance["balance_poisha"], 50_000, "the value came back");
    assert_eq!(balance["status"], "expired", "but the expiry stands");
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "gift_card_code": code
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "expired again: {body}");
}
