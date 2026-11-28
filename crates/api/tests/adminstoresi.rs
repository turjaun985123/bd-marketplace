//! Admin stores remake, test I: the DETAIL endpoint's
//! guards and aggregates. Non-admins get 403, no token
//! 401, an unknown id 404. On a delivered-order fixture the
//! aggregates follow the settled definitions exactly:
//! delivered GMV = goods − discount + delivery over
//! delivered sub-orders (206_000 on this fixture), earnings
//! identical to the seller balance endpoint (the detail
//! composes `seller_earnings_on`, never re-derives), the
//! requested payout shows as pending exposure, the open
//! return shows in the returns exposure, and the owner
//! rollup carries each sibling's STATUS. Helpers are
//! copied from the Wave 9 test on purpose — integration
//! test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`.

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

#[tokio::test]
async fn adminstores_detail_aggregates_and_guards() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin stores remake test I");
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
    let state = AppState::new(Some(pool.clone()), "adminstoresi-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // One owner, two stores: the subject and its sibling.
    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "istores-main").await;
    let sibling = create_store(&app, &seller, "istores-side").await;
    // 200_000 of goods: 8% commission is 16_000, so a
    // delivered unit earns 184_000; inside-Dhaka delivery
    // is 6_000, so delivered GMV is 206_000.
    let product = create_product(&app, &seller, &store, "I Product", 200_000, 10).await;
    let buyer = login(&app, "01933333333").await;
    buy_and_deliver(&app, &buyer, &seller, &product).await;

    // A requested payout of 100_000 (account first).
    let (status, account) = call(
        &app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "I Seller",
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

    // One OPEN return against the delivered sub-order, by
    // direct insert (the returns flow is another surface's
    // fixture; the pre-receipt refund_status default is
    // 'none', so nothing counts as refund-pending).
    let (sub_order_id, order_id, buyer_id): (String, String, String) = sqlx::query_as(
        "SELECT so.id::text, so.order_id::text, o.buyer_user_id::text \
         FROM sub_orders so JOIN orders o ON o.id = so.order_id \
         WHERE so.store_id = $1::uuid",
    )
    .bind(&store)
    .fetch_one(&pool)
    .await
    .expect("delivered sub-order ids");
    sqlx::query(
        "INSERT INTO return_requests \
           (sub_order_id, order_id, buyer_id, store_id, reason, status, refund_poisha) \
         VALUES ($1::uuid, $2::uuid, $3::uuid, $4::uuid, 'Damaged in transit', \
                 'requested', 50000)",
    )
    .bind(&sub_order_id)
    .bind(&order_id)
    .bind(&buyer_id)
    .bind(&store)
    .execute(&pool)
    .await
    .expect("insert open return");

    // The sibling is suspended by direct SQL (fixture
    // economy — the transition itself is tests D–F's job).
    sqlx::query("UPDATE stores SET status = 'suspended' WHERE id = $1::uuid")
        .bind(&sibling)
        .execute(&pool)
        .await
        .expect("suspend sibling");

    // Guards: buyer 403, another seller 403, no token 401,
    // unknown id 404.
    let uri = format!("/v1/admin/stores/{store}");
    let (status, body) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "buyer: {body}");
    let other_seller = login(&app, "01744444444").await;
    create_store(&app, &other_seller, "istores-stranger").await;
    let (status, body) = call(&app, Method::GET, &uri, None, Some(&other_seller)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "other seller: {body}");
    let (status, body) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no token: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/stores/00000000-0000-0000-0000-000000000000",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown store: {body}");

    // The detail itself.
    let (status, detail) = call(&app, Method::GET, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["id"], store);
    assert_eq!(detail["slug"], "istores-main");
    assert_eq!(detail["status"], "active");
    assert_eq!(detail["owner_phone_masked"], "017****222");
    assert!(detail.get("owner_phone").is_none());
    assert_eq!(detail["product_count"], 1);
    assert_eq!(detail["follower_count"], 0);
    assert_eq!(detail["order_count"], 1);
    assert_eq!(detail["delivered_gmv_poisha"], 206_000);
    assert_eq!(detail["pending_payout_count"], 1);
    assert_eq!(detail["pending_payout_poisha"], 100_000);
    assert_eq!(detail["open_returns_count"], 1);

    // Earnings: the canonical numbers, and identical to
    // the seller's own balance endpoint.
    let balance = get_balance(&app, &seller).await;
    assert_eq!(detail["earnings"]["earned_poisha"], 184_000);
    assert_eq!(detail["earnings"]["earned_poisha"], balance["earned_poisha"]);
    assert_eq!(detail["earnings"]["pending_poisha"], 100_000);
    assert_eq!(detail["earnings"]["pending_poisha"], balance["pending_poisha"]);
    assert_eq!(detail["earnings"]["paid_out_poisha"], 0);
    assert_eq!(detail["earnings"]["available_poisha"], 84_000);
    assert_eq!(detail["earnings"]["available_poisha"], balance["available_poisha"]);

    // Returns exposure: the open return's seller-payout
    // value is at risk; nothing is refund-pending yet.
    assert_eq!(detail["returns_exposure"]["in_flight_count"], 1);
    assert_eq!(detail["returns_exposure"]["at_risk_payout_poisha"], 184_000);
    assert_eq!(detail["returns_exposure"]["received_refund_pending_count"], 0);
    assert_eq!(detail["returns_exposure"]["received_refund_pending_poisha"], 0);

    // The owner rollup includes BOTH stores, each with its
    // own status — the suspended sibling is visible as such.
    let owner_stores = detail["owner_stores"].as_array().expect("owner stores");
    assert_eq!(owner_stores.len(), 2, "owner stores: {detail}");
    let sibling_row = owner_stores
        .iter()
        .find(|row| row["store_id"] == sibling.as_str())
        .expect("sibling in rollup");
    assert_eq!(sibling_row["status"], "suspended");
    assert_eq!(sibling_row["store_slug"], "istores-side");
    let subject_row = owner_stores
        .iter()
        .find(|row| row["store_id"] == store.as_str())
        .expect("subject in rollup");
    assert_eq!(subject_row["status"], "active");
}
