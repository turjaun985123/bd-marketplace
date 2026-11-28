//! Wave 60 integration test: the access rules are the
//! deliberate ones from the module doc — issuing and
//! freeze/unfreeze are owner-level (`manage_payouts`: a
//! manager who runs the shop day to day still cannot mint
//! store liability), reads are `view_finances` (owner +
//! manager, not staff), and a total stranger gets the
//! uniform 404 on a card detail so card ids cannot be
//! probed across stores.
//!
//! Gated on `DATABASE_URL`: skips cleanly without one.

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

/// Full dev-mode OTP login; returns (JWT, user id).
async fn login_with_id(app: &Router, phone: &str) -> (String, String) {
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
    let token = body["token"].as_str().expect("token issued").to_string();
    let user_id = body["user"]["id"]
        .as_str()
        .expect("user id")
        .to_string();
    (token, user_id)
}

/// Full dev-mode OTP login for a phone number; returns the JWT.
async fn login(app: &Router, phone: &str) -> String {
    login_with_id(app, phone).await.0
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 60 integration tests");
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
         buyer_addresses, bulk_imports, store_layouts, storefront_events, \
         store_pages, store_posts, store_post_events, \
         store_collections, store_collection_products, \
         product_images, product_variants, products, store_members, store_follows, \
         gift_card_ledger, gift_cards, \
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
    let state = AppState::new(Some(pool.clone()), "wave60-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// POST a store for the token's user; returns the store id.
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

/// Seat a user in a store with a role, directly in SQL
/// (the member-management API is Wave 34's surface; this
/// test is about what the roles may DO with gift cards).
async fn seat_member(pool: &PgPool, store_id: &str, user_id: &str, role: &str) {
    sqlx::query("INSERT INTO store_members (store_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(sqlx::types::Uuid::parse_str(store_id).expect("store uuid"))
        .bind(sqlx::types::Uuid::parse_str(user_id).expect("user uuid"))
        .bind(role)
        .execute(pool)
        .await
        .expect("seat member");
}

#[tokio::test]
async fn wave60_access_rules_owner_issues_manager_reads_stranger_404() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let owner = login(&app, "01711111111").await;
    let (manager, manager_id) = login_with_id(&app, "01722222222").await;
    let (staff, staff_id) = login_with_id(&app, "01733333333").await;
    let stranger = login(&app, "01744444444").await;
    let store_id = create_store(&app, &owner, "w60h-store").await;
    seat_member(&pool, &store_id, &manager_id, "manager").await;
    seat_member(&pool, &store_id, &staff_id, "staff").await;

    // The owner issues; the card exists.
    let (status, issued) = call(
        &app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": 30_000 })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "owner issue: {issued}");
    let card_id = issued["cards"][0]["id"]
        .as_str()
        .expect("card id")
        .to_string();

    // The MANAGER can read — list, detail, stats — because
    // finances are visible to managers…
    let uri = format!("/v1/seller/gift-cards?store_id={store_id}");
    let (status, list) = call(&app, Method::GET, &uri, None, Some(&manager)).await;
    assert_eq!(status, StatusCode::OK, "manager list: {list}");
    assert_eq!(list["total"], 1);
    let uri = format!("/v1/seller/gift-cards/{card_id}");
    let (status, detail) = call(&app, Method::GET, &uri, None, Some(&manager)).await;
    assert_eq!(status, StatusCode::OK, "manager detail: {detail}");
    assert_eq!(detail["balance_poisha"], 30_000);
    let uri = format!("/v1/seller/gift-card-stats?store_id={store_id}");
    let (status, stats) = call(&app, Method::GET, &uri, None, Some(&manager)).await;
    assert_eq!(status, StatusCode::OK, "manager stats: {stats}");
    assert_eq!(stats["cards_total"], 1);

    // …but cannot ISSUE (minting liability is moving money,
    // owner-level like a payout request)…
    let uri = format!("/v1/seller/gift-cards?store_id={store_id}");
    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "amount_poisha": 30_000 })),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "manager issue: {body}");
    // …and cannot freeze or unfreeze either.
    let uri = format!("/v1/seller/gift-cards/{card_id}/disable");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&manager)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "manager disable: {body}");

    // STAFF cannot even read the finances: list and detail
    // are both 403.
    let uri = format!("/v1/seller/gift-cards?store_id={store_id}");
    let (status, body) = call(&app, Method::GET, &uri, None, Some(&staff)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "staff list: {body}");
    let uri = format!("/v1/seller/gift-cards/{card_id}");
    let (status, body) = call(&app, Method::GET, &uri, None, Some(&staff)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "staff detail: {body}");

    // A STRANGER gets the uniform 404 on the detail — the
    // card's existence is not leaked across stores — and a
    // 404 (not an empty list) on the scoped list too, the
    // access module's stranger convention.
    let uri = format!("/v1/seller/gift-cards/{card_id}");
    let (status, body) = call(&app, Method::GET, &uri, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger detail: {body}");
    let uri = format!("/v1/seller/gift-cards?store_id={store_id}");
    let (status, body) = call(&app, Method::GET, &uri, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger list: {body}");

    // And the owner's own disable works, proving the 403s
    // above were role decisions, not a broken endpoint.
    let uri = format!("/v1/seller/gift-cards/{card_id}/disable");
    let (status, view) = call(&app, Method::POST, &uri, None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK, "owner disable: {view}");
    assert_eq!(view["status"], "disabled");
}
