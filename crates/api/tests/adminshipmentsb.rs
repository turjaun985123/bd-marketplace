//! Admin shipments remake, test B: the list's filters
//! are fail-closed (an unknown status / courier / queue /
//! sort is a 400 naming the allowed set, never a silently
//! empty page), the summary describes the WHOLE queue
//! even under a filter that matches nothing, pagination
//! walks the filtered set, and search follows the
//! platform's phone discipline — a phone matches exactly
//! (in any typed form, Bengali digits included) and is
//! never substring-searched, while courier references
//! (consignment ids, manual receipt numbers) match as
//! substrings because they are operational strings, not
//! identity credentials. Helpers are copied from the
//! Wave 9 / admin orders tests on purpose.
//!
//! Gated on `DATABASE_URL`: without it the test returns
//! early. CI provides a Postgres 16 service.

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

/// Checkout the buyer's current cart with COD; returns the order.
async fn checkout_cod(app: &Router, auth: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01888888888",
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

/// Set a sub-order's seller-side status (the Wave 9 walk).
async fn set_sub_order_status(app: &Router, auth: &str, sub_order_id: &str, status: &str) {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/status");
    let (code, body) = call(
        app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": status })),
        Some(auth),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "set sub-order status {status}: {body}");
}

/// Book a shipment for a confirmed sub-order; returns the shipment.
async fn book_shipment(app: &Router, auth: &str, sub_order_id: &str, body: Value) -> Value {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(body), Some(auth)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    shipment
}

/// GET the admin shipment list with a query string.
async fn list_shipments(app: &Router, auth: &str, qs: &str) -> (StatusCode, Value) {
    let uri = if qs.is_empty() {
        "/v1/admin/shipments".to_string()
    } else {
        format!("/v1/admin/shipments?{qs}")
    };
    call(app, Method::GET, &uri, None, Some(auth)).await
}

#[tokio::test]
async fn adminshipments_filters_search_and_summary() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin shipments remake test B");
            return;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, return_requests, store_follows, \
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
    let state = AppState::new(Some(pool.clone()), "adminshipmentsb-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111112").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111112'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // Two shipments for one buyer: a DEV steadfast booking
    // from store A and a manual Sundarban booking (receipt
    // SB-777) from store B.
    let seller_a = login(&app, "01722222223").await;
    let store_a = create_store(&app, &seller_a, "bship-alpha").await;
    let product_a = create_product(&app, &seller_a, &store_a, "Ship BA", 150_000, 10).await;
    let seller_b = login(&app, "01722222224").await;
    let store_b = create_store(&app, &seller_b, "bship-beta").await;
    let product_b = create_product(&app, &seller_b, &store_b, "Ship BB", 150_000, 10).await;

    let buyer = login(&app, "01999999998").await;
    add_to_cart(&app, &buyer, &product_a, 1).await;
    let order_a = checkout_cod(&app, &buyer).await;
    let sub_a = order_a["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order a")
        .to_string();
    set_sub_order_status(&app, &seller_a, &sub_a, "confirmed").await;
    let dev_shipment = book_shipment(&app, &seller_a, &sub_a, json!({})).await;
    let dev_id = dev_shipment["id"].as_str().expect("dev shipment id").to_string();

    add_to_cart(&app, &buyer, &product_b, 1).await;
    let order_b = checkout_cod(&app, &buyer).await;
    let sub_b = order_b["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order b")
        .to_string();
    set_sub_order_status(&app, &seller_b, &sub_b, "confirmed").await;
    let manual_shipment = book_shipment(
        &app,
        &seller_b,
        &sub_b,
        json!({ "courier": "manual", "manual_courier": "sundarban", "manual_tracking_id": "SB-777" }),
    )
    .await;
    let manual_id = manual_shipment["id"]
        .as_str()
        .expect("manual shipment id")
        .to_string();

    // Filters: status, courier, store.
    let (status, list) = list_shipments(&app, &admin, "status=created").await;
    assert_eq!(status, StatusCode::OK, "status filter: {list}");
    assert_eq!(list["total"], 2);
    let (status, list) = list_shipments(&app, &admin, "status=delivered").await;
    assert_eq!(status, StatusCode::OK, "delivered filter: {list}");
    assert_eq!(list["total"], 0);
    // ...and the summary under that empty filter still
    // describes the WHOLE queue (never filter-shaped).
    assert_eq!(list["summary"]["total"], 2, "summary is whole-queue: {list}");
    assert_eq!(list["summary"]["by_status"]["created"], 2);
    let (status, list) = list_shipments(&app, &admin, "courier=manual").await;
    assert_eq!(status, StatusCode::OK, "manual filter: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], manual_id);
    let (status, list) = list_shipments(&app, &admin, "courier=steadfast").await;
    assert_eq!(status, StatusCode::OK, "steadfast filter: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], dev_id);
    let store_qs = format!("store_id={store_a}");
    let (status, list) = list_shipments(&app, &admin, &store_qs).await;
    assert_eq!(status, StatusCode::OK, "store filter: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], dev_id);

    // Fail-closed: unknown filter values are 400s.
    for qs in [
        "status=bogus",
        "courier=dhl",
        "queue=everything",
        "sort=cheapest",
    ] {
        let (status, body) = list_shipments(&app, &admin, qs).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{qs}: {body}");
    }

    // Search: courier references match as substrings.
    let (status, list) = list_shipments(&app, &admin, "q=DEV-steadfast").await;
    assert_eq!(status, StatusCode::OK, "consignment search: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], dev_id);
    let (status, list) = list_shipments(&app, &admin, "q=SB-777").await;
    assert_eq!(status, StatusCode::OK, "receipt search: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], manual_id);
    // An id prefix finds its shipment.
    let prefix = &dev_id[..8];
    let prefix_qs = format!("q={prefix}");
    let (status, list) = list_shipments(&app, &admin, &prefix_qs).await;
    assert_eq!(status, StatusCode::OK, "id prefix search: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], dev_id);
    // The delivery phone matches EXACTLY — both orders —
    // in local form and in Bengali digits.
    let (status, list) = list_shipments(&app, &admin, "q=01888888888").await;
    assert_eq!(status, StatusCode::OK, "phone search: {list}");
    assert_eq!(list["total"], 2);
    let bengali_phone = "q=%E0%A7%A6%E0%A7%A7%E0%A7%AE%E0%A7%AE%E0%A7%AE%E0%A7%AE%E0%A7%AE%E0%A7%AE%E0%A7%AE%E0%A7%AE%E0%A7%AE";
    let (status, list) = list_shipments(&app, &admin, bengali_phone).await;
    assert_eq!(status, StatusCode::OK, "bengali phone search: {list}");
    assert_eq!(list["total"], 2, "Bengali-digit phone matches exactly: {list}");
    // A phone FRAGMENT matches nothing: phones are never
    // substring-searched.
    let (status, list) = list_shipments(&app, &admin, "q=888888").await;
    assert_eq!(status, StatusCode::OK, "phone fragment search: {list}");
    assert_eq!(list["total"], 0, "phone fragment must not match: {list}");
    // A bare % is a literal, not a wildcard.
    let (status, list) = list_shipments(&app, &admin, "q=%25").await;
    assert_eq!(status, StatusCode::OK, "percent search: {list}");
    assert_eq!(list["total"], 0, "escaped % matches nothing: {list}");

    // Pagination walks the filtered set.
    let (status, page1) = list_shipments(&app, &admin, "limit=1&page=1").await;
    assert_eq!(status, StatusCode::OK, "page 1: {page1}");
    assert_eq!(page1["total"], 2);
    assert_eq!(page1["items"].as_array().expect("page 1 items").len(), 1);
    let (status, page2) = list_shipments(&app, &admin, "limit=1&page=2").await;
    assert_eq!(status, StatusCode::OK, "page 2: {page2}");
    assert_eq!(page2["items"].as_array().expect("page 2 items").len(), 1);
    assert_ne!(
        page1["items"][0]["id"], page2["items"][0]["id"],
        "pages are disjoint"
    );

    // Sorts answer.
    let (status, list) = list_shipments(&app, &admin, "sort=cod_desc").await;
    assert_eq!(status, StatusCode::OK, "cod_desc sort: {list}");
    assert_eq!(list["total"], 2);
    let (status, list) = list_shipments(&app, &admin, "sort=stale_first").await;
    assert_eq!(status, StatusCode::OK, "stale_first sort: {list}");
    assert_eq!(list["total"], 2);
}
