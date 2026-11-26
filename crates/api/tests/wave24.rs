//! Wave 24 integration test: wishlist + recommendations.
//!
//! Wishlist: add is idempotent and answers with the resulting
//! state, the ids read tracks adds and removes, remove is
//! idempotent too, a draft product cannot be added (404 — the
//! public visibility rule), and one user's list is invisible
//! to another. Related: the scoring rule orders a
//! same-category product (other store) before a same-store
//! product (other category) before an unrelated one, drafts
//! and the base product itself never appear, and a draft
//! base is a 404. Also-bought: two products bought in one
//! split checkout (two stores, two sub-orders, one order)
//! pair up with `co_count` 1, a second order containing only
//! one of them adds nothing, and a never-ordered product
//! honestly returns an empty list. Recommendations: a buyer
//! with a DELIVERED electronics purchase gets the other
//! electronics (not the bought one) with `personalized`
//! true; a wishlist-only signal also personalises (and the
//! wishlisted product itself is excluded, leaving an empty
//! list here); a fresh user gets the labelled top-rated
//! fallback.
//!
//! The small helpers are copied from the Wave 9 test on
//! purpose, since integration test files stay self-contained.
//! Delivery is the seller status walk (placed -> confirmed ->
//! shipped -> delivered) — no courier simulator needed.
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

/// Create a product with a category and an explicit status
/// (the Wave 15 create payload carries `category`); returns
/// the product id.
async fn create_categorised_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
    stock: i64,
    category: &str,
    product_status: &str,
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
            "status": product_status,
            "category": category
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

/// Walk one sub-order placed -> confirmed -> shipped ->
/// delivered through the seller status endpoint.
async fn deliver_sub_order(app: &Router, seller: &str, sub_order_id: &str) {
    for next in ["confirmed", "shipped", "delivered"] {
        let (status, body) = set_sub_order_status(app, seller, sub_order_id, next).await;
        assert_eq!(status, StatusCode::OK, "move to {next}: {body}");
    }
}

/// The ids on a user's wishlist, in the API's order.
async fn wishlist_ids(app: &Router, token: &str) -> Vec<String> {
    let (status, body) = call(app, Method::GET, "/v1/wishlist/ids", None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "wishlist ids: {body}");
    body["product_ids"]
        .as_array()
        .expect("product_ids array")
        .iter()
        .map(|id| id.as_str().expect("product id").to_string())
        .collect()
}

/// The product ids of a `{items: [...]}` rail response, in order.
fn item_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|item| item["id"].as_str().expect("item id").to_string())
        .collect()
}

