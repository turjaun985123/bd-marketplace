//! Wave 60 integration test: checkout redemption with FULL
//! money pinning — a two-store order where the card's store
//! also has a store coupon. Pins, end to end: the coupon
//! allocates first, the gift card applies to the post-coupon
//! goods, commission is computed on goods − coupon − card
//! (seller-funded), the payout snapshot nets the card out,
//! the grand total and the receipt's COD due drop by the
//! redemption, the courier's booking COD excludes the
//! card-funded slice, and the card ends depleted.
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
async fn wave60_checkout_redemption_full_money_pinning() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller_a = login(&app, "01711111111").await;
    let seller_b = login(&app, "01722222222").await;
    let buyer = login(&app, "01733333333").await;
    let store_a = create_store(&app, &seller_a, "w60-alpha").await;
    let store_b = create_store(&app, &seller_b, "w60-beta").await;
    let product_a = create_product(&app, &seller_a, &store_a, "Alpha Widget", 200_000, 10).await;
    let product_b = create_product(&app, &seller_b, &store_b, "Beta Gadget", 245_000, 10).await;

    // Store A's fixed ৳200 store coupon.
    let (status, coupon) = call(
        &app,
        Method::POST,
        "/v1/seller/coupons",
        Some(json!({
            "code": "ALPHA200",
            "discount_type": "fixed",
            "discount_value": 20_000
        })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "store coupon: {coupon}");

    // Store A's ৳500 gift card.
    let (_card, code) = issue_card(&app, &seller_a, 50_000).await;

    // The buyer carts one of each and checks out COD inside
    // Dhaka with BOTH the coupon and the card.
    add_to_cart(&app, &buyer, &product_a, 1).await;
    add_to_cart(&app, &buyer, &product_b, 1).await;
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "coupon_code": "ALPHA200",
            "gift_card_code": code
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");

    // Order level: goods 445_000, coupon 20_000, card 50_000,
    // delivery 12_000 → grand 387_000.
    assert_eq!(order["goods_total_poisha"], 445_000);
    assert_eq!(order["discount_total_poisha"], 20_000);
    assert_eq!(order["gift_card_discount_poisha"], 50_000);
    assert_eq!(order["gift_card_last4"], &code[code.len() - 4..]);
    assert_eq!(order["delivery_total_poisha"], 12_000);
    assert_eq!(order["grand_total_poisha"], 387_000);

    // Sub-order A: commission on 200_000 − 20_000 − 50_000
    // = 130_000 at 8% = 10_400; payout nets everything out.
    let subs = order["sub_orders"].as_array().expect("sub-orders");
    assert_eq!(subs.len(), 2);
    let sub_a = subs
        .iter()
        .find(|sub| sub["store_id"] == store_a)
        .expect("sub-order A");
    assert_eq!(sub_a["goods_subtotal_poisha"], 200_000);
    assert_eq!(sub_a["discount_poisha"], 20_000);
    assert_eq!(sub_a["gift_card_discount_poisha"], 50_000);
    assert_eq!(sub_a["commission_poisha"], 10_400);
    assert_eq!(sub_a["seller_payout_poisha"], 119_600);
    // Sub-order B is untouched by coupon and card alike.
    let sub_b = subs
        .iter()
        .find(|sub| sub["store_id"] == store_b)
        .expect("sub-order B");
    assert_eq!(sub_b["discount_poisha"], 0);
    assert_eq!(sub_b["gift_card_discount_poisha"], 0);
    assert_eq!(sub_b["commission_poisha"], 19_600);
    assert_eq!(sub_b["seller_payout_poisha"], 225_400);
    let sub_a_id = sub_a["id"].as_str().expect("sub A id").to_string();
    let order_id = order["id"].as_str().expect("order id").to_string();

    // The receipt: the gift-card line shows, and the COD due
    // is the reduced grand total.
    let uri = format!("/v1/orders/{order_id}/receipt");
    let (status, receipt) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "receipt: {receipt}");
    assert_eq!(receipt["gift_card_discount_poisha"], 50_000);
    assert_eq!(receipt["gift_card_last4"], &code[code.len() - 4..]);
    assert_eq!(receipt["discount_total_poisha"], 20_000);
    assert_eq!(receipt["grand_total_poisha"], 387_000);
    assert_eq!(receipt["cod_amount_due_poisha"], 387_000);

    // The card is fully spent: balance 0, derived status
    // depleted, and the seller's detail shows the redeem row.
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK, "balance after: {balance}");
    assert_eq!(balance["balance_poisha"], 0);
    assert_eq!(balance["status"], "depleted");
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/seller/gift-cards",
        None,
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller list: {list}");
    assert_eq!(list["items"][0]["redeemed_net_poisha"], 50_000);
    assert_eq!(list["items"][0]["status"], "depleted");
    let card_id = list["items"][0]["id"].as_str().expect("card id").to_string();
    let uri = format!("/v1/seller/gift-cards/{card_id}");
    let (status, detail) = call(&app, Method::GET, &uri, None, Some(&seller_a)).await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    let kinds: Vec<&str> = detail["ledger"]
        .as_array()
        .expect("ledger")
        .iter()
        .map(|row| row["kind"].as_str().expect("kind"))
        .collect();
    assert_eq!(kinds, ["redeem", "issue"], "newest first: {kinds:?}");
    assert_eq!(detail["ledger"][0]["delta_poisha"], -50_000);
    assert_eq!(detail["ledger"][0]["balance_after_poisha"], 0);
    assert_eq!(detail["ledger"][0]["order_id"], order_id);

    // Booking COD: seller A confirms and books — the courier
    // collects goods − coupon + delivery − card =
    // 200_000 − 20_000 + 6_000 − 50_000 = 136_000, never the
    // card-funded slice in cash.
    let uri = format!("/v1/seller/sub-orders/{sub_a_id}/status");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "confirmed" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub_a_id}/shipment");
    let (status, shipment) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({})),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    assert_eq!(shipment["cod_amount_poisha"], 136_000);

    // The seller sub-order view carries the redemption, so
    // the reduced payout never looks like a platform error.
    let (status, seller_orders) = call(
        &app,
        Method::GET,
        "/v1/seller/sub-orders",
        None,
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller sub-orders: {seller_orders}");
    assert_eq!(seller_orders["items"][0]["gift_card_discount_poisha"], 50_000);
    assert_eq!(seller_orders["items"][0]["seller_payout_poisha"], 119_600);
}
