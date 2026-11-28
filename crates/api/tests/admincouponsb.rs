//! Admin coupons remake, file B: the W14 money canon end to end
//! through the remade admin surface, plus the deactivation flow.
//! A platform coupon (percent 10, capped at 5_000) over the
//! standard two-store cart discounts 5_000, allocated 2_753 /
//! 2_247, with commission on the FULL goods (the platform
//! bears it) and payouts netting the discount; the degraded
//! advance is recorded, never collected (this app has no
//! payments dev mode). An admin deactivation — reason required,
//! stamped on the row, audited — makes preview and checkout
//! reject with the SAME shared reason and leaves the cart,
//! orders and redemption counts untouched; reactivation
//! restores checkout and clears the stamps, with the outgoing
//! reason preserved in its audit detail. A store coupon's
//! deactivation notices the store owner in the same
//! transaction.
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
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

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

/// Log in, then promote to admin by SQL (login FIRST — the
/// promotion must land on the already-created user row).
async fn make_admin(app: &Router, pool: &PgPool, phone_local: &str, phone_canonical: &str) -> String {
    let token = login(app, phone_local).await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = $1")
        .bind(phone_canonical)
        .execute(pool)
        .await
        .expect("promote admin");
    token
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

/// Checkout the buyer's current cart with COD; returns (status, body).
async fn checkout_cod(app: &Router, token: &str, coupon_code: Option<&str>) -> (StatusCode, Value) {
    let mut body = json!({
        "delivery_name": "Test Buyer",
        "delivery_phone": "01999999999",
        "delivery_address": "House 9, Road 3, Khulna",
        "delivery_zone": "inside_dhaka",
        "payment_method": "cod"
    });
    if let Some(code) = coupon_code {
        body["coupon_code"] = json!(code);
    }
    call(app, Method::POST, "/v1/orders", Some(body), Some(token)).await
}

/// The sub-order of `order` belonging to `store_id`.
fn sub_order<'a>(order: &'a Value, store_id: &str) -> &'a Value {
    order["sub_orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|sub| sub["store_id"].as_str() == Some(store_id))
        .expect("sub-order for store")
}

/// Count audit rows for one coupon and action.
async fn audit_count(pool: &PgPool, coupon_id: &str, action: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_audit_log \
         WHERE target_type = 'coupon' AND target_id = $1 AND action = $2",
    )
    .bind(coupon_id)
    .bind(action)
    .fetch_one(pool)
    .await
    .expect("count audit rows")
}

/// Count notifications of one kind addressed to one phone.
async fn notice_count(pool: &PgPool, kind: &str, phone: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE kind = $1 AND phone = $2")
        .bind(kind)
        .bind(phone)
        .fetch_one(pool)
        .await
        .expect("count notifications")
}

