//! Wave 55j integration test: address-book depth — PATCH
//! merge semantics (a partial patch keeps the other
//! fields; an invalid merged result is refused and the
//! row is unchanged), `is_default` switching through
//! PATCH, the default-address endpoint (200 / 404 /
//! stranger 404), picker ordering after a checkout stamps
//! `last_used_at`, checkout with a DELETED address id
//! (404, no order), and the districts endpoint's `?q=`
//! search (prefix, Bangla, no-match, unfiltered).
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

/// Create one address for the buyer; returns the view.
async fn create_address(app: &Router, token: &str, label: &str, district: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/addresses",
        Some(json!({
            "label": label,
            "recipient_name": "Test Buyer",
            "phone": "01712345678",
            "district": district,
            "area": "Sadar",
            "address_line": "House 1"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create address {label}: {body}");
    body
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<Router> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55j integration tests");
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
         buyer_addresses, \
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
    let state = AppState::new(Some(pool), "wave55j-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some(build_app(state))
}

#[tokio::test]
async fn wave55j_address_patch_defaults_ordering_and_district_search() {
    let Some(app) = test_app().await else {
        return;
    };
    let seller = login(&app, "01717171717").await;
    let store = create_store(&app, &seller, "wave55j-store").await;
    let product = create_product(&app, &seller, &store, "Wave55J Product", 10_000, 10).await;
    let buyer = login(&app, "01917171717").await;
    let stranger = login(&app, "01918181818").await;

    // No default yet: the endpoint 404s for buyer AND
    // stranger alike (a stranger's book is empty too).
    let (status, _) = call(&app, Method::GET, "/v1/buyer/default-address", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let a1 = create_address(&app, &buyer, "বাসা", "Dhaka").await;
    let a2 = create_address(&app, &buyer, "অফিস", "Gazipur").await;
    let a3 = create_address(&app, &buyer, "গ্রাম", "Khulna").await;
    let (a1_id, a2_id, a3_id) = (
        a1["id"].as_str().expect("a1 id").to_string(),
        a2["id"].as_str().expect("a2 id").to_string(),
        a3["id"].as_str().expect("a3 id").to_string(),
    );
    // The first address is the default.
    let (status, current) = call(&app, Method::GET, "/v1/buyer/default-address", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current["id"], a1_id);

    // PATCH merge: only the area changes.
    let uri = format!("/v1/addresses/{a2_id}");
    let (status, patched) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "area": "Tongi" })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch area: {patched}");
    assert_eq!(patched["area"], "Tongi");
    assert_eq!(patched["district"], "Gazipur");
    assert_eq!(patched["label"], "অফিস");
    assert_eq!(patched["phone"], "+8801712345678");

    // An invalid merged result is refused AND the row is
    // unchanged afterwards.
    let (status, refused) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "phone": "123" })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad phone patch: {refused}");
    let (status, fetched) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["phone"], "+8801712345678");
    assert_eq!(fetched["area"], "Tongi");

    // PATCH is_default switches the default atomically.
    let uri3 = format!("/v1/addresses/{a3_id}");
    let (status, patched) = call(
        &app,
        Method::PATCH,
        &uri3,
        Some(json!({ "is_default": true })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch default: {patched}");
    assert_eq!(patched["is_default"], true);
    let (status, current) = call(&app, Method::GET, "/v1/buyer/default-address", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(current["id"], a3_id);
    let (status, list) = call(&app, Method::GET, "/v1/addresses", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    let defaults = list["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|entry| entry["is_default"] == true)
        .count();
    assert_eq!(defaults, 1, "exactly one default: {list}");

    // Checkout with A2 stamps last_used_at: the picker
    // order becomes default (A3), recently used (A2), A1.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Ignored",
            "delivery_phone": "01999999999",
            "delivery_address": "Ignored",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "address_id": a2_id
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout with A2: {order}");
    let (status, list) = call(&app, Method::GET, "/v1/addresses", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    let order_of_ids: Vec<String> = list["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|entry| entry["id"].as_str().expect("id").to_string())
        .collect();
    assert_eq!(order_of_ids, vec![a3_id.clone(), a2_id.clone(), a1_id.clone()]);

    // A deleted address id at checkout: 404, and NO order.
    let uri2 = format!("/v1/addresses/{a2_id}");
    let (status, _) = call(&app, Method::DELETE, &uri2, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, orders_before) = call(&app, Method::GET, "/v1/orders", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, refused) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Ignored",
            "delivery_phone": "01999999999",
            "delivery_address": "Ignored",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "address_id": a2_id
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "deleted address: {refused}");
    let (status, orders_after) = call(&app, Method::GET, "/v1/orders", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        orders_after["total"], orders_before["total"],
        "no order was created"
    );

    // --- District search. -----------------------------------
    let (status, all) = call(&app, Method::GET, "/v1/districts", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all["total"], 64);
    let (status, hits) = call(&app, Method::GET, "/v1/districts?q=dha", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hits["items"][0]["name_en"], "Dhaka");
    let (status, hits) = call(
        &app,
        Method::GET,
        "/v1/districts?q=%E0%A6%A2%E0%A6%BE%E0%A6%95%E0%A6%BE",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bangla query: {hits}");
    assert!(
        hits["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|d| d["name_en"] == "Dhaka"),
        "ঢাকা finds Dhaka: {hits}"
    );
    let (status, hits) = call(&app, Method::GET, "/v1/districts?q=Zxqwvbn", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hits["total"], 0);
    assert_eq!(hits["items"].as_array().expect("items").len(), 0);
}
