//! Admin coupons remake, file D: the admin coupon detail —
//! usage computed ONLY from the redemption/order snapshots
//! (gross, the cancelled share labelled, the net derived),
//! recent redemptions with MASKED buyer phones (the raw body
//! never carries a full phone, in either form), the audit
//! history the remade mutations write, the no-op toggle 409,
//! and 404s. Three real checkouts build the usage: two stand,
//! one is buyer-cancelled — and the cancelled redemption is
//! NOT removed (the gross-vs-net rule the page labels).
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

/// Checkout the buyer's current cart with COD and the coupon; returns the order.
async fn checkout_with_coupon(app: &Router, token: &str, code: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "coupon_code": code
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout with {code}: {body}");
    body
}

/// GET the admin coupon detail; asserts 200.
async fn get_detail(app: &Router, admin: &str, coupon_id: &str) -> Value {
    let uri = format!("/v1/admin/coupons/{coupon_id}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(admin)).await;
    assert_eq!(status, StatusCode::OK, "detail: {body}");
    body
}

/// Count audit rows for one coupon and action.
async fn audit_count(pool: &PgPool, coupon_id: &str, action: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_audit_log \
         WHERE target_type = 'coupon' AND target_id = $1 AND action = $2",
    )
    .bind(coupon_id)
    .bind(action)
    .fetch_one(pool)
    .await
    .expect("count audit rows")
}

