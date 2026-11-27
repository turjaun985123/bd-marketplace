//! Wave 56 integration test: subtree browse and the
//! Wave 53 facet convention under the new filter.
//! - `GET /v1/search?category_slug=<slug>` scopes items
//!   AND total to the slug's whole subtree (a parent slug
//!   returns its descendants' products);
//! - an unknown slug matches nothing (total 0), never an
//!   error — the unknown-text-label precedent;
//! - THE CONVENTION HOLDS: the slug filter belongs to the
//!   categories facet group, so the categories facet is
//!   computed WITHOUT it (a slug-scoped browse still
//!   reports the whole catalogue's category counts),
//!   while the other facets (stores, price) ARE computed
//!   over the slug-scoped set;
//! - slug and text category filters combine with AND;
//! - the product list endpoints take the same scoping
//!   (`/v1/products` and the store list — the seller
//!   filter's server side).
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
    price: i64,
    extra: Value,
) -> Value {
    let mut body = json!({
        "store_id": store_id,
        "title": title,
        "price_poisha": price,
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

/// GET a browse and return its body (asserting 200).
async fn browse(app: &Router, uri: &str) -> Value {
    let (status, body) = call(app, Method::GET, uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "browse {uri}: {body}");
    body
}

/// The titles a browse returned, sorted.
fn titles(body: &Value) -> Vec<String> {
    let mut names: Vec<String> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["title"].as_str().expect("title").to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn wave56_subtree_browse_and_facets() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "w56d-store").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    let panjabi_id = seed_id(&app, "panjabi").await;
    let saree_id = seed_id(&app, "saree").await;

    // Fixtures: A deep in the Fashion branch, B in a
    // sibling branch, C under Electronics (by label), D
    // free-text. Prices differ so the price facet can
    // prove which set it counted.
    create_product(
        &app,
        &seller,
        &store_id,
        "W56D Alpha",
        10_000,
        json!({ "category_id": panjabi_id }),
    )
    .await;
    create_product(
        &app,
        &seller,
        &store_id,
        "W56D Beta",
        20_000,
        json!({ "category_id": saree_id }),
    )
    .await;
    create_product(
        &app,
        &seller,
        &store_id,
        "W56D Gamma",
        30_000,
        json!({ "category": "Electronics" }),
    )
    .await;
    create_product(
        &app,
        &seller,
        &store_id,
        "W56D Delta",
        40_000,
        json!({ "category": "w56d-misc" }),
    )
    .await;

    // 1. A parent slug returns its whole subtree: Fashion
    // covers the Panjabi and Saree products, nothing else.
    let body = browse(&app, "/v1/search?category_slug=fashion").await;
    assert_eq!(body["total"], 2, "fashion subtree total: {body}");
    assert_eq!(titles(&body), vec!["W56D Alpha", "W56D Beta"]);

    // 2. A mid-level slug narrows to its own subtree.
    let body = browse(&app, "/v1/search?category_slug=mens-fashion").await;
    assert_eq!(body["total"], 1);
    assert_eq!(titles(&body), vec!["W56D Alpha"]);
    let body = browse(&app, "/v1/search?category_slug=womens-fashion").await;
    assert_eq!(body["total"], 1);
    assert_eq!(titles(&body), vec!["W56D Beta"]);

    // 3. Two slugs union their subtrees.
    let body = browse(
        &app,
        "/v1/search?category_slug=mens-fashion&category_slug=electronics",
    )
    .await;
    assert_eq!(body["total"], 2, "union: {body}");
    assert_eq!(titles(&body), vec!["W56D Alpha", "W56D Gamma"]);

    // 4. An unknown slug matches nothing — total 0, no
    // error, the unknown-label precedent.
    let body = browse(&app, "/v1/search?category_slug=w56d-nope").await;
    assert_eq!(body["total"], 0);
    assert_eq!(titles(&body), Vec::<String>::new());

    // 5. THE FACET CONVENTION: with ONLY the slug filter
    // active, the categories facet is computed WITHOUT
    // the categories group — it reports the whole
    // catalogue's labels (including Electronics and the
    // free-text label the slug scope excluded from the
    // items) — while the price facet IS scoped (it counts
    // a different group): its span covers only the two
    // in-scope prices.
    let body = browse(&app, "/v1/search?category_slug=fashion").await;
    let facet_count = |label: &str| {
        body["facets"]["categories"]
            .as_array()
            .expect("category facet")
            .iter()
            .find(|row| row["category"] == label)
            .map(|row| row["count"].as_i64().expect("count"))
    };
    assert_eq!(facet_count("Panjabi"), Some(1), "facet: {body}");
    assert_eq!(facet_count("Saree"), Some(1));
    assert_eq!(
        facet_count("Electronics"),
        Some(1),
        "the facet ignores the slug scope: {body}"
    );
    assert_eq!(facet_count("w56d-misc"), Some(1));
    assert_eq!(
        body["facets"]["price"]["min_poisha"], 10_000,
        "the price facet IS slug-scoped: {body}"
    );
    assert_eq!(body["facets"]["price"]["max_poisha"], 20_000);
    // The store facet is scoped too (different group):
    // the one store, with the two in-scope products.
    let store_facet = &body["facets"]["stores"].as_array().expect("store facet")[0];
    assert_eq!(store_facet["slug"], "w56d-store");
    assert_eq!(store_facet["count"], 2);

    // 6. Slug and text filters combine with AND: inside
    // Fashion, only the Saree-labelled product remains.
    let body = browse(&app, "/v1/search?category_slug=fashion&category=Saree").await;
    assert_eq!(body["total"], 1);
    assert_eq!(titles(&body), vec!["W56D Beta"]);
    // ...and a text label outside the subtree empties it.
    let body = browse(&app, "/v1/search?category_slug=fashion&category=Electronics").await;
    assert_eq!(body["total"], 0);

    // 7. Browse items carry their taxonomy paths (the
    // card breadcrumb data) — Alpha's is three deep.
    let body = browse(&app, "/v1/search?category_slug=panjabi").await;
    let item = &body["items"].as_array().expect("items")[0];
    let path: Vec<String> = item["category_path"]
        .as_array()
        .expect("item path")
        .iter()
        .map(|entry| entry["slug"].as_str().expect("slug").to_string())
        .collect();
    assert_eq!(path, vec!["fashion", "mens-fashion", "panjabi"]);

    // 8. The plain product list takes the same scoping
    // (the seller filter's server side): the store list
    // narrows to the subtree, unscoped lists everything.
    let body = browse(&app, "/v1/stores/w56d-store/products?category_slug=fashion").await;
    assert_eq!(body["total"], 2, "store list scoped: {body}");
    let body = browse(&app, "/v1/stores/w56d-store/products").await;
    assert_eq!(body["total"], 4);
    let body = browse(&app, "/v1/products?category_slug=womens-fashion").await;
    assert_eq!(body["total"], 1);
    assert_eq!(titles(&body), vec!["W56D Beta"]);
    let body = browse(&app, "/v1/products?category_slug=w56d-nope").await;
    assert_eq!(body["total"], 0);
}
