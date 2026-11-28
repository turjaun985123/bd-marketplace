//! Admin payouts remake, part G: the queue itself —
//! search, sort, status validation, and the whole-queue
//! summary.
//!
//! Before the remake the list was newest-first with a
//! hard-coded page of 50 and the total thrown away, an
//! unknown status silently returned an empty queue, and
//! there was no search at all. Now: status and sort are
//! validated (400, never a silent empty), the search
//! matches store / phone / account substrings (LIKE-escaped)
//! and idempotency keys exactly or by prefix, the sort is a
//! total order, and every response carries the whole-queue
//! summary — whose `requested` bucket must agree with the
//! overview's `pending_payouts` (the same canonical count
//! the remade overview links here with).
//!
//! Helpers are copied verbatim from the Wave 9 test, since
//! integration test files stay self-contained. Gated on
//! `DATABASE_URL`, like every integration test here.

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

/// One store + one delivered sale + one payout for a
/// fresh seller; returns the payout id.
async fn seller_with_payout(
    app: &Router,
    seller_phone: &str,
    slug: &str,
    buyer_phone: &str,
    amount: i64,
) -> String {
    let seller = login(app, seller_phone).await;
    let store = create_store(app, &seller, slug).await;
    let product = create_product(app, &seller, &store, "Remake G Product", 200_000, 10).await;
    let buyer = login(app, buyer_phone).await;
    buy_and_deliver(app, &buyer, &seller, &product).await;
    save_account(app, &seller, seller_phone).await;
    let (status, payout) = request_payout(app, &seller, amount).await;
    assert_eq!(status, StatusCode::CREATED, "request payout {slug}: {payout}");
    payout["id"].as_str().expect("payout id").to_string()
}

#[tokio::test]
async fn admin_payouts_list_search_sort_summary() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin payouts remake test G");
            return;
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
         payments, order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(
        Some(pool.clone()),
        "adminpayoutsg-test-secret".to_string(),
        true,
    )
    .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = make_admin(&app, &pool, "01770000007", "+8801770000007").await;

    // Three payouts across two stores: A files 80_000 then
    // 60_000; B files 120_000.
    let payout_a1 = seller_with_payout(
        &app, "01777777771", "adminpayoutsg-store-a", "01977777771", 80_000,
    )
    .await;
    let seller_a = login(&app, "01777777771").await;
    let (status, payout) = request_payout(&app, &seller_a, 60_000).await;
    assert_eq!(status, StatusCode::CREATED, "A's second request: {payout}");
    let payout_a2 = payout["id"].as_str().expect("payout id").to_string();
    let payout_b1 = seller_with_payout(
        &app, "01777777772", "adminpayoutsg-store-b", "01977777772", 120_000,
    )
    .await;

    // The summary covers the whole queue and agrees with
    // the overview's pending count.
    let (status, list) = call(&app, Method::GET, "/v1/admin/payouts", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "list: {list}");
    assert_eq!(list["total"], 3, "list: {list}");
    assert_eq!(list["summary"]["requested"]["count"], 3, "list: {list}");
    assert_eq!(list["summary"]["requested"]["amount_poisha"], 260_000, "list: {list}");
    assert_eq!(list["summary"]["paid"]["count"], 0, "list: {list}");
    assert!(
        list["summary"]["requested"]["oldest_age_secs"].is_number(),
        "oldest age present: {list}"
    );
    let (status, overview) = call(&app, Method::GET, "/v1/admin/overview", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "overview: {overview}");
    assert_eq!(
        overview["payouts_pending"]["count"], list["summary"]["requested"]["count"],
        "summary and overview agree: {overview} vs {list}"
    );
    assert_eq!(
        overview["payouts_pending"]["amount_poisha"],
        list["summary"]["requested"]["amount_poisha"],
        "summary and overview agree: {overview} vs {list}"
    );

    // Sorts are total orders over the same set.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?sort=amount_desc",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "amount_desc: {list}");
    assert_eq!(list["items"][0]["id"], payout_b1, "largest first: {list}");
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?sort=amount_asc",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "amount_asc list: {list}");
    assert_eq!(list["items"][0]["id"], payout_a2, "smallest first: {list}");
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?sort=oldest",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "oldest list: {list}");
    assert_eq!(list["items"][0]["id"], payout_a1, "oldest first: {list}");
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?sort=newest",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "newest list: {list}");
    assert_eq!(list["items"][0]["id"], payout_b1, "newest first: {list}");

    // Validation: unknown status and sort are 400s, never a
    // silently empty queue.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?status=pending",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "status=pending: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?sort=random",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "sort=random: {body}");

    // Search: by store slug fragment, by local phone, and
    // by account-number substring — each narrows the total
    // while the summary stays global.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?q=store-b",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "q store: {list}");
    assert_eq!(list["total"], 1, "q store: {list}");
    assert_eq!(list["items"][0]["id"], payout_b1, "q store: {list}");
    assert_eq!(list["summary"]["requested"]["count"], 3, "summary stays global: {list}");

    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?q=01777777771",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "q phone list: {list}");
    assert_eq!(list["total"], 2, "q local phone finds store A: {list}");

    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?q=7777772",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "q account list: {list}");
    assert_eq!(list["total"], 1, "q account substring finds store B: {list}");
    assert_eq!(list["items"][0]["id"], payout_b1, "q account: {list}");

    // A LIKE wildcard in the query is data, not a pattern:
    // "%" must match nothing rather than everything.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?q=%25",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "q wildcard: {list}");
    assert_eq!(list["total"], 0, "wildcard escaped: {list}");
}

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
/// Checkout the buyer's current cart with COD; returns the order.
async fn checkout_cod(app: &Router, token: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {body}");
    body
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
/// Buy one unit of the product and drive the order all the way to
/// delivered through the DEV courier; returns the order id.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    let (status, body) = set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");

    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(json!({})), Some(seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();

    // Four polls walk the DEV courier created -> delivered.
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(app, Method::POST, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
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
/// Save the seller's bKash payout account.
async fn save_account(app: &Router, seller: &str, number: &str) {
    let (status, body) = call(
        app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "Remake Seller",
            "account_number": number
        })),
        Some(seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save payout account: {body}");
}
/// Request a payout as the seller.
async fn request_payout(app: &Router, seller: &str, amount: i64) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": amount })),
        Some(seller),
    )
    .await
}
