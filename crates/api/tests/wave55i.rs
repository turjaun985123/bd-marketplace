//! Wave 55i integration test: the Wave 55 checkout inputs
//! are ADDITIVE ONLY — the money canon does not move.
//!
//! The same cart checked out twice — once with manual
//! fields, once with `address_id` pointing at an address
//! in the same zone — with the same platform coupon,
//! produces identical goods / discount / delivery / grand
//! totals AND identical seller payout + commission
//! snapshots. Loyalty redemption on the address path
//! discounts the buyer's total exactly like the manual
//! path while leaving the seller payout untouched (the
//! Wave 35 rule), and COD collection nets the loyalty
//! discount out.
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

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55i integration tests");
            return None;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE loyalty_points, notifications, admin_audit_log, otp_request_log, \
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
    let state = AppState::new(Some(pool.clone()), "wave55i-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// The money fields two equivalent checkouts must share.
fn money_snapshot(order: &Value) -> (i64, i64, i64, i64, i64, i64, i64) {
    let sub = &order["sub_orders"][0];
    (
        order["goods_total_poisha"].as_i64().expect("goods"),
        order["discount_total_poisha"].as_i64().expect("discount"),
        order["delivery_total_poisha"].as_i64().expect("delivery"),
        order["grand_total_poisha"].as_i64().expect("grand"),
        sub["seller_payout_poisha"].as_i64().expect("payout"),
        sub["commission_poisha"].as_i64().expect("commission"),
        order["cod_amount_due_poisha"].as_i64().expect("cod due"),
    )
}

#[tokio::test]
async fn wave55i_address_checkout_keeps_the_money_canon() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01715151515").await;
    let store = create_store(&app, &seller, "wave55i-store").await;
    let product = create_product(&app, &seller, &store, "Canon Product", 100_000, 50).await;
    let buyer = login(&app, "01915151515").await;
    // The admin: login FIRST, then promote.
    let admin = login(&app, "01916161616").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801916161616'")
        .execute(&pool)
        .await
        .expect("promote admin");
    // Two identical platform coupons (10%, capped 5_000) —
    // one per checkout, so no per-buyer redemption rule
    // can interfere with the comparison.
    for code in ["CANON10A", "CANON10B"] {
        let (status, body) = call(
            &app,
            Method::POST,
            "/v1/admin/coupons",
            Some(json!({
                "code": code,
                "discount_type": "percent",
                "discount_value": 10,
                "max_discount_poisha": 5_000
            })),
            Some(&admin),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "coupon {code}: {body}");
    }
    // The buyer's Dhaka address.
    let (status, address) = call(
        &app,
        Method::POST,
        "/v1/addresses",
        Some(json!({
            "label": "বাসা",
            "recipient_name": "Canon Buyer",
            "phone": "01712345678",
            "district": "Dhaka",
            "area": "Uttara",
            "address_line": "Sector 7, Road 2"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "address: {address}");
    let address_id = address["id"].as_str().expect("address id").to_string();

    // --- Order 1: manual checkout with coupon A. ------------
    add_to_cart(&app, &buyer, &product, 2).await;
    let (status, manual) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Canon Buyer",
            "delivery_phone": "01712345678",
            "delivery_address": "Sector 7, Road 2, Uttara, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "coupon_code": "CANON10A"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "manual checkout: {manual}");
    // goods 200_000, coupon min(10%, cap) = 5_000, delivery
    // 6_000 (2 × 500 g = 1 kg inside Dhaka), grand 201_000.
    let manual_money = money_snapshot(&manual);
    assert_eq!(manual_money.0, 200_000);
    assert_eq!(manual_money.1, 5_000);
    assert_eq!(manual_money.2, 6_000);
    assert_eq!(manual_money.3, 201_000);

    // --- Order 2: address checkout with coupon B. -----------
    add_to_cart(&app, &buyer, &product, 2).await;
    let (status, via_address) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Ignored",
            "delivery_phone": "01999999999",
            "delivery_address": "Ignored",
            "delivery_zone": "outside_dhaka",
            "payment_method": "cod",
            "address_id": address_id,
            "coupon_code": "CANON10B"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "address checkout: {via_address}");
    assert_eq!(
        money_snapshot(&via_address),
        manual_money,
        "the address path must not move a single poisha"
    );

    // --- Loyalty on the address path. -----------------------
    // Earn first: a delivered order of 100_000 goods earns
    // 10 points (1 per 10_000 poisha of net goods).
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, earn_order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Canon Buyer",
            "delivery_phone": "01712345678",
            "delivery_address": "Sector 7, Road 2, Uttara, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "earn checkout: {earn_order}");
    let earn_sub = earn_order["sub_orders"][0]["id"]
        .as_str()
        .expect("earn sub-order")
        .to_string();
    let (status, body) = set_sub_order_status(&app, &seller, &earn_sub, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{earn_sub}/shipment");
    let (status, shipment) = call(&app, Method::POST, &uri, Some(json!({})), Some(&seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
    let (status, loyalty) = call(&app, Method::GET, "/v1/loyalty", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "loyalty: {loyalty}");
    assert_eq!(loyalty["available_points"], 10, "earn landed: {loyalty}");

    // Redeem all 10 points (1_000 poisha) on an ADDRESS
    // checkout: grand = 100_000 + 6_000 − 1_000 = 105_000,
    // and the seller payout ignores the loyalty discount.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, redeemed) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Ignored",
            "delivery_phone": "01999999999",
            "delivery_address": "Ignored",
            "delivery_zone": "outside_dhaka",
            "payment_method": "cod",
            "address_id": address_id,
            "loyalty_points": 10
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "redeem checkout: {redeemed}");
    assert_eq!(redeemed["loyalty_discount_poisha"], 1_000);
    assert_eq!(redeemed["grand_total_poisha"], 105_000);
    let sub = &redeemed["sub_orders"][0];
    assert_eq!(sub["goods_subtotal_poisha"], 100_000);
    assert_eq!(sub["commission_poisha"], 8_000, "8% of goods");
    assert_eq!(
        sub["seller_payout_poisha"], 92_000,
        "loyalty never reduces the seller payout"
    );
    assert_eq!(
        sub["cod_collect_poisha"], 105_000,
        "COD collection nets the loyalty discount out"
    );
}
