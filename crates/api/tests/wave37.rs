//! Wave 37 integration test: third-party / mobile API keys —
//! a key is minted with a JWT (the full key is shown once,
//! only its hash is stored), authenticates its owner on
//! ordinary routes, is narrowed by its scopes (a read-only
//! key cannot write), is refused on the JWT-only routes
//! (admin, payout account, subscription spend, key
//! management itself, exports) whatever its scopes, dies on
//! revocation and on expiry (401), cannot be revoked by a
//! stranger (404), never leaks the full key or the hash
//! through the list endpoint, and is rate limited per key
//! while JWT traffic on the same state is not. The small
//! helpers are copied from the Wave 9 test on purpose, since
//! integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.
//! CI provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job.

use api::api_keys::hash_api_key;
use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use domain::api_key_prefix;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

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

/// Create an active product; returns the product id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {body}");
    body["id"].as_str().expect("product id").to_string()
}

/// Mint an API key with a JWT; returns the creation body
/// (which carries the full key, once).
async fn create_key(
    app: &Router,
    jwt: &str,
    name: &str,
    scopes: Value,
    expires_in_days: Option<i64>,
) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/api-keys",
        Some(json!({
            "name": name,
            "scopes": scopes,
            "expires_in_days": expires_in_days
        })),
        Some(jwt),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create api key {name}: {body}");
    body
}

