//! Wave 36 integration test: seller subscription plans — a
//! plan is prepaid from the earnings balance through the
//! `seller_charges` ledger, a live pro plan discounts the
//! store's checkout commission from 800 to 700 bps (the
//! delivered-earning anchor moves 184_000 -> 186_000 on
//! 200_000 of goods), the free plan's product cap is enforced
//! at the tunable setting, only the owner may subscribe (a
//! manager gets the matrix's 403), cancelling stops renewal
//! but not the live period, and the worker-facing sweep
//! (`process_subscriptions`) renews a lapsed plan from the
//! balance — extending from the OLD expiry — or expires it
//! back to free when the balance cannot cover the price.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL` for
//! the rust job; a local scratch Postgres 16 works the same.
//!
//! platform_settings is global state shared with the other
//! test binaries: this test deletes every row it writes
//! before finishing.

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
/// delivered through the DEV courier; returns the order.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) -> Value {
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
    order
}

/// GET the seller balance for a token.
async fn get_balance(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/seller/balance", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "balance: {body}");
    body
}

/// GET the caller's subscription view (payout store).
async fn get_subscription(app: &Router, token: &str) -> Value {
    let (status, body) = call(
        app,
        Method::GET,
        "/v1/seller/subscription",
        None,
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "subscription: {body}");
    body
}

