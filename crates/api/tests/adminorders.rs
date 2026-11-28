//! Admin orders remake, test A: the admin order LIST is a
//! purpose-built triage row — the delivery phone is MASKED
//! and the full delivery block (phone, address, note) and
//! the order items are GONE from it (the pre-remake list
//! returned the buyer's full order view per row) — while
//! the new DETAIL endpoint returns the buyer's full view
//! plus the masked account phone and the receipt's COD
//! due. The standard two-store flow pins the money
//! (goods 445_000, delivery 12_000, grand 457_000, advance
//! 91_400). Helpers are copied from the Wave 9 test on
//! purpose — integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test returns
//! early, so a plain local `cargo test` (no database)
//! still passes. CI provides a Postgres 16 service and
//! sets `DATABASE_URL`.

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
    auth: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = auth {
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
async fn create_store(app: &Router, auth: &str, slug: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body["id"].as_str().expect("store id").to_string()
}

/// Create an active product; returns the product id.
async fn create_product(
    app: &Router,
    auth: &str,
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
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Add a product to the buyer's cart.
async fn add_to_cart(app: &Router, auth: &str, product_id: &str, quantity: i64) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_id, "quantity": quantity })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {body}");
}

/// Checkout the buyer's current cart with COD; returns the order.
async fn checkout_cod(app: &Router, auth: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01888888888",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {body}");
    body
}

/// Mint an API key with a JWT; returns the full key (shown once).
async fn create_key(app: &Router, jwt: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/api-keys",
        Some(json!({ "name": "orders-test", "scopes": ["read"], "expires_in_days": null })),
        Some(jwt),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create api key: {body}");
    body["key"].as_str().expect("full key once").to_string()
}

