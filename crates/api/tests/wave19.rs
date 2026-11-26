//! Wave 19 integration test: manual couriers (Sundarban, SA
//! Paribahan, Karatoa, Carrybee) as a seller-driven workflow —
//! booking with `courier = "manual"` writes the shipment row
//! directly (no adapter), statuses are seller-entered against
//! the domain transition table and sync the sub-order (delivery
//! earns; a return restocks), the counter receipt number is
//! recorded at booking or later, manual entries appear in the
//! courier list and estimates (unpriced, never routed to), and
//! manual operations on an API-courier shipment are a 409. The
//! small helpers are copied from the Wave 9 test on purpose,
//! since integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a
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

/// Buy one unit and confirm the sub-order; returns (order, sub-order id).
async fn buy_and_confirm(
    app: &Router,
    buyer: &str,
    seller: &str,
    product_id: &str,
) -> (Value, String) {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    (order, sub_order_id)
}

/// Book a shipment for a sub-order with an explicit body.
async fn book_shipment(
    app: &Router,
    seller: &str,
    sub_order_id: &str,
    body: Value,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    call(app, Method::POST, &uri, Some(body), Some(seller)).await
}

/// Record one seller-entered status for a manual shipment.
async fn set_manual_status(
    app: &Router,
    token: &str,
    shipment_id: &str,
    status: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/shipments/{shipment_id}/manual-status");
    call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "status": status })),
        Some(token),
    )
    .await
}

/// Record the counter receipt number for a manual shipment.
async fn set_manual_tracking(
    app: &Router,
    token: &str,
    shipment_id: &str,
    tracking_id: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/shipments/{shipment_id}/manual-tracking");
    call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "tracking_id": tracking_id })),
        Some(token),
    )
    .await
}

