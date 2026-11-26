//! Wave 22 integration test: per-store sequential invoices —
//! a sub-order that has not been delivered has no invoice
//! (409), the first entitled read of a delivered sub-order
//! issues its store's `INV-000001`, re-reading returns the
//! SAME invoice (same id, same number — no sequence burned),
//! the same store's next delivered sub-order draws
//! `INV-000002` while a DIFFERENT store starts at its own
//! `INV-000001`, strangers and other stores' sellers get 404s,
//! the seller and admin endpoints serve the same document,
//! and a sub-order that is later returned keeps its invoice.
//! Delivery is driven by the seller status PATCH (placed ->
//! confirmed -> shipped -> delivered, the domain transition
//! table), so no courier is involved. The small helpers are
//! copied from the Wave 9 test on purpose, since integration
//! test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.
//! CI provides a Postgres 16 service and sets `DATABASE_URL`.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
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

/// Buy one unit of the product; returns the order's only
/// sub-order id.
async fn buy_one(app: &Router, buyer: &str, product_id: &str) -> String {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string()
}

/// Drive one sub-order placed -> confirmed -> shipped ->
/// delivered through the seller status endpoint.
async fn deliver(app: &Router, seller: &str, sub_order_id: &str) {
    for next in ["confirmed", "shipped", "delivered"] {
        let (status, body) = set_sub_order_status(app, seller, sub_order_id, next).await;
        assert_eq!(status, StatusCode::OK, "move to {next}: {body}");
    }
}

/// Read a sub-order's invoice through one role's endpoint.
async fn get_invoice(
    app: &Router,
    token: &str,
    role: &str,
    sub_order_id: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/{role}/sub-orders/{sub_order_id}/invoice");
    call(app, Method::GET, &uri, None, Some(token)).await
}

/// POST a return request for a sub-order as a buyer.
async fn request_return(
    app: &Router,
    token: &str,
    sub_order_id: &str,
    reason: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/buyer/sub-orders/{sub_order_id}/return-request");
    call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "reason": reason })),
        Some(token),
    )
    .await
}

