//! Wave 58 integration test: shelves, grids, and the
//! empty-drop rule:
//! - an on-sale rail with nothing on sale is omitted;
//! - an all-collections shelf carries the store's
//!   collections (only those with active products);
//! - a single-collection shelf carries that collection
//!   and its products in position order;
//! - after the named collection is DELETED, its shelf
//!   drops out of the public layout (the layout itself
//!   is untouched — the seller view still lists it);
//! - the label category grid carries the free-text label
//!   with its count, and the taxonomy grid carries the
//!   linked node with its count.
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

/// POST one active product (optionally with a free-text
/// category label); returns its id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    category: Option<&str>,
) -> String {
    let mut body = json!({
        "store_id": store_id,
        "title": title,
        "price_poisha": 120_000,
        "stock": 10,
        "status": "active"
    });
    if let Some(category) = category {
        body["category"] = json!(category);
    }
    let (status, created) = call(app, Method::POST, "/v1/products", Some(body), Some(token)).await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {created}");
    created["id"].as_str().expect("product id").to_string()
}

/// POST one store collection and set its members;
/// returns the collection id.
async fn create_collection(
    app: &Router,
    token: &str,
    slug: &str,
    product_ids: &[String],
) -> String {
    let (status, created) = call(
        app,
        Method::POST,
        "/v1/seller/store/collections",
        Some(json!({ "slug": slug, "title": format!("Collection {slug}") })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create collection {slug}: {created}");
    let collection_id = created["id"].as_str().expect("collection id").to_string();
    let (status, body) = call(
        app,
        Method::PUT,
        &format!("/v1/seller/store/collections/{collection_id}/products"),
        Some(json!({ "product_ids": product_ids })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set collection products: {body}");
    collection_id
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

#[tokio::test]
async fn wave58_shelves_grids_and_the_empty_drop_rule() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w58-shelves").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // X carries a free-text label; Y is linked straight
    // into the taxonomy (the seeded 'panjabi' node) by
    // SQL, the deterministic way to pin a link in a test.
    let labelled = create_product(&app, &seller, &store_id, "Labelled Craft", Some("Handicrafts")).await;
    let linked = create_product(&app, &seller, &store_id, "Linked Panjabi", None).await;
    let panjabi_id: String = sqlx::query_scalar("SELECT id::text FROM categories WHERE slug = 'panjabi'")
        .fetch_one(&pool)
        .await
        .expect("panjabi node is seeded");
    sqlx::query("UPDATE products SET category_id = $1::uuid, category = 'Panjabi' WHERE id = $2::uuid")
        .bind(&panjabi_id)
        .bind(&linked)
        .execute(&pool)
        .await
        .expect("link product to panjabi");

    let keep = create_collection(&app, &seller, "eid-picks", std::slice::from_ref(&labelled)).await;
    let ghost = create_collection(&app, &seller, "ghost-picks", std::slice::from_ref(&linked)).await;

    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "sections": [
                {
                    "id": "rail-on-sale",
                    "type": "product_rail",
                    "config": { "source": "on_sale", "title": "অফারে আছে" }
                },
                { "id": "shelf-all", "type": "collection_shelf", "config": {} },
                {
                    "id": "shelf-one",
                    "type": "collection_shelf",
                    "config": { "collection_id": ghost }
                },
                {
                    "id": "grid-labels",
                    "type": "category_grid",
                    "config": { "mode": "labels" }
                },
                {
                    "id": "grid-tax",
                    "type": "category_grid",
                    "config": { "mode": "taxonomy" }
                }
            ]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put layout: {saved}");
    assert!(keep.len() == 36, "collection id shape");

    // First public read: the sale rail drops (nothing is
    // on sale); both shelves and both grids resolve.
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-shelves/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(
        section_ids(&public),
        vec!["shelf-all", "shelf-one", "grid-labels", "grid-tax"],
        "empty sale rail omitted: {public}"
    );
    let shelf_all = section_by_id(&public, "shelf-all").expect("all shelf");
    let tiles = shelf_all["collections"].as_array().expect("collection tiles");
    assert_eq!(tiles.len(), 2, "both collections have products: {tiles:?}");
    let shelf_one = section_by_id(&public, "shelf-one").expect("one shelf");
    assert_eq!(shelf_one["collection"]["slug"], json!("ghost-picks"));
    let shelf_products = shelf_one["products"].as_array().expect("shelf products");
    assert_eq!(shelf_products.len(), 1);
    assert_eq!(shelf_products[0]["id"], json!(linked));
    let labels = section_by_id(&public, "grid-labels").expect("label grid");
    let label_tiles = labels["categories"].as_array().expect("label tiles");
    let handicrafts = label_tiles
        .iter()
        .find(|tile| tile["category"] == json!("Handicrafts"))
        .expect("the label tile exists");
    assert_eq!(handicrafts["product_count"], json!(1));
    let taxonomy = section_by_id(&public, "grid-tax").expect("taxonomy grid");
    let tax_tiles = taxonomy["categories"].as_array().expect("taxonomy tiles");
    let panjabi = tax_tiles
        .iter()
        .find(|tile| tile["slug"] == json!("panjabi"))
        .expect("the panjabi tile exists");
    assert_eq!(panjabi["product_count"], json!(1));

    // Delete the named collection: its shelf drops from
    // the public layout; the seller's document still
    // lists the section (nothing rewrote their layout).
    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/seller/store/collections/{ghost}"),
        None,
        Some(&seller),
    )
    .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::NO_CONTENT,
        "delete collection: {status} {body}"
    );
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-shelves/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(
        section_ids(&public),
        vec!["shelf-all", "grid-labels", "grid-tax"],
        "the deleted collection's shelf dropped: {public}"
    );
    let shelf_all = section_by_id(&public, "shelf-all").expect("all shelf");
    let tiles = shelf_all["collections"].as_array().expect("collection tiles");
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0]["slug"], json!("eid-picks"));
    let (status, fetched) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller layout: {fetched}");
    assert_eq!(
        section_ids(&fetched),
        vec!["rail-on-sale", "shelf-all", "shelf-one", "grid-labels", "grid-tax"],
        "the stored document is untouched by the deletion"
    );
}
