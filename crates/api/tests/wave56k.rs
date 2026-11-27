//! Wave 56 integration test: the REGRESSION contract.
//! The taxonomy was added additively precisely so the
//! Wave 52/53 surfaces keep their pinned shapes; this
//! file re-pins those invariants WITH the taxonomy live:
//! - the home feed's `categories` tiles are still the
//!   free-text aggregate (a "Fashion" tile counting the
//!   three label-carrying products, the Wave 52 pin),
//!   while the feed's `taxonomy` node for the same root
//!   counts the SUBTREE (4) — two vocabularies, each
//!   honest about what it counts;
//! - `GET /v1/categories`' `categories` field and the
//!   home feed's tiles are the same SET (the Wave 53
//!   "one category vocabulary" invariant);
//! - browse facets still group by the text column with
//!   the same shape, and suggestions still group labels
//!   by text.
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

/// The (category, product_count) set of a legacy tile
/// list, sorted for comparison.
fn tile_set(tiles: &Value) -> Vec<(String, i64)> {
    let mut set: Vec<(String, i64)> = tiles
        .as_array()
        .expect("tiles")
        .iter()
        .map(|tile| {
            (
                tile["category"].as_str().expect("tile label").to_string(),
                tile["product_count"].as_i64().expect("tile count"),
            )
        })
        .collect();
    set.sort();
    set
}

#[tokio::test]
async fn wave56_legacy_surfaces_keep_their_pinned_shapes() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01733333333").await;
    let store = create_store(&app, &seller, "w56k-store").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    // The Wave 52 fixture shape: three products carrying
    // the free-text label "Fashion" (which now ALSO links
    // them to the fashion root — the label is unchanged),
    // one deep in the branch by label, one unlinked.
    for title in ["W56K Fashion One", "W56K Fashion Two", "W56K Fashion Three"] {
        create_product(
            &app,
            &seller,
            &store_id,
            title,
            json!({ "category": "Fashion" }),
        )
        .await;
    }
    create_product(
        &app,
        &seller,
        &store_id,
        "W56K Panjabi",
        json!({ "category": "Panjabi" }),
    )
    .await;
    create_product(
        &app,
        &seller,
        &store_id,
        "W56K Odd",
        json!({ "category": "w56k-odd" }),
    )
    .await;

    // 1. The home feed: the legacy tile is the Wave 52
    // pin (label "Fashion", count 3 — direct labels
    // only), and the taxonomy node beside it counts the
    // subtree (the three root products + the Panjabi one
    // = 4). Neither number is the other's.
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "feed: {feed}");
    let feed_tiles = tile_set(&feed["categories"]);
    assert!(
        feed_tiles.contains(&("Fashion".to_string(), 3)),
        "the Wave 52 tile pin holds: {feed_tiles:?}"
    );
    assert!(feed_tiles.contains(&("Panjabi".to_string(), 1)));
    assert!(feed_tiles.contains(&("w56k-odd".to_string(), 1)));
    let taxonomy = feed["taxonomy"].as_array().expect("taxonomy strip");
    let fashion_node = taxonomy
        .iter()
        .find(|node| node["slug"] == "fashion")
        .expect("fashion node in the strip");
    assert_eq!(fashion_node["product_count"], 4, "the subtree rollup");
    assert_eq!(fashion_node["own_product_count"], 3, "three sit at the root itself");

    // 2. One vocabulary: the categories endpoint's
    // `categories` set EQUALS the feed's tile set (the
    // Wave 53 invariant), with the tree riding along.
    let (status, endpoint) = call(&app, Method::GET, "/v1/categories", None, None).await;
    assert_eq!(status, StatusCode::OK, "categories endpoint: {endpoint}");
    assert_eq!(
        tile_set(&endpoint["categories"]),
        feed_tiles,
        "endpoint and feed share one category vocabulary"
    );
    assert!(endpoint["tree"].is_array(), "the tree rides along additively");

    // 3. Browse facets: still grouped by the text column,
    // same shape, same counts.
    let (status, browse) = call(&app, Method::GET, "/v1/search", None, None).await;
    assert_eq!(status, StatusCode::OK, "browse: {browse}");
    assert_eq!(browse["total"], 5);
    let facet_count = |label: &str| {
        browse["facets"]["categories"]
            .as_array()
            .expect("category facet")
            .iter()
            .find(|row| row["category"] == label)
            .map(|row| row["count"].as_i64().expect("facet count"))
    };
    assert_eq!(facet_count("Fashion"), Some(3));
    assert_eq!(facet_count("Panjabi"), Some(1));
    assert_eq!(facet_count("w56k-odd"), Some(1));

    // 4. Suggestions: the text grouping is unchanged
    // (every title matches the fragment, so every label
    // appears with its product count).
    let (status, suggest) = call(
        &app,
        Method::GET,
        "/v1/search/suggest?q=w56k",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suggest: {suggest}");
    let suggested: Vec<(String, i64)> = suggest["categories"]
        .as_array()
        .expect("suggested categories")
        .iter()
        .map(|row| {
            (
                row["category"].as_str().expect("label").to_string(),
                row["product_count"].as_i64().expect("count"),
            )
        })
        .collect();
    assert!(suggested.contains(&("Fashion".to_string(), 3)), "{suggested:?}");
    assert!(suggested.contains(&("Panjabi".to_string(), 1)));
    assert!(suggested.contains(&("w56k-odd".to_string(), 1)));
}
