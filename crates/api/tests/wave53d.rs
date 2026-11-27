//! Wave 53 (part D) integration test: saved-search
//! canonicalisation and analytics-log integrity — the
//! properties that make the stored data trustworthy.
//!
//! Canonicalisation: the create endpoint validates
//! through the ONE browse validator and stores its
//! canonical form, so equivalent inputs converge —
//! categories deduplicated and order-stable, the default
//! sort spelled out (relevance with a query, newest
//! without), flags defaulted to false, absent bounds
//! null. Two differently-spelled saves of the same
//! browse therefore return identical filter JSON.
//!
//! Analytics integrity: the log's filters_hash is the
//! domain fingerprint of the VALIDATED filters, so two
//! browses whose parameters differ only in order (or in
//! the legacy store spelling) hash identically, while a
//! genuinely different filter hashes differently. The
//! hash is asserted at the table (the read endpoints
//! group by text by design; the hash is the grouping key
//! for filter-level analysis) — and the row still
//! carries no user identifier, which the test verifies
//! by listing the table's columns.
//!
//! Helpers follow the Wave 9 test (self-contained file);
//! gated on `DATABASE_URL` like every integration test.

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

/// Save one search for the token's user; returns (status, body).
async fn save_search(app: &Router, token: &str, name: &str, filters: Value) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/buyer/saved-searches",
        Some(json!({ "name": name, "filters": filters })),
        Some(token),
    )
    .await
}

/// Run one browse (status only matters — the analytics
/// log is the subject here).
async fn browse(app: &Router, query: &str) {
    let (status, body) = call(app, Method::GET, &format!("/v1/search?{query}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "browse {query}: {body}");
}

#[tokio::test]
async fn wave53d_canonicalisation_and_log_integrity() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 53d integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate (the Wave 52 truncate canon plus this
    // wave's tables).
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_images, product_variants, products, store_members, stores, \
         otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "wave53d-test-secret".to_string(), true);
    let app = build_app(state);
    let buyer = login(&app, "01953533001").await;
    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // ------------------------------------------------------------------
    // 1. Canonicalisation: differently-spelled saves of
    //    the same browse converge to identical JSON.
    // ------------------------------------------------------------------
    let (status, first) = save_search(
        &app,
        &buyer,
        "একই খোঁজ ১",
        json!({
            "q": "  shirt  ",
            "categories": ["clothing", "clothing", "footwear"],
            "store_slugs": ["dhaka-fashion"],
            "min_price_poisha": 10_000
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "first save: {first}");
    let (status, second) = save_search(
        &app,
        &buyer,
        "একই খোঁজ ২",
        json!({
            "q": "shirt",
            "categories": ["clothing", "footwear"],
            "store_slugs": ["dhaka-fashion"],
            "min_price_poisha": 10_000,
            "max_price_poisha": null,
            "min_rating": null,
            "in_stock": false,
            "on_sale": false,
            "sort": "relevance"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "second save: {second}");
    assert_eq!(
        first["filters"], second["filters"],
        "equivalent saves store identical filters"
    );
    // The canonical details, spelled out: the query is
    // trimmed, the sort defaults to relevance WITH a
    // query, the flags are real booleans, bounds nullable.
    assert_eq!(first["filters"]["q"], "shirt");
    assert_eq!(first["filters"]["sort"], "relevance");
    assert_eq!(first["filters"]["in_stock"], false);
    assert_eq!(first["filters"]["on_sale"], false);
    assert!(first["filters"]["max_price_poisha"].is_null());
    assert!(first["filters"]["min_rating"].is_null());

    // Without a query the default sort is newest.
    let (status, third) = save_search(&app, &buyer, "শুধু ফিল্টার", json!({
        "categories": ["electronics"]
    }))
    .await;
    assert_eq!(status, StatusCode::CREATED, "third save: {third}");
    assert_eq!(third["filters"]["sort"], "newest");
    assert!(third["filters"]["q"].is_null());

    // A rating of exactly 5 and exactly 0 are both inside
    // the domain's 0..=5 rule (0 stores as null-equivalent
    // behaviour: it validates and round-trips).
    let (status, fourth) = save_search(&app, &buyer, "সেরা রেটিং", json!({
        "min_rating": 5.0
    }))
    .await;
    assert_eq!(status, StatusCode::CREATED, "rating 5 save: {fourth}");
    assert_eq!(fourth["filters"]["min_rating"], 5.0);

    // ------------------------------------------------------------------
    // 2. The log's fingerprint: parameter ORDER and the
    //    legacy spelling do not change the hash; a real
    //    filter change does.
    // ------------------------------------------------------------------
    browse(&app, "q=HashTest&category=clothing&store=wave53d-shop").await;
    browse(&app, "store=wave53d-shop&q=HashTest&category=clothing").await;
    browse(&app, "q=HashTest&store_slug=wave53d-shop&category=clothing").await;
    browse(&app, "q=HashTest&category=footwear").await;

    let hashes: Vec<String> = sqlx::query_scalar(
        "SELECT filters_hash FROM search_queries \
         WHERE query_text = 'HashTest' ORDER BY id ASC",
    )
    .fetch_all(&pool)
    .await
    .expect("read logged hashes");
    assert_eq!(hashes.len(), 4, "four logged browses: {hashes:?}");
    assert_eq!(hashes[0], hashes[1], "parameter order is not identity");
    assert_eq!(
        hashes[0], hashes[2],
        "the legacy store spelling is the same filter"
    );
    assert_ne!(hashes[0], hashes[3], "a different category is a different search");

    // The recorded result counts ride along.
    let counts: Vec<i64> = sqlx::query_scalar(
        "SELECT result_count FROM search_queries \
         WHERE query_text = 'HashTest' ORDER BY id ASC",
    )
    .fetch_all(&pool)
    .await
    .expect("read logged counts");
    assert!(
        counts.iter().all(|count| *count == 0),
        "the empty catalogue answers 0, honestly logged: {counts:?}"
    );

    // The three spellings are ONE combination in the
    // admin read, with the canonical filters readable —
    // the fingerprint groups, the JSON explains.
    let (status, analytics) = call(
        &app,
        Method::GET,
        "/v1/admin/search-analytics",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "analytics: {analytics}");
    let combination = analytics["top_filter_combinations"]
        .as_array()
        .expect("combinations")
        .iter()
        .find(|row| row["filters"]["q"] == "HashTest" && row["filters"]["categories"][0] == "clothing")
        .expect("the HashTest combination is grouped")
        .clone();
    assert_eq!(
        combination["searches"], 3,
        "three spellings, one combination: {combination}"
    );
    assert_eq!(
        combination["filters"]["store_slugs"],
        json!(["wave53d-shop"])
    );

    // ------------------------------------------------------------------
    // 3. The log carries NO identifier: the table's
    //    columns are exactly the ones the design states.
    // ------------------------------------------------------------------
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns \
         WHERE table_name = 'search_queries' ORDER BY ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .expect("read log columns");
    assert_eq!(
        columns,
        [
            "id",
            "query_text",
            "result_count",
            "filters_hash",
            "created_at",
            "filters_json"
        ],
        "no user id, no session, no address — by construction"
    );
}
