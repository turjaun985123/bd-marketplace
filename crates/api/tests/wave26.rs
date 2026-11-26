//! Wave 26 integration test: sponsored product promotions —
//! a seller with no earnings cannot promote (400); after a
//! delivered order earns a known balance, a 3-day promotion
//! at the default 100-taka day price charges exactly 30_000
//! from that balance (the balance endpoint's new
//! `charges_total_poisha` and the dropped `available_poisha`
//! prove the deduction, and the dashboard agrees); a second
//! overlapping promotion is a 409; the product view carries
//! `is_promoted`; search puts the promoted product first
//! under the default ordering but keeps the price sort pure;
//! an admin price change reprices the NEXT promotion only;
//! cancelling stops the placement with NO refund (charges
//! unchanged); the remaining balance is exactly what a
//! payout may take (available + 1 refuses, available itself
//! succeeds); and a second seller sees none of it. The
//! settings row the test writes is DELETEd at the end —
//! platform_settings is global across test binaries.
//!
//! The small helpers are copied from the Wave 9 test on
//! purpose, since integration test files stay self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.
//! CI provides a Postgres 16 service and sets `DATABASE_URL`.

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
    create_product_with_status(app, token, store_id, title, price_poisha, stock, "active").await
}

/// Create a product with an explicit status; returns the id.
#[allow(clippy::too_many_arguments)]
async fn create_product_with_status(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
    status: &str,
) -> String {
    let (status_code, body) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": stock,
            "status": status
        })),
        Some(token),
    )
    .await;
    assert_eq!(
        status_code,
        StatusCode::CREATED,
        "create product {title}: {body}"
    );
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

/// GET the seller balance for a token.
async fn get_balance(app: &Router, token: &str) -> Value {
    let (status, body) = call(app, Method::GET, "/v1/seller/balance", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "balance: {body}");
    body
}

/// POST a promotion purchase for a product.
async fn promote(
    app: &Router,
    token: &str,
    product_id: &str,
    days: i64,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/products/{product_id}/promote");
    call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "days": days })),
        Some(token),
    )
    .await
}

