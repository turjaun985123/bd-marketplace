//! Admin products remake, test F: the queue's filters,
//! sorts and summary. Every filter is fail-closed (an
//! unknown value is a 400 naming the allowed set), search
//! is LIKE-escaped (a bare `%` matches nothing), the
//! summary describes the WHOLE queue even while a filter
//! narrows the page, and the sorts are total orders over
//! the fixtures' distinct prices.
//!
//! Fixtures: A active/manual, price 100_000, stock 0 (out
//! of stock, no image); B active/manual, price 300_000,
//! stock 10, WITH an image; C draft, source facebook
//! (stamped by SQL — the import path is not under test
//! here); D archived, source bulk (same); E active,
//! price 200_000, stock 5, TAKEN DOWN.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job.

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
use uuid::Uuid;

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

/// Create a product with an explicit status; returns the id.
async fn create_product_with_status(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
    status: &str,
) -> String {
    let (code, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": status
        })),
        Some(token),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// GET the admin product list with a query string.
async fn list(app: &Router, admin: &str, query: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::GET,
        &format!("/v1/admin/products?{query}"),
        None,
        Some(admin),
    )
    .await
}

/// The ids of a list response, in page order.
fn ids(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|item| item["id"].as_str().expect("item id").to_string())
        .collect()
}

