//! Wave 60 integration test: the two documents at the
//! edges of a fully card-funded order. (1) The Wave 22
//! per-seller INVOICE nets the gift redemption out of its
//! money block exactly like the coupon discount — goods
//! − coupon − card + delivery — because the redemption is
//! a sub-order snapshot, not an order-level figure like
//! loyalty. (2) When coupon + card covered ALL the goods,
//! a return-received restores the whole redemption to
//! the card and the stored CASH refund is exactly zero:
//! the buyer's recovery is complete, and none of it is
//! cash nobody ever collected.
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
            eprintln!("DATABASE_URL not set — skipping Wave 60p integration tests");
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

/// Move one of the seller's sub-orders to a new status.
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

#[tokio::test]
async fn wave60_invoice_nets_the_card_and_zero_cash_refund() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01766666670").await;
    let buyer = login(&app, "01966666670").await;
    let store = create_store(&app, &seller, "wave60p-store").await;
    let product = create_product(&app, &seller, &store, "Wave60P Item", 200_000, 5).await;

    // A fixed 20_000 store coupon + a 180_000 card cover
    // the 200_000 of goods between them, exactly.
    let (status, coupon) = call(
        &app,
        Method::POST,
        "/v1/seller/coupons",
        Some(json!({
            "code": "FULL200",
            "discount_type": "fixed",
            "discount_value": 20_000
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "coupon: {coupon}");
    let (_card, code) = issue_card(&app, &seller, 180_000).await;

    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01966666670",
            "delivery_address": "House 9, Road 3, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "coupon_code": "FULL200",
            "gift_card_code": code
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 180_000);
    assert_eq!(order["grand_total_poisha"], 6_000, "delivery only");
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    // Drive the parcel to delivered through the DEV courier.
    let (status, body) = set_sub_order_status(&app, &seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(&app, Method::POST, &uri, Some(json!({})), Some(&seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    assert_eq!(shipment["cod_amount_poisha"], 6_000);
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }

    // (1) The invoice's money block: goods 200_000, coupon
    // 20_000, card 180_000, delivery 6_000, grand 6_000 —
    // and the cash due on delivery is the same 6_000.
    let uri = format!("/v1/buyer/sub-orders/{sub_order_id}/invoice");
    let (status, invoice) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "invoice: {invoice}");
    assert_eq!(invoice["money"]["goods_subtotal_poisha"], 200_000);
    assert_eq!(invoice["money"]["discount_poisha"], 20_000);
    assert_eq!(invoice["money"]["gift_card_discount_poisha"], 180_000);
    assert_eq!(invoice["money"]["delivery_charge_poisha"], 6_000);
    assert_eq!(invoice["money"]["grand_total_poisha"], 6_000);
    assert_eq!(invoice["money"]["cod_amount_due_poisha"], 6_000);

    // (2) Return it: the refund basis is goods − coupon =
    // 180_000, ALL of it card-funded, so the restore is
    // the full 180_000 and the stored cash refund is 0.
    let uri = format!("/v1/buyer/sub-orders/{sub_order_id}/return-request");
    let (status, request) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "reason": "ভুল সাইজ — wrong size" })),
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
    assert_eq!(received["refund_poisha"], 0, "no cash was ever collected for the goods");
    assert_eq!(received["refund_status"], "pending");
    let stored_refund: i64 = sqlx::query_scalar(
        "SELECT refund_poisha FROM return_requests WHERE id = $1::uuid",
    )
    .bind(&return_id)
    .fetch_one(&pool)
    .await
    .expect("stored refund");
    assert_eq!(stored_refund, 0);

    // The card holds its full value again, spendable.
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["balance_poisha"], 180_000);
    assert_eq!(balance["status"], "active");
}
