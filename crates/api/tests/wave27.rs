//! Wave 27 integration test: seller and platform analytics —
//! seller totals count goods over NON-cancelled sub-orders
//! only and earnings as the delivered payout snapshots, the
//! daily series is zero-filled to the window length with
//! today's Asia/Dhaka bucket carrying today's sales, top
//! products rank by goods, sellers see only their own
//! numbers, the CSV export carries the same daily series,
//! the admin view aggregates the whole platform (GMV =
//! goods − discount + delivery over non-cancelled slices,
//! commission over delivered slices only), non-admins get a
//! 403, and the window clamps to 7..=90 days. The delivered
//! orders are produced by driving the real flow (COD
//! checkout -> seller confirms -> DEV courier shipment ->
//! refresh until delivered); the small helpers are copied
//! from the Wave 9 test on purpose, since integration test
//! files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.
//! CI provides a Postgres 16 service and sets `DATABASE_URL`
//! for the rust job.

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

/// Send one request and return (status, content-type,
/// content-disposition, raw text body) — for the CSV export,
/// which is not JSON.
async fn call_raw(
    app: &Router,
    uri: &str,
    token: &str,
) -> (StatusCode, String, String, String) {
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request builds");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let content_disposition = headers
        .get("content-disposition")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let text = String::from_utf8(bytes.to_vec()).expect("body is UTF-8");
    (status, content_type, content_disposition, text)
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

/// Buy one unit of the product and drive the order all the way to
/// delivered through the DEV courier; returns the order id.
async fn buy_and_deliver(app: &Router, buyer: &str, seller: &str, product_id: &str) {
    add_to_cart(app, buyer, product_id, 1).await;
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

    // Four polls walk the DEV courier created -> delivered.
    for _ in 0..4 {
        let uri = format!("/v1/shipments/{shipment_id}/refresh");
        let (status, _) = call(app, Method::POST, &uri, None, Some(seller)).await;
        assert_eq!(status, StatusCode::OK, "refresh");
    }
}

/// Buy one unit and have the BUYER cancel the whole order
/// while it is still placed.
async fn buy_and_cancel(app: &Router, buyer: &str, product_id: &str) {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    let order_id = order["id"].as_str().expect("order id").to_string();
    let uri = format!("/v1/orders/{order_id}/cancel");
    let (status, body) = call(app, Method::POST, &uri, None, Some(buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer cancel: {body}");
}

/// GET a seller's analytics for a URI; asserts 200.
async fn get_seller_analytics(app: &Router, token: &str, uri: &str) -> Value {
    let (status, body) = call(app, Method::GET, uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "seller analytics {uri}: {body}");
    body
}

#[tokio::test]
async fn wave27_seller_and_admin_analytics_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 27 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode (deliveries) on.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave27-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // The platform admin (promoted by SQL — there is no endpoint).
    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // Seller A: two products (200_000 and 50_000 poisha of goods).
    // At the default 8% commission a delivered unit of the first
    // earns 184_000 and of the second 46_000. Inside-Dhaka
    // delivery for the default 1 kg parcel is 6_000 per slice.
    let seller_a = login(&app, "01799999999").await;
    let store_a = create_store(&app, &seller_a, "wave27-store-a").await;
    let product_a1 = create_product(&app, &seller_a, &store_a, "Wave27 Product A1", 200_000, 10).await;
    let product_a2 = create_product(&app, &seller_a, &store_a, "Wave27 Product A2", 50_000, 10).await;

    // Seller B: one 100_000 product (delivered earns 92_000).
    let seller_b = login(&app, "01599999999").await;
    let store_b = create_store(&app, &seller_b, "wave27-store-b").await;
    let product_b = create_product(&app, &seller_b, &store_b, "Wave27 Product B", 100_000, 10).await;

    let buyer = login(&app, "01999999999").await;

    // The sales: A delivers both products (two orders) and has
    // one more order cancelled by the buyer; B delivers one.
    buy_and_deliver(&app, &buyer, &seller_a, &product_a1).await;
    buy_and_deliver(&app, &buyer, &seller_a, &product_a2).await;
    buy_and_cancel(&app, &buyer, &product_a1).await;
    buy_and_deliver(&app, &buyer, &seller_b, &product_b).await;

    // 1. Seller A, default window: goods count the two
    // non-cancelled slices only (250_000), earnings are the
    // delivered snapshots (184_000 + 46_000 = 230_000).
    let analytics = get_seller_analytics(&app, &seller_a, "/v1/seller/analytics").await;
    assert_eq!(analytics["days"], 30, "analytics: {analytics}");
    let totals = &analytics["totals"];
    assert_eq!(totals["sub_orders"], 2, "totals: {totals}");
    assert_eq!(totals["goods_poisha"], 250_000, "totals: {totals}");
    assert_eq!(totals["earnings_poisha"], 230_000, "totals: {totals}");
    assert_eq!(totals["delivered"], 2, "totals: {totals}");
    assert_eq!(totals["returned"], 0, "totals: {totals}");
    assert_eq!(totals["cancelled"], 1, "totals: {totals}");
    assert_eq!(totals["return_rate_bps"], 0, "totals: {totals}");

    // The daily series is zero-filled to the window length,
    // and its last (today, Asia/Dhaka) bucket carries today's
    // sales; the whole series sums back to the totals.
    let daily = analytics["daily"].as_array().expect("daily series");
    assert_eq!(daily.len(), 30, "daily length: {analytics}");
    let today = &daily[daily.len() - 1];
    assert_eq!(today["sub_orders"], 2, "today bucket: {today}");
    assert_eq!(today["goods_poisha"], 250_000, "today bucket: {today}");
    assert_eq!(today["earnings_poisha"], 230_000, "today bucket: {today}");
    let series_goods: i64 = daily
        .iter()
        .map(|row| row["goods_poisha"].as_i64().expect("goods number"))
        .sum();
    assert_eq!(series_goods, 250_000, "series sums to totals");

    // Top products rank by goods: A1 (200_000) before A2.
    let top = analytics["top_products"].as_array().expect("top products");
    assert_eq!(top.len(), 2, "top products: {top:?}");
    assert_eq!(top[0]["product_id"], product_a1, "top products: {top:?}");
    assert_eq!(top[0]["units"], 1);
    assert_eq!(top[0]["goods_poisha"], 200_000);
    assert_eq!(top[1]["product_id"], product_a2, "top products: {top:?}");
    assert_eq!(top[1]["goods_poisha"], 50_000);

    // The status breakdown is the operations view: cancelled
    // is in there too.
    let breakdown = analytics["status_breakdown"]
        .as_array()
        .expect("status breakdown");
    let count_for = |status: &str| {
        breakdown
            .iter()
            .find(|row| row["status"] == status)
            .map(|row| row["count"].as_i64().expect("count number"))
    };
    assert_eq!(count_for("delivered"), Some(2), "breakdown: {breakdown:?}");
    assert_eq!(count_for("cancelled"), Some(1), "breakdown: {breakdown:?}");

    // Both delivered slices shipped with the DEV courier.
    let couriers = analytics["courier_breakdown"]
        .as_array()
        .expect("courier breakdown");
    assert_eq!(couriers.len(), 1, "couriers: {couriers:?}");
    assert_eq!(couriers[0]["courier"], "dev", "couriers: {couriers:?}");
    assert_eq!(couriers[0]["count"], 2, "couriers: {couriers:?}");

    // 2. Isolation: seller B sees only their own 100_000 sale.
    let analytics_b = get_seller_analytics(&app, &seller_b, "/v1/seller/analytics").await;
    assert_eq!(analytics_b["totals"]["sub_orders"], 1, "B: {analytics_b}");
    assert_eq!(analytics_b["totals"]["goods_poisha"], 100_000, "B: {analytics_b}");
    assert_eq!(analytics_b["totals"]["earnings_poisha"], 92_000, "B: {analytics_b}");
    assert_eq!(analytics_b["totals"]["cancelled"], 0, "B: {analytics_b}");
    let top_b = analytics_b["top_products"].as_array().expect("B top");
    assert_eq!(top_b.len(), 1, "B top: {top_b:?}");
    assert_eq!(top_b[0]["product_id"], product_b, "B top: {top_b:?}");

    // A user without a store gets the dashboard's 404.
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/seller/analytics",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no store: {body}");

    // 3. The window clamps: 3 days asks become 7, 500 become
    // 90 — in the response's `days` and the series length.
    let clamped = get_seller_analytics(&app, &seller_a, "/v1/seller/analytics?days=3").await;
    assert_eq!(clamped["days"], 7, "clamped: {clamped}");
    assert_eq!(clamped["daily"].as_array().expect("daily").len(), 7);
    let clamped = get_seller_analytics(&app, &seller_a, "/v1/seller/analytics?days=500").await;
    assert_eq!(clamped["days"], 90, "clamped: {clamped}");
    assert_eq!(clamped["daily"].as_array().expect("daily").len(), 90);

    // 4. The CSV export: same daily series, exact header,
    // attachment filename, and today's row matches the JSON.
    let (status, content_type, disposition, csv) =
        call_raw(&app, "/v1/seller/analytics/export.csv?days=7", &seller_a).await;
    assert_eq!(status, StatusCode::OK, "csv status: {csv}");
    assert!(
        content_type.starts_with("text/csv"),
        "csv content type: {content_type}"
    );
    assert!(
        disposition.contains("seller-analytics-7d.csv"),
        "csv filename: {disposition}"
    );
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 8, "header + 7 days: {csv}");
    assert_eq!(lines[0], "date,orders,goods_poisha,earnings_poisha");
    let today_line = lines[lines.len() - 1];
    let json_today = {
        let week = get_seller_analytics(&app, &seller_a, "/v1/seller/analytics?days=7").await;
        let daily = week["daily"].as_array().expect("week daily").clone();
        daily[daily.len() - 1].clone()
    };
    let expected_today = format!(
        "{},2,250000,230000",
        json_today["date"].as_str().expect("today date")
    );
    assert_eq!(today_line, expected_today, "csv today row vs json");

    // 5. Admin analytics: GMV over the three non-cancelled
    // slices = goods 350_000 + delivery 3 * 6_000 = 368_000;
    // commission over delivered = 8% of 350_000 = 28_000.
    let (status, platform) = call(
        &app,
        Method::GET,
        "/v1/admin/analytics",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin analytics: {platform}");
    assert_eq!(platform["days"], 30, "platform: {platform}");
    let totals = &platform["totals"];
    assert_eq!(totals["gmv_poisha"], 368_000, "totals: {totals}");
    assert_eq!(totals["orders"], 4, "totals: {totals}");
    assert_eq!(totals["sub_orders"], 4, "totals: {totals}");
    assert_eq!(totals["commission_poisha"], 28_000, "totals: {totals}");
    assert!(
        totals["new_stores"].as_i64().expect("new stores") >= 2,
        "totals: {totals}"
    );
    assert!(
        totals["new_users"].as_i64().expect("new users") >= 4,
        "totals: {totals}"
    );
    assert_eq!(totals["charges_poisha"], 0, "totals: {totals}");
    assert_eq!(totals["coupons"]["redemptions"], 0, "totals: {totals}");
    assert_eq!(totals["payouts"]["requested_count"], 0, "totals: {totals}");

    // Top stores: A (262_000 GMV) ahead of B (106_000).
    let top_stores = platform["top_stores"].as_array().expect("top stores");
    assert_eq!(top_stores.len(), 2, "top stores: {top_stores:?}");
    assert_eq!(top_stores[0]["store_id"], store_a, "top stores: {top_stores:?}");
    assert_eq!(top_stores[0]["slug"], "wave27-store-a");
    assert_eq!(top_stores[0]["gmv_poisha"], 262_000, "top stores: {top_stores:?}");
    assert_eq!(top_stores[0]["sub_orders"], 2, "top stores: {top_stores:?}");
    assert_eq!(top_stores[1]["store_id"], store_b, "top stores: {top_stores:?}");
    assert_eq!(top_stores[1]["gmv_poisha"], 106_000, "top stores: {top_stores:?}");

    // The admin daily series: zero-filled, today's bucket
    // carries 3 non-cancelled orders' slices (A's two
    // delivered + B's one) and the full GMV.
    let daily = platform["daily"].as_array().expect("admin daily");
    assert_eq!(daily.len(), 30, "admin daily: {platform}");
    let today = &daily[daily.len() - 1];
    assert_eq!(today["orders"], 3, "admin today: {today}");
    assert_eq!(today["gmv_poisha"], 368_000, "admin today: {today}");

    // 6. Non-admins are refused: a seller and a buyer both
    // get 403 on the platform view.
    for (label, token) in [("seller", &seller_a), ("buyer", &buyer)] {
        let (status, body) = call(
            &app,
            Method::GET,
            "/v1/admin/analytics",
            None,
            Some(token),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label} refused: {body}");
    }
}