#[tokio::test]
async fn admincoupons_detail_usage_masking_and_history() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin coupons test D");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, api_keys, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         buyer_addresses, bulk_imports, store_layouts, storefront_events, \
         store_pages, store_posts, store_post_events, \
         store_collections, store_collection_products, \
         product_images, product_variants, products, store_members, store_follows, \
         gift_card_ledger, gift_cards, \
         stores, otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "admincouponsd-test-secret".to_string(), true);
    let app = build_app(state);

    let admin = make_admin(&app, &pool, "01711111111", "+8801711111111").await;
    let seller_a = login(&app, "01733333333").await;
    let store_a = create_store(&app, &seller_a, "coupons-d-a").await;
    let product_a = create_product(&app, &seller_a, &store_a, "Product A", 245_000, 20).await;
    let seller_b = login(&app, "01744444444").await;
    let store_b = create_store(&app, &seller_b, "coupons-d-b").await;
    let product_b = create_product(&app, &seller_b, &store_b, "Product B", 200_000, 20).await;
    let buyer1 = login(&app, "01755551111").await;
    let buyer2 = login(&app, "01766662222").await;

    let (status, created) = call(
        &app,
        Method::POST,
        "/v1/admin/coupons",
        Some(json!({
            "code": "DETAIL10",
            "discount_type": "percent",
            "discount_value": 10,
            "max_discount_poisha": 5000
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create DETAIL10: {created}");
    let coupon_id = created["id"].as_str().expect("coupon id").to_string();

    // Three redemptions of the standard cart (5_000 each):
    // buyer1, buyer2, then buyer1 again — whose order is
    // cancelled, which must NOT remove the redemption.
    for buyer in [&buyer1, &buyer2] {
        add_to_cart(&app, buyer, &product_a, 1).await;
        add_to_cart(&app, buyer, &product_b, 1).await;
        checkout_with_coupon(&app, buyer, "DETAIL10").await;
    }
    add_to_cart(&app, &buyer1, &product_a, 1).await;
    add_to_cart(&app, &buyer1, &product_b, 1).await;
    let third = checkout_with_coupon(&app, &buyer1, "DETAIL10").await;
    let third_id = third["id"].as_str().expect("third order id").to_string();
    let (status, cancelled) = call(
        &app,
        Method::POST,
        &format!("/v1/orders/{third_id}/cancel"),
        None,
        Some(&buyer1),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "buyer cancel: {cancelled}");
    assert_eq!(cancelled["status"], "cancelled");

    // The detail: usage from the snapshots, cross-checked
    // against SQL ground truth.
    let detail = get_detail(&app, &admin, &coupon_id).await;
    assert_eq!(detail["code"], "DETAIL10");
    assert_eq!(detail["status"], "live");
    assert_eq!(detail["store_name"], Value::Null);
    let usage = &detail["usage"];
    assert_eq!(usage["gross_redemptions"], 3);
    assert_eq!(usage["gross_discount_poisha"], 15_000);
    assert_eq!(usage["cancelled_redemptions"], 1);
    assert_eq!(usage["cancelled_discount_poisha"], 5_000);
    assert_eq!(usage["returned_sub_orders"], 0);
    assert_eq!(usage["returned_discount_poisha"], 0);
    assert_eq!(usage["net_effective_discount_poisha"], 10_000);
    assert_eq!(usage["distinct_buyers"], 2);
    assert!(usage["first_redeemed_at"].is_string(), "first set: {usage}");
    assert!(usage["last_redeemed_at"].is_string(), "last set: {usage}");

    let coupon_uuid = Uuid::parse_str(&coupon_id).unwrap();
    let (sql_gross, sql_gross_discount, sql_buyers): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(discount_poisha), 0)::bigint, \
         COUNT(DISTINCT buyer_id) FROM coupon_redemptions WHERE coupon_id = $1",
    )
    .bind(coupon_uuid)
    .fetch_one(&pool)
    .await
    .expect("sql usage ground truth");
    assert_eq!(usage["gross_redemptions"], sql_gross);
    assert_eq!(usage["gross_discount_poisha"], sql_gross_discount);
    assert_eq!(usage["distinct_buyers"], sql_buyers);

    // Recent redemptions: three rows, one on the cancelled
    // order, every buyer phone masked — and the raw body
    // carries NO full phone in either form and no unmasked
    // buyer_phone key.
    let recent = detail["recent_redemptions"].as_array().expect("recent array");
    assert_eq!(recent.len(), 3);
    assert!(
        recent.iter().any(|row| row["order_status"] == "cancelled"),
        "the cancelled order's redemption is listed: {recent:?}"
    );
    for row in recent {
        let masked = row["buyer_phone_masked"].as_str().expect("masked phone");
        assert!(masked.contains('*'), "phone is masked: {masked}");
    }
    let raw = detail.to_string();
    for leaked in ["+8801755551111", "01755551111", "+8801766662222", "01766662222"] {
        assert!(!raw.contains(leaked), "detail leaks a full phone: {leaked}");
    }
    assert!(!raw.contains("\"buyer_phone\":"), "no unmasked buyer_phone key");

    // Audit history: the create is there; deactivation and
    // reactivation append, newest first.
    let history = detail["audit_history"].as_array().expect("history array");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["action"], "coupon.create");

    let toggle_uri = format!("/v1/admin/coupons/{coupon_id}");
    let (status, response) = call(
        &app,
        Method::PATCH,
        &toggle_uri,
        Some(json!({ "active": false, "reason": "বিরতি — paused" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "deactivate: {response}");
    let detail = get_detail(&app, &admin, &coupon_id).await;
    let history = detail["audit_history"].as_array().expect("history array");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["action"], "coupon.deactivate", "newest first");
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.deactivate").await, 1);

    let (status, response) = call(
        &app,
        Method::PATCH,
        &toggle_uri,
        Some(json!({ "active": true })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reactivate: {response}");
    let detail = get_detail(&app, &admin, &coupon_id).await;
    assert_eq!(detail["audit_history"].as_array().map(Vec::len), Some(3));

    // A no-op toggle is a 409 that writes nothing.
    let (status, response) = call(
        &app,
        Method::PATCH,
        &toggle_uri,
        Some(json!({ "active": true })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "no-op toggle: {response}");
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.reactivate").await, 1);
    let detail = get_detail(&app, &admin, &coupon_id).await;
    assert_eq!(detail["audit_history"].as_array().map(Vec::len), Some(3));
    assert_eq!(detail["active"], true);

    // Unknown ids 404 on both the detail and the toggle.
    let missing = "00000000-0000-0000-0000-000000000000";
    let (status, response) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/coupons/{missing}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "missing detail: {response}");
    let (status, response) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/coupons/{missing}"),
        Some(json!({ "active": false, "reason": "x" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "missing toggle: {response}");

    // A store coupon's detail carries its store identity; a
    // never-redeemed coupon's usage is zeroed with null stamps.
    let (status, store_coupon) = call(
        &app,
        Method::POST,
        "/v1/seller/coupons",
        Some(json!({ "code": "STOREDET", "discount_type": "fixed", "discount_value": 10000 })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "store coupon: {store_coupon}");
    let store_coupon_id = store_coupon["id"].as_str().expect("store coupon id").to_string();
    let detail = get_detail(&app, &admin, &store_coupon_id).await;
    assert_eq!(detail["store_name"], "Store coupons-d-a");
    assert_eq!(detail["store_slug"], "coupons-d-a");
    assert_eq!(detail["usage"]["gross_redemptions"], 0);
    assert_eq!(detail["usage"]["gross_discount_poisha"], 0);
    assert_eq!(detail["usage"]["net_effective_discount_poisha"], 0);
    assert_eq!(detail["usage"]["first_redeemed_at"], Value::Null);
    assert_eq!(detail["usage"]["last_redeemed_at"], Value::Null);
    assert_eq!(detail["recent_redemptions"].as_array().map(Vec::len), Some(0));
    assert_eq!(detail["audit_history"].as_array().map(Vec::len), Some(0));
}
