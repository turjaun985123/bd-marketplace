//! Admin overview remake test, part 2: the metrics
//! snapshot's new operator blocks — advance queues (owed
//! vs never-collected are DIFFERENT populations), refunds
//! pending on received returns, the KYC review queue,
//! stock alerts by effective stock, the status groupings,
//! growth, and the notification age fields (null, not
//! fabricated, on an empty outbox). Queue states that are
//! expensive to stage through the API (a verification
//! application, a received return, the advance columns a
//! checkout writes) are arranged by direct SQL on top of
//! real API-created rows — the same fixture style the
//! repo uses for admin promotion and timestamp backdating.
//! Helpers are copied from the Wave 9 test on purpose.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

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

/// GET the metrics snapshot as the admin.
async fn get_metrics(app: &Router, admin: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/admin/metrics", None, Some(admin)).await;
    assert_eq!(status, StatusCode::OK, "metrics: {body}");
    body
}

#[tokio::test]
async fn adminoverview_metrics_operator_blocks() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin overview metrics test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE otp_request_log, payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, \
         return_requests, store_verifications, notifications, \
         gift_card_ledger, gift_cards, promotions, flash_sale_items, \
         flash_sales, order_items, sub_orders, orders, cart_items, \
         carts, products, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(
        Some(pool.clone()),
        "adminoverviewb-test-secret".to_string(),
        true,
    );
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    let seller = login(&app, "01799999999").await;
    let buyer = login(&app, "01999999999").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin by SQL");

    // 1. Empty-platform baseline: every new block is zeroed,
    // ages are null (never a fabricated number), the sparse
    // groupings are empty objects, and nothing is live.
    let metrics = get_metrics(&app, &admin).await;
    assert_eq!(metrics["advance_pending"]["count"], 0);
    assert_eq!(metrics["advance_pending"]["outstanding_poisha"], 0);
    assert_eq!(metrics["advance_pending"]["blocked_total_poisha"], 0);
    assert_eq!(metrics["advance_pending"]["over_24h"], 0);
    assert_eq!(metrics["advance_pending"]["over_72h"], 0);
    assert!(metrics["advance_pending"]["oldest_age_secs"].is_null());
    assert_eq!(metrics["advance_unfulfilled"]["count"], 0);
    assert_eq!(metrics["advance_unfulfilled"]["required_poisha"], 0);
    assert_eq!(metrics["refunds_pending"]["count"], 0);
    assert_eq!(metrics["refunds_pending"]["amount_poisha"], 0);
    assert_eq!(metrics["verifications_pending"]["count"], 0);
    assert!(metrics["verifications_pending"]["oldest_age_secs"].is_null());
    assert_eq!(metrics["sub_orders_by_status"], json!({}));
    assert_eq!(metrics["orders_by_payment_status"], json!({}));
    assert_eq!(metrics["stock_alerts"]["out_of_stock"], 0);
    assert_eq!(metrics["stock_alerts"]["low_stock"], 0);
    assert_eq!(metrics["orders_yesterday"]["count"], 0);
    assert_eq!(metrics["orders_yesterday"]["gmv_poisha"], 0);
    assert_eq!(metrics["growth_today"]["new_users"], 3);
    assert_eq!(metrics["growth_today"]["new_stores"], 0);
    assert_eq!(metrics["stores_suspended"], 0);
    assert_eq!(metrics["active_promotions"], 0);
    assert!(metrics["live_flash_sale"].is_null());
    let backlog = &metrics["notification_backlog"];
    assert!(backlog["oldest_pending_age_secs"].is_null());
    assert!(backlog["oldest_sending_claim_age_secs"].is_null());
    assert_eq!(backlog["stale_sending_count"], 0);
    assert_eq!(backlog["sending_unstamped_count"], 0);
    assert_eq!(backlog["sent_last_24h"], 0);
    assert!(backlog["last_sent_age_secs"].is_null());

    // 2. Catalogue fixtures: one out-of-stock and one
    // low-stock active product (default threshold 5), one
    // healthy; plus a pending KYC application for the store.
    let store = create_store(&app, &seller, "aovb-store").await;
    create_product(&app, &seller, &store, "AOVB Out", 100_000, 0).await;
    create_product(&app, &seller, &store, "AOVB Low", 100_000, 3).await;
    let stocked = create_product(&app, &seller, &store, "AOVB Stocked", 100_000, 50).await;
    sqlx::query(
        "INSERT INTO store_verifications (store_id, status, nid_number, nid_last4) \
         VALUES ($1::uuid, 'pending', '1234567890123', '0123')",
    )
    .bind(&store)
    .execute(&pool)
    .await
    .expect("insert pending verification");

    // 3. Two real COD checkouts of the stocked product.
    // Order 1 is flipped to the advance-owed state a real
    // advance checkout writes; order 2 keeps pay_on_delivery
    // but carries an uncollected advance requirement (the
    // degraded population).
    add_to_cart(&app, &buyer, &stocked, 1).await;
    let order1 = checkout_cod(&app, &buyer).await;
    assert_eq!(order1["grand_total_poisha"], 106_000, "order1: {order1}");
    add_to_cart(&app, &buyer, &stocked, 1).await;
    let order2 = checkout_cod(&app, &buyer).await;
    let order1_id = order1["id"].as_str().expect("order1 id");
    let order2_id = order2["id"].as_str().expect("order2 id");
    sqlx::query(
        "UPDATE orders SET payment_status = 'advance_pending', \
         advance_required_poisha = 50000, advance_paid_poisha = 10000 \
         WHERE id = $1::uuid",
    )
    .bind(order1_id)
    .execute(&pool)
    .await
    .expect("flip order1 to advance_pending");
    sqlx::query(
        "UPDATE orders SET advance_required_poisha = 30000 WHERE id = $1::uuid",
    )
    .bind(order2_id)
    .execute(&pool)
    .await
    .expect("mark order2 advance unfulfilled");

    // 4. A received return whose refund still waits for the
    // admin mark-done, on order 2's sub-order.
    let sub2_id = order2["sub_orders"][0]["id"]
        .as_str()
        .expect("order2 sub-order id")
        .to_string();
    let buyer_id: String = sqlx::query_scalar(
        "SELECT id::text FROM users WHERE phone = '+8801999999999'",
    )
    .fetch_one(&pool)
    .await
    .expect("buyer id by phone");
    sqlx::query(
        "INSERT INTO return_requests \
         (sub_order_id, order_id, buyer_id, store_id, reason, status, \
          refund_poisha, refund_status) \
         VALUES ($1::uuid, $2::uuid, $3::uuid, $4::uuid, 'damaged', \
                 'received', 180000, 'pending')",
    )
    .bind(&sub2_id)
    .bind(order2_id)
    .bind(&buyer_id)
    .bind(&store)
    .execute(&pool)
    .await
    .expect("insert received return");

    // 5. The populated snapshot: every block reads its own
    // population and only its own.
    let metrics = get_metrics(&app, &admin).await;
    assert_eq!(metrics["advance_pending"]["count"], 1);
    assert_eq!(metrics["advance_pending"]["outstanding_poisha"], 40_000);
    assert_eq!(metrics["advance_pending"]["blocked_total_poisha"], 106_000);
    assert_eq!(metrics["advance_pending"]["over_24h"], 0);
    assert_eq!(metrics["advance_pending"]["over_72h"], 0);
    let age = metrics["advance_pending"]["oldest_age_secs"]
        .as_i64()
        .expect("advance oldest age present");
    assert!(age >= 0, "advance oldest age sane: {age}");
    assert_eq!(metrics["advance_unfulfilled"]["count"], 1);
    assert_eq!(metrics["advance_unfulfilled"]["required_poisha"], 30_000);
    assert_eq!(metrics["refunds_pending"]["count"], 1);
    assert_eq!(metrics["refunds_pending"]["amount_poisha"], 180_000);
    assert_eq!(metrics["verifications_pending"]["count"], 1);
    let vage = metrics["verifications_pending"]["oldest_age_secs"]
        .as_i64()
        .expect("verification oldest age present");
    assert!(vage >= 0, "verification oldest age sane: {vage}");
    assert_eq!(metrics["sub_orders_by_status"], json!({ "placed": 2 }));
    assert_eq!(
        metrics["orders_by_payment_status"],
        json!({ "advance_pending": 1, "pay_on_delivery": 1 })
    );
    assert_eq!(metrics["stock_alerts"]["out_of_stock"], 1);
    assert_eq!(metrics["stock_alerts"]["low_stock"], 1);
    assert_eq!(metrics["growth_today"]["new_users"], 3);
    assert_eq!(metrics["growth_today"]["new_stores"], 1);
    // open_returns still counts only requested/approved —
    // the received return above is NOT double-counted there.
    assert_eq!(metrics["open_returns"], 0);
}
