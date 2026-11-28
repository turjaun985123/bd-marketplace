//! Wave 58 integration test: the manual rail's
//! lifecycle:
//! - a manual rail naming ANOTHER store's product is
//!   refused at PUT (400, naming the section) — ownership
//!   is checked against the store, not just the id's
//!   shape;
//! - a saved manual rail renders in the seller's exact
//!   order;
//! - when one of its products is later drafted, the
//!   public rail quietly drops just that product (the
//!   stored layout is NOT rewritten — the seller view
//!   still lists all three ids, so re-activating the
//!   product restores the rail as saved).
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
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
) -> String {
    let (status, created) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": 90_000,
            "stock": 5,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {created}");
    created["id"].as_str().expect("product id").to_string()
}

/// Find one section in a layout body by id.
fn section_by_id<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["sections"]
        .as_array()?
        .iter()
        .find(|section| section["id"].as_str() == Some(id))
}

/// The product ids a resolved rail carries, in order.
fn rail_product_ids(section: &Value) -> Vec<String> {
    section["products"]
        .as_array()
        .expect("rail products")
        .iter()
        .map(|item| item["id"].as_str().expect("product id").to_string())
        .collect()
}

#[tokio::test]
async fn wave58_manual_rail_ownership_order_and_draft_drop() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w58-manual").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let p1 = create_product(&app, &seller, &store_id, "Pick One").await;
    let p2 = create_product(&app, &seller, &store_id, "Pick Two").await;
    let p3 = create_product(&app, &seller, &store_id, "Pick Three").await;

    // Another store's product, for the ownership refusal.
    let other = login(&app, "01722222222").await;
    let other_store = create_store(&app, &other, "w58-other").await;
    let other_store_id = other_store["id"].as_str().expect("store id").to_string();
    let foreign = create_product(&app, &other, &other_store_id, "Foreign Pick").await;

    let rail_doc = |ids: Vec<String>| {
        json!({
            "sections": [
                {
                    "id": "picks",
                    "type": "product_rail",
                    "config": { "source": "manual", "product_ids": ids }
                }
            ]
        })
    };

    // A foreign product in the rail: refused at write,
    // naming the section.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(rail_doc(vec![foreign.clone()])),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "foreign product: {body}");
    let message = body["error"].as_str().expect("error message");
    assert!(message.contains("section 1"), "names the section: {message}");
    assert!(
        message.contains(&foreign),
        "names the product: {message}"
    );

    // The honest rail saves and renders in the seller's
    // exact order.
    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(rail_doc(vec![p2.clone(), p1.clone(), p3.clone()])),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put layout: {saved}");
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-manual/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    let rail = section_by_id(&public, "picks").expect("rail section");
    assert_eq!(rail_product_ids(rail), vec![p2.clone(), p1.clone(), p3.clone()]);

    // Draft the third pick (by SQL — the status walk is
    // the products module's own tests' business): the
    // public rail drops just it, in order, and the stored
    // layout still names all three.
    sqlx::query("UPDATE products SET status = 'draft' WHERE id = $1::uuid")
        .bind(&p3)
        .execute(&pool)
        .await
        .expect("draft the third pick");
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-manual/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    let rail = section_by_id(&public, "picks").expect("rail section");
    assert_eq!(rail_product_ids(rail), vec![p2.clone(), p1.clone()]);
    let (status, fetched) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller layout: {fetched}");
    let rail = section_by_id(&fetched, "picks").expect("rail in seller view");
    assert_eq!(
        rail["config"]["product_ids"],
        json!([p2, p1, p3]),
        "the stored document is not rewritten by a draft"
    );
}
