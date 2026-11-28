//! Admin payouts remake, part H: transitions under
//! contention — the double-approve and the two-payout
//! coverage squeeze.
//!
//! The transition's row locks plus the status re-check
//! make a second approve of the same payout a 409 whose
//! message names the CURRENT status (the web uses it to
//! freeze the row and refetch). And when two payouts
//! together exceed what the store's earnings cover after
//! a drop, the coverage gate refuses at BOTH remaining
//! decision points — approve and mark-paid — while the
//! payouts themselves stay put for a later retry.
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
use uuid::Uuid;

/// Set one sub-order's payout snapshot directly (the
/// earnings-drop simulation the Wave 39 test established).
async fn set_snapshot(pool: &PgPool, sub_order_id: Uuid, poisha: i64) {
    sqlx::query("UPDATE sub_orders SET seller_payout_poisha = $2 WHERE id = $1")
        .bind(sub_order_id)
        .bind(poisha)
        .execute(pool)
        .await
        .expect("set sub-order snapshot");
}

/// The store's sub-order ids, oldest first.
async fn store_sub_orders(pool: &PgPool, store_id: &str) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT id FROM sub_orders WHERE store_id = $1 ORDER BY created_at ASC")
        .bind(Uuid::parse_str(store_id).expect("store id is a uuid"))
        .fetch_all(pool)
        .await
        .expect("load sub-orders")
}

#[tokio::test]
async fn admin_payouts_transition_contention() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin payouts remake test H");
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
        "adminpayoutsh-test-secret".to_string(),
        true,
    )
    .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let seller = login(&app, "01788888888").await;
    let store = create_store(&app, &seller, "adminpayoutsh-store").await;
    let product = create_product(&app, &seller, &store, "Remake H Product", 200_000, 10).await;
    let buyer = login(&app, "01988888888").await;
    let admin = make_admin(&app, &pool, "01780000008", "+8801780000008").await;

    // Two delivered sales: 368_000 earned.
    buy_and_deliver(&app, &buyer, &seller, &product).await;
    buy_and_deliver(&app, &buyer, &seller, &product).await;
    save_account(&app, &seller, "01788888888").await;

    let (status, payout) = request_payout(&app, &seller, 150_000).await;
    assert_eq!(status, StatusCode::CREATED, "request 1: {payout}");
    let payout1 = payout["id"].as_str().expect("payout id").to_string();
    let (status, payout) = request_payout(&app, &seller, 150_000).await;
    assert_eq!(status, StatusCode::CREATED, "request 2: {payout}");
    let payout2 = payout["id"].as_str().expect("payout id").to_string();

    let subs = store_sub_orders(&pool, &store).await;
    assert_eq!(subs.len(), 2, "two delivered sub-orders");

    // Earnings drop: one sale's snapshot zeroed. Earned is
    // now 184_000 against 300_000 of pending requests.
    set_snapshot(&pool, subs[0], 0).await;
    let approve1 = format!("/v1/admin/payouts/{payout1}/approve");
    let (status, body) = call(&app, Method::POST, &approve1, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::CONFLICT, "approve uncovered: {body}");
    assert_eq!(body["details"]["kind"], "payout_coverage", "details: {body}");
    assert_eq!(body["details"]["shortfall_poisha"], 116_000, "details: {body}");

    // Restored: both approvals go through.
    set_snapshot(&pool, subs[0], 184_000).await;
    let (status, body) = call(&app, Method::POST, &approve1, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "approve 1: {body}");

    // The double-approve: a 409 that names the current
    // status in both languages.
    let (status, body) = call(&app, Method::POST, &approve1, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::CONFLICT, "approve twice: {body}");
    let message = body["error"].as_str().expect("error text").to_string();
    assert!(message.contains("'approved'"), "message names the status: {message}");
    assert!(message.contains("অনুমোদিত"), "message is bilingual: {message}");
    assert!(
        body["details"].is_null(),
        "an illegal transition carries no coverage details: {body}"
    );

    let approve2 = format!("/v1/admin/payouts/{payout2}/approve");
    let (status, body) = call(&app, Method::POST, &approve2, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "approve 2: {body}");

    // Pay the first; then drop earnings again so the second
    // is uncovered at mark-paid: earned 194_000 against
    // 300_000 committed (the paid 150_000 plus the approved
    // 150_000 — the canonical aggregate counts approved in
    // paid_out, requested in pending).
    let paid1 = format!("/v1/admin/payouts/{payout1}/mark-paid");
    let (status, body) = call(
        &app,
        Method::POST,
        &paid1,
        Some(json!({ "payment_reference": "TXN-H-001" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mark-paid 1: {body}");

    set_snapshot(&pool, subs[1], 10_000).await;
    let paid2 = format!("/v1/admin/payouts/{payout2}/mark-paid");
    let (status, body) = call(
        &app,
        Method::POST,
        &paid2,
        Some(json!({ "payment_reference": "TXN-H-002" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "mark-paid 2 uncovered: {body}");
    assert_eq!(body["details"]["kind"], "payout_coverage", "details: {body}");
    assert_eq!(body["details"]["shortfall_poisha"], 106_000, "details: {body}");

    // The second payout is still approved — retryable when
    // the balance recovers.
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["paid_out_poisha"], 300_000, "balance: {balance}");
    assert_eq!(balance["pending_poisha"], 0, "balance: {balance}");
    set_snapshot(&pool, subs[1], 184_000).await;
    let (status, body) = call(
        &app,
        Method::POST,
        &paid2,
        Some(json!({ "payment_reference": "TXN-H-002" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mark-paid 2 after recovery: {body}");
    assert_eq!(body["status"], "paid");
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
/// GET the seller balance for a token.
async fn get_balance(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/seller/balance", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "balance: {body}");
    body
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
