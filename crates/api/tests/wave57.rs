//! Wave 57 integration test: bulk CSV import — the happy
//! path end to end.
//! - a sheet creates products through the shared insert
//!   path (source 'bulk', sku in source_ref);
//! - category resolution per row: the slug links and the
//!   label syncs to the node's English name; a Bangla
//!   label links on the exact-match rule and is stored
//!   exactly as typed; a label matching nothing stays
//!   free-text with a note on the outcome;
//! - a blank status defaults to draft;
//! - follower notices fire ONLY for rows born active AND
//!   newly created (the single-create funnel's semantics):
//!   the follower's outbox gains exactly one
//!   store_new_product row per active creation, none for
//!   the draft.
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

/// Send one raw CSV body to the bulk-import endpoint and
/// return (status, parsed report). The one helper the
/// shared `call` cannot express (it JSON-encodes its
/// body); the Wave 51 raw-body helper precedent.
async fn call_csv(
    app: &Router,
    uri: &str,
    csv: &str,
    token: &str,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "text/csv")
        .body(Body::from(csv.to_string()))
        .expect("request builds");
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

#[tokio::test]
async fn wave57_bulk_import_creates_with_category_resolution_and_follower_notices() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-main").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // A follower signs up BEFORE the import: born-active
    // creations must reach their outbox.
    let follower = login(&app, "01733333333").await;
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores/w57-main/follow",
        None,
        Some(&follower),
    )
    .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "follow: {status} {body}"
    );

    // The sheet is built cell by cell (12 columns, the
    // domain's canonical order) so no comma is miscounted.
    let header = "sku,title,title_bn,description,price_poisha,compare_at_poisha,stock,weight_grams,status,category_slug,category_label,image_url";
    let row = |cells: [&str; 12]| cells.join(",");
    let csv = [
        header.to_string(),
        row(["A-1", "Cotton Panjabi", "সুতি পাঞ্জাবি", "", "185000", "220000", "25", "400", "active", "panjabi", "", ""]),
        row(["A-2", "Label Linked Panjabi", "", "", "150000", "", "10", "", "active", "", "পাঞ্জাবি", ""]),
        row(["A-3", "Free Text Thing", "", "", "99000", "", "3", "", "active", "", "Handicrafts", ""]),
        row(["A-4", "Draft Thing", "", "", "50000", "", "7", "", "", "", "", ""]),
    ]
    .join("\n")
        + "\n";
    let (status, report) = call_csv(
        &app,
        &format!("/v1/seller/products/bulk-import?store_id={store_id}"),
        &csv,
        &seller,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "import: {report}");
    assert_eq!(report["dry_run"], json!(false));
    assert_eq!(report["total_rows"], json!(4));
    assert_eq!(report["created"], json!(4));
    assert_eq!(report["updated"], json!(0));
    assert_eq!(report["skipped"], json!(0));
    assert!(report["import_id"].is_string(), "run recorded: {report}");
    let outcomes = report["outcomes"].as_array().expect("outcomes");
    assert_eq!(outcomes.len(), 4);
    for outcome in outcomes {
        assert_eq!(outcome["action"], json!("created"), "{outcome}");
        assert!(outcome["errors"].as_array().expect("errors").is_empty());
    }
    // The free-text row carries its explanatory note.
    let free_text = &outcomes[2];
    assert!(
        free_text["notes"]
            .as_array()
            .expect("notes")
            .iter()
            .any(|note| note.as_str().unwrap_or("").contains("free-text")),
        "free-text note: {free_text}"
    );

    // Row 1: slug-linked — the stored label syncs to the
    // node's English name and the detail carries the link.
    let first_id = outcomes[0]["product_id"].as_str().expect("product id");
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{first_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    // The detail body is FLAT (Wave 56 canon).
    assert_eq!(detail["category"], json!("Panjabi"), "{detail}");
    assert!(detail["category_id"].is_string(), "{detail}");
    assert_eq!(detail["source_ref"], json!("A-1"), "{detail}");
    let panjabi_id = detail["category_id"].as_str().expect("node id").to_string();

    // Row 2: the Bangla label linked the SAME node and is
    // stored exactly as typed.
    let second_id = outcomes[1]["product_id"].as_str().expect("product id");
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{second_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail 2: {detail}");
    assert_eq!(detail["category"], json!("পাঞ্জাবি"), "{detail}");
    assert_eq!(detail["category_id"], json!(panjabi_id), "{detail}");

    // Row 3: free-text — no link, label as typed.
    let third_id = outcomes[2]["product_id"].as_str().expect("product id");
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{third_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail 3: {detail}");
    assert_eq!(detail["category"], json!("Handicrafts"), "{detail}");
    assert_eq!(detail["category_id"], Value::Null, "{detail}");

    // Row 4: blank status defaulted to draft.
    let fourth_id = outcomes[3]["product_id"].as_str().expect("product id");
    let draft_status: String = sqlx::query_scalar("SELECT status FROM products WHERE id = $1")
        .bind(
            uuid::Uuid::parse_str(fourth_id).expect("uuid"),
        )
        .fetch_one(&pool)
        .await
        .expect("draft row");
    assert_eq!(draft_status, "draft");

    // The follower-notice rule: exactly the three
    // born-active creations notified; the draft did not.
    let follower_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE phone = '+8801733333333'")
            .fetch_one(&pool)
            .await
            .expect("follower row");
    let notices: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND kind = 'store_new_product'",
    )
    .bind(follower_id)
    .fetch_one(&pool)
    .await
    .expect("count notices");
    assert_eq!(notices, 3, "one notice per born-active creation");

    // All four products carry the bulk source tag.
    let bulk_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM products WHERE store_id = $1::uuid AND source = 'bulk'",
    )
    .bind(&store_id)
    .fetch_one(&pool)
    .await
    .expect("count bulk products");
    assert_eq!(bulk_count, 4);
}