#[tokio::test]
async fn adminproducts_list_filters_sorts_summary() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin products remake test F");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, return_requests, store_follows, \
         store_verification_events, store_verifications, platform_settings, \
         payouts, payout_accounts, review_replies, reviews, shipment_events, \
         shipments, payment_events, payments, order_items, sub_orders, orders, \
         cart_items, carts, products, api_keys, store_members, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(
        Some(pool.clone()),
        "adminproductsf-test-secret".to_string(),
        true,
    )
    .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller = login(&app, "01722222222").await;
    let store = create_store(&app, &seller, "aprod-foxtrot").await;
    let a = create_product_with_status(&app, &seller, &store, "Foxtrot A", 100_000, 0, "active").await;
    let b = create_product_with_status(&app, &seller, &store, "Foxtrot B", 300_000, 10, "active").await;
    let c = create_product_with_status(&app, &seller, &store, "Foxtrot C", 150_000, 3, "draft").await;
    let d = create_product_with_status(&app, &seller, &store, "Foxtrot D", 250_000, 7, "archived").await;
    let e = create_product_with_status(&app, &seller, &store, "Foxtrot E", 200_000, 5, "active").await;

    // C reads as a Facebook import, D as a bulk row (the
    // source column is the import paths' stamp; stamping it
    // directly keeps this file about the queue, not the
    // importers).
    for (id, source, source_ref) in [(&c, "facebook", "post-9"), (&d, "bulk", "SKU-D")] {
        sqlx::query("UPDATE products SET source = $2, source_ref = $3 WHERE id = $1")
            .bind(Uuid::parse_str(id).expect("fixture uuid"))
            .bind(source)
            .bind(source_ref)
            .execute(&pool)
            .await
            .expect("stamp source");
    }
    // B has an image (an external hotlink row).
    sqlx::query(
        "INSERT INTO product_images (product_id, external_url, position, is_primary) \
         VALUES ($1, 'https://example.com/b.jpg', 0, true)",
    )
    .bind(Uuid::parse_str(&b).expect("fixture uuid"))
    .execute(&pool)
    .await
    .expect("insert image row");
    // E is taken down.
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/admin/products/{e}/takedown"),
        Some(json!({ "note": "পরীক্ষার জন্য সরানো" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "takedown E: {body}");

    // --- Summary: whole queue, exact buckets. ---
    let (status, all) = list(&app, &admin, "").await;
    assert_eq!(status, StatusCode::OK, "list all: {all}");
    assert_eq!(all["total"], json!(5));
    assert_eq!(all["summary"]["total"], json!(5));
    assert_eq!(all["summary"]["active"], json!(3));
    assert_eq!(all["summary"]["draft"], json!(1));
    assert_eq!(all["summary"]["archived"], json!(1));
    assert_eq!(all["summary"]["taken_down"], json!(1));
    assert_eq!(all["summary"]["out_of_stock"], json!(1), "only A: {all}");
    assert_eq!(all["summary"]["no_image"], json!(4), "all but B: {all}");

    // --- Filters narrow the page but never the summary. ---
    let (status, by_status) = list(&app, &admin, "status=draft").await;
    assert_eq!(status, StatusCode::OK, "status filter: {by_status}");
    assert_eq!(ids(&by_status), vec![c.clone()]);
    assert_eq!(by_status["summary"]["total"], json!(5), "summary unfiltered: {by_status}");

    let (status, by_source) = list(&app, &admin, "source=facebook").await;
    assert_eq!(status, StatusCode::OK, "source filter: {by_source}");
    assert_eq!(ids(&by_source), vec![c.clone()]);
    let (status, by_source) = list(&app, &admin, "source=bulk").await;
    assert_eq!(status, StatusCode::OK, "source filter: {by_source}");
    assert_eq!(ids(&by_source), vec![d.clone()]);

    let (status, by_visibility) = list(&app, &admin, "visibility=taken_down").await;
    assert_eq!(status, StatusCode::OK, "visibility filter: {by_visibility}");
    assert_eq!(ids(&by_visibility), vec![e.clone()]);
    let (status, by_visibility) = list(&app, &admin, "visibility=visible").await;
    assert_eq!(status, StatusCode::OK, "visibility filter: {by_visibility}");
    assert_eq!(by_visibility["total"], json!(4));

    let (status, by_stock) = list(&app, &admin, "stock_state=out_of_stock").await;
    assert_eq!(status, StatusCode::OK, "stock filter: {by_stock}");
    assert_eq!(ids(&by_stock), vec![a.clone()]);
    let (status, by_images) = list(&app, &admin, "has_images=true").await;
    assert_eq!(status, StatusCode::OK, "image filter: {by_images}");
    assert_eq!(ids(&by_images), vec![b.clone()]);
    let (status, by_store) = list(&app, &admin, &format!("store_id={store}")).await;
    assert_eq!(status, StatusCode::OK, "store filter: {by_store}");
    assert_eq!(by_store["total"], json!(5));

    // --- Search: substring works, LIKE wildcards do not. ---
    let (status, by_q) = list(&app, &admin, "q=Foxtrot%20B").await;
    assert_eq!(status, StatusCode::OK, "search: {by_q}");
    assert_eq!(ids(&by_q), vec![b.clone()]);
    let (status, by_q) = list(&app, &admin, "q=%25").await;
    assert_eq!(status, StatusCode::OK, "wildcard search: {by_q}");
    assert_eq!(by_q["total"], json!(0), "% must not act as a wildcard: {by_q}");

    // --- Sorts are total orders. ---
    let (status, by_price) = list(&app, &admin, "sort=price_asc").await;
    assert_eq!(status, StatusCode::OK, "price sort: {by_price}");
    assert_eq!(
        ids(&by_price),
        vec![a.clone(), c.clone(), e.clone(), d.clone(), b.clone()]
    );
    let (status, by_price) = list(&app, &admin, "sort=price_desc").await;
    assert_eq!(status, StatusCode::OK, "price sort: {by_price}");
    assert_eq!(
        ids(&by_price),
        vec![b.clone(), d.clone(), e.clone(), c.clone(), a.clone()]
    );

    // --- Every invalid filter value is a fail-closed 400. ---
    for (query, needle) in [
        ("status=hidden", "status must be one of"),
        ("source=import", "source must be one of"),
        ("visibility=gone", "visibility must be one of"),
        ("stock_state=plenty", "stock_state must be one of"),
        ("has_images=yes", "has_images must be one of"),
        ("sort=cheapest", "sort must be one of"),
    ] {
        let (status, body) = list(&app, &admin, query).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {body}");
        assert!(
            body["error"].as_str().unwrap_or("").contains(needle),
            "{query} names the allowed set: {body}"
        );
    }
}
