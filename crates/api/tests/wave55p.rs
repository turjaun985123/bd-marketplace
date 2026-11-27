//! Wave 55p integration test: Wave 55 features across a
//! SPLIT (multi-store) order — one address-book checkout
//! spanning two sellers charges each store's delivery at
//! the address's zone rate, both sellers see the note +
//! district on their own sub-order, and reordering the
//! split order re-adds BOTH stores' items in one report.
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

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<Router> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55p integration tests");
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
    let state = AppState::new(Some(pool), "wave55p-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some(build_app(state))
}

#[tokio::test]
async fn wave55p_split_order_address_checkout_and_reorder() {
    let Some(app) = test_app().await else {
        return;
    };
    let seller_a = login(&app, "01723232323").await;
    let store_a = create_store(&app, &seller_a, "wave55p-store-a").await;
    let product_a = create_product(&app, &seller_a, &store_a, "Split A", 10_000, 10).await;
    let seller_b = login(&app, "01724242424").await;
    let store_b = create_store(&app, &seller_b, "wave55p-store-b").await;
    let product_b = create_product(&app, &seller_b, &store_b, "Split B", 20_000, 10).await;
    let buyer = login(&app, "01923232323").await;

    // A Gazipur (suburb) address.
    let (status, address) = call(
        &app,
        Method::POST,
        "/v1/addresses",
        Some(json!({
            "label": "অফিস",
            "recipient_name": "Split Buyer",
            "phone": "01712345678",
            "district": "Gazipur",
            "area": "Tongi",
            "address_line": "Station Road 4"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "address: {address}");
    let address_id = address["id"].as_str().expect("address id").to_string();

    // One cart, two stores, one address checkout + note.
    add_to_cart(&app, &buyer, &product_a, 1).await;
    add_to_cart(&app, &buyer, &product_b, 1).await;
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
            "address_id": address_id,
            "order_note": "দুই দোকানেই একই নোট"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "split checkout: {order}");
    let order_id = order["id"].as_str().expect("order id").to_string();
    assert_eq!(order["delivery_district"], "Gazipur");
    let subs = order["sub_orders"].as_array().expect("sub-orders");
    assert_eq!(subs.len(), 2, "the cart split by store: {order}");
    // Each store's parcel is 1 kg at the SUBURB first-kg
    // rate (10_000): delivery total 20_000, goods 30_000,
    // grand 50_000.
    for sub in subs {
        assert_eq!(sub["delivery_charge_poisha"], 10_000, "{sub}");
    }
    assert_eq!(order["delivery_total_poisha"], 20_000);
    assert_eq!(order["goods_total_poisha"], 30_000);
    assert_eq!(order["grand_total_poisha"], 50_000);

    // Each seller sees the note + district on THEIR
    // sub-order (and only theirs in their list).
    for (seller, title) in [(&seller_a, "Split A"), (&seller_b, "Split B")] {
        let (status, list) = call(
            &app,
            Method::GET,
            "/v1/seller/sub-orders?limit=10",
            None,
            Some(seller),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let items = list["items"].as_array().expect("items");
        assert_eq!(items.len(), 1, "each seller sees only their slice");
        assert_eq!(items[0]["order_note"], "দুই দোকানেই একই নোট");
        assert_eq!(items[0]["delivery_district"], "Gazipur");
        assert_eq!(items[0]["items"][0]["title"], title);
    }

    // Both stores' fulfillment queues carry the district
    // for the packing bench.
    for (seller, store) in [(&seller_a, &store_a), (&seller_b, &store_b)] {
        let uri = format!("/v1/seller/fulfillment?store_id={store}");
        let (status, queue) = call(&app, Method::GET, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "queue: {queue}");
        assert_eq!(queue["items"][0]["delivery_district"], "Gazipur");
        assert_eq!(queue["items"][0]["order_note"], "দুই দোকানেই একই নোট");
    }

    // Reordering the split order re-adds BOTH stores'
    // items in one honest report.
    let uri = format!("/v1/orders/{order_id}/reorder");
    let (status, report) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "split reorder: {report}");
    let added = report["added"].as_array().expect("added");
    assert_eq!(added.len(), 2, "{report}");
    assert_eq!(report["skipped"].as_array().expect("skipped").len(), 0);
    assert_eq!(report["cart"]["goods_total_poisha"], 30_000);
}
