//! Wave 52 (second test file): the flash-sale money paths the
//! main Wave 52 test does not reach — variant composition and
//! cap accounting on VARIANT lines, coupon allocation over
//! sale-priced goods, the seller's flash-sale view, and the
//! merchandising boundary rules (preview clock edges, the
//! malformed-preview 400, unpublished collections).
//!
//! The variant assertions pin the subtlest rule of the wave:
//! the flash price replaces the product's BASE price, and a
//! variant's price delta composes on top of it — in the cart,
//! in the checkout snapshot, AND in the cap aggregate (a
//! variant line's snapshot is sale + delta, so the aggregate
//! must add the delta back or variant purchases would never
//! consume the cap).
//!
//! The small helpers are copied from the Wave 52 test on
//! purpose (integration test files stay self-contained).
//!
//! Gated on `DATABASE_URL`: without it the test prints a skip
//! notice and passes. CI provides a Postgres 16 service.

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

/// Add a product (optionally one variant) to the buyer's cart.
async fn add_to_cart(
    app: &Router,
    token: &str,
    product_id: &str,
    variant_id: Option<&str>,
    quantity: i64,
) {
    let mut body = json!({ "product_id": product_id, "quantity": quantity });
    if let Some(variant_id) = variant_id {
        body["variant_id"] = json!(variant_id);
    }
    let (status, response) = call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(body),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add to cart: {response}");
}

/// Checkout the buyer's current cart with COD; returns the order.
async fn checkout_cod(app: &Router, token: &str, coupon_code: Option<&str>) -> Value {
    let mut body = json!({
        "delivery_name": "Test Buyer",
        "delivery_phone": "01999999999",
        "delivery_address": "House 9, Road 3, Khulna",
        "delivery_zone": "inside_dhaka",
        "payment_method": "cod"
    });
    if let Some(code) = coupon_code {
        body["coupon_code"] = json!(code);
    }
    let (status, response) = call(
        app,
        Method::POST,
        "/v1/orders",
        Some(body),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "cod checkout: {response}");
    response
}

/// Format a unix timestamp as RFC 3339 UTC (the civil-from-days
/// algorithm — the test crate has no chrono dependency).
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

/// The current unix time, read from the database itself (the
/// clock every stored window is compared against).
async fn db_now_unix(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM now())::bigint")
        .fetch_one(pool)
        .await
        .expect("read database clock")
}

