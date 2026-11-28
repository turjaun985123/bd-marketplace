//! Admin coupons remake, file A: the admin coupon endpoints are
//! admin-only (401 without a token, 403 for a non-admin JWT, 403
//! for an API key on every verb — the W37 blanket rule), the
//! audited create normalizes and validates through the ONE shared
//! terms validation (every bad shape is a 400 that writes nothing
//! and audits nothing — including the remake's one tightening,
//! `max_discount_poisha = 0`), and codes are platform-global: a
//! duplicate is a 409 in either scope, in either direction.
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

/// Log in, then promote to admin by SQL (login FIRST — the
/// promotion must land on the already-created user row).
async fn make_admin(app: &Router, pool: &PgPool, phone_local: &str, phone_canonical: &str) -> String {
    let token = login(app, phone_local).await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = $1")
        .bind(phone_canonical)
        .execute(pool)
        .await
        .expect("promote admin");
    token
}

/// Create a store for the token's user; returns the store id.
async fn create_store(app: &Router, token: &str, slug: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": format!("Store {slug}"), "slug": slug })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store {slug}: {body}");
    body["id"].as_str().expect("store id").to_string()
}

/// Mint an API key with a JWT; returns the full key (shown once).
async fn create_key(app: &Router, jwt: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/api-keys",
        Some(json!({ "name": "coupons-test", "scopes": ["read", "write"], "expires_in_days": null })),
        Some(jwt),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create api key: {body}");
    let key = body["key"].as_str().expect("full key once").to_string();
    assert!(key.starts_with("bdm_live_"), "key shape: {key}");
    key
}

/// Count rows in the coupons table.
async fn coupon_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM coupons")
        .fetch_one(pool)
        .await
        .expect("count coupons")
}

/// Count audit rows for one coupon and action.
async fn audit_count(pool: &PgPool, coupon_id: &str, action: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_audit_log \
         WHERE target_type = 'coupon' AND target_id = $1 AND action = $2",
    )
    .bind(coupon_id)
    .bind(action)
    .fetch_one(pool)
    .await
    .expect("count audit rows")
}

/// Count ALL coupon audit rows.
async fn coupon_audit_total(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM admin_audit_log WHERE target_type = 'coupon'")
        .fetch_one(pool)
        .await
        .expect("count coupon audit rows")
}

