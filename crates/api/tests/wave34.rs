//! Wave 34 integration test: multi-store access + staff roles.
//! The owner seats a manager and a staff member by phone; the
//! domain permission matrix then governs what each may do in
//! the store — the manager runs the shop (catalogue, orders,
//! finances read) but cannot seat members or move money out;
//! staff work catalogue and orders but are refused marketing
//! and finances; a stranger seller is invisible to the store
//! (404/403 by each surface's historical convention); removing
//! a member ends their access at once. The owner regression
//! anchor is the Wave 9 money flow: one delivered unit of a
//! 200_000-poisha product earns exactly 184_000.
//!
//! Gated on `DATABASE_URL`: without it the test prints a skip
//! message and passes, so a plain local `cargo test` (no
//! database) still passes. CI provides a Postgres 16 service
//! and sets `DATABASE_URL` for the rust job.

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

/// The caller's own user id, via /v1/auth/me.
async fn my_id(app: &Router, token: &str) -> String {
    let (status, body) = call(app, Method::GET, "/v1/auth/me", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "me: {body}");
    body["id"].as_str().expect("user id").to_string()
}

/// Buy one unit and return the new sub-order's id (left `placed`).
async fn buy_one(app: &Router, buyer: &str, product_id: &str) -> String {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string()
}

/// POST a shipment booking for a sub-order; returns (status, body).
async fn book_shipment(
    app: &Router,
    token: &str,
    sub_order_id: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    call(app, Method::POST, &uri, Some(json!({})), Some(token)).await
}

