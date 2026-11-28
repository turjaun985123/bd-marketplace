//! Wave 60 integration test: the extension surfaces —
//! gift cards ride the seller data export (masked: no
//! code, no hash anywhere in the document), the buyer's
//! export shows the redemption on their order, the admin
//! metrics carry the platform's gift-card totals, and a
//! Wave 37 API key may READ cards but may not ISSUE one
//! (issuance is JWT-only, like payout requests).
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

#[tokio::test]
async fn wave60_exports_metrics_and_api_key_restriction() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let buyer = login(&app, "01733333333").await;
    let (admin, admin_id) = login_with_id(&app, "01744444444").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE id = $1")
        .bind(sqlx::types::Uuid::parse_str(&admin_id).expect("uuid"))
        .execute(&pool)
        .await
        .expect("promote admin");
    let store_id = create_store(&app, &seller, "w60j-store").await;
    let product = create_product(&app, &seller, &store_id, "Thing", 60_000, 10).await;

    // Issue ৳1,000 and spend ৳600 of it.
    let (status, issued) = call(
        &app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": 100_000, "note": "export pin" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "issue: {issued}");
    let code = issued["cards"][0]["code"]
        .as_str()
        .expect("code")
        .to_string();
    let last4 = issued["cards"][0]["last4"]
        .as_str()
        .expect("last4")
        .to_string();
    add_to_cart(&app, &buyer, &product, 1).await;
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
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 60_000);

    // 1. Seller export: the gift_cards section carries the
    // card with its post-spend balance — and the document
    // NOWHERE contains the full code or a code hash.
    let (status, export) = call(&app, Method::GET, "/v1/seller/export", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "seller export: {export}");
    let cards = export["gift_cards"].as_array().expect("gift_cards section");
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["last4"], last4);
    assert_eq!(cards[0]["initial_amount_poisha"], 100_000);
    assert_eq!(cards[0]["balance_poisha"], 40_000);
    assert_eq!(cards[0]["note"], "export pin");
    let document_text = export.to_string();
    assert!(
        !document_text.contains(&code),
        "the export must never contain the full code"
    );
    assert!(
        !document_text.contains("code_hash"),
        "the export must never contain card hashes"
    );
    // The seller's sub-order in the same export shows the
    // redemption that reduced the payout.
    assert_eq!(
        export["sub_orders"][0]["gift_card_discount_poisha"], 60_000,
        "export sub-order: {}", export["sub_orders"]
    );

    // 2. Buyer export: their order row shows the redemption
    // and the card tail.
    let (status, export) = call(&app, Method::GET, "/v1/buyer/export", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer export: {export}");
    assert_eq!(export["orders"][0]["gift_card_discount_poisha"], 60_000);
    assert_eq!(export["orders"][0]["gift_card_last4"], last4);
    assert_eq!(
        export["orders"][0]["sub_orders"][0]["gift_card_discount_poisha"], 60_000
    );

    // 3. Admin metrics: the platform gift-card block reads
    // the same figures.
    let (status, metrics) = call(&app, Method::GET, "/v1/admin/metrics", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "metrics: {metrics}");
    let gift = &metrics["gift_cards"];
    assert_eq!(gift["cards_total"], 1);
    assert_eq!(gift["active_cards"], 1);
    assert_eq!(gift["initial_total_poisha"], 100_000);
    assert_eq!(gift["outstanding_poisha"], 40_000);
    assert_eq!(gift["redeemed_total_poisha"], 60_000);
    assert_eq!(gift["restored_total_poisha"], 0);

    // 4. API keys: a read+write key may READ the card list
    // (money reading is readable) but may NOT issue —
    // issuance is JWT-only like payout requests, so a
    // leaked key cannot mint store liability.
    let (status, key) = call(
        &app,
        Method::POST,
        "/v1/api-keys",
        Some(json!({
            "name": "Seller sync",
            "scopes": ["read", "write"],
            "expires_in_days": null
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create api key: {key}");
    let api_key = key["key"].as_str().expect("full key once").to_string();
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/seller/gift-cards",
        None,
        Some(&api_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "key reads the list: {list}");
    assert_eq!(list["total"], 1);
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": 10_000 })),
        Some(&api_key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "key cannot issue: {body}");
    let card_id = list["items"][0]["id"].as_str().expect("card id").to_string();
    let uri = format!("/v1/seller/gift-cards/{card_id}/disable");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&api_key)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "key cannot freeze: {body}");
}
