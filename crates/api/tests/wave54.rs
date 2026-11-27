//! Wave 54 integration tests (part 1): store follows —
//! follow/unfollow idempotency, the own-store refusal, the
//! follower count + viewer follow state on the public
//! store view, the buyer's /v1/follows list (newest
//! products riding along, suspended stores hidden), the
//! seller's followers list, the buyer export's
//! followed_stores section, and the new-product
//! notification the outbox gains for followers when a
//! followed store's product becomes active (create-active,
//! draft-to-active, and NOT on a plain edit).
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
/// dev-mode app. Returns the app and the pool (the tests
/// read the notifications table directly).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 54 integration tests");
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
    Some((build_app(state), pool))
}

/// Count the store_new_product notification rows for one phone.
async fn new_product_notice_count(pool: &PgPool, phone: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::bigint FROM notifications \
         WHERE kind = 'store_new_product' AND phone = $1",
    )
    .bind(phone)
    .fetch_one(pool)
    .await
    .expect("count notices")
}

#[tokio::test]
async fn wave54_follow_flow_counts_and_lists() {
    let Some((app, pool)) = test_app().await else {
        return;
    };

    let seller = login(&app, "01710000001").await;
    let store_id = create_store(&app, &seller, "wave54-shop").await;
    let product_id =
        create_product_with_status(&app, &seller, &store_id, "Wave54 Follow Shoes", "active")
            .await;
    let buyer = login(&app, "01710000002").await;

    // Following requires an account.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/stores/wave54-shop/follow",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous follow refused");

    // The owner cannot follow their own store (bilingual 400).
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/wave54-shop/follow",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "own-store follow: {body}");
    let message = body["error"].as_str().unwrap_or_default().to_string();
    assert!(
        message.contains("নিজের স্টোর"),
        "bilingual self-follow error: {message}"
    );

    // An unknown store is a 404, follow or not.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/stores/wave54-nope/follow",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown store follow");

    // Follow: the response is the store's new follow state.
    let (status, state) = call(
        &app,
        Method::POST,
        "/v1/stores/wave54-shop/follow",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "follow: {state}");
    assert_eq!(state["following"], true);
    assert_eq!(state["follower_count"], 1);

    // Following again changes nothing (the PK is the guard).
    let (status, state) = call(
        &app,
        Method::POST,
        "/v1/stores/wave54-shop/follow",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-follow: {state}");
    assert_eq!(state["follower_count"], 1);

    // The public store view: anonymous sees the count but
    // not a follow state; the follower sees their own.
    let (status, view) = call(&app, Method::GET, "/v1/stores/wave54-shop", None, None).await;
    assert_eq!(status, StatusCode::OK, "public store: {view}");
    assert_eq!(view["follower_count"], 1);
    assert_eq!(view["is_following"], false);
    let (status, view) = call(
        &app,
        Method::GET,
        "/v1/stores/wave54-shop",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "follower store view: {view}");
    assert_eq!(view["is_following"], true);

    // The buyer's follows list carries the store, its
    // product count and its newest products.
    let (status, follows) = call(&app, Method::GET, "/v1/follows", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "follows: {follows}");
    assert_eq!(follows["total"], 1);
    let followed = &follows["items"][0];
    assert_eq!(followed["slug"], "wave54-shop");
    assert_eq!(followed["follower_count"], 1);
    assert_eq!(followed["product_count"], 1);
    assert_eq!(followed["newest_products"][0]["id"], product_id);

    // The seller's own followers list names the follower
    // (masked display) and counts them.
    let (status, seller_view) =
        call(&app, Method::GET, "/v1/seller/followers", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "seller followers: {seller_view}");
    assert_eq!(seller_view["total"], 1);
    assert!(
        seller_view["items"][0]["display"]
            .as_str()
            .unwrap_or_default()
            .contains('*'),
        "follower display is masked: {seller_view}"
    );

    // A user with no store gets a 404 on the seller list.
    let (status, _) = call(&app, Method::GET, "/v1/seller/followers", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "storeless seller followers");

    // The buyer export carries the follow (data portability).
    let (status, export) = call(&app, Method::GET, "/v1/buyer/export", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer export: {export}");
    assert_eq!(export["followed_stores"][0]["slug"], "wave54-shop");

    // Unfollow is idempotent too.
    let (status, state) = call(
        &app,
        Method::DELETE,
        "/v1/stores/wave54-shop/follow",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unfollow: {state}");
    assert_eq!(state["following"], false);
    assert_eq!(state["follower_count"], 0);
    let (status, state) = call(
        &app,
        Method::DELETE,
        "/v1/stores/wave54-shop/follow",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-unfollow: {state}");
    assert_eq!(state["follower_count"], 0);
    let (status, follows) = call(&app, Method::GET, "/v1/follows", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(follows["total"], 0);

    // A suspended store leaves the follows list (the follow
    // row itself survives — re-follow below proves the
    // state after reinstatement).
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/stores/wave54-shop/follow",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801710000003'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let admin = login(&app, "01710000003").await;
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/stores/{store_id}/status"),
        Some(json!({ "status": "suspended" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suspend store: {body}");
    let (status, follows) = call(&app, Method::GET, "/v1/follows", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(follows["total"], 0, "suspended store hidden: {follows}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/stores/{store_id}/status"),
        Some(json!({ "status": "active" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "reinstate store: {body}");
    let (status, follows) = call(&app, Method::GET, "/v1/follows", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(follows["total"], 1, "reinstated store returns: {follows}");
}

#[tokio::test]
async fn wave54_new_product_notifications_reach_followers() {
    let Some((app, pool)) = test_app().await else {
        return;
    };

    let seller = login(&app, "01710000011").await;
    let store_id = create_store(&app, &seller, "wave54-notify").await;
    let follower_a = login(&app, "01710000012").await;
    let follower_b = login(&app, "01710000013").await;
    // (Phone …014 never logs in or follows: the bystander
    // whose notice count must stay zero.)
    for token in [&follower_a, &follower_b] {
        let (status, _) = call(
            &app,
            Method::POST,
            "/v1/stores/wave54-notify/follow",
            None,
            Some(token),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "follow before publish");
    }

    // A product born active notifies both followers, once
    // each, and nobody else.
    let live_id = create_product_with_status(
        &app,
        &seller,
        &store_id,
        "Wave54 Notify Live Shoes",
        "active",
    )
    .await;
    assert_eq!(new_product_notice_count(&pool, "+8801710000012").await, 1);
    assert_eq!(new_product_notice_count(&pool, "+8801710000013").await, 1);
    assert_eq!(new_product_notice_count(&pool, "+8801710000014").await, 0);

    // The rendered body names the store and the product.
    let body: String = sqlx::query_scalar(
        "SELECT body FROM notifications WHERE kind = 'store_new_product' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("notice body");
    assert!(
        body.contains("Store wave54-notify"),
        "body names the store: {body}"
    );
    assert!(
        body.contains("Wave54 Notify Live Shoes"),
        "body names the product: {body}"
    );

    // A draft notifies nobody…
    let draft_id = create_product_with_status(
        &app,
        &seller,
        &store_id,
        "Wave54 Notify Draft Shoes",
        "draft",
    )
    .await;
    assert_eq!(new_product_notice_count(&pool, "+8801710000012").await, 1);

    // …until it is published (draft -> active).
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/products/{draft_id}"),
        Some(json!({ "status": "active" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "publish draft: {body}");
    assert_eq!(new_product_notice_count(&pool, "+8801710000012").await, 2);
    assert_eq!(new_product_notice_count(&pool, "+8801710000013").await, 2);

    // Editing an already-live product never re-notifies.
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/products/{live_id}"),
        Some(json!({ "title": "Wave54 Notify Live Shoes Renamed" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "edit live product: {body}");
    assert_eq!(new_product_notice_count(&pool, "+8801710000012").await, 2);
}
