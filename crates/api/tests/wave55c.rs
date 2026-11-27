//! Wave 55c integration test: cart v2 — save-for-later and
//! the stock-capped quantity write. A saved line leaves the
//! goods total, cannot check out (a cart of only saved lines
//! is "empty" to checkout), survives DELETE /v1/cart, and
//! returns on unsave; adding a saved product again revives
//! its line into the active cart. The quantity PATCH now
//! refuses more than the line's real stock — the product's
//! stock for a plain line, the VARIANT's stock for a variant
//! line — with the bilingual 400, while exactly-at-stock is
//! accepted.
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

/// GET the buyer's cart view.
async fn get_cart(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/cart", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "get cart: {body}");
    body
}

/// Find one line in a cart array by product id.
fn line_of<'a>(cart: &'a Value, key: &str, product_id: &str) -> Option<&'a Value> {
    cart[key]
        .as_array()
        .expect("cart array")
        .iter()
        .find(|row| row["product_id"] == product_id)
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55c integration tests");
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
    let state = AppState::new(Some(pool.clone()), "wave55c-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

#[tokio::test]
async fn wave55c_cart_save_for_later_and_stock_caps_flow() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01744444444").await;
    let store = create_store(&app, &seller, "wave55c-store").await;
    // Plain product: stock 3. Variant product: S stock 2, L stock 5.
    let plain = create_product(&app, &seller, &store, "Wave55C Plain", 10_000, 3).await;
    let sized = create_product(&app, &seller, &store, "Wave55C Sized", 20_000, 0).await;
    let uri = format!("/v1/seller/products/{sized}/variants");
    let (status, small) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "name": "S", "price_delta_poisha": 0, "stock": 2, "sort": 0 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create variant S: {small}");
    let small_id = small["id"].as_str().expect("variant id").to_string();
    let buyer = login(&app, "01933333333").await;

    // Cart: plain x2 + sized(S) x1 — total 40_000, no saved.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": plain, "quantity": 2 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add plain: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": sized, "quantity": 1, "variant_id": small_id })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add sized: {body}");
    let cart = get_cart(&app, &buyer).await;
    assert_eq!(cart["goods_total_poisha"], 40_000);
    assert_eq!(cart["saved_items"].as_array().expect("saved").len(), 0);

    // 1. Save the plain line: it leaves the items and the
    // total, and lands on the shelf flagged saved.
    let uri = format!("/v1/cart/items/{plain}/save");
    let (status, cart) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({})),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save plain: {cart}");
    assert_eq!(cart["goods_total_poisha"], 20_000);
    assert!(line_of(&cart, "items", &plain).is_none(), "plain left the cart");
    let saved = line_of(&cart, "saved_items", &plain).expect("plain on the shelf");
    assert_eq!(saved["saved"], true);
    assert_eq!(saved["quantity"], 2);

    // 2. Save the variant line too (variant addressed in
    // the body): the cart is now empty for checkout.
    let uri = format!("/v1/cart/items/{sized}/save");
    let (status, cart) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "variant_id": small_id })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save sized: {cart}");
    assert_eq!(cart["items"].as_array().expect("items").len(), 0);
    assert_eq!(cart["saved_items"].as_array().expect("saved").len(), 2);
    let (status, refused) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01933333333",
            "delivery_address": "House 9",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "saved lines never check out: {refused}"
    );

    // 3. Unsave the plain line; then CLEAR the cart — the
    // still-saved variant line survives the clear.
    let uri = format!("/v1/cart/items/{plain}/unsave");
    let (status, cart) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({})),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unsave plain: {cart}");
    assert_eq!(cart["goods_total_poisha"], 20_000);
    let (status, cart) = call(&app, Method::DELETE, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "clear cart: {cart}");
    assert_eq!(cart["items"].as_array().expect("items").len(), 0);
    assert_eq!(
        cart["saved_items"].as_array().expect("saved").len(),
        1,
        "the saved line survives clear: {cart}"
    );

    // 4. Stock caps on the quantity write. Bring the
    // variant back, re-add the plain line, then push both
    // past their stock.
    let uri = format!("/v1/cart/items/{sized}/unsave");
    let (status, _) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "variant_id": small_id })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unsave sized");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": plain, "quantity": 1 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "re-add plain: {body}");

    let uri = format!("/v1/cart/items/{plain}");
    let (status, refused) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "quantity": 4 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "over product stock: {refused}");
    assert!(
        refused["error"]
            .as_str()
            .expect("error message")
            .contains("স্টকে মাত্র 3টি"),
        "the bilingual cap message: {refused}"
    );
    let (status, cart) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "quantity": 3 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "exactly at stock is fine: {cart}");
    assert_eq!(line_of(&cart, "items", &plain).expect("plain line")["quantity"], 3);

    let uri = format!("/v1/cart/items/{sized}");
    let (status, refused) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "quantity": 3, "variant_id": small_id })),
        Some(&buyer),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "over VARIANT stock (2), not the product's: {refused}"
    );
    let (status, cart) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "quantity": 2, "variant_id": small_id })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "at variant stock: {cart}");

    // 5. Adding a saved line revives it: save the plain
    // line (qty 3), add 1 more — the line is back in the
    // active cart with the accumulated 4, off the shelf.
    let uri = format!("/v1/cart/items/{plain}/save");
    let (status, cart) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({})),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save plain again: {cart}");
    let (status, cart) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": plain, "quantity": 1 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add revives: {cart}");
    let revived = line_of(&cart, "items", &plain).expect("plain revived");
    assert_eq!(revived["quantity"], 4, "accumulated onto the saved line");
    assert_eq!(revived["saved"], false);
    assert!(line_of(&cart, "saved_items", &plain).is_none());

    // 6. Moving a line that is not in the cart at all is a
    // 404; saving an already-saved line is a no-op success.
    let uri = format!("/v1/cart/items/{plain}/save");
    let (status, _) = call(&app, Method::POST, &uri, Some(json!({})), Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "save once");
    let (status, cart) = call(&app, Method::POST, &uri, Some(json!({})), Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "save twice is a no-op: {cart}");
    assert!(line_of(&cart, "saved_items", &plain).is_some());
    let (status, refused) = call(
        &app,
        Method::POST,
        "/v1/cart/items/00000000-0000-0000-0000-000000000000/save",
        Some(json!({})),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown line: {refused}");
}
