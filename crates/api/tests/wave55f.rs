//! Wave 55f integration test: the Wave 55 extension
//! surfaces — the API-computed tracking STEPS on the buyer
//! order view (placed order: first step current; delivered
//! order: every step done), the seller sub-order list
//! carrying the same timeline + steps, the per-store
//! invoice showing the order's district + note, the buyer
//! data export's new `addresses` section, the admin
//! metrics' district/zone/address-book blocks, and the
//! single-address GET with its ownership 404.
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

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55f integration tests");
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
    let state = AppState::new(Some(pool.clone()), "wave55f-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// The (kind, state) pairs of one steps array.
fn step_pairs(steps: &Value) -> Vec<(String, String)> {
    steps
        .as_array()
        .expect("steps array")
        .iter()
        .map(|step| {
            (
                step["kind"].as_str().expect("step kind").to_string(),
                step["state"].as_str().expect("step state").to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn wave55f_steps_seller_timeline_invoice_export_metrics_flow() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01777777777").await;
    let store = create_store(&app, &seller, "wave55f-store").await;
    let product = create_product(&app, &seller, &store, "Wave55F Product", 50_000, 5).await;
    let buyer = login(&app, "01977777777").await;
    let stranger = login(&app, "01988888888").await;
    // The admin: login FIRST (creates the user row), then
    // the SQL promotion lands.
    let admin = login(&app, "01900000000").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801900000000'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // The buyer's address (Dhaka) + the single-address GET.
    let (status, address) = call(
        &app,
        Method::POST,
        "/v1/addresses",
        Some(json!({
            "label": "বাসা",
            "recipient_name": "Test Buyer",
            "phone": "01712345678",
            "district": "Dhaka",
            "area": "Dhanmondi",
            "address_line": "Road 8, House 3"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create address: {address}");
    let address_id = address["id"].as_str().expect("address id").to_string();
    let uri = format!("/v1/addresses/{address_id}");
    let (status, fetched) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "get own address: {fetched}");
    assert_eq!(fetched["id"], address_id);
    assert_eq!(fetched["zone"], "inside_dhaka");
    let (status, refused) = call(&app, Method::GET, &uri, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger get: {refused}");

    // Order with the address + a note, then drive it to
    // delivered through the DEV courier (the Wave 9 walk).
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Ignored",
            "delivery_phone": "01999999999",
            "delivery_address": "Ignored",
            "delivery_zone": "outside_dhaka",
            "payment_method": "cod",
            "address_id": address_id,
            "order_note": "গেটের সামনে রাখুন"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout: {order}");
    let order_id = order["id"].as_str().expect("order id").to_string();
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();

    // 1. Steps on a PLACED order: placed is current, the
    // rest upcoming — the domain derivation, served.
    let uri = format!("/v1/orders/{order_id}");
    let (status, fetched) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer order: {fetched}");
    assert_eq!(
        step_pairs(&fetched["sub_orders"][0]["steps"]),
        vec![
            ("placed".to_string(), "current".to_string()),
            ("confirmed".to_string(), "upcoming".to_string()),
            ("shipped".to_string(), "upcoming".to_string()),
            ("delivered".to_string(), "upcoming".to_string()),
        ]
    );

    // Drive to delivered.
    let (status, body) = set_sub_order_status(&app, &seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(&app, Method::POST, &uri, Some(json!({})), Some(&seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }

    // 2. Steps on the DELIVERED order: all done.
    let uri = format!("/v1/orders/{order_id}");
    let (status, fetched) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        step_pairs(&fetched["sub_orders"][0]["steps"]),
        vec![
            ("placed".to_string(), "done".to_string()),
            ("confirmed".to_string(), "done".to_string()),
            ("shipped".to_string(), "done".to_string()),
            ("delivered".to_string(), "done".to_string()),
        ]
    );

    // 3. The seller list carries the same timeline + steps.
    let (status, seller_list) = call(
        &app,
        Method::GET,
        "/v1/seller/sub-orders?limit=10",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller list: {seller_list}");
    let row = &seller_list["items"][0];
    assert_eq!(row["id"], sub_order_id);
    let seller_kinds: Vec<String> = row["timeline"]
        .as_array()
        .expect("seller timeline")
        .iter()
        .map(|event| event["kind"].as_str().expect("kind").to_string())
        .collect();
    assert!(seller_kinds.contains(&"shipment_booked".to_string()));
    assert!(seller_kinds.contains(&"delivered".to_string()));
    assert_eq!(
        step_pairs(&row["steps"]).last().map(|pair| pair.1.clone()),
        Some("done".to_string())
    );

    // 4. The invoice (issued on first read for a delivered
    // sub-order) shows the district + the note.
    let uri = format!("/v1/buyer/sub-orders/{sub_order_id}/invoice");
    let (status, invoice) = call(&app, Method::GET, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "invoice: {invoice}");
    assert_eq!(invoice["buyer"]["district"], "Dhaka");
    assert_eq!(invoice["order_note"], "গেটের সামনে রাখুন");

    // 5. The buyer export carries the address book.
    let (status, export) = call(&app, Method::GET, "/v1/buyer/export", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "export: {export}");
    let exported = export["addresses"].as_array().expect("exported addresses");
    assert_eq!(exported.len(), 1);
    assert_eq!(exported[0]["label"], "বাসা");
    assert_eq!(exported[0]["district"], "Dhaka");

    // 6. Admin metrics: the district block counts this
    // order, the zone block counts it under inside_dhaka,
    // and the address-book block sees the one address.
    let (status, metrics) = call(&app, Method::GET, "/v1/admin/metrics", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "metrics: {metrics}");
    let dhaka = metrics["orders_by_district"]
        .as_array()
        .expect("district block")
        .iter()
        .find(|row| row["district"] == "Dhaka")
        .expect("Dhaka counted");
    assert_eq!(dhaka["orders"], 1);
    let inside = metrics["orders_by_zone"]
        .as_array()
        .expect("zone block")
        .iter()
        .find(|row| row["zone"] == "inside_dhaka")
        .expect("inside_dhaka counted");
    assert_eq!(inside["orders"], 1);
    assert_eq!(metrics["address_book"]["total_addresses"], 1);
    assert_eq!(metrics["address_book"]["buyers_with_addresses"], 1);
    assert_eq!(metrics["address_book"]["defaults_count"], 1);
}
