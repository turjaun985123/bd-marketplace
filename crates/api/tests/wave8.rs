//! Wave 8 integration test: reviews, ratings and seller replies —
//! delivered-only eligibility (403 before delivery, 201 after), one
//! review per buyer per product (409 on the second), the live rating
//! summary on the shared product view, author-only edit/delete,
//! store-owner-only replies (404 for other sellers, 409 on a
//! duplicate), and the public list showing masked buyer phones plus
//! the reply. A delivered order is produced by driving the real
//! Wave 6 flow (COD checkout -> seller confirms -> DEV courier
//! shipment -> refresh until delivered); the small helpers are
//! copied from the Wave 6 test on purpose, since integration test
//! files stay self-contained.
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
            "delivery_phone": "01988888888",
            "delivery_address": "House 8, Road 2, Chittagong",
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
async fn buy_and_deliver(
    app: &Router,
    buyer: &str,
    seller: &str,
    product_id: &str,
) -> String {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
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
    for expected in ["picked_up", "in_transit", "out_for_delivery", "delivered"] {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, shipment) = call(app, Method::POST, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh: {shipment}");
        assert_eq!(shipment["status"], expected, "refresh step: {shipment}");
    }
    order_id
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

/// The product view (carries the rating summary).
async fn get_product(app: &Router, product_id: &str) -> Value {
    let uri = format!("/v1/products/{product_id}");
    let (status, body) = call(app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "get product: {body}");
    body
}

/// POST a seller reply to a review as the given user.
async fn post_reply(
    app: &Router,
    token: &str,
    review_id: &str,
    body: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/reviews/{review_id}/reply");
    call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "body": body })),
        Some(token),
    )
    .await
}