#[tokio::test]
async fn wave22_per_store_sequential_invoices_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 22 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         platform_settings, coupon_redemptions, coupons, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode on (OTP login); deliveries go through the
    // seller status PATCH, so no courier configuration is needed.
    let state = AppState::new(Some(pool.clone()), "wave22-test-secret".to_string(), true);
    let app = build_app(state);

    // Cast: an admin (promoted by SQL, the only way), two
    // sellers with one store each, the buyer, and a stranger.
    let admin = login(&app, "01511111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801511111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller_a = login(&app, "01799999999").await;
    let store_a = create_store(&app, &seller_a, "wave22-store-a").await;
    // 200_000 poisha of goods + 6_000 inside-Dhaka delivery for
    // the default 1 kg parcel: the slice grand is 206_000.
    let product_a = create_product(&app, &seller_a, &store_a, "Wave22 Product A", 200_000, 10).await;
    let seller_b = login(&app, "01599999999").await;
    let store_b = create_store(&app, &seller_b, "wave22-store-b").await;
    let product_b = create_product(&app, &seller_b, &store_b, "Wave22 Product B", 150_000, 10).await;
    let buyer = login(&app, "01999999999").await;
    let stranger = login(&app, "01611111111").await;

    // 1. Store A's first sale, only CONFIRMED so far: no
    // completed sale, so no invoice — a 409 for buyer and
    // seller alike.
    let sub_a1 = buy_one(&app, &buyer, &product_a).await;
    let (status, body) = set_sub_order_status(&app, &seller_a, &sub_a1, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");

    let (status, body) = get_invoice(&app, &buyer, "buyer", &sub_a1).await;
    assert_eq!(status, StatusCode::CONFLICT, "invoice before delivery: {body}");
    let (status, body) = get_invoice(&app, &seller_a, "seller", &sub_a1).await;
    assert_eq!(status, StatusCode::CONFLICT, "seller invoice before delivery: {body}");

    // 2. Deliver it: the buyer's first read ISSUES the invoice —
    // store A's number one, with the slice's items and money.
    deliver(&app, &seller_a, &sub_a1).await;
    let (status, invoice) = get_invoice(&app, &buyer, "buyer", &sub_a1).await;
    assert_eq!(status, StatusCode::OK, "buyer invoice: {invoice}");
    assert_eq!(invoice["invoice_number"], "INV-000001");
    assert_eq!(invoice["seq"], 1);
    assert_eq!(invoice["sub_order_id"], sub_a1);
    assert_eq!(invoice["sub_order_status"], "delivered");
    assert_eq!(invoice["store"]["slug"], "wave22-store-a");
    assert_eq!(invoice["store"]["name"], "Store wave22-store-a");
    assert_eq!(invoice["store"]["verified_tier"], "unverified");
    assert_eq!(invoice["buyer"]["name"], "Test Buyer");
    assert_eq!(invoice["buyer"]["phone"], "+8801999999999");
    assert_eq!(invoice["items"][0]["title_snapshot"], "Wave22 Product A");
    assert_eq!(invoice["items"][0]["quantity"], 1);
    assert_eq!(invoice["items"][0]["line_total_poisha"], 200_000);
    assert_eq!(invoice["money"]["goods_subtotal_poisha"], 200_000);
    assert_eq!(invoice["money"]["discount_poisha"], 0);
    assert_eq!(invoice["money"]["delivery_charge_poisha"], 6_000);
    assert_eq!(invoice["money"]["grand_total_poisha"], 206_000);
    assert_eq!(invoice["money"]["advance_paid_poisha"], 0);
    assert_eq!(invoice["money"]["cod_amount_due_poisha"], 206_000);
    assert_eq!(invoice["payment_method"], "cod");
    assert_eq!(invoice["payment_status"], "pay_on_delivery");
    let invoice_a1_id = invoice["id"].as_str().expect("invoice id").to_string();

    // 3. Idempotent: a second read is the SAME invoice (same
    // row id, same number) — no new sequence is burned.
    let (status, again) = get_invoice(&app, &buyer, "buyer", &sub_a1).await;
    assert_eq!(status, StatusCode::OK, "re-read invoice: {again}");
    assert_eq!(again["id"], invoice_a1_id);
    assert_eq!(again["invoice_number"], "INV-000001");

    // The seller's endpoint serves the same document.
    let (status, seller_view) = get_invoice(&app, &seller_a, "seller", &sub_a1).await;
    assert_eq!(status, StatusCode::OK, "seller invoice: {seller_view}");
    assert_eq!(seller_view["id"], invoice_a1_id);
    assert_eq!(seller_view["invoice_number"], "INV-000001");

    // 4. No leaks: a stranger buyer and the OTHER store's
    // seller both get 404; a non-admin on the admin endpoint
    // gets 403; the admin reads the same invoice.
    let (status, body) = get_invoice(&app, &stranger, "buyer", &sub_a1).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger invoice: {body}");
    let (status, body) = get_invoice(&app, &seller_b, "seller", &sub_a1).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "other seller invoice: {body}");
    let (status, body) = get_invoice(&app, &seller_a, "admin", &sub_a1).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "admin invoice as seller: {body}");
    let (status, admin_view) = get_invoice(&app, &admin, "admin", &sub_a1).await;
    assert_eq!(status, StatusCode::OK, "admin invoice: {admin_view}");
    assert_eq!(admin_view["id"], invoice_a1_id);

    // An unknown sub-order id is the same 404 for everyone.
    let missing = "00000000-0000-0000-0000-000000000000";
    let (status, body) = get_invoice(&app, &buyer, "buyer", missing).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown sub-order: {body}");

    // 5. Store A's SECOND delivered sale draws INV-000002.
    let sub_a2 = buy_one(&app, &buyer, &product_a).await;
    deliver(&app, &seller_a, &sub_a2).await;
    let (status, invoice2) = get_invoice(&app, &buyer, "buyer", &sub_a2).await;
    assert_eq!(status, StatusCode::OK, "second invoice: {invoice2}");
    assert_eq!(invoice2["invoice_number"], "INV-000002");
    assert_eq!(invoice2["seq"], 2);

    // 6. Store B's first delivered sale starts at B's OWN
    // INV-000001 — sequences are per store.
    let sub_b1 = buy_one(&app, &buyer, &product_b).await;
    deliver(&app, &seller_b, &sub_b1).await;
    let (status, invoice_b) = get_invoice(&app, &seller_b, "seller", &sub_b1).await;
    assert_eq!(status, StatusCode::OK, "store B invoice: {invoice_b}");
    assert_eq!(invoice_b["invoice_number"], "INV-000001");
    assert_eq!(invoice_b["seq"], 1);
    assert_eq!(invoice_b["store"]["slug"], "wave22-store-b");
    assert_eq!(invoice_b["money"]["goods_subtotal_poisha"], 150_000);
    assert_eq!(invoice_b["money"]["grand_total_poisha"], 156_000);

    // 7. A RETURNED sub-order keeps its invoice: return store
    // A's second sale end to end (request -> approve ->
    // received moves it to `returned`), then the invoice still
    // reads, with the same number and the returned status.
    let (status, created) = request_return(&app, &buyer, &sub_a2, "পণ্যটি ছিঁড়ে গেছে").await;
    assert_eq!(status, StatusCode::CREATED, "create return: {created}");
    let return_id = created["id"].as_str().expect("return id").to_string();

    let uri = format!("/v1/seller/return-requests/{return_id}/approve");
    let (status, body) = call(&app, Method::POST, &uri, Some(json!({})), Some(&seller_a)).await;
    assert_eq!(status, StatusCode::OK, "approve return: {body}");

    let uri = format!("/v1/seller/return-requests/{return_id}/received");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&seller_a)).await;
    assert_eq!(status, StatusCode::OK, "receive return: {body}");

    let (status, returned_invoice) = get_invoice(&app, &buyer, "buyer", &sub_a2).await;
    assert_eq!(status, StatusCode::OK, "returned invoice: {returned_invoice}");
    assert_eq!(returned_invoice["invoice_number"], "INV-000002");
    assert_eq!(returned_invoice["sub_order_status"], "returned");
}