#[tokio::test]
async fn wave26_sponsored_promotions_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 26 integration test");
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
    let state = AppState::new(Some(pool.clone()), "wave26-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // The platform admin (promoted by SQL — there is no endpoint).
    let admin = login(&app, "01711111111").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin");

    // Seller A with three products, seller B with a same-titled
    // cheaper product, and a buyer.
    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave26-store").await;
    // 200_000 poisha of goods: 8% commission is 16_000, so a
    // delivered unit earns exactly 184_000 (the Wave 9 numbers).
    let product1 = create_product(&app, &seller, &store, "Wave26 Product", 200_000, 10).await;
    let product2 =
        create_product(&app, &seller, &store, "Wave26 Second Product", 100_000, 10).await;
    let draft = create_product_with_status(
        &app,
        &seller,
        &store,
        "Wave26 Draft Product",
        80_000,
        10,
        "draft",
    )
    .await;
    let other_seller = login(&app, "01599999999").await;
    let other_store = create_store(&app, &other_seller, "wave26-other-store").await;
    let product3 =
        create_product(&app, &other_seller, &other_store, "Wave26 Product", 50_000, 10).await;
    let buyer = login(&app, "01999999999").await;

    // 1. The public policy: the default price and the day cap.
    let (status, policy) = call(&app, Method::GET, "/v1/promotions/policy", None, None).await;
    assert_eq!(status, StatusCode::OK, "policy: {policy}");
    assert_eq!(policy["price_per_day_poisha"], 10_000);
    assert_eq!(policy["max_days"], 30);

    // 2. No earnings yet: a promotion cannot be bought. Illegal
    // day counts and a draft product refuse too, and another
    // seller's product is a 404.
    let (status, body) = promote(&app, &seller, &product1, 3).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "promote broke: {body}");
    assert!(
        body["error"]
            .as_str()
            .expect("error message")
            .contains("insufficient"),
        "insufficient message: {body}"
    );
    let (status, _) = promote(&app, &seller, &product1, 0).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "zero days");
    let (status, _) = promote(&app, &seller, &product1, 31).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "31 days");
    let (status, _) = promote(&app, &seller, &draft, 1).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "promote draft");
    let (status, _) = promote(&app, &other_seller, &product1, 1).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "promote someone else's");

    // 3. One delivered sale earns exactly 184_000; charges are 0.
    buy_and_deliver(&app, &buyer, &seller, &product1).await;
    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 184_000, "balance: {balance}");
    assert_eq!(balance["charges_total_poisha"], 0);
    assert_eq!(balance["available_poisha"], 184_000);

    // 4. Buy 3 days at the default 10_000/day: the charge is
    // 30_000 and the available balance drops by exactly that.
    let (status, bought) = promote(&app, &seller, &product1, 3).await;
    assert_eq!(status, StatusCode::CREATED, "promote: {bought}");
    assert_eq!(bought["cost_poisha"], 30_000);
    assert_eq!(bought["available_after_poisha"], 154_000);
    assert_eq!(bought["promotion"]["product_id"], product1);
    assert_eq!(bought["promotion"]["days"], 3);
    assert_eq!(bought["promotion"]["cost_poisha"], 30_000);
    assert_eq!(bought["promotion"]["status"], "active");
    let promotion1 = bought["promotion"]["id"]
        .as_str()
        .expect("promotion id")
        .to_string();

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["earned_poisha"], 184_000, "balance: {balance}");
    assert_eq!(balance["charges_total_poisha"], 30_000);
    assert_eq!(balance["available_poisha"], 154_000);

    // The dashboard reads the same aggregate, charges included.
    let (status, dashboard) =
        call(&app, Method::GET, "/v1/seller/dashboard", None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "dashboard: {dashboard}");
    assert_eq!(dashboard["earnings"]["charges_total_poisha"], 30_000);
    assert_eq!(dashboard["earnings"]["available_poisha"], 154_000);

    // 5. A second live promotion on the same product is a 409.
    let (status, body) = promote(&app, &seller, &product1, 1).await;
    assert_eq!(status, StatusCode::CONFLICT, "overlap: {body}");

    // 6. The product view carries the flag; the unpromoted
    // product does not.
    let (status, view) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product1}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "product view: {view}");
    assert_eq!(view["is_promoted"], true, "product view: {view}");
    let (status, view) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product2}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "product view: {view}");
    assert_eq!(view["is_promoted"], false, "product view: {view}");

    // 7. Search: under the default (relevance) ordering the
    // promoted product leads the identically-titled cheaper
    // one; under price_asc the sort stays pure and the cheap
    // product leads.
    let (status, results) = call(
        &app,
        Method::GET,
        "/v1/search?q=Wave26%20Product",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "search: {results}");
    let items = results["items"].as_array().expect("search items");
    let ids: Vec<&str> = items
        .iter()
        .map(|item| item["id"].as_str().expect("item id"))
        .collect();
    assert_eq!(ids.first(), Some(&product1.as_str()), "search ids: {ids:?}");
    assert_eq!(items[0]["is_promoted"], true);
    let pos1 = ids.iter().position(|id| *id == product1).expect("p1 in results");
    let pos3 = ids.iter().position(|id| *id == product3).expect("p3 in results");
    assert!(pos1 < pos3, "promoted first: {ids:?}");

    let (status, results) = call(
        &app,
        Method::GET,
        "/v1/search?q=Wave26%20Product&sort=price_asc",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "price search: {results}");
    assert_eq!(
        results["items"][0]["id"], product3,
        "price sort stays pure: {results}"
    );

    // 8. The admin halves the per-day price; the policy and the
    // NEXT purchase use it (the bought one keeps its snapshot).
    let (status, settings) = call(
        &app,
        Method::PUT,
        "/v1/admin/settings",
        Some(json!({ "promotion_price_per_day_poisha": 5_000 })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "settings: {settings}");
    assert_eq!(settings["promotion_price_per_day_poisha"], 5_000);
    let (status, policy) = call(&app, Method::GET, "/v1/promotions/policy", None, None).await;
    assert_eq!(status, StatusCode::OK, "policy: {policy}");
    assert_eq!(policy["price_per_day_poisha"], 5_000);

    let (status, bought) = promote(&app, &seller, &product2, 2).await;
    assert_eq!(status, StatusCode::CREATED, "promote second: {bought}");
    assert_eq!(bought["cost_poisha"], 10_000);
    assert_eq!(bought["available_after_poisha"], 144_000);
    let promotion2 = bought["promotion"]["id"]
        .as_str()
        .expect("promotion id")
        .to_string();

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["charges_total_poisha"], 40_000, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 144_000);

    // 9. The seller's promotion list: both rows, both live.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/seller/promotions",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "promotions: {list}");
    let items = list["items"].as_array().expect("promotion items");
    assert_eq!(items.len(), 2, "promotions: {list}");
    for item in items {
        assert_eq!(item["display_status"], "active", "promotion: {item}");
    }

    // 10. Cancel the first promotion: placement stops at once
    // and the money does NOT come back (no refunds).
    let uri = format!("/v1/seller/promotions/{promotion1}/cancel");
    let (status, cancelled) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "cancel: {cancelled}");
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["display_status"], "cancelled");

    let (status, view) = call(
        &app,
        Method::GET,
        &format!("/v1/products/{product1}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "product view: {view}");
    assert_eq!(view["is_promoted"], false, "product view: {view}");

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["charges_total_poisha"], 40_000, "balance: {balance}");
    assert_eq!(balance["available_poisha"], 144_000);

    // A second cancel of the same promotion is a 409.
    let (status, _) = call(&app, Method::POST, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::CONFLICT, "re-cancel");

    // 11. The other seller sees an empty list and cannot cancel
    // seller A's live promotion (404).
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/seller/promotions",
        None,
        Some(&other_seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "other promotions: {list}");
    assert_eq!(list["items"].as_array().map(Vec::len), Some(0));
    let uri = format!("/v1/seller/promotions/{promotion2}/cancel");
    let (status, _) = call(&app, Method::POST, &uri, None, Some(&other_seller)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "cancel someone else's");

    // 12. The balance the promotions left is exactly what a
    // payout may take: one poisha more refuses, the exact
    // amount succeeds (the Wave 9 account + amount rules).
    let (status, account) = call(
        &app,
        Method::PUT,
        "/v1/seller/payout-account",
        Some(json!({
            "method": "bkash",
            "account_name": "Wave26 Seller",
            "account_number": "01799999999"
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save payout account: {account}");

    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": 144_001 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "payout over balance: {body}");

    let (status, payout) = call(
        &app,
        Method::POST,
        "/v1/seller/payouts",
        Some(json!({ "amount_poisha": 144_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "payout exact balance: {payout}");

    let balance = get_balance(&app, &seller).await;
    assert_eq!(balance["pending_poisha"], 144_000, "balance: {balance}");
    assert_eq!(balance["charges_total_poisha"], 40_000);
    assert_eq!(balance["available_poisha"], 0);

    // 13. Cleanup: platform_settings is global across test
    // binaries — remove the row this test wrote and prove the
    // policy falls back to the built-in default.
    sqlx::query("DELETE FROM platform_settings WHERE key = 'promotion_price_per_day_poisha'")
        .execute(&pool)
        .await
        .expect("clean promotion price setting");
    let (status, policy) = call(&app, Method::GET, "/v1/promotions/policy", None, None).await;
    assert_eq!(status, StatusCode::OK, "policy: {policy}");
    assert_eq!(policy["price_per_day_poisha"], 10_000);
}
