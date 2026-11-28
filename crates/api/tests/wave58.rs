//! Wave 58 integration test: the DEFAULT layout — a
//! store that never opens the section editor renders the
//! platform default composition, resolved exactly like
//! the pre-Wave-58 storefront:
//! - the seller read returns the 9-section default
//!   document with `is_custom: false`, and NO row exists
//!   in store_layouts (the fallback is code, not data);
//! - the public read resolves it: the hero falls back to
//!   the store's branding (name / banner / announcement /
//!   description), the on-sale rail carries the one
//!   compare-at product, the best-sellers rail drops (no
//!   delivered sales yet), both category grids drop (no
//!   categorised products), the collection shelf drops
//!   (no collections), the catalogue grid carries the
//!   product total, and store info carries the branding
//!   texts;
//! - an unknown store's layout is a 404.
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
    price_poisha: i64,
    compare_at_poisha: Option<i64>,
) -> String {
    let mut body = json!({
        "store_id": store_id,
        "title": title,
        "price_poisha": price_poisha,
        "stock": 10,
        "status": "active"
    });
    if let Some(compare) = compare_at_poisha {
        body["compare_at_poisha"] = json!(compare);
    }
    let (status, created) = call(app, Method::POST, "/v1/products", Some(body), Some(token)).await;
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

#[tokio::test]
async fn wave58_default_layout_matches_the_historical_storefront() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w58-default").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // Branding the hero and the info blocks fall back to.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/branding",
        Some(json!({
            "banner_url": "https://cdn.example.com/w58-banner.png",
            "announcement": "ঈদের বিশেষ অফার চলছে",
            "description": "ঢাকার সেরা কালেকশন",
            "about": "আমাদের গল্প — ২০১৫ সাল থেকে।",
            "return_policy": "৭ দিনের সহজ রিটার্ন।"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "branding: {body}");

    // One product on sale (compare-at above price), one
    // plain. Neither is categorised; there are no sales
    // and no collections — several default sections must
    // resolve empty and drop.
    let on_sale = create_product(&app, &seller, &store_id, "Sale Panjabi", 185_000, Some(220_000)).await;
    let plain = create_product(&app, &seller, &store_id, "Plain Panjabi", 150_000, None).await;

    // The seller read: the default document, not custom,
    // and NO stored row behind it.
    let (status, layout) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller layout: {layout}");
    assert_eq!(layout["is_custom"], json!(false));
    assert_eq!(layout["version"], json!(1));
    assert_eq!(layout["updated_at"], Value::Null);
    assert_eq!(
        section_ids(&layout),
        vec![
            "hero",
            "rail-on-sale",
            "rail-best-sellers",
            "rail-new-arrivals",
            "collections",
            "specialties",
            "categories",
            "all-products",
            "store-info",
        ]
    );
    let stored_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_layouts WHERE store_id = $1::uuid",
    )
    .bind(&store_id)
    .fetch_one(&pool)
    .await
    .expect("count stored layouts");
    assert_eq!(stored_rows, 0, "the default is code, never a row");

    // The public read: resolved, with the empty sections
    // dropped and the survivors in the default order.
    let (status, public) = call(
        &app,
        Method::GET,
        "/v1/stores/w58-default/layout",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(public["is_custom"], json!(false));
    assert_eq!(public["store_slug"], json!("w58-default"));
    assert_eq!(
        section_ids(&public),
        vec![
            "hero",
            "rail-on-sale",
            "rail-new-arrivals",
            "all-products",
            "store-info",
        ],
        "best-sellers / collections / both grids dropped empty: {public}"
    );

    // The hero fell back to branding, field by field.
    let hero = section_by_id(&public, "hero").expect("hero section");
    assert_eq!(hero["type"], json!("hero"));
    assert_eq!(hero["hero"]["headline"], json!("Store w58-default"));
    assert_eq!(
        hero["hero"]["image_url"],
        json!("https://cdn.example.com/w58-banner.png")
    );
    assert_eq!(hero["hero"]["announcement"], json!("ঈদের বিশেষ অফার চলছে"));
    assert_eq!(hero["hero"]["subtext"], json!("ঢাকার সেরা কালেকশন"));

    // The on-sale rail carries exactly the sale product;
    // the newest rail carries both.
    let sale_rail = section_by_id(&public, "rail-on-sale").expect("sale rail");
    let sale_items = sale_rail["products"].as_array().expect("rail products");
    assert_eq!(sale_items.len(), 1);
    assert_eq!(sale_items[0]["id"], json!(on_sale));
    let new_rail = section_by_id(&public, "rail-new-arrivals").expect("new rail");
    let new_items = new_rail["products"].as_array().expect("rail products");
    assert_eq!(new_items.len(), 2);
    let new_ids: Vec<&str> = new_items
        .iter()
        .map(|item| item["id"].as_str().expect("product id"))
        .collect();
    assert!(new_ids.contains(&on_sale.as_str()));
    assert!(new_ids.contains(&plain.as_str()));

    // The catalogue grid carries the total; store info
    // carries the branding texts and the live aggregates.
    let grid = section_by_id(&public, "all-products").expect("grid section");
    assert_eq!(grid["total_products"], json!(2));
    let info = section_by_id(&public, "store-info").expect("info section");
    assert_eq!(info["store_info"]["about"], json!("আমাদের গল্প — ২০১৫ সাল থেকে।"));
    assert_eq!(info["store_info"]["return_policy"], json!("৭ দিনের সহজ রিটার্ন।"));
    assert_eq!(info["store_info"]["product_count"], json!(2));
    assert_eq!(info["store_info"]["total_sold"], json!(0));

    // An unknown store's layout is the shared 404.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/stores/w58-no-such-store/layout",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown store: {body}");
}
