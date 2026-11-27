//! Wave 53 (part J) integration test: the analytics
//! log's counting semantics — what counts, what lists,
//! and what the window actually excludes.
//!
//! - EVERY browse through the handler counts toward the
//!   total, including pure filter browses with no text;
//! - only TEXT searches appear in the text lists (top /
//!   zero-result / popular) — a filter browse has no
//!   words to report;
//! - suggestions do NOT log (they are not searches; the
//!   shopper has not committed to a query);
//! - the window is a real boundary: a row backdated past
//!   it (inserted directly — the API offers no way to
//!   backdate, which is itself the point) falls out of
//!   the 7-day read and stays in the 30-day read, in both
//!   the totals and the zero-result list.
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

/// Run one browse (the log is the subject; results are not).
async fn browse(app: &Router, query: &str) {
    let (status, body) = call(app, Method::GET, &format!("/v1/search?{query}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "browse {query}: {body}");
}

/// Read the admin analytics for a window.
async fn analytics(app: &Router, admin: &str, days: i64) -> Value {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/admin/search-analytics?days={days}"),
        None,
        Some(admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "analytics days={days}: {body}");
    body
}

#[tokio::test]
async fn wave53j_analytics_counting_semantics() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 53j integration test");
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

    let state = AppState::new(Some(pool.clone()), "wave53j-test-secret".to_string(), true);
    let app = build_app(state);
    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // Three filter-only browses and one text browse.
    browse(&app, "category=clothing").await;
    browse(&app, "category=clothing").await;
    browse(&app, "in_stock=true").await;
    browse(&app, "q=LonelyQuery").await;

    // Suggestions are not searches: two suggest calls
    // must not move any number below.
    for _ in 0..2 {
        let (status, body) = call(
            &app,
            Method::GET,
            "/v1/search/suggest?q=Lonely",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "suggest: {body}");
    }

    let read = analytics(&app, &admin, 30).await;
    assert_eq!(
        read["total_searches"], 4,
        "every browse counts, suggestions do not: {read}"
    );
    let top = read["top_searches"].as_array().expect("top searches");
    assert_eq!(top.len(), 1, "only the text search lists: {read}");
    assert_eq!(top[0]["query_text"], "LonelyQuery");
    assert_eq!(top[0]["searches"], 1);
    // The empty catalogue answered 0, so the text search
    // is also the window's only zero-result entry.
    let zero = read["zero_result_searches"].as_array().expect("zero list");
    assert_eq!(zero.len(), 1);
    assert_eq!(zero[0]["query_text"], "LonelyQuery");
    // The filter-only browses DO appear as combinations —
    // with a null query text, exactly as saved.
    let clothing_combo = read["top_filter_combinations"]
        .as_array()
        .expect("combinations")
        .iter()
        .find(|row| {
            row["filters"]["categories"]
                .as_array()
                .map(|cats| cats.len() == 1 && cats[0] == "clothing")
                .unwrap_or(false)
        })
        .expect("the clothing combination is logged")
        .clone();
    assert_eq!(clothing_combo["searches"], 2);
    assert!(clothing_combo["filters"]["q"].is_null());
    // And the stock-flag browse is its own combination,
    // distinguishable from the category one.
    let stock_combo = read["top_filter_combinations"]
        .as_array()
        .expect("combinations")
        .iter()
        .find(|row| row["filters"]["in_stock"] == true)
        .expect("the in-stock combination is logged")
        .clone();
    assert_eq!(stock_combo["searches"], 1);
    assert!(
        stock_combo["filters"]["categories"]
            .as_array()
            .map(|cats| cats.is_empty())
            .unwrap_or(false),
        "no categories on the stock browse: {stock_combo}"
    );

    // The public popular list tells the same story.
    let (status, popular) = call(&app, Method::GET, "/v1/search/popular", None, None).await;
    assert_eq!(status, StatusCode::OK, "popular: {popular}");
    let texts: Vec<&str> = popular["searches"]
        .as_array()
        .expect("popular")
        .iter()
        .map(|row| row["query_text"].as_str().expect("text"))
        .collect();
    assert_eq!(texts, ["LonelyQuery"], "texts only: {popular}");

    // ------------------------------------------------------------------
    // The window boundary: a backdated zero-result row
    // (direct insert — the API cannot backdate) is inside
    // 30 days and outside 7.
    // ------------------------------------------------------------------
    sqlx::query(
        "INSERT INTO search_queries (query_text, result_count, filters_hash, created_at) \
         VALUES ('OldZeroQuery', 0, 'deadbeefdeadbeef', now() - interval '8 days')",
    )
    .execute(&pool)
    .await
    .expect("insert backdated row");

    let read7 = analytics(&app, &admin, 7).await;
    assert_eq!(read7["total_searches"], 4, "the old row is outside 7 days");
    assert!(
        read7["zero_result_searches"]
            .as_array()
            .expect("zero list")
            .iter()
            .all(|row| row["query_text"] != "OldZeroQuery"),
        "and out of the 7-day zero list: {read7}"
    );
    let read30 = analytics(&app, &admin, 30).await;
    assert_eq!(read30["total_searches"], 5, "inside 30 days it counts");
    assert!(
        read30["zero_result_searches"]
            .as_array()
            .expect("zero list")
            .iter()
            .any(|row| row["query_text"] == "OldZeroQuery"),
        "and into the 30-day zero list: {read30}"
    );
}
