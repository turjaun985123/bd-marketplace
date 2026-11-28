//! Wave 58 integration test: the best-sellers rail is
//! fed by REAL delivered sales, end to end:
//! - before any sale, a best-sellers rail resolves empty
//!   and drops from the public layout;
//! - a buyer checks out a COD order (2 of A, 1 of B) and
//!   the seller walks the sub-order to delivered;
//! - the rail then renders, ordered by delivered units
//!   (A before B), with the products' sold counts
//!   agreeing — the platform's one "sold" definition,
//!   the same aggregate the old StorefrontSections rail
//!   sorted by.
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
            eprintln!("DATABASE_URL not set — skipping Wave 58 integration tests");
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
         store_collections, store_collection_products, \
         product_images, product_variants, products, store_members, store_follows, \
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
    let state = AppState::new(Some(pool.clone()), "wave58-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// POST a store for the token's user; returns the store view.
async fn create_store(app: &Router, token: &str, slug: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body
}

/// POST one active product; returns its id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
) -> String {
    let (status, created) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": 20,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {created}");
    created["id"].as_str().expect("product id").to_string()
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

/// Find one section in a layout body by id.
fn section_by_id<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["sections"]
        .as_array()?
        .iter()
        .find(|section| section["id"].as_str() == Some(id))
}

#[tokio::test]
async fn wave58_best_sellers_rail_from_real_delivered_sales() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w58-sold").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let product_a = create_product(&app, &seller, &store_id, "Popular A", 100_000).await;
    let product_b = create_product(&app, &seller, &store_id, "Steady B", 120_000).await;

    // The layout: just the best-sellers rail.
    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "sections": [
                {
                    "id": "best",
                    "type": "product_rail",
                    "config": { "source": "best_sellers", "title": "সেরা বিক্রিত" }
                }
            ]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put layout: {saved}");

    // No sales yet: the rail resolves empty and drops —
    // the public layout is an empty composition.
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-sold/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(
        public["sections"].as_array().expect("sections").len(),
        0,
        "nothing sold, no rail: {public}"
    );

    // A real COD order: 2 of A, 1 of B, walked to
    // delivered by the seller.
    let buyer = login(&app, "01755555555").await;
    add_to_cart(&app, &buyer, &product_a, 2).await;
    add_to_cart(&app, &buyer, &product_b, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub order id")
        .to_string();
    for status in ["confirmed", "shipped", "delivered"] {
        let (code, body) = set_sub_order_status(&app, &seller, &sub_order_id, status).await;
        assert_eq!(code, StatusCode::OK, "walk to {status}: {body}");
    }

    // Now the rail renders, A (2 units) ahead of B (1),
    // with the cards' sold counts agreeing.
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-sold/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    let rail = section_by_id(&public, "best").expect("best rail renders now");
    let items = rail["products"].as_array().expect("rail products");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], json!(product_a), "most units first");
    assert_eq!(items[0]["sold_count"], json!(2));
    assert_eq!(items[1]["id"], json!(product_b));
    assert_eq!(items[1]["sold_count"], json!(1));
}
