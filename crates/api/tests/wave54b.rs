//! Wave 54 integration tests (part 2): the store's public
//! aggregates (rating over all its products' reviews,
//! delivered units, active product count), the rating
//! summary distributions (product + store, floored
//! percents, photo counts), the review list sorts
//! (newest / highest / lowest / with photos, and a 400 on
//! an unknown sort), and the store's all-reviews list.
//!
//! The small helpers are copied from the Wave 8/9 tests on
//! purpose, since integration test files stay
//! self-contained.
//!
//! Gated on `DATABASE_URL`: skipped (with a printed note)
//! when it is unset, so a plain local `cargo test` (no
//! database) still passes. CI provides a Postgres 16
//! service and sets `DATABASE_URL` for the rust job.

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

/// One multipart part for the review-photo upload.
struct Part<'a> {
    name: &'a str,
    filename: Option<&'a str>,
    content_type: Option<&'a str>,
    data: &'a [u8],
}

/// The smallest valid PNG the media pipeline accepts
/// (4x4; copied byte-for-byte from the Wave 51 test).
const PNG_4X4: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82,
    0, 0, 0, 4, 0, 0, 0, 4, 8, 2, 0, 0, 0, 38, 147, 9,
    41, 0, 0, 0, 16, 73, 68, 65, 84, 120, 218, 99, 56, 33, 39, 7,
    71, 12, 196, 113, 0, 177, 99, 16, 65, 211, 74, 204, 13, 0, 0, 0,
    0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// POST a multipart form through the router (copied from