/// The seller's current view of one sub-order's status.
async fn sub_order_status(app: &Router, seller: &str, sub_order_id: &str) -> String {
    let (status, list) = call(
        app,
        Method::GET,
        "/v1/seller/sub-orders?limit=50",
        None,
        Some(seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "sub-order list: {list}");
    list["items"]
        .as_array()
        .expect("sub-order items")
        .iter()
        .find(|sub| sub["id"] == sub_order_id)
        .expect("sub-order in list")["status"]
        .as_str()
        .expect("sub-order status")
        .to_string()
}

/// The product's stock, read straight from the database.
async fn product_stock(pool: &PgPool, product_id: &str) -> i64 {
    let uuid = uuid::Uuid::parse_str(product_id).expect("product id is a uuid");
    sqlx::query_scalar("SELECT stock FROM products WHERE id = $1")
        .bind(uuid)
        .fetch_one(pool)
        .await
        .expect("product stock")
}

#[tokio::test]
async fn wave19_manual_couriers_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 19 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE return_requests, payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, \
         order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode on, so the
    // API-courier legs (auto routing, the manual-op 409s) have a
    // working DEV courier to book with.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave19-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave19-store").await;
    // 200_000 poisha of goods: 8% commission is 16_000, so a
    // delivered unit earns exactly 184_000; inside Dhaka at the
    // default 1 kg the delivery charge is 6_000, so a COD parcel
    // collects 206_000.
    let product = create_product(&app, &seller, &store, "Wave19 Product", 200_000, 10).await;
    let other_seller = login(&app, "01599999999").await;
    create_store(&app, &other_seller, "wave19-other-store").await;
    let buyer = login(&app, "01999999999").await;

    // 1. The courier list carries the four manual couriers,
    // always available, mode `manual`, with display names.
    let (status, couriers) = call(&app, Method::GET, "/v1/couriers", None, None).await;
    assert_eq!(status, StatusCode::OK, "couriers: {couriers}");
    let manual_entries: Vec<&Value> = couriers
        .as_array()
        .expect("courier list")
        .iter()
        .filter(|entry| entry["mode"] == "manual")
        .collect();
    assert_eq!(manual_entries.len(), 4, "four manual entries: {couriers}");
    for (key, display) in [
        ("sundarban", "Sundarban Courier"),
        ("sa_paribahan", "SA Paribahan"),
        ("karatoa", "Karatoa Courier"),
        ("carrybee", "Carrybee"),
    ] {
        let entry = manual_entries
            .iter()
            .find(|entry| entry["courier"] == key)
            .expect("manual entry present");
        assert_eq!(entry["available"], true, "manual available: {entry}");
        assert_eq!(entry["display_name"], display, "display name: {entry}");
    }

    // 2. Estimates list the manual couriers unpriced with a note,
    // and the recommendation is never a manual courier.
    let (status, estimates) = call(
        &app,
        Method::GET,
        "/v1/couriers/estimate?zone=inside_dhaka&weight_grams=1000",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "estimates: {estimates}");
    let manual_estimates: Vec<&Value> = estimates["estimates"]
        .as_array()
        .expect("estimate list")
        .iter()
        .filter(|entry| entry["mode"] == "manual")
        .collect();
    assert_eq!(manual_estimates.len(), 4, "four manual estimates: {estimates}");
    for entry in &manual_estimates {
        assert!(entry["price_poisha"].is_null(), "manual unpriced: {entry}");
        assert!(entry["note"].is_string(), "manual note: {entry}");
    }
    let recommended = estimates["recommended"]
        .as_str()
        .expect("a recommendation exists in dev mode");
    assert!(
        !["sundarban", "sa_paribahan", "karatoa", "carrybee"].contains(&recommended),
        "auto never recommends a manual courier: {recommended}"
    );

    // 3. Order 1: book Sundarban manually, receipt number in
    // hand. The key is case-insensitive and canonicalized.
    let (order1, sub1) = buy_and_confirm(&app, &buyer, &seller, &product).await;
    let order1_id = order1["id"].as_str().expect("order id").to_string();
    let (status, shipment) = book_shipment(
        &app,
        &seller,
        &sub1,
        json!({
            "courier": "manual",
            "manual_courier": "Sundarban",
            "manual_tracking_id": "SB-12345"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "manual booking: {shipment}");
    assert_eq!(shipment["courier"], "manual");
    assert_eq!(shipment["manual_courier_name"], "Sundarban Courier");
    assert_eq!(shipment["manual_tracking_id"], "SB-12345");
    assert_eq!(shipment["consignment_id"], "SB-12345");
    assert_eq!(shipment["status"], "created");
    assert_eq!(shipment["cod_amount_poisha"], 206_000);
    let shipment1_id = shipment["id"].as_str().expect("shipment id").to_string();
    assert_eq!(sub_order_status(&app, &seller, &sub1).await, "shipped");

    // A second booking for the same sub-order is a 409.
    let (status, body) = book_shipment(
        &app,
        &seller,
        &sub1,
        json!({ "courier": "manual", "manual_courier": "karatoa" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "duplicate booking: {body}");

    // 4. The transition table binds the seller too: created
    // cannot jump to delivered, and a non-status is a 400. A
    // stranger seller cannot move the shipment at all.
    let (status, body) = set_manual_status(&app, &seller, &shipment1_id, "delivered").await;
    assert_eq!(status, StatusCode::CONFLICT, "created -> delivered: {body}");
    let (status, body) = set_manual_status(&app, &seller, &shipment1_id, "bogus").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bogus status: {body}");
    let (status, body) = set_manual_status(&app, &other_seller, &shipment1_id, "picked_up").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "foreign seller: {body}");

    // 5. Walk the parcel to delivered; the sub-order follows and
    // the delivery earns exactly like a courier-reported one.
    for next in ["picked_up", "in_transit", "out_for_delivery", "delivered"] {
        let (status, updated) = set_manual_status(&app, &seller, &shipment1_id, next).await;
        assert_eq!(status, StatusCode::OK, "manual status {next}: {updated}");
        assert_eq!(updated["status"], next);
    }
    assert_eq!(sub_order_status(&app, &seller, &sub1).await, "delivered");
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 184_000, "balance: {balance}");

    // 6. Public tracking shows the manual courier's name and the
    // receipt number.
    let track_uri = format!("/v1/track?order_id={order1_id}&phone=01999999999");
    let (status, tracked) = call(&app, Method::GET, &track_uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "track: {tracked}");
    let tracked_shipment = &tracked["sub_orders"][0]["shipment"];
    assert_eq!(tracked_shipment["courier"], "manual");
    assert_eq!(tracked_shipment["manual_courier_name"], "Sundarban Courier");
    assert_eq!(tracked_shipment["manual_tracking_id"], "SB-12345");
    assert_eq!(tracked_shipment["status"], "delivered");

    // 7. Order 2: the invalid manual bookings are 400s that
    // change nothing — an API courier is not a manual courier, a
    // missing key is a 400, and a too-short receipt number is a
    // 400. The sub-order stays confirmed and bookable.
    let (_order2, sub2) = buy_and_confirm(&app, &buyer, &seller, &product).await;
    let (status, body) = book_shipment(
        &app,
        &seller,
        &sub2,
        json!({ "courier": "manual", "manual_courier": "pathao" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "api courier as manual: {body}");
    let (status, body) = book_shipment(&app, &seller, &sub2, json!({ "courier": "manual" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "missing manual_courier: {body}");
    let (status, body) = book_shipment(
        &app,
        &seller,
        &sub2,
        json!({
            "courier": "manual",
            "manual_courier": "karatoa",
            "manual_tracking_id": "ab"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "short tracking id: {body}");
    assert_eq!(sub_order_status(&app, &seller, &sub2).await, "confirmed");

    // Book Karatoa WITHOUT a receipt number: the consignment id
    // is the generated placeholder, visibly not a real number.
    let (status, shipment) = book_shipment(
        &app,
        &seller,
        &sub2,
        json!({ "courier": "manual", "manual_courier": "karatoa" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "manual booking 2: {shipment}");
    assert_eq!(shipment["manual_courier_name"], "Karatoa Courier");
    assert!(shipment["manual_tracking_id"].is_null(), "no receipt yet: {shipment}");
    assert!(
        shipment["consignment_id"]
            .as_str()
            .expect("consignment id")
            .starts_with("MANUAL-"),
        "placeholder consignment: {shipment}"
    );
    let shipment2_id = shipment["id"].as_str().expect("shipment id").to_string();

    // Recording the receipt number later replaces the
    // placeholder in both fields; a too-short one is a 400.
    let (status, body) = set_manual_tracking(&app, &seller, &shipment2_id, "x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "short receipt update: {body}");
    let (status, updated) = set_manual_tracking(&app, &seller, &shipment2_id, "KT-9999").await;
    assert_eq!(status, StatusCode::OK, "receipt update: {updated}");
    assert_eq!(updated["manual_tracking_id"], "KT-9999");
    assert_eq!(updated["consignment_id"], "KT-9999");

    // 8. The return path: out_for_delivery -> partial_delivered
    // -> returned syncs the sub-order to returned and restocks.
    assert_eq!(product_stock(&pool, &product).await, 8, "two units sold");
    for next in ["picked_up", "in_transit", "out_for_delivery", "partial_delivered"] {
        let (status, updated) = set_manual_status(&app, &seller, &shipment2_id, next).await;
        assert_eq!(status, StatusCode::OK, "manual status {next}: {updated}");
    }
    let (status, updated) = set_manual_status(&app, &seller, &shipment2_id, "returned").await;
    assert_eq!(status, StatusCode::OK, "manual return: {updated}");
    assert_eq!(sub_order_status(&app, &seller, &sub2).await, "returned");
    assert_eq!(product_stock(&pool, &product).await, 9, "return restocked");

    // 9. Manual operations on an API (DEV) shipment are 409s.
    let (_order3, sub3) = buy_and_confirm(&app, &buyer, &seller, &product).await;
    let (status, api_shipment) = book_shipment(&app, &seller, &sub3, json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "dev booking: {api_shipment}");
    assert_ne!(api_shipment["courier"], "manual");
    assert!(api_shipment["manual_courier_name"].is_null());
    let api_shipment_id = api_shipment["id"].as_str().expect("shipment id").to_string();
    let (status, body) = set_manual_status(&app, &seller, &api_shipment_id, "picked_up").await;
    assert_eq!(status, StatusCode::CONFLICT, "manual status on api shipment: {body}");
    let (status, body) = set_manual_tracking(&app, &seller, &api_shipment_id, "XX-1234").await;
    assert_eq!(status, StatusCode::CONFLICT, "manual tracking on api shipment: {body}");

    // Refreshing a MANUAL shipment is an honest no-op (there is
    // no API to poll), not an error and not an invented update.
    let uri = format!("/v1/shipments/{shipment2_id}/refresh");
    let (status, refreshed) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "manual refresh: {refreshed}");
    assert_eq!(refreshed["status"], "returned");

    // 10. `auto` still resolves to an API courier, never manual.
    let (_order4, sub4) = buy_and_confirm(&app, &buyer, &seller, &product).await;
    let (status, auto_shipment) =
        book_shipment(&app, &seller, &sub4, json!({ "courier": "auto" })).await;
    assert_eq!(status, StatusCode::CREATED, "auto booking: {auto_shipment}");
    assert_ne!(auto_shipment["courier"], "manual");
    assert!(auto_shipment["manual_courier_name"].is_null());
}
