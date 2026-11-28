//! Admin payouts remake, part F: the payout detail — the
//! decision-ready view behind one queue row.
//!
//! Pins: the detail is admin-only (403) and 404s cleanly;
//! it is the ONLY admin payout response carrying the full
//! destination and owner phone (the list stays masked, and
//! the raw list body must not leak either value); the
//! earnings block is the canonical aggregate (with a
//! seller charge subtracted); the returns exposure shows
//! the in-flight return's at-risk amount; the verification
//! summary reads `none` for a store that never applied;
//! the account-divergence flag flips when the seller
//! changes their account after requesting — while the
//! payout's snapshot destination never moves; the store
//! history and duplicate candidates name the twin payout;
//! and the audit trail appears after a decision.
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

/// Ask for a return on a sub-order as its buyer (Wave 29 helper).
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
async fn admin_payouts_detail_view() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin payouts remake test F");
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
        "adminpayoutsf-test-secret".to_string(),
        true,
    )
    .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let seller = login(&app, "01766666666").await;
    let store = create_store(&app, &seller, "adminpayoutsf-store").await;
    let product = create_product(&app, &seller, &store, "Remake F Product", 200_000, 10).await;
    let buyer = login(&app, "01966666666").await;
    let admin = make_admin(&app, &pool, "01760000006", "+8801760000006").await;

    buy_and_deliver(&app, &buyer, &seller, &product).await;
    save_account(&app, &seller, "01766667777").await;

    // A seller charge (a promotion with no surviving
    // reference row): the earnings aggregate subtracts it.
    let seller_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE phone = '+8801766666666'")
            .fetch_one(&pool)
            .await
            .expect("load seller id");
    sqlx::query(
        "INSERT INTO seller_charges (seller_user_id, amount_poisha, kind, ref_id) \
         VALUES ($1, 5000, 'promotion', NULL)",
    )
    .bind(seller_id)
    .execute(&pool)
    .await
    .expect("insert seller charge");

    // Two identical requests: each is the other's duplicate
    // candidate.
    let (status, payout) = request_payout(&app, &seller, 60_000).await;
    assert_eq!(status, StatusCode::CREATED, "request 1: {payout}");
    let payout1 = payout["id"].as_str().expect("payout id").to_string();
    let (status, payout) = request_payout(&app, &seller, 60_000).await;
    assert_eq!(status, StatusCode::CREATED, "request 2: {payout}");
    let payout2 = payout["id"].as_str().expect("payout id").to_string();

    // An in-flight return against the delivered sub-order:
    // its 184_000 is still inside earnings today and is the
    // exposure the detail must show.
    let sub_order_id: Uuid =
        sqlx::query_scalar("SELECT id FROM sub_orders WHERE store_id = $1 LIMIT 1")
            .bind(Uuid::parse_str(&store).expect("store id is a uuid"))
            .fetch_one(&pool)
            .await
            .expect("load sub-order");
    let (status, created) = request_return(
        &app,
        &buyer,
        &sub_order_id.to_string(),
        "পণ্যে ত্রুটি আছে",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "request return: {created}");

    let detail_uri = format!("/v1/admin/payouts/{payout1}");

    // Admin-only, and unknown ids 404.
    let (status, body) = call(&app, Method::GET, &detail_uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "detail as seller: {body}");
    let missing = Uuid::new_v4();
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/payouts/{missing}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "detail of nothing: {body}");

    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    // Full values live here — and only here.
    assert_eq!(detail["owner_phone"], "+8801766666666", "detail: {detail}");
    assert_eq!(detail["account_number"], "01766667777", "detail: {detail}");
    assert_eq!(detail["owner_phone_masked"], "017****666", "detail: {detail}");
    assert_eq!(detail["account_number_masked"], "••••7777", "detail: {detail}");
    assert_eq!(detail["store"]["slug"], "adminpayoutsf-store", "detail: {detail}");
    assert_eq!(detail["store"]["status"], "active", "detail: {detail}");
    // The canonical earnings: 184_000 earned, 120_000 in the
    // two pending requests, 5_000 of charges.
    assert_eq!(detail["earnings"]["earned_poisha"], 184_000, "detail: {detail}");
    assert_eq!(detail["earnings"]["pending_poisha"], 120_000, "detail: {detail}");
    assert_eq!(detail["earnings"]["charges_total_poisha"], 5_000, "detail: {detail}");
    assert_eq!(detail["unclamped_available_poisha"], 59_000, "detail: {detail}");
    assert_eq!(detail["covered"], true, "detail: {detail}");
    // The returns exposure.
    assert_eq!(detail["returns_exposure"]["in_flight_count"], 1, "detail: {detail}");
    assert_eq!(
        detail["returns_exposure"]["at_risk_payout_poisha"], 184_000,
        "detail: {detail}"
    );
    assert_eq!(
        detail["returns_exposure"]["received_refund_pending_count"], 0,
        "detail: {detail}"
    );
    // Charges: one line, labelled without a reference, and
    // the total copied from the earnings above.
    assert_eq!(detail["charges_count"], 1, "detail: {detail}");
    assert_eq!(detail["charges_total_poisha"], 5_000, "detail: {detail}");
    assert_eq!(detail["charges_recent"][0]["amount_poisha"], 5_000, "detail: {detail}");
    // Verification: never applied.
    assert_eq!(detail["verification"]["status"], "none", "detail: {detail}");
    assert_eq!(detail["verification"]["tier"], "unverified", "detail: {detail}");
    // The twin payout: flagged, listed, and in the history.
    assert_eq!(detail["possible_duplicate"], true, "detail: {detail}");
    assert!(
        detail["duplicate_candidate_ids"]
            .as_array()
            .expect("candidate ids")
            .iter()
            .any(|id| id == &json!(payout2)),
        "candidates name the twin: {detail}"
    );
    assert!(
        detail["store_payout_history"]
            .as_array()
            .expect("history")
            .iter()
            .any(|p| p["id"] == json!(payout2)),
        "history names the twin: {detail}"
    );
    // No decisions yet: no audit entries, no divergence.
    assert_eq!(
        detail["audit_entries"].as_array().map(Vec::len),
        Some(0),
        "detail: {detail}"
    );
    assert_eq!(detail["account_diverged"], false, "detail: {detail}");

    // The queue stays masked: neither full value appears
    // anywhere in the raw list body.
    let (status, queue) = call(
        &app,
        Method::GET,
        "/v1/admin/payouts?status=requested",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "queue: {queue}");
    let queue_raw = queue.to_string();
    assert!(
        !queue_raw.contains("01766667777"),
        "list leaked the full account number: {queue_raw}"
    );
    assert!(
        !queue_raw.contains("+8801766666666"),
        "list leaked the full owner phone: {queue_raw}"
    );
    assert_eq!(queue["items"][0]["possible_duplicate"], true, "queue: {queue}");

    // The seller changes their account: the detail flags
    // the divergence, but the payout's snapshot is unmoved.
    save_account(&app, &seller, "01766668888").await;
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "detail after change: {detail}");
    assert_eq!(detail["account_diverged"], true, "detail: {detail}");
    assert_eq!(detail["account_number"], "01766667777", "snapshot: {detail}");
    assert_eq!(
        detail["current_account"]["account_number"], "01766668888",
        "detail: {detail}"
    );

    // A decision lands in the audit trail.
    let uri = format!("/v1/admin/payouts/{payout1}/approve");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "approve: {body}");
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "detail after approve: {detail}");
    assert_eq!(
        detail["audit_entries"].as_array().map(Vec::len),
        Some(1),
        "detail: {detail}"
    );
    assert_eq!(detail["audit_entries"][0]["action"], "payout.approve", "detail: {detail}");
    assert!(detail["approved_at"].is_string(), "detail: {detail}");
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
