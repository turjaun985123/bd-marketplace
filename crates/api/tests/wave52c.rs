//! Wave 52 (third test file): the update paths and the
//! remaining surfaces — merged-row validation on PATCH,
//! overlap refusal on flash-sale UPDATE (not just create),
//! membership validation, live-sale deletion, the wishlist
//! surface, the cart's pre-sale list price, the category
//! tile's cover image, and the admin recent-events list.
//!
//! The small helpers are copied from the Wave 52 test on
//! purpose (integration test files stay self-contained),
//! including its multipart helper for the image upload.
//!
//! Gated on `DATABASE_URL`: without it the test prints a skip
//! notice and passes. CI provides a Postgres 16 service.

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

/// A real 4x4 PNG (solid red), copied from the Wave 51 test.
const PNG_4X4: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82,
    0, 0, 0, 4, 0, 0, 0, 4, 8, 2, 0, 0, 0, 38, 147, 9,
    41, 0, 0, 0, 16, 73, 68, 65, 84, 120, 218, 99, 56, 33, 39, 7,
    71, 12, 196, 113, 0, 177, 99, 16, 65, 211, 74, 204, 13, 0, 0, 0,
    0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// One part of a multipart request (the Wave 51 helper shape).
struct Part<'a> {
    name: &'a str,
    filename: Option<&'a str>,
    content_type: Option<&'a str>,
    data: &'a [u8],
}

