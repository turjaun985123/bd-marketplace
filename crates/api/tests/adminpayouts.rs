//! Admin payouts remake, part A: the mark-paid coverage
//! gate, the payment-reference requirement, and the
//! approved -> rejected escape hatch.
//!
//! The Wave 39 coverage re-check originally ran only at
//! approve; earnings can fall AFTER approval (a delivered
//! order is returned), and `paid` is final — so the remake
//! re-checks at mark-paid too, with the refusal carrying
//! the earnings picture as structured `details`. Mark-paid
//! also REQUIRES the payment reference (the recorded proof
//! of the send), and an approved payout the admin cannot
//! pay can be rejected (freeing the seller's hold) instead
//! of being trapped in `approved` forever.
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

#[tokio::test]
async fn admin_payouts_mark_paid_coverage_reference_and_escape_hatch() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin payouts remake test A");
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
        "adminpayouts-test-secret".to_string(),
        true,
    )
    .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "adminpayouts-store").await;
    // A delivered unit of 200_000 earns exactly 184_000.
    let product = create_product(&app, &seller, &store, "Remake A Product", 200_000, 10).await;
    let buyer = login(&app, "01911111111").await;
    let admin = make_admin(&app, &pool, "01710000001", "+8801710000001").await;

    buy_and_deliver(&app, &buyer, &seller, &product).await;
    save_account(&app, &seller, "01711111111").await;

    let (status, payout) = request_payout(&app, &seller, 100_000).await;
    assert_eq!(status, StatusCode::CREATED, "request payout: {payout}");
    let payout_id = payout["id"].as_str().expect("payout id").to_string();

    let approve_uri = format!("/v1/admin/payouts/{payout_id}/approve");
    let (status, body) = call(&app, Method::POST, &approve_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "approve: {body}");
    assert_eq!(body["status"], "approved");

    // Earnings fall AFTER approval (the delivered sub-order's
    // snapshot is corrected down, as a return would do):
    // earned 50_000 against a 100_000 approved payout.
    let sub_order_id: Uuid =
        sqlx::query_scalar("SELECT id FROM sub_orders WHERE store_id = $1 LIMIT 1")
            .bind(Uuid::parse_str(&store).expect("store id is a uuid"))
            .fetch_one(&pool)
            .await
            .expect("load sub-order");
    sqlx::query("UPDATE sub_orders SET seller_payout_poisha = 50_000 WHERE id = $1")
        .bind(sub_order_id)
        .execute(&pool)
        .await
        .expect("simulate earnings drop");

    // Mark-paid is now refused with the coverage picture in
    // the structured details — the pre-remake API paid this
    // payout in full.
    let paid_uri = format!("/v1/admin/payouts/{payout_id}/mark-paid");
    let (status, body) = call(
        &app,
        Method::POST,
        &paid_uri,
        Some(json!({ "payment_reference": "TXN-A-001" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "mark-paid uncovered: {body}");
    assert_eq!(body["details"]["kind"], "payout_coverage", "details: {body}");
    assert_eq!(body["details"]["shortfall_poisha"], 50_000, "details: {body}");
    assert_eq!(body["details"]["earned_poisha"], 50_000, "details: {body}");
    assert_eq!(body["details"]["pending_poisha"], 100_000, "details: {body}");
    assert_eq!(
        body["details"]["available_unclamped_poisha"], -50_000,
        "details: {body}"
    );

    // The payout is still approved — nothing was written.
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/payouts/{payout_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["status"], "approved", "detail: {detail}");
    assert!(detail["payment_reference"].is_null(), "detail: {detail}");

    // The escape hatch: the admin rejects the APPROVED
    // payout with a reason; the seller's hold is freed.
    let reject_uri = format!("/v1/admin/payouts/{payout_id}/reject");
    let (status, body) = call(
        &app,
        Method::POST,
        &reject_uri,
        Some(json!({ "note": "earnings no longer cover this payout" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reject approved: {body}");
    assert_eq!(body["status"], "rejected");
    assert!(body["rejected_at"].is_string(), "rejected_at: {body}");

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["pending_poisha"], 0, "hold freed: {balance}");
    assert_eq!(balance["earned_poisha"], 50_000, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 50_000, "balance: {balance}");

    // The reference requirement, on a fresh covered payout:
    // no body and a too-short reference are both 400s, and
    // the payout stays approved through both.
    sqlx::query("UPDATE sub_orders SET seller_payout_poisha = 184_000 WHERE id = $1")
        .bind(sub_order_id)
        .execute(&pool)
        .await
        .expect("restore earnings");
    let (status, payout) = request_payout(&app, &seller, 50_000).await;
    assert_eq!(status, StatusCode::CREATED, "request payout 2: {payout}");
    let payout2 = payout["id"].as_str().expect("payout id").to_string();
    let approve2 = format!("/v1/admin/payouts/{payout2}/approve");
    let (status, body) = call(&app, Method::POST, &approve2, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "approve 2: {body}");

    let paid2 = format!("/v1/admin/payouts/{payout2}/mark-paid");
    let (status, body) = call(&app, Method::POST, &paid2, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "mark-paid no reference: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &paid2,
        Some(json!({ "payment_reference": "ab" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "mark-paid short reference: {body}");

    let (status, body) = call(
        &app,
        Method::POST,
        &paid2,
        Some(json!({ "payment_reference": "TXN-A-002" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mark-paid with reference: {body}");
    assert_eq!(body["status"], "paid");
    assert_eq!(body["payment_reference"], "TXN-A-002");
    assert!(body["paid_at"].is_string(), "paid_at: {body}");
    assert!(body["approved_at"].is_string(), "approved_at kept: {body}");

    // The seller's own history carries the recorded reference.
    let (status, mine) = call(&app, Method::GET, "/v1/seller/payouts", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "seller payouts: {mine}");
    let paid_row = mine["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|p| p["id"] == payout2)
        .expect("paid payout in seller history")
        .clone();
    assert_eq!(paid_row["payment_reference"], "TXN-A-002", "row: {paid_row}");
    assert!(paid_row["paid_at"].is_string(), "row: {paid_row}");
}