#[tokio::test]
async fn wave36_subscription_plans_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 36 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE loyalty_points, notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_variants, products, store_subscriptions, store_members, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode (deliveries) on.
    // Two identical states: one for the router, one for calling the
    // worker-facing sweep service directly.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave36-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let sweep_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let sweep_state = AppState::new(Some(pool.clone()), "wave36-test-secret".to_string(), true)
        .with_couriers_config(sweep_couriers);
    let app = build_app(state);

    // The cast: the subscribing seller, a free-plan seller, a
    // manager of the first seller's store, and a buyer.
    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave36-store").await;
    let free_seller = login(&app, "01599999999").await;
    let free_store = create_store(&app, &free_seller, "wave36-free-store").await;
    let manager = login(&app, "01822222222").await;
    let buyer = login(&app, "01999999999").await;

    // ---------------------------------------------------------------
    // 0. The catalogue at defaults, and the free default view.
    // ---------------------------------------------------------------
    let (status, plans) = call(
        &app,
        Method::GET,
        "/v1/seller/subscription/plans",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "plans: {plans}");
    let items = plans["items"].as_array().expect("plan items");
    assert_eq!(items.len(), 3, "plans: {plans}");
    assert_eq!(items[0]["key"], "free");
    assert_eq!(items[0]["price_poisha"], 0);
    assert_eq!(items[0]["product_cap"], 50);
    assert_eq!(items[1]["key"], "pro");
    assert_eq!(items[1]["price_poisha"], 49_900);
    assert_eq!(items[1]["commission_discount_bps"], 100);
    assert_eq!(items[1]["product_cap"], 500);
    assert_eq!(items[2]["key"], "business");
    assert_eq!(items[2]["price_poisha"], 149_900);
    assert_eq!(items[2]["commission_discount_bps"], 200);
    assert_eq!(items[2]["product_cap"], i64::MAX);

    let view = get_subscription(&app, &seller).await;
    assert_eq!(view["plan"], "free", "default view: {view}");
    assert_eq!(view["status"], "free");
    assert_eq!(view["product_cap"], 50);
    assert_eq!(view["commission_discount_bps"], 0);

    // ---------------------------------------------------------------
    // (a) Earn 184_000, then subscribe to pro from the balance.
    // ---------------------------------------------------------------
    // 200_000 poisha of goods: 8% commission is 16_000, so a
    // delivered unit earns exactly 184_000 (the Wave 9 anchor).
    let product = create_product(&app, &seller, &store, "Wave36 Product", 200_000, 30).await;
    let order = buy_and_deliver(&app, &buyer, &seller, &product).await;
    assert_eq!(order["sub_orders"][0]["commission_poisha"], 16_000);
    assert_eq!(order["sub_orders"][0]["seller_payout_poisha"], 184_000);

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 184_000, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 184_000);

    let (status, result) = call(
        &app,
        Method::POST,
        "/v1/seller/subscription",
        Some(json!({ "plan": "pro" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "subscribe pro: {result}");
    assert_eq!(result["charged_poisha"], 49_900);
    assert_eq!(result["available_after_poisha"], 134_100);
    assert_eq!(result["subscription"]["plan"], "pro");
    assert_eq!(result["subscription"]["status"], "active");
    assert_eq!(result["subscription"]["auto_renew"], true);
    assert_eq!(result["subscription"]["commission_discount_bps"], 100);
    assert_eq!(result["subscription"]["product_cap"], 500);

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["charges_total_poisha"], 49_900, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 134_100);

    let view = get_subscription(&app, &seller).await;
    assert_eq!(view["plan"], "pro", "view after subscribe: {view}");
    assert_eq!(view["status"], "active");

    // ---------------------------------------------------------------
    // (b) The plan discount at checkout: 700 bps, earning 186_000.
    // ---------------------------------------------------------------
    let order = buy_and_deliver(&app, &buyer, &seller, &product).await;
    assert_eq!(order["sub_orders"][0]["commission_poisha"], 14_000);
    assert_eq!(order["sub_orders"][0]["seller_payout_poisha"], 186_000);
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 370_000, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 320_100);

    // ---------------------------------------------------------------
    // (c) The free plan's product cap, tuned by the setting.
    // ---------------------------------------------------------------
    sqlx::query(
        "INSERT INTO platform_settings (key, value) VALUES ('plan_free_product_cap', '2')",
    )
    .execute(&pool)
    .await
    .expect("set free cap override");
    create_product(&app, &free_seller, &free_store, "Free One", 10_000, 5).await;
    create_product(&app, &free_seller, &free_store, "Free Two", 10_000, 5).await;
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": free_store,
            "title": "Free Three",
            "price_poisha": 10_000,
            "stock": 5,
            "status": "active"
        })),
        Some(&free_seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "third free product: {body}");
    // The pro store (cap 500) is unaffected by the free cap.
    create_product(&app, &seller, &store, "Wave36 Extra", 10_000, 5).await;
    // Removing the override restores the default cap of 50.
    sqlx::query("DELETE FROM platform_settings WHERE key = 'plan_free_product_cap'")
        .execute(&pool)
        .await
        .expect("clear free cap override");
    create_product(&app, &free_seller, &free_store, "Free Three", 10_000, 5).await;

    // ---------------------------------------------------------------
    // (e) A manager may view the plan but never buy one.
    // ---------------------------------------------------------------
    let members_uri = format!("/v1/seller/stores/{store}/members");
    let (status, body) = call(
        &app,
        Method::POST,
        &members_uri,
        Some(json!({ "phone": "01822222222", "role": "manager" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add manager: {body}");

    let (status, view) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/subscription?store_id={store}"),
        None,
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "manager views plan: {view}");
    assert_eq!(view["plan"], "pro");

    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/subscription?store_id={store}"),
        Some(json!({ "plan": "business" })),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "manager subscribes: {body}");

    // ---------------------------------------------------------------
    // (f) Cancel stops renewal, not the live period; then
    // re-subscribe (full price again) to restore auto-renew.
    // ---------------------------------------------------------------
    let (status, view) = call(
        &app,
        Method::POST,
        "/v1/seller/subscription/cancel",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cancel renewal: {view}");
    assert_eq!(view["auto_renew"], false);
    assert_eq!(view["status"], "active");
    assert_eq!(view["plan"], "pro");

    let (status, result) = call(
        &app,
        Method::POST,
        "/v1/seller/subscription",
        Some(json!({ "plan": "pro" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "re-subscribe pro: {result}");
    assert_eq!(result["charged_poisha"], 49_900);
    assert_eq!(result["subscription"]["auto_renew"], true);
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["available_poisha"], 270_200, "balance: {balance}");

    // ---------------------------------------------------------------
    // (d) The sweep renews from the balance, extending from the
    // OLD expiry; with the price pushed past the balance it
    // expires the store back to free instead.
    // ---------------------------------------------------------------
    sqlx::query(
        "UPDATE store_subscriptions SET expires_at = now() - interval '1 hour' \
         WHERE store_id = $1::uuid",
    )
    .bind(store.as_str())
    .execute(&pool)
    .await
    .expect("backdate subscription expiry");

    let report = api::subscriptions::process_subscriptions(&sweep_state)
        .await
        .expect("sweep runs");
    assert_eq!(report.checked, 1, "report: {report:?}");
    assert_eq!(report.renewed, 1, "report: {report:?}");
    assert_eq!(report.expired, 0, "report: {report:?}");

    // The new expiry is ~30 days from the OLD one (an hour in
    // the past): comfortably more than 29 days out, less than 30.
    let extended: bool = sqlx::query_scalar(
        "SELECT expires_at > now() + interval '29 days' \
            AND expires_at < now() + interval '30 days' \
         FROM store_subscriptions WHERE store_id = $1::uuid",
    )
    .bind(store.as_str())
    .fetch_one(&pool)
    .await
    .expect("read extended expiry");
    assert!(extended, "renewal extends from the old expiry");

    let charges: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM seller_charges WHERE kind = 'subscription'",
    )
    .fetch_one(&pool)
    .await
    .expect("count subscription charges");
    assert_eq!(charges, 3, "subscribe + re-subscribe + renewal");
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["available_poisha"], 220_300, "balance: {balance}");

    // Nothing is lapsed now: a second sweep is a no-op.
    let report = api::subscriptions::process_subscriptions(&sweep_state)
        .await
        .expect("sweep runs");
    assert_eq!(report.checked, 0, "report: {report:?}");

    // Push the price past the balance and lapse the plan again:
    // the sweep must expire it, not renew it.
    sqlx::query(
        "INSERT INTO platform_settings (key, value) \
         VALUES ('subscription_price_pro_poisha', '9999999')",
    )
    .execute(&pool)
    .await
    .expect("set huge pro price");
    sqlx::query(
        "UPDATE store_subscriptions SET expires_at = now() - interval '1 hour' \
         WHERE store_id = $1::uuid",
    )
    .bind(store.as_str())
    .execute(&pool)
    .await
    .expect("backdate subscription expiry again");

    let report = api::subscriptions::process_subscriptions(&sweep_state)
        .await
        .expect("sweep runs");
    assert_eq!(report.checked, 1, "report: {report:?}");
    assert_eq!(report.renewed, 0, "report: {report:?}");
    assert_eq!(report.expired, 1, "report: {report:?}");

    let view = get_subscription(&app, &seller).await;
    assert_eq!(view["plan"], "free", "view after expiry: {view}");
    assert_eq!(view["status"], "expired");
    assert_eq!(view["commission_discount_bps"], 0);
    assert_eq!(view["product_cap"], 50);

    // And the commission is back to the full 800 bps: the next
    // delivered order earns the original 184_000 again.
    let order = buy_and_deliver(&app, &buyer, &seller, &product).await;
    assert_eq!(order["sub_orders"][0]["commission_poisha"], 16_000);
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 554_000, "balance: {balance}");

    // Cleanup: platform_settings is global across test binaries.
    sqlx::query("DELETE FROM platform_settings")
        .execute(&pool)
        .await
        .expect("clean platform_settings");
}