#[tokio::test]
async fn wave24_wishlist_and_recommendations_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 24 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE payouts, payout_accounts, review_replies, reviews, \
         shipment_events, shipments, payment_events, payments, \
         order_items, sub_orders, orders, cart_items, carts, wishlist_items, \
         products, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "wave24-test-secret".to_string(), true);
    let app = build_app(state);

    // Two stores. The catalogue is arranged for the related
    // scoring rule: from P1 (electronics, store A), P2 is
    // same-category/other-store (score 4), P3 is
    // other-category/same-store (score 2), P4 is unrelated
    // (score 0), and P5 is a draft in P1's own category —
    // invisible everywhere.
    let seller_a = login(&app, "01799999999").await;
    let store_a = create_store(&app, &seller_a, "wave24-store-a").await;
    let seller_b = login(&app, "01599999999").await;
    let store_b = create_store(&app, &seller_b, "wave24-store-b").await;

    let p1 = create_categorised_product(
        &app,
        &seller_a,
        &store_a,
        "Wave24 Phone",
        100_000,
        50,
        "electronics",
        "active",
    )
    .await;
    let p2 = create_categorised_product(
        &app,
        &seller_b,
        &store_b,
        "Wave24 Earbuds",
        120_000,
        50,
        "electronics",
        "active",
    )
    .await;
    let p3 = create_categorised_product(
        &app,
        &seller_a,
        &store_a,
        "Wave24 Rice Cooker",
        80_000,
        50,
        "kitchen",
        "active",
    )
    .await;
    let p4 = create_product(&app, &seller_b, &store_b, "Wave24 Notebook", 50_000, 50).await;
    let p5 = create_categorised_product(
        &app,
        &seller_b,
        &store_b,
        "Wave24 Draft Speaker",
        70_000,
        50,
        "electronics",
        "draft",
    )
    .await;

    let buyer = login(&app, "01999999999").await;
    let other_buyer = login(&app, "01699999999").await;

    // ---------------------------------------------------------------
    // 1. Wishlist: auth, idempotent add, reads, visibility, remove.
    // ---------------------------------------------------------------

    // Signed out, adding is a 401 — the wishlist is account data.
    let uri = format!("/v1/wishlist/{p2}");
    let (status, body) = call(&app, Method::POST, &uri, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "wishlist signed out: {body}");

    let (status, body) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "wishlist add: {body}");
    assert_eq!(body["wishlisted"], true);
    assert_eq!(body["product_id"], p2);
    assert_eq!(wishlist_ids(&app, &buyer).await, vec![p2.clone()]);

    // Re-adding is a 200 with the same answer, not an error.
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "wishlist re-add: {body}");
    assert_eq!(body["wishlisted"], true);
    assert_eq!(wishlist_ids(&app, &buyer).await, vec![p2.clone()]);

    // The list returns the full shared product view.
    let (status, list) = call(&app, Method::GET, "/v1/wishlist", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "wishlist list: {list}");
    assert_eq!(item_ids(&list), vec![p2.clone()]);
    assert_eq!(list["items"][0]["title"], "Wave24 Earbuds");
    assert_eq!(list["items"][0]["store_slug"], "wave24-store-b");

    // A draft product is not publicly visible: adding it is a
    // 404, exactly like a product that does not exist.
    let uri = format!("/v1/wishlist/{p5}");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "wishlist add draft: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/wishlist/00000000-0000-0000-0000-000000000000",
        None,
        Some(&buyer),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "wishlist add missing: {body}");

    // Another user's wishlist is empty — lists are per-account.
    assert!(wishlist_ids(&app, &other_buyer).await.is_empty());
    let (status, list) = call(&app, Method::GET, "/v1/wishlist", None, Some(&other_buyer)).await;
    assert_eq!(status, StatusCode::OK, "other wishlist: {list}");
    assert_eq!(item_ids(&list), Vec::<String>::new());

    // Remove answers with the new state; removing again is
    // still a 200 (idempotent).
    let uri = format!("/v1/wishlist/{p2}");
    let (status, body) = call(&app, Method::DELETE, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "wishlist remove: {body}");
    assert_eq!(body["wishlisted"], false);
    assert!(wishlist_ids(&app, &buyer).await.is_empty());
    let (status, body) = call(&app, Method::DELETE, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "wishlist re-remove: {body}");
    assert_eq!(body["wishlisted"], false);

    // ---------------------------------------------------------------
    // 2. Related: the scoring rule's order, drafts excluded.
    // ---------------------------------------------------------------

    let uri = format!("/v1/products/{p1}/related");
    let (status, related) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "related: {related}");
    // Same category (4) beats same store (2) beats unrelated
    // (0); the base product and the draft never appear.
    assert_eq!(item_ids(&related), vec![p2.clone(), p3.clone(), p4.clone()]);

    let uri = format!("/v1/products/{p1}/related?limit=2");
    let (status, related) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "related limit: {related}");
    assert_eq!(item_ids(&related), vec![p2.clone(), p3.clone()]);

    // A draft base product is not publicly visible: 404.
    let uri = format!("/v1/products/{p5}/related");
    let (status, body) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "related of draft: {body}");

    // ---------------------------------------------------------------
    // 3. Also-bought: order-level co-purchase across stores.
    // ---------------------------------------------------------------

    // Buyer one: P1 + P2 in ONE checkout — two stores, two
    // sub-orders, one parent order.
    add_to_cart(&app, &buyer, &p1, 1).await;
    add_to_cart(&app, &buyer, &p2, 1).await;
    let order = checkout_cod(&app, &buyer).await;
    assert_eq!(order["sub_orders"].as_array().map(Vec::len), Some(2));

    // Buyer two: P1 alone — no companion, adds no co-purchase.
    add_to_cart(&app, &other_buyer, &p1, 1).await;
    checkout_cod(&app, &other_buyer).await;

    let uri = format!("/v1/products/{p1}/also-bought");
    let (status, also) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "also-bought: {also}");
    assert_eq!(item_ids(&also), vec![p2.clone()]);
    assert_eq!(also["items"][0]["co_count"], 1);

    // The pairing is symmetric for the cross-store case.
    let uri = format!("/v1/products/{p2}/also-bought");
    let (status, also) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "also-bought reverse: {also}");
    assert_eq!(item_ids(&also), vec![p1.clone()]);
    assert_eq!(also["items"][0]["co_count"], 1);

    // A never-ordered product honestly has no companions.
    let uri = format!("/v1/products/{p4}/also-bought");
    let (status, also) = call(&app, Method::GET, &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "also-bought empty: {also}");
    assert_eq!(item_ids(&also), Vec::<String>::new());

    // ---------------------------------------------------------------
    // 4. Recommendations: delivered signal, wishlist signal, fallback.
    // ---------------------------------------------------------------

    // A second electronics product at store A, created only
    // now so the related assertions above stayed exact.
    let p6 = create_categorised_product(
        &app,
        &seller_a,
        &store_a,
        "Wave24 Charger",
        90_000,
        50,
        "electronics",
        "active",
    )
    .await;

    // A third buyer RECEIVES P1 (delivered): the electronics
    // category becomes their signal.
    let delivered_buyer = login(&app, "01899999999").await;
    add_to_cart(&app, &delivered_buyer, &p1, 1).await;
    let order = checkout_cod(&app, &delivered_buyer).await;
    let sub_order_id = order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string();
    deliver_sub_order(&app, &seller_a, &sub_order_id).await;

    let (status, recs) = call(
        &app,
        Method::GET,
        "/v1/recommendations",
        None,
        Some(&delivered_buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "recommendations: {recs}");
    assert_eq!(recs["personalized"], true);
    let rec_ids = item_ids(&recs);
    // The other electronics are discovered; the bought
    // product, the kitchen product, the uncategorised one
    // and the draft are not recommended.
    assert!(rec_ids.contains(&p2), "recs contain P2: {recs}");
    assert!(rec_ids.contains(&p6), "recs contain P6: {recs}");
    assert!(!rec_ids.contains(&p1), "bought product excluded: {recs}");
    assert!(!rec_ids.contains(&p3), "other category excluded: {recs}");
    assert!(!rec_ids.contains(&p4), "uncategorised excluded: {recs}");
    assert!(!rec_ids.contains(&p5), "draft excluded: {recs}");

    // A wishlist-only signal personalises too: the other
    // buyer saves the kitchen product P3. Their only kitchen
    // candidate is P3 itself — wishlisted, so excluded —
    // and their placed (not delivered) P1 order is no
    // signal; the honest result is personalised and empty.
    let uri = format!("/v1/wishlist/{p3}");
    let (status, body) = call(&app, Method::POST, &uri, None, Some(&other_buyer)).await;
    assert_eq!(status, StatusCode::OK, "wishlist signal add: {body}");
    let (status, recs) = call(
        &app,
        Method::GET,
        "/v1/recommendations",
        None,
        Some(&other_buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "wishlist-signal recs: {recs}");
    assert_eq!(recs["personalized"], true);
    assert_eq!(item_ids(&recs), Vec::<String>::new());

    // A fresh user has no signals: the labelled top-rated
    // fallback, with no drafts in it.
    let fresh_buyer = login(&app, "01399999999").await;
    let (status, recs) = call(
        &app,
        Method::GET,
        "/v1/recommendations",
        None,
        Some(&fresh_buyer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "fallback recs: {recs}");
    assert_eq!(recs["personalized"], false);
    let rec_ids = item_ids(&recs);
    assert!(!rec_ids.is_empty(), "fallback is not empty: {recs}");
    assert!(!rec_ids.contains(&p5), "fallback has no draft: {recs}");
}
