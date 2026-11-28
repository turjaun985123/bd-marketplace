//! Wave 60 integration test: checkout rejections and
//! partial redemption — a card whose store is not in the
//! cart is a 400 (never a silent ignore), an unknown code
//! is the same uniform 400, a rejected checkout leaves the
//! cart intact, a card bigger than the goods applies only
//! the goods' worth and keeps the rest, and a second order
//! drains the remainder to a depleted card that then
//! refuses further use.
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

/// Place a COD order with the given gift-card code
/// (None = no card); returns (status, body) unasserted.
async fn checkout_with_card(
    app: &Router,
    token: &str,
    gift_card_code: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = json!({
        "delivery_name": "Test Buyer",
        "delivery_phone": "01999999999",
        "delivery_address": "House 9, Road 3, Dhaka",
        "delivery_zone": "inside_dhaka",
        "payment_method": "cod"
    });
    if let Some(code) = gift_card_code {
        body["gift_card_code"] = json!(code);
    }
    call(app, Method::POST, "/v1/orders", Some(body), Some(token)).await
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
async fn wave60_wrong_store_unknown_and_partial_redemption() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller_a = login(&app, "01711111111").await;
    let seller_b = login(&app, "01722222222").await;
    let buyer = login(&app, "01733333333").await;
    let store_a = create_store(&app, &seller_a, "w60c-alpha").await;
    let store_b = create_store(&app, &seller_b, "w60c-beta").await;
    let product_a = create_product(&app, &seller_a, &store_a, "Alpha Thing", 60_000, 10).await;
    let product_b = create_product(&app, &seller_b, &store_b, "Beta Thing", 80_000, 10).await;

    // A ৳1,000 card for store A.
    let (_card, code_a) = issue_card(&app, &seller_a, 100_000).await;

    // 1. The cart holds only store B's product: store A's
    // card is a 400 naming the mismatch — and the cart
    // survives, so the same cart checks out cleanly with
    // no card right after.
    add_to_cart(&app, &buyer, &product_b, 1).await;
    let (status, body) = checkout_with_card(&app, &buyer, Some(&code_a)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "wrong-store card: {body}");
    let (status, order) = checkout_with_card(&app, &buyer, None).await;
    assert_eq!(status, StatusCode::CREATED, "cart intact, plain checkout: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 0);

    // 2. An unknown (but well-formed) code is a 400 with the
    // one uniform message; a malformed code is the SAME
    // message — checkout is not an enumeration oracle.
    add_to_cart(&app, &buyer, &product_a, 1).await;
    let (status, body_unknown) =
        checkout_with_card(&app, &buyer, Some("bdgc_BBBBBBBBBBBBBBBBBBBBBBBB")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown code: {body_unknown}");
    let (status, body_bad) = checkout_with_card(&app, &buyer, Some("hello")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "malformed code: {body_bad}");
    assert_eq!(body_unknown["error"], body_bad["error"], "uniform invalid-code message");

    // 3. Partial redemption: the ৳1,000 card against a ৳600
    // product applies exactly the goods (60_000) and keeps
    // 40_000. The grand total is delivery only.
    let (status, order) = checkout_with_card(&app, &buyer, Some(&code_a)).await;
    assert_eq!(status, StatusCode::CREATED, "partial checkout: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 60_000);
    assert_eq!(order["grand_total_poisha"], 6_000);
    let (status, balance) = balance_check(&app, &code_a).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["balance_poisha"], 40_000);
    assert_eq!(balance["status"], "active");

    // 4. A second order drains the rest: only 40_000 of the
    // 60_000 goods is card-paid; the buyer owes the other
    // 20_000 plus delivery in cash.
    add_to_cart(&app, &buyer, &product_a, 1).await;
    let (status, order) = checkout_with_card(&app, &buyer, Some(&code_a)).await;
    assert_eq!(status, StatusCode::CREATED, "drain checkout: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 40_000);
    assert_eq!(order["grand_total_poisha"], 26_000);
    let (status, balance) = balance_check(&app, &code_a).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["balance_poisha"], 0);
    assert_eq!(balance["status"], "depleted");

    // 5. The depleted card now refuses at checkout.
    add_to_cart(&app, &buyer, &product_a, 1).await;
    let (status, body) = checkout_with_card(&app, &buyer, Some(&code_a)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "depleted card: {body}");
}
