//! Wave 56 integration test: what the tree counts
//! count. The public tree's product counts follow the
//! catalogue's visibility rule exactly — an ACTIVE
//! product in an ACTIVE store — so:
//! - a draft product linked to a node does not move its
//!   count (and publishing it does);
//! - suspending the store removes its products from
//!   every count until the store is reactivated;
//! - the counts a parent shows are the same visibility
//!   applied to the whole subtree (rollup of visible
//!   products only).
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

/// Find a node by slug inside a built tree payload.
fn find_node<'a>(nodes: &'a [Value], slug: &str) -> Option<&'a Value> {
    for node in nodes {
        if node["slug"] == slug {
            return Some(node);
        }
        if let Some(children) = node["children"].as_array() {
            if let Some(found) = find_node(children, slug) {
                return Some(found);
            }
        }
    }
    None
}

/// A seed node's id, read from the public tree endpoint.
async fn seed_id(app: &Router, slug: &str) -> String {
    let (status, body) = call(app, Method::GET, "/v1/categories/tree", None, None).await;
    assert_eq!(status, StatusCode::OK, "tree: {body}");
    let nodes = body["tree"].as_array().expect("tree nodes");
    find_node(nodes, slug)
        .unwrap_or_else(|| panic!("seed node {slug} in the public tree"))
        ["id"]
        .as_str()
        .expect("node id")
        .to_string()
}

/// A node's current subtree count on the public tree.
async fn tree_count(app: &Router, slug: &str) -> i64 {
    let (status, body) = call(app, Method::GET, "/v1/categories/tree", None, None).await;
    assert_eq!(status, StatusCode::OK, "tree: {body}");
    let nodes = body["tree"].as_array().expect("tree nodes");
    find_node(nodes, slug)
        .unwrap_or_else(|| panic!("node {slug} in the public tree"))["product_count"]
        .as_i64()
        .expect("count")
}

/// Set a store's status as admin.
async fn set_store_status(app: &Router, admin: &str, store_id: &str, status: &str) {
    let (code, body) = call(
        app,
        Method::PATCH,
        &format!("/v1/admin/stores/{store_id}/status"),
        Some(json!({ "status": status })),
        Some(admin),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "store status {status}: {body}");
}

#[tokio::test]
async fn wave56_tree_count_visibility() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let admin = login_admin(&app, &pool, "01711111111").await;
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "w56j-store").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // Kitchen (a Home & Living child) starts empty in
    // this binary: products are truncated per binary, so
    // the counts below are this file's alone.
    let kitchen_id = seed_id(&app, "kitchen").await;
    assert_eq!(tree_count(&app, "kitchen").await, 0);
    assert_eq!(tree_count(&app, "home-living").await, 0);

    // 1. One active linked product: the node counts 1
    // and its parent rolls the same product up.
    create_product(
        &app,
        &seller,
        &store_id,
        "W56J Visible",
        json!({ "category_id": kitchen_id }),
    )
    .await;
    assert_eq!(tree_count(&app, "kitchen").await, 1);
    assert_eq!(tree_count(&app, "home-living").await, 1, "the rollup is visible-only too");

    // 2. A DRAFT linked to the same node changes nothing
    // until it is published.
    let draft = create_product(
        &app,
        &seller,
        &store_id,
        "W56J Draft",
        json!({ "category_id": kitchen_id, "status": "draft" }),
    )
    .await;
    assert_eq!(tree_count(&app, "kitchen").await, 1, "drafts do not count");
    let draft_id = draft["id"].as_str().expect("draft id").to_string();
    let (status, published) = call(
        &app,
        Method::PATCH,
        &format!("/v1/products/{draft_id}"),
        Some(json!({ "status": "active" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "publish the draft: {published}");
    assert_eq!(tree_count(&app, "kitchen").await, 2, "publishing counts it");

    // 3. Suspending the store removes BOTH products from
    // every count; reactivating restores them.
    set_store_status(&app, &admin, &store_id, "suspended").await;
    assert_eq!(tree_count(&app, "kitchen").await, 0, "a suspended store is invisible");
    assert_eq!(tree_count(&app, "home-living").await, 0);
    set_store_status(&app, &admin, &store_id, "active").await;
    assert_eq!(tree_count(&app, "kitchen").await, 2, "reactivation restores the counts");
}
