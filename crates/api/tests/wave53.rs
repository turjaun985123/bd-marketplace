//! Wave 53 integration test: discovery & facets — the
//! faceted browse behind `GET /v1/search`.
//!
//! The catalogue is controlled end to end: two stores,
//! four categories, nine products with known prices,
//! stocks, ratings (real delivered orders + reviews),
//! sold counts and one live flash sale. Against it the
//! test pins the wave's contract:
//!
//! - the facet convention: each facet group counts the
//!   set filtered by every OTHER group (the categories
//!   facet does not move when a category is picked; the
//!   stores facet does);
//! - prices are EFFECTIVE: a flash product enters a price
//!   band its base price is outside of, and leaves a band
//!   its base price is inside of, because the band prices
//!   the flash price — and the item's own
//!   `flash_price_poisha` agrees;
//! - rating / in-stock / on-sale filters use the platform
//!   definitions (unrated = 0 stars; effective stock is
//!   the variant sum; on sale = live flash or a real
//!   compare-at above the price);
//! - every sort's first item is the expected product;
//! - pagination totals and slicing are exact under both
//!   page-size spellings (`limit`, legacy `per_page`);
//! - suggestions carry the effective price, the category
//!   and the image URL;
//! - `/v1/categories` agrees with the home feed;
//! - invalid filters are 400s;
//! - the products list gained the same sorts;
//! - searches land in the analytics log and the admin
//!   read reports them (top + zero-result), admin-only.
//!
//! The small helpers are copied from the Wave 9 test on
//! purpose (integration test files stay self-contained);
//! the multipart pair and the clock helpers follow the
//! Wave 51/52 tests.
//!
//! Gated on `DATABASE_URL`: without it the test skips
//! cleanly so a plain local `cargo test` (no database)
//! still passes. CI provides a Postgres 16 service and
//! sets `DATABASE_URL` for the rust job.

use std::sync::Arc;

use api::media::LocalDiskStore;
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

/// Buy one unit of the product and drive the order all the way to
/// delivered through the DEV courier; returns the order id.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) {
    buy_and_deliver_qty(app, buyer, seller, product_id, 1).await;
}

/// Buy `quantity` units and drive the order to delivered
/// (the Wave 9 flow with a quantity — the sold-count
/// assertions need multi-unit deliveries).
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

    // Four polls walk the DEV courier created -> delivered.
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(app, Method::POST, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
}

/// PATCH a product's category / compare-at as its seller.
async fn patch_product(app: &Router, token: &str, product_id: &str, patch: Value) {
    let (status, body) = call(
        app,
        Method::PATCH,
        &format!("/v1/products/{product_id}"),
        Some(patch),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch product: {body}");
}

/// Review a delivered product as a buyer (Wave 8 shape).
async fn review(app: &Router, token: &str, product_id: &str, rating: i64) {
    let (status, body) = call(
        app,
        Method::POST,
        &format!("/v1/products/{product_id}/reviews"),
        Some(json!({ "rating": rating })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "review: {body}");
}

/// Run one browse; returns the results envelope.
async fn browse(app: &Router, query: &str) -> Value {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/search?{query}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "browse {query}: {body}");
    body
}

/// The ids of a browse envelope, in result order.
fn result_ids(results: &Value) -> Vec<String> {
    results["items"]
        .as_array()
        .expect("items is an array")
        .iter()
        .map(|item| item["id"].as_str().expect("item id").to_string())
        .collect()
}

/// One category facet's count, by label (None = absent).
fn category_facet(results: &Value, category: &str) -> Option<i64> {
    results["facets"]["categories"]
        .as_array()
        .expect("category facets")
        .iter()
        .find(|facet| facet["category"] == category)
        .map(|facet| facet["count"].as_i64().expect("facet count"))
}

/// One store facet's count, by slug (None = absent).
fn store_facet(results: &Value, slug: &str) -> Option<i64> {
    results["facets"]["stores"]
        .as_array()
        .expect("store facets")
        .iter()
        .find(|facet| facet["slug"] == slug)
        .map(|facet| facet["count"].as_i64().expect("facet count"))
}

/// The rating facet's count for one threshold.
fn rating_facet(results: &Value, threshold: f64) -> Option<i64> {
    results["facets"]["ratings"]
        .as_array()
        .expect("rating facets")
        .iter()
        .find(|facet| facet["min_rating"].as_f64() == Some(threshold))
        .map(|facet| facet["count"].as_i64().expect("facet count"))
}

/// One part of a hand-built multipart body (Wave 51's helper).
struct Part<'a> {
    name: &'a str,
    filename: Option<&'a str>,
    content_type: Option<&'a str>,
    data: &'a [u8],
}

