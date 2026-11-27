//! Wave 52 integration test: merchandising — home banners,
//! curated collections, flash sales, the home feed, sold
//! counts and the merchandising event counters.
//!
//! The money-critical assertions: a live flash price is the
//! price the CART shows, the price CHECKOUT snapshots (goods
//! subtotal 150_000 on a 200_000 product in a 150_000 sale),
//! and therefore the price the seller's payout snapshot is
//! computed from (150_000 − 8% commission = 138_000 earned on
//! delivery — the Wave 9 earning shape at the sale price).
//! The cap assertions pin the documented cap rule: units in
//! non-cancelled orders placed inside the window at the sale
//! price count, and once the cap is spent the price reverts
//! to base everywhere. Sold counts move only on delivery.
//!
//! The small helpers are copied from the Wave 9 test on
//! purpose (integration test files stay self-contained), plus
//! Wave 51's multipart helper for the banner image upload.
//!
//! Gated on `DATABASE_URL`: without it the test prints a skip
//! notice and passes, so a plain local `cargo test` (no
//! database) still passes. CI provides a Postgres 16 service
//! and sets `DATABASE_URL` for the rust job.

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

/// A real 4x4 PNG (solid red), copied from the Wave 51 test:
/// smaller than every derived width, so its display URLs fall
/// back to the original (the pipeline never upscales).
const PNG_4X4: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82,
    0, 0, 0, 4, 0, 0, 0, 4, 8, 2, 0, 0, 0, 38, 147, 9,
    41, 0, 0, 0, 16, 73, 68, 65, 84, 120, 218, 99, 56, 33, 39, 7,
    71, 12, 196, 113, 0, 177, 99, 16, 65, 211, 74, 204, 13, 0, 0, 0,
    0, 73, 69, 78, 68, 174, 66, 96, 130,
];

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

/// Add a product to the buyer's cart.
async fn add_to_cart(app: &Router, token: &str, product_id: &str, quantity: i64) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({ "product_id": product_id, "quantity": quantity })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {body}");
}