/// the Wave 51 test, which owns the pattern).
async fn call_multipart(
    app: &Router,
    uri: &str,
    token: Option<&str>,
    parts: &[Part<'_>],
) -> (StatusCode, Value) {
    const BOUNDARY: &str = "----bdm-wave54-test-boundary";
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

/// Buy one unit of the product and drive the order all the
/// way to delivered through the DEV courier.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) {
    add_to_cart(app, buyer, product_id, 1).await;
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

/// POST a review of the product as the given user.
async fn post_review(
    app: &Router,
    token: &str,
    product_id: &str,
    rating: i64,
    body: Option<&str>,
) -> (StatusCode, Value) {
    let uri = format!("/v1/products/{product_id}/reviews");
    let mut payload = json!({ "rating": rating });
    if let Some(body) = body {
        payload["body"] = json!(body);
    }
    call(app, Method::POST, &uri, Some(payload), Some(token)).await
}

/// The shared setup: migrated, truncated database + a
/// dev-mode app.
async fn test_app() -> Option<Router> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 54b integration tests");
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
    let state = AppState::new(Some(pool.clone()), "wave54-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some(build_app(state))
}

/// The distribution level for `stars` in a rating summary.
fn level(summary: &Value, stars: i64) -> &Value {
    summary["distribution"]
        .as_array()
        .expect("distribution array")
        .iter()
        .find(|entry| entry["stars"] == stars)
        .expect("level present")
}

#[tokio::test]
async fn wave54_store_aggregates_summaries_and_review_sorts() {
    let Some(app) = test_app().await else {
        return;
    };

    let seller = login(&app, "01710000021").await;
    let store_id = create_store(&app, &seller, "wave54-aggro").await;
    let product_a = create_product(&app, &seller, &store_id, "Wave54 Aggro A", 40_000, 50).await;
    let product_b = create_product(&app, &seller, &store_id, "Wave54 Aggro B", 30_000, 50).await;
    let product_c = create_product(&app, &seller, &store_id, "Wave54 Aggro C", 20_000, 50).await;

    // Three delivered purchases, three reviews: A gets 5
    // and 3, B gets 4 (with a photo).
    let buyer1 = login(&app, "01710000022").await;
    let buyer2 = login(&app, "01710000023").await;
    let buyer3 = login(&app, "01710000024").await;
    buy_and_deliver(&app, &buyer1, &seller, &product_a).await;
    buy_and_deliver(&app, &buyer2, &seller, &product_a).await;
    buy_and_deliver(&app, &buyer3, &seller, &product_b).await;

    let (status, review) = post_review(&app, &buyer1, &product_a, 5, Some("দারুণ")).await;
    assert_eq!(status, StatusCode::CREATED, "review 5: {review}");
    let (status, review) = post_review(&app, &buyer2, &product_a, 3, None).await;
    assert_eq!(status, StatusCode::CREATED, "review 3: {review}");
    let (status, review) = post_review(&app, &buyer3, &product_b, 4, Some("ভালো")).await;
    assert_eq!(status, StatusCode::CREATED, "review 4: {review}");
    let photo_review_id = review["id"].as_str().expect("review id").to_string();
    let (status, with_photo) = call_multipart(
        &app,
        &format!("/v1/reviews/{photo_review_id}/image"),
        Some(&buyer3),
        &[Part {
            name: "file",
            filename: Some("worn.png"),
            content_type: Some("image/png"),
            data: PNG_4X4,
        }],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "attach photo: {with_photo}");

    // The public store view's aggregates: rating over all
    // three reviews (4.0), three delivered units, three
    // active products.
    let (status, view) = call(&app, Method::GET, "/v1/stores/wave54-aggro", None, None).await;
    assert_eq!(status, StatusCode::OK, "store view: {view}");
    assert_eq!(view["rating_count"], 3);
    assert_eq!(view["rating_avg"], 4.0);
    assert_eq!(view["total_sold"], 3);
    assert_eq!(view["product_count"], 3);

    // Product A's summary: two reviews, 5 and 3.
    let (status, summary) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_a}/rating-summary"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "product summary: {summary}");
    assert_eq!(summary["rating_count"], 2);
    assert_eq!(summary["rating_avg"], 4.0);
    assert_eq!(summary["with_photos_count"], 0);
    assert_eq!(level(&summary, 5)["count"], 1);
    assert_eq!(level(&summary, 5)["percent"], 50);
    assert_eq!(level(&summary, 3)["count"], 1);
    assert_eq!(level(&summary, 4)["count"], 0);

    // The store summary: three reviews, one photographed,
    // percents floored (1/3 -> 33, never rounded up).
    let (status, summary) = call(
        &app,
        Method::GET,
        "/v1/stores/wave54-aggro/rating-summary",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "store summary: {summary}");
    assert_eq!(summary["rating_count"], 3);
    assert_eq!(summary["with_photos_count"], 1);
    assert_eq!(level(&summary, 5)["percent"], 33);
    assert_eq!(level(&summary, 4)["percent"], 33);
    assert_eq!(level(&summary, 3)["percent"], 33);

    // An unreviewed product: null average, zeroed levels.
    let (status, summary) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_c}/rating-summary"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "empty summary: {summary}");
    assert!(summary["rating_avg"].is_null());
    assert_eq!(summary["rating_count"], 0);
    assert_eq!(level(&summary, 5)["percent"], 0);

    // Review sorts on product A.
    let (status, list) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_a}/reviews?sort=highest"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "highest: {list}");
    assert_eq!(list["items"][0]["rating"], 5);
    assert_eq!(list["items"][1]["rating"], 3);
    let (status, list) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_a}/reviews?sort=lowest"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "lowest: {list}");
    assert_eq!(list["items"][0]["rating"], 3);
    assert_eq!(list["items"][1]["rating"], 5);
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_a}/reviews?sort=banana"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad sort: {body}");

    // The store's all-reviews list: every review, product
    // titles attached, photos sort puts the photographed
    // review first, pagination keeps the total honest.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/stores/wave54-aggro/reviews?sort=photos",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "store reviews: {list}");
    assert_eq!(list["total"], 3);
    assert_eq!(list["items"][0]["id"], photo_review_id);
    assert_eq!(list["items"][0]["product_title"], "Wave54 Aggro B");
    let (status, page) = call(
        &app,
        Method::GET,
        "/v1/stores/wave54-aggro/reviews?limit=2",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "store reviews page: {page}");
    assert_eq!(page["total"], 3);
    assert_eq!(page["items"].as_array().expect("items").len(), 2);

    // Unknown stores 404 on both new reads.
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/stores/wave54-nope/rating-summary",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, Method::GET, "/v1/stores/wave54-nope/reviews", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
