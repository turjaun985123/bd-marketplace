//! Wave 14 integration tests: coupons and promotions.
//!
//! The money design under test (README, Wave 14):
//! - A platform coupon (percent 10%, capped at 5_000) on the
//!   wave-3-style two-store cart (goods 445_000) discounts exactly
//!   5_000 — the cap — allocated across both sub-orders summing
//!   exactly 5_000, the grand total drops by 5_000, and
//!   commissions are UNCHANGED (the platform bears the discount);
//!   the redemption row and used_count are recorded, and the
//!   receipt shows the discount.
//! - A store coupon (fixed 10_000 for store A) discounts only A's
//!   sub-order, A's commission is computed on the DISCOUNTED goods
//!   (the seller bears it), B is untouched; after delivery, A's
//!   earnings equal goods − discount − commission through the
//!   seller-payout snapshot sum.
//! - Rejections: min order unmet, expired, usage limit exhausted,
//!   inactive — each fails checkout with a 400 and leaves the
//!   cart intact.
//! - The preview endpoint agrees with checkout on validity and
//!   reasons.
//!
//! Skipped unless `DATABASE_URL` is set.

use api::{build_app, AppState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use courier::CouriersConfig;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

fn database_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok().filter(|u| !u.is_empty())
}

async fn app_with_pool() -> (axum::Router, PgPool) {
    let pool = PgPool::connect(&database_url().unwrap())
        .await
        .expect("connect to test database");
    api::run_migrations(&pool).await.expect("run migrations");
    sqlx::query(
        "TRUNCATE sidra_suggestions, messages, conversations, platform_settings, \
         coupon_redemptions, coupons, payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, order_items, sub_orders, \
         orders, cart_items, carts, products, stores, otp_codes, users \
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let state = AppState::new(Some(pool.clone()), "test-secret".to_string(), true)
        .with_couriers_config(CouriersConfig {
            dev_mode: true,
            ..Default::default()
        });
    (build_app(state), pool)
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(value) => builder
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn login(app: &axum::Router, phone: &str) -> String {
    let (status, body) = call(
        app,
        "POST",
        "/v1/auth/request-otp",
        None,
        Some(json!({ "phone": phone })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "request-otp {phone}: {body}");
    let dev_otp = body["dev_otp"]
        .as_str()
        .expect("dev mode returns dev_otp")
        .to_string();
    let (status, body) = call(
        app,
        "POST",
        "/v1/auth/verify-otp",
        None,
        Some(json!({ "phone": phone, "code": dev_otp })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "verify otp: {body}");
    body["token"].as_str().unwrap().to_string()
}

async fn create_store(app: &axum::Router, token: &str, slug: &str) -> Value {
    let (status, body) = call(
        app,
        "POST",
        "/v1/stores",
        Some(token),
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store: {body}");
    body
}

async fn create_product(
    app: &axum::Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
) -> Value {
    let (status, body) = call(
        app,
        "POST",
        "/v1/products",
        Some(token),
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": "active",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product: {body}");
    body
}

async fn add_to_cart(app: &axum::Router, token: &str, product_id: &str, quantity: i64) {
    let (status, body) = call(
        app,
        "POST",
        "/v1/cart/items",
        Some(token),
        Some(json!({ "product_id": product_id, "quantity": quantity })),
    )
    .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "add to cart: {status} {body}"
    );
}

fn checkout_body(coupon_code: Option<&str>) -> Value {
    let mut body = json!({
        "delivery_name": "Rahim Uddin",
        "delivery_phone": "01712345678",
        "delivery_address": "House 1, Road 2, Dhanmondi",
        "delivery_zone": "inside_dhaka",
        "payment_method": "cod",
    });
    if let Some(code) = coupon_code {
        body["coupon_code"] = json!(code);
    }
    body
}

async fn checkout(app: &axum::Router, token: &str, coupon_code: Option<&str>) -> (StatusCode, Value) {
    call(app, "POST", "/v1/orders", Some(token), Some(checkout_body(coupon_code))).await
}

async fn set_sub_order_status(app: &axum::Router, token: &str, id: &str, status: &str) {
    let (code, body) = call(
        app,
        "PATCH",
        &format!("/v1/seller/sub-orders/{id}"),
        Some(token),
        Some(json!({ "status": status })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "set sub-order {status}: {body}");
}

/// Confirm a sub-order and drive its DEV shipment to delivered.
async fn buy_and_deliver(app: &axum::Router, seller_token: &str, sub_order_id: &str) {
    set_sub_order_status(app, seller_token, sub_order_id, "confirmed").await;
    let (status, shipment) = call(
        app,
        "POST",
        &format!("/v1/seller/sub-orders/{sub_order_id}/shipment"),
        Some(seller_token),
        Some(json!({ "courier": "dev" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().unwrap().to_string();
    for _ in 0..4 {
        let (status, refreshed) = call(
            app,
            "POST",
            &format!("/v1/shipments/{shipment_id}/refresh"),
            Some(seller_token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "refresh shipment: {refreshed}");
    }
}

fn sub_order<'a>(order: &'a Value, store_id: &str) -> &'a Value {
    order["sub_orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|sub| sub["store_id"].as_str() == Some(store_id))
        .expect("sub-order for store")
}

#[tokio::test]
async fn wave14_coupons_end_to_end() {
    if database_url().is_none() {
        eprintln!("DATABASE_URL not set; skipping wave14 integration test");
        return;
    }
    let (app, pool) = app_with_pool().await;

    // --- Cast: admin, two sellers, one buyer. ---------------------
    let admin = login(&app, "+8801711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller_a = login(&app, "+8801722222222").await;
    let seller_b = login(&app, "+8801733333333").await;
    let buyer = login(&app, "+8801744444444").await;

    let store_a = create_store(&app, &seller_a, "wave14-a").await;
    let store_b = create_store(&app, &seller_b, "wave14-b").await;
    let store_a_id = store_a["id"].as_str().unwrap().to_string();
    let store_b_id = store_b["id"].as_str().unwrap().to_string();

    let product_a =
        create_product(&app, &seller_a, &store_a_id, "Wave14 A", 245_000, 10).await;
    let product_b =
        create_product(&app, &seller_b, &store_b_id, "Wave14 B", 200_000, 10).await;
    let product_a_id = product_a["id"].as_str().unwrap().to_string();
    let product_b_id = product_b["id"].as_str().unwrap().to_string();

    // The wave-3-style two-store cart: goods 445_000, delivery 24_000.
    async fn fill_cart(app: &axum::Router, buyer: &str, a: &str, b: &str) {
        add_to_cart(app, buyer, a, 1).await;
        add_to_cart(app, buyer, b, 1).await;
    }

    // --- Coupon creation: admin platform + seller store. ---------
    let (status, eid) = call(
        &app,
        "POST",
        "/v1/admin/coupons",
        Some(&admin),
        Some(json!({
            "code": "eid10",
            "discount_type": "percent",
            "discount_value": 10,
            "max_discount_poisha": 5_000,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "admin coupon: {eid}");
    assert_eq!(eid["code"], "EID10", "code stored normalized");
    assert!(eid["store_id"].is_null(), "platform coupon has no store");

    let (status, astore) = call(
        &app,
        "POST",
        "/v1/seller/coupons",
        Some(&seller_a),
        Some(json!({
            "code": "ASTORE",
            "discount_type": "fixed",
            "discount_value": 10_000,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "seller coupon: {astore}");
    assert_eq!(astore["store_id"], store_a_id);

    // Codes are platform-global: seller B cannot reuse EID10.
    let (status, _) = call(
        &app,
        "POST",
        "/v1/seller/coupons",
        Some(&seller_b),
        Some(json!({ "code": "EID10", "discount_type": "fixed", "discount_value": 1_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "duplicate code is a 409");

    // Validation: percent above 90 is a 400.
    let (status, _) = call(
        &app,
        "POST",
        "/v1/seller/coupons",
        Some(&seller_a),
        Some(json!({ "code": "TOOBIG", "discount_type": "percent", "discount_value": 95 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "percent 95 rejected");

    // A non-admin cannot create platform coupons.
    let (status, _) = call(
        &app,
        "POST",
        "/v1/admin/coupons",
        Some(&seller_a),
        Some(json!({ "code": "NOPE", "discount_type": "fixed", "discount_value": 1_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "admin coupons need an admin");

    // --- Preview: agrees with the design before any checkout. ----
    let (status, preview) = call(
        &app,
        "POST",
        "/v1/coupons/preview",
        Some(&buyer),
        Some(json!({ "code": "eid10", "goods_total_poisha": 445_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "preview: {preview}");
    assert_eq!(preview["valid"], true);
    assert_eq!(preview["discount_poisha"], 5_000, "10% capped at 5_000");
    assert_eq!(preview["scope"], "platform");

    let (status, preview) = call(
        &app,
        "POST",
        "/v1/coupons/preview",
        Some(&buyer),
        Some(json!({
            "code": "ASTORE",
            "goods_total_poisha": 445_000,
            "store_id": store_a_id,
            "store_goods_poisha": 245_000,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["valid"], true);
    assert_eq!(preview["discount_poisha"], 10_000);
    assert_eq!(preview["scope"], "store");

    let (status, preview) = call(
        &app,
        "POST",
        "/v1/coupons/preview",
        Some(&buyer),
        Some(json!({ "code": "MISSING", "goods_total_poisha": 445_000 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(preview["valid"], false);
    assert_eq!(preview["reason"], "coupon not found");

    // --- Checkout 1: platform coupon — platform bears it. --------
    fill_cart(&app, &buyer, &product_a_id, &product_b_id).await;
    let (status, order) = checkout(&app, &buyer, Some("eid10")).await;
    assert_eq!(status, StatusCode::CREATED, "checkout with EID10: {order}");
    assert_eq!(order["coupon_code"], "EID10");
    assert_eq!(order["discount_total_poisha"], 5_000);
    assert_eq!(order["goods_total_poisha"], 445_000);
    assert_eq!(
        order["grand_total_poisha"], 464_000,
        "469_000 without the coupon, minus the 5_000 discount"
    );
    let sub_a = sub_order(&order, &store_a_id);
    let sub_b = sub_order(&order, &store_b_id);
    let discount_a = sub_a["discount_poisha"].as_i64().unwrap();
    let discount_b = sub_b["discount_poisha"].as_i64().unwrap();
    assert_eq!(discount_a + discount_b, 5_000, "allocation sums exactly");
    assert_eq!(discount_a, 2_753, "proportional share, remainder to largest");
    assert_eq!(discount_b, 2_247);
    assert_eq!(
        sub_a["commission_poisha"], 19_600,
        "platform bears the discount: commission on full goods"
    );
    assert_eq!(sub_b["commission_poisha"], 16_000);
    assert_eq!(
        sub_a["seller_payout_poisha"], 222_647,
        "payout still nets the discount out (245_000 − 2_753 − 19_600)"
    );

    // The redemption is recorded and the use counted.
    let (redemption_discount,): (i64,) = sqlx::query_as(
        "SELECT discount_poisha FROM coupon_redemptions WHERE order_id = $1",
    )
    .bind(Uuid::parse_str(order["id"].as_str().unwrap()).unwrap())
    .fetch_one(&pool)
    .await
    .expect("redemption row exists");
    assert_eq!(redemption_discount, 5_000);
    let (status, admin_list) = call(&app, "GET", "/v1/admin/coupons", Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK);
    let eid_row = admin_list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "EID10")
        .expect("EID10 in admin list");
    assert_eq!(eid_row["used_count"], 1);

    // The receipt shows the discount.
    let order_id = order["id"].as_str().unwrap().to_string();
    let (status, receipt) = call(
        &app,
        "GET",
        &format!("/v1/orders/{order_id}/receipt"),
        Some(&buyer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "receipt: {receipt}");
    assert_eq!(receipt["discount_total_poisha"], 5_000);
    assert_eq!(receipt["coupon_code"], "EID10");
    assert_eq!(receipt["grand_total_poisha"], 464_000);

    // --- Checkout 2: store coupon — seller A bears it. -----------
    fill_cart(&app, &buyer, &product_a_id, &product_b_id).await;
    let (status, order2) = checkout(&app, &buyer, Some("ASTORE")).await;
    assert_eq!(status, StatusCode::CREATED, "checkout with ASTORE: {order2}");
    assert_eq!(order2["discount_total_poisha"], 10_000);
    assert_eq!(order2["grand_total_poisha"], 459_000);
    let sub_a2 = sub_order(&order2, &store_a_id);
    let sub_b2 = sub_order(&order2, &store_b_id);
    assert_eq!(sub_a2["discount_poisha"], 10_000, "only A's sub is discounted");
    assert_eq!(sub_b2["discount_poisha"], 0);
    assert_eq!(
        sub_a2["commission_poisha"], 18_800,
        "seller bears it: commission on 245_000 − 10_000"
    );
    assert_eq!(sub_b2["commission_poisha"], 16_000, "B untouched");
    assert_eq!(sub_a2["seller_payout_poisha"], 216_200);

    // Deliver A's sub-order: earnings come from the payout snapshot
    // (goods − discount − commission), not a recompute.
    let sub_a2_id = sub_a2["id"].as_str().unwrap().to_string();
    buy_and_deliver(&app, &seller_a, &sub_a2_id).await;
    let (status, balance) = call(
        &app,
        "GET",
        "/v1/seller/balance",
        Some(&seller_a),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(
        balance["earned_poisha"], 216_200,
        "delivered earnings = goods − discount − commission"
    );

    // --- Rejections: checkout fails, cart stays intact. ----------
    // Min order unmet (store A goods 245_000 < 300_000 minimum).
    let (status, _) = call(
        &app,
        "POST",
        "/v1/seller/coupons",
        Some(&seller_a),
        Some(json!({
            "code": "MINORD",
            "discount_type": "fixed",
            "discount_value": 5_000,
            "min_order_poisha": 300_000,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    add_to_cart(&app, &buyer, &product_a_id, 1).await;
    let (status, body) = checkout(&app, &buyer, Some("MINORD")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "min order unmet: {body}");
    let (status, cart) = call(&app, "GET", "/v1/cart", Some(&buyer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cart["items"].as_array().unwrap().len(), 1, "cart intact");
    let (status, _) = call(&app, "DELETE", "/v1/cart", Some(&buyer), None).await;
    assert_eq!(status, StatusCode::OK);

    // Expired coupon.
    let (status, _) = call(
        &app,
        "POST",
        "/v1/admin/coupons",
        Some(&admin),
        Some(json!({
            "code": "OLD10",
            "discount_type": "percent",
            "discount_value": 10,
            "ends_at": "2020-01-01T00:00:00Z",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    add_to_cart(&app, &buyer, &product_a_id, 1).await;
    let (status, body) = checkout(&app, &buyer, Some("OLD10")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "expired coupon: {body}");
    let (status, _) = call(&app, "DELETE", "/v1/cart", Some(&buyer), None).await;
    assert_eq!(status, StatusCode::OK);

    // Usage limit 1: first checkout succeeds, second is a 400.
    let (status, _) = call(
        &app,
        "POST",
        "/v1/admin/coupons",
        Some(&admin),
        Some(json!({
            "code": "ONCE5",
            "discount_type": "fixed",
            "discount_value": 5_000,
            "usage_limit": 1,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    add_to_cart(&app, &buyer, &product_a_id, 1).await;
    let (status, once_order) = checkout(&app, &buyer, Some("ONCE5")).await;
    assert_eq!(status, StatusCode::CREATED, "first use: {once_order}");
    assert_eq!(once_order["discount_total_poisha"], 5_000);
    add_to_cart(&app, &buyer, &product_a_id, 1).await;
    let (status, body) = checkout(&app, &buyer, Some("ONCE5")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "second use rejected: {body}");
    let (status, _) = call(&app, "DELETE", "/v1/cart", Some(&buyer), None).await;
    assert_eq!(status, StatusCode::OK);

    // Inactive coupon: seller deactivates ASTORE, checkout fails.
    let (status, seller_list) = call(&app, "GET", "/v1/seller/coupons", Some(&seller_a), None).await;
    assert_eq!(status, StatusCode::OK);
    let astore_id = seller_list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "ASTORE")
        .expect("ASTORE in seller list")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, toggled) = call(
        &app,
        "PATCH",
        &format!("/v1/seller/coupons/{astore_id}"),
        Some(&seller_a),
        Some(json!({ "active": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "toggle: {toggled}");
    assert_eq!(toggled["active"], false);
    add_to_cart(&app, &buyer, &product_a_id, 1).await;
    let (status, body) = checkout(&app, &buyer, Some("ASTORE")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "inactive coupon: {body}");

    // Another seller cannot toggle store A's coupon.
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/v1/seller/coupons/{astore_id}"),
        Some(&seller_b),
        Some(json!({ "active": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign coupon toggle is a 404");
}
