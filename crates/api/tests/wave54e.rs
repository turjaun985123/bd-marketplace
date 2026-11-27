//! Wave 54 integration tests (part 5, the extension
//! scope): helpful votes on reviews (cast/retract
//! idempotency, the own-review refusal, counts on the
//! review view, the `helpful` sort), the per-follow
//! notification mute (the fan-out honours it, the follow
//! survives), and the question list's answered-first sort
//! + unanswered count.
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

/// The fns in this file share ONE database (the suite's
/// convention); cargo runs one binary's fns on parallel
/// threads, so each fn holds this lock for its whole run —
/// files stay independent, fns inside a file serialise.
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());

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
/// dev-mode app. Returns the app and the pool (the mute
/// test counts notification rows directly).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 54e integration tests");
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
         review_replies, reviews, review_helpful_votes, shipment_events, shipments, \
         payment_events, payments, order_items, sub_orders, orders, cart_items, \
         carts, product_images, product_variants, products, store_members, \
         store_follows, store_collections, store_collection_products, \
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
    Some((build_app(state), pool))
}

#[tokio::test]
async fn wave54_helpful_votes_and_helpful_sort() {
    let _db_guard = DB_LOCK.lock().await;
    let Some((app, _pool)) = test_app().await else {
        return;
    };

    let seller = login(&app, "01710000071").await;
    let store_id = create_store(&app, &seller, "wave54-helpful").await;
    let product = create_product(&app, &seller, &store_id, "Wave54 Helpful Shoes", 40_000, 50).await;

    let author = login(&app, "01710000072").await;
    let other_author = login(&app, "01710000073").await;
    let voter = login(&app, "01710000074").await;
    buy_and_deliver(&app, &author, &seller, &product).await;
    buy_and_deliver(&app, &other_author, &seller, &product).await;
    let (status, review) = post_review(&app, &author, &product, 5, Some("মাপ একদম ঠিক")).await;
    assert_eq!(status, StatusCode::CREATED, "review A: {review}");
    let review_a = review["id"].as_str().expect("review id").to_string();
    assert_eq!(review["helpful_count"], 0);
    let (status, review) = post_review(&app, &other_author, &product, 4, None).await;
    assert_eq!(status, StatusCode::CREATED, "review B: {review}");
    let review_b = review["id"].as_str().expect("review id").to_string();

    // The author cannot vote on their own review.
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/reviews/{review_a}/helpful"),
        None,
        Some(&author),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "own-review vote: {body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("নিজের রিভিউতে"),
        "bilingual refusal: {body}"
    );

    // Voting is idempotent; the state read agrees; the
    // review list carries the count.
    let (status, state) = call(
        &app,
        Method::POST,
        &format!("/v1/reviews/{review_a}/helpful"),
        None,
        Some(&voter),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "vote: {state}");
    assert_eq!(state["helpful"], true);
    assert_eq!(state["helpful_count"], 1);
    let (status, state) = call(
        &app,
        Method::POST,
        &format!("/v1/reviews/{review_a}/helpful"),
        None,
        Some(&voter),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-vote: {state}");
    assert_eq!(state["helpful_count"], 1, "second vote is the same row");
    let (status, state) = call(
        &app,
        Method::GET,
        &format!("/v1/reviews/{review_a}/helpful"),
        None,
        Some(&other_author),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "state read: {state}");
    assert_eq!(state["helpful"], false);
    assert_eq!(state["helpful_count"], 1);

    // The helpful sort leads with the voted review.
    let (status, list) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product}/reviews?sort=helpful"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "helpful sort: {list}");
    assert_eq!(list["items"][0]["id"], review_a);
    assert_eq!(list["items"][0]["helpful_count"], 1);
    assert_eq!(list["items"][1]["id"], review_b);

    // The store's all-reviews list honours the same sort
    // and carries the same count.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/stores/wave54-helpful/reviews?sort=helpful",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "store helpful sort: {list}");
    assert_eq!(list["items"][0]["id"], review_a);
    assert_eq!(list["items"][0]["helpful_count"], 1);

    // Retracting is idempotent too.
    let (status, state) = call(
        &app,
        Method::DELETE,
        &format!("/v1/reviews/{review_a}/helpful"),
        None,
        Some(&voter),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unvote: {state}");
    assert_eq!(state["helpful_count"], 0);
    let (status, state) = call(
        &app,
        Method::DELETE,
        &format!("/v1/reviews/{review_a}/helpful"),
        None,
        Some(&voter),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-unvote: {state}");
    assert_eq!(state["helpful_count"], 0);

    // The helpful endpoints are signed-in only.
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/v1/reviews/{review_a}/helpful"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "state read needs auth");

    // A missing review 404s on both verbs.
    let missing = "00000000-0000-0000-0000-000000000000";
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/reviews/{missing}/helpful"),
        None,
        Some(&voter),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn wave54_follow_mute_and_question_sort() {
    let _db_guard = DB_LOCK.lock().await;
    let Some((app, pool)) = test_app().await else {
        return;
    };

    let seller = login(&app, "01710000081").await;
    let store_id = create_store(&app, &seller, "wave54-mute").await;
    let buyer = login(&app, "01710000082").await;

    // Muting a store you do not follow is a 404.
    let (status, _) = call(
        &app,
        Method::PATCH,
        "/v1/stores/wave54-mute/follow",
        Some(json!({ "notify_new_products": false })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "mute without follow");

    let (status, state) = call(
        &app,
        Method::POST,
        "/v1/stores/wave54-mute/follow",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "follow: {state}");
    assert_eq!(state["notify_new_products"], true);

    // Mute, then publish: no notice is enqueued, but the
    // follow itself (and the feed) still work.
    let (status, state) = call(
        &app,
        Method::PATCH,
        "/v1/stores/wave54-mute/follow",
        Some(json!({ "notify_new_products": false })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mute: {state}");
    assert_eq!(state["following"], true);
    assert_eq!(state["notify_new_products"], false);
    let muted_product = create_product(
        &app,
        &seller,
        &store_id,
        "Wave54 Muted Launch",
        10_000,
        5,
    )
    .await;
    let notices: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM notifications \
         WHERE kind = 'store_new_product' AND phone = '+8801710000082'",
    )
    .fetch_one(&pool)
    .await
    .expect("count notices");
    assert_eq!(notices, 0, "muted follow receives no notice");
    let (status, feed) = call(&app, Method::GET, "/v1/follows/feed", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "feed still flows: {feed}");
    assert_eq!(feed["items"][0]["id"], muted_product);
    let (status, follows) = call(&app, Method::GET, "/v1/follows", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(follows["items"][0]["notify_new_products"], false);

    // Unmute: the next publish notifies again.
    let (status, _) = call(
        &app,
        Method::PATCH,
        "/v1/stores/wave54-mute/follow",
        Some(json!({ "notify_new_products": true })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unmute");
    let _second = create_product(
        &app,
        &seller,
        &store_id,
        "Wave54 Unmuted Launch",
        10_000,
        5,
    )
    .await;
    let notices: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM notifications \
         WHERE kind = 'store_new_product' AND phone = '+8801710000082'",
    )
    .fetch_one(&pool)
    .await
    .expect("count notices");
    assert_eq!(notices, 1, "unmuted follow notified again");

    // Q&A: two questions, one answered. The answered-first
    // sort leads with the answered one even though it is
    // older; the envelope counts the unanswered.
    let asker = login(&app, "01710000083").await;
    let (status, q1) = call(
        &app,
        Method::POST,
        &format!("/v1/products/{muted_product}/questions"),
        Some(json!({ "body": "এটা কি অরিজিনাল?" })),
        Some(&asker),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "ask q1: {q1}");
    let q1_id = q1["id"].as_str().expect("question id").to_string();
    let (status, _q2) = call(
        &app,
        Method::POST,
        &format!("/v1/products/{muted_product}/questions"),
        Some(json!({ "body": "ডেলিভারি কতদিনে পাবো?" })),
        Some(&asker),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "ask q2");
    let (status, answered) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/questions/{q1_id}/answer"),
        Some(json!({ "body": "হ্যাঁ, ১০০% অরিজিনাল।" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "answer q1: {answered}");

    let (status, list) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{muted_product}/questions?sort=answered"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "answered sort: {list}");
    assert_eq!(list["items"][0]["id"], q1_id);
    assert_eq!(list["unanswered_count"], 1);
    assert_eq!(list["total"], 2);
    let (status, list) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{muted_product}/questions"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "newest sort: {list}");
    assert_ne!(list["items"][0]["id"], q1_id, "newest leads with q2");
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{muted_product}/questions?sort=banana"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad question sort: {body}");
}
