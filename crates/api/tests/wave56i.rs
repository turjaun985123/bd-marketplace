//! Wave 56 integration test: taxonomy change semantics
//! over time — the behaviours that only appear when the
//! tree MOVES under existing products.
//! - renaming a category does NOT rewrite labels already
//!   synced onto products (a label re-syncs on the
//!   product's own next write); resolution always uses
//!   CURRENT names, so the old name stops linking and
//!   the new name starts;
//! - retiring a category keeps its products' links (the
//!   path empties on public views) but its slug stops
//!   resolving for subtree browse; reactivating restores
//!   both;
//! - when a create carries BOTH `category_id` and a text
//!   label, the id wins and the label syncs;
//! - a featured root with an early sort order leads the
//!   home feed's taxonomy strip.
//!
//! Gated on `DATABASE_URL`: <redacted>
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

/// Login, THEN promote by SQL (login creates the user row).
async fn login_admin(app: &Router, pool: &PgPool, phone: &str) -> String {
    let token = login(app, phone).await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = $1")
        .bind(format!("+880{}", &phone[1..]))
        .execute(pool)
        .await
        .expect("promote admin");
    token
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 56 integration tests");
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
         buyer_addresses, \
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
    let state = AppState::new(Some(pool.clone()), "wave56-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// POST a store for the token's user; returns the store view.
async fn create_store(app: &Router, token: &str, slug: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body
}

/// POST one product with extra fields merged in; returns the view.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    extra: Value,
) -> Value {
    let mut body = json!({
        "store_id": store_id,
        "title": title,
        "price_poisha": 10_000,
        "stock": 5,
        "status": "active"
    });
    if let (Some(target), Some(source)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    let (status, created) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(body),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {created}");
    created
}

/// POST one category as admin; returns the admin view.
async fn create_category(app: &Router, admin: &str, body: Value) -> Value {
    let (status, created) = call(
        app,
        Method::POST,
        "/v1/admin/categories",
        Some(body),
        Some(admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create category: {created}");
    created
}

/// PATCH one category as admin; returns the admin view.
async fn patch_category(app: &Router, admin: &str, id: &str, body: Value) -> Value {
    let (status, updated) = call(
        app,
        Method::PATCH,
        &format!("/v1/admin/categories/{id}"),
        Some(body),
        Some(admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch category {id}: {updated}");
    updated
}

#[tokio::test]
async fn wave56_taxonomy_change_semantics() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let admin = login_admin(&app, &pool, "01711111111").await;
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "w56i-store").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // A throwaway branch to move around.
    let node = create_category(
        &app,
        &admin,
        json!({ "slug": "w56i-node", "name_en": "W56I Original" }),
    )
    .await;
    let node_id = node["id"].as_str().expect("node id").to_string();

    // 1. The id wins over a simultaneously-carried label:
    // the product files under the node and its label
    // syncs to the node's name, not the typed text.
    let both = create_product(
        &app,
        &seller,
        &store_id,
        "W56I Both Fields",
        json!({ "category_id": node_id, "category": "Some Other Label" }),
    )
    .await;
    assert_eq!(both["category_id"], node_id);
    assert_eq!(both["category"], "W56I Original", "the id syncs the label");
    let both_id = both["id"].as_str().expect("product id").to_string();

    // 2. Rename the node: the product's stored label does
    // NOT move (labels re-sync on the product's own
    // writes, never by side effect)...
    patch_category(
        &app,
        &admin,
        &node_id,
        json!({ "name_en": "W56I Renamed" }),
    )
    .await;
    let (status, after) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{both_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "product after rename: {after}");
    assert_eq!(after["category"], "W56I Original");
    assert_eq!(after["category_id"], node_id, "the link survives");

    // ...but RESOLUTION uses current names: a new product
    // labelled with the OLD name no longer links, and one
    // labelled with the NEW name does.
    let stale = create_product(
        &app,
        &seller,
        &store_id,
        "W56I Stale Label",
        json!({ "category": "W56I Original" }),
    )
    .await;
    assert!(stale["category_id"].is_null(), "old name no longer resolves");
    let fresh = create_product(
        &app,
        &seller,
        &store_id,
        "W56I Fresh Label",
        json!({ "category": "W56I Renamed" }),
    )
    .await;
    assert_eq!(fresh["category_id"], node_id, "new name resolves");

    // 3. Retire the node: the slug stops resolving for
    // browse (total 0), while the linked product keeps
    // its link on the seller's own list (the link is a
    // fact about the product, not about the node's
    // visibility).
    let (status, scoped) = call(
        &app,
        Method::GET,
        "/v1/search?category_slug=w56i-node",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "browse before retire: {scoped}");
    assert_eq!(scoped["total"], 2, "both linked products browse");
    patch_category(
        &app,
        &admin,
        &node_id,
        json!({ "is_active": false }),
    )
    .await;
    let (status, scoped) = call(
        &app,
        Method::GET,
        "/v1/search?category_slug=w56i-node",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "browse after retire: {scoped}");
    assert_eq!(scoped["total"], 0, "a retired slug resolves to nothing");
    let (status, mine) = call(
        &app,
        Method::GET,
        "/v1/stores/w56i-store/products",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller list: {mine}");
    let kept = mine["items"]
        .as_array()
        .expect("seller items")
        .iter()
        .find(|item| item["id"] == both_id)
        .expect("the product is still listed");
    assert_eq!(kept["category_id"], node_id, "the link is kept");

    // Reactivating restores subtree browse.
    patch_category(&app, &admin, &node_id, json!({ "is_active": true })).await;
    let (status, scoped) = call(
        &app,
        Method::GET,
        "/v1/search?category_slug=w56i-node",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "browse after restore: {scoped}");
    assert_eq!(scoped["total"], 2, "reactivation restores the subtree");

    // 4. A featured root with an early sort order leads
    // the home strip (featured first, display order
    // inside the group) — ahead of the seed's featured
    // pair.
    create_category(
        &app,
        &admin,
        json!({
            "slug": "w56i-featured",
            "name_en": "W56I Featured",
            "sort_order": 5,
            "is_featured": true
        }),
    )
    .await;
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "feed: {feed}");
    let taxonomy = feed["taxonomy"].as_array().expect("taxonomy strip");
    assert_eq!(taxonomy[0]["slug"], "w56i-featured", "featured + order: {taxonomy:?}");
    assert_eq!(taxonomy[1]["slug"], "fashion");
    assert_eq!(taxonomy[2]["slug"], "electronics");
}