#[tokio::test]
async fn wave8_reviews_ratings_replies_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 8 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE review_replies, reviews, shipment_events, shipments, \
         payment_events, payments, order_items, sub_orders, orders, \
         cart_items, carts, products, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode (deliveries) on.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave8-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // One seller + store + product; a second seller (with a store of
    // their own) who must NOT be able to reply; two buyers.
    let seller = login(&app, "01788888888").await;
    let store = create_store(&app, &seller, "wave8-store").await;
    let product = create_product(&app, &seller, &store, "Wave8 Product", 40_000, 10).await;
    let other_seller = login(&app, "01588888888").await;
    create_store(&app, &other_seller, "wave8-other-store").await;
    let buyer1 = login(&app, "01988888888").await;
    let buyer2 = login(&app, "01688888888").await;

    // 1. Fresh product: the summary is honest about having no reviews.
    let view = get_product(&app, &product).await;
    assert!(view["rating_avg"].is_null(), "no reviews yet: {view}");
    assert_eq!(view["rating_count"], 0);

    // 2. Buyer 1 orders but has NOT received anything yet: a review
    // attempt is a 403 with the eligibility message.
    add_to_cart(&app, &buyer1, &product, 1).await;
    let order = checkout_cod(&app, &buyer1).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = post_review(&app, &buyer1, &product, 5, Some("আগেই রিভিউ!")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "review before delivery: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("only products you received"),
        "eligibility message: {body}"
    );

    // Drive buyer 1's order to delivered (confirm, book, refresh x4).
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

    // 3. An out-of-range rating is a 400 even for an eligible buyer.
    let (status, body) = post_review(&app, &buyer1, &product, 6, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "rating 6: {body}");

    // 4. After delivery the review lands: 201, and the summary on the
    // product view shows 5.0 over 1 review. The buyer has no name on
    // file, so the display is their masked phone.
    let (status, review) = post_review(&app, &buyer1, &product, 5, Some("খুব ভালো পণ্য")).await;
    assert_eq!(status, StatusCode::CREATED, "create review: {review}");
    assert_eq!(review["rating"], 5);
    assert_eq!(review["body"], "খুব ভালো পণ্য");
    assert_eq!(review["buyer_display"], "019****888");
    assert!(review["reply"].is_null(), "no reply yet: {review}");
    let review1_id = review["id"].as_str().expect("review id").to_string();

    let view = get_product(&app, &product).await;
    assert_eq!(view["rating_avg"], 5.0, "summary after one review: {view}");
    assert_eq!(view["rating_count"], 1);

    // 5. One review per buyer per product: the second create is a 409.
    let (status, body) = post_review(&app, &buyer1, &product, 4, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "duplicate review: {body}");

    // 6. Buyer 2 receives the product too and rates it 3: the summary
    // becomes 4.0 over 2 reviews.
    buy_and_deliver(&app, &buyer2, &seller, &product).await;
    let (status, review2) = post_review(&app, &buyer2, &product, 3, None).await;
    assert_eq!(status, StatusCode::CREATED, "buyer2 review: {review2}");
    let review2_id = review2["id"].as_str().expect("review id").to_string();
    let view = get_product(&app, &product).await;
    assert_eq!(view["rating_avg"], 4.0, "summary after two reviews: {view}");
    assert_eq!(view["rating_count"], 2);

    // 7. The author edits their own review (5 -> 4): the summary
    // follows to 3.5. Another buyer editing it gets a 404.
    let uri = format!("/v1/reviews/{review1_id}");
    let (status, updated) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "rating": 4 })),
        Some(&buyer1),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "edit own review: {updated}");
    assert_eq!(updated["rating"], 4);
    let view = get_product(&app, &product).await;
    assert_eq!(view["rating_avg"], 3.5, "summary after edit: {view}");

    let uri = format!("/v1/reviews/{review1_id}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "rating": 1 })),
        Some(&buyer2),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "edit other's review: {body}");

    // 8. Replies: another seller cannot reply (404); the owning
    // seller can (201); a second reply is a 409; the owner can edit.
    let (status, body) = post_reply(&app, &other_seller, &review1_id, "আমার দোকানের নয়").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "reply by other seller: {body}");

    let (status, reply) = post_reply(&app, &seller, &review1_id, "ধন্যবাদ!").await;
    assert_eq!(status, StatusCode::CREATED, "seller reply: {reply}");
    assert_eq!(reply["body"], "ধন্যবাদ!");

    let (status, body) = post_reply(&app, &seller, &review1_id, "আবার উত্তর").await;
    assert_eq!(status, StatusCode::CONFLICT, "duplicate reply: {body}");

    let uri = format!("/v1/seller/reviews/{review1_id}/reply");
    let (status, reply) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "body": "অনেক ধন্যবাদ!" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "edit reply: {reply}");
    assert_eq!(reply["body"], "অনেক ধন্যবাদ!");

    // 9. The public list shows both reviews, newest first, with the
    // masked buyer display and the reply attached to review 1.
    let uri = format!("/v1/products/{product}/reviews");
    let (status, list) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "list reviews: {list}");
    assert_eq!(list["total"], 2);
    let items = list["items"].as_array().expect("review items");
    assert_eq!(items.len(), 2);
    let listed1 = items
        .iter()
        .find(|r| r["id"] == review1_id)
        .expect("review 1 listed");
    assert_eq!(listed1["buyer_display"], "019****888");
    assert_eq!(listed1["reply"]["body"], "অনেক ধন্যবাদ!");
    let listed2 = items
        .iter()
        .find(|r| r["id"] == review2_id)
        .expect("review 2 listed");
    assert_eq!(listed2["buyer_display"], "016****888");
    assert!(listed2["reply"].is_null());

    // 10. The author deletes their review: 204, the reply goes with
    // it, and the summary drops to buyer 2's lone 3-star review.
    let uri = format!("/v1/reviews/{review1_id}");
    let (status, _) = call(&app, Method::DELETE, &uri, None, Some(&buyer1)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "delete own review");

    let view = get_product(&app, &product).await;
    assert_eq!(view["rating_avg"], 3.0, "summary after delete: {view}");
    assert_eq!(view["rating_count"], 1);

    // Deleting someone else's review is a 404 and changes nothing.
    let uri = format!("/v1/reviews/{review2_id}");
    let (status, body) = call(&app, Method::DELETE, &uri, None, Some(&buyer1)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "delete other's review: {body}");
    let view = get_product(&app, &product).await;
    assert_eq!(view["rating_count"], 1);
}
