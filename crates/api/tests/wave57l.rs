//! Wave 57 integration test: bulk import UPDATE
//! semantics (the re-import contract).
//! - blank optional cells (title_bn, description,
//!   compare_at) leave the stored values standing — a
//!   price-only re-import never erases copy;
//! - data columns (title, price, weight, status,
//!   category) always overwrite;
//! - an update may re-link the category by naming a new
//!   slug, and may flip draft -> active — and even that
//!   flip does NOT notify followers (the documented
//!   rule: a re-import is a sync, not a publish; only
//!   the bulk EDIT's flip notifies, via the update path);
//! - a dry run against existing skus reports
//!   would_update and writes nothing.
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

const HEADER: &str = "sku,title,title_bn,description,price_poisha,compare_at_poisha,stock,weight_grams,status,category_slug,category_label,image_url";

/// One sheet row from its 12 cells (the domain's column
/// order) — no hand-counted commas.
fn row(cells: [&str; 12]) -> String {
    cells.join(",")
}

#[tokio::test]
async fn wave57l_update_preserves_blank_optionals_and_never_notifies() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w57-update").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let uri = format!("/v1/seller/products/bulk-import?store_id={store_id}");

    let follower = login(&app, "01733333333").await;
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/stores/w57-update/follow",
        None,
        Some(&follower),
    )
    .await;
    assert!(status.is_success(), "follow: {status}");

    // V1: a draft with full copy (Bangla title,
    // description, compare-at) and a fashion label.
    let v1 = [
        HEADER.to_string(),
        row(["L-1", "Original Title", "আসল শিরোনাম", "Original description", "100000", "130000", "5", "300", "draft", "", "Fashion", ""]),
    ]
    .join("\n");
    let (status, report) = call_csv(&app, &uri, &v1, &seller).await;
    assert_eq!(status, StatusCode::OK, "v1: {report}");
    assert_eq!(report["created"], json!(1));
    let product_id = report["outcomes"][0]["product_id"]
        .as_str()
        .expect("product id")
        .to_string();

    // Dry run of V2 reports would_update, writes nothing.
    let v2 = [
        HEADER.to_string(),
        row(["L-1", "Updated Title", "", "", "111000", "", "9", "350", "active", "panjabi", "", ""]),
    ]
    .join("\n");
    let (status, report) = call_csv(&app, &format!("{uri}&dry_run=true"), &v2, &seller).await;
    assert_eq!(status, StatusCode::OK, "dry v2: {report}");
    assert_eq!(report["outcomes"][0]["action"], json!("would_update"));
    let title_now: String = sqlx::query_scalar("SELECT title FROM products WHERE id = $1::uuid")
        .bind(&product_id)
        .fetch_one(&pool)
        .await
        .expect("title");
    assert_eq!(title_now, "Original Title", "the dry run wrote nothing");

    // Real V2: title/price/weight/status/category
    // overwrite; the blank optionals are preserved.
    let (status, report) = call_csv(&app, &uri, &v2, &seller).await;
    assert_eq!(status, StatusCode::OK, "v2: {report}");
    assert_eq!(report["updated"], json!(1));
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["title"], json!("Updated Title"));
    assert_eq!(detail["price_poisha"], json!(111_000));
    assert_eq!(detail["status"], json!("active"), "the flip landed");
    assert_eq!(
        detail["title_bn"],
        json!("আসল শিরোনাম"),
        "blank title_bn preserved the stored copy"
    );
    assert_eq!(
        detail["description"],
        json!("Original description"),
        "blank description preserved"
    );
    assert_eq!(
        detail["compare_at_poisha"],
        json!(130_000),
        "blank compare_at preserved"
    );
    assert_eq!(
        detail["category"],
        json!("Panjabi"),
        "the new slug re-linked and re-synced the label"
    );
    assert!(detail["category_id"].is_string());

    // Even though the update flipped draft -> active,
    // NO follower notice fired (updates never notify).
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
    assert_eq!(notices, 0, "a re-import flip is a sync, not a publish");
}
