//! Wave 12 integration test: Facebook Page linking and the
//! post-to-product import pipeline. A seller links a Page
//! (identity only), previews drafts parsed from three pasted
//! posts (Bengali-digit price, English price, no price) with NO
//! database writes, imports two valid drafts plus a zero-price
//! one (created 2, skipped 1), re-imports the same posts
//! (everything skipped as already imported), and unlinks. The
//! Graph API source answers 503 because the test app has no Meta
//! app credentials. Driven through the real Axum router with
//! tower oneshot; the small helpers are copied from the Wave 11
//! test on purpose: integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

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
    // The store view carries the (still empty) Facebook link fields.
    assert!(body["facebook_page_id"].is_null(), "no page yet: {body}");
    assert!(body["facebook_page_name"].is_null(), "no page yet: {body}");
    body["id"].as_str().expect("store id").to_string()
}

/// Product row count straight from the database.
async fn product_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM products")
        .fetch_one(pool)
        .await
        .expect("count products")
}

#[tokio::test]
async fn wave12_facebook_import_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 12 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE platform_settings, payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, \
         order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // No FACEBOOK_APP_ID / FACEBOOK_APP_SECRET in the test config:
    // the Graph API must report not_configured throughout.
    let state = AppState::new(Some(pool.clone()), "wave12-test-secret".to_string(), true);
    let app = build_app(state);

    let seller = login(&app, "01712121212").await;
    create_store(&app, &seller, "wave12-store").await;

    // 1. Status before linking: not linked, Graph API not configured.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/facebook/status",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "facebook status: {body}");
    assert_eq!(body["linked"], false, "not linked yet: {body}");
    assert!(body["page_id"].is_null(), "no page id: {body}");
    assert_eq!(
        body["graph_api"], "not_configured",
        "no Meta app credentials in tests: {body}"
    );

    // 2. A malformed page id is a 400; a good link sticks.
    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/facebook",
        Some(json!({ "page_id": "bad page!", "page_name": "Wave12 Shop" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad page id: {body}");

    let (status, body) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/facebook",
        Some(json!({ "page_id": "102345678901234", "page_name": "Wave12 Shop" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "link page: {body}");
    assert_eq!(body["facebook_page_id"], "102345678901234");
    assert_eq!(body["facebook_page_name"], "Wave12 Shop");

    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/facebook/status",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "facebook status: {body}");
    assert_eq!(body["linked"], true, "linked now: {body}");
    assert_eq!(body["page_id"], "102345678901234");
    assert_eq!(body["page_name"], "Wave12 Shop");

    // 3. Preview three posts: Bengali-digit price, English price,
    // and a price-less announcement whose phone number must NOT
    // become a price. Preview writes nothing.
    let posts = json!({ "posts": [
        { "source_ref": "post-1",
          "text": "নতুন শাড়ি এসেছে!\nদামঃ ১,৫০০ টাকা\nঅর্ডার করতে ইনবক্স করুন: 01712345678" },
        { "source_ref": "post-2",
          "text": "Cotton Panjabi for Eid\nPrice: 1200 tk\nHome delivery available" },
        { "source_ref": "post-3",
          "text": "New collection coming soon!\nযোগাযোগ: 01812345678" }
    ]});
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/products/import-preview",
        Some(posts),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "import preview: {body}");
    let drafts = body["drafts"].as_array().expect("drafts array");
    assert_eq!(drafts.len(), 3, "three drafts: {body}");
    assert_eq!(drafts[0]["title"], "নতুন শাড়ি এসেছে!");
    assert_eq!(drafts[0]["price_poisha"], 150_000);
    assert_eq!(drafts[0]["valid"], true, "post-1 valid: {}", drafts[0]);
    assert_eq!(drafts[1]["title"], "Cotton Panjabi for Eid");
    assert_eq!(drafts[1]["price_poisha"], 120_000);
    assert_eq!(drafts[1]["valid"], true, "post-2 valid: {}", drafts[1]);
    assert!(
        drafts[2]["price_poisha"].is_null(),
        "a phone number is not a price: {}",
        drafts[2]
    );
    assert_eq!(drafts[2]["valid"], false, "post-3 invalid: {}", drafts[2]);
    assert!(
        !drafts[2]["errors"].as_array().expect("errors").is_empty(),
        "post-3 names its problem: {}",
        drafts[2]
    );
    assert_eq!(product_count(&pool).await, 0, "preview wrote nothing");

    // The batch limit is enforced: 51 posts is a 400.
    let many = json!({ "posts": (0..51)
        .map(|i| json!({ "source_ref": format!("bulk-{i}"), "text": "দাম 100 টাকা" }))
        .collect::<Vec<_>>() });
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/seller/products/import-preview",
        Some(many),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "51 posts rejected");

    // 4. Import: the two valid drafts plus a zero-price draft.
    // Created 2, skipped 1 (the zero price).
    let import_body = json!({ "drafts": [
        { "source_ref": "post-1", "title": "নতুন শাড়ি এসেছে!",
          "price_poisha": 150_000, "weight_grams": 1000, "stock": 5 },
        { "source_ref": "post-2", "title": "Cotton Panjabi for Eid",
          "price_poisha": 120_000, "weight_grams": 1000, "stock": 3 },
        { "source_ref": "post-zero", "title": "Zero Price Item",
          "price_poisha": 0, "stock": 1 }
    ]});
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/products/import",
        Some(import_body.clone()),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "import: {body}");
    let created = body["created"].as_array().expect("created array");
    assert_eq!(created.len(), 2, "two products created: {body}");
    for product in created {
        assert_eq!(product["source"], "facebook", "provenance: {product}");
        assert_eq!(product["status"], "active", "imports go live: {product}");
    }
    let skipped = body["skipped"].as_array().expect("skipped array");
    assert_eq!(skipped.len(), 1, "one skipped: {body}");
    assert_eq!(skipped[0]["source_ref"], "post-zero");
    assert!(
        skipped[0]["reason"]
            .as_str()
            .expect("skip reason")
            .contains("price"),
        "the skip names the price rule: {}",
        skipped[0]
    );
    assert_eq!(product_count(&pool).await, 2, "exactly two rows written");

    // 5. Re-importing the same drafts is a no-op: both valid ones
    // are skipped as already imported, nothing new is created.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/products/import",
        Some(import_body),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-import: {body}");
    assert_eq!(
        body["created"].as_array().expect("created").len(),
        0,
        "re-import creates nothing: {body}"
    );
    let skipped = body["skipped"].as_array().expect("skipped array");
    assert_eq!(skipped.len(), 3, "all three skipped: {body}");
    let already = skipped
        .iter()
        .filter(|s| s["reason"] == "already imported")
        .count();
    assert_eq!(already, 2, "both valid drafts already imported: {body}");
    assert_eq!(product_count(&pool).await, 2, "still exactly two rows");

    // 6. The imported products show in the store's own product
    // list, stamped with their Facebook provenance.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/stores/wave12-store/products",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "store products: {body}");
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2, "two imported products listed: {body}");
    let refs: Vec<&str> = items
        .iter()
        .map(|p| p["source_ref"].as_str().expect("source_ref"))
        .collect();
    assert!(refs.contains(&"post-1") && refs.contains(&"post-2"), "refs: {refs:?}");
    for product in items {
        assert_eq!(product["source"], "facebook", "provenance: {product}");
    }

    // 7. Unlink clears both fields; the status follows.
    let (status, body) = call(
        &app,
        Method::DELETE,
        "/v1/seller/store/facebook",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unlink: {body}");
    assert!(body["facebook_page_id"].is_null(), "page id cleared: {body}");
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/facebook/status",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "facebook status: {body}");
    assert_eq!(body["linked"], false, "unlinked: {body}");

    // 8. The Graph API source is honestly unconfigured: 503.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/products/import-from-page",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "graph import without app credentials: {body}"
    );
}
