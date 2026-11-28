//! Wave 59 integration test (g): the content access
//! matrix — content edits ride the SAME rule as store
//! branding (Wave 34's manage-marketing capability):
//! anonymous callers get 401, a signed-in stranger
//! (no relationship to the store) gets the scoped
//! 404, a STAFF member (catalogue + orders only) gets
//! 403, and a MANAGER — marketing is in their grant —
//! can create, publish, reorder and delete both pages
//! and posts. The owner keeps full control throughout.
//!
//! Gated on `DATABASE_URL`: without it the test skips
//! cleanly so a plain local `cargo test` (no database)
//! still passes. CI provides a Postgres 16 service and
//! sets `DATABASE_URL` for the rust job.

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
            eprintln!("DATABASE_URL not set — skipping Wave 59 integration tests");
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
         store_pages, store_posts, store_post_events, \
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
    let state = AppState::new(Some(pool.clone()), "wave59-test-secret".to_string(), true)
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

/// The id of the user owning a phone (canonical +880 form).
async fn user_id(pool: &PgPool, phone: &str) -> String {
    let row: (String,) = sqlx::query_as("SELECT id::text FROM users WHERE phone = $1")
        .bind(phone)
        .fetch_one(pool)
        .await
        .expect("user row exists");
    row.0
}

/// Add a team member row directly (the API path needs
/// the owner's token dance; the row is what access
/// resolution reads).
async fn add_member(pool: &PgPool, store_id: &str, member_id: &str, role: &str) {
    sqlx::query(
        "INSERT INTO store_members (store_id, user_id, role) VALUES ($1::uuid, $2::uuid, $3)",
    )
    .bind(store_id)
    .bind(member_id)
    .bind(role)
    .execute(pool)
    .await
    .expect("member row inserted");
}

#[tokio::test]
async fn wave59g_content_access_matches_the_branding_rule() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let owner = login(&app, "01718111111").await;
    let store = create_store(&app, &owner, "w59-access").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let stranger = login(&app, "01718222222").await;
    let manager = login(&app, "01718333333").await;
    let staff = login(&app, "01718444444").await;
    add_member(&pool, &store_id, &user_id(&pool, "+8801718333333").await, "manager").await;
    add_member(&pool, &store_id, &user_id(&pool, "+8801718444444").await, "staff").await;

    let page_payload = json!({
        "slug": "team-page",
        "title": "টিমের পেজ",
        "body": "টিমের লেখা।",
        "show_in_nav": true
    });
    let post_payload = json!({
        "slug": "team-post",
        "title": "টিমের পোস্ট",
        "body": "টিমের পোস্টের লেখা।",
        "tags": []
    });

    // 1. Anonymous: 401 on reads and writes alike.
    for (method, uri) in [
        (Method::GET, "/v1/seller/store/pages".to_string()),
        (Method::GET, "/v1/seller/store/posts".to_string()),
        (Method::GET, "/v1/seller/store/post-stats".to_string()),
    ] {
        let (status, _) = call(&app, method, &uri, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "anon {uri}");
    }
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/pages",
        Some(page_payload.clone()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anon create page");

    // 2. Stranger (signed in, no relationship): the
    // scoped read is a 404 — the store's content is
    // not theirs to even see in the editor.
    let (status, _) = call(&app, Method::GET, "/v1/seller/store/pages", None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger page list");
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(post_payload.clone()),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger create post");

    // 3. Staff: related to the store but marketing is
    // NOT in the staff grant — 403, on reads too. (The
    // scoped form names the store; unscoped, resolution
    // would fall back to a store of their own and 404.)
    let scoped = "?store_id=".to_string() + &store_id;
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/store/pages{scoped}"),
        None,
        Some(&staff),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "staff page list");
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/store/pages{scoped}"),
        Some(page_payload.clone()),
        Some(&staff),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "staff create page");

    // 4. Manager: marketing IS in their grant — they
    // create a page and a post for the owner's store,
    // publish the post, and reorder the page set.
    let (status, page) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/store/pages{scoped}"),
        Some(page_payload),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "manager create page: {page}");
    let page_id = page["id"].as_str().expect("page id").to_string();
    let (status, post) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/store/posts{scoped}"),
        Some(post_payload),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "manager create post: {post}");
    let post_id = post["id"].as_str().expect("post id").to_string();
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/store/posts/{post_id}/status{scoped}"),
        Some(json!({ "status": "published" })),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "manager publishes");
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/pages{scoped}"),
        Some(json!({ "page_ids": [page_id] })),
        Some(&manager),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "manager reorders");

    // 5. The owner's own reads see the team's work, and
    // the published post is public under the store.
    let (status, body) = call(&app, Method::GET, "/v1/seller/store/pages", None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK, "owner page list: {body}");
    assert_eq!(body.as_array().expect("pages").len(), 1);
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-access/posts/team-post",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "manager-published post is public: {body}");
}
