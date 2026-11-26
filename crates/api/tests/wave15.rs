//! Wave 15 integration test: Postgres-native product search —
//! text matches in the English title, the Bangla title and the
//! description (a description-only match ranks below a title
//! match), the category / price-range / store filters compose,
//! price sorts order correctly, pagination reports the total and
//! slices by page, drafts and suspended-store products never
//! appear, suggestions return product titles and stores (and
//! nothing for a one-character query), and invalid parameters are
//! a 400. Helpers follow the Wave 9/13 patterns (dev_otp read
//! from the request-otp response; products created through
//! POST /v1/products with store_id and an explicit status).
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so
//! a plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`.

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

/// Create a product with the Wave 15 fields; returns the product id.
#[allow(clippy::too_many_arguments)]
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    title_bn: Option<&str>,
    description: Option<&str>,
    category: Option<&str>,
    price_poisha: i64,
    status: &str,
) -> String {
    let (code, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "title_bn": title_bn,
            "description": description,
            "category": category,
            "price_poisha": price_poisha,
            "stock": 10,
            "status": status,
        })),
        Some(token),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "create product {title}: {body}");
    // The Wave 15 category round-trips through the product view.
    assert_eq!(
        body["category"],
        json!(category),
        "category stored for {title}"
    );
    body["id"].as_str().expect("product id").to_string()
}

/// Run one public search; returns the results envelope.
async fn search(app: &Router, query: &str) -> Value {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/search?{query}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "search {query}: {body}");
    body
}

/// The ids of a search envelope, in result order.
fn result_ids(results: &Value) -> Vec<String> {
    results["items"]
        .as_array()
        .expect("items is an array")
        .iter()
        .map(|item| item["id"].as_str().expect("item id").to_string())
        .collect()
}