#[tokio::test]
async fn admincoupons_guards_and_create_validation() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin coupons test A");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, api_keys, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         buyer_addresses, bulk_imports, store_layouts, storefront_events, \
         store_pages, store_posts, store_post_events, \
         store_collections, store_collection_products, \
         product_images, product_variants, products, store_members, store_follows, \
         gift_card_ledger, gift_cards, \
         stores, otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "admincoupons-test-secret".to_string(), true);
    let app = build_app(state);

    let admin = make_admin(&app, &pool, "01711111111", "+8801711111111").await;
    let plain = login(&app, "01722222222").await;
    let seller = login(&app, "01733333333").await;
    create_store(&app, &seller, "coupons-a-store").await;

    // 1. No token at all -> 401 on the list.
    let (status, body) = call(&app, Method::GET, "/v1/admin/coupons", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "list without token: {body}");

    // 2. The happy-path audited create (lowercase code in,
    // normalized code stored).
    let (status, created) = call(
        &app,
        Method::POST,
        "/v1/admin/coupons",
        Some(json!({
            "code": "eid2026",
            "discount_type": "percent",
            "discount_value": 10,
            "max_discount_poisha": 5000
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "admin create: {created}");
    assert_eq!(created["code"], "EID2026");
    assert_eq!(created["store_id"], Value::Null);
    assert_eq!(created["active"], true);
    assert_eq!(created["used_count"], 0);
    assert_eq!(created["admin_deactivated_reason"], Value::Null);
    let coupon_id = created["id"].as_str().expect("coupon id").to_string();
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.create").await, 1);
    assert_eq!(coupon_count(&pool).await, 1);

    // 3. A non-admin JWT is refused on EVERY verb, and the
    // refusals change nothing.
    let detail_uri = format!("/v1/admin/coupons/{coupon_id}");
    let terms_uri = format!("/v1/admin/coupons/{coupon_id}/terms");
    for (method, uri, body) in [
        (Method::GET, "/v1/admin/coupons".to_string(), None),
        (
            Method::POST,
            "/v1/admin/coupons".to_string(),
            Some(json!({ "code": "NOPE1", "discount_type": "percent", "discount_value": 5 })),
        ),
        (Method::GET, detail_uri.clone(), None),
        (
            Method::PATCH,
            detail_uri.clone(),
            Some(json!({ "active": false, "reason": "x" })),
        ),
        (
            Method::PATCH,
            terms_uri.clone(),
            Some(json!({ "discount_type": "percent", "discount_value": 5 })),
        ),
    ] {
        let (status, response) = call(&app, method.clone(), &uri, body, Some(&plain)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "non-admin {method} {uri}: {response}");
    }
    assert_eq!(coupon_count(&pool).await, 1, "non-admin attempts wrote nothing");
    assert_eq!(coupon_audit_total(&pool).await, 1, "only the real create is audited");

    // 4. An API key (even read+write, minted by the admin) is
    // refused on every admin coupon verb — the W37 blanket rule.
    let key = create_key(&app, &admin).await;
    for (method, uri, body) in [
        (Method::GET, "/v1/admin/coupons".to_string(), None),
        (
            Method::POST,
            "/v1/admin/coupons".to_string(),
            Some(json!({ "code": "NOPE2", "discount_type": "percent", "discount_value": 5 })),
        ),
        (Method::GET, detail_uri.clone(), None),
        (
            Method::PATCH,
            detail_uri.clone(),
            Some(json!({ "active": false, "reason": "x" })),
        ),
        (
            Method::PATCH,
            terms_uri.clone(),
            Some(json!({ "discount_type": "percent", "discount_value": 5 })),
        ),
    ] {
        let (status, response) = call(&app, method.clone(), &uri, body, Some(&key)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "api key {method} {uri}: {response}");
    }
    assert_eq!(coupon_count(&pool).await, 1, "api-key attempts wrote nothing");

    // 5. Every invalid create shape is a 400 that writes
    // nothing and audits nothing.
    let bad_shapes: Vec<Value> = vec![
        json!({ "code": "AB", "discount_type": "percent", "discount_value": 10 }),
        json!({ "code": "A".repeat(33), "discount_type": "percent", "discount_value": 10 }),
        json!({ "code": "BAD CODE", "discount_type": "percent", "discount_value": 10 }),
        json!({ "code": "BAD%CODE", "discount_type": "percent", "discount_value": 10 }),
        json!({ "code": "OKCODE1", "discount_type": "voucher", "discount_value": 10 }),
        json!({ "code": "OKCODE2", "discount_type": "percent", "discount_value": 0 }),
        json!({ "code": "OKCODE3", "discount_type": "percent", "discount_value": 91 }),
        json!({ "code": "OKCODE4", "discount_type": "percent", "discount_value": 95 }),
        json!({ "code": "OKCODE5", "discount_type": "percent", "discount_value": -5 }),
        json!({ "code": "OKCODE6", "discount_type": "fixed", "discount_value": 0 }),
        json!({ "code": "OKCODE7", "discount_type": "fixed", "discount_value": 99 }),
        json!({ "code": "OKCODE8", "discount_type": "percent", "discount_value": 10, "min_order_poisha": -1 }),
        json!({ "code": "OKCODE9", "discount_type": "percent", "discount_value": 10, "max_discount_poisha": -1 }),
        // The remake's one tightening: a zero cap is a coupon
        // that can never discount — rejected like a negative one.
        json!({ "code": "OKCODE10", "discount_type": "percent", "discount_value": 10, "max_discount_poisha": 0 }),
        json!({ "code": "OKCODE11", "discount_type": "percent", "discount_value": 10, "usage_limit": 0 }),
        json!({ "code": "OKCODE12", "discount_type": "percent", "discount_value": 10, "usage_limit": -3 }),
        json!({
            "code": "OKCODE13", "discount_type": "percent", "discount_value": 10,
            "starts_at": "2030-01-01T00:00:00Z", "ends_at": "2030-01-01T00:00:00Z"
        }),
        json!({
            "code": "OKCODE14", "discount_type": "percent", "discount_value": 10,
            "starts_at": "2030-01-02T00:00:00Z", "ends_at": "2030-01-01T00:00:00Z"
        }),
    ];
    for shape in &bad_shapes {
        let (status, response) = call(
            &app,
            Method::POST,
            "/v1/admin/coupons",
            Some(shape.clone()),
            Some(&admin),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "bad shape {shape}: {response}");
    }
    assert_eq!(coupon_count(&pool).await, 1, "invalid creates wrote nothing");
    assert_eq!(coupon_audit_total(&pool).await, 1, "invalid creates audited nothing");

    // 6. Duplicates are a 409 — exact, case variant, padded
    // variant, and across scopes in both directions (codes are
    // platform-global).
    for code in ["EID2026", "eid2026", " EID2026 "] {
        let (status, response) = call(
            &app,
            Method::POST,
            "/v1/admin/coupons",
            Some(json!({ "code": code, "discount_type": "percent", "discount_value": 5 })),
            Some(&admin),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "duplicate {code:?}: {response}");
    }
    let (status, response) = call(
        &app,
        Method::POST,
        "/v1/seller/coupons",
        Some(json!({ "code": "EID2026", "discount_type": "fixed", "discount_value": 10000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "seller duplicate of platform code: {response}");

    let (status, store_coupon) = call(
        &app,
        Method::POST,
        "/v1/seller/coupons",
        Some(json!({ "code": "STOREONLY", "discount_type": "fixed", "discount_value": 10000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "seller create: {store_coupon}");
    let (status, response) = call(
        &app,
        Method::POST,
        "/v1/admin/coupons",
        Some(json!({ "code": "storeonly", "discount_type": "percent", "discount_value": 5 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "admin duplicate of store code: {response}");
    assert_eq!(coupon_count(&pool).await, 2);
}
