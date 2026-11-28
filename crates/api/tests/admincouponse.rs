//! Admin coupons remake, file E: the terms edit — a platform
//! coupon's terms may change ONLY while it has never been
//! redeemed (orders snapshot discount amounts, never terms,
//! so after a first redemption the terms are locked), the
//! edit reuses the ONE shared terms validation, an
//! identical-terms edit is a 409 no-op, a store coupon's
//! terms are never an admin's to rewrite (409), and the
//! audit row carries the full before/after snapshots.
//! Non-admins and API keys are refused.
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

/// Create an active product; returns the product id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Add a product to the buyer's cart.
async fn add_to_cart(app: &Router, token: &str, product_id: &str, quantity: i64) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_id, "quantity": quantity })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {body}");
}

/// Mint an API key with a JWT; returns the full key (shown once).
async fn create_key(app: &Router, jwt: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/api-keys",
        Some(json!({ "name": "coupons-e-test", "scopes": ["read", "write"], "expires_in_days": null })),
        Some(jwt),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create api key: {body}");
    body["key"].as_str().expect("full key once").to_string()
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

#[tokio::test]
async fn admincoupons_terms_edit_gates() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin coupons test E");
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

    let state = AppState::new(Some(pool.clone()), "admincouponse-test-secret".to_string(), true);
    let app = build_app(state);

    let admin = make_admin(&app, &pool, "01711111111", "+8801711111111").await;
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "coupons-e-store").await;
    let product = create_product(&app, &seller, &store, "Product E", 100_000, 10).await;
    let buyer = login(&app, "01999999999").await;

    let (status, created) = call(
        &app,
        Method::POST,
        "/v1/admin/coupons",
        Some(json!({
            "code": "EDIT10",
            "discount_type": "percent",
            "discount_value": 10,
            "max_discount_poisha": 5000,
            "usage_limit": 100
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create EDIT10: {created}");
    let coupon_id = created["id"].as_str().expect("coupon id").to_string();
    let terms_uri = format!("/v1/admin/coupons/{coupon_id}/terms");

    let new_terms = json!({
        "discount_type": "percent",
        "discount_value": 20,
        "min_order_poisha": 1000,
        "max_discount_poisha": 8000,
        "starts_at": null,
        "ends_at": null,
        "usage_limit": 50
    });

    // Guards first: non-admin and API key are refused, and an
    // unknown id 404s.
    let (status, response) = call(
        &app,
        Method::PATCH,
        &terms_uri,
        Some(new_terms.clone()),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "non-admin terms edit: {response}");
    let key = create_key(&app, &admin).await;
    let (status, response) = call(
        &app,
        Method::PATCH,
        &terms_uri,
        Some(new_terms.clone()),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "api-key terms edit: {response}");
    let (status, response) = call(
        &app,
        Method::PATCH,
        "/v1/admin/coupons/00000000-0000-0000-0000-000000000000/terms",
        Some(new_terms.clone()),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "missing coupon terms edit: {response}");
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.update_terms").await, 0);

    // The edit lands and is live at preview immediately.
    let (status, updated) = call(
        &app,
        Method::PATCH,
        &terms_uri,
        Some(new_terms.clone()),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "terms edit: {updated}");
    assert_eq!(updated["discount_value"], 20);
    assert_eq!(updated["max_discount_poisha"], 8000);
    assert_eq!(updated["min_order_poisha"], 1000);
    assert_eq!(updated["usage_limit"], 50);
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.update_terms").await, 1);
    let (detail_text,): (String,) = sqlx::query_as(
        "SELECT detail::text FROM admin_audit_log \
         WHERE target_type = 'coupon' AND target_id = $1 AND action = 'coupon.update_terms'",
    )
    .bind(&coupon_id)
    .fetch_one(&pool)
    .await
    .expect("terms audit row");
    assert!(
        detail_text.contains("\"discount_value\":10"),
        "audit keeps the BEFORE terms: {detail_text}"
    );
    assert!(
        detail_text.contains("\"discount_value\":20"),
        "audit keeps the AFTER terms: {detail_text}"
    );

    let (status, preview) = call(
        &app,
        Method::POST,
        "/v1/coupons/preview",
        Some(json!({ "code": "EDIT10", "goods_total_poisha": 445_000 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "preview: {preview}");
    assert_eq!(preview["valid"], true);
    assert_eq!(
        preview["discount_poisha"], 8_000,
        "20% of 445_000 capped at the NEW 8_000 cap"
    );

    // An identical-terms edit is a no-op: 409, nothing written.
    let (status, response) = call(
        &app,
        Method::PATCH,
        &terms_uri,
        Some(new_terms.clone()),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "identical terms edit: {response}");
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.update_terms").await, 1);

    // Invalid terms are a 400 through the shared validation —
    // including the zero-cap tightening — and write nothing.
    for bad in [
        json!({
            "discount_type": "percent", "discount_value": 95,
            "max_discount_poisha": 8000, "usage_limit": 50
        }),
        json!({
            "discount_type": "percent", "discount_value": 20,
            "max_discount_poisha": 0, "usage_limit": 50
        }),
    ] {
        let (status, response) = call(
            &app,
            Method::PATCH,
            &terms_uri,
            Some(bad.clone()),
            Some(&admin),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "invalid terms {bad}: {response}");
    }
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.update_terms").await, 1);

    // One real redemption locks the terms for good.
    add_to_cart(&app, &buyer, &product, 1).await;
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod",
            "coupon_code": "EDIT10"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "checkout with EDIT10: {order}");
    assert_eq!(
        order["discount_total_poisha"], 8_000,
        "20% of 100_000 capped at 8_000"
    );

    let (status, response) = call(
        &app,
        Method::PATCH,
        &terms_uri,
        Some(json!({
            "discount_type": "percent", "discount_value": 30,
            "max_discount_poisha": 9000, "usage_limit": 60
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "edit after redemption: {response}");
    let (stored_value,): (i64,) = sqlx::query_as("SELECT discount_value FROM coupons WHERE id = $1::uuid")
        .bind(&coupon_id)
        .fetch_one(&pool)
        .await
        .expect("stored terms");
    assert_eq!(stored_value, 20, "the locked terms did not move");
    assert_eq!(audit_count(&pool, &coupon_id, "coupon.update_terms").await, 1);

    // A store coupon's terms belong to its seller: 409.
    let (status, store_coupon) = call(
        &app,
        Method::POST,
        "/v1/seller/coupons",
        Some(json!({ "code": "STOREEDIT", "discount_type": "fixed", "discount_value": 10000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "store coupon: {store_coupon}");
    let store_coupon_id = store_coupon["id"].as_str().expect("store coupon id").to_string();
    let (status, response) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/coupons/{store_coupon_id}/terms"),
        Some(json!({ "discount_type": "fixed", "discount_value": 20000 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "store coupon terms edit: {response}");
    assert_eq!(audit_count(&pool, &store_coupon_id, "coupon.update_terms").await, 0);
}
