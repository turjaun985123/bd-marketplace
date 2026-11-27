//! Wave 56 integration test: the category read surfaces
//! in depth —
//! - `GET /v1/admin/categories/:id` (the single admin
//!   view: 404 unknown, 403 non-admin, 401 anonymous);
//! - `GET /v1/admin/categories/tree` keeps INACTIVE
//!   nodes the public tree prunes;
//! - `GET /v1/categories/:slug/products`, the public
//!   subtree product list (404 for unknown AND retired
//!   slugs — direct addressing, unlike the list filter);
//! - the admin drill-in paginates (total is the whole
//!   subtree, pages slice it);
//! - reorder validates its input (empty → 400).
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

/// Login, THEN promote by SQL (login creates the user row).
async fn login_admin(app: &Router, pool: &PgPool, phone: &str) -> String {
    let token = login(app, phone).await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = $1")
        .bind(format!("+880{}", &phone[1..]))
        .execute(pool)
        .await
        .expect("promote admin");
    token
}

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 56 integration tests");
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
    let state = AppState::new(Some(pool.clone()), "wave56-test-secret".to_string(), true)
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

/// POST one product with extra fields merged in; returns the view.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    extra: Value,
) -> Value {
    let mut body = json!({
        "store_id": store_id,
        "title": title,
        "price_poisha": 10_000,
        "stock": 5,
        "status": "active"
    });
    if let (Some(target), Some(source)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    let (status, created) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(body),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {created}");
    created
}

/// Find a node by slug inside a built tree payload.
fn find_node<'a>(nodes: &'a [Value], slug: &str) -> Option<&'a Value> {
    for node in nodes {
        if node["slug"] == slug {
            return Some(node);
        }
        if let Some(children) = node["children"].as_array() {
            if let Some(found) = find_node(children, slug) {
                return Some(found);
            }
        }
    }
    None
}

/// A seed node's id, read from the public tree endpoint.
async fn seed_id(app: &Router, slug: &str) -> String {
    let (status, body) = call(app, Method::GET, "/v1/categories/tree", None, None).await;
    assert_eq!(status, StatusCode::OK, "tree: {body}");
    let nodes = body["tree"].as_array().expect("tree nodes");
    find_node(nodes, slug)
        .unwrap_or_else(|| panic!("seed node {slug} in the public tree"))
        ["id"]
        .as_str()
        .expect("node id")
        .to_string()
}

#[tokio::test]
async fn wave56_category_read_surfaces() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let admin = login_admin(&app, &pool, "01711111111").await;
    let plain = login(&app, "01722222222").await;
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "w56f-store").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    let fashion_id = seed_id(&app, "fashion").await;
    let panjabi_id = seed_id(&app, "panjabi").await;

    // 1. The single admin view: full shape for an admin,
    // 403 for a plain user, 401 anonymous, 404 for an id
    // that does not exist.
    let (status, view) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/categories/{fashion_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin get one: {view}");
    assert_eq!(view["slug"], "fashion");
    assert_eq!(view["depth"], 1);
    assert_eq!(view["is_featured"], true);
    assert!(view["child_count"].as_i64().expect("child count") >= 4);
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/categories/{fashion_id}"),
        None,
        Some(&plain),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "plain user is not an admin");
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/categories/{fashion_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous gets 401");
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/admin/categories/00000000-0000-0000-0000-000000000000",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown id 404s");

    // 2. The public subtree product list: three products
    // across the Fashion branch are all here, and only
    // here (the Electronics product is not).
    for (title, category_id) in [
        ("W56F One", panjabi_id.clone()),
        ("W56F Two", panjabi_id.clone()),
        ("W56F Three", fashion_id.clone()),
    ] {
        create_product(
            &app,
            &seller,
            &store_id,
            title,
            json!({ "category_id": category_id }),
        )
        .await;
    }
    let electronics_id = seed_id(&app, "electronics").await;
    create_product(
        &app,
        &seller,
        &store_id,
        "W56F Outsider",
        json!({ "category_id": electronics_id }),
    )
    .await;

    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/categories/fashion/products",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "subtree products: {list}");
    assert_eq!(list["total"], 3);
    // Pagination slices the same total.
    let (status, page) = call(
        &app,
        Method::GET,
        "/v1/categories/fashion/products?limit=2&page=2",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "page 2: {page}");
    assert_eq!(page["total"], 3);
    assert_eq!(page["items"].as_array().expect("page items").len(), 1);
    // A leaf's list is exactly its own products.
    let (status, leaf) = call(
        &app,
        Method::GET,
        "/v1/categories/panjabi/products",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "leaf products: {leaf}");
    assert_eq!(leaf["total"], 2);
    // Unknown and retired slugs 404 on direct addressing.
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/categories/w56f-nope/products",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown slug 404s");

    let (status, retired) = call(
        &app,
        Method::POST,
        "/v1/admin/categories",
        Some(json!({ "slug": "w56f-retired", "name_en": "W56F Retired" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "retired node: {retired}");
    let retired_id = retired["id"].as_str().expect("retired id").to_string();
    let (status, _) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/categories/{retired_id}"),
        Some(json!({ "is_active": false })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "retire it");
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/categories/w56f-retired/products",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "retired slug 404s");

    // 3. The admin tree keeps the retired node (with its
    // flag) that the public tree pruned.
    let (status, admin_tree) = call(
        &app,
        Method::GET,
        "/v1/admin/categories/tree",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin tree: {admin_tree}");
    let admin_nodes = admin_tree["tree"].as_array().expect("admin nodes");
    let retired_node = find_node(admin_nodes, "w56f-retired").expect("admin keeps retired");
    assert_eq!(retired_node["is_active"], false);
    let (status, public_tree) = call(&app, Method::GET, "/v1/categories/tree", None, None).await;
    assert_eq!(status, StatusCode::OK, "public tree: {public_tree}");
    assert!(
        find_node(public_tree["tree"].as_array().expect("public nodes"), "w56f-retired")
            .is_none(),
        "public tree prunes it"
    );

    // 4. The drill-in paginates over the subtree: total
    // counts all three Fashion products, pages slice.
    let (status, drill) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/categories/{fashion_id}/products?limit=2"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "drill page 1: {drill}");
    assert_eq!(drill["total"], 3);
    assert_eq!(drill["items"].as_array().expect("drill items").len(), 2);
    let (status, drill2) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/categories/{fashion_id}/products?limit=2&page=2"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "drill page 2: {drill2}");
    assert_eq!(drill2["items"].as_array().expect("drill items 2").len(), 1);
    // A non-admin cannot drill in.
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/categories/{fashion_id}/products"),
        None,
        Some(&plain),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "drill-in is admin-only");

    // 5. Reorder input validation: an empty item list is
    // a 400, not a silent no-op.
    let (status, err) = call(
        &app,
        Method::PUT,
        "/v1/admin/categories/reorder",
        Some(json!({ "items": [] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty reorder: {err}");
}
