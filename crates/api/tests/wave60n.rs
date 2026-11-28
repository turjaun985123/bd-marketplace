//! Wave 60 integration test: one card's whole life, with
//! the ledger audited at the end — bulk issue, a full
//! spend, a cancel restore, an admin correction, a
//! disable/enable cycle, a final draining spend — plus
//! the companion rule on a second card: an admin adjust
//! on an EXPIRED card moves its balance but never revives
//! it (an adjust is not an un-expiry, exactly as it is
//! not an unfreeze).
//!
//! Gated on `DATABASE_URL`: skips cleanly so a plain local
//! `cargo test` (no database) still passes. CI provides a
//! Postgres 16 service and sets `DATABASE_URL`.

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

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 60n integration tests");
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

/// POST one active product; returns its id.
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

/// The public balance check for a code.
async fn balance_check(app: &Router, code: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/gift-cards/balance",
        Some(json!({ "code": code })),
        None,
    )
    .await
}

/// COD checkout presenting a gift-card code.
async fn checkout_with_card(
    app: &Router,
    token: &str,
    code: &str,
) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01966666666",
            "delivery_address": "House 3, Road 5, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "gift_card_code": code
        })),
        Some(token),
    )
    .await
}

/// The ledger kinds of one card, newest first, via the
/// seller detail view.
async fn ledger_kinds(app: &Router, token: &str, card_id: &str) -> Vec<String> {
    let uri = format!("/v1/seller/gift-cards/{card_id}");
    let (status, detail) = call(app, Method::GET, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "card detail: {detail}");
    detail["ledger"]
        .as_array()
        .expect("ledger")
        .iter()
        .map(|row| row["kind"].as_str().expect("kind").to_string())
        .collect()
}

/// Pin the ledger invariant for one card: the signed
/// ledger sum equals the cached balance.
async fn assert_ledger_sums_to_balance(pool: &PgPool, card_id: &str) {
    let (sum, balance): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COALESCE(SUM(delta_poisha), 0)::bigint \
                 FROM gift_card_ledger WHERE gift_card_id = $1::uuid), \
                (SELECT balance_poisha FROM gift_cards WHERE id = $1::uuid)",
    )
    .bind(card_id)
    .fetch_one(pool)
    .await
    .expect("ledger sum and balance");
    assert_eq!(sum, balance, "ledger sums to the cached balance");
}

#[tokio::test]
async fn wave60_full_lifecycle_saga_with_ledger_audit() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01766666666").await;
    let store = create_store(&app, &seller, "wave60n-store").await;
    let product = create_product(&app, &seller, &store, "Wave60N Item", 150_000, 10).await;
    let buyer = login(&app, "01966666666").await;
    // Admin: login FIRST (creates the user row), then the
    // SQL promotion — the standing convention.
    let admin = login(&app, "01766666667").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801766666667'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // Bulk issue: two 100_000 cards, X and Y.
    let (status, issued) = call(
        &app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": 100_000, "count": 2 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "bulk issue: {issued}");
    let card_x = issued["cards"][0].clone();
    let card_y = issued["cards"][1].clone();
    let x_id = card_x["id"].as_str().expect("X id").to_string();
    let y_id = card_y["id"].as_str().expect("Y id").to_string();
    let x_code = card_x["code"].as_str().expect("X code").to_string();
    let y_code = card_y["code"].as_str().expect("Y code").to_string();

    // X: full spend against 150_000 of goods → depleted.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = checkout_with_card(&app, &buyer, &x_code).await;
    assert_eq!(status, StatusCode::CREATED, "X spend: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 100_000);
    assert_eq!(order["grand_total_poisha"], 56_000);
    let order_id = order["id"].as_str().expect("order id").to_string();
    let (status, balance) = balance_check(&app, &x_code).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["status"], "depleted");

    // X: buyer cancel restores it in full.
    let uri = format!("/v1/orders/{order_id}/cancel");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cancel: {body}");
    assert_eq!(
        ledger_kinds(&app, &seller, &x_id).await,
        ["cancel_restore", "redeem", "issue"]
    );
    assert_ledger_sums_to_balance(&pool, &x_id).await;

    // X: an admin correction takes 30_000 back out.
    let uri = format!("/v1/admin/gift-cards/{x_id}/adjust");
    let (status, view) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "delta_poisha": -30_000, "note": "campaign correction" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin adjust X: {view}");
    assert_eq!(view["balance_poisha"], 70_000);
    assert_eq!(view["status"], "active");

    // X: disable refuses spending; enable restores it; the
    // remaining 70_000 drains against the next order.
    let uri = format!("/v1/seller/gift-cards/{x_id}/disable");
    let (status, view) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "disable X: {view}");
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, body) = checkout_with_card(&app, &buyer, &x_code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "disabled X refused: {body}");
    let uri = format!("/v1/seller/gift-cards/{x_id}/enable");
    let (status, view) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "enable X: {view}");
    // The refused checkout left the cart intact — retry.
    let (status, order) = checkout_with_card(&app, &buyer, &x_code).await;
    assert_eq!(status, StatusCode::CREATED, "X final spend: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 70_000);
    assert_eq!(order["grand_total_poisha"], 86_000);
    assert_eq!(
        ledger_kinds(&app, &seller, &x_id).await,
        ["redeem", "admin_adjust", "cancel_restore", "redeem", "issue"]
    );
    assert_ledger_sums_to_balance(&pool, &x_id).await;
    let (status, balance) = balance_check(&app, &x_code).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["balance_poisha"], 0);
    assert_eq!(balance["status"], "depleted");

    // Y: expire it by backdating, then adjust +10_000 —
    // the balance moves, the expiry stands, spending is
    // still refused.
    sqlx::query(
        "UPDATE gift_cards SET expires_at = now() - interval '1 hour' WHERE id = $1::uuid",
    )
    .bind(&y_id)
    .execute(&pool)
    .await
    .expect("backdate Y expiry");
    let uri = format!("/v1/admin/gift-cards/{y_id}/adjust");
    let (status, view) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "delta_poisha": 10_000, "note": "goodwill top-up" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin adjust Y: {view}");
    assert_eq!(view["balance_poisha"], 110_000);
    assert_eq!(view["status"], "expired", "an adjust never un-expires");
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, body) = checkout_with_card(&app, &buyer, &y_code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "expired Y refused: {body}");
    assert_eq!(
        ledger_kinds(&app, &seller, &y_id).await,
        ["admin_adjust", "issue"]
    );
    assert_ledger_sums_to_balance(&pool, &y_id).await;
}
