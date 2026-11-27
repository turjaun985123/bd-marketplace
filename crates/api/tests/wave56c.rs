//! Wave 56 integration test: the public taxonomy
//! surfaces and the compatibility contract —
//! - `GET /v1/categories` keeps the Wave 53 free-text
//!   aggregate in `categories` BYTE-IDENTICAL in shape and
//!   adds the taxonomy as a `tree` field; `?flat=1`
//!   returns exactly the old shape (no `tree` key);
//! - tree counts roll subtrees up (a parent counts its
//!   descendants' products) and count only VISIBLE
//!   products;
//! - `GET /v1/categories/:slug` serves the node + path,
//!   404s unknown and inactive slugs;
//! - an inactive node's active children surface at the
//!   top of the public tree (the orphan rule) while the
//!   admin tree keeps the real shape;
//! - the home feed gains `taxonomy` (featured first) and
//!   keeps its legacy `categories` tiles;
//! - search suggestions resolve labels into taxonomy
//!   nodes (`category_nodes`) without changing the
//!   Wave 53 fields.
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
async fn wave56_public_taxonomy_surfaces() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let admin = login_admin(&app, &pool, "01711111111").await;
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "w56c-store").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    let fashion_id = seed_id(&app, "fashion").await;
    let panjabi_id = seed_id(&app, "panjabi").await;
    let saree_id = seed_id(&app, "saree").await;

    // Fixtures: one product filed at each of three levels
    // of the Fashion branch, one free-text product.
    create_product(
        &app,
        &seller,
        &store_id,
        "W56C Root Product",
        json!({ "category_id": fashion_id }),
    )
    .await;
    create_product(
        &app,
        &seller,
        &store_id,
        "W56C Panjabi Special",
        json!({ "category_id": panjabi_id }),
    )
    .await;
    create_product(
        &app,
        &seller,
        &store_id,
        "W56C Saree Product",
        json!({ "category_id": saree_id }),
    )
    .await;
    create_product(
        &app,
        &seller,
        &store_id,
        "W56C Odd Product",
        json!({ "category": "w56c-odd" }),
    )
    .await;

    // 1. GET /v1/categories: the legacy aggregate is all
    // there (the odd label with its count, the synced
    // "Fashion" label with its ONE direct product — the
    // aggregate never rolled up, and still does not), AND
    // the tree rides along additively.
    let (status, body) = call(&app, Method::GET, "/v1/categories", None, None).await;
    assert_eq!(status, StatusCode::OK, "categories: {body}");
    let legacy = body["categories"].as_array().expect("legacy categories");
    let legacy_count = |label: &str| {
        legacy
            .iter()
            .find(|row| row["category"] == label)
            .map(|row| row["product_count"].as_i64().expect("count"))
    };
    assert_eq!(legacy_count("w56c-odd"), Some(1), "legacy odd label: {body}");
    assert_eq!(legacy_count("Fashion"), Some(1), "legacy is NOT rolled up");
    assert_eq!(legacy_count("Panjabi"), Some(1));
    let tree = body["tree"].as_array().expect("tree field present");
    let fashion = find_node(tree, "fashion").expect("fashion in tree");
    // The TREE count is the subtree rollup: root + panjabi
    // + saree products = 3, with 1 directly linked.
    assert_eq!(fashion["product_count"], 3, "subtree rollup: {fashion}");
    assert_eq!(fashion["own_product_count"], 1);
    assert_eq!(fashion["name_bn"], "ফ্যাশন");
    assert_eq!(fashion["icon_key"], "shirt");
    let panjabi = find_node(tree, "panjabi").expect("panjabi in tree");
    assert_eq!(panjabi["product_count"], 1);
    assert_eq!(panjabi["depth"], 3);

    // 2. ?flat=1 returns EXACTLY the pre-Wave-56 shape:
    // the aggregate and no tree key at all.
    let (status, flat) = call(&app, Method::GET, "/v1/categories?flat=1", None, None).await;
    assert_eq!(status, StatusCode::OK, "flat: {flat}");
    assert!(flat["categories"].is_array(), "flat keeps categories");
    assert!(flat.get("tree").is_none(), "flat has no tree: {flat}");

    // 3. The per-slug detail: node + breadcrumb path.
    let (status, detail) = call(&app, Method::GET, "/v1/categories/panjabi", None, None).await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["category"]["slug"], "panjabi");
    assert_eq!(detail["category"]["product_count"], 1);
    let path: Vec<String> = detail["path"]
        .as_array()
        .expect("path")
        .iter()
        .map(|entry| entry["slug"].as_str().expect("slug").to_string())
        .collect();
    assert_eq!(path, vec!["fashion", "mens-fashion", "panjabi"]);
    let (status, _) = call(&app, Method::GET, "/v1/categories/w56c-nope", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown slug 404s");

    // 4. Inactive pruning: a retired parent disappears
    // from the public tree (its slug 404s), its active
    // child surfaces at the top level, and the ADMIN tree
    // keeps the true shape.
    let (status, parent) = call(
        &app,
        Method::POST,
        "/v1/admin/categories",
        Some(json!({ "slug": "w56c-hidden", "name_en": "W56C Hidden" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "hidden parent: {parent}");
    let parent_id = parent["id"].as_str().expect("parent id").to_string();
    let (status, child) = call(
        &app,
        Method::POST,
        "/v1/admin/categories",
        Some(json!({
            "slug": "w56c-hidden-child",
            "name_en": "W56C Hidden Child",
            "parent_id": parent_id
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "hidden child: {child}");
    let (status, _) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/categories/{parent_id}"),
        Some(json!({ "is_active": false })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "retire parent");

    let (status, body) = call(&app, Method::GET, "/v1/categories/tree", None, None).await;
    assert_eq!(status, StatusCode::OK, "public tree: {body}");
    let public_nodes = body["tree"].as_array().expect("public tree");
    assert!(
        find_node(public_nodes, "w56c-hidden").is_none(),
        "inactive parent is pruned publicly"
    );
    let promoted = public_nodes
        .iter()
        .find(|node| node["slug"] == "w56c-hidden-child")
        .expect("active child surfaces at the top level");
    assert_eq!(promoted["depth"], 1, "orphan rule depth: {promoted}");
    let (status, _) = call(&app, Method::GET, "/v1/categories/w56c-hidden", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "inactive slug 404s");

    let (status, admin_tree) = call(
        &app,
        Method::GET,
        "/v1/admin/categories/tree",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin tree: {admin_tree}");
    let admin_nodes = admin_tree["tree"].as_array().expect("admin tree");
    let admin_parent = find_node(admin_nodes, "w56c-hidden").expect("admin keeps parent");
    assert_eq!(admin_parent["is_active"], false);
    assert!(
        find_node(admin_nodes, "w56c-hidden-child").is_some(),
        "admin keeps the child under it"
    );

    // 5. The home feed: `taxonomy` rides additively with
    // the featured seed nodes first (Fashion, then
    // Electronics — display order within the featured
    // group), and the legacy tiles are untouched.
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "feed: {feed}");
    let taxonomy = feed["taxonomy"].as_array().expect("taxonomy field");
    assert_eq!(taxonomy[0]["slug"], "fashion", "featured first: {taxonomy:?}");
    assert_eq!(taxonomy[1]["slug"], "electronics");
    assert!(
        taxonomy.iter().all(|node| node["depth"] == 1),
        "the strip is top-level only"
    );
    let feed_fashion = taxonomy[0].clone();
    assert_eq!(feed_fashion["product_count"], 3, "feed rollup agrees");
    let feed_legacy = feed["categories"].as_array().expect("legacy tiles");
    assert!(
        feed_legacy.iter().any(|tile| tile["category"] == "w56c-odd"),
        "legacy tiles still carry the free-text vocabulary"
    );

    // 6. Suggestions: the "panjabi" fragment matches the
    // Panjabi product; its label resolves into a taxonomy
    // node with the full path, alongside the Wave 53
    // text fields, unchanged in shape.
    let (status, suggest) = call(
        &app,
        Method::GET,
        "/v1/search/suggest?q=panjabi",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suggest: {suggest}");
    assert!(
        suggest["categories"]
            .as_array()
            .expect("text categories")
            .iter()
            .any(|row| row["category"] == "Panjabi"),
        "text suggestions unchanged: {suggest}"
    );
    let nodes = suggest["category_nodes"].as_array().expect("category nodes");
    let node = nodes
        .iter()
        .find(|row| row["slug"] == "panjabi")
        .expect("panjabi resolved to a node");
    assert_eq!(node["label"], "Panjabi");
    assert_eq!(node["product_count"], 1);
    let node_path: Vec<String> = node["path"]
        .as_array()
        .expect("node path")
        .iter()
        .map(|entry| entry["slug"].as_str().expect("slug").to_string())
        .collect();
    assert_eq!(node_path, vec!["fashion", "mens-fashion", "panjabi"]);
}
