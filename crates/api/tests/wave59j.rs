//! Wave 59 integration test (j): post covers — the
//! hotlink field validates like every other external
//! image URL on the platform (an ftp:// URL is a 400,
//! a good https URL sticks, a blank clears), and the
//! upload path runs the real Wave 51 media pipeline
//! under the post's own key: the stored cover URL
//! points into /media/, the object serves back, and
//! deleting the cover clears the field (and, for an
//! upload, removes the objects).
//!
//! Gated on `DATABASE_URL`: without it the test skips
//! cleanly so a plain local `cargo test` (no database)
//! still passes. CI provides a Postgres 16 service and
//! sets `DATABASE_URL` for the rust job.

use std::sync::Arc;

use api::media::LocalDiskStore;
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

/// A real 4x4 PNG (solid red), byte-for-byte the
/// wave51 fixture: it decodes with the `image` crate
/// and is smaller than every derived width.
const PNG_4X4: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 4, 0, 0, 0, 4, 8, 2, 0,
    0, 0, 38, 147, 9, 41, 0, 0, 0, 16, 73, 68, 65, 84, 120, 218, 99, 56, 33, 39, 7, 71, 12, 196,
    113, 0, 177, 99, 16, 65, 211, 74, 204, 13, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// One multipart part (the wave51 fixture shape).
struct Part<'a> {
    name: &'a str,
    filename: Option<&'a str>,
    content_type: Option<&'a str>,
    data: &'a [u8],
}

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

/// Send one multipart request and return (status, JSON body).
async fn call_multipart(
    app: &Router,
    uri: &str,
    token: Option<&str>,
    parts: &[Part<'_>],
) -> (StatusCode, Value) {
    const BOUNDARY: &str = "----bdm-wave59-test-boundary";
    let mut body: Vec<u8> = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let mut disposition = format!("Content-Disposition: form-data; name=\"{}\"", part.name);
        if let Some(filename) = part.filename {
            disposition.push_str(&format!("; filename=\"{filename}\""));
        }
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(b"\r\n");
        if let Some(content_type) = part.content_type {
            body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(part.data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    let mut builder = Request::builder().method(Method::POST).uri(uri);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let request = builder
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .expect("request builds");
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

/// GET a URL and return only the status (the media
/// serve route answers with image bytes, not JSON).
async fn get_status(app: &Router, uri: &str) -> StatusCode {
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .expect("request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router responds");
    response.status()
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

/// Build the test app (dev couriers on, a local-disk
/// media store in a scratch directory), or None when
/// no database is configured. Truncates every table
/// EXCEPT `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 59 integration tests");
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
         buyer_addresses, bulk_imports, store_layouts, storefront_events, \
         store_pages, store_posts, store_post_events, \
         store_collections, store_collection_products, \
         product_images, product_variants, products, store_members, store_follows, \
         stores, otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let media_root = std::env::temp_dir().join(format!(
        "bdm-wave59-media-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave59-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers)
        .with_media_store(Arc::new(LocalDiskStore::new(media_root)));
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

#[tokio::test]
async fn wave59j_post_cover_hotlink_and_upload() {
    let Some((app, _pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01721111111").await;
    create_store(&app, &seller, "w59-cover").await;
    let (status, post) = call(
        &app,
        Method::POST,
        "/v1/seller/store/posts",
        Some(json!({
            "slug": "covered-post",
            "title": "কভারসহ পোস্ট",
            "body": "কভার পরীক্ষা।",
            "status": "published"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post created: {post}");
    let post_id = post["id"].as_str().expect("post id").to_string();
    assert_eq!(post["cover_image_url"], Value::Null);

    // 1. A bad-scheme hotlink is refused; a good one
    // sticks and shows on the public read.
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/posts/{post_id}"),
        Some(json!({ "cover_image_url": "ftp://example.com/cover.png" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "ftp cover refused");
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/seller/store/posts/{post_id}"),
        Some(json!({ "cover_image_url": "https://cdn.example.com/cover.png" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "hotlink set: {body}");
    assert_eq!(body["cover_image_url"], "https://cdn.example.com/cover.png");
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/stores/w59-cover/posts/covered-post",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public read: {body}");
    assert_eq!(
        body["cover_image_url"],
        "https://cdn.example.com/cover.png"
    );

    // 2. Deleting the cover of a HOTLINK clears the
    // field without touching any stored object.
    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/seller/store/posts/{post_id}/cover-image"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "hotlink cover cleared: {body}");
    assert_eq!(body["cover_image_url"], Value::Null);

    // 3. The upload runs the pipeline: the returned
    // cover URL points under the post's own media key
    // and the object serves back over /media/.
    let (status, body) = call_multipart(
        &app,
        &format!("/v1/seller/store/posts/{post_id}/cover-image"),
        Some(&seller),
        &[Part {
            name: "file",
            filename: Some("cover.png"),
            content_type: Some("image/png"),
            data: PNG_4X4,
        }],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cover upload: {body}");
    let cover_url = body["cover_image_url"]
        .as_str()
        .expect("cover url")
        .to_string();
    assert!(
        cover_url.starts_with("/media/stores/") && cover_url.contains("/posts/"),
        "cover lives under the post key: {cover_url}"
    );
    let status = get_status(&app, &cover_url).await;
    assert_eq!(status, StatusCode::OK, "the stored cover serves");

    // 4. A non-image upload is refused by the pipeline,
    // and deleting the uploaded cover clears the field
    // and removes the object (the URL now 404s).
    let (status, _) = call_multipart(
        &app,
        &format!("/v1/seller/store/posts/{post_id}/cover-image"),
        Some(&seller),
        &[Part {
            name: "file",
            filename: Some("notes.txt"),
            content_type: Some("text/plain"),
            data: b"not an image",
        }],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "non-image refused");
    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/seller/store/posts/{post_id}/cover-image"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "uploaded cover removed: {body}");
    assert_eq!(body["cover_image_url"], Value::Null);
    let status = get_status(&app, &cover_url).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the object is gone too");

    // 5. Deleting the post itself is the last word:
    // the seller read 404s afterwards.
    let (status, _) = call(
        &app,
        Method::DELETE,
        &format!("/v1/seller/store/posts/{post_id}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "post deleted");
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/store/posts/{post_id}"),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "deleted post is gone");
}
