//! Wave 60 integration test: the admin surface — an admin
//! finds any store's card by search, disables it (the
//! freeze is audited in the same transaction), adjusts its
//! balance with a mandatory note (ledger row + audit row),
//! and cannot drive a balance below zero. Non-admins are
//! refused everywhere on this surface.
//!
//! Gated on `DATABASE_URL`: skips cleanly without one.

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

/// Full dev-mode OTP login; returns (JWT, user id).
async fn login_with_id(app: &Router, phone: &str) -> (String, String) {
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
    let user_id = body["user"]["id"]
        .as_str()
        .expect("user id")
        .to_string();
    (token, user_id)
}

/// Full dev-mode OTP login for a phone number; returns the JWT.
async fn login(app: &Router, phone: &str) -> String {
    login_with_id(app, phone).await.0
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 60 integration tests");
            return None;
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
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave60-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// POST a store for the token's user; returns the store id.
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

/// Create one active product; returns the product id.
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

/// Count audit rows for one action, from SQL.
async fn audit_count(pool: &PgPool, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*)::bigint FROM admin_audit_log WHERE action = $1")
        .bind(action)
        .fetch_one(pool)
        .await
        .expect("audit count")
}

#[tokio::test]
async fn wave60_admin_disable_adjust_and_audit() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let buyer = login(&app, "01733333333").await;
    // Admin: login FIRST (creates the user row), then the
    // SQL promotion — the standing convention.
    let (admin, admin_id) = login_with_id(&app, "01744444444").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE id = $1")
        .bind(sqlx::types::Uuid::parse_str(&admin_id).expect("uuid"))
        .execute(&pool)
        .await
        .expect("promote admin");

    let store_id = create_store(&app, &seller, "w60g-store").await;
    let product = create_product(&app, &seller, &store_id, "Thing", 60_000, 10).await;
    let (status, issued) = call(
        &app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": 40_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "issue: {issued}");
    let card_id = issued["cards"][0]["id"].as_str().expect("card id").to_string();
    let code = issued["cards"][0]["code"].as_str().expect("code").to_string();
    let last4 = issued["cards"][0]["last4"].as_str().expect("last4").to_string();

    // 1. Non-admins are refused on the whole admin surface.
    let (status, _) = call(&app, Method::GET, "/v1/admin/gift-cards", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller is not an admin");
    let uri = format!("/v1/admin/gift-cards/{card_id}/disable");
    let (status, _) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "buyer is not an admin");

    // 2. The admin finds the card by its last4 and by the
    // store slug; the rows are masked like everywhere else.
    let uri = format!("/v1/admin/gift-cards?q={last4}");
    let (status, list) = call(&app, Method::GET, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin search by last4: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], card_id);
    assert!(list["items"][0].get("code").is_none());
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/admin/gift-cards?q=w60g-store",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin search by slug: {list}");
    assert_eq!(list["total"], 1);

    // 3. Admin disable: the card freezes for checkout AND
    // the audit log gains exactly one gift_card.disable row
    // naming this card.
    let uri = format!("/v1/admin/gift-cards/{card_id}/disable");
    let (status, view) = call(&app, Method::POST, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin disable: {view}");
    assert_eq!(view["status"], "disabled");
    assert_eq!(audit_count(&pool, "gift_card.disable").await, 1);
    let audit_target: String = sqlx::query_scalar(
        "SELECT target_id FROM admin_audit_log WHERE action = 'gift_card.disable'",
    )
    .fetch_one(&pool)
    .await
    .expect("audit target");
    assert_eq!(audit_target, card_id);
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "gift_card_code": code
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "admin-disabled card refused: {body}");

    // 4. Admin adjust: +10_000 with a note lands as a ledger
    // row and an audit row carrying the delta; a missing
    // note and a below-zero result are both 400s.
    let uri = format!("/v1/admin/gift-cards/{card_id}/adjust");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "delta_poisha": 10_000, "note": "  " })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "note is required: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "delta_poisha": -40_001, "note": "too much" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "below zero refused: {body}");
    let (status, view) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "delta_poisha": 10_000, "note": "support recovery — buyer lost the first code" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "adjust: {view}");
    assert_eq!(view["balance_poisha"], 50_000);
    assert_eq!(view["status"], "disabled", "an adjust never unfreezes by itself");
    assert_eq!(audit_count(&pool, "gift_card.adjust").await, 1);
    let card_uuid = sqlx::types::Uuid::parse_str(&card_id).expect("uuid");
    let (kind, delta, after): (String, i64, i64) = sqlx::query_as(
        "SELECT kind, delta_poisha, balance_after_poisha FROM gift_card_ledger \
         WHERE gift_card_id = $1 AND kind = 'admin_adjust'",
    )
    .bind(card_uuid)
    .fetch_one(&pool)
    .await
    .expect("adjust ledger row");
    assert_eq!(kind, "admin_adjust");
    assert_eq!(delta, 10_000);
    assert_eq!(after, 50_000);

    // 5. Admin enable revives the card with its adjusted
    // balance; the buyer spends it (the cart from step 3 is
    // still holding its product).
    let uri = format!("/v1/admin/gift-cards/{card_id}/enable");
    let (status, view) = call(&app, Method::POST, &uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin enable: {view}");
    assert_eq!(view["status"], "active");
    assert_eq!(audit_count(&pool, "gift_card.enable").await, 1);
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "gift_card_code": code
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "spend after enable: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 50_000);
}