#[tokio::test]
async fn adminorders_list_masks_and_detail_composes() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin orders remake test A");
            return;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, return_requests, store_follows, \
         store_verification_events, store_verifications, platform_settings, \
         payouts, payout_accounts, review_replies, reviews, shipment_events, \
         shipments, payment_events, payments, order_items, sub_orders, orders, \
         cart_items, carts, products, api_keys, store_members, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "adminorders-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller_a = login(&app, "01722222222").await;
    let store_a = create_store(&app, &seller_a, "aorders-alpha").await;
    let product_a = create_product(&app, &seller_a, &store_a, "Orders A", 245_000, 10).await;
    let seller_b = login(&app, "01733333333").await;
    let store_b = create_store(&app, &seller_b, "aorders-beta").await;
    let product_b = create_product(&app, &seller_b, &store_b, "Orders B", 200_000, 10).await;

    // The standard two-store flow: goods 445_000, two
    // inside-Dhaka delivery charges of 6_000, grand 457_000.
    // A plain COD checkout over the 200_000 threshold with
    // no advance method is the CR-15 degraded shape:
    // advance_required 91_400 (20%), nothing collected.
    let buyer = login(&app, "01999999999").await;
    add_to_cart(&app, &buyer, &product_a, 1).await;
    add_to_cart(&app, &buyer, &product_b, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
    assert_eq!(order["goods_total_poisha"], 445_000);
    assert_eq!(order["grand_total_poisha"], 457_000);

    // The surface is admin-only: no token 401, buyer 403,
    // and an API key 403 even when it belongs to the admin
    // (the blanket /v1/admin/ key exclusion).
    let (status, body) = call(&app, Method::GET, "/v1/admin/orders", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no token: {body}");
    let (status, body) = call(&app, Method::GET, "/v1/admin/orders", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "buyer list: {body}");
    let detail_uri = format!("/v1/admin/orders/{order_id}");
    let (status, body) = call(&app, Method::GET, &detail_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "buyer detail: {body}");
    let api_key = create_key(&app, &admin).await;
    let (status, body) = call(&app, Method::GET, "/v1/admin/orders", None, Some(&api_key)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "api key list: {body}");
    let (status, body) = call(&app, Method::GET, &detail_uri, None, Some(&api_key)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "api key detail: {body}");

    // The list: envelope, the triage row, masking.
    let (status, list) = call(&app, Method::GET, "/v1/admin/orders", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin list: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["page"], 1);
    assert_eq!(list["limit"], 20);
    let row = &list["items"][0];
    assert_eq!(row["id"], order_id);
    assert_eq!(row["status"], "placed");
    assert_eq!(row["payment_method"], "cod");
    assert_eq!(row["payment_status"], "pay_on_delivery");
    assert_eq!(row["advance_required_poisha"], 91_400);
    assert_eq!(row["advance_paid_poisha"], 0);
    assert_eq!(row["advance_unfulfilled"], true);
    assert_eq!(row["delivery_name"], "Test Buyer");
    assert_eq!(row["delivery_phone_masked"], "018****888");
    assert_eq!(row["delivery_zone"], "inside_dhaka");
    assert_eq!(row["goods_total_poisha"], 445_000);
    assert_eq!(row["discount_total_poisha"], 0);
    assert_eq!(row["loyalty_discount_poisha"], 0);
    assert_eq!(row["gift_card_discount_poisha"], 0);
    assert_eq!(row["delivery_total_poisha"], 12_000);
    assert_eq!(row["grand_total_poisha"], 457_000);
    // The full delivery block and the heavy composition are
    // GONE from the list row.
    assert!(row.get("delivery_phone").is_none(), "row: {row}");
    assert!(row.get("delivery_address").is_none(), "row: {row}");
    assert!(row.get("order_note").is_none(), "row: {row}");
    assert!(row.get("items").is_none(), "row: {row}");
    let raw = list.to_string();
    assert!(!raw.contains("01888888888"), "full delivery phone leaked: {raw}");
    assert!(!raw.contains("+8801888888888"), "canonical phone leaked: {raw}");
    assert!(!raw.contains("House 9"), "delivery address leaked: {raw}");
    // Sub-order chips: both stores, pipeline status, goods.
    let subs = row["sub_orders"].as_array().expect("sub_orders array");
    assert_eq!(subs.len(), 2);
    let sub_a = subs
        .iter()
        .find(|s| s["store_id"] == store_a)
        .expect("alpha sub-order");
    assert_eq!(sub_a["store_slug"], "aorders-alpha");
    assert_eq!(sub_a["status"], "placed");
    assert_eq!(sub_a["goods_subtotal_poisha"], 245_000);
    let sub_b = subs
        .iter()
        .find(|s| s["store_id"] == store_b)
        .expect("beta sub-order");
    assert_eq!(sub_b["goods_subtotal_poisha"], 200_000);

    // The summary describes the whole queue.
    let summary = &list["summary"];
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["placed"], 1);
    assert_eq!(summary["cancelled"], 0);
    assert_eq!(summary["payment_pay_on_delivery"], 1);
    assert_eq!(summary["payment_advance_pending"], 0);
    assert_eq!(summary["advance_pending_count"], 0);
    assert_eq!(summary["advance_outstanding_poisha"], 0);
    assert_eq!(summary["advance_unfulfilled_count"], 1);
    assert_eq!(summary["advance_unfulfilled_poisha"], 91_400);
    assert_eq!(summary["orders_today"], 1);
    assert_eq!(summary["gmv_today_poisha"], 457_000);

    // The detail: the buyer's full view (full delivery
    // phone lives HERE only) plus the admin extras.
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin detail: {detail}");
    assert_eq!(detail["id"], order_id);
    assert_eq!(detail["delivery_phone"], "+8801888888888");
    assert_eq!(detail["delivery_address"], "House 9, Road 3, Khulna");
    assert_eq!(detail["goods_total_poisha"], 445_000);
    assert_eq!(detail["grand_total_poisha"], 457_000);
    assert_eq!(detail["buyer_phone_masked"], "019****999");
    assert_eq!(detail["cod_amount_due_poisha"], 457_000);
    let detail_subs = detail["sub_orders"].as_array().expect("detail sub-orders");
    assert_eq!(detail_subs.len(), 2);
    assert_eq!(detail_subs[0]["items"].as_array().expect("items").len(), 1);
    assert!(detail_subs[0]["timeline"].is_array(), "timeline: {detail}");
    assert!(detail_subs[0]["steps"].is_array(), "steps: {detail}");

    // An unknown order id 404s.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/orders/00000000-0000-0000-0000-000000000000",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown order: {body}");
}
