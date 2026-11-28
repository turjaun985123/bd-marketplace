//! Admin shipments remake, test A: the admin shipment LIST
//! is a purpose-built triage row — the recipient phone is
//! MASKED and the full delivery block (phone, address,
//! note) and the buyer's account phone are GONE from it —
//! while the new DETAIL endpoint carries the full delivery
//! block (a stuck delivery cannot be worked without it),
//! the masked account phone, the raw event history and
//! the shared timeline. Gates: no token 401, non-admin
//! 403, and an API key 403 even when minted by the admin
//! (the blanket /v1/admin/ key exclusion). Helpers are
//! copied from the Wave 9 / admin orders tests on purpose
//! — integration test files stay self-contained.
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

/// Mint an API key with a JWT; returns the full key (shown once).
async fn create_key(app: &Router, jwt: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/api-keys",
        Some(json!({ "name": "shipments-test", "scopes": ["read"], "expires_in_days": null })),
        Some(jwt),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create api key: {body}");
    body["key"].as_str().expect("full key once").to_string()
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

#[tokio::test]
async fn adminshipments_gates_masking_and_detail() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin shipments remake test A");
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
    let state = AppState::new(Some(pool.clone()), "adminshipments-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "aship-alpha").await;
    let product = create_product(&app, &seller, &store, "Ship A", 150_000, 10).await;

    // One booked DEV shipment: goods 150_000 + inside-Dhaka
    // delivery 6_000, COD snapshot 156_000.
    let buyer = login(&app, "01999999999").await;
    add_to_cart(&app, &buyer, &product, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    set_sub_order_status(&app, &seller, &sub_order_id, "confirmed").await;
    let shipment = book_shipment(&app, &seller, &sub_order_id, json!({})).await;
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    assert_eq!(shipment["courier"], "steadfast");
    assert_eq!(shipment["cod_amount_poisha"], 156_000);

    // The surface is admin-only: no token 401, buyer 403 on
    // all three routes, and an API key 403 even when it
    // belongs to the admin (the blanket /v1/admin/ key
    // exclusion).
    let (status, body) = call(&app, Method::GET, "/v1/admin/shipments", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no token: {body}");
    let (status, body) = call(&app, Method::GET, "/v1/admin/shipments", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "buyer list: {body}");
    let detail_uri = format!("/v1/admin/shipments/{shipment_id}");
    let (status, body) = call(&app, Method::GET, &detail_uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "buyer detail: {body}");
    let refresh_uri = format!("/v1/admin/shipments/{shipment_id}/refresh");
    let (status, body) = call(
        &app,
        Method::POST,
        &refresh_uri,
        Some(json!({})),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "buyer refresh: {body}");
    let api_key = create_key(&app, &admin).await;
    let (status, body) = call(&app, Method::GET, "/v1/admin/shipments", None, Some(&api_key)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "api key list: {body}");
    let (status, body) = call(&app, Method::GET, &detail_uri, None, Some(&api_key)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "api key detail: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        &refresh_uri,
        Some(json!({})),
        Some(&api_key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "api key refresh: {body}");

    // The list: envelope, the triage row, masking.
    let (status, list) = call(&app, Method::GET, "/v1/admin/shipments", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin list: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["page"], 1);
    assert_eq!(list["limit"], 20);
    let row = &list["items"][0];
    assert_eq!(row["id"], shipment_id);
    assert_eq!(row["courier"], "steadfast");
    assert_eq!(row["status"], "created");
    assert_eq!(row["sub_order_status"], "shipped");
    assert_eq!(row["cod_amount_poisha"], 156_000);
    assert_eq!(row["delivery_name"], "Test Buyer");
    assert_eq!(row["delivery_phone_masked"], "018****888");
    assert_eq!(row["delivery_zone"], "inside_dhaka");
    assert_eq!(row["store_slug"], "aship-alpha");
    assert_eq!(row["unrefreshed"], false);
    assert_eq!(row["poll_overdue"], false);
    assert_eq!(row["aged"], false);
    assert_eq!(row["has_return_request"], false);
    assert!(row["last_polled_at"].is_null(), "never polled: {row}");
    assert!(
        row["consignment_id"]
            .as_str()
            .expect("consignment")
            .starts_with("DEV-steadfast-"),
        "dev consignment: {row}"
    );
    // The heavy / identifying fields are GONE from the row.
    assert!(row.get("delivery_phone").is_none(), "row: {row}");
    assert!(row.get("delivery_address").is_none(), "row: {row}");
    assert!(row.get("order_note").is_none(), "row: {row}");
    assert!(row.get("buyer_phone_masked").is_none(), "row: {row}");
    let raw = list.to_string();
    assert!(!raw.contains("01888888888"), "full delivery phone leaked: {raw}");
    assert!(!raw.contains("+8801888888888"), "canonical phone leaked: {raw}");
    assert!(!raw.contains("House 9"), "delivery address leaked: {raw}");
    assert!(!raw.contains("01999999999"), "buyer account phone leaked: {raw}");

    // The summary describes the whole queue.
    let summary = &list["summary"];
    assert_eq!(summary["total"], 1);
    assert_eq!(summary["non_terminal"], 1);
    assert_eq!(summary["by_status"]["created"], 1);
    assert_eq!(summary["by_status"]["delivered"], 0);
    assert_eq!(summary["unrefreshed"], 0);
    assert_eq!(summary["poll_overdue"], 0);
    assert_eq!(summary["status_aged"], 0);
    assert_eq!(summary["cod_created_poisha"], 156_000);
    assert_eq!(summary["cod_delivered_held_poisha"], 0);
    assert_eq!(summary["couriers_dev_mode"], true);
    assert_eq!(summary["aged_thresholds"]["created"], 24);
    let couriers = summary["couriers"].as_array().expect("courier rollup");
    assert_eq!(couriers.len(), 9, "all nine couriers appear: {summary}");
    let steadfast = couriers
        .iter()
        .find(|c| c["courier"] == "steadfast")
        .expect("steadfast rollup row");
    assert_eq!(steadfast["total"], 1);
    assert_eq!(steadfast["created"], 1);
    assert_eq!(steadfast["dev_shipment_count"], 1);
    assert_eq!(steadfast["mode"], "dev");

    // The detail: full delivery block lives HERE only,
    // plus events, items, the shared timeline and the
    // masked account phone.
    let (status, detail) = call(&app, Method::GET, &detail_uri, None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin detail: {detail}");
    assert_eq!(detail["id"], shipment_id);
    assert_eq!(detail["delivery_phone"], "+8801888888888");
    assert_eq!(detail["delivery_address"], "House 9, Road 3, Khulna");
    assert_eq!(detail["buyer_phone_masked"], "019****999");
    assert_eq!(detail["cod_amount_poisha"], 156_000);
    assert_eq!(detail["goods_subtotal_poisha"], 150_000);
    assert_eq!(detail["grand_total_poisha"], 156_000);
    assert_eq!(detail["payment_status"], "pay_on_delivery");
    assert_eq!(detail["advance_stranded"], false);
    assert!(detail["return_request"].is_null(), "no return: {detail}");
    assert!(detail["last_polled_at"].is_null(), "never polled: {detail}");
    let events = detail["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1, "one create event: {detail}");
    assert_eq!(events[0]["status"], "created");
    assert_eq!(events[0]["source"], "create");
    assert!(events[0]["raw_status"].is_null(), "create has no raw: {detail}");
    let items = detail["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title_snapshot"], "Ship A");
    assert!(detail["timeline"].is_array(), "timeline: {detail}");
    assert!(detail["steps"].is_array(), "steps: {detail}");
    assert_eq!(
        detail["audit_entries"].as_array().expect("audit entries").len(),
        0,
        "no admin actions yet: {detail}"
    );

    // An unknown shipment id 404s on detail and refresh.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/shipments/00000000-0000-0000-0000-000000000000",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown detail: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/admin/shipments/00000000-0000-0000-0000-000000000000/refresh",
        Some(json!({})),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown refresh: {body}");
}
