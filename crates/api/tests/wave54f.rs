//! Wave 54 integration tests (part 6, the extension
//! scope): flash-sale alerts to followers (a product
//! NEWLY joining a LIVE sale notifies unmuted followers
//! once; re-saving the same set is silent; a future sale
//! alerts nobody yet), the admin follower list endpoint
//! (admin-only, masked, 404 for a missing store), and
//! the admin analytics community totals.
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

/// The shared setup: migrated, truncated database + a
/// dev-mode app. Returns the app and the pool (tests
/// promote the admin and count notification rows
/// directly).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 54f integration tests");
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
    let state = AppState::new(Some(pool.clone()), "wave54-test-secret".to_string(), true);
    Some((build_app(state), pool))
}

/// How many notifications of one kind one phone holds.
async fn notice_count(pool: &PgPool, phone: &str, kind: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM notifications WHERE phone = $1 AND kind = $2",
    )
    .bind(phone)
    .bind(kind)
    .fetch_one(pool)
    .await
    .expect("count notices")
}

#[tokio::test]
async fn wave54_flash_sale_alerts_admin_followers_and_analytics() {
    let Some((app, pool)) = test_app().await else {
        return;
    };

    let seller = login(&app, "01710000091").await;
    let store_id = create_store(&app, &seller, "wave54-sale").await;
    let product = create_product(&app, &seller, &store_id, "Wave54 Sale Gadget", 50_000, 20).await;
    let follower = login(&app, "01710000092").await;
    let muted = login(&app, "01710000093").await;
    for token in [&follower, &muted] {
        let (status, _) = call(
            &app,
            Method::POST,
            "/v1/stores/wave54-sale/follow",
            None,
            Some(token),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "follow");
    }
    let (status, _) = call(
        &app,
        Method::PATCH,
        "/v1/stores/wave54-sale/follow",
        Some(json!({ "notify_new_products": false })),
        Some(&muted),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mute one follower");

    // The admin is SQL-promoted (the platform's only
    // promotion path); the token issued before promotion
    // works because require_admin re-reads the row.
    let admin = login(&app, "01710000094").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801710000094'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // A LIVE sale (window straddles now): setting the
    // items alerts the unmuted follower once, with the
    // sale title in the rendered body.
    let (status, sale) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Wave54 Mega Sale",
            "starts_at": "2020-01-01T00:00:00Z",
            "ends_at": "2030-01-01T00:00:00Z",
            "is_active": true
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create sale: {sale}");
    let sale_id = sale["id"].as_str().expect("sale id").to_string();
    let items = json!({
        "items": [{ "product_id": product, "sale_price_poisha": 40_000, "stock_cap": null }]
    });
    let (status, detail) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{sale_id}/items"),
        Some(items.clone()),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set items: {detail}");
    assert_eq!(
        notice_count(&pool, "+8801710000092", "store_flash_sale").await,
        1,
        "unmuted follower alerted"
    );
    assert_eq!(
        notice_count(&pool, "+8801710000093", "store_flash_sale").await,
        0,
        "muted follower not alerted"
    );
    let body: String = sqlx::query_scalar(
        "SELECT body FROM notifications \
         WHERE phone = '+8801710000092' AND kind = 'store_flash_sale'",
    )
    .fetch_one(&pool)
    .await
    .expect("read alert body");
    assert!(body.contains("Wave54 Mega Sale"), "sale title renders: {body}");
    assert!(body.contains("Store wave54-sale"), "store renders: {body}");

    // The alert is equally visible through the follower's
    // own inbox endpoint, kind and unread state included.
    let (status, inbox) = call(
        &app,
        Method::GET,
        "/v1/notifications",
        None,
        Some(&follower),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "inbox: {inbox}");
    assert!(
        inbox["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|row| row["kind"] == "store_flash_sale" && row["read_at"].is_null()),
        "the alert sits unread in the inbox: {inbox}"
    );
    assert!(inbox["unread_count"].as_i64().unwrap_or(0) >= 1);
    // …while the muted follower's inbox holds no such row.
    let (status, inbox) = call(
        &app,
        Method::GET,
        "/v1/notifications",
        None,
        Some(&muted),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "muted inbox: {inbox}");
    assert!(
        inbox["items"]
            .as_array()
            .expect("items")
            .iter()
            .all(|row| row["kind"] != "store_flash_sale"),
        "muted follower's inbox stays silent: {inbox}"
    );

    // Re-saving the SAME set alerts nobody again.
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{sale_id}/items"),
        Some(items),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-set items");
    assert_eq!(
        notice_count(&pool, "+8801710000092", "store_flash_sale").await,
        1,
        "no duplicate alert"
    );

    // A FUTURE sale's items alert nobody at set time.
    let (status, future) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Wave54 Future Sale",
            "starts_at": "2034-01-01T00:00:00Z",
            "ends_at": "2034-02-01T00:00:00Z",
            "is_active": true
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create future sale: {future}");
    let future_id = future["id"].as_str().expect("sale id").to_string();
    let other = create_product(&app, &seller, &store_id, "Wave54 Future Gadget", 30_000, 5).await;
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{future_id}/items"),
        Some(json!({
            "items": [{ "product_id": other, "sale_price_poisha": 25_000, "stock_cap": null }]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set future items");
    assert_eq!(
        notice_count(&pool, "+8801710000092", "store_flash_sale").await,
        1,
        "future sale stays silent"
    );

    // The admin follower list: masked, admin-only, and a
    // missing store 404s.
    let (status, list) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/stores/{store_id}/followers"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin followers: {list}");
    assert_eq!(list["total"], 2);
    let muted_row = list["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|row| row["notify_new_products"] == false)
        .expect("the muted follower is visible as muted");
    assert!(
        muted_row["display"]
            .as_str()
            .unwrap_or_default()
            .contains('*'),
        "display is masked: {muted_row}"
    );
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/stores/{store_id}/followers"),
        None,
        Some(&follower),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "non-admin refused");
    // The seller's own list carries the same mute flags.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/seller/followers",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller followers: {list}");
    assert_eq!(list["total"], 2);
    assert!(
        list["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|row| row["notify_new_products"] == false),
        "the muted follower shows as muted to the seller: {list}"
    );
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/admin/stores/00000000-0000-0000-0000-000000000000/followers",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "missing store 404s");

    // The analytics community totals see the same world.
    let (status, analytics) = call(
        &app,
        Method::GET,
        "/v1/admin/analytics?days=30",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "analytics: {analytics}");
    assert_eq!(analytics["totals"]["community"]["total_follows"], 2);
    assert_eq!(analytics["totals"]["community"]["new_follows"], 2);
    assert_eq!(analytics["totals"]["community"]["muted_follows"], 1);
    assert_eq!(analytics["totals"]["community"]["store_collections"], 0);
}
