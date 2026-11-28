//! Wave 57 integration test: bulk-edit category by id
//! and the variant-stranding guard.
//! - category assignment by taxonomy id links the
//!   product and syncs its label (the update path's
//!   rule), reported as a category change;
//! - a price cut that would strand a variant's final
//!   price at zero is a per-item error through the bulk
//!   path too (the shared update path's guard), and the
//!   product's price is untouched;
//! - an unknown category slug in a bulk edit is a
//!   whole-request 400 (one pick for the batch — a
//!   request-shape error, not per-item data).
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

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 57 integration tests");
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
         buyer_addresses, bulk_imports, \
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
    let state = AppState::new(Some(pool.clone()), "wave57-test-secret".to_string(), true)
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

/// POST one product (the wave9 shape); returns its id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i32,
    status: &str,
) -> String {
    let (code, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": status
        })),
        Some(token),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "create {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
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
        .unwrap_or_else(|| panic!("seed node {slug} in the public tree"))["id"]
        .as_str()
        .expect("node id")
        .to_string()
}

#[tokio::test]
async fn wave57r_bulk_edit_category_by_id_and_variant_stranding_guard() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-edit-deep").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // Category by id: the saree node, by its tree id.
    let plain = create_product(&app, &seller, &store_id, "Plain", 30_000, 4, "active").await;
    let saree_id = seed_id(&app, "saree").await;
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({
            "product_ids": [plain],
            "category": { "category_id": saree_id }
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "by id: {report}");
    assert_eq!(report["outcomes"][0]["changed"], json!(["category"]));
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{plain}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["category_id"], json!(saree_id), "{detail}");
    assert_eq!(detail["category"], json!("Saree"), "label synced: {detail}");

    // The stranding guard: base 100_000 with a variant at
    // delta -60_000 (final 40_000). A bulk set to 50_000
    // would take the variant's final price to -10_000 —
    // refused per-item, price untouched.
    let varied = create_product(&app, &seller, &store_id, "Varied", 100_000, 0, "active").await;
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/products/{varied}/variants"),
        Some(json!({ "name": "Small", "price_delta_poisha": -60_000, "stock": 2 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "variant: {body}");
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({
            "product_ids": [varied],
            "price": { "mode": "set", "price_poisha": 50_000 }
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stranding: {report}");
    assert_eq!(report["failed"], json!(1), "{report}");
    let price: i64 = sqlx::query_scalar("SELECT price_poisha FROM products WHERE id = $1::uuid")
        .bind(&varied)
        .fetch_one(&pool)
        .await
        .expect("price");
    assert_eq!(price, 100_000, "the guarded price is untouched");

    // A legal cut for the same product (to 70_000, final
    // variant price 10_000) applies cleanly.
    let (status, report) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({
            "product_ids": [varied],
            "price": { "mode": "set", "price_poisha": 70_000 }
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "legal cut: {report}");
    assert_eq!(report["succeeded"], json!(1));

    // An unknown category slug is a whole-request 400.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({
            "product_ids": [plain],
            "category": { "category_slug": "no-such-node" }
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown slug: {body}");

    // Two category keys at once is likewise a 400.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/products/bulk-edit",
        Some(json!({
            "product_ids": [plain],
            "category": { "category_slug": "saree", "category_label": "Saree" }
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "two keys refused");
}
