//! Wave 10 integration test: the platform admin — the `is_admin`
//! guard (non-admins get 403 on every admin endpoint), promotion
//! by direct SQL (there is deliberately no promotion endpoint),
//! overview metrics, the payout processing lifecycle (approve ->
//! mark-paid, reject restores the seller's available balance),
//! store suspension (products vanish from the public listing,
//! storefront and detail; checkout refuses with a 409; the owner
//! still manages the catalogue; reactivation restores everything),
//! platform settings (a stored commission rate changes the next
//! checkout's commission; invalid values are a 400), and user
//! admin flags (promote/demote others, never yourself).
//! Helpers are copied from the Wave 9 test on purpose — integration
//! test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

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

/// Full dev-mode OTP login for a phone number; returns (JWT, user).
async fn login(app: &Router, phone: &str) -> (String, Value) {
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
    let token = body["token"].as_str().expect("token issued").to_string();
    (token, body["user"].clone())
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
async fn checkout_cod(app: &Router, token: &str) -> (StatusCode, Value) {
    call(
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
    .await
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
/// delivered through the DEV courier.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) {
    add_to_cart(app, buyer, product_id, 1).await;
    let (status, order) = checkout_cod(app, buyer).await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {order}");
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

#[tokio::test]
async fn wave10_admin_panel_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 10 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE platform_settings, payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, \
         order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode (deliveries) on.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave10-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // The future admin, a seller, a buyer, and a plain user.
    let (admin, admin_user) = login(&app, "01711111111").await;
    let admin_id = admin_user["id"].as_str().expect("admin id").to_string();
    assert_eq!(admin_user["is_admin"], false, "login starts non-admin");
    let (seller, _seller_user) = login(&app, "01722222222").await;
    let (buyer, _buyer_user) = login(&app, "01933333333").await;
    let (plain, plain_user) = login(&app, "01644444444").await;
    let plain_id = plain_user["id"].as_str().expect("plain id").to_string();

    // 1. Before promotion, NOBODY is an admin: every admin endpoint
    // is a 403 for a signed-in non-admin.
    for uri in [
        "/v1/admin/overview",
        "/v1/admin/stores",
        "/v1/admin/orders",
        "/v1/admin/payouts",
        "/v1/admin/users",
        "/v1/admin/settings",
    ] {
        let (status, body) = call(&app, Method::GET, uri, None, Some(&plain)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "non-admin GET {uri}: {body}");
    }

    // 2. Promotion happens by SQL — there is no endpoint for it.
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin by SQL");

    // /v1/auth/me re-reads the row: the flag shows immediately.
    let (status, me) = call(&app, Method::GET, "/v1/auth/me", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin me: {me}");
    assert_eq!(me["is_admin"], true);
    let (status, me) = call(&app, Method::GET, "/v1/auth/me", None, Some(&plain)).await;
    assert_eq!(status, StatusCode::OK, "plain me: {me}");
    assert_eq!(me["is_admin"], false);

    // 3. Seed the platform: one store, one product, one placed order.
    let store = create_store(&app, &seller, "wave10-store").await;
    let product = create_product(&app, &seller, &store, "Wave10 Product", 200_000, 50).await;
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = checkout_cod(&app, &buyer).await;
    assert_eq!(status, StatusCode::CREATED, "seed checkout: {order}");
    // The startup rate still applies: 8% of 200_000 goods = 16_000.
    assert_eq!(order["sub_orders"][0]["commission_poisha"], 16_000);

    // 4. Overview metrics after seeding.
    let (status, overview) = call(&app, Method::GET, "/v1/admin/overview", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "overview: {overview}");
    assert_eq!(overview["users"], 4);
    assert_eq!(overview["stores"], 1);
    assert_eq!(overview["products"], 1);
    assert_eq!(overview["orders"], 1);
    // Overview remake: GMV is the canonical Wave-27 figure —
    // goods 200_000 − discount 0 + inside-Dhaka delivery 6_000 —
    // and commission is earned only on DELIVERED sub-orders, so
    // the placed order's 16_000 snapshot is not yet earned.
    assert_eq!(overview["gmv_poisha"], 206_000);
    assert_eq!(overview["commission_earned_poisha"], 0);
    assert_eq!(overview["payouts_pending"]["count"], 0);
    assert_eq!(overview["payouts_pending"]["amount_poisha"], 0);
    assert_eq!(overview["payments_pending_count"], 0);

    // Admin order list: the seeded order is there; a cancelled
    // filter matches nothing.
    let (status, list) = call(&app, Method::GET, "/v1/admin/orders", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin orders: {list}");
    assert_eq!(list["total"], 1);
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/orders?status=cancelled",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin orders filtered: {list}");
    assert_eq!(list["total"], 0);

    // Admin store list: owner phone, product count, active status.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/stores?q=wave10",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin stores: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], store);
    assert_eq!(list["items"][0]["owner_phone"], "+8801722222222");
    assert_eq!(list["items"][0]["product_count"], 1);
    assert_eq!(list["items"][0]["status"], "active");

    // 5. Payout lifecycle. A delivered sale earns the seller
    // 200_000 - 16_000 = 184_000; the seller saves an account and
    // requests 100_000.
    buy_and_deliver(&app, &buyer, &seller, &product).await;
    let (status, account) = call(
        &app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "Wave10 Seller",
            "account_number": "01722222222"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save payout account: {account}");

    let (status, payout) = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": 100_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "request payout: {payout}");
    let payout1 = payout["id"].as_str().expect("payout id").to_string();

    // The admin queue shows it, with store + owner context, and the
    // overview counts it as pending.
    let (status, queue) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?status=requested",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin payout queue: {queue}");
    assert_eq!(queue["total"], 1);
    assert_eq!(queue["items"][0]["id"], payout1);
    assert_eq!(queue["items"][0]["store_name"], "Store wave10-store");
    // Admin payouts remake: the queue carries MASKED
    // identifiers only (the full values live in the detail
    // endpoint) — the raw list body must not leak either the
    // owner's full phone or the full account number.
    assert_eq!(queue["items"][0]["owner_phone_masked"], "017****222");
    let queue_raw = queue.to_string();
    assert!(
        !queue_raw.contains("+8801722222222"),
        "admin payout list leaked the full owner phone: {queue_raw}"
    );
    assert!(
        !queue_raw.contains("01722222222"),
        "admin payout list leaked the full account number: {queue_raw}"
    );

    let (status, overview) = call(&app, Method::GET, "/v1/admin/overview", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "overview: {overview}");
    assert_eq!(overview["payouts_pending"]["count"], 1);
    assert_eq!(overview["payouts_pending"]["amount_poisha"], 100_000);

    // Approve, then approve AGAIN (illegal: approved -> approved),
    // then mark paid.
    let uri = format!("/v1/admin/payouts/{payout1}/approve");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "approve: {body}");
    assert_eq!(body["status"], "approved");
    assert!(body["processed_at"].is_string(), "processed_at set: {body}");
    // Admin payouts remake: the per-stage stamp is recorded too.
    assert!(body["approved_at"].is_string(), "approved_at set: {body}");
    assert!(body["paid_at"].is_null(), "paid_at not yet set: {body}");

    let (status, body) = call(&app, Method::POST, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::CONFLICT, "approve twice: {body}");

    // Admin payouts remake: mark-paid REQUIRES the payment
    // reference — the recorded proof of the send.
    let uri = format!("/v1/admin/payouts/{payout1}/mark-paid");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "payment_reference": "WAVE10-TXN-001" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mark-paid: {body}");
    assert_eq!(body["status"], "paid");
    assert_eq!(body["payment_reference"], "WAVE10-TXN-001");
    assert!(body["paid_at"].is_string(), "paid_at set: {body}");

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 184_000, "balance: {balance}");
    assert_eq!(balance["paid_out_poisha"], 100_000);
    assert_eq!(balance["pending_poisha"], 0);
    assert_eq!(balance["available_poisha"], 84_000);

    // A second request is REJECTED with a note; the seller's
    // available balance is restored (pending drops back to zero).
    let (status, payout) = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": 60_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "request payout 2: {payout}");
    let payout2 = payout["id"].as_str().expect("payout id").to_string();
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["pending_poisha"], 60_000);
    assert_eq!(balance["available_poisha"], 24_000);

    let uri = format!("/v1/admin/payouts/{payout2}/reject");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "note": "account details mismatch" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reject: {body}");
    assert_eq!(body["status"], "rejected");
    assert_eq!(body["note"], "account details mismatch");

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["pending_poisha"], 0, "balance: {balance}");
    assert_eq!(balance["paid_out_poisha"], 100_000);
    assert_eq!(balance["available_poisha"], 84_000);

    // 6. Store suspension. A second buyer already holds the product
    // in their cart when the store is suspended.
    let (buyer2, _buyer2_user) = login(&app, "01855555555").await;
    add_to_cart(&app, &buyer2, &product, 1).await;

    let uri = format!("/v1/admin/stores/{store}/status");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "suspended" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suspend store: {body}");
    assert_eq!(body["status"], "suspended");

    // Public listing, storefront listing and public detail all hide
    // the product (detail: 404 even for the signed-in buyer).
    let (status, list) = call(&app, Method::GET, "/v1/products", None, None).await;
    assert_eq!(status, StatusCode::OK, "marketplace list: {list}");
    assert_eq!(list["total"], 0);

    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/stores/wave10-store/products",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "storefront list: {list}");
    assert_eq!(list["total"], 0);

    let uri = format!("/v1/products/{product}");
    let (status, body) = call(&app, Method::GET, &uri, None, Some(&buyer2)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "suspended detail: {body}");

    // The OWNER still sees and manages the product.
    let (status, body) = call(&app, Method::GET, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "owner detail: {body}");
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/stores/wave10-store/products",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "owner storefront list: {list}");
    assert_eq!(list["total"], 1);

    // Checkout refuses the suspended store's product with a 409;
    // the cart survives the refusal.
    let (status, body) = checkout_cod(&app, &buyer2).await;
    assert_eq!(status, StatusCode::CONFLICT, "suspended checkout: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("suspended"),
        "suspension message: {body}"
    );

    // Reactivation restores everything, including that checkout.
    let uri = format!("/v1/admin/stores/{store}/status");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "active" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reactivate store: {body}");
    assert_eq!(body["status"], "active");

    let uri = format!("/v1/products/{product}");
    let (status, _body) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "detail restored");
    let (status, order) = checkout_cod(&app, &buyer2).await;
    assert_eq!(status, StatusCode::CREATED, "checkout restored: {order}");

    // An invalid store status is a 400.
    let uri = format!("/v1/admin/stores/{store}/status");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "banned" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad store status: {body}");

    // 7. Platform settings. Defaults first: the startup 800 bps and
    // the built-in 50_000 minimum.
    let (status, settings) = call(&app, Method::GET, "/v1/admin/settings", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "settings: {settings}");
    assert_eq!(settings["commission_bps"], 800);
    assert_eq!(settings["min_payout_poisha"], 50_000);
    assert_eq!(settings["platform_name"], "BD Marketplace");

    // Out-of-range values are a 400 and change nothing.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/admin/settings",
        Some(json!({ "commission_bps": 5_000 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "invalid bps: {body}");
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/admin/settings",
        Some(json!({ "min_payout_poisha": -5 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "invalid min payout: {body}");

    // A valid commission override changes the NEXT checkout: 10%
    // of 200_000 goods = 20_000 commission.
    let (status, settings) = call(
        &app,
        Method::PUT,
        "/v1/admin/settings",
        Some(json!({ "commission_bps": 1_000 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put settings: {settings}");
    assert_eq!(settings["commission_bps"], 1_000);

    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = checkout_cod(&app, &buyer).await;
    assert_eq!(status, StatusCode::CREATED, "checkout at 10%: {order}");
    assert_eq!(order["sub_orders"][0]["commission_poisha"], 20_000);

    // The minimum payout override is stored and reported back.
    let (status, settings) = call(
        &app,
        Method::PUT,
        "/v1/admin/settings",
        Some(json!({ "min_payout_poisha": 200_000 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put min payout: {settings}");
    assert_eq!(settings["min_payout_poisha"], 200_000);

    // 8. User admin flags: promote the plain user (their very next
    // admin call succeeds), demote them again, and prove the admin
    // cannot change their OWN flag.
    let uri = format!("/v1/admin/users/{plain_id}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "is_admin": true })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "promote plain: {body}");
    assert_eq!(body["is_admin"], true);

    let (status, _overview) = call(&app, Method::GET, "/v1/admin/overview", None, Some(&plain)).await;
    assert_eq!(status, StatusCode::OK, "promoted user reaches admin");

    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "is_admin": false })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "demote plain: {body}");
    assert_eq!(body["is_admin"], false);

    let uri = format!("/v1/admin/users/{admin_id}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "is_admin": false })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "self-demote: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("your own admin flag"),
        "self-demote message: {body}"
    );

    // A non-admin cannot touch flags at all.
    let uri = format!("/v1/admin/users/{plain_id}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "is_admin": true })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller patch users: {body}");

    // The admin user list finds people by phone substring and
    // reports store counts.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/users?q=16444",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin users search: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], plain_id);
    assert_eq!(list["items"][0]["is_admin"], false);
    assert_eq!(list["items"][0]["store_count"], 0);

    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/users?q=17222",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin users search seller: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["store_count"], 1);
    assert_eq!(list["items"][0]["role"], "seller");

    // Cleanup: platform_settings is global state shared with the other
    // wave test binaries (they run sequentially against one database).
    // Leaving commission_bps=1000 behind would change every later
    // checkout's commission, so remove the rows this test wrote.
    sqlx::query("DELETE FROM platform_settings")
        .execute(&pool)
        .await
        .expect("clean platform_settings");
}
