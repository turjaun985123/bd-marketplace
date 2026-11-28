//! Admin stores remake, test B: the list's status / tier
//! filters, search, sort orders, fail-closed validation of
//! unknown filter values, and pagination — plus the rule
//! that the summary describes the whole queue, not the
//! filtered page. Helpers are copied from the Wave 9 test
//! on purpose — integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI
//! provides a Postgres 16 service and sets `DATABASE_URL`.

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

/// Suspend a store with a note; asserts success.
async fn suspend_store(app: &Router, admin: &str, store_id: &str) {
    let uri = format!("/v1/admin/stores/{store_id}/status");
    let (status, body) = call(
        app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": "suspended", "note": "Filter fixture suspension" })),
        Some(admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suspend store: {body}");
}

/// GET the admin store list with a query string; asserts 200.
async fn list(app: &Router, admin: &str, query: &str) -> Value {
    let uri = format!("/v1/admin/stores?{query}");
    let (status, body) = call(app, Method::GET, &uri, None, Some(admin)).await;
    assert_eq!(status, StatusCode::OK, "list ?{query}: {body}");
    body
}

/// The item ids of a list body, in page order.
fn ids(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|item| item["id"].as_str().expect("item id").to_string())
        .collect()
}

/// The item ids of a list body, sorted (set comparison).
fn id_set(list: &Value) -> Vec<String> {
    let mut found = ids(list);
    found.sort();
    found
}

#[tokio::test]
async fn adminstores_filters_sorts_and_validation() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin stores remake test B");
            return;
        }
    };
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, return_requests, store_follows, \
         store_verification_events, store_verifications, platform_settings, \
         payouts, payout_accounts, review_replies, reviews, shipment_events, \
         shipments, payment_events, payments, order_items, sub_orders, orders, \
         cart_items, carts, products, api_keys, store_members, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "adminstoresb-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    let seller_one = login(&app, "01722222222").await;
    let one = create_store(&app, &seller_one, "bstores-one").await;
    let seller_two = login(&app, "01733333333").await;
    let two = create_store(&app, &seller_two, "bstores-two").await;
    let seller_three = login(&app, "01744444444").await;
    let three = create_store(&app, &seller_three, "bstores-three").await;
    let seller_four = login(&app, "01755555555").await;
    let four = create_store(&app, &seller_four, "bstores-four").await;

    // Tiers by SQL (the verification flow is another
    // surface's fixture); statuses via the real endpoint.
    for (store, tier) in [(&two, "basic"), (&three, "verified"), (&four, "basic")] {
        sqlx::query("UPDATE stores SET verified_tier = $1 WHERE id = $2::uuid")
            .bind(tier)
            .bind(store)
            .execute(&pool)
            .await
            .expect("set tier");
    }
    suspend_store(&app, &admin, &three).await;
    suspend_store(&app, &admin, &four).await;
    // Deterministic ages for the sort orders.
    for (store, days) in [(&one, 3), (&two, 2), (&three, 1)] {
        sqlx::query("UPDATE stores SET created_at = now() - ($1 || ' days')::interval WHERE id = $2::uuid")
            .bind(days.to_string())
            .bind(store)
            .execute(&pool)
            .await
            .expect("backdate store");
    }

    // Status filters.
    let active = list(&app, &admin, "status=active").await;
    assert_eq!(active["total"], 2);
    let mut expected = vec![one.clone(), two.clone()];
    expected.sort();
    assert_eq!(id_set(&active), expected);
    // The summary ignores the filter — it describes the queue.
    assert_eq!(active["summary"]["total"], 4);
    assert_eq!(active["summary"]["active"], 2);
    assert_eq!(active["summary"]["suspended"], 2);
    let suspended = list(&app, &admin, "status=suspended").await;
    assert_eq!(suspended["total"], 2);
    let mut expected = vec![three.clone(), four.clone()];
    expected.sort();
    assert_eq!(id_set(&suspended), expected);

    // Tier filters, alone and combined.
    let basic = list(&app, &admin, "tier=basic").await;
    assert_eq!(basic["total"], 2);
    let mut expected = vec![two.clone(), four.clone()];
    expected.sort();
    assert_eq!(id_set(&basic), expected);
    let combo = list(&app, &admin, "tier=verified&status=suspended").await;
    assert_eq!(combo["total"], 1);
    assert_eq!(ids(&combo), vec![three.clone()]);

    // Search matches name or slug, case-insensitively.
    let found = list(&app, &admin, "q=bstores-t").await;
    assert_eq!(found["total"], 2);
    let mut expected = vec![two.clone(), three.clone()];
    expected.sort();
    assert_eq!(id_set(&found), expected);
    let found = list(&app, &admin, "q=BSTORES-ONE").await;
    assert_eq!(found["total"], 1);
    assert_eq!(ids(&found), vec![one.clone()]);

    // Sorts: default newest first; oldest reverses it;
    // name_asc orders by the store name ("Store bstores-…").
    let newest = list(&app, &admin, "").await;
    assert_eq!(
        ids(&newest),
        vec![four.clone(), three.clone(), two.clone(), one.clone()]
    );
    let oldest = list(&app, &admin, "sort=oldest").await;
    assert_eq!(
        ids(&oldest),
        vec![one.clone(), two.clone(), three.clone(), four.clone()]
    );
    let by_name = list(&app, &admin, "sort=name_asc").await;
    assert_eq!(
        ids(&by_name),
        vec![four.clone(), one.clone(), three.clone(), two.clone()]
    );

    // Unknown filter / sort values fail closed with a 400.
    for query in ["status=banana", "tier=banana", "sort=banana"] {
        let uri = format!("/v1/admin/stores?{query}");
        let (status, body) = call(&app, Method::GET, &uri, None, Some(&admin)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "?{query}: {body}");
    }

    // Pagination composes with filters.
    let page2 = list(&app, &admin, "status=suspended&limit=1&page=2").await;
    assert_eq!(page2["total"], 2);
    assert_eq!(page2["items"].as_array().expect("items").len(), 1);
}
