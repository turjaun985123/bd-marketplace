//! Wave 53 (part F) integration test: the remaining
//! agreement pins — flash windows that have not started,
//! suggestion title dedup across stores, the categories
//! cover image, store-scoped facets, and the rating /
//! sold sorts against unrated and unsold products.
//!
//! The future-sale case matters because the SQL twin and
//! the Rust resolution must agree about TIME as well as
//! price: a sale whose window opens in an hour resolves
//! NOWHERE — not in the view, not in a price band, not
//! in the on-sale filter.
//!
//! Helpers follow the Wave 9 test (self-contained file);
//! gated on `DATABASE_URL` like every integration test.

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

/// PATCH a product's category as its seller.
async fn patch_category(app: &Router, token: &str, product_id: &str, category: &str) {
    let (status, body) = call(
        app,
        Method::PATCH,
        &format!("/v1/products/{product_id}"),
        Some(json!({ "category": category })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch category: {body}");
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

/// Buy `quantity` units and drive the order to delivered
/// through the DEV courier (the Wave 9 flow).
async fn buy_and_deliver_qty(
    app: &Router,
    buyer: &str,
    seller: &str,
    product_id: &str,
    quantity: i64,
) {
    add_to_cart(app, buyer, product_id, quantity).await;
    let order = checkout_cod(app, buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    let (status, body) = set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(json!({})), Some(seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    let shipment_id = shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(app, Method::POST, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
}

/// Review a delivered product as a buyer (Wave 8 shape).
async fn review(app: &Router, token: &str, product_id: &str, rating: i64) {
    let (status, body) = call(
        app,
        Method::POST,
        &format!("/v1/products/{product_id}/reviews"),
        Some(json!({ "rating": rating })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "review: {body}");
}

/// Run one browse; returns the results envelope.
async fn browse(app: &Router, query: &str) -> Value {
    let (status, body) = call(app, Method::GET, &format!("/v1/search?{query}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "browse {query}: {body}");
    body
}

/// One part of a hand-built multipart body (Wave 51's helper).
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
    const BOUNDARY: &str = "----bdm-wave53f-test-boundary";
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

/// A real 4x4 PNG (solid red) — the Wave 51 fixture bytes.
const PNG_4X4: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 4, 0, 0, 0, 4, 8, 2, 0,
    0, 0, 38, 147, 9, 41, 0, 0, 0, 16, 73, 68, 65, 84, 120, 218, 99, 56, 33, 39, 7, 71, 12, 196,
    113, 0, 177, 99, 16, 65, 211, 74, 204, 13, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// Format unix seconds as an RFC 3339 UTC timestamp (the
/// civil-from-days conversion — the Wave 52 helper).
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

/// The current unix time, read from the database itself.
async fn db_now_unix(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM now())::bigint")
        .fetch_one(pool)
        .await
        .expect("read database clock")
}

#[tokio::test]
async fn wave53f_future_sales_dedup_and_scoped_facets() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 53f integration test");
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

    let media_root = std::env::temp_dir().join(format!(
        "bdm-wave53f-media-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave53f-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers)
        .with_media_store(Arc::new(LocalDiskStore::new(media_root)));
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01753536001").await;
    let shop = create_store(&app, &seller, "wave53f-shop").await;
    let seller2 = login(&app, "01753536002").await;
    let mart = create_store(&app, &seller2, "wave53f-mart").await;
    let buyer = login(&app, "01953536001").await;

    // The same title in two stores (the dedup case), one
    // photographed; a lamp in a FUTURE sale; two radios,
    // one sold + rated, one untouched.
    let duo_shop = create_product(&app, &seller, &shop, "Wave53F Duo Mug", 30_000, 5).await;
    patch_category(&app, &seller, &duo_shop, "kitchen").await;
    let (status, uploaded) = call_multipart(
        &app,
        &format!("/v1/seller/products/{duo_shop}/images"),
        Some(&seller),
        &[Part {
            name: "file",
            filename: Some("mug.png"),
            content_type: Some("image/png"),
            data: PNG_4X4,
        }],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "upload mug photo: {uploaded}");
    let duo_mart = create_product(&app, &seller2, &mart, "Wave53F Duo Mug", 35_000, 5).await;
    patch_category(&app, &seller2, &duo_mart, "kitchen").await;
    let lamp = create_product(&app, &seller, &shop, "Wave53F Future Lamp", 90_000, 5).await;
    patch_category(&app, &seller, &lamp, "lighting").await;
    let old_radio = create_product(&app, &seller, &shop, "Wave53F Old Radio", 70_000, 5).await;
    patch_category(&app, &seller, &old_radio, "electronics").await;
    let new_radio = create_product(&app, &seller, &shop, "Wave53F New Radio", 75_000, 5).await;
    patch_category(&app, &seller, &new_radio, "electronics").await;

    buy_and_deliver_qty(&app, &buyer, &seller, &old_radio, 2).await;
    review(&app, &buyer, &old_radio, 2).await;

    // A flash sale that opens in an hour, the lamp at 50_000.
    let t0 = db_now_unix(&pool).await;
    let (status, sale) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Wave53F Future Flash",
            "starts_at": rfc3339_from_unix(t0 + 3_600),
            "ends_at": rfc3339_from_unix(t0 + 7_200)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create future sale: {sale}");
    let sale_id = sale["id"].as_str().expect("sale id").to_string();
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{sale_id}/items"),
        Some(json!({ "items": [{ "product_id": lamp, "sale_price_poisha": 50_000 }] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set future items: {body}");

    // ------------------------------------------------------------------
    // 1. Suggestions dedupe by title across stores.
    // ------------------------------------------------------------------
    let (status, body) = call(&app, Method::GET, "/v1/search/suggest?q=Duo", None, None).await;
    assert_eq!(status, StatusCode::OK, "suggest duo: {body}");
    let mugs: Vec<&Value> = body["products"]
        .as_array()
        .expect("suggested products")
        .iter()
        .filter(|p| p["title"] == "Wave53F Duo Mug")
        .collect();
    assert_eq!(mugs.len(), 1, "one row per title: {body}");

    // ------------------------------------------------------------------
    // 2. The categories endpoint: counts plus the cover
    //    image from the category's imaged product.
    // ------------------------------------------------------------------
    let (status, body) = call(&app, Method::GET, "/v1/categories", None, None).await;
    assert_eq!(status, StatusCode::OK, "categories: {body}");
    let kitchen = body["categories"]
        .as_array()
        .expect("categories")
        .iter()
        .find(|row| row["category"] == "kitchen")
        .expect("kitchen listed")
        .clone();
    assert_eq!(kitchen["product_count"], 2);
    let cover = kitchen["image_url"].as_str().expect("kitchen has a cover");
    assert!(cover.starts_with("/media/"), "cover is a media URL: {cover}");

    // ------------------------------------------------------------------
    // 3. The future sale resolves NOWHERE.
    // ------------------------------------------------------------------
    let results = browse(&app, "").await;
    let lamp_view = results["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["id"] == lamp)
        .expect("lamp listed")
        .clone();
    assert!(
        lamp_view["flash_price_poisha"].is_null(),
        "no flash price before the window: {lamp_view}"
    );
    let results = browse(&app, "on_sale=true").await;
    assert_eq!(results["total"], 0, "nothing is on sale yet: {results}");
    let results = browse(&app, "min_price_poisha=45000&max_price_poisha=55000").await;
    assert_eq!(results["total"], 0, "the future flash band is empty: {results}");
    let results = browse(&app, "min_price_poisha=85000&max_price_poisha=95000").await;
    assert_eq!(results["total"], 1, "the lamp prices at base: {results}");
    assert_eq!(results["items"][0]["id"], lamp);

    // ------------------------------------------------------------------
    // 4. Store-scoped facets (the shop page's data): the
    //    scope narrows the other groups, not its own.
    // ------------------------------------------------------------------
    let results = browse(&app, "store_slug=wave53f-mart").await;
    assert_eq!(results["total"], 1);
    assert_eq!(results["items"][0]["id"], duo_mart);
    let shop_facet = results["facets"]["stores"]
        .as_array()
        .expect("store facets")
        .iter()
        .find(|facet| facet["slug"] == "wave53f-shop")
        .expect("shop facet")
        .clone();
    assert_eq!(shop_facet["count"], 4, "the shop's full count: {results}");
    let kitchen_facet = results["facets"]["categories"]
        .as_array()
        .expect("category facets")
        .iter()
        .find(|facet| facet["category"] == "kitchen")
        .expect("kitchen facet")
        .clone();
    assert_eq!(
        kitchen_facet["count"], 1,
        "categories count the scoped set: {results}"
    );

    // ------------------------------------------------------------------
    // 5. Rating and sold sorts against the untouched
    //    product: rated-and-sold beats unrated-and-unsold.
    // ------------------------------------------------------------------
    let results = browse(&app, "category=electronics&sort=rating").await;
    assert_eq!(results["items"][0]["id"], old_radio, "rated first: {results}");
    assert_eq!(results["items"][1]["id"], new_radio);
    let results = browse(&app, "category=electronics&sort=sold").await;
    assert_eq!(results["items"][0]["id"], old_radio, "sold first: {results}");
    let old_view = results["items"][0].clone();
    assert_eq!(old_view["sold_count"], 2);
    assert_eq!(old_view["rating_avg"], 2.0);
}