#[tokio::test]
async fn wave37_api_keys_auth_scopes_exclusions_and_rate_limit() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 37 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE api_keys, payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, \
         order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "wave37-test-secret".to_string(), true);
    let app = build_app(state);

    // A seller with something to buy, and the buyer whose
    // "mobile app" the keys will act for.
    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave37-store").await;
    let product = create_product(&app, &seller, &store, "Wave37 Product", 50_000, 5).await;

    let buyer = login(&app, "01999999999").await;
    let (status, me) = call(&app, Method::GET, "/v1/auth/me", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "jwt me: {me}");
    let buyer_id = me["id"].as_str().expect("buyer id").to_string();

    // 1. Invalid creations are 400s before anything is stored:
    // an empty name, an invented scope, and out-of-range
    // expiries (0 days and 366 days).
    for bad in [
        json!({ "name": "  ", "scopes": ["read"] }),
        json!({ "name": "Ok name", "scopes": ["admin"] }),
        json!({ "name": "Ok name", "scopes": [] }),
        json!({ "name": "Ok name", "scopes": ["read"], "expires_in_days": 0 }),
        json!({ "name": "Ok name", "scopes": ["read"], "expires_in_days": 366 }),
    ] {
        let (status, body) = call(&app, Method::POST, "/v1/api-keys", Some(bad), Some(&buyer)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "invalid key create: {body}");
    }

    // 2. A read-only key: 201, the full key shown once with
    // the marker and full length, its prefix matching the
    // domain rule, no expiry when none was requested.
    let created = create_key(&app, &buyer, "Mobile app", json!(["read"]), None).await;
    let read_key = created["key"].as_str().expect("full key shown once").to_string();
    let read_key_id = created["id"].as_str().expect("key id").to_string();
    assert!(read_key.starts_with("bdm_live_"), "key marker: {read_key}");
    assert_eq!(read_key.chars().count(), 49, "key length: {read_key}");
    assert_eq!(created["key_prefix"], api_key_prefix(&read_key).as_str());
    assert_eq!(created["scopes"], json!(["read"]));
    assert_eq!(created["status"], "active");
    assert!(created["expires_at"].is_null(), "no expiry: {created}");
    assert!(created["last_used_at"].is_null(), "unused yet: {created}");

    // 3. The read key authenticates as the SAME user on an
    // ordinary route.
    let (status, key_me) = call(&app, Method::GET, "/v1/auth/me", None, Some(&read_key)).await;
    assert_eq!(status, StatusCode::OK, "key me: {key_me}");
    assert_eq!(key_me["id"], buyer_id.as_str());

    // 4. …but a read-only key cannot write: the scope rule
    // refuses the cart add before any handler runs.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product, "quantity": 1 })),
        Some(&read_key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "read-only write: {body}");

    // 5. A read+write key CAN write: the same cart add lands.
    let created = create_key(&app, &buyer, "Seller sync", json!(["read", "write"]), Some(90)).await;
    let write_key = created["key"].as_str().expect("full key").to_string();
    assert_eq!(created["scopes"], json!(["read", "write"]));
    assert!(created["expires_at"].is_string(), "expiry set: {created}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product, "quantity": 1 })),
        Some(&write_key),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "write key cart add: {body}");

    // 6. The JWT-only exclusions refuse even the write key:
    // admin, the payout account, subscription spend, key
    // management itself, and the data export.
    for (method, uri, body) in [
        (Method::GET, "/v1/admin/users".to_string(), None),
        (Method::GET, "/v1/seller/payout-account".to_string(), None),
        (
            Method::POST,
            "/v1/seller/subscription".to_string(),
            Some(json!({ "plan": "pro" })),
        ),
        (
            Method::POST,
            "/v1/api-keys".to_string(),
            Some(json!({ "name": "Nested", "scopes": ["read"] })),
        ),
        (Method::GET, "/v1/buyer/export".to_string(), None),
    ] {
        let (status, resp) = call(&app, method, &uri, body, Some(&write_key)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "key on {uri}: {resp}");
    }

    // 7. A key-shaped token that was never issued is a 401.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/auth/me",
        None,
        Some("bdm_live_neverissued00000000000000000000000000"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "unknown key: {body}");

    // 8. The list shows metadata + prefixes only: neither
    // full key nor the stored hash appears anywhere in the
    // body, and the read key's last_used_at was stamped by
    // its successful calls above.
    let stored_hash: String =
        sqlx::query_scalar("SELECT key_hash FROM api_keys WHERE id = $1")
            .bind(Uuid::parse_str(&read_key_id).expect("key id parses"))
            .fetch_one(&pool)
            .await
            .expect("stored hash reads");
    assert_eq!(stored_hash, hash_api_key(&read_key), "only the hash is stored");
    let (status, list) = call(&app, Method::GET, "/v1/api-keys", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "key list: {list}");
    let list_text = list.to_string();
    assert!(!list_text.contains(&read_key), "list leaks the read key");
    assert!(!list_text.contains(&write_key), "list leaks the write key");
    assert!(!list_text.contains(&stored_hash), "list leaks the hash");
    let items = list["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2, "two keys listed: {list}");
    let read_entry = items
        .iter()
        .find(|item| item["id"] == read_key_id.as_str())
        .expect("read key listed");
    assert_eq!(read_entry["key_prefix"], api_key_prefix(&read_key).as_str());
    assert!(
        read_entry["last_used_at"].is_string(),
        "last use stamped: {read_entry}"
    );

    // 9. A stranger cannot revoke the key (404 — the key is
    // invisible to them); the owner can, twice (idempotent).
    let stranger = login(&app, "01599999999").await;
    let uri = format!("/v1/api-keys/{read_key_id}");
    let (status, body) = call(&app, Method::DELETE, &uri, None, Some(&stranger)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger revoke: {body}");
    let (status, revoked) = call(&app, Method::DELETE, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "revoke: {revoked}");
    assert_eq!(revoked["status"], "revoked");
    let (status, revoked) = call(&app, Method::DELETE, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "re-revoke: {revoked}");
    assert_eq!(revoked["status"], "revoked");

    // 10. The revoked key is dead: 401, like a bad JWT.
    let (status, body) = call(&app, Method::GET, "/v1/auth/me", None, Some(&read_key)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "revoked key: {body}");

    // 11. An expired key is equally dead. The row is inserted
    // directly with a past expiry, hashed with the same
    // function the API stores (the full key itself is never
    // in the database).
    let expired_key = "bdm_live_expiredkey0000000000000000000000000";
    sqlx::query(
        "INSERT INTO api_keys (user_id, name, key_prefix, key_hash, scopes, expires_at) \
         VALUES ($1, 'Old integration', $2, $3, 'read', now() - interval '1 day')",
    )
    .bind(Uuid::parse_str(&buyer_id).expect("buyer id parses"))
    .bind(api_key_prefix(expired_key))
    .bind(hash_api_key(expired_key))
    .execute(&pool)
    .await
    .expect("insert expired key");
    let (status, body) = call(&app, Method::GET, "/v1/auth/me", None, Some(expired_key)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "expired key: {body}");

    // 12. Rate limiting: a second state over the SAME
    // database with a 3-requests-per-minute key limit. A
    // fresh key gets three admissions and a 429 on the
    // fourth — while JWT traffic on the same state sails
    // past the same count, because the limiter only ever
    // sees key-authenticated requests.
    let limited_state =
        AppState::new(Some(pool.clone()), "wave37-test-secret".to_string(), true)
            .with_api_key_rate_limit_per_min(3);
    let limited_app = build_app(limited_state);
    let created = create_key(&app, &buyer, "Rate probe", json!(["read"]), None).await;
    let rate_key = created["key"].as_str().expect("full key").to_string();
    for attempt in 1..=3 {
        let (status, body) =
            call(&limited_app, Method::GET, "/v1/auth/me", None, Some(&rate_key)).await;
        assert_eq!(status, StatusCode::OK, "attempt {attempt}: {body}");
    }
    let (status, body) =
        call(&limited_app, Method::GET, "/v1/auth/me", None, Some(&rate_key)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "fourth call: {body}");
    for attempt in 1..=4 {
        let (status, body) =
            call(&limited_app, Method::GET, "/v1/auth/me", None, Some(&buyer)).await;
        assert_eq!(status, StatusCode::OK, "jwt attempt {attempt}: {body}");
    }
}
