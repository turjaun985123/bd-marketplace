//! Wave 58 integration test: resolution follows the
//! catalogue's CURRENT state, not the layout's save
//! time:
//! - a manual rail saved with a duplicated product id
//!   stores it once (the domain's dedupe, visible in
//!   the seller view);
//! - a single-collection shelf renders while its
//!   collection has an active member, and drops once
//!   that member is drafted — the collection still
//!   exists, the shelf simply has nothing honest to
//!   show;
//! - re-activating the member restores the shelf with
//!   no layout write in between.
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
            eprintln!("DATABASE_URL not set — skipping Wave 58 integration tests");
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
         store_collections, store_collection_products, \
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
    let state = AppState::new(Some(pool.clone()), "wave58-test-secret".to_string(), true)
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

/// POST one active product; returns its id.
async fn create_product(app: &Router, token: &str, store_id: &str, title: &str) -> String {
    let (status, created) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": 80_000,
            "stock": 5,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {created}");
    created["id"].as_str().expect("product id").to_string()
}

/// The ids of a layout body's sections, in order.
fn section_ids(body: &Value) -> Vec<String> {
    body["sections"]
        .as_array()
        .expect("sections array")
        .iter()
        .map(|section| section["id"].as_str().expect("section id").to_string())
        .collect()
}

/// Find one section in a layout body by id.
fn section_by_id<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["sections"]
        .as_array()?
        .iter()
        .find(|section| section["id"].as_str() == Some(id))
}

/// Set one product's status directly (the status walk
/// itself is the products module's tests' business).
async fn set_product_status(pool: &PgPool, product_id: &str, status: &str) {
    sqlx::query("UPDATE products SET status = $2 WHERE id = $1::uuid")
        .bind(product_id)
        .bind(status)
        .execute(pool)
        .await
        .expect("set product status");
}

#[tokio::test]
async fn wave58_resolution_follows_the_current_catalogue() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w58-current").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let member = create_product(&app, &seller, &store_id, "Shelf Member").await;

    // A collection holding exactly the member product.
    let (status, created) = call(
        &app,
        Method::POST,
        "/v1/seller/store/collections",
        Some(json!({ "slug": "only-shelf", "title": "একমাত্র শেলফ" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create collection: {created}");
    let collection_id = created["id"].as_str().expect("collection id").to_string();
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/collections/{collection_id}/products"),
        Some(json!({ "product_ids": [member] })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set members: {body}");

    // The layout: the shelf, plus a manual rail whose
    // product list repeats the member (dedupe check).
    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "sections": [
                {
                    "id": "shelf",
                    "type": "collection_shelf",
                    "config": { "collection_id": collection_id }
                },
                {
                    "id": "picks",
                    "type": "product_rail",
                    "config": {
                        "source": "manual",
                        "product_ids": [member, member]
                    }
                }
            ]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put layout: {saved}");
    let picks = section_by_id(&saved, "picks").expect("picks in saved view");
    assert_eq!(
        picks["config"]["product_ids"],
        json!([member]),
        "the duplicate id collapsed on write"
    );

    // Both sections render while the member is active.
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-current/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(section_ids(&public), vec!["shelf", "picks"]);

    // Draft the member: the shelf AND the rail both lose
    // their only product and drop — nothing rewrites the
    // layout; resolution simply follows the catalogue.
    set_product_status(&pool, &member, "draft").await;
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-current/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(
        public["sections"].as_array().expect("sections").len(),
        0,
        "both sections dropped with their only product drafted: {public}"
    );

    // Re-activate: both sections return, untouched.
    set_product_status(&pool, &member, "active").await;
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-current/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(section_ids(&public), vec!["shelf", "picks"]);
    let shelf = section_by_id(&public, "shelf").expect("shelf back");
    assert_eq!(shelf["collection"]["slug"], json!("only-shelf"));
}
