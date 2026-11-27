//! Wave 56 integration test: the seeded taxonomy itself
//! and the label-resolution matrix.
//! - the seed is complete and well-formed: all 53 nodes
//!   (11 roots, 36 children, 6 grandchildren) with the
//!   documented depths, Bangla names everywhere, English
//!   names unique across the tree (the resolution rule's
//!   determinism rests on it), exactly two featured
//!   roots;
//! - the resolution rule, exercised label by label:
//!   slug (any case), English name (any case) and Bangla
//!   name all link; the label is never rewritten.
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

/// POST one product with a free-text category label.
async fn create_labelled_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    label: &str,
) -> Value {
    let (status, created) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": 10_000,
            "stock": 5,
            "status": "active",
            "category": label
        })),
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

/// The 53 seed slugs, grouped by depth (migration 0061).
const SEED_ROOTS: [&str; 11] = [
    "fashion",
    "electronics",
    "grocery",
    "home-living",
    "beauty-health",
    "baby-kids",
    "sports-fitness",
    "motors",
    "agriculture",
    "books-stationery",
    "others",
];
const SEED_CHILDREN: [&str; 36] = [
    "mens-fashion",
    "womens-fashion",
    "footwear",
    "watches-accessories",
    "mobile-phones",
    "mobile-accessories",
    "computers",
    "home-appliances",
    "audio",
    "rice-grains",
    "spices",
    "edible-oil",
    "snacks-drinks",
    "furniture",
    "kitchen",
    "bedding",
    "lighting",
    "skincare",
    "makeup",
    "personal-care",
    "baby-food",
    "diapers",
    "kids-fashion",
    "toys",
    "cricket",
    "football",
    "exercise-equipment",
    "motorcycles",
    "car-accessories",
    "bicycle",
    "seeds",
    "fertilizer",
    "farm-tools",
    "academic-books",
    "islamic-books",
    "office-supplies",
];
const SEED_GRANDCHILDREN: [&str; 6] = [
    "panjabi",
    "shirts-t-shirts",
    "saree",
    "salwar-kameez",
    "laptops",
    "computer-accessories",
];

#[tokio::test]
async fn wave56_seed_integrity_and_resolution_matrix() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let admin = login_admin(&app, &pool, "01711111111").await;

    // 1. Every seed slug is in the admin list, at its
    // documented depth, with a Bangla name; the English
    // names are unique across the seed (the resolution
    // rule's determinism); exactly Fashion and
    // Electronics are featured.
    let (status, list) = call(app, Method::GET, "/v1/admin/categories", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "admin list: {list}");
    let items = list["items"].as_array().expect("admin items");
    let view_of = |slug: &str| {
        items
            .iter()
            .find(|item| item["slug"] == slug)
            .unwrap_or_else(|| panic!("seed slug {slug} in the admin list"))
    };
    let mut english_names: Vec<String> = Vec::new();
    for (slugs, depth) in [
        (&SEED_ROOTS[..], 1),
        (&SEED_CHILDREN[..], 2),
        (&SEED_GRANDCHILDREN[..], 3),
    ] {
        for slug in slugs {
            let view = view_of(slug);
            assert_eq!(view["depth"], depth, "{slug} depth: {view}");
            assert!(
                view["name_bn"].as_str().is_some_and(|name| !name.is_empty()),
                "{slug} carries a Bangla name"
            );
            english_names.push(
                view["name_en"].as_str().expect("english name").to_string(),
            );
        }
    }
    let mut unique = english_names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        english_names.len(),
        "seed English names are unique across the tree"
    );
    for slug in SEED_ROOTS {
        let expected = matches!(slug, "fashion" | "electronics");
        assert_eq!(
            view_of(slug)["is_featured"], expected,
            "{slug} featured flag"
        );
    }
    // Parentage spot-checks across all three levels.
    assert_eq!(view_of("mens-fashion")["parent_id"], seed_id(&app, "fashion").await);
    assert_eq!(
        view_of("panjabi")["parent_id"],
        seed_id(&app, "mens-fashion").await
    );
    assert!(view_of("others")["parent_id"].is_null(), "others is a root");

    // 2. The resolution matrix: each label form links to
    // the same node, and the label itself is NEVER
    // rewritten by linking.
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "w56h-store").await;
    let store_id = store["id"].as_str().expect("store id").to_string();
    let mens_id = seed_id(&app, "mens-fashion").await;
    let fashion_id = seed_id(&app, "fashion").await;

    let by_slug = create_labelled_product(
        &app,
        &seller,
        &store_id,
        "W56H By Slug",
        "mens-fashion",
    )
    .await;
    assert_eq!(by_slug["category_id"], mens_id);
    assert_eq!(by_slug["category"], "mens-fashion", "label kept as typed");

    let by_name = create_labelled_product(
        &app,
        &seller,
        &store_id,
        "W56H By Name",
        "Men's Fashion",
    )
    .await;
    assert_eq!(by_name["category_id"], mens_id);
    assert_eq!(by_name["category"], "Men's Fashion");

    let by_lower_name = create_labelled_product(
        &app,
        &seller,
        &store_id,
        "W56H By Lower Name",
        "men's fashion",
    )
    .await;
    assert_eq!(by_lower_name["category_id"], mens_id);

    let by_bangla = create_labelled_product(
        &app,
        &seller,
        &store_id,
        "W56H By Bangla",
        "পুরুষদের ফ্যাশন",
    )
    .await;
    assert_eq!(by_bangla["category_id"], mens_id);
    assert_eq!(by_bangla["category"], "পুরুষদের ফ্যাশন");

    let upper_slug = create_labelled_product(
        &app,
        &seller,
        &store_id,
        "W56H Upper",
        "FASHION",
    )
    .await;
    assert_eq!(upper_slug["category_id"], fashion_id);

    // A near-miss does NOT link: resolution is exact,
    // never fuzzy — "Fashions" is a different word.
    let near_miss = create_labelled_product(
        &app,
        &seller,
        &store_id,
        "W56H Near Miss",
        "Fashions",
    )
    .await;
    assert!(near_miss["category_id"].is_null(), "no fuzzy linking: {near_miss}");
    assert_eq!(near_miss["category"], "Fashions");
}
