//! Wave 60 integration test: the derived-status FILTER
//! on both list endpoints. Status is computed at read
//! (disabled > expired > depleted > active), and the SQL
//! filter fragment must mirror the domain derivation
//! exactly — a card disabled AND past expiry counts as
//! disabled in the filter just as it does in the view, a
//! zero-balance expired card counts as expired, not
//! depleted. Five cards, one per outcome class (plus the
//! precedence collision), filtered on the seller list and
//! the admin list alike, with page/limit pagination.
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
            eprintln!("DATABASE_URL not set — skipping Wave 60q integration tests");
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

/// Issue one card with an explicit body; returns the card view.
async fn issue_with(app: &Router, token: &str, body: Value) -> Value {
    let (status, response) = call(
        app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(body),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "issue card: {response}");
    response["cards"][0].clone()
}

/// GET a gift-card list URI; returns the parsed body.
async fn list_cards(app: &Router, token: &str, uri: &str) -> Value {
    let (status, body) = call(app, Method::GET, uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "list {uri}: {body}");
    body
}

#[tokio::test]
async fn wave60_list_status_filters_mirror_the_derived_status() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01766666671").await;
    let store = create_store(&app, &seller, "wave60q-store").await;
    let product = create_product(&app, &seller, &store, "Wave60Q Item", 30_000, 5).await;
    let buyer = login(&app, "01966666671").await;
    // Admin: login FIRST, then the SQL promotion.
    let admin = login(&app, "01766666672").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '01766666672'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // One card per outcome class.
    // 1–2: active (one no-expiry, one timed).
    issue_with(&app, &seller, json!({ "amount_poisha": 10_000, "no_expiry": true })).await;
    issue_with(&app, &seller, json!({ "amount_poisha": 10_000, "validity_days": 90 })).await;
    // 3: disabled AND backdated past expiry — the
    // precedence collision: it must count as DISABLED.
    let frozen = issue_with(
        &app,
        &seller,
        json!({ "amount_poisha": 10_000, "validity_days": 90 }),
    )
    .await;
    let frozen_id = frozen["id"].as_str().expect("frozen id").to_string();
    let uri = format!("/v1/seller/gift-cards/{frozen_id}/disable");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "disable: {body}");
    sqlx::query(
        "UPDATE gift_cards SET expires_at = now() - interval '1 hour' WHERE id = $1::uuid",
    )
    .bind(&frozen_id)
    .execute(&pool)
    .await
    .expect("backdate frozen card");
    // 4: expired with its balance intact.
    let stale = issue_with(
        &app,
        &seller,
        json!({ "amount_poisha": 10_000, "validity_days": 90 }),
    )
    .await;
    let stale_id = stale["id"].as_str().expect("stale id").to_string();
    sqlx::query(
        "UPDATE gift_cards SET expires_at = now() - interval '1 hour' WHERE id = $1::uuid",
    )
    .bind(&stale_id)
    .execute(&pool)
    .await
    .expect("backdate stale card");
    // 5: depleted by a real spend.
    let spent = issue_with(&app, &seller, json!({ "amount_poisha": 30_000 })).await;
    let spent_code = spent["code"].as_str().expect("spent code").to_string();
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01966666671",
            "delivery_address": "House 3, Road 5, Dhaka",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "gift_card_code": spent_code
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "draining checkout: {order}");

    // The seller list, filtered per status.
    let all = list_cards(&app, &seller, "/v1/seller/gift-cards").await;
    assert_eq!(all["total"], 5, "all five cards listed");
    let active = list_cards(&app, &seller, "/v1/seller/gift-cards?status=active").await;
    assert_eq!(active["total"], 2, "two active");
    let disabled = list_cards(&app, &seller, "/v1/seller/gift-cards?status=disabled").await;
    assert_eq!(disabled["total"], 1, "the frozen card counts once");
    assert_eq!(disabled["items"][0]["id"], frozen_id);
    assert_eq!(disabled["items"][0]["status"], "disabled");
    let expired = list_cards(&app, &seller, "/v1/seller/gift-cards?status=expired").await;
    assert_eq!(expired["total"], 1, "only the stale card is expired");
    assert_eq!(expired["items"][0]["id"], stale_id);
    let depleted = list_cards(&app, &seller, "/v1/seller/gift-cards?status=depleted").await;
    assert_eq!(depleted["total"], 1, "the spent card is depleted");
    // An out-of-vocabulary filter is a 400, not an empty list.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/gift-cards?status=bogus",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bogus status refused: {body}");

    // Pagination: page/limit walk the same five cards.
    let page1 = list_cards(&app, &seller, "/v1/seller/gift-cards?limit=2&page=1").await;
    assert_eq!(page1["total"], 5);
    assert_eq!(page1["items"].as_array().expect("items").len(), 2);
    let page3 = list_cards(&app, &seller, "/v1/seller/gift-cards?limit=2&page=3").await;
    assert_eq!(page3["items"].as_array().expect("items").len(), 1);

    // The admin list applies the same derivation.
    let admin_active = list_cards(&app, &admin, "/v1/admin/gift-cards?status=active").await;
    assert_eq!(admin_active["total"], 2, "admin sees the same active set");
    let admin_disabled = list_cards(&app, &admin, "/v1/admin/gift-cards?status=disabled").await;
    assert_eq!(admin_disabled["total"], 1);
    assert_eq!(admin_disabled["items"][0]["id"], frozen_id);
}