/// Send one multipart/form-data request through the router.
async fn call_multipart(
    app: &Router,
    uri: &str,
    token: Option<&str>,
    parts: &[Part<'_>],
) -> (StatusCode, Value) {
    const BOUNDARY: &str = "----bdm-wave52-test-boundary";
    let mut body: Vec<u8> = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let mut disposition =
            format!("Content-Disposition: form-data; name=\"{}\"", part.name);
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

/// Format a unix timestamp as RFC 3339 UTC (civil-from-days).
fn rfc3339_from_unix(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let hour = secs / 3_600;
    let minute = (secs % 3_600) / 60;
    let second = secs % 60;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// The current unix time, read from the database itself.
async fn db_now_unix(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM now())::bigint")
        .fetch_one(pool)
        .await
        .expect("read database clock")
}

#[tokio::test]
async fn wave52c_update_paths_and_remaining_surfaces() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 52c integration test");
            return;
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
         product_images, product_variants, products, store_members, stores, \
         otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let media_root = std::env::temp_dir().join(format!(
        "bdm-wave52c-media-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave52c-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers)
        .with_media_store(Arc::new(LocalDiskStore::new(media_root)));
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave52c-store").await;
    let buyer = login(&app, "01999999999").await;
    let product = create_product(&app, &seller, &store, "Wave52C Kurti", 120_000, 10).await;
    let t0 = db_now_unix(&pool).await;

    // ------------------------------------------------------------------
    // 1. Banner PATCH validates the MERGED row: moving only
    //    `ends_at` before the stored `starts_at` is a 400.
    // ------------------------------------------------------------------
    let (status, banner) = call(
        &app,
        Method::POST,
        "/v1/admin/banners",
        Some(json!({
            "title": "Windowed",
            "starts_at": rfc3339_from_unix(t0 + 1_000),
            "ends_at": rfc3339_from_unix(t0 + 2_000)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "windowed banner: {banner}");
    let banner_id = banner["id"].as_str().expect("banner id").to_string();
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/banners/{banner_id}"),
        Some(json!({ "ends_at": rfc3339_from_unix(t0 + 500) })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "merged window inverted: {body}");
    // A patch that keeps the window valid still applies.
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/banners/{banner_id}"),
        Some(json!({ "ends_at": rfc3339_from_unix(t0 + 3_000) })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "valid window patch: {body}");

    // ------------------------------------------------------------------
    // 2. Flash sale UPDATE refuses overlap too: reactivating a
    //    dormant sale into the live sale's window is a 409, as
    //    is moving the live sale's window over a scheduled
    //    active one. Back-to-back (touching) windows are fine.
    // ------------------------------------------------------------------
    let (status, live) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Live Sale",
            "starts_at": rfc3339_from_unix(t0 - 60),
            "ends_at": rfc3339_from_unix(t0 + 3_600)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "live sale: {live}");
    let live_id = live["id"].as_str().expect("sale id").to_string();
    let (status, dormant) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Dormant Sale",
            "starts_at": rfc3339_from_unix(t0 - 30),
            "ends_at": rfc3339_from_unix(t0 + 3_600),
            "is_active": false
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "dormant sale: {dormant}");
    let dormant_id = dormant["id"].as_str().expect("sale id").to_string();
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/flash-sales/{dormant_id}"),
        Some(json!({ "is_active": true })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "reactivation overlap: {body}");
    // Touching is not overlapping: a sale starting exactly at
    // the live sale's end may be created active.
    let (status, next) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Next Sale",
            "starts_at": rfc3339_from_unix(t0 + 3_600),
            "ends_at": rfc3339_from_unix(t0 + 7_200)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "touching sale: {next}");
    let next_id = next["id"].as_str().expect("sale id").to_string();
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/flash-sales/{live_id}"),
        Some(json!({ "ends_at": rfc3339_from_unix(t0 + 7_200) })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "window move overlap: {body}");

    // ------------------------------------------------------------------
    // 3. Collection membership refuses a product that does not
    //    exist (400 with the reason), and accepts a real one.
    // ------------------------------------------------------------------
    let (status, collection) = call(
        &app,
        Method::POST,
        "/v1/admin/collections",
        Some(json!({ "slug": "wave52c-picks", "title": "Wave52C Picks" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "collection: {collection}");
    let collection_id = collection["id"].as_str().expect("collection id").to_string();
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/collections/{collection_id}/products"),
        Some(json!({
            "product_ids": ["00000000-0000-0000-0000-000000000000"]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "ghost product refused: {body}");
    let (status, detail) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/collections/{collection_id}/products"),
        Some(json!({ "product_ids": [product] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "real membership: {detail}");
    assert_eq!(detail["products"][0]["id"], product);

    // ------------------------------------------------------------------
    // 4. The wishlist surface and the cart's list price carry
    //    the flash resolution; deleting the live sale reverts
    //    every surface at once.
    // ------------------------------------------------------------------
    let (status, detail) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{live_id}/items"),
        Some(json!({
            "items": [{ "product_id": product, "sale_price_poisha": 90_000 }]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set live items: {detail}");

    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/wishlist/{product}"),
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "wishlist add: {body}");
    assert_eq!(body["wishlisted"], true, "wishlist add: {body}");
    let (status, wishlist) = call(&app, Method::GET, "/v1/wishlist", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "wishlist: {wishlist}");
    assert_eq!(wishlist["items"][0]["flash_price_poisha"], 90_000, "wishlist: {wishlist}");
    assert_eq!(wishlist["items"][0]["sold_count"], 0);

    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product, "quantity": 2 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cart add: {body}");
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cart: {cart}");
    assert_eq!(cart["items"][0]["unit_price_poisha"], 90_000, "cart: {cart}");
    assert_eq!(
        cart["items"][0]["list_unit_price_poisha"], 120_000,
        "the pre-sale list price rides along: {cart}"
    );

    let (status, _) = call(
        &app,
        Method::DELETE,
        &format!("/v1/admin/flash-sales/{live_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete live sale");
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        cart["items"][0]["unit_price_poisha"], 120_000,
        "deleting the sale reverts the cart: {cart}"
    );
    assert_eq!(cart["items"][0]["list_unit_price_poisha"], 120_000);
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail["flash_price_poisha"].is_null(), "detail reverted: {detail}");
    // The scheduled "Next" sale is untouched by the deletion.
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/flash-sales/{next_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "next sale survives: {detail}");

    // ------------------------------------------------------------------
    // 5. The category tile's cover comes from a product image:
    //    upload one, categorise the product, and the tile for
    //    that category serves the media URL.
    // ------------------------------------------------------------------
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/products/{product}"),
        Some(json!({ "category": "Kurtis" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "categorise: {body}");
    let (status, uploaded) = call_multipart(
        &app,
        &format!("/v1/seller/products/{product}/images"),
        Some(&seller),
        &[Part {
            name: "file",
            filename: Some("kurti.png"),
            content_type: Some("image/png"),
            data: PNG_4X4,
        }],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "product image upload: {uploaded}");
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "feed: {feed}");
    let tile = feed["categories"]
        .as_array()
        .expect("category tiles")
        .iter()
        .find(|c| c["category"] == "Kurtis")
        .expect("Kurtis tile")
        .clone();
    assert_eq!(tile["product_count"], 1, "tile: {tile}");
    let tile_image = tile["image_url"].as_str().expect("tile image url");
    assert!(
        tile_image.contains("/media/"),
        "tile cover is the uploaded image: {tile}"
    );

    // ------------------------------------------------------------------
    // 6. The cap rule, pinned exactly: the cap is measured on
    //    units ALREADY sold when an order is placed — so one
    //    order for 2 units against a cap of 1 prices BOTH units
    //    at the sale price (the price resolved when the order
    //    was placed), the count lands at 2 with the remaining
    //    clamped to 0, and only the NEXT buyer sees base price.
    // ------------------------------------------------------------------
    // The section-4 line is still in the buyer's cart; this
    // section prices its own cart, so clear first.
    let (status, _) = call(&app, Method::DELETE, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "clear cart before cap section");
    let capped = create_product(&app, &seller, &store, "Wave52C Scarf", 60_000, 10).await;
    let (status, cap_sale) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Cap Rule Sale",
            "starts_at": rfc3339_from_unix(t0 - 30),
            "ends_at": rfc3339_from_unix(t0 + 1_800)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cap sale: {cap_sale}");
    let cap_sale_id = cap_sale["id"].as_str().expect("sale id").to_string();
    let (status, detail) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{cap_sale_id}/items"),
        Some(json!({
            "items": [{ "product_id": capped, "sale_price_poisha": 45_000, "stock_cap": 1 }]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cap items: {detail}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": capped, "quantity": 2 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cap cart add: {body}");
    let (status, order) = call(
        &app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cap checkout: {order}");
    assert_eq!(
        order["sub_orders"][0]["goods_subtotal_poisha"], 90_000,
        "both units at the price resolved at placement: {order}"
    );
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/flash-sales/{cap_sale_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "cap detail: {detail}");
    assert_eq!(detail["items"][0]["sold_at_sale"], 2, "overshoot counted: {detail}");
    assert_eq!(detail["items"][0]["cap_remaining"], 0, "remaining clamps at 0: {detail}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": capped, "quantity": 1 })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "post-cap cart add: {body}");
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        cart["items"][0]["unit_price_poisha"], 60_000,
        "the next buyer pays base: {cart}"
    );
    let (status, _) = call(&app, Method::DELETE, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "clear cart");

    // ------------------------------------------------------------------
    // 7. The admin recent-events list resolves target titles
    //    and is admin-only.
    // ------------------------------------------------------------------
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/merch/events",
        Some(json!({ "kind": "collection_view", "target_id": collection_id })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "event: {body}");
    let (status, events) = call(
        &app,
        Method::GET,
        "/v1/admin/merch-events?limit=5",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "events list: {events}");
    let first = &events[0];
    assert_eq!(first["kind"], "collection_view", "events: {events}");
    assert_eq!(first["target_title"], "Wave52C Picks", "events: {events}");
    let (status, _) = call(
        &app,
        Method::GET,
        "/v1/admin/merch-events",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller refused the events list");
}
