//! Wave 58 integration test: the editor catalogue and
//! the layout presets:
//! - the catalogue is signed-in only (401 anonymous);
//! - it serves the domain's nine types with bilingual
//!   labels and descriptions, the caps, and three
//!   presets;
//! - a preset's sections are directly PUT-able (they
//!   pass the same domain validation as any document)
//!   and resolve publicly with the empty sections
//!   dropped — the merchandising preset on an empty
//!   store leaves hero + catalogue grid + FAQ.
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

/// The ids of a layout body's sections, in order.
fn section_ids(body: &Value) -> Vec<String> {
    body["sections"]
        .as_array()
        .expect("sections array")
        .iter()
        .map(|section| section["id"].as_str().expect("section id").to_string())
        .collect()
}

#[tokio::test]
async fn wave58_catalogue_and_presets() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    // Anonymous: the catalogue is seller-surface metadata.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout/catalogue",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous catalogue: {body}");

    let seller = login(&app, "01711111111").await;
    create_store(&app, &seller, "w58-catalogue").await;
    let (status, catalogue) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout/catalogue",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "catalogue: {catalogue}");
    assert_eq!(catalogue["version"], json!(1));
    assert_eq!(catalogue["max_sections"], json!(20));
    let types = catalogue["types"].as_array().expect("types array");
    assert_eq!(types.len(), 9);
    let type_names: Vec<&str> = types
        .iter()
        .map(|meta| meta["section_type"].as_str().expect("type name"))
        .collect();
    assert_eq!(
        type_names,
        vec![
            "hero",
            "rich_text",
            "product_rail",
            "collection_shelf",
            "category_grid",
            "product_grid",
            "image_banner",
            "store_info",
            "faq",
        ]
    );
    for meta in types {
        for field in ["label_en", "label_bn", "description_en", "description_bn"] {
            let text = meta[field].as_str().expect("meta text");
            assert!(!text.is_empty(), "{field} present for {}", meta["section_type"]);
        }
    }
    // The form vocabularies ride the same response, from
    // the domain's closed lists.
    assert_eq!(
        catalogue["rail_sources"],
        json!(["newest", "best_sellers", "on_sale", "manual"])
    );
    assert_eq!(catalogue["category_grid_modes"], json!(["taxonomy", "labels"]));
    let presets = catalogue["presets"].as_array().expect("presets array");
    assert_eq!(presets.len(), 3);
    let preset_keys: Vec<&str> = presets
        .iter()
        .map(|preset| preset["key"].as_str().expect("preset key"))
        .collect();
    assert_eq!(preset_keys, vec!["default", "minimal", "merchandising"]);
    let minimal = &presets[1];
    assert_eq!(
        minimal["sections"].as_array().expect("preset sections").len(),
        3,
        "the minimal preset is hero + grid + info"
    );

    // Apply the merchandising preset verbatim: it must
    // pass the same validation as any seller document.
    let merchandising = &presets[2];
    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "version": 1,
            "sections": merchandising["sections"]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "apply preset: {saved}");
    assert_eq!(saved["is_custom"], json!(true));
    assert_eq!(section_ids(&saved).len(), 9);

    // On an empty store the preset resolves to the three
    // sections that need no catalogue data.
    let (status, public) = call(
        &app,
        Method::GET,
        "/v1/stores/w58-catalogue/layout",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(
        section_ids(&public),
        vec!["hero", "all-products", "faq"],
        "rails/shelf/grid/info dropped empty: {public}"
    );
    let faq = &public["sections"][2];
    let items = faq["faq_items"].as_array().expect("preset faq items");
    assert_eq!(items.len(), 2, "the preset ships two starter answers");
}
