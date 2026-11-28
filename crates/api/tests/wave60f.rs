//! Wave 60 integration test: a return received on a
//! gift-card-funded sub-order restores the redemption to
//! the card and reduces the stored CASH refund by exactly
//! the restored amount — the buyer recovers the rest as
//! card balance, the total recovery is unchanged, and
//! nobody is paid twice. Figures pinned exactly: goods
//! 200_000, store coupon 20_000, card 50_000 → refund
//! basis 180_000, restored 50_000, stored cash refund
//! 130_000.
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
async fn wave60_return_received_restores_card_and_nets_the_cash_refund() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let buyer = login(&app, "01733333333").await;
    let store_id = create_store(&app, &seller, "w60f-store").await;
    let product = create_product(&app, &seller, &store_id, "Returnable Thing", 200_000, 5).await;

    // The store's fixed ৳200 coupon and ৳500 gift card.
    let (status, coupon) = call(
        &app,
        Method::POST,
        "/v1/seller/coupons",
        Some(json!({
            "code": "RET200",
            "discount_type": "fixed",
            "discount_value": 20_000
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "coupon: {coupon}");
    let (status, issued) = call(
        &app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": 50_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "issue: {issued}");
    let code = issued["cards"][0]["code"]
        .as_str()
        .expect("code")
        .to_string();
    let card_id = issued["cards"][0]["id"]
        .as_str()
        .expect("card id")
        .to_string();

    // Buy with coupon + card, then drive the parcel to
    // delivered through the DEV courier.
    add_to_cart(&app, &buyer, &product, 1).await;
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
            "coupon_code": "RET200",
            "gift_card_code": code
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 50_000);
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(&app, &seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(&app, Method::POST, &uri, Some(json!({})), Some(&seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(balance["balance_poisha"], 0, "card spent before the return");

    // The buyer requests the return; the seller approves
    // and marks it received.
    let uri = format!("/v1/buyer/sub-orders/{sub_order_id}/return-request");
    let (status, request) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "reason": "ভাঙা অবস্থায় পৌঁছেছে — arrived damaged" })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "return request: {request}");
    let return_id = request["id"].as_str().expect("return id").to_string();
    let uri = format!("/v1/seller/return-requests/{return_id}/approve");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "approve: {body}");
    let uri = format!("/v1/seller/return-requests/{return_id}/received");
    let (status, received) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "received: {received}");

    // THE PIN: refund basis = goods − coupon = 180_000; the
    // card-funded 50_000 returns to the CARD, so the stored
    // cash refund is 130_000 — and 130_000 + 50_000 =
    // 180_000, the buyer's full recovery, paid exactly once.
    assert_eq!(received["refund_poisha"], 130_000);
    assert_eq!(received["refund_status"], "pending");
    let stored_refund: i64 = sqlx::query_scalar(
        "SELECT refund_poisha FROM return_requests WHERE id = $1",
    )
    .bind(sqlx::types::Uuid::parse_str(&return_id).expect("uuid"))
    .fetch_one(&pool)
    .await
    .expect("stored refund");
    assert_eq!(stored_refund, 130_000);
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(balance["balance_poisha"], 50_000, "the redemption is back on the card");
    assert_eq!(balance["status"], "active");

    // The ledger tells the whole story and sums to the
    // balance: issue +50_000, redeem −50_000,
    // return_restore +50_000.
    let card_uuid = sqlx::types::Uuid::parse_str(&card_id).expect("uuid");
    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM gift_card_ledger WHERE gift_card_id = $1 ORDER BY created_at, id",
    )
    .bind(card_uuid)
    .fetch_all(&pool)
    .await
    .expect("ledger kinds");
    assert_eq!(kinds, ["issue", "redeem", "return_restore"]);
    let (sum, cached): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(delta_poisha), 0)::bigint, \
         (SELECT balance_poisha FROM gift_cards WHERE id = $1) \
         FROM gift_card_ledger WHERE gift_card_id = $1",
    )
    .bind(card_uuid)
    .fetch_one(&pool)
    .await
    .expect("ledger sum");
    assert_eq!(sum, cached);
    assert_eq!(sum, 50_000);
}