#[tokio::test]
async fn wave15_search_end_to_end() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 15 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE sidra_suggestions, messages, conversations, platform_settings, \
         coupon_redemptions, coupons, payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, order_items, sub_orders, \
         orders, cart_items, carts, products, stores, otp_codes, users \
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "wave15-test-secret".to_string(), true);
    let app = build_app(state);

    // --- Seed: two visible stores plus one that gets suspended. --
    let seller_a = login(&app, "01715150001").await;
    let seller_b = login(&app, "01815150002").await;
    let seller_c = login(&app, "01915150003").await;
    let store_a = create_store(&app, &seller_a, "wave15-alpha").await;
    let store_b = create_store(&app, &seller_b, "wave15-beta").await;
    let store_c = create_store(&app, &seller_c, "wave15-gamma").await;

    // Store A: a title-prefix match, a second title match, a
    // description-only match, a Bangla-title product, a draft.
    let cotton_shirt = create_product(
        &app,
        &seller_a,
        &store_a,
        "Cotton Shirt",
        Some("সুতি শার্ট"),
        Some("Soft breathable fabric for summer"),
        Some("clothing"),
        50_000,
        "active",
    )
    .await;
    let premium_tee = create_product(
        &app,
        &seller_a,
        &store_a,
        "Premium Cotton T-Shirt",
        None,
        Some("Heavyweight tee, pre-shrunk"),
        Some("clothing"),
        80_000,
        "active",
    )
    .await;
    let wallet = create_product(
        &app,
        &seller_a,
        &store_a,
        "Leather Wallet",
        None,
        Some("Pairs well with a cotton shirt outfit"),
        Some("accessories"),
        30_000,
        "active",
    )
    .await;
    let saree = create_product(
        &app,
        &seller_a,
        &store_a,
        "Silk Saree",
        Some("সিল্ক শাড়ি"),
        Some("Handloom silk"),
        Some("clothing"),
        250_000,
        "active",
    )
    .await;
    let _draft_hoodie = create_product(
        &app,
        &seller_a,
        &store_a,
        "Draft Hoodie",
        None,
        Some("Not published yet"),
        Some("clothing"),
        60_000,
        "draft",
    )
    .await;

    // Store B: two more visible products (pagination + filters).
    let jeans = create_product(
        &app,
        &seller_b,
        &store_b,
        "Denim Jeans",
        None,
        Some("Slim fit denim"),
        Some("clothing"),
        120_000,
        "active",
    )
    .await;
    let cap = create_product(
        &app,
        &seller_b,
        &store_b,
        "Cotton Cap",
        None,
        Some("Adjustable strap"),
        Some("accessories"),
        20_000,
        "active",
    )
    .await;

    // Store C: one active product, then the store is suspended.
    let _gamma_shirt = create_product(
        &app,
        &seller_c,
        &store_c,
        "Gamma Shirt",
        None,
        Some("From a store about to be suspended"),
        Some("clothing"),
        40_000,
        "active",
    )
    .await;
    sqlx::query("UPDATE stores SET status = 'suspended' WHERE id = $1")
        .bind(store_c.parse::<uuid::Uuid>().expect("store c id is a uuid"))
        .execute(&pool)
        .await
        .expect("suspend store c");

    // --- Text matching + ranking. --------------------------------
    // "cotton" hits two titles in A, one title in B, and the
    // wallet's description only. Title matches rank above the
    // description-only match.
    let results = search(&app, "q=cotton").await;
    let ids = result_ids(&results);
    assert_eq!(results["total"], 4, "cotton matches: {results}");
    for expected in [&cotton_shirt, &premium_tee, &wallet, &cap] {
        assert!(ids.contains(expected), "{expected} in cotton results");
    }
    let pos = |id: &str| ids.iter().position(|x| x == id).expect("present");
    assert!(
        pos(&cotton_shirt) < pos(&wallet),
        "title match ranks above description-only match: {ids:?}"
    );
    assert!(
        pos(&premium_tee) < pos(&wallet),
        "second title match also ranks above description-only: {ids:?}"
    );

    // The Bangla title is searchable (percent-encoded শাড়ি).
    let results = search(&app, "q=%E0%A6%B6%E0%A6%BE%E0%A6%A1%E0%A6%BC%E0%A6%BF").await;
    assert_eq!(results["total"], 1, "bangla title search: {results}");
    assert_eq!(result_ids(&results), vec![saree.clone()]);

    // --- Filters compose. -----------------------------------------
    // Category only (case-insensitive): four visible clothing
    // products — the draft hoodie and the suspended store's shirt
    // are clothing too, and both must stay hidden.
    let results = search(&app, "category=Clothing").await;
    assert_eq!(results["total"], 4, "category filter: {results}");
    assert_eq!(
        result_ids(&results).len(),
        4,
        "category page holds all four"
    );

    // Price range only: 40_000..=100_000 keeps the shirt and the
    // tee, drops the wallet, cap, jeans and saree.
    let results = search(&app, "min_price_poisha=40000&max_price_poisha=100000").await;
    assert_eq!(results["total"], 2, "price range: {results}");
    let ids = result_ids(&results);
    assert!(ids.contains(&cotton_shirt) && ids.contains(&premium_tee));

    // One store only.
    let results = search(&app, "store_slug=wave15-beta").await;
    assert_eq!(results["total"], 2, "store filter: {results}");
    let ids = result_ids(&results);
    assert!(ids.contains(&jeans) && ids.contains(&cap));

    // Text + category together: cotton in accessories is the
    // wallet (description) and the cap (title) — not the shirts.
    let results = search(&app, "q=cotton&category=accessories").await;
    assert_eq!(results["total"], 2, "text + category: {results}");
    let ids = result_ids(&results);
    assert!(ids.contains(&wallet) && ids.contains(&cap));

    // --- Sorts. ----------------------------------------------------
    let results = search(&app, "sort=price_asc&per_page=48").await;
    let prices: Vec<i64> = results["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["price_poisha"].as_i64().expect("price"))
        .collect();
    assert_eq!(
        prices,
        vec![20_000, 30_000, 50_000, 80_000, 120_000, 250_000],
        "price_asc over all six visible products"
    );
    let results = search(&app, "sort=price_desc&per_page=48").await;
    let first = results["items"][0]["id"].as_str().expect("first id");
    assert_eq!(first, saree, "price_desc puts the saree first");

    // --- Pagination: total + page slices (price order is
    // deterministic, unlike newest-first timestamps). -------------
    let page1 = search(&app, "sort=price_asc&per_page=2&page=1").await;
    assert_eq!(page1["total"], 6, "six visible products in total");
    assert_eq!(page1["page"], 1);
    assert_eq!(page1["per_page"], 2);
    assert_eq!(result_ids(&page1), vec![cap.clone(), wallet.clone()]);
    let page2 = search(&app, "sort=price_asc&per_page=2&page=2").await;
    assert_eq!(
        result_ids(&page2),
        vec![cotton_shirt.clone(), premium_tee.clone()]
    );
    let page3 = search(&app, "sort=price_asc&per_page=2&page=3").await;
    assert_eq!(result_ids(&page3), vec![jeans.clone(), saree.clone()]);

    // --- Invisible products never appear. --------------------------
    let results = search(&app, "q=hoodie").await;
    assert_eq!(results["total"], 0, "draft product hidden: {results}");
    let results = search(&app, "q=gamma").await;
    assert_eq!(
        results["total"], 0,
        "suspended store's product hidden: {results}"
    );

    // --- Suggestions. ----------------------------------------------
    let (status, body) = call(&app, Method::GET, "/v1/search/suggest?q=sh", None, None).await;
    assert_eq!(status, StatusCode::OK, "suggest: {body}");
    let suggested_titles: Vec<&str> = body["products"]
        .as_array()
        .expect("suggested products")
        .iter()
        .map(|p| p["title"].as_str().expect("suggested title"))
        .collect();
    assert!(
        suggested_titles.contains(&"Cotton Shirt"),
        "suggest finds the shirt: {suggested_titles:?}"
    );
    assert!(
        !suggested_titles.contains(&"Gamma Shirt"),
        "suspended store never suggested: {suggested_titles:?}"
    );

    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/search/suggest?q=wave15",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "store suggest: {body}");
    let suggested_slugs: Vec<&str> = body["stores"]
        .as_array()
        .expect("suggested stores")
        .iter()
        .map(|s| s["slug"].as_str().expect("suggested slug"))
        .collect();
    assert!(
        suggested_slugs.contains(&"wave15-alpha"),
        "suggest finds the store: {suggested_slugs:?}"
    );
    assert!(
        !suggested_slugs.contains(&"wave15-gamma"),
        "suspended store not suggested: {suggested_slugs:?}"
    );

    // One character is below the suggestion threshold.
    let (status, body) = call(&app, Method::GET, "/v1/search/suggest?q=s", None, None).await;
    assert_eq!(status, StatusCode::OK, "short suggest: {body}");
    assert_eq!(body["products"].as_array().expect("products").len(), 0);
    assert_eq!(body["stores"].as_array().expect("stores").len(), 0);

    // --- Invalid parameters are a 400. ------------------------------
    let (status, body) = call(&app, Method::GET, "/v1/search?sort=banana", None, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad sort: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/search?min_price_poisha=1000&max_price_poisha=500",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "min above max: {body}");
}
