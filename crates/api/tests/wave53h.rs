//! Wave 53 (part H) integration test: ONE vocabulary,
//! ONE price — the cross-surface agreement pins.
//!
//! The wave put the same facts on many surfaces: the
//! home feed's categories, `/v1/categories`, and the
//! browse's category facet; the home feed's flash card,
//! the browse item, and the suggestion row. Each surface
//! computes its numbers its own way (the feed in Rust
//! aggregates, the browse in the SQL twin, the suggestion
//! through the price map), so agreement is a property to
//! TEST, not to assume:
//!
//! - all three category surfaces report the same counts
//!   for the same catalogue;
//! - the feed's category list is CAPPED (its named tile
//!   limit) while `/v1/categories` is not — with thirteen
//!   categories the feed shows twelve and the endpoint
//!   thirteen, sorted by count desc;
//! - the flash product's price is the same number on the
//!   feed card (`sale_price_poisha` / the view's flash
//!   field), the browse item, the products list and the
//!   suggestion;
//! - the feed's best-sellers rail and the browse's `sold`
//!   sort put the same product first.
//!
//! Helpers follow the Wave 9 test (self-contained file);
//! gated on `DATABASE_URL` like every integration test.

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

/// PATCH a product's category as its seller.
async fn patch_category(app: &Router, token: &str, product_id: &str, category: &str) {
    let (status, body) = call(
        app,
        Method::PATCH,
        &format!("/v1/products/{product_id}"),
        Some(json!({ "category": category })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch category: {body}");
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

/// Buy `quantity` units and drive the order to delivered
/// through the DEV courier (the Wave 9 flow).
async fn buy_and_deliver_qty(
    app: &Router,
    buyer: &str,
    seller: &str,
    product_id: &str,
    quantity: i64,
) {
    add_to_cart(app, buyer, product_id, quantity).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(json!({})), Some(seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(app, Method::POST, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
}

/// Format unix seconds as an RFC 3339 UTC timestamp (the
/// civil-from-days conversion — the Wave 52 helper).
fn rfc3339_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (hour, minute, second) = (
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// The current unix time, read from the database itself.
async fn db_now_unix(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM now())::bigint")
        .fetch_one(pool)
        .await
        .expect("read database clock")
}

#[tokio::test]
async fn wave53h_cross_surface_agreement() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 53h integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate (the Wave 52 truncate canon plus this
    // wave's tables).
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_images, product_variants, products, store_members, stores, \
         otp_codes, users, home_banners, collections, collection_products, \
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
    let state = AppState::new(Some(pool.clone()), "wave53h-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01753538001").await;
    let store = create_store(&app, &seller, "wave53h-shop").await;
    let buyer = login(&app, "01953538001").await;

    // Thirteen categories, one product each — plus a
    // second clothing product (the biggest category), a
    // flash product and a best seller.
    let mut first_by_category: Vec<String> = Vec::new();
    for index in 0..13 {
        let category = format!("cat-{index:02}");
        let id = create_product(
            &app,
            &seller,
            &store,
            &format!("Wave53H Item {index:02}"),
            10_000 + index as i64 * 1_000,
            5,
        )
        .await;
        patch_category(&app, &seller, &id, &category).await;
        first_by_category.push(id);
    }
    let hero = create_product(&app, &seller, &store, "Wave53H Hero Shirt", 200_000, 20).await;
    patch_category(&app, &seller, &hero, "cat-00").await;
    let extra = create_product(&app, &seller, &store, "Wave53H Extra Shirt", 60_000, 20).await;
    patch_category(&app, &seller, &extra, "cat-00").await;

    // The hero sells 3 (the best seller) and rides the
    // flash sale at 150_000.
    buy_and_deliver_qty(&app, &buyer, &seller, &hero, 3).await;
    let t0 = db_now_unix(&pool).await;
    let (status, sale) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Wave53H Flash",
            "starts_at": rfc3339_from_unix(t0 - 3_600),
            "ends_at": rfc3339_from_unix(t0 + 7_200)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create flash sale: {sale}");
    let sale_id = sale["id"].as_str().expect("sale id").to_string();
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{sale_id}/items"),
        Some(json!({ "items": [{ "product_id": hero, "sale_price_poisha": 150_000 }] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set flash items: {body}");

    // ------------------------------------------------------------------
    // 1. Categories: feed (capped) vs endpoint (full) vs
    //    browse facet (uncapped, same counts).
    // ------------------------------------------------------------------
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "home feed: {feed}");
    assert_eq!(
        feed["categories"].as_array().expect("feed categories").len(),
        12,
        "the feed's tile cap applies: {feed}"
    );
    let (status, body) = call(&app, Method::GET, "/v1/categories", None, None).await;
    assert_eq!(status, StatusCode::OK, "categories: {body}");
    let endpoint_categories = body["categories"].as_array().expect("categories").clone();
    assert_eq!(endpoint_categories.len(), 13, "the endpoint is uncapped");
    assert_eq!(
        endpoint_categories[0]["category"], "cat-00",
        "sorted by count desc: {body}"
    );
    assert_eq!(endpoint_categories[0]["product_count"], 3);

    let (status, results) = call(&app, Method::GET, "/v1/search", None, None).await;
    assert_eq!(status, StatusCode::OK, "browse: {results}");
    let facet = results["facets"]["categories"]
        .as_array()
        .expect("category facets")
        .iter()
        .find(|row| row["category"] == "cat-00")
        .expect("cat-00 facet")
        .clone();
    assert_eq!(facet["count"], 3, "the facet agrees with the endpoint");

    // ------------------------------------------------------------------
    // 2. One flash price on every surface.
    // ------------------------------------------------------------------
    let feed_flash = feed["flash_sale"]["items"]
        .as_array()
        .expect("feed flash items")
        .iter()
        .find(|item| item["id"] == hero)
        .expect("hero in the feed's sale")
        .clone();
    assert_eq!(feed_flash["sale_price_poisha"], 150_000);
    assert_eq!(feed_flash["flash_price_poisha"], 150_000);

    let browse_hero = results["items"]
        .as_array()
        .expect("browse items")
        .iter()
        .find(|item| item["id"] == hero)
        .expect("hero browsed")
        .clone();
    assert_eq!(browse_hero["flash_price_poisha"], 150_000);

    let (status, list) = call(&app, Method::GET, "/v1/products?limit=60", None, None).await;
    assert_eq!(status, StatusCode::OK, "products list: {list}");
    let list_hero = list["items"]
        .as_array()
        .expect("list items")
        .iter()
        .find(|item| item["id"] == hero)
        .expect("hero listed")
        .clone();
    assert_eq!(list_hero["flash_price_poisha"], 150_000);

    let (status, body) = call(&app, Method::GET, "/v1/search/suggest?q=Hero", None, None).await;
    assert_eq!(status, StatusCode::OK, "suggest: {body}");
    let suggested = body["products"]
        .as_array()
        .expect("suggested")
        .iter()
        .find(|p| p["id"] == hero)
        .expect("hero suggested")
        .clone();
    assert_eq!(
        suggested["price_poisha"], 150_000,
        "the suggestion prices the flash too: {suggested}"
    );

    // ------------------------------------------------------------------
    // 3. The best seller leads both the feed rail and the
    //    sold sort.
    // ------------------------------------------------------------------
    assert_eq!(
        feed["best_sellers"][0]["id"], hero,
        "the feed's best seller: {feed}"
    );
    let (status, results) = call(&app, Method::GET, "/v1/search?sort=sold", None, None).await;
    assert_eq!(status, StatusCode::OK, "sold browse: {results}");
    assert_eq!(results["items"][0]["id"], hero);
    assert_eq!(results["items"][0]["sold_count"], 3);
}
