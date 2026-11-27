//! Wave 56 integration test: product <-> taxonomy linking.
//! The sync rule end to end —
//! - create with `category_id` validates the node and
//!   syncs the stored text label to the node's English
//!   name; views carry `category_id` + `category_path`;
//! - create with only a text label links on the domain's
//!   exact-match rule and stores the label EXACTLY as
//!   typed (never rewritten);
//! - a label matching nothing stays free-text, NULL link;
//! - an unknown or inactive `category_id` is a 400;
//! - PATCH mirrors create: an id wins and re-syncs, a
//!   label-only patch re-resolves (match links, no match
//!   UNLINKS), a patch touching neither keeps the link.
//!
//! Seed taxonomy ids are read from the public tree (the
//! seed is shared reference data; `categories` is never
//! truncated — see wave56.rs's module doc).
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

/// POST one product with extra fields merged in; returns (status, body).
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    extra: Value,
) -> (StatusCode, Value) {
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
    call(app, Method::POST, "/v1/products", Some(body), Some(token)).await
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

/// A seed node's id, read from the public tree endpoint
/// (the seller picker's own data source).
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

/// The slug chain of a product view's `category_path`.
fn path_slugs(product: &Value) -> Vec<String> {
    product["category_path"]
        .as_array()
        .expect("category_path array")
        .iter()
        .map(|entry| entry["slug"].as_str().expect("path slug").to_string())
        .collect()
}

#[tokio::test]
async fn wave56_product_category_linking() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let admin = login_admin(&app, &pool, "01711111111").await;
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "w56b-store").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    let fashion_id = seed_id(&app, "fashion").await;
    let electronics_id = seed_id(&app, "electronics").await;
    let panjabi_id = seed_id(&app, "panjabi").await;
    let footwear_id = seed_id(&app, "footwear").await;

    // 1. Create with category_id: the label syncs to the
    // node's English name, and the create response itself
    // carries the link + a one-entry path.
    let (status, by_id) = create_product(
        &app,
        &seller,
        &store_id,
        "W56B By Id",
        json!({ "category_id": fashion_id }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create by id: {by_id}");
    assert_eq!(by_id["category_id"], fashion_id);
    assert_eq!(by_id["category"], "Fashion", "label synced: {by_id}");
    assert_eq!(path_slugs(&by_id), vec!["fashion"]);
    let by_id_id = by_id["id"].as_str().expect("product id").to_string();

    // 2. The detail view carries the same path (the
    // breadcrumb source), and so does the store list.
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{by_id_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["product"]["category_id"], fashion_id);
    assert_eq!(path_slugs(&detail["product"]), vec!["fashion"]);
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/stores/w56b-store/products",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "store list: {list}");
    let listed = list["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["id"] == by_id_id)
        .cloned()
        .expect("product listed");
    assert_eq!(path_slugs(&listed), vec!["fashion"]);

    // 3. Create with only a matching label: the link
    // resolves (slug match, case-insensitive) and the
    // label is stored EXACTLY as typed — "electronics",
    // not the node's "Electronics".
    let (status, by_text) = create_product(
        &app,
        &seller,
        &store_id,
        "W56B By Text",
        json!({ "category": "electronics" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create by text: {by_text}");
    assert_eq!(by_text["category_id"], electronics_id, "resolved: {by_text}");
    assert_eq!(by_text["category"], "electronics", "label untouched");
    assert_eq!(path_slugs(&by_text), vec!["electronics"]);

    // 4. A Bangla label resolves by the Bangla name too.
    let (status, by_bn) = create_product(
        &app,
        &seller,
        &store_id,
        "W56B By Bangla",
        json!({ "category": "পাঞ্জাবি" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create by bangla: {by_bn}");
    assert_eq!(by_bn["category_id"], panjabi_id);
    assert_eq!(
        path_slugs(&by_bn),
        vec!["fashion", "mens-fashion", "panjabi"]
    );

    // 5. A label matching nothing stays free-text: NULL
    // link, empty path, label intact. (This is also the
    // backfill rule's shape — the migration applies the
    // same exact-match resolution; the rule itself is
    // what this step pins, per the wave brief.)
    let (status, free) = create_product(
        &app,
        &seller,
        &store_id,
        "W56B Free Text",
        json!({ "category": "W56B Oddments" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "free text: {free}");
    assert!(free["category_id"].is_null(), "no link: {free}");
    assert_eq!(free["category"], "W56B Oddments");
    assert_eq!(path_slugs(&free), Vec::<String>::new());
    let free_id = free["id"].as_str().expect("product id").to_string();

    // 6. An unknown category_id is a 400, and so is an
    // INACTIVE one: make a throwaway node, retire it,
    // then try to file under it.
    let (status, err) = create_product(
        &app,
        &seller,
        &store_id,
        "W56B Unknown Category",
        json!({ "category_id": "00000000-0000-0000-0000-000000000000" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown category: {err}");

    let (status, retired) = call(
        &app,
        Method::POST,
        "/v1/admin/categories",
        Some(json!({ "slug": "w56b-retired", "name_en": "W56B Retired" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "retired node: {retired}");
    let retired_id = retired["id"].as_str().expect("node id").to_string();
    let (status, _) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/categories/{retired_id}"),
        Some(json!({ "is_active": false })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "retire node");
    let (status, err) = create_product(
        &app,
        &seller,
        &store_id,
        "W56B Inactive Category",
        json!({ "category_id": retired_id }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "inactive category: {err}");

    // 7. PATCH with a category_id wins outright: the
    // free-text product files under Panjabi, its label
    // re-syncs, and its path is the full three levels.
    let (status, patched) = call(
        &app,
        Method::PATCH,
        &format!("/v1/products/{free_id}"),
        Some(json!({ "category_id": panjabi_id })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch by id: {patched}");
    assert_eq!(patched["category_id"], panjabi_id);
    assert_eq!(patched["category"], "Panjabi");
    assert_eq!(
        path_slugs(&patched),
        vec!["fashion", "mens-fashion", "panjabi"]
    );

    // 8. A label-only PATCH re-resolves: "Footwear" links
    // the by-id product to the Footwear node (label kept
    // as typed — it happens to equal the node name here).
    let (status, relinked) = call(
        &app,
        Method::PATCH,
        &format!("/v1/products/{by_id_id}"),
        Some(json!({ "category": "Footwear" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch by label: {relinked}");
    assert_eq!(relinked["category_id"], footwear_id);
    assert_eq!(path_slugs(&relinked), vec!["fashion", "footwear"]);

    // 9. A label-only PATCH to a non-matching label
    // UNLINKS: the product leaves the taxonomy rather
    // than sitting under a node its label contradicts.
    let (status, unlinked) = call(
        &app,
        Method::PATCH,
        &format!("/v1/products/{by_id_id}"),
        Some(json!({ "category": "W56B Elsewhere" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch unlink: {unlinked}");
    assert!(unlinked["category_id"].is_null(), "unlinked: {unlinked}");
    assert_eq!(unlinked["category"], "W56B Elsewhere");
    assert_eq!(path_slugs(&unlinked), Vec::<String>::new());

    // 10. A PATCH touching neither category field keeps
    // the stored link exactly as it was.
    let (status, untouched) = call(
        &app,
        Method::PATCH,
        &format!("/v1/products/{free_id}"),
        Some(json!({ "title": "W56B Free Text Renamed" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch title only: {untouched}");
    assert_eq!(untouched["category_id"], panjabi_id);
    assert_eq!(untouched["category"], "Panjabi");
}