/// Send one multipart/form-data request through the router
/// and return (status, JSON body).
async fn call_multipart(
    app: &Router,
    uri: &str,
    token: Option<&str>,
    parts: &[Part<'_>],
) -> (StatusCode, Value) {
    const BOUNDARY: &str = "----bdm-wave53-test-boundary";
    let mut body: Vec<u8> = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let mut disposition =
            format!("Content-Disposition: form-data; name=\"{}\"", part.name);
        if let Some(filename) = part.filename {
            disposition.push_str(&format!("; filename=\"{filename}\""));
        }
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(b"\r\n");
        if let Some(content_type) = part.content_type {
            body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(part.data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

    let mut builder = Request::builder().method(Method::POST).uri(uri);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let request = builder
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .expect("request builds");
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

/// A real 4x4 PNG (solid red) — the Wave 51 fixture bytes.
const PNG_4X4: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 4, 0, 0, 0, 4, 8, 2, 0,
    0, 0, 38, 147, 9, 41, 0, 0, 0, 16, 73, 68, 65, 84, 120, 218, 99, 56, 33, 39, 7, 71, 12, 196,
    113, 0, 177, 99, 16, 65, 211, 74, 204, 13, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// Format unix seconds as an RFC 3339 UTC timestamp (the
/// civil-from-days conversion), so the test can schedule
/// flash windows relative to the database clock without a
/// date library. (The Wave 52 helper.)
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

/// The current unix time, read from the database itself
/// (the clock every stored window is compared against).
async fn db_now_unix(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM now())::bigint")
        .fetch_one(pool)
        .await
        .expect("read database clock")
}

#[tokio::test]
async fn wave53_faceted_browse_end_to_end() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 53 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the
    // same database (the Wave 52 truncate canon, plus this
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

    // SMS dev mode (OTP login), courier dev mode
    // (deliveries) and a scratch media store (the shirt's
    // photo) — the Wave 52 test's state shape.
    let media_root = std::env::temp_dir().join(format!(
        "bdm-wave53-media-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave53-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers)
        .with_media_store(Arc::new(LocalDiskStore::new(media_root)));
    let app = build_app(state);

    // The cast: an admin (flash sales), two sellers, two
    // buyers (two reviewers for one product).
    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller_a = login(&app, "01753530001").await;
    let store_a = create_store(&app, &seller_a, "wave53-alpha").await;
    let seller_b = login(&app, "01753530002").await;
    let store_b = create_store(&app, &seller_b, "wave53-beta").await;
    let buyer1 = login(&app, "01953530001").await;
    let buyer2 = login(&app, "01953530002").await;

    // The catalogue. Store alpha (6 products):
    //   shirt  — clothing, ৳500, the rated + photographed one
    //   saree  — clothing, ৳3,000, compare-at ৳4,000 (on sale)
    //   shoes  — footwear, ৳1,200, stock 0
    //   jacket — clothing, ৳2,000, the flash-sale product
    //   belt   — accessories, ৳300, variants summing to 0
    //   wallet — accessories, ৳800, compare-at == price (NOT on sale)
    // Store beta (3 products):
    //   jeans   — clothing, ৳900
    //   earbuds — electronics, ৳1,500, the best seller
    //   case    — electronics, ৳200, stock 0, created last
    let shirt = create_product(&app, &seller_a, &store_a, "Wave53 Cotton Shirt", 50_000, 10).await;
    patch_product(&app, &seller_a, &shirt, json!({ "category": "clothing" })).await;
    let saree = create_product(&app, &seller_a, &store_a, "Wave53 Silk Saree", 300_000, 5).await;
    patch_product(
        &app,
        &seller_a,
        &saree,
        json!({ "category": "clothing", "compare_at_poisha": 400_000 }),
    )
    .await;
    let shoes = create_product(&app, &seller_a, &store_a, "Wave53 Running Shoes", 120_000, 0).await;
    patch_product(&app, &seller_a, &shoes, json!({ "category": "footwear" })).await;
    let jacket = create_product(&app, &seller_a, &store_a, "Wave53 Flash Jacket", 200_000, 10).await;
    patch_product(&app, &seller_a, &jacket, json!({ "category": "clothing" })).await;
    let belt = create_product(&app, &seller_a, &store_a, "Wave53 Variant Belt", 30_000, 0).await;
    patch_product(&app, &seller_a, &belt, json!({ "category": "accessories" })).await;
    for (name, delta) in [("Black", 0), ("Brown", 5_000)] {
        let (status, body) = call(
            &app,
            Method::POST,
            &format!("/v1/seller/products/{belt}/variants"),
            Some(json!({ "name": name, "price_delta_poisha": delta, "stock": 0 })),
            Some(&seller_a),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "create variant {name}: {body}");
    }
    let wallet = create_product(&app, &seller_a, &store_a, "Wave53 Leather Wallet", 80_000, 3).await;
    patch_product(
        &app,
        &seller_a,
        &wallet,
        json!({ "category": "accessories", "compare_at_poisha": 80_000 }),
    )
    .await;
    let jeans = create_product(&app, &seller_b, &store_b, "Wave53 Denim Jeans", 90_000, 7).await;
    patch_product(&app, &seller_b, &jeans, json!({ "category": "clothing" })).await;
    let earbuds =
        create_product(&app, &seller_b, &store_b, "Wave53 Wireless Earbuds", 150_000, 10).await;
    patch_product(&app, &seller_b, &earbuds, json!({ "category": "electronics" })).await;
    let case = create_product(&app, &seller_b, &store_b, "Wave53 Phone Case", 20_000, 0).await;
    patch_product(&app, &seller_b, &case, json!({ "category": "electronics" })).await;

    // The shirt gets a real photo through the media
    // pipeline (the suggestion assertion needs one).
    let (status, uploaded) = call_multipart(
        &app,
        &format!("/v1/seller/products/{shirt}/images"),
        Some(&seller_a),
        &[Part {
            name: "file",
            filename: Some("shirt.png"),
            content_type: Some("image/png"),
            data: PNG_4X4,
        }],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "upload shirt photo: {uploaded}");

    // Sales + ratings, all real: shirt sells 3 (2 + 1) and
    // is reviewed 5 and 4 (avg 4.5); earbuds sell 4 and
    // are reviewed 3 (avg 3.0).
    buy_and_deliver_qty(&app, &buyer1, &seller_a, &shirt, 2).await;
    review(&app, &buyer1, &shirt, 5).await;
    buy_and_deliver(&app, &buyer2, &seller_a, &shirt).await;
    review(&app, &buyer2, &shirt, 4).await;
    buy_and_deliver_qty(&app, &buyer1, &seller_b, &earbuds, 4).await;
    review(&app, &buyer1, &earbuds, 3).await;

    // The flash sale: live now, the jacket at 150_000.
    let t0 = db_now_unix(&pool).await;
    let (status, sale) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Wave53 Flash",
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
        Some(json!({ "items": [{ "product_id": jacket, "sale_price_poisha": 150_000 }] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set flash items: {body}");

    // ------------------------------------------------------------------
    // 1. The unfiltered browse: totals, envelope, facets.
    // ------------------------------------------------------------------
    let results = browse(&app, "").await;
    assert_eq!(results["total"], 9, "all nine products: {results}");
    assert_eq!(results["page"], 1);
    assert_eq!(results["per_page"], 24, "browse default page size");
    assert_eq!(results["limit"], 24, "limit echoes the page size");
    assert_eq!(category_facet(&results, "clothing"), Some(4));
    assert_eq!(category_facet(&results, "accessories"), Some(2));
    assert_eq!(category_facet(&results, "electronics"), Some(2));
    assert_eq!(category_facet(&results, "footwear"), Some(1));
    assert_eq!(store_facet(&results, "wave53-alpha"), Some(6));
    assert_eq!(store_facet(&results, "wave53-beta"), Some(3));
    assert_eq!(results["facets"]["price"]["min_poisha"], 20_000);
    assert_eq!(results["facets"]["price"]["max_poisha"], 300_000);
    assert_eq!(rating_facet(&results, 4.0), Some(1), "only the shirt is 4★+");
    assert_eq!(rating_facet(&results, 3.0), Some(2));
    assert_eq!(rating_facet(&results, 2.0), Some(2));

    // The text search still works and sees all nine.
    let results = browse(&app, "q=Wave53").await;
    assert_eq!(results["total"], 9, "text match: {results}");

    // ------------------------------------------------------------------
    // 2. The facet convention: picking a category leaves
    //    the category facet alone and moves the others.
    // ------------------------------------------------------------------
    let results = browse(&app, "category=clothing").await;
    assert_eq!(results["total"], 4, "clothing only: {results}");
    assert_eq!(
        category_facet(&results, "clothing"),
        Some(4),
        "own facet excludes its own filter"
    );
    assert_eq!(category_facet(&results, "electronics"), Some(2));
    assert_eq!(store_facet(&results, "wave53-alpha"), Some(3));
    assert_eq!(store_facet(&results, "wave53-beta"), Some(1));
    assert_eq!(results["facets"]["price"]["min_poisha"], 50_000);
    assert_eq!(results["facets"]["price"]["max_poisha"], 300_000);

    // Multi-value categories OR together — repeated
    // parameters and the comma form agree.
    let results = browse(&app, "category=clothing&category=electronics").await;
    assert_eq!(results["total"], 6, "repeated category params: {results}");
    let results = browse(&app, "category=clothing,electronics").await;
    assert_eq!(results["total"], 6, "comma category list: {results}");

    // Picking a store moves the category facet instead.
    let results = browse(&app, "store=wave53-beta").await;
    assert_eq!(results["total"], 3, "beta only: {results}");
    assert_eq!(store_facet(&results, "wave53-alpha"), Some(6));
    assert_eq!(category_facet(&results, "clothing"), Some(1));
    assert_eq!(category_facet(&results, "electronics"), Some(2));
    assert_eq!(
        category_facet(&results, "accessories"),
        None,
        "beta has no accessories"
    );
    // The legacy single-store spelling still works.
    let results = browse(&app, "store_slug=wave53-alpha").await;
    assert_eq!(results["total"], 6, "legacy store_slug: {results}");

    // ------------------------------------------------------------------
    // 3. Prices are effective: the flash price, not the
    //    base, decides the bands — in both directions.
    // ------------------------------------------------------------------
    let results = browse(&app, "min_price_poisha=140000&max_price_poisha=160000").await;
    assert_eq!(results["total"], 2, "the 140–160k band: {results}");
    let band_ids = result_ids(&results);
    assert!(band_ids.contains(&jacket), "flash price pulls the jacket in: {band_ids:?}");
    assert!(band_ids.contains(&earbuds), "earbuds at base 150k: {band_ids:?}");
    let jacket_view = results["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["id"] == jacket)
        .expect("jacket in band")
        .clone();
    assert_eq!(
        jacket_view["flash_price_poisha"], 150_000,
        "the view agrees with the band that admitted it: {jacket_view}"
    );
    // The jacket's BASE (200_000) sits inside this band,
    // but its effective price (150_000) does not: it is
    // excluded. Pricing by base would wrongly include it.
    let results = browse(&app, "min_price_poisha=160000&max_price_poisha=250000").await;
    assert_eq!(results["total"], 0, "the 160–250k band is empty: {results}");
    assert!(!result_ids(&results).contains(&jacket));

    // ------------------------------------------------------------------
    // 4. Rating, stock and sale filters.
    // ------------------------------------------------------------------
    let results = browse(&app, "min_rating=4").await;
    assert_eq!(results["total"], 1, "4★ and up: {results}");
    assert_eq!(result_ids(&results), vec![shirt.clone()]);
    // The rating facet excludes its own filter.
    assert_eq!(rating_facet(&results, 4.0), Some(1));
    assert_eq!(rating_facet(&results, 3.0), Some(2));
    let results = browse(&app, "min_rating=3").await;
    assert_eq!(results["total"], 2, "3★ and up: {results}");

    let results = browse(&app, "in_stock=true").await;
    assert_eq!(results["total"], 6, "in stock: {results}");
    let stocked = result_ids(&results);
    for absent in [&shoes, &belt, &case] {
        assert!(
            !stocked.contains(absent),
            "out-of-stock product excluded: {stocked:?}"
        );
    }

    let results = browse(&app, "on_sale=true").await;
    assert_eq!(results["total"], 2, "on sale: {results}");
    let on_sale = result_ids(&results);
    assert!(on_sale.contains(&saree), "compare-at sale: {on_sale:?}");
    assert!(on_sale.contains(&jacket), "flash sale: {on_sale:?}");
    assert!(
        !on_sale.contains(&wallet),
        "compare-at equal to price is not a sale: {on_sale:?}"
    );

    // ------------------------------------------------------------------
    // 5. Sorts: each first item is the expected product.
    // ------------------------------------------------------------------
    let results = browse(&app, "sort=price_asc").await;
    assert_eq!(result_ids(&results)[0], case, "cheapest first: {results}");
    let results = browse(&app, "sort=price_desc").await;
    assert_eq!(result_ids(&results)[0], saree, "dearest first: {results}");
    let results = browse(&app, "sort=rating").await;
    assert_eq!(result_ids(&results)[0], shirt, "best rated first: {results}");
    let results = browse(&app, "sort=sold").await;
    assert_eq!(result_ids(&results)[0], earbuds, "best seller first: {results}");
    let results = browse(&app, "sort=newest").await;
    assert_eq!(result_ids(&results)[0], case, "newest first: {results}");

    // ------------------------------------------------------------------
    // 6. Pagination: exact totals and slicing under both
    //    page-size spellings.
    // ------------------------------------------------------------------
    let page1 = browse(&app, "limit=4&page=1").await;
    assert_eq!(page1["total"], 9);
    assert_eq!(page1["limit"], 4);
    assert_eq!(page1["items"].as_array().expect("items").len(), 4);
    let page2 = browse(&app, "limit=4&page=2").await;
    assert_eq!(page2["items"].as_array().expect("items").len(), 4);
    let page3 = browse(&app, "limit=4&page=3").await;
    assert_eq!(page3["items"].as_array().expect("items").len(), 1);
    let mut paged = result_ids(&page1);
    paged.extend(result_ids(&page2));
    paged.extend(result_ids(&page3));
    paged.sort();
    paged.dedup();
    assert_eq!(paged.len(), 9, "pages partition the set: {paged:?}");
    let legacy = browse(&app, "per_page=2&page=3").await;
    assert_eq!(legacy["per_page"], 2);
    assert_eq!(legacy["items"].as_array().expect("items").len(), 2);

    // ------------------------------------------------------------------
    // 7. Suggestions carry the effective price, the
    //    category and the image.
    // ------------------------------------------------------------------
    let (status, body) = call(&app, Method::GET, "/v1/search/suggest?q=Cotton", None, None).await;
    assert_eq!(status, StatusCode::OK, "suggest: {body}");
    let suggested = body["products"]
        .as_array()
        .expect("suggested products")
        .iter()
        .find(|p| p["id"] == shirt)
        .expect("shirt suggested")
        .clone();
    assert_eq!(suggested["price_poisha"], 50_000, "suggest price: {suggested}");
    assert_eq!(suggested["category"], "clothing");
    let suggested_image = suggested["image_url"].as_str().expect("image url present");
    assert!(
        suggested_image.starts_with("/media/"),
        "suggest image is a media URL: {suggested_image}"
    );
    let (status, body) = call(&app, Method::GET, "/v1/search/suggest?q=Flash", None, None).await;
    assert_eq!(status, StatusCode::OK, "suggest flash: {body}");
    let suggested = body["products"]
        .as_array()
        .expect("suggested products")
        .iter()
        .find(|p| p["id"] == jacket)
        .expect("jacket suggested")
        .clone();
    assert_eq!(
        suggested["price_poisha"], 150_000,
        "the suggestion prices the flash, not the base: {suggested}"
    );

    // ------------------------------------------------------------------
    // 8. /v1/categories agrees with the home feed.
    // ------------------------------------------------------------------
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "home feed: {feed}");
    let (status, body) = call(&app, Method::GET, "/v1/categories", None, None).await;
    assert_eq!(status, StatusCode::OK, "categories: {body}");
    let mut from_feed: Vec<(String, i64)> = feed["categories"]
        .as_array()
        .expect("feed categories")
        .iter()
        .map(|c| {
            (
                c["category"].as_str().expect("category").to_string(),
                c["product_count"].as_i64().expect("count"),
            )
        })
        .collect();
    let mut from_endpoint: Vec<(String, i64)> = body["categories"]
        .as_array()
        .expect("endpoint categories")
        .iter()
        .map(|c| {
            (
                c["category"].as_str().expect("category").to_string(),
                c["product_count"].as_i64().expect("count"),
            )
        })
        .collect();
    from_feed.sort();
    from_endpoint.sort();
    assert_eq!(from_endpoint, from_feed, "one category vocabulary");

    // ------------------------------------------------------------------
    // 9. Invalid filters are 400s.
    // ------------------------------------------------------------------
    for query in [
        "min_rating=7",
        "min_rating=-1",
        "in_stock=yes",
        "on_sale=maybe",
        "limit=0",
        "limit=61",
        "min_price_poisha=100000001",
        "sort=banana",
    ] {
        let (status, body) = call(
            &app,
            Method::GET,
            &format!("/v1/search?{query}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
    }
    let long_category = "x".repeat(61);
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/search?category={long_category}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "over-long category: {body}");

    // ------------------------------------------------------------------
    // 10. The products list gained the same sorts.
    // ------------------------------------------------------------------
    let (status, list) = call(&app, Method::GET, "/v1/products?sort=price_asc", None, None).await;
    assert_eq!(status, StatusCode::OK, "list price_asc: {list}");
    assert_eq!(list["total"], 9);
    assert_eq!(list["items"][0]["id"], case, "list cheapest first: {list}");
    let (status, list) = call(&app, Method::GET, "/v1/products?sort=sold", None, None).await;
    assert_eq!(status, StatusCode::OK, "list sold: {list}");
    assert_eq!(list["items"][0]["id"], earbuds, "list best seller: {list}");
    let (status, body) = call(&app, Method::GET, "/v1/products?sort=banana", None, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "list bad sort: {body}");

    // ------------------------------------------------------------------
    // 11. Analytics: searches are logged; the admin read
    //     reports top + zero-result queries, admin-only.
    // ------------------------------------------------------------------
    // The zero-result probe shares no trigrams with any fixture
    // title — a probe like "Wave53Unobtainium" would legitimately
    // match every "Wave53 …" product through the search's
    // documented 0.2 trigram typo tolerance.
    let results = browse(&app, "q=Zxqwvbn").await;
    assert_eq!(results["total"], 0, "the zero-result search: {results}");

    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/search-analytics",
        None,
        Some(&buyer1),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "buyer analytics: {body}");

    let (status, analytics) = call(
        &app,
        Method::GET,
        "/v1/admin/search-analytics",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "analytics: {analytics}");
    assert_eq!(analytics["window_days"], 30);
    assert!(
        analytics["total_searches"].as_i64().expect("total") >= 20,
        "every browse above was logged: {analytics}"
    );
    let top_texts: Vec<&str> = analytics["top_searches"]
        .as_array()
        .expect("top searches")
        .iter()
        .map(|row| row["query_text"].as_str().expect("query text"))
        .collect();
    assert!(
        top_texts.contains(&"Wave53"),
        "the text search was logged: {top_texts:?}"
    );
    let zero = analytics["zero_result_searches"]
        .as_array()
        .expect("zero-result searches")
        .iter()
        .find(|row| row["query_text"] == "Zxqwvbn")
        .expect("the zero-result search is reported")
        .clone();
    assert!(zero["searches"].as_i64().expect("searches") >= 1);
}