/// Checkout the buyer's current cart with COD; returns the order.
async fn checkout_cod(app: &Router, token: &str) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(json!({
            "delivery_name": "Test Buyer",
            "delivery_phone": "01999999999",
            "delivery_address": "House 9, Road 3, Khulna",
            "delivery_zone": "inside_dhaka",
            "payment_method": "cod"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {body}");
    body
}

/// Move one sub-order to a new status as its seller.
async fn set_sub_order_status(
    app: &Router,
    token: &str,
    sub_order_id: &str,
    status: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/status");
    call(
        app,
        Method::PATCH,
        &uri,
        Some(json!({ "status": status })),
        Some(token),
    )
    .await
}

/// GET the seller balance for a token.
async fn get_balance(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/seller/balance", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "balance: {body}");
    body
}

/// One part of a hand-built multipart body (Wave 51's helper).
struct Part<'a> {
    name: &'a str,
    filename: Option<&'a str>,
    content_type: Option<&'a str>,
    data: &'a [u8],
}

/// Send one multipart/form-data request through the router
/// and return (status, JSON body). Added because the shared
/// `call` helper speaks JSON only; the body is assembled with
/// a fixed boundary over the same oneshot mechanism.
async fn call_multipart(
    app: &Router,
    uri: &str,
    token: Option<&str>,
    parts: &[Part<'_>],
) -> (StatusCode, Value) {
    call_multipart_as(app, Method::POST, uri, token, parts).await
}

/// The method-taking form of [`call_multipart`].
async fn call_multipart_as(
    app: &Router,
    method: Method,
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

    let mut builder = Request::builder().method(method).uri(uri);
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

/// Format unix seconds as an RFC 3339 UTC timestamp (the
/// civil-from-days conversion), so the test can schedule
/// banner and flash windows relative to the database clock
/// without a date library.
fn rfc3339_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (hour, minute, second) = (
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60,
    );
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

/// The current unix time, read from the database itself (the
/// clock every stored window is compared against).
async fn db_now_unix(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM now())::bigint")
        .fetch_one(pool)
        .await
        .expect("read database clock")
}

/// Drive one sub-order through the PATCH status walk to
/// delivered (confirmed → shipped → delivered).
async fn deliver_sub_order(app: &Router, seller: &str, sub_order_id: &str) {
    for status in ["confirmed", "shipped", "delivered"] {
        let (code, body) = set_sub_order_status(app, seller, sub_order_id, status).await;
        assert_eq!(code, StatusCode::OK, "walk to {status}: {body}");
    }
}

#[tokio::test]
async fn wave52_merchandising_flash_sales_and_home_feed() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 52 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same
    // database (the Wave 51 truncate canon, plus this wave's
    // tables).
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

    // The media store for this run: a local disk store in a
    // scratch directory, so the banner upload writes real
    // objects the serve route could read back.
    let media_root = std::env::temp_dir().join(format!(
        "bdm-wave52-media-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave52-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers)
        .with_media_store(Arc::new(LocalDiskStore::new(media_root)));
    let app = build_app(state);

    // The cast: a platform admin (promoted by SQL, the only
    // way in), one seller with a store, one plain buyer.
    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave52-store").await;
    let buyer = login(&app, "01999999999").await;

    // Three products in one category. A: the flash-sale hero
    // (200_000 base). B: the cap test (100_000 base). C: the
    // ended-sale control (50_000 base).
    let product_a = create_product(&app, &seller, &store, "Wave52 Jacket", 200_000, 10).await;
    let product_b = create_product(&app, &seller, &store, "Wave52 Shoes", 100_000, 10).await;
    let product_c = create_product(&app, &seller, &store, "Wave52 Cap", 50_000, 10).await;
    for product in [&product_a, &product_b, &product_c] {
        let (status, body) = call(
            &app,
            Method::PATCH,
            &format!("/v1/products/{product}"),
            Some(json!({ "category": "Fashion" })),
            Some(&seller),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "categorise product: {body}");
    }

    let t0 = db_now_unix(&pool).await;

    // ------------------------------------------------------------------
    // 1. Merchandising writes are admin-only.
    // ------------------------------------------------------------------
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/admin/banners",
        Some(json!({ "title": "Seller Banner" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller banner create: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/admin/collections",
        Some(json!({ "slug": "seller-picks", "title": "Seller Picks" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller collection create: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Seller Sale",
            "starts_at": rfc3339_from_unix(t0 - 60),
            "ends_at": rfc3339_from_unix(t0 + 3_600)
        })),
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "buyer flash create: {body}");

    // ------------------------------------------------------------------
    // 2. Banners: validation, CRUD, window visibility, image
    //    upload through the media pipeline.
    // ------------------------------------------------------------------
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/admin/banners",
        Some(json!({ "title": "Bad Link", "link_url": "javascript:alert(1)" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "javascript link refused: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/admin/banners",
        Some(json!({
            "title": "Inverted Window",
            "starts_at": rfc3339_from_unix(t0 + 3_600),
            "ends_at": rfc3339_from_unix(t0)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "inverted window refused: {body}");

    let (status, banner1) = call(
        &app,
        Method::POST,
        "/v1/admin/banners",
        Some(json!({
            "title": "Eid Sale",
            "subtitle": "Up to 25% off fashion",
            "link_url": "/collections/wave52-picks",
            "external_image_url": "https://cdn.example.com/eid.png"
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create banner: {banner1}");
    let banner1_id = banner1["id"].as_str().expect("banner id").to_string();
    assert_eq!(banner1["image_url"], "https://cdn.example.com/eid.png");

    // A banner scheduled for tomorrow is stored but invisible.
    let (status, banner2) = call(
        &app,
        Method::POST,
        "/v1/admin/banners",
        Some(json!({
            "title": "Tomorrow Sale",
            "starts_at": rfc3339_from_unix(t0 + 86_400),
            "ends_at": rfc3339_from_unix(t0 + 172_800)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create future banner: {banner2}");
    let banner2_id = banner2["id"].as_str().expect("banner id").to_string();

    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "home feed: {feed}");
    let feed_banner_ids: Vec<&str> = feed["banners"]
        .as_array()
        .expect("banners array")
        .iter()
        .map(|b| b["id"].as_str().expect("banner id"))
        .collect();
    assert_eq!(feed_banner_ids, vec![banner1_id.as_str()], "feed banners: {feed}");

    // Upload replaces the hotlink as the image source.
    let (status, uploaded) = call_multipart(
        &app,
        &format!("/v1/admin/banners/{banner1_id}/image"),
        Some(&admin),
        &[Part {
            name: "file",
            filename: Some("eid.png"),
            content_type: Some("image/png"),
            data: PNG_4X4,
        }],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "banner image upload: {uploaded}");
    let uploaded_url = uploaded["image_url"].as_str().expect("image url").to_string();
    assert!(
        uploaded_url.starts_with("/media/site/banners/"),
        "uploaded banner image is served from the media store: {uploaded}"
    );

    // Deleting the upload leaves a text-only banner.
    let (status, cleared) = call(
        &app,
        Method::DELETE,
        &format!("/v1/admin/banners/{banner1_id}/image"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "banner image delete: {cleared}");
    assert!(cleared["image_url"].is_null(), "image cleared: {cleared}");

    // PATCH renames; the admin list shows both banners.
    let (status, renamed) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/banners/{banner1_id}"),
        Some(json!({ "title": "Eid Sale — Extended" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "banner patch: {renamed}");
    assert_eq!(renamed["title"], "Eid Sale — Extended");
    let (status, list) = call(&app, Method::GET, "/v1/admin/banners", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "banner list: {list}");
    assert_eq!(list["total"], 2, "banner list: {list}");

    // ------------------------------------------------------------------
    // 3. Collections: slug rules, membership order, public page.
    // ------------------------------------------------------------------
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/admin/collections",
        Some(json!({ "slug": "Bad Slug", "title": "Nope" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad slug refused: {body}");

    let (status, collection) = call(
        &app,
        Method::POST,
        "/v1/admin/collections",
        Some(json!({ "slug": "wave52-picks", "title": "Wave52 Picks" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create collection: {collection}");
    let collection_id = collection["id"].as_str().expect("collection id").to_string();

    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/admin/collections",
        Some(json!({ "slug": "wave52-picks", "title": "Duplicate" })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "duplicate slug: {body}");

    // Membership order is the admin's order: B before A.
    let (status, detail) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/collections/{collection_id}/products"),
        Some(json!({ "product_ids": [product_b, product_a] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set membership: {detail}");
    let member_ids: Vec<&str> = detail["products"]
        .as_array()
        .expect("products array")
        .iter()
        .map(|p| p["id"].as_str().expect("product id"))
        .collect();
    assert_eq!(member_ids, vec![product_b.as_str(), product_a.as_str()]);

    // A duplicated product in the list is a 400, not a
    // silently halved membership.
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/collections/{collection_id}/products"),
        Some(json!({ "product_ids": [product_a, product_a] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "duplicate member refused: {body}");

    // The public landing data shows the same order.
    let (status, public) = call(
        &app,
        Method::GET,
        "/v1/collections/wave52-picks",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public collection: {public}");
    let public_ids: Vec<&str> = public["products"]
        .as_array()
        .expect("products array")
        .iter()
        .map(|p| p["id"].as_str().expect("product id"))
        .collect();
    assert_eq!(public_ids, vec![product_b.as_str(), product_a.as_str()]);

    // ------------------------------------------------------------------
    // 4. Flash sale 1: the price the buyer actually pays.
    // ------------------------------------------------------------------
    // A "sale" priced at the base is refused at write time.
    let (status, sale1) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Flash One",
            "starts_at": rfc3339_from_unix(t0 - 3_600),
            "ends_at": rfc3339_from_unix(t0 + 7_200)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create flash sale: {sale1}");
    let sale1_id = sale1["id"].as_str().expect("sale id").to_string();

    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{sale1_id}/items"),
        Some(json!({ "items": [{ "product_id": product_a, "sale_price_poisha": 200_000 }] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "non-reduction refused: {body}");

    let (status, detail) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{sale1_id}/items"),
        Some(json!({ "items": [{ "product_id": product_a, "sale_price_poisha": 150_000 }] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set flash items: {detail}");
    assert_eq!(detail["items"][0]["base_price_poisha"], 200_000);
    assert_eq!(detail["items"][0]["sale_price_poisha"], 150_000);
    assert_eq!(detail["items"][0]["sold_at_sale"], 0);

    // A second ACTIVE sale overlapping the window is refused:
    // at most one flash sale is ever live.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Overlapping Sale",
            "starts_at": rfc3339_from_unix(t0),
            "ends_at": rfc3339_from_unix(t0 + 3_600)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "overlap refused: {body}");

    // The product view carries the live flash price; nothing
    // has sold yet.
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_a}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "product detail: {detail}");
    assert_eq!(detail["flash_price_poisha"], 150_000, "detail: {detail}");
    assert!(detail["flash_ends_at"].is_string(), "detail: {detail}");
    assert_eq!(detail["sold_count"], 0, "detail: {detail}");

    // The cart shows the sale price.
    add_to_cart(&app, &buyer, &product_a, 1).await;
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cart: {cart}");
    assert_eq!(cart["items"][0]["unit_price_poisha"], 150_000, "cart: {cart}");
    assert_eq!(cart["goods_total_poisha"], 150_000, "cart: {cart}");

    // Checkout snapshots the SALE price: goods 150_000, and
    // the payout snapshot is the sale goods minus the 8%
    // commission on the sale price — 138_000, the Wave 9
    // earning shape at the price actually paid.
    let order = checkout_cod(&app, &buyer).await;
    let sub_order = &order["sub_orders"][0];
    assert_eq!(sub_order["goods_subtotal_poisha"], 150_000, "order: {order}");
    assert_eq!(sub_order["seller_payout_poisha"], 138_000, "order: {order}");
    let sub_order_id = sub_order["id"].as_str().expect("sub-order id").to_string();

    // Sold counts move only on DELIVERY: still 0 now, exactly
    // 1 after the walk, and the balance earns the snapshot.
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_a}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["sold_count"], 0, "placed is not sold: {detail}");
    deliver_sub_order(&app, &seller, &sub_order_id).await;
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_a}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["sold_count"], 1, "delivered is sold: {detail}");
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 138_000, "balance: {balance}");

    // The home feed's flash section and best-sellers rail
    // agree with the detail view.
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "home feed: {feed}");
    let flash = &feed["flash_sale"];
    assert_eq!(flash["id"], sale1_id, "feed flash: {feed}");
    assert_eq!(flash["items"][0]["id"], product_a, "feed flash: {feed}");
    assert_eq!(flash["items"][0]["flash_price_poisha"], 150_000);
    assert_eq!(flash["items"][0]["sale_price_poisha"], 150_000);
    assert_eq!(flash["items"][0]["sold_at_sale"], 1);
    let best_ids: Vec<&str> = feed["best_sellers"]
        .as_array()
        .expect("best sellers")
        .iter()
        .map(|p| p["id"].as_str().expect("product id"))
        .collect();
    assert_eq!(best_ids, vec![product_a.as_str()], "best sellers: {feed}");
    let fashion = feed["categories"]
        .as_array()
        .expect("categories")
        .iter()
        .find(|c| c["category"] == "Fashion")
        .expect("Fashion tile");
    assert_eq!(fashion["product_count"], 3, "categories: {feed}");
    let feed_collection_ids: Vec<&str> = feed["collections"][0]["products"]
        .as_array()
        .expect("collection products")
        .iter()
        .map(|p| p["id"].as_str().expect("product id"))
        .collect();
    assert_eq!(feed_collection_ids, vec![product_b.as_str(), product_a.as_str()]);

    // ------------------------------------------------------------------
    // 5. Cap exhaustion: one unit at the sale price, then the
    //    price reverts to base everywhere.
    // ------------------------------------------------------------------
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/admin/flash-sales/{sale1_id}"),
        Some(json!({ "is_active": false })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "deactivate sale 1: {body}");
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_a}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail["flash_price_poisha"].is_null(), "sale off: {detail}");

    let (status, sale2) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Flash Two — Capped",
            "starts_at": rfc3339_from_unix(t0 - 60),
            "ends_at": rfc3339_from_unix(t0 + 3_600)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create capped sale: {sale2}");
    let sale2_id = sale2["id"].as_str().expect("sale id").to_string();
    let (status, detail) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{sale2_id}/items"),
        Some(json!({
            "items": [{ "product_id": product_b, "sale_price_poisha": 80_000, "stock_cap": 1 }]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set capped items: {detail}");
    assert_eq!(detail["items"][0]["stock_cap"], 1);
    assert_eq!(detail["items"][0]["cap_remaining"], 1);

    // The one capped unit sells at the sale price…
    add_to_cart(&app, &buyer, &product_b, 1).await;
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cart["items"][0]["unit_price_poisha"], 80_000, "cart: {cart}");
    let order = checkout_cod(&app, &buyer).await;
    assert_eq!(order["sub_orders"][0]["goods_subtotal_poisha"], 80_000, "order: {order}");

    // …and the cap is now spent (the placed, non-cancelled
    // order counts): the cart reverts to the base price, the
    // product view shows no flash price, and the admin item
    // view reports the spent cap.
    add_to_cart(&app, &buyer, &product_b, 1).await;
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cart["items"][0]["unit_price_poisha"], 100_000, "cart: {cart}");
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_b}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail["flash_price_poisha"].is_null(), "cap spent: {detail}");
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/flash-sales/{sale2_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "sale detail: {detail}");
    assert_eq!(detail["items"][0]["sold_at_sale"], 1);
    assert_eq!(detail["items"][0]["cap_remaining"], 0);
    let (status, _) = call(&app, Method::DELETE, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "clear cart");

    // ------------------------------------------------------------------
    // 6. An ended sale prices nothing; the admin preview sees
    //    it live at its own instant.
    // ------------------------------------------------------------------
    let (status, sale3) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Flash Three — Yesterday",
            "starts_at": rfc3339_from_unix(t0 - 7_200),
            "ends_at": rfc3339_from_unix(t0 - 3_600)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create past sale: {sale3}");
    let sale3_id = sale3["id"].as_str().expect("sale id").to_string();
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{sale3_id}/items"),
        Some(json!({ "items": [{ "product_id": product_c, "sale_price_poisha": 40_000 }] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set past-sale items");
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_c}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail["flash_price_poisha"].is_null(), "ended sale: {detail}");

    // Preview at an instant inside sale 3's window: sale 3 is
    // the live sale there. Preview is admin-only.
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/home-preview?at={}", rfc3339_from_unix(t0 - 5_400)),
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "seller preview: {body}");
    let (status, preview) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/home-preview?at={}", rfc3339_from_unix(t0 - 5_400)),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "preview: {preview}");
    assert_eq!(preview["flash_sale"]["id"], sale3_id, "preview: {preview}");
    assert_eq!(preview["flash_sale"]["items"][0]["flash_price_poisha"], 40_000);

    // ------------------------------------------------------------------
    // 7. Merchandising events + the metrics block.
    // ------------------------------------------------------------------
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/merch/events",
        Some(json!({ "kind": "banner_click", "target_id": banner1_id })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "banner click: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/merch/events",
        Some(json!({ "kind": "collection_view", "target_id": collection_id })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "collection view: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/merch/events",
        Some(json!({ "kind": "page_view", "target_id": collection_id })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown kind refused: {body}");

    let (status, metrics) = call(
        &app,
        Method::GET,
        "/v1/admin/metrics",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "metrics: {metrics}");
    assert_eq!(metrics["merchandising"]["banner_clicks_total"], 1, "metrics: {metrics}");
    assert_eq!(metrics["merchandising"]["collection_views_total"], 1);
    assert_eq!(metrics["merchandising"]["banner_clicks_today"], 1);

    // ------------------------------------------------------------------
    // 8. Featured stores: only verified tiers are spotlighted.
    // ------------------------------------------------------------------
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        feed["featured_stores"].as_array().map(Vec::len),
        Some(0),
        "unverified store not featured: {feed}"
    );
    sqlx::query("UPDATE stores SET verified_tier = 'verified' WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&store).expect("store uuid"))
        .execute(&pool)
        .await
        .expect("verify store");
    let (status, feed) = call(&app, Method::GET, "/v1/home", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(feed["featured_stores"][0]["slug"], "wave52-store", "feed: {feed}");
    assert_eq!(feed["featured_stores"][0]["product_count"], 3);

    // ------------------------------------------------------------------
    // 9. Deletes: the sale, the collection, the future banner.
    // ------------------------------------------------------------------
    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/admin/flash-sales/{sale3_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete sale: {body}");
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/v1/admin/flash-sales/{sale3_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "sale gone");
    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/admin/collections/{collection_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete collection: {body}");
    let (status, _) = call(&app, Method::GET, "/v1/collections/wave52-picks", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "collection page gone");
    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/admin/banners/{banner2_id}"),
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "delete banner: {body}");
    let (status, list) = call(&app, Method::GET, "/v1/admin/banners", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["total"], 1, "one banner remains: {list}");
}
