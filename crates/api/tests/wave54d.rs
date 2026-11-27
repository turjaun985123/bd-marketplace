//! Wave 54 integration tests (part 4, the extension
//! scope): store collections (seller CRUD + membership
//! rules + public reads), the buyer's notification inbox
//! (list, unread count, mark read / mark all, ownership),
//! the follows feed, store suggestions, and the seller's
//! follower statistics.
//!
//! The small helpers are copied from the Wave 9 test on
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

/// Create a product with an explicit status; returns the product id.
async fn create_product_with_status(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    status_wanted: &str,
) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": 50_000,
            "stock": 10,
            "status": status_wanted
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// The shared setup: migrated, truncated database + a
/// dev-mode app.
async fn test_app() -> Option<Router> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 54d integration tests");
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
         store_collections, store_collection_products, \
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

#[tokio::test]
async fn wave54_store_collections_crud_membership_and_public_reads() {
    let Some(app) = test_app().await else {
        return;
    };

    let seller = login(&app, "01710000051").await;
    let store_id = create_store(&app, &seller, "wave54-shelves").await;
    let other_seller = login(&app, "01710000052").await;
    let other_store_id = create_store(&app, &other_seller, "wave54-other-shelves").await;

    let p1 = create_product_with_status(&app, &seller, &store_id, "Wave54 Shelf One", "active").await;
    let p2 = create_product_with_status(&app, &seller, &store_id, "Wave54 Shelf Two", "active").await;
    let p_draft =
        create_product_with_status(&app, &seller, &store_id, "Wave54 Shelf Draft", "draft").await;
    let foreign = create_product_with_status(
        &app,
        &other_seller,
        &other_store_id,
        "Wave54 Foreign",
        "active",
    )
    .await;

    // Create: validation first (bad slug, missing title),
    // then the happy path, then the slug conflict.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/store/collections",
        Some(json!({ "slug": "Bad Slug", "title": "ঈদ" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad slug: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/store/collections",
        Some(json!({ "slug": "eid-shelf", "title": "  " })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "blank title: {body}");
    let (status, created) = call(
        &app,
        Method::POST,
        "/v1/seller/store/collections",
        Some(json!({
            "slug": "eid-shelf",
            "title": "ঈদ কালেকশন",
            "description": "উৎসবের বাছাই"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create collection: {created}");
    let collection_id = created["id"].as_str().expect("collection id").to_string();
    assert_eq!(created["product_count"], 0);
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/store/collections",
        Some(json!({ "slug": "eid-shelf", "title": "আবার ঈদ" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "slug conflict: {body}");

    // Membership: a foreign product refuses the whole
    // replace; duplicates collapse; order is preserved.
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/collections/{collection_id}/products"),
        Some(json!({ "product_ids": [p1, foreign] })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "foreign product: {body}");
    let (status, updated) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/collections/{collection_id}/products"),
        Some(json!({ "product_ids": [p2, p1, p2, p_draft] })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set products: {updated}");
    assert_eq!(updated["product_count"], 3, "seller counts all members: {updated}");

    // The seller detail lists members in position order.
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/store/collections/{collection_id}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(
        detail["product_ids"],
        json!([p2, p1, p_draft]),
        "deduped, click order kept: {detail}"
    );

    // Another seller cannot read or mutate the collection.
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/store/collections/{collection_id}"),
        None,
        Some(&other_seller),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign detail hidden");
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/collections/{collection_id}/products"),
        Some(json!({ "product_ids": [] })),
        Some(&other_seller),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign replace hidden");

    // Public reads: the list counts ACTIVE members only,
    // and the collection page serves them in the seller's
    // position order (draft invisible).
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/stores/wave54-shelves/collections",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public list: {list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["product_count"], 2, "public counts active: {list}");
    let (status, page) = call(
        &app,
        Method::GET,
        "/v1/stores/wave54-shelves/collections/eid-shelf",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public page: {page}");
    assert_eq!(page["total"], 2);
    assert_eq!(page["products"][0]["id"], p2);
    assert_eq!(page["products"][1]["id"], p1);

    // Update merges (title only), delete removes the shelf
    // and its public page 404s; the products survive.
    let (status, updated) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/collections/{collection_id}"),
        Some(json!({ "title": "ঈদ কালেকশন ২০২৬" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update: {updated}");
    assert_eq!(updated["title"], "ঈদ কালেকশন ২০২৬");
    assert_eq!(updated["slug"], "eid-shelf");
    let (status, _) = call(
        &app,
        Method::DELETE,
        &format!("/v1/seller/store/collections/{collection_id}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete collection");
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/stores/wave54-shelves/collections/eid-shelf",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "deleted shelf 404s");
    let (status, product) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{p1}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "product survives: {product}");
}

#[tokio::test]
async fn wave54_inbox_feed_suggestions_and_follower_stats() {
    let Some(app) = test_app().await else {
        return;
    };

    let seller = login(&app, "01710000061").await;
    let store_id = create_store(&app, &seller, "wave54-inbox-shop").await;
    let buyer = login(&app, "01710000062").await;
    let stranger = login(&app, "01710000063").await;

    // Before any follow: the feed is empty and the shop is
    // suggested to the buyer (not to its owner).
    let (status, feed) = call(&app, Method::GET, "/v1/follows/feed", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "empty feed: {feed}");
    assert_eq!(feed["total"], 0);
    let (status, suggested) = call(
        &app,
        Method::GET,
        "/v1/follows/suggested",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suggestions: {suggested}");
    assert_eq!(suggested["items"][0]["slug"], "wave54-inbox-shop");
    let (status, suggested) = call(
        &app,
        Method::GET,
        "/v1/follows/suggested",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(suggested["total"], 0, "own store never suggested: {suggested}");

    // Follow, then publish: the feed gains the product,
    // the suggestion disappears, and the inbox gains the
    // notice — unread.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/stores/wave54-inbox-shop/follow",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let product_id = create_product_with_status(
        &app,
        &seller,
        &store_id,
        "Wave54 Inbox Shoes",
        "active",
    )
    .await;
    let (status, feed) = call(&app, Method::GET, "/v1/follows/feed", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "feed: {feed}");
    assert_eq!(feed["total"], 1);
    assert_eq!(feed["items"][0]["id"], product_id);
    let (status, suggested) = call(
        &app,
        Method::GET,
        "/v1/follows/suggested",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(suggested["total"], 0, "followed store not suggested: {suggested}");

    let (status, inbox) = call(&app, Method::GET, "/v1/notifications", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "inbox: {inbox}");
    assert_eq!(inbox["total"], 1);
    assert_eq!(inbox["unread_count"], 1);
    assert_eq!(inbox["items"][0]["kind"], "store_new_product");
    assert!(inbox["items"][0]["read_at"].is_null());
    let notice_id = inbox["items"][0]["id"].as_str().expect("notice id").to_string();

    // A stranger's inbox is empty, and they cannot mark
    // the buyer's notice read (404 — not in their inbox).
    let (status, inbox) = call(&app, Method::GET, "/v1/notifications", None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inbox["total"], 0);
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/notifications/{notice_id}/read"),
        None,
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "foreign notice not markable");

    // Mark read: the row returns read; a second mark keeps
    // the first read time; the unread count drops.
    let (status, marked) = call(
        &app,
        Method::POST,
        &format!("/v1/notifications/{notice_id}/read"),
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mark read: {marked}");
    let first_read_at = marked["read_at"].clone();
    assert!(!first_read_at.is_null());
    let (status, marked) = call(
        &app,
        Method::POST,
        &format!("/v1/notifications/{notice_id}/read"),
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marked["read_at"], first_read_at, "first read time stands");
    let (status, inbox) = call(&app, Method::GET, "/v1/notifications", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inbox["unread_count"], 0);

    // A second notice arrives; mark-all clears it.
    let _second = create_product_with_status(
        &app,
        &seller,
        &store_id,
        "Wave54 Inbox Shoes Two",
        "active",
    )
    .await;
    let (status, inbox) = call(&app, Method::GET, "/v1/notifications", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inbox["unread_count"], 1);
    let (status, result) = call(
        &app,
        Method::PUT,
        "/v1/notifications",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mark all: {result}");
    assert_eq!(result["marked_read"], 1);
    let (status, inbox) = call(&app, Method::GET, "/v1/notifications", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inbox["unread_count"], 0);
    assert_eq!(inbox["total"], 2);

    // The seller's follower stats: one follower, gained
    // today (the series' last day), 30 zero-filled days.
    let (status, stats) = call(
        &app,
        Method::GET,
        "/v1/seller/followers/stats",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "follower stats: {stats}");
    assert_eq!(stats["total"], 1);
    assert_eq!(stats["last_7_days"], 1);
    assert_eq!(stats["last_30_days"], 1);
    let daily = stats["daily"].as_array().expect("daily series");
    assert_eq!(daily.len(), 30);
    assert_eq!(daily[29]["new_followers"], 1, "today's follow: {stats}");
    assert_eq!(daily[0]["new_followers"], 0);

    // A storeless user gets the store read's 404 on stats.
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/seller/followers/stats",
        None,
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "storeless stats");
}