#[tokio::test]
async fn wave52b_variant_coupon_seller_view_and_boundaries() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 52b integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate (the Wave 51 truncate canon plus the Wave 52
    // tables) so the test is re-runnable against the same DB.
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

    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave52b-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave52b-store").await;
    let buyer = login(&app, "01999999999").await;

    // V: a variant product (base 100_000; "Large" adds 5_000).
    // W: the coupon product (base 200_000, no variants).
    let product_v = create_product(&app, &seller, &store, "Wave52B Shirt", 100_000, 10).await;
    let (status, variant) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/products/{product_v}/variants"),
        Some(json!({ "name": "Large", "price_delta_poisha": 5_000, "stock": 10 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create variant: {variant}");
    let variant_id = variant["id"].as_str().expect("variant id").to_string();
    let product_w = create_product(&app, &seller, &store, "Wave52B Watch", 200_000, 10).await;

    let t0 = db_now_unix(&pool).await;

    // One live sale: V at 80_000 with cap 1, W at 150_000
    // uncapped.
    let (status, sale) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Wave52B Sale",
            "starts_at": rfc3339_from_unix(t0 - 60),
            "ends_at": rfc3339_from_unix(t0 + 3_600)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create sale: {sale}");
    let sale_id = sale["id"].as_str().expect("sale id").to_string();
    let (status, detail) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{sale_id}/items"),
        Some(json!({
            "items": [
                { "product_id": product_v, "sale_price_poisha": 80_000, "stock_cap": 1 },
                { "product_id": product_w, "sale_price_poisha": 150_000 }
            ]
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set items: {detail}");

    // ------------------------------------------------------------------
    // 1. Variant lines: the delta composes over the SALE base,
    //    in the cart and in the checkout snapshot.
    // ------------------------------------------------------------------
    add_to_cart(&app, &buyer, &product_v, Some(&variant_id), 1).await;
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "cart: {cart}");
    assert_eq!(
        cart["items"][0]["unit_price_poisha"], 85_000,
        "variant line = sale 80_000 + delta 5_000: {cart}"
    );
    let order = checkout_cod(&app, &buyer, None).await;
    assert_eq!(
        order["sub_orders"][0]["goods_subtotal_poisha"], 85_000,
        "checkout snapshot carries sale + delta: {order}"
    );
    assert_eq!(
        order["sub_orders"][0]["items"][0]["unit_price_poisha"], 85_000,
        "order line snapshot: {order}"
    );

    // The cap aggregate counts the variant line (its snapshot
    // is sale + delta — the aggregate's variant term is what
    // makes this purchase consume the cap): with cap 1 spent,
    // the flash price stops resolving everywhere.
    let (status, detail) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product_v}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        detail["flash_price_poisha"].is_null(),
        "variant purchase spent the cap: {detail}"
    );
    add_to_cart(&app, &buyer, &product_v, Some(&variant_id), 1).await;
    let (status, cart) = call(&app, Method::GET, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        cart["items"][0]["unit_price_poisha"], 105_000,
        "cap spent: variant line back to base + delta: {cart}"
    );
    let (status, _) = call(&app, Method::DELETE, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "clear cart");

    // ------------------------------------------------------------------
    // 2. A platform coupon's discount is computed on the
    //    SALE-priced goods (10% of 150_000 = 15_000, not 10%
    //    of the 200_000 base), and the payout snapshot is the
    //    platform-coupon shape: goods − commission on the
    //    full sale goods (the platform bears its discount).
    // ------------------------------------------------------------------
    let (status, coupon) = call(
        &app,
        Method::POST,
        "/v1/admin/coupons",
        Some(json!({
            "code": "flash10",
            "discount_type": "percent",
            "discount_value": 10,
            "max_discount_poisha": 1_000_000
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create coupon: {coupon}");
    add_to_cart(&app, &buyer, &product_w, None, 1).await;
    let order = checkout_cod(&app, &buyer, Some("FLASH10")).await;
    let sub_order = &order["sub_orders"][0];
    assert_eq!(sub_order["goods_subtotal_poisha"], 150_000, "order: {order}");
    assert_eq!(
        sub_order["discount_poisha"], 15_000,
        "coupon base is the sale price actually paid: {order}"
    );
    assert_eq!(sub_order["seller_payout_poisha"], 138_000, "order: {order}");

    // ------------------------------------------------------------------
    // 3. The seller's flash view: their items, the live cap
    //    accounting, and nobody else's seat.
    // ------------------------------------------------------------------
    let (status, view) = call(
        &app,
        Method::GET,
        "/v1/seller/flash-sale",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller flash view: {view}");
    assert_eq!(view["sale_id"], sale_id, "seller view: {view}");
    let items = view["items"].as_array().expect("seller items");
    assert_eq!(items.len(), 2, "both products are the seller's: {view}");
    let v_item = items
        .iter()
        .find(|item| item["product_id"] == product_v)
        .expect("V in seller view");
    assert_eq!(v_item["sale_price_poisha"], 80_000);
    assert_eq!(v_item["sold_at_sale"], 1, "variant sale counted: {v_item}");
    assert_eq!(v_item["cap_remaining"], 0);
    let w_item = items
        .iter()
        .find(|item| item["product_id"] == product_w)
        .expect("W in seller view");
    assert_eq!(w_item["sold_at_sale"], 1);
    assert!(w_item["cap_remaining"].is_null(), "uncapped: {w_item}");

    // A buyer sees the sale but none of its items (they own no
    // store); an anonymous caller is refused outright.
    let (status, view) = call(
        &app,
        Method::GET,
        "/v1/seller/flash-sale",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "buyer flash view: {view}");
    assert_eq!(view["items"].as_array().map(Vec::len), Some(0));
    let (status, _) = call(&app, Method::GET, "/v1/seller/flash-sale", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "anonymous refused");

    // ------------------------------------------------------------------
    // 4. Search carries the same view fields (sold_count and
    //    the flash pair) — one product view everywhere.
    // ------------------------------------------------------------------
    let (status, results) = call(
        &app,
        Method::GET,
        "/v1/search?q=Wave52B%20Watch",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "search: {results}");
    let found = results["items"]
        .as_array()
        .expect("search items")
        .iter()
        .find(|p| p["id"] == product_w)
        .expect("W in search results")
        .clone();
    assert_eq!(found["flash_price_poisha"], 150_000, "search view: {found}");
    assert_eq!(found["sold_count"], 0, "placed is not sold: {found}");

    // ------------------------------------------------------------------
    // 5. Boundaries: the preview clock is exact, a malformed
    //    `at` is a 400, and unpublished collections are 404s.
    // ------------------------------------------------------------------
    let (status, banner) = call(
        &app,
        Method::POST,
        "/v1/admin/banners",
        Some(json!({
            "title": "Boundary Banner",
            "starts_at": rfc3339_from_unix(t0 + 1_000),
            "ends_at": rfc3339_from_unix(t0 + 2_000)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "boundary banner: {banner}");
    let banner_id = banner["id"].as_str().expect("banner id").to_string();

    for (at, visible) in [(t0 + 999, false), (t0 + 1_000, true), (t0 + 1_999, true), (t0 + 2_000, false)] {
        let (status, preview) = call(
            &app,
            Method::GET,
            &format!("/v1/admin/home-preview?at={}", rfc3339_from_unix(at)),
            None,
            Some(&admin),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "preview at {at}: {preview}");
        let present = preview["banners"]
            .as_array()
            .expect("preview banners")
            .iter()
            .any(|b| b["id"] == banner_id);
        assert_eq!(present, visible, "banner visibility at {at}: {preview}");
    }

    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/admin/home-preview?at=not-a-time",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "malformed at: {body}");

    let (status, collection) = call(
        &app,
        Method::POST,
        "/v1/admin/collections",
        Some(json!({
            "slug": "wave52b-draft",
            "title": "Draft Picks",
            "is_active": false
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "draft collection: {collection}");
    let (status, _) = call(&app, Method::GET, "/v1/collections/wave52b-draft", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unpublished collection 404s");
    let (status, _) = call(&app, Method::GET, "/v1/collections/no-such-slug", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown collection 404s");
}
