//! Wave 53 (part C) integration test: the agreement
//! edges — where the SQL flash twin and the Rust flash
//! resolution are most likely to drift apart, plus the
//! combined-filter facet matrix, the price histogram,
//! suggestion categories, popular searches and the
//! analytics window rule.
//!
//! The centrepiece is the CAP BOUNDARY: a flash item
//! with `stock_cap = 2`. Before the cap is consumed, the
//! product prices at the flash price in the items, the
//! price bands and the histogram. Two units are then
//! bought at the flash price through a real checkout
//! (the cap counts placed, non-cancelled orders — no
//! delivery needed), and every surface must flip to the
//! base price together: the view's `flash_price_poisha`
//! clears, the flash band empties, the base band gains
//! the product, and the histogram's counts move bands.
//! If the SQL twin and the Rust resolution disagreed by
//! even the variant-delta term or the sub-order status
//! term, one of these assertions would catch it.
//!
//! Helpers follow the Wave 9 test (self-contained file);
//! gated on `DATABASE_URL` like every integration test.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
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

/// Run one browse; returns the results envelope.
async fn browse(app: &Router, query: &str) -> Value {
    let (status, body) = call(app, Method::GET, &format!("/v1/search?{query}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "browse {query}: {body}");
    body
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

/// The histogram bucket containing a price, as (min, max, count).
fn histogram_bucket(results: &Value, price: i64) -> (i64, i64, i64) {
    let bucket = results["facets"]["price"]["histogram"]
        .as_array()
        .expect("histogram")
        .iter()
        .find(|bucket| {
            bucket["min_poisha"].as_i64().expect("bucket min") <= price
                && price <= bucket["max_poisha"].as_i64().expect("bucket max")
        })
        .expect("a bucket contains the price")
        .clone();
    (
        bucket["min_poisha"].as_i64().expect("min"),
        bucket["max_poisha"].as_i64().expect("max"),
        bucket["count"].as_i64().expect("count"),
    )
}

/// The histogram's total count across all buckets.
fn histogram_total(results: &Value) -> i64 {
    results["facets"]["price"]["histogram"]
        .as_array()
        .expect("histogram")
        .iter()
        .map(|bucket| bucket["count"].as_i64().expect("bucket count"))
        .sum()
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
async fn wave53c_flash_cap_boundary_and_combined_facets() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 53c integration test");
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

    let state = AppState::new(Some(pool.clone()), "wave53c-test-secret".to_string(), true);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01753532001").await;
    let shop = create_store(&app, &seller, "wave53c-shop").await;
    let seller2 = login(&app, "01753532002").await;
    let other = create_store(&app, &seller2, "wave53c-other").await;
    let buyer = login(&app, "01953532001").await;

    // The catalogue: the capped jacket, a tee and a saree
    // in the shop; a shirt and a gadget in the other store.
    let jacket = create_product(&app, &seller, &shop, "Wave53C Cap Jacket", 200_000, 10).await;
    patch_category(&app, &seller, &jacket, "clothing").await;
    let tee = create_product(&app, &seller, &shop, "Wave53C Tee", 50_000, 10).await;
    patch_category(&app, &seller, &tee, "clothing").await;
    let saree = create_product(&app, &seller, &shop, "Wave53C Saree", 300_000, 10).await;
    patch_category(&app, &seller, &saree, "clothing").await;
    let shirt = create_product(&app, &seller2, &other, "Wave53C Other Shirt", 60_000, 10).await;
    patch_category(&app, &seller2, &shirt, "clothing").await;
    let gadget = create_product(&app, &seller2, &other, "Wave53C Gadget", 250_000, 10).await;
    patch_category(&app, &seller2, &gadget, "electronics").await;

    // The flash sale: the jacket at 150_000, cap TWO units.
    let t0 = db_now_unix(&pool).await;
    let (status, sale) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Wave53C Flash",
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
        Some(json!({
            "items": [{
                "product_id": jacket,
                "sale_price_poisha": 150_000,
                "stock_cap": 2
            }]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set flash items: {body}");

    // ------------------------------------------------------------------
    // 1. Suggestion categories group the matching products.
    // ------------------------------------------------------------------
    let (status, body) = call(&app, Method::GET, "/v1/search/suggest?q=Wave53C", None, None).await;
    assert_eq!(status, StatusCode::OK, "suggest: {body}");
    let clothing = body["categories"]
        .as_array()
        .expect("suggested categories")
        .iter()
        .find(|row| row["category"] == "clothing")
        .expect("clothing suggested")
        .clone();
    assert_eq!(clothing["product_count"], 4, "four clothing matches: {body}");
    let electronics = body["categories"]
        .as_array()
        .expect("suggested categories")
        .iter()
        .find(|row| row["category"] == "electronics")
        .expect("electronics suggested")
        .clone();
    assert_eq!(electronics["product_count"], 1);

    // ------------------------------------------------------------------
    // 2. Combined filters: each facet group answers the
    //    OTHER groups' combined set.
    // ------------------------------------------------------------------
    let results = browse(&app, "category=clothing&store=wave53c-other").await;
    assert_eq!(results["total"], 1, "other store's clothing: {results}");
    assert_eq!(results["items"][0]["id"], shirt);
    // Categories facet: the store filter applies, the
    // category filter does not — the other store's
    // electronics stay visible as a widening option.
    assert_eq!(category_facet(&results, "clothing"), Some(1));
    assert_eq!(category_facet(&results, "electronics"), Some(1));
    // Stores facet: the category filter applies — the
    // shop's three clothing products are counted.
    assert_eq!(store_facet(&results, "wave53c-shop"), Some(3));
    assert_eq!(store_facet(&results, "wave53c-other"), Some(1));

    // Text + facets compose: the text narrows everything,
    // facets included.
    let results = browse(&app, "q=Gadget&category=clothing").await;
    assert_eq!(results["total"], 0, "the gadget is not clothing: {results}");
    assert_eq!(category_facet(&results, "electronics"), Some(1));

    // ------------------------------------------------------------------
    // 3. The histogram prices EFFECTIVE prices and covers
    //    the whole set.
    // ------------------------------------------------------------------
    let results = browse(&app, "").await;
    assert_eq!(results["total"], 5);
    assert_eq!(histogram_total(&results), 5, "every product in a band");
    assert_eq!(
        histogram_bucket(&results, 150_000).2,
        1,
        "the jacket counts at its FLASH price: {results}"
    );
    assert_eq!(
        histogram_bucket(&results, 200_000).2,
        0,
        "nothing counts at the jacket's base yet: {results}"
    );
    // The view agrees: the jacket carries the flash price.
    let jacket_view = results["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["id"] == jacket)
        .expect("jacket listed")
        .clone();
    assert_eq!(jacket_view["flash_price_poisha"], 150_000);

    // ------------------------------------------------------------------
    // 4. Popular searches: the public aggregate over the
    //    same identifier-free log.
    // ------------------------------------------------------------------
    for _ in 0..3 {
        let results = browse(&app, "q=Wave53C").await;
        assert_eq!(results["total"], 5);
    }
    let (status, popular) = call(&app, Method::GET, "/v1/search/popular", None, None).await;
    assert_eq!(status, StatusCode::OK, "popular: {popular}");
    assert_eq!(popular["window_days"], 30);
    let wave53c = popular["searches"]
        .as_array()
        .expect("popular searches")
        .iter()
        .find(|row| row["query_text"] == "Wave53C")
        .expect("the repeated search is popular")
        .clone();
    assert!(
        wave53c["searches"].as_i64().expect("searches") >= 3,
        "counted at least the three runs: {wave53c}"
    );

    // ------------------------------------------------------------------
    // 5. THE CAP BOUNDARY: buy the cap's two units at the
    //    flash price; every surface flips together.
    // ------------------------------------------------------------------
    add_to_cart(&app, &buyer, &jacket, 2).await;
    let order = checkout_cod(&app, &buyer).await;
    let line = &order["sub_orders"][0]["items"][0];
    assert_eq!(
        line["unit_price_poisha"], 150_000,
        "the checkout charged the flash price: {order}"
    );

    let results = browse(&app, "").await;
    let jacket_view = results["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["id"] == jacket)
        .expect("jacket listed")
        .clone();
    assert!(
        jacket_view["flash_price_poisha"].is_null(),
        "the Rust resolution stops at the cap: {jacket_view}"
    );
    assert_eq!(histogram_total(&results), 5, "the set did not change");
    assert_eq!(
        histogram_bucket(&results, 150_000).2,
        0,
        "the SQL twin stops at the cap too: {results}"
    );
    assert_eq!(histogram_bucket(&results, 200_000).2, 1);
    let results = browse(&app, "min_price_poisha=140000&max_price_poisha=160000").await;
    assert_eq!(results["total"], 0, "the flash band is empty now: {results}");
    let results = browse(&app, "min_price_poisha=190000&max_price_poisha=210000").await;
    assert_eq!(results["total"], 1, "the base band holds the jacket: {results}");
    assert_eq!(results["items"][0]["id"], jacket);

    // ------------------------------------------------------------------
    // 6. The analytics window rule: default, explicit and
    //    out-of-range.
    // ------------------------------------------------------------------
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/search-analytics?days=7",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "days=7: {body}");
    assert_eq!(body["window_days"], 7);
    for days in ["0", "91", "-1"] {
        let (status, body) = call(
            &app,
            Method::GET,
            &format!("/v1/admin/search-analytics?days={days}"),
            None,
            Some(&admin),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "days={days}: {body}");
    }
}
