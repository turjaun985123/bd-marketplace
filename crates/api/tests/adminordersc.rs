//! Admin orders remake, test C: SEARCH and SORT and
//! fail-closed validation. Search follows the platform's
//! phone discipline — a phone in any typed form (local,
//! canonical, Bengali digits) matches the delivery or
//! account phone EXACTLY and never by substring; an
//! order-id prefix matches by prefix; other text matches
//! the delivery name or a store name/slug with LIKE
//! wildcards escaped (a bare `%` finds nothing). Unknown
//! filter and sort values are 400s, never empty pages.
//! Helpers are copied from the Wave 9 test on purpose —
//! integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test returns
//! early, so a plain local `cargo test` (no database)
//! still passes. CI provides a Postgres 16 service and
//! sets `DATABASE_URL`.

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
    auth: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = auth {
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
async fn create_store(app: &Router, auth: &str, slug: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body["id"].as_str().expect("store id").to_string()
}

/// Create an active product; returns the product id.
async fn create_product(
    app: &Router,
    auth: &str,
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
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Add a product to the buyer's cart.
async fn add_to_cart(app: &Router, auth: &str, product_id: &str, quantity: i64) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_id, "quantity": quantity })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {body}");
}

/// Checkout the buyer's current cart with COD under a
/// given delivery name and phone; returns the order.
async fn checkout_cod(app: &Router, auth: &str, name: &str, phone: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": name,
            "delivery_phone": phone,
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(auth),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {body}");
    body
}

/// GET the admin order list with a query string; asserts 200.
async fn list_orders(app: &Router, admin: &str, qs: &str) -> Value {
    let uri = format!("/v1/admin/orders?{qs}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(admin)).await;
    assert_eq!(status, StatusCode::OK, "list {qs}: {body}");
    body
}

#[tokio::test]
async fn adminorders_search_sort_and_validation() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin orders remake test C");
            return;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, return_requests, store_follows, \
         store_verification_events, store_verifications, platform_settings, \
         payouts, payout_accounts, review_replies, reviews, shipment_events, \
         shipments, payment_events, payments, order_items, sub_orders, orders, \
         cart_items, carts, products, api_keys, store_members, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "adminordersc-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller_a = login(&app, "01722222222").await;
    let store_a = create_store(&app, &seller_a, "aordersc-alpha").await;
    let product_a = create_product(&app, &seller_a, &store_a, "OrdersC A", 100_000, 10).await;
    let seller_b = login(&app, "01733333333").await;
    let store_b = create_store(&app, &seller_b, "aordersc-beta").await;
    let product_b = create_product(&app, &seller_b, &store_b, "OrdersC B", 150_000, 10).await;

    // Order A: account phone 01911111111, delivery phone
    // 01811111111, name "Rahim Uddin", grand 106_000.
    let buyer_a = login(&app, "01911111111").await;
    add_to_cart(&app, &buyer_a, &product_a, 1).await;
    let order_a = checkout_cod(&app, &buyer_a, "Rahim Uddin", "01811111111").await;
    let order_a_id = order_a["id"].as_str().expect("order id").to_string();

    // Order B: another buyer, store beta, grand 156_000.
    let buyer_b = login(&app, "01922222222").await;
    add_to_cart(&app, &buyer_b, &product_b, 1).await;
    let order_b = checkout_cod(&app, &buyer_b, "Karim Sheikh", "01822222222").await;
    let order_b_id = order_b["id"].as_str().expect("order id").to_string();

    // Phone search: exact in every typed form.
    let by_local = list_orders(&app, &admin, "q=01811111111").await;
    assert_eq!(by_local["total"], 1);
    assert_eq!(by_local["items"][0]["id"], order_a_id);
    let by_canonical = list_orders(&app, &admin, "q=%2B8801811111111").await;
    assert_eq!(by_canonical["total"], 1);
    assert_eq!(by_canonical["items"][0]["id"], order_a_id);
    // The ACCOUNT phone finds the order too (users join).
    let by_account = list_orders(&app, &admin, "q=01911111111").await;
    assert_eq!(by_account["total"], 1);
    assert_eq!(by_account["items"][0]["id"], order_a_id);
    // Bengali digits transliterate before matching.
    let bengali = "০১৮১১১১১১১১";
    let by_bengali = list_orders(
        &app,
        &admin,
        &format!("q={}", urlencode(bengali)),
    )
    .await;
    assert_eq!(by_bengali["total"], 1);
    assert_eq!(by_bengali["items"][0]["id"], order_a_id);
    // A phone SUBSTRING matches nothing — phones are
    // never substring-searched.
    let by_substring = list_orders(&app, &admin, "q=8111111").await;
    assert_eq!(by_substring["total"], 0);

    // Order-id prefix search.
    let prefix = &order_a_id[..8];
    let by_prefix = list_orders(&app, &admin, &format!("q={prefix}")).await;
    assert_eq!(by_prefix["total"], 1);
    assert_eq!(by_prefix["items"][0]["id"], order_a_id);

    // Name and store search (LIKE-escaped substrings).
    let by_name = list_orders(&app, &admin, "q=Rahim").await;
    assert_eq!(by_name["total"], 1);
    assert_eq!(by_name["items"][0]["id"], order_a_id);
    let by_store = list_orders(&app, &admin, "q=aordersc-beta").await;
    assert_eq!(by_store["total"], 1);
    assert_eq!(by_store["items"][0]["id"], order_b_id);
    // A bare % is escaped: it matches only a literal %
    // (present in no name), never everything.
    let by_wildcard = list_orders(&app, &admin, "q=%25").await;
    assert_eq!(by_wildcard["total"], 0);

    // Sorts: default newest first (B after A), oldest
    // reverses, amount orders follow the grand totals.
    let newest = list_orders(&app, &admin, "").await;
    assert_eq!(newest["items"][0]["id"], order_b_id);
    assert_eq!(newest["items"][1]["id"], order_a_id);
    let oldest = list_orders(&app, &admin, "sort=oldest").await;
    assert_eq!(oldest["items"][0]["id"], order_a_id);
    let amount_asc = list_orders(&app, &admin, "sort=amount_asc").await;
    assert_eq!(amount_asc["items"][0]["id"], order_a_id);
    let amount_desc = list_orders(&app, &admin, "sort=amount_desc").await;
    assert_eq!(amount_desc["items"][0]["id"], order_b_id);

    // Fail-closed validation: unknown values are 400s
    // with messages naming the allowed set.
    for (qs, needle) in [
        ("status=shipped", "status must be one of: placed, cancelled"),
        ("payment_status=bogus", "payment_status must be one of"),
        ("sub_status=bogus", "sub_status must be one of"),
        ("advance_state=bogus", "advance_state must be one of"),
        (
            "sort=bogus",
            "sort must be one of: newest, oldest, amount_desc, amount_asc",
        ),
    ] {
        let uri = format!("/v1/admin/orders?{qs}");
        let (status, body) = call(&app, Method::GET, &uri, None, Some(&admin)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{qs}: {body}");
        let message = body["error"].as_str().unwrap_or("");
        assert!(
            message.contains(needle),
            "{qs} error should name the set: {body}"
        );
    }
}

/// Minimal percent-encoding for the Bengali-digit query
/// (the test client builds raw URIs).
fn urlencode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        out.push_str(&format!("%{byte:02X}"));
    }
    out
}
