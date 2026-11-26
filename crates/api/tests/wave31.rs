//! Wave 31 integration test: data portability exports — a
//! seller downloads one JSON document with their store, their
//! full catalogue and their sales (sub-orders with items),
//! scoped to their own stores only; a buyer downloads their
//! profile (own phone in full), their orders with sub-orders
//! and items; a user without a store gets a 404 on the seller
//! export, and both endpoints reject anonymous callers with
//! 401. The download headers carry the domain filename
//! (kind + store slug / phone + date).
//!
//! The small helpers are copied from the Wave 9 test on
//! purpose, since integration test files stay self-contained;
//! `call_raw` (status + headers + raw body) is copied from
//! the Wave 28 test, which needed headers for the same
//! reason.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.
//! CI provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
    Router,
};
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

/// Like `call`, but returns the headers and the raw body text —
/// the export assertions need Content-Disposition and the raw
/// document (to prove another seller's ids are ABSENT, not
/// just unlisted in the parsed shape).
async fn call_raw(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
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
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    (status, headers, String::from_utf8_lossy(&bytes).into_owned())
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

/// GET the seller balance for a token.
async fn get_balance(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/seller/balance", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "balance: {body}");
    body
}

/// The Content-Disposition header of a response, as text.
fn content_disposition(headers: &HeaderMap) -> String {
    headers
        .get("content-disposition")
        .and_then(|value| value.to_str().ok())
        .expect("export responses carry Content-Disposition")
        .to_string()
}

#[tokio::test]
async fn wave31_seller_and_buyer_exports_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 31 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
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

    // SMS dev mode on (OTP login); no courier/payment simulators
    // needed — a plain COD checkout is the whole fixture.
    let state = AppState::new(Some(pool.clone()), "wave31-test-secret".to_string(), true);
    let app = build_app(state);

    // Seller A with a product, seller B with their own product,
    // one buyer, and one user who will never own a store.
    let seller_a = login(&app, "01799999999").await;
    let store_a = create_store(&app, &seller_a, "wave31-store-a").await;
    let product_a = create_product(&app, &seller_a, &store_a, "Wave31 Product A", 200_000, 10).await;
    let seller_b = login(&app, "01599999999").await;
    let store_b = create_store(&app, &seller_b, "wave31-store-b").await;
    let product_b = create_product(&app, &seller_b, &store_b, "Wave31 Product B", 150_000, 5).await;
    let buyer = login(&app, "01999999999").await;
    let storeless = login(&app, "01611111111").await;

    // The buyer orders one unit of A's product.
    add_to_cart(&app, &buyer, &product_a, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    // Sanity: the helpers this file shares with Wave 9 agree
    // with the export's world (A's balance exists and is zeroed
    // before delivery; the sub-order can still move).
    let balance = get_balance(&app, &seller_a).await;
    assert_eq!(balance["available_poisha"], 0, "balance: {balance}");
    let (status, body) = set_sub_order_status(&app, &seller_a, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm sub-order: {body}");

    // 1. Seller A's export: 200, a download filename carrying
    // the kind and A's slug, A's catalogue and sale inside —
    // and B's product NOWHERE in the raw document.
    let (status, headers, raw) =
        call_raw(&app, Method::GET, "/v1/seller/export", None, Some(&seller_a)).await;
    assert_eq!(status, StatusCode::OK, "seller export: {raw}");
    let disposition = content_disposition(&headers);
    assert!(
        disposition.starts_with("attachment; filename=\"bd-marketplace-seller-wave31-store-a-"),
        "seller disposition: {disposition}"
    );
    assert!(disposition.ends_with(".json\""), "seller disposition: {disposition}");

    let export: Value = serde_json::from_str(&raw).expect("seller export is JSON");
    assert_eq!(export["kind"], "seller", "export: {export}");
    assert_eq!(export["store"]["id"], store_a, "export: {export}");
    assert_eq!(export["store"]["slug"], "wave31-store-a");
    assert_eq!(export["charges_total_poisha"], 0);

    let products = export["products"].as_array().expect("products list");
    assert!(
        products.iter().any(|p| p["id"] == product_a),
        "A's product exported: {export}"
    );
    assert!(
        !raw.contains(&product_b),
        "B's product must not leak into A's export"
    );
    assert!(
        !raw.contains(&store_b),
        "B's store must not leak into A's export"
    );

    let sub_orders = export["sub_orders"].as_array().expect("sub-orders list");
    assert_eq!(sub_orders.len(), 1, "sub-orders: {export}");
    assert_eq!(sub_orders[0]["id"], sub_order_id);
    assert_eq!(sub_orders[0]["status"], "confirmed");
    let items = sub_orders[0]["items"].as_array().expect("items list");
    assert_eq!(items.len(), 1, "items: {export}");
    assert_eq!(items[0]["product_id"], product_a);
    assert_eq!(items[0]["title_snapshot"], "Wave31 Product A");

    // 2. The buyer's export: profile with the phone IN FULL
    // (their own data), the order with its sub-order + items.
    let (status, headers, raw) =
        call_raw(&app, Method::GET, "/v1/buyer/export", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer export: {raw}");
    let disposition = content_disposition(&headers);
    assert!(
        disposition.contains("bd-marketplace-buyer-"),
        "buyer disposition: {disposition}"
    );

    let export: Value = serde_json::from_str(&raw).expect("buyer export is JSON");
    assert_eq!(export["kind"], "buyer", "export: {export}");
    assert_eq!(
        export["profile"]["phone"], "+8801999999999",
        "the buyer's own phone, in full: {export}"
    );
    let orders = export["orders"].as_array().expect("orders list");
    assert_eq!(orders.len(), 1, "orders: {export}");
    assert_eq!(orders[0]["id"], order_id);
    assert_eq!(orders[0]["delivery_phone"], "+8801999999999");
    let exported_subs = orders[0]["sub_orders"].as_array().expect("order sub-orders");
    assert_eq!(exported_subs.len(), 1, "order sub-orders: {export}");
    assert_eq!(exported_subs[0]["id"], sub_order_id);
    assert_eq!(exported_subs[0]["items"][0]["product_id"], product_a);

    // 3. A user without a store: the seller export is the same
    // 404 the dashboard gives; their buyer export still works.
    let (status, body) = call(&app, Method::GET, "/v1/seller/export", None, Some(&storeless)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "storeless seller export: {body}");
    let (status, body) = call(&app, Method::GET, "/v1/buyer/export", None, Some(&storeless)).await;
    assert_eq!(status, StatusCode::OK, "storeless buyer export: {body}");
    assert_eq!(body["orders"].as_array().map(Vec::len), Some(0));

    // 4. Anonymous callers get 401 on both endpoints.
    let (status, _headers, _raw) =
        call_raw(&app, Method::GET, "/v1/seller/export", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous seller export");
    let (status, _headers, _raw) =
        call_raw(&app, Method::GET, "/v1/buyer/export", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous buyer export");
}
