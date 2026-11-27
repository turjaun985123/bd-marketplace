//! Wave 55b integration test: checkout's three Wave 55
//! paths — (1) `address_id`: the order's delivery fields,
//! zone and district come from the SAVED ADDRESS while the
//! client-sent fields disagree on purpose (the server is
//! the source of truth), the address's `last_used_at` is
//! stamped, and another buyer's address id is a 404 that
//! creates NO order; (2) the manual `delivery_district`:
//! the zone DERIVES from the district even when the sent
//! zone disagrees, and an unknown district is a 400;
//! (3) the order note: stored on the order and visible on
//! the buyer's order, the seller's sub-order list and the
//! fulfillment queue — and a 501-char note is a 400.
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

/// POST /v1/orders with an explicit body; returns (status, body).
async fn checkout(app: &Router, token: &str, body: Value) -> (StatusCode, Value) {
    call(app, Method::POST, "/v1/orders", Some(body), Some(token)).await
}

/// The manual checkout fields every scenario starts from;
/// scenarios override the keys they exercise.
fn manual_body() -> Value {
    json!({
        "delivery_name": "Manual Name",
        "delivery_phone": "01999999999",
        "delivery_address": "House 9, Road 3",
        "delivery_zone": "inside_dhaka",
        "payment_method": "cod"
    })
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55b integration tests");
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
    let state = AppState::new(Some(pool.clone()), "wave55b-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

#[tokio::test]
async fn wave55b_checkout_address_district_and_note_flow() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "wave55b-store").await;
    let product = create_product(&app, &seller, &store, "Wave55B Product", 50_000, 40).await;
    let buyer = login(&app, "01911111111").await;
    let stranger = login(&app, "01922222222").await;

    // The buyer's saved address: Gazipur (suburb zone),
    // its own name/phone — deliberately different from
    // every manual field sent below.
    let (status, address) = call(
        &app,
        Method::POST,
        "/v1/addresses",
        Some(json!({
            "label": "অফিস",
            "recipient_name": "Address Name",
            "phone": "01712345678",
            "district": "Gazipur",
            "area": "Uttara",
            "address_line": "House 5"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create address: {address}");
    let address_id = address["id"].as_str().expect("address id").to_string();

    // 1. Checkout with address_id and DISAGREEING body
    // fields: the order carries the address's fields, the
    // suburb zone (and its charge for the 2 kg parcel —
    // 10_000 first kg + 3_000 extra — not the 8_000 an
    // inside-Dhaka 2 kg parcel would cost), and the
    // district.
    add_to_cart(&app, &buyer, &product, 2).await;
    let mut body = manual_body();
    body["address_id"] = json!(address_id);
    let (status, order) = checkout(&app, &buyer, body).await;
    assert_eq!(status, StatusCode::CREATED, "address checkout: {order}");
    assert_eq!(order["delivery_name"], "Address Name");
    assert_eq!(order["delivery_phone"], "+8801712345678");
    assert_eq!(order["delivery_address"], "House 5, Uttara, Gazipur");
    assert_eq!(order["delivery_zone"], "dhaka_suburb");
    assert_eq!(order["delivery_district"], "Gazipur");
    assert_eq!(
        order["delivery_total_poisha"], 13_000,
        "the suburb charge proves the zone came from the address"
    );
    assert!(order["order_note"].is_null(), "no note was written");

    // 2. The checkout stamped the address's last_used_at.
    let (status, book) = call(&app, Method::GET, "/v1/addresses", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !book["items"][0]["last_used_at"].is_null(),
        "checkout stamps last_used_at: {book}"
    );

    // 3. Another buyer's address id: 404, NO order, and
    // the stranger's cart survives untouched.
    add_to_cart(&app, &stranger, &product, 1).await;
    let mut body = manual_body();
    body["address_id"] = json!(address_id);
    let (status, refused) = checkout(&app, &stranger, body).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign address: {refused}");
    let (status, orders) = call(&app, Method::GET, "/v1/orders", None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(orders["total"], 0, "no order was created");
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        cart["items"].as_array().expect("cart items").len(),
        1,
        "the stranger's cart is intact"
    );

    // 4. Manual district: Tangail sent with a (wrong)
    // inside_dhaka zone — the order lands outside_dhaka
    // with the outside 12_000 charge and stores Tangail.
    add_to_cart(&app, &buyer, &product, 1).await;
    let mut body = manual_body();
    body["delivery_district"] = json!("Tangail");
    let (status, order) = checkout(&app, &buyer, body).await;
    assert_eq!(status, StatusCode::CREATED, "district checkout: {order}");
    assert_eq!(order["delivery_zone"], "outside_dhaka");
    assert_eq!(order["delivery_district"], "Tangail");
    assert_eq!(order["delivery_total_poisha"], 12_000);

    // 5. An unknown district is a 400 and creates nothing.
    add_to_cart(&app, &buyer, &product, 1).await;
    let mut body = manual_body();
    body["delivery_district"] = json!("Atlantis");
    let (status, refused) = checkout(&app, &buyer, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown district: {refused}");
    let (status, orders) = call(&app, Method::GET, "/v1/orders", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(orders["total"], 2, "still exactly the two real orders");

    // 6. The order note round-trips: buyer order, seller
    // sub-order list, fulfillment queue. (The cart still
    // holds the unit from step 5's refused checkout.)
    let mut body = manual_body();
    body["order_note"] = json!("সন্ধ্যায় ডেলিভারি দিন");
    let (status, order) = checkout(&app, &buyer, body).await;
    assert_eq!(status, StatusCode::CREATED, "note checkout: {order}");
    assert_eq!(order["order_note"], "সন্ধ্যায় ডেলিভারি দিন");
    let order_id = order["id"].as_str().expect("order id").to_string();

    let (status, seller_list) = call(
        &app,
        Method::GET,
        "/v1/seller/sub-orders?limit=50",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller list: {seller_list}");
    let noted = seller_list["items"]
        .as_array()
        .expect("seller items")
        .iter()
        .find(|row| row["order_id"] == order_id)
        .expect("the noted sub-order is listed");
    assert_eq!(noted["order_note"], "সন্ধ্যায় ডেলিভারি দিন");

    let uri = format!("/v1/seller/fulfillment?store_id={store}");
    let (status, queue) = call(&app, Method::GET, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "fulfillment queue: {queue}");
    let queued = queue["items"]
        .as_array()
        .expect("queue items")
        .iter()
        .find(|row| row["order_id"] == order_id)
        .expect("the noted sub-order is queued");
    assert_eq!(queued["order_note"], "সন্ধ্যায় ডেলিভারি দিন");

    // 7. A 501-char note is a 400 before anything writes.
    add_to_cart(&app, &buyer, &product, 1).await;
    let mut body = manual_body();
    body["order_note"] = json!("x".repeat(501));
    let (status, refused) = checkout(&app, &buyer, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "over-long note: {refused}");
    let (status, orders) = call(&app, Method::GET, "/v1/orders", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(orders["total"], 3, "the refused checkout created nothing");
}
