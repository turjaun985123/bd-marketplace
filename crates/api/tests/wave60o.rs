//! Wave 60 integration test: the bearer property and the
//! code-format matrix. A gift card belongs to whoever
//! HOLDS the code — a second, unrelated buyer can check
//! its balance and spend it, because no account owns it
//! (that is the instrument's design, and its documented
//! risk). And the code's typed form is forgiving exactly
//! where humans are sloppy: lowercase, spaces and hyphen
//! grouping all normalise to the same card, while an
//! empty or whitespace-only `gift_card_code` is treated
//! as ABSENT (a plain checkout), never as an error.
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
            eprintln!("DATABASE_URL not set — skipping Wave 60o integration tests");
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

/// Issue one card for the caller's store; returns (view, code).
async fn issue_card(app: &Router, token: &str, amount_poisha: i64) -> (Value, String) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": amount_poisha })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "issue card: {body}");
    let card = body["cards"][0].clone();
    let code = card["code"]
        .as_str()
        .expect("full code shown once")
        .to_string();
    (card, code)
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

/// COD checkout; `gift_card_code` is sent verbatim when
/// Some (including the empty string), omitted when None.
async fn checkout(
    app: &Router,
    token: &str,
    gift_card_code: Option<&str>,
) -> (StatusCode, Value) {
    let mut body = json!({
        "delivery_name": "Test Buyer",
        "delivery_phone": "01966666666",
        "delivery_address": "House 3, Road 5, Dhaka",
        "delivery_zone": "inside_dhaka",
        "payment_method": "cod"
    });
    if let Some(code) = gift_card_code {
        body["gift_card_code"] = json!(code);
    }
    call(app, Method::POST, "/v1/orders", Some(body), Some(token)).await
}

/// Group a code the way a printed card might: a hyphen
/// after every five characters.
fn grouped(code: &str) -> String {
    let mut out = String::new();
    for (index, ch) in code.chars().enumerate() {
        if index > 0 && index % 5 == 0 {
            out.push('-');
        }
        out.push(ch);
    }
    out
}

#[tokio::test]
async fn wave60_bearer_spending_and_code_format_matrix() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01766666668").await;
    let store = create_store(&app, &seller, "wave60o-store").await;
    let product = create_product(&app, &seller, &store, "Wave60O Item", 40_000, 10).await;
    let (_card, code) = issue_card(&app, &seller, 90_000).await;

    // Bearer: the balance check needs no account at all —
    // an anonymous caller holding the code reads the true
    // balance, because holding the code IS the credential.
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK, "anonymous balance check: {balance}");
    assert_eq!(balance["balance_poisha"], 90_000);
    assert_eq!(balance["store_slug"], "wave60o-store");

    // A buyer who is not the seller and was never named
    // anywhere spends it — first with the code typed in
    // lowercase (normalisation uppercases).
    let buyer_b = login(&app, "01966666669").await;
    add_to_cart(&app, &buyer_b, &product, 1).await;
    let (status, order) = checkout(&app, &buyer_b, Some(&code.to_lowercase())).await;
    assert_eq!(status, StatusCode::CREATED, "lowercase code spends: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 40_000);
    assert_eq!(order["grand_total_poisha"], 6_000);

    // Then with the printed-card grouping (hyphens, plus
    // stray spaces around it).
    add_to_cart(&app, &buyer_b, &product, 1).await;
    let messy = format!("  {}  ", grouped(&code));
    let (status, order) = checkout(&app, &buyer_b, Some(&messy)).await;
    assert_eq!(status, StatusCode::CREATED, "grouped code spends: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 40_000);
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["balance_poisha"], 10_000, "two spends of 40_000 each");

    // An empty code is ABSENT, not an error: the order is
    // a plain COD order and the card is untouched.
    add_to_cart(&app, &buyer_b, &product, 1).await;
    let (status, order) = checkout(&app, &buyer_b, Some("")).await;
    assert_eq!(status, StatusCode::CREATED, "empty code is absent: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 0);
    assert_eq!(order["grand_total_poisha"], 46_000);

    // Whitespace-only is absent too.
    add_to_cart(&app, &buyer_b, &product, 1).await;
    let (status, order) = checkout(&app, &buyer_b, Some("   ")).await;
    assert_eq!(status, StatusCode::CREATED, "blank code is absent: {order}");
    assert_eq!(order["gift_card_discount_poisha"], 0);
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["balance_poisha"], 10_000, "absent codes never touch the card");
}