#[tokio::test]
async fn wave34_multi_store_roles_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 34 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_variants, products, store_members, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode (deliveries) on.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave34-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // The cast: a store owner, their future manager and staff,
    // a stranger seller with a store of their own, and a buyer.
    // Every member target is registered BEFORE being seated —
    // membership never creates an account.
    let owner = login(&app, "01711111111").await;
    let manager = login(&app, "01822222222").await;
    let staff = login(&app, "01933333333").await;
    let stranger = login(&app, "01644444444").await;
    let buyer = login(&app, "01555555555").await;
    let manager_id = my_id(&app, &manager).await;
    let staff_id = my_id(&app, &staff).await;

    let store = create_store(&app, &owner, "wave34-store").await;
    create_store(&app, &stranger, "wave34-stranger-store").await;

    // ---------------------------------------------------------------
    // 1. Owner regression anchor (the Wave 9 money flow, untouched):
    // one delivered unit of a 200_000-poisha product earns 184_000.
    // ---------------------------------------------------------------
    let anchor = create_product(&app, &owner, &store, "Wave34 Anchor", 200_000, 10).await;
    buy_and_deliver(&app, &buyer, &owner, &anchor).await;
    let balance = get_balance(&app, &owner).await;
    assert_eq!(balance["earned_poisha"], 184_000, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 184_000);

    // ---------------------------------------------------------------
    // 2. Member management: seat, duplicates, ghosts, bad roles.
    // ---------------------------------------------------------------
    let members_uri = format!("/v1/seller/stores/{store}/members");

    // A number with no account is a 404 — no silent creation.
    let (status, body) = call(
        &app,
        Method::POST,
        &members_uri,
        Some(json!({ "phone": "01366666666", "role": "staff" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "ghost member: {body}");

    // `owner` is not a member role.
    let (status, body) = call(
        &app,
        Method::POST,
        &members_uri,
        Some(json!({ "phone": "01822222222", "role": "owner" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "owner role: {body}");

    // The owner cannot seat themself.
    let (status, body) = call(
        &app,
        Method::POST,
        &members_uri,
        Some(json!({ "phone": "01711111111", "role": "staff" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "self member: {body}");

    // Seat the manager (phone given in local form; it is
    // normalized before lookup).
    let (status, body) = call(
        &app,
        Method::POST,
        &members_uri,
        Some(json!({ "phone": "01822222222", "role": "manager" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add manager: {body}");
    assert_eq!(body["user_id"], manager_id);
    assert_eq!(body["role"], "manager");
    assert!(
        body["phone_masked"]
            .as_str()
            .expect("masked phone")
            .contains("****"),
        "phone is masked in member views: {body}"
    );

    // Seating the same user twice is a 409.
    let (status, body) = call(
        &app,
        Method::POST,
        &members_uri,
        Some(json!({ "phone": "+8801822222222", "role": "staff" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "duplicate member: {body}");

    // A manager may NOT seat members (manage_members is the
    // owner's alone) — the matrix's 403.
    let (status, body) = call(
        &app,
        Method::POST,
        &members_uri,
        Some(json!({ "phone": "01933333333", "role": "staff" })),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "manager adds member: {body}");

    // The owner seats the staff member.
    let (status, body) = call(
        &app,
        Method::POST,
        &members_uri,
        Some(json!({ "phone": "01933333333", "role": "staff" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add staff: {body}");
    assert_eq!(body["user_id"], staff_id);
    assert_eq!(body["role"], "staff");

    // The team list: owner and manager may view it, staff not.
    let (status, body) = call(&app, Method::GET, &members_uri, None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK, "owner lists members: {body}");
    assert_eq!(body.as_array().map(Vec::len), Some(2), "members: {body}");
    let (status, _) = call(&app, Method::GET, &members_uri, None, Some(&manager)).await;
    assert_eq!(status, StatusCode::OK, "manager lists members");
    let (status, body) = call(&app, Method::GET, &members_uri, None, Some(&staff)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "staff lists members: {body}");
    // A stranger cannot even learn the store exists here.
    let (status, body) = call(&app, Method::GET, &members_uri, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger lists members: {body}");

    // The seller stores list carries each caller's own role.
    let (status, body) = call(&app, Method::GET, "/v1/seller/stores", None, Some(&manager)).await;
    assert_eq!(status, StatusCode::OK, "manager stores: {body}");
    let stores = body.as_array().expect("stores list");
    assert_eq!(stores.len(), 1, "manager stores: {body}");
    assert_eq!(stores[0]["id"], store);
    assert_eq!(stores[0]["my_role"], "manager");
    let (status, body) = call(&app, Method::GET, "/v1/seller/stores", None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK, "owner stores: {body}");
    assert_eq!(body[0]["my_role"], "owner");

    // ---------------------------------------------------------------
    // 3. The manager runs the shop: catalogue, orders, finance
    // reads — but never payouts or members.
    // ---------------------------------------------------------------
    let manager_product =
        create_product(&app, &manager, &store, "Manager Product", 100_000, 5).await;

    let sub2 = buy_one(&app, &buyer, &manager_product).await;
    let (status, body) = set_sub_order_status(&app, &manager, &sub2, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "manager confirms: {body}");
    let (status, body) = book_shipment(&app, &manager, &sub2).await;
    assert_eq!(status, StatusCode::CREATED, "manager books: {body}");

    // Finances are readable (view_finances)…
    let scoped_balance_uri = format!("/v1/seller/balance?store_id={store}");
    let (status, body) = call(&app, Method::GET, &scoped_balance_uri, None, Some(&manager)).await;
    assert_eq!(status, StatusCode::OK, "manager balance: {body}");
    assert_eq!(body["earned_poisha"], 184_000, "manager balance: {body}");

    // …but moving money out is the owner's alone (manage_payouts).
    let scoped_payouts_uri = format!("/v1/seller/payouts?store_id={store}");
    let (status, body) = call(
        &app,
        Method::POST,
        &scoped_payouts_uri,
        Some(json!({ "amount_poisha": 50_000 })),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "manager payout request: {body}");
    let scoped_account_uri = format!("/v1/seller/payout-account?store_id={store}");
    let (status, body) = call(
        &app,
        Method::PUT,
        &scoped_account_uri,
        Some(json!({
            "method": "bkash",
            "account_name": "Manager",
            "account_number": "01822222222"
        })),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "manager payout account: {body}");

    // The store-scoped dashboard is open to any serving role,
    // and its earnings describe the store.
    let scoped_dashboard_uri = format!("/v1/seller/dashboard?store_id={store}");
    let (status, body) = call(&app, Method::GET, &scoped_dashboard_uri, None, Some(&manager)).await;
    assert_eq!(status, StatusCode::OK, "manager dashboard: {body}");
    assert_eq!(body["store"]["id"], store);
    assert_eq!(body["earnings"]["earned_poisha"], 184_000, "dashboard: {body}");
    // Without a scope the manager has no store of their own: 404,
    // exactly like any storeless user before this wave.
    let (status, body) = call(&app, Method::GET, "/v1/seller/dashboard", None, Some(&manager)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "manager default dashboard: {body}");

    // ---------------------------------------------------------------
    // 4. Staff work the floor: catalogue + orders yes,
    // marketing + finances no.
    // ---------------------------------------------------------------
    let staff_product = create_product(&app, &staff, &store, "Staff Product", 80_000, 5).await;
    let sub3 = buy_one(&app, &buyer, &staff_product).await;
    let (status, body) = set_sub_order_status(&app, &staff, &sub3, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "staff confirms: {body}");
    let (status, body) = book_shipment(&app, &staff, &sub3).await;
    assert_eq!(status, StatusCode::CREATED, "staff books shipment: {body}");

    // Staff see the store's sub-orders (their work queue)…
    let (status, body) = call(&app, Method::GET, "/v1/seller/sub-orders", None, Some(&staff)).await;
    assert_eq!(status, StatusCode::OK, "staff sub-orders: {body}");
    assert!(
        body["total"].as_i64().expect("total") >= 3,
        "staff sees the store's sub-orders: {body}"
    );

    // …but coupons are marketing: the matrix's 403.
    let scoped_coupons_uri = format!("/v1/seller/coupons?store_id={store}");
    let (status, body) = call(
        &app,
        Method::POST,
        &scoped_coupons_uri,
        Some(json!({
            "code": "STAFF10",
            "discount_type": "percent",
            "discount_value": 10
        })),
        Some(&staff),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "staff coupon create: {body}");

    // …and the balance is finances: also 403.
    let (status, body) = call(&app, Method::GET, &scoped_balance_uri, None, Some(&staff)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "staff balance: {body}");

    // The manager CAN create the same coupon (marketing is
    // in their matrix row).
    let (status, body) = call(
        &app,
        Method::POST,
        &scoped_coupons_uri,
        Some(json!({
            "code": "MGR10",
            "discount_type": "percent",
            "discount_value": 10
        })),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "manager coupon create: {body}");

    // ---------------------------------------------------------------
    // 5. A stranger seller is invisible to the store.
    // ---------------------------------------------------------------
    // Creating into the owner's store: this module's 403.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store,
            "title": "Stranger Product",
            "price_poisha": 10_000,
            "stock": 1,
            "status": "active"
        })),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "stranger creates product: {body}");

    // The seller variant surface answers 404 (its convention).
    let variants_uri = format!("/v1/seller/products/{anchor}/variants");
    let (status, body) = call(&app, Method::GET, &variants_uri, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger variant list: {body}");

    // The scoped dashboard answers 404 (no relationship).
    let (status, body) = call(&app, Method::GET, &scoped_dashboard_uri, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger dashboard: {body}");

    // ---------------------------------------------------------------
    // 6. Role changes and removal end access immediately.
    // ---------------------------------------------------------------
    let manager_member_uri = format!("{members_uri}/{manager_id}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &manager_member_uri,
        Some(json!({ "role": "staff" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "demote manager: {body}");
    assert_eq!(body["role"], "staff");
    // Demoted, the ex-manager loses the finance read at once.
    let (status, body) = call(&app, Method::GET, &scoped_balance_uri, None, Some(&manager)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "demoted manager balance: {body}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &manager_member_uri,
        Some(json!({ "role": "manager" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "promote back: {body}");
    assert_eq!(body["role"], "manager");

    // Remove the staff member: their next call is a 404 and
    // the store leaves their seller-stores list.
    let staff_member_uri = format!("{members_uri}/{staff_id}");
    let (status, _) = call(&app, Method::DELETE, &staff_member_uri, None, Some(&owner)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "remove staff");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store,
            "title": "Ex Staff Product",
            "price_poisha": 10_000,
            "stock": 1,
            "status": "active"
        })),
        Some(&staff),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "ex-staff creates product: {body}");
    let (status, body) = call(&app, Method::GET, "/v1/seller/stores", None, Some(&staff)).await;
    assert_eq!(status, StatusCode::OK, "ex-staff stores: {body}");
    assert_eq!(body.as_array().map(Vec::len), Some(0), "ex-staff stores: {body}");
    // Removing a non-member is a 404, not a silent success.
    let (status, body) = call(&app, Method::DELETE, &staff_member_uri, None, Some(&owner)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "remove non-member: {body}");
}