#[tokio::test]
async fn admincoupons_money_canon_and_deactivation() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin coupons test B");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, api_keys, \
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

    // No payments config: over-threshold COD orders record the
    // advance requirement and proceed (the W17 degradation).
    let state = AppState::new(Some(pool.clone()), "admincouponsb-test-secret".to_string(), true);
    let app = build_app(state);

    let admin = make_admin(&app, &pool, "01711111111", "+8801711111111").await;
    let seller_a = login(&app, "01733333333").await;
    let store_a = create_store(&app, &seller_a, "coupons-b-a").await;
    let product_a = create_product(&app, &seller_a, &store_a, "Product A", 245_000, 10).await;
    let seller_b = login(&app, "01744444444").await;
    let store_b = create_store(&app, &seller_b, "coupons-b-b").await;
    let product_b = create_product(&app, &seller_b, &store_b, "Product B", 200_000, 10).await;
    let buyer = login(&app, "01999999999").await;

    // The platform coupon, created through the remade admin API.
    let (status, created) = call(
        &app,
        Method::POST,
        "/v1/admin/coupons",
        Some(json!({
            "code": "EID10",
            "discount_type": "percent",
            "discount_value": 10,
            "max_discount_poisha": 5000
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create EID10: {created}");
    let coupon_id = created["id"].as_str().expect("coupon id").to_string();

    // Preview agrees with what checkout will do.
    let (status, preview) = call(
        &app,
        Method::POST,
        "/v1/coupons/preview",
        Some(json!({ "code": "eid10", "goods_total_poisha": 445_000 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "preview: {preview}");
    assert_eq!(preview["valid"], true);
    assert_eq!(preview["discount_poisha"], 5_000);
    assert_eq!(preview["scope"], "platform");

    // Checkout: the wave14 canon numbers, unchanged.
    add_to_cart(&app, &buyer, &product_a, 1).await;
    add_to_cart(&app, &buyer, &product_b, 1).await;
    let (status, order) = checkout_cod(&app, &buyer, Some("EID10")).await;
    assert_eq!(status, StatusCode::CREATED, "checkout with EID10: {order}");
    assert_eq!(order["coupon_code"], "EID10");
    assert_eq!(order["goods_total_poisha"], 445_000);
    assert_eq!(order["discount_total_poisha"], 5_000);
    assert_eq!(order["grand_total_poisha"], 452_000);
    let sub_a = sub_order(&order, &store_a);
    let sub_b = sub_order(&order, &store_b);
    assert_eq!(sub_a["discount_poisha"], 2_753);
    assert_eq!(sub_b["discount_poisha"], 2_247);
    assert_eq!(sub_a["commission_poisha"], 19_600, "platform bears: commission on full goods");
    assert_eq!(sub_b["commission_poisha"], 16_000);
    assert_eq!(sub_a["seller_payout_poisha"], 222_647);
    assert_eq!(sub_b["seller_payout_poisha"], 181_753);
    // Degraded advance: 20% of the 452_000 grand total is
    // recorded as required, nothing is collected.
    assert_eq!(order["payment_status"], "pay_on_delivery");
    assert_eq!(order["advance_required_poisha"], 90_400);
    assert_eq!(order["advance_paid_poisha"], 0);

    let order_id = order["id"].as_str().expect("order id").to_string();
    let (redemption_discount,): (i64,) = sqlx::query_as(
        "SELECT discount_poisha FROM coupon_redemptions WHERE order_id = $1",
    )
    .bind(Uuid::parse_str(&order_id).unwrap())
    .fetch_one(&pool)
    .await
    .expect("redemption row exists");
    assert_eq!(redemption_discount, 5_000);

    // Deactivation without a reason is a 400 and writes nothing.
    let toggle_uri = format!("/v1/admin/coupons/{coupon_id}");
    let (status, response) = call(
        &app,
        Method::PATCH,
        &toggle_uri,
        Some(json!({ "active": false })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "deactivate without reason: {response}");
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.deactivate").await, 0);

    // Deactivation with a reason: stamped, audited.
    let reason = "ভুয়া প্রচারণা — misleading campaign";
    let (status, toggled) = call(
        &app,
        Method::PATCH,
        &toggle_uri,
        Some(json!({ "active": false, "reason": reason })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "deactivate: {toggled}");
    assert_eq!(toggled["active"], false);
    assert_eq!(toggled["admin_deactivated_reason"], reason);
    assert!(toggled["admin_deactivated_at"].is_string(), "stamp set: {toggled}");
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.deactivate").await, 1);

    // Preview and checkout now reject with the SAME shared reason.
    let (status, preview) = call(
        &app,
        Method::POST,
        "/v1/coupons/preview",
        Some(json!({ "code": "EID10", "goods_total_poisha": 445_000 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "preview after deactivation: {preview}");
    assert_eq!(preview["valid"], false);
    assert_eq!(preview["reason"], "coupon is not active");

    add_to_cart(&app, &buyer, &product_a, 1).await;
    add_to_cart(&app, &buyer, &product_b, 1).await;
    let (status, rejected) = checkout_cod(&app, &buyer, Some("EID10")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "checkout with dead coupon: {rejected}");
    let message = rejected["error"].as_str().expect("error message").to_string();
    assert!(
        message.contains("coupon is not active"),
        "checkout reason is the shared rejection reason: {message}"
    );

    // The failed checkout changed nothing: cart intact, no new
    // order, no new redemption, use count unmoved.
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cart: {cart}");
    assert_eq!(cart["items"].as_array().map(Vec::len), Some(2), "cart intact");
    let (orders_count, redemptions_count, used_count): (i64, i64, i32) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM orders), \
         (SELECT COUNT(*) FROM coupon_redemptions), \
         (SELECT used_count FROM coupons WHERE id = $1)",
    )
    .bind(Uuid::parse_str(&coupon_id).unwrap())
    .fetch_one(&pool)
    .await
    .expect("post-rejection counts");
    assert_eq!(orders_count, 1);
    assert_eq!(redemptions_count, 1);
    assert_eq!(used_count, 1);

    // A second deactivation is a no-op: 409, nothing more written.
    let (status, response) = call(
        &app,
        Method::PATCH,
        &toggle_uri,
        Some(json!({ "active": false, "reason": "again" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "double deactivate: {response}");
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.deactivate").await, 1);

    // Reactivation clears the stamps; the audit detail keeps the
    // outgoing reason.
    let (status, toggled) = call(
        &app,
        Method::PATCH,
        &toggle_uri,
        Some(json!({ "active": true })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reactivate: {toggled}");
    assert_eq!(toggled["active"], true);
    assert_eq!(toggled["admin_deactivated_reason"], Value::Null);
    assert_eq!(toggled["admin_deactivated_at"], Value::Null);
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.reactivate").await, 1);
    let (detail_text,): (String,) = sqlx::query_as(
        "SELECT detail::text FROM admin_audit_log \
         WHERE target_type = 'coupon' AND target_id = $1 AND action = 'coupon.reactivate'",
    )
    .bind(&coupon_id)
    .fetch_one(&pool)
    .await
    .expect("reactivate audit row");
    assert!(
        detail_text.contains("ভুয়া প্রচারণা"),
        "outgoing reason preserved in the reactivation audit detail: {detail_text}"
    );

    // Checkout works again with the same code.
    let (status, order2) = checkout_cod(&app, &buyer, Some("EID10")).await;
    assert_eq!(status, StatusCode::CREATED, "checkout after reactivation: {order2}");
    assert_eq!(order2["discount_total_poisha"], 5_000);
    let (used_count,): (i32,) = sqlx::query_as("SELECT used_count FROM coupons WHERE id = $1")
        .bind(Uuid::parse_str(&coupon_id).unwrap())
        .fetch_one(&pool)
        .await
        .expect("used count after second checkout");
    assert_eq!(used_count, 2);

    // A store coupon's deactivation notices its owner (account
    // phone), and so does its reactivation.
    let (status, store_coupon) = call(
        &app,
        Method::POST,
        "/v1/seller/coupons",
        Some(json!({ "code": "ASTOREB", "discount_type": "fixed", "discount_value": 10000 })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "seller coupon: {store_coupon}");
    let store_coupon_id = store_coupon["id"].as_str().expect("store coupon id").to_string();
    let store_toggle_uri = format!("/v1/admin/coupons/{store_coupon_id}");
    let (status, response) = call(
        &app,
        Method::PATCH,
        &store_toggle_uri,
        Some(json!({ "active": false, "reason": "নীতিমালা ভঙ্গ — policy breach" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "deactivate store coupon: {response}");
    assert_eq!(
        notice_count(&pool, "coupon_deactivated", "+8801733333333").await,
        1,
        "owner noticed once"
    );
    assert_eq!(notice_count(&pool, "coupon_reactivated", "+8801733333333").await, 0);
    let (status, response) = call(
        &app,
        Method::PATCH,
        &store_toggle_uri,
        Some(json!({ "active": true })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reactivate store coupon: {response}");
    assert_eq!(
        notice_count(&pool, "coupon_reactivated", "+8801733333333").await,
        1,
        "owner noticed on reactivation"
    );
}
