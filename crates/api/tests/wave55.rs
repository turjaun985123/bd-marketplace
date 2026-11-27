//! Wave 55 integration test: the buyer address book and the
//! public district table — CRUD with the domain validation
//! (canonical phone, district canonicalised against the
//! 64-district table, zone always DERIVED from the district),
//! the one-default rules (first address auto-defaults,
//! switching is atomic, deleting the default leaves NO
//! default), the 10-address cap, and ownership (another
//! buyer's id is a 404 on every verb). The district endpoint
//! is public reference data: 64 districts, Dhaka inside,
//! Gazipur suburb, Tangail outside (it does not border
//! Dhaka district — the documented policy).
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
/// database is configured. Truncates every table, the
/// Wave 54 list plus the Wave 55 `buyer_addresses`.
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 55 integration tests");
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
    let state = AppState::new(Some(pool.clone()), "wave55-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// POST one address for the token's buyer; returns (status, body).
async fn create_address(app: &Router, token: &str, body: Value) -> (StatusCode, Value) {
    call(app, Method::POST, "/v1/addresses", Some(body), Some(token)).await
}

/// A valid address body over the given district.
fn address_body(label: &str, district: &str) -> Value {
    json!({
        "label": label,
        "recipient_name": "Test Buyer",
        "phone": "01712345678",
        "district": district,
        "area": "Mirpur",
        "address_line": "House 1, Road 2"
    })
}

#[tokio::test]
async fn wave55_address_book_and_districts_flow() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let buyer = login(&app, "01711111111").await;
    let stranger = login(&app, "01722222222").await;

    // 1. The district table is public: 64 districts, and the
    // zone policy pins — Dhaka inside, Gazipur suburb,
    // Tangail outside, Khulna outside.
    let (status, districts) = call(&app, Method::GET, "/v1/districts", None, None).await;
    assert_eq!(status, StatusCode::OK, "districts: {districts}");
    assert_eq!(districts["total"], 64, "64 districts: {districts}");
    let items = districts["items"].as_array().expect("district items");
    assert_eq!(items.len(), 64);
    let zone_of = |name: &str| {
        items
            .iter()
            .find(|row| row["name_en"] == name)
            .unwrap_or_else(|| panic!("district {name} in table"))
            ["zone"]
            .as_str()
            .expect("zone")
            .to_string()
    };
    assert_eq!(zone_of("Dhaka"), "inside_dhaka");
    assert_eq!(zone_of("Gazipur"), "dhaka_suburb");
    assert_eq!(zone_of("Narayanganj"), "dhaka_suburb");
    assert_eq!(zone_of("Tangail"), "outside_dhaka");
    assert_eq!(zone_of("Khulna"), "outside_dhaka");

    // 2. The book starts empty, and it needs a login.
    let (status, _) = call(&app, Method::GET, "/v1/addresses", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "book needs login");
    let (status, book) = call(&app, Method::GET, "/v1/addresses", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "empty book: {book}");
    assert_eq!(book["total"], 0);
    assert_eq!(book["items"].as_array().expect("items").len(), 0);

    // 3. The FIRST address becomes the default on its own;
    // the district canonicalises ("dhaka" -> Dhaka), the
    // phone canonicalises, the zone derives.
    let (status, first) = create_address(&app, &buyer, address_body("বাসা", "dhaka")).await;
    assert_eq!(status, StatusCode::CREATED, "create first: {first}");
    assert_eq!(first["district"], "Dhaka");
    assert_eq!(first["district_bn"], "ঢাকা");
    assert_eq!(first["zone"], "inside_dhaka");
    assert_eq!(first["phone"], "+8801712345678");
    assert_eq!(first["full_address"], "House 1, Road 2, Mirpur, Dhaka");
    assert_eq!(first["is_default"], true, "first address auto-defaults");
    assert!(first["last_used_at"].is_null(), "never used yet");
    let first_id = first["id"].as_str().expect("address id").to_string();

    // 4. A second address does not steal the default.
    let (status, second) =
        create_address(&app, &buyer, address_body("অফিস", "Gazipur")).await;
    assert_eq!(status, StatusCode::CREATED, "create second: {second}");
    assert_eq!(second["zone"], "dhaka_suburb");
    assert_eq!(second["is_default"], false);
    let second_id = second["id"].as_str().expect("address id").to_string();

    // 5. Creating with is_default switches the default
    // atomically — exactly one default in the list after.
    let mut third_body = address_body("বাবার বাসা", "Khulna");
    third_body["is_default"] = json!(true);
    let (status, third) = create_address(&app, &buyer, third_body).await;
    assert_eq!(status, StatusCode::CREATED, "create third: {third}");
    assert_eq!(third["is_default"], true);
    let third_id = third["id"].as_str().expect("address id").to_string();
    let (status, book) = call(&app, Method::GET, "/v1/addresses", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    let defaults: Vec<&Value> = book["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|row| row["is_default"] == true)
        .collect();
    assert_eq!(defaults.len(), 1, "exactly one default: {book}");
    assert_eq!(defaults[0]["id"], third_id);
    assert_eq!(
        book["items"][0]["id"], third_id,
        "the default lists first (picker order)"
    );

    // 6. The set-default endpoint moves the flag back.
    let uri = format!("/v1/addresses/{second_id}/default");
    let (status, made) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "set default: {made}");
    assert_eq!(made["is_default"], true);

    // 7. PATCH merges: a new district re-derives the zone;
    // an invalid phone and an unknown district are 400s.
    let uri = format!("/v1/addresses/{first_id}");
    let (status, patched) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "district": "Tangail" })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch district: {patched}");
    assert_eq!(patched["district"], "Tangail");
    assert_eq!(patched["zone"], "outside_dhaka");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "phone": "123" })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad phone: {body}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "district": "Atlantis" })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown district: {body}");

    // 8. Validation on create: blank label, bad phone,
    // unknown district — all 400, nothing stored.
    let (status, body) = create_address(&app, &buyer, address_body("  ", "Dhaka")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "blank label: {body}");
    let mut bad_phone = address_body("খারাপ", "Dhaka");
    bad_phone["phone"] = json!("999");
    let (status, body) = create_address(&app, &buyer, bad_phone).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad phone: {body}");
    let (status, body) = create_address(&app, &buyer, address_body("খারাপ", "Nowhere")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown district: {body}");

    // 9. Ownership: the stranger's book is their own
    // (empty), and every verb on the buyer's ids is a 404.
    let (status, book) = call(&app, Method::GET, "/v1/addresses", None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(book["total"], 0, "the stranger's book is empty");
    let uri = format!("/v1/addresses/{first_id}");
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "label": "চুরি" })),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger patch: {body}");
    let (status, body) = call(&app, Method::DELETE, &uri, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger delete: {body}");
    let uri = format!("/v1/addresses/{first_id}/default");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger default: {body}");

    // 10. The cap: the buyer holds 3; seven more reach 10,
    // and the eleventh is refused.
    for n in 4..=10 {
        let (status, body) =
            create_address(&app, &buyer, address_body(&format!("ঠিকানা {n}"), "Dhaka")).await;
        assert_eq!(status, StatusCode::CREATED, "create #{n}: {body}");
    }
    let (status, body) = create_address(&app, &buyer, address_body("একাদশ", "Dhaka")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "eleventh refused: {body}");

    // 11. Deleting the DEFAULT leaves no default — no
    // silent promotion. Deleting a stranger's (unknown)
    // id is a 404.
    let uri = format!("/v1/addresses/{second_id}");
    let (status, book) = call(&app, Method::DELETE, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "delete default: {book}");
    assert_eq!(book["total"], 9);
    let any_default = book["items"]
        .as_array()
        .expect("items")
        .iter()
        .any(|row| row["is_default"] == true);
    assert!(!any_default, "no default after deleting it: {book}");
    let (status, body) = call(
        &app,
        Method::DELETE,
        "/v1/addresses/00000000-0000-0000-0000-000000000000",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown id delete: {body}");
}
