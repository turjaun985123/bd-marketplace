//! Wave 2 integration test: migrations + OTP auth + store + product +
//! marketplace listing, driven through the real Axum router with tower
//! oneshot (no TCP port, no network).
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly so a plain
//! local `cargo test` (no database) still passes. CI provides a
//! Postgres 16 service and sets `DATABASE_URL` for the rust job.

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

#[tokio::test]
async fn wave2_auth_store_product_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 2 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query("TRUNCATE products, stores, otp_codes, users CASCADE")
        .execute(&pool)
        .await
        .expect("truncate tables");

    // Dev mode on: OTPs are log-sent and echoed in the response.
    let state = AppState::new(Some(pool), "wave2-test-secret".to_string(), true);
    let app = build_app(state);

    // 1. Request an OTP (dev mode returns the code for testing).
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/auth/request-otp",
        Some(json!({ "phone": "01712345678" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "request-otp: {body}");
    assert_eq!(body["phone"], "+8801712345678");
    let dev_otp = body["dev_otp"]
        .as_str()
        .expect("dev mode returns dev_otp")
        .to_string();

    // 2. A wrong code is rejected (pick one that is certainly wrong).
    let wrong_code = if dev_otp == "000000" { "111111" } else { "000000" };
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/auth/verify-otp",
        Some(json!({ "phone": "01712345678", "code": wrong_code })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 3. Verify with the real code -> JWT + a fresh buyer user.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/auth/verify-otp",
        Some(json!({ "phone": "01712345678", "code": dev_otp })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "verify-otp: {body}");
    let token = body["token"].as_str().expect("token issued").to_string();
    assert_eq!(body["user"]["role"], "buyer");

    // 4. Protected endpoint without a token -> 401.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": "Rahim Store", "slug": "rahim-store" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 5. Create a store (upgrades the user to seller).
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/stores",
        Some(json!({
            "name": "Rahim Store",
            "slug": "rahim-store",
            "description": "Wave 2 test store"
        })),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create store: {body}");
    let store_id = body["id"].as_str().expect("store id").to_string();
    assert_eq!(body["slug"], "rahim-store");

    // 6. Duplicate slug -> 409.
    let (status, _) = call(
        &app,
        Method::POST,
        "/v1/stores",
        Some(json!({ "name": "Copycat", "slug": "rahim-store" })),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // 7. Public store profile.
    let (status, body) = call(&app, Method::GET, "/v1/stores/rahim-store", None, None).await;
    assert_eq!(status, StatusCode::OK, "get store: {body}");
    assert_eq!(body["name"], "Rahim Store");

    // 8. Create an active product in the store.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": "Cotton Panjabi",
            "title_bn": "সুতি পাঞ্জাবি",
            "price_poisha": 150000,
            "stock": 10,
            "status": "active"
        })),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product: {body}");
    assert_eq!(body["store_slug"], "rahim-store");
    assert_eq!(body["price_poisha"], 150000);
    let product_id = body["id"].as_str().expect("product id").to_string();

    // 9. Marketplace listing shows it, search finds it, storefront too.
    let (status, body) = call(&app, Method::GET, "/v1/products", None, None).await;
    assert_eq!(status, StatusCode::OK, "list products: {body}");
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["id"], product_id);

    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/products?q=Panjabi",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);

    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/stores/rahim-store/products",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "storefront listing: {body}");
    assert_eq!(body["total"], 1);

    // 10. Public product detail.
    let uri = format!("/v1/products/{product_id}");
    let (status, body) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "get product: {body}");
    assert_eq!(body["title"], "Cotton Panjabi");
}
