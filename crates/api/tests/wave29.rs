//! Wave 29 integration test: the notification outbox.
//! Every state-changing funnel queues exactly one SMS row
//! inside its own transaction — order placed (once per
//! order, with the grand total in the rendered body), the
//! seller PATCH walk (shipped, then delivered, no duplicate
//! delivered from any second funnel), a seller cancel, the
//! payout paid notice to the seller, and return updates on
//! approve / received (while a platform return does NOT
//! produce the RTO-only `sub_order_returned` kind). The
//! admin notifications view reports the counts (everything
//! still pending — no worker runs in this test) and refuses
//! non-admins.
//!
//! The small helpers are copied from the Wave 9 / Wave 16
//! tests on purpose, since integration test files stay
//! self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.
//! CI provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job.

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

/// How many outbox rows exist for one kind.
async fn count_kind(pool: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE kind = $1")
        .bind(kind)
        .fetch_one(pool)
        .await
        .expect("count notifications by kind")
}

/// The newest body + phone + status for one kind.
async fn latest_notice(pool: &PgPool, kind: &str) -> (String, String, String) {
    sqlx::query_as(
        "SELECT body, phone, status FROM notifications \
         WHERE kind = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(kind)
    .fetch_one(pool)
    .await
    .expect("load latest notification")
}

#[tokio::test]
async fn wave29_notification_outbox_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 29 integration test");
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
         payments, order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode on (OTP login + the log sender). Deliveries
    // are driven by the seller PATCH walk, so no courier dev
    // mode is needed.
    let state = AppState::new(Some(pool.clone()), "wave29-test-secret".to_string(), true);
    let app = build_app(state);

    // Cast: an admin (promoted by SQL, the only way), one
    // seller, one buyer.
    let admin = login(&app, "01511111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801511111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave29-store").await;
    // 200_000 poisha of goods + 6_000 inside-Dhaka delivery:
    // the order grand total is 206_000 and a delivered unit
    // earns 184_000 (8% commission).
    let product = create_product(&app, &seller, &store, "Wave29 Product", 200_000, 10).await;
    let buyer = login(&app, "01999999999").await;

    // 1. Placing a COD order queues exactly ONE notification:
    // order_placed, pending, to the delivery phone, with the
    // grand total (৳2060) and the short order id in the body.
    add_to_cart(&app, &buyer, &product, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    assert_eq!(count_kind(&pool, "order_placed").await, 1);
    let (body, phone, status) = latest_notice(&pool, "order_placed").await;
    assert_eq!(phone, "+8801999999999", "notice: {body}");
    assert_eq!(status, "pending");
    assert!(body.contains("৳2060"), "body: {body}");
    assert!(
        body.contains(&format!("#{}", &order_id[..8])),
        "body: {body}"
    );

    // 2. The seller PATCH walk: confirming queues nothing;
    // shipped queues sub_order_shipped (naming the store);
    // delivered queues sub_order_delivered — exactly one,
    // from this funnel only.
    let (status, body) = set_sub_order_status(&app, &seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    assert_eq!(count_kind(&pool, "sub_order_shipped").await, 0);

    let (status, body) = set_sub_order_status(&app, &seller, &sub_order_id, "shipped").await;
    assert_eq!(status, StatusCode::OK, "ship: {body}");
    assert_eq!(count_kind(&pool, "sub_order_shipped").await, 1);
    let (body, _, _) = latest_notice(&pool, "sub_order_shipped").await;
    assert!(body.contains("Store wave29-store"), "body: {body}");

    let (status, body) = set_sub_order_status(&app, &seller, &sub_order_id, "delivered").await;
    assert_eq!(status, StatusCode::OK, "deliver: {body}");
    assert_eq!(count_kind(&pool, "sub_order_delivered").await, 1);

    // 3. A second order the seller cancels after confirming:
    // one more order_placed and one sub_order_cancelled.
    add_to_cart(&app, &buyer, &product, 1).await;
    let order2 = checkout_cod(&app, &buyer).await;
    let sub2_id = order2["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(&app, &seller, &sub2_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm 2: {body}");
    let (status, body) = set_sub_order_status(&app, &seller, &sub2_id, "cancelled").await;
    assert_eq!(status, StatusCode::OK, "cancel 2: {body}");
    assert_eq!(count_kind(&pool, "order_placed").await, 2);
    assert_eq!(count_kind(&pool, "sub_order_cancelled").await, 1);

    // 4. The payout flow: the delivered order earned 184_000.
    // Account first, then a 100_000 request, admin approve +
    // mark-paid — the paid move queues payout_paid to the
    // SELLER at their account phone, with the amount (৳1000).
    let (status, account) = call(
        &app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "Wave29 Seller",
            "account_number": "01799999999"
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
    let payout_id = payout["id"].as_str().expect("payout id").to_string();

    let uri = format!("/v1/admin/payouts/{payout_id}/approve");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "approve payout: {body}");
    assert_eq!(count_kind(&pool, "payout_paid").await, 0);

    let uri = format!("/v1/admin/payouts/{payout_id}/mark-paid");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "mark payout paid: {body}");
    assert_eq!(count_kind(&pool, "payout_paid").await, 1);
    let (body, phone, _) = latest_notice(&pool, "payout_paid").await;
    assert_eq!(phone, "+8801799999999", "notice: {body}");
    assert!(body.contains("৳1000"), "body: {body}");

    // 5. The return flow on the delivered order: approve and
    // received each queue a return_update for the buyer; the
    // received body carries the refund (goods 200_000 ->
    // ৳2000). A platform return is NOT an RTO: no
    // sub_order_returned row may exist.
    let (status, created) = request_return(&app, &buyer, &sub_order_id, "পণ্যটি ছিঁড়ে গেছে").await;
    assert_eq!(status, StatusCode::CREATED, "create return: {created}");
    let return_id = created["id"].as_str().expect("return id").to_string();

    let uri = format!("/v1/seller/return-requests/{return_id}/approve");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "note": "ফেরত পাঠিয়ে দিন" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve return: {body}");
    assert_eq!(count_kind(&pool, "return_update").await, 1);
    let (body, phone, _) = latest_notice(&pool, "return_update").await;
    assert_eq!(phone, "+8801999999999", "notice: {body}");
    assert!(body.contains("অনুমোদিত"), "body: {body}");

    let uri = format!("/v1/seller/return-requests/{return_id}/received");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "receive return: {body}");
    assert_eq!(count_kind(&pool, "return_update").await, 2);
    let (body, _, _) = latest_notice(&pool, "return_update").await;
    assert!(body.contains("রিফান্ড ৳2000"), "body: {body}");
    assert_eq!(count_kind(&pool, "sub_order_returned").await, 0);

    // 6. The admin view: 8 rows total, all still pending (no
    // worker runs here), none failed; the status filter
    // works; phones are masked; a non-admin gets a 403.
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications")
        .fetch_one(&pool)
        .await
        .expect("count all notifications");
    assert_eq!(total, 8, "total notifications");

    let (status, view) = call(
        &app,
        Method::GET,
        "/v1/admin/notifications",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin notifications: {view}");
    assert_eq!(view["counts"]["pending"], 8, "view: {view}");
    assert_eq!(view["counts"]["sending"], 0);
    assert_eq!(view["counts"]["sent"], 0);
    assert_eq!(view["counts"]["failed"], 0);
    assert_eq!(view["items"].as_array().map(Vec::len), Some(8));
    let first_phone = view["items"][0]["phone_masked"]
        .as_str()
        .expect("masked phone");
    assert!(first_phone.contains('*'), "phone is masked: {first_phone}");
    assert!(
        !view.to_string().contains("+8801999999999"),
        "no full buyer phone leaks: {view}"
    );

    let (status, view) = call(
        &app,
        Method::GET,
        "/v1/admin/notifications?status=failed",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "filtered view: {view}");
    assert_eq!(view["items"].as_array().map(Vec::len), Some(0));
    assert_eq!(view["counts"]["pending"], 8, "counts stay global: {view}");

    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/notifications",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller is not admin: {body}");
}
