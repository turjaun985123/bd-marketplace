//! Wave 58 integration test: the content sections and
//! events on hidden sections:
//! - an image banner resolves with its config intact
//!   (image, link, caption) and a rich-text section with
//!   its heading and body;
//! - a hero inside a LIVE scheduling window (2020 →
//!   2999) renders like an unscheduled one;
//! - an event for a DISABLED section still records —
//!   the section exists in the effective layout, and a
//!   buyer with the page open in a tab can honestly
//!   click it — and the seller stats carry its row.
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

/// Find one section in a layout body by id.
fn section_by_id<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["sections"]
        .as_array()?
        .iter()
        .find(|section| section["id"].as_str() == Some(id))
}

#[tokio::test]
async fn wave58_content_sections_and_events_on_hidden_sections() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    create_store(&app, &seller, "w58-media").await;

    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "sections": [
                {
                    "id": "hero",
                    "type": "hero",
                    "starts_at": "2020-01-01T00:00:00Z",
                    "ends_at": "2999-01-01T00:00:00Z",
                    "config": { "headline": "সবসময়ের হিরো" }
                },
                {
                    "id": "campaign",
                    "type": "image_banner",
                    "config": {
                        "image_url": "https://cdn.example.com/campaign.png",
                        "link_url": "/w58-media/shop?on_sale=true",
                        "caption": "ঈদ ক্যাম্পেইন"
                    }
                },
                {
                    "id": "story",
                    "type": "rich_text",
                    "config": {
                        "heading": "আমাদের গল্প",
                        "body": "২০১৫ সাল থেকে ঢাকায় হাতে বোনা পণ্য।"
                    }
                },
                {
                    "id": "promo",
                    "type": "rich_text",
                    "enabled": false,
                    "config": { "body": "বন্ধ থাকা প্রমো" }
                }
            ]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put layout: {saved}");

    // Publicly: the in-window hero, the banner and the
    // story render; the disabled promo does not.
    let (status, public) = call(&app, Method::GET, "/v1/stores/w58-media/layout", None, None).await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(section_ids(&public), vec!["hero", "campaign", "story"]);
    let hero = section_by_id(&public, "hero").expect("hero section");
    assert_eq!(hero["hero"]["headline"], json!("সবসময়ের হিরো"));
    assert_eq!(hero["hero"]["is_customised"], json!(true));
    let banner = section_by_id(&public, "campaign").expect("banner section");
    assert_eq!(
        banner["config"]["image_url"],
        json!("https://cdn.example.com/campaign.png")
    );
    assert_eq!(banner["config"]["link_url"], json!("/w58-media/shop?on_sale=true"));
    assert_eq!(banner["config"]["caption"], json!("ঈদ ক্যাম্পেইন"));
    let story = section_by_id(&public, "story").expect("story section");
    assert_eq!(story["config"]["heading"], json!("আমাদের গল্প"));
    assert_eq!(
        story["config"]["body"],
        json!("২০১৫ সাল থেকে ঢাকায় হাতে বোনা পণ্য।")
    );

    // An event for the DISABLED promo section records
    // (it is in the effective layout), and the stats row
    // for it exists alongside the visible sections' rows.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/w58-media/section-events",
        Some(json!({ "section_id": "promo", "kind": "section_click" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "event on disabled section: {body}");
    let (status, stats) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout/stats",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stats: {stats}");
    assert_eq!(
        stats["sections"].as_array().expect("stat rows").len(),
        4,
        "one row per layout section, disabled included"
    );
    let promo = stats["sections"]
        .as_array()
        .expect("stat rows")
        .iter()
        .find(|row| row["section_id"] == json!("promo"))
        .expect("promo stats row");
    assert_eq!(promo["clicks_total"], json!(1));
    assert_eq!(promo["views_total"], json!(0));
}
