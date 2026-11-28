//! Wave 60 integration test: the redemption ORDERING is
//! pinned where it is observable — coupon first, gift card
//! second, loyalty last. The buyer holds 300 points. On a
//! 60_000-goods order with a 6_000 store coupon and a
//! 20_000 gift card, loyalty's 50% cap base is
//! 60_000 − 6_000 − 20_000 = 34_000, so exactly 170 points
//! are usable (17_000 of discount). Had loyalty priced
//! against post-coupon-only goods (54_000), 270 points
//! would have been used — the assertion separates the two
//! orderings exactly.
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

/// Buy one unit COD (no extras) and drive it to delivered
/// through the DEV courier — the loyalty earn path.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) {
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
            "payment_method": "cod"
        })),
        Some(buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/status");
    let (status, body) = call(
        app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "confirmed" })),
        Some(seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(json!({})), Some(seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(app, Method::POST, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
}

#[tokio::test]
async fn wave60_redemption_ordering_coupon_then_card_then_loyalty() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let buyer = login(&app, "01733333333").await;
    let store_id = create_store(&app, &seller, "w60i-store").await;
    let big = create_product(&app, &seller, &store_id, "Big Ticket", 3_000_000, 3).await;
    let small = create_product(&app, &seller, &store_id, "Small Thing", 60_000, 5).await;

    // Earn 300 points: a delivered ৳30,000 order earns
    // 1 point per ৳100 of net goods.
    buy_and_deliver(&app, &buyer, &seller, &big).await;
    let (status, loyalty) = call(&app, Method::GET, "/v1/loyalty", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "loyalty: {loyalty}");
    assert_eq!(loyalty["available_points"], 300);

    // The store's fixed ৳60 coupon and a ৳200 gift card.
    let (status, coupon) = call(
        &app,
        Method::POST,
        "/v1/seller/coupons",
        Some(json!({
            "code": "SIXTY",
            "discount_type": "fixed",
            "discount_value": 6_000
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "coupon: {coupon}");
    let (status, issued) = call(
        &app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": 20_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "issue: {issued}");
    let code = issued["cards"][0]["code"]
        .as_str()
        .expect("code")
        .to_string();

    // The pinned order: 60_000 goods, all three discounts,
    // the buyer asking to spend ALL 300 points.
    add_to_cart(&app, &buyer, &small, 1).await;
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
            "coupon_code": "SIXTY",
            "gift_card_code": code,
            "loyalty_points": 300
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "pinned checkout: {order}");
    assert_eq!(order["discount_total_poisha"], 6_000);
    assert_eq!(order["gift_card_discount_poisha"], 20_000);
    // THE ORDERING PIN: cap base 34_000 → 170 points, not
    // the 270 a post-coupon-only base would have allowed.
    assert_eq!(order["loyalty_points_used"], 170);
    assert_eq!(order["loyalty_discount_poisha"], 17_000);
    // Grand: 60_000 − 6_000 − 20_000 − 17_000 + 6_000.
    assert_eq!(order["grand_total_poisha"], 23_000);
    // The sub-order carries all three figures, and loyalty
    // (platform-funded) did NOT touch the payout:
    // payout = 60_000 − 6_000 − 20_000 − commission.
    // Commission base (store coupon): 60_000 − 6_000 −
    // 20_000 = 34_000 → 8% = 2_720.
    let sub = &order["sub_orders"][0];
    assert_eq!(sub["discount_poisha"], 6_000);
    assert_eq!(sub["gift_card_discount_poisha"], 20_000);
    assert_eq!(sub["loyalty_discount_poisha"], 17_000);
    assert_eq!(sub["commission_poisha"], 2_720);
    assert_eq!(sub["seller_payout_poisha"], 31_280);

    // Points left: 300 − 170 = 130.
    let (status, loyalty) = call(&app, Method::GET, "/v1/loyalty", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "loyalty after: {loyalty}");
    assert_eq!(loyalty["available_points"], 130);
}
