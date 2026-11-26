//! Wave 33 integration test: product variants — seller CRUD
//! with the final-price positivity rule (create/reprice
//! refusals, stranger 404s), the public detail's active
//! variant list with final prices (inactive hidden) and the
//! product view's stock becoming the SUM of active variant
//! stocks, cart rules (variant required, variant must be an
//! active variant of the product, lines priced at the final
//! price), checkout drawing stock from the VARIANT row (the
//! product column untouched) with variant-named order items
//! and variant-labelled audit rows, insufficient variant
//! stock as a 409, buyer-cancel restocking the variant,
//! per-variant adjust-stock (audited, variant id on the row),
//! the product-level adjust-stock refusal while variants are
//! active, and the variant-less two-store checkout keeping
//! the exact Wave 9 money (goods 445_000 + delivery 12_000 =
//! 457_000). The small helpers are copied from the Wave 9
//! test on purpose, since integration test files stay
//! self-contained.
//!
//! Gated on `DATABASE_URL`: without it the test prints a note and
//! passes, so a plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

use api::{build_app, run_migrations, AppState};
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
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


/// POST one variant on a product; returns (status, body).
async fn create_variant(
    app: &Router,
    token: &str,
    product_id: &str,
    body: Value,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/products/{product_id}/variants");
    call(app, Method::POST, &uri, Some(body), Some(token)).await
}

/// GET the seller variant list of a product.
async fn list_variants(app: &Router, token: &str, product_id: &str) -> Value {
    let uri = format!("/v1/seller/products/{product_id}/variants");
    let (status, body) = call(app, Method::GET, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "list variants: {body}");
    body
}

/// Find one variant in a seller variant list by name.
fn variant_by_name<'a>(list: &'a Value, name: &str) -> &'a Value {
    list.as_array()
        .expect("variant list")
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("variant '{name}' in list: {list}"))
}

/// POST a cart add carrying an explicit variant id (or null).
async fn add_to_cart_variant(
    app: &Router,
    token: &str,
    product_id: &str,
    quantity: i64,
    variant_id: Option<&str>,
) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/cart/items",
        Some(json!({
            "product_id": product_id,
            "quantity": quantity,
            "variant_id": variant_id
        })),
        Some(token),
    )
    .await
}

/// POST a checkout without asserting success (for the 409 case).
async fn checkout_raw(app: &Router, token: &str) -> (StatusCode, Value) {
    call(
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
    .await
}

/// GET one product as the given token sees it (detail shape).
async fn get_product_detail(app: &Router, token: Option<&str>, product_id: &str) -> Value {
    let uri = format!("/v1/products/{product_id}");
    let (status, body) = call(app, Method::GET, &uri, None, token).await;
    assert_eq!(status, StatusCode::OK, "product detail: {body}");
    body
}

/// GET a product's full stock history as its seller.
async fn get_history(app: &Router, token: &str, product_id: &str) -> Value {
    let uri = format!("/v1/seller/products/{product_id}/stock-history?per_page=50");
    let (status, body) = call(app, Method::GET, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "stock history: {body}");
    body
}

#[tokio::test]
async fn wave33_product_variants_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 33 integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, \
         product_variants, products, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(Some(pool.clone()), "wave33-test-secret".to_string(), true);
    let app = build_app(state);

    let seller = login(&app, "01799999999").await;
    let store = create_store(&app, &seller, "wave33-store").await;
    let stranger = login(&app, "01599999999").await;
    let other_store = create_store(&app, &stranger, "wave33-other-store").await;
    let buyer = login(&app, "01999999999").await;

    // The variant product: base price 100_000 (1000 taka). Its
    // product-level stock (7) becomes a display fallback the
    // moment variants exist — checkout must never touch it.
    let product = create_product(&app, &seller, &store, "Wave33 Shirt", 100_000, 7).await;

    // 1. Variant creation validation: the final price must
    // stay positive, the name must be a real label, the delta
    // must sit inside the sanity bound, and a stranger's
    // product is a 404.
    let (status, body) = create_variant(
        &app,
        &seller,
        &product,
        json!({ "name": "Free", "price_delta_poisha": -100_000, "stock": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "zero final price: {body}");
    let (status, body) = create_variant(
        &app,
        &seller,
        &product,
        json!({ "name": "Negative", "price_delta_poisha": -150_000, "stock": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "negative final price: {body}");
    let (status, body) = create_variant(
        &app,
        &seller,
        &product,
        json!({ "name": "   ", "price_delta_poisha": 0, "stock": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "blank name: {body}");
    let (status, body) = create_variant(
        &app,
        &seller,
        &product,
        json!({ "name": "Huge delta", "price_delta_poisha": 100_000_001, "stock": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "delta bound: {body}");
    let (status, body) = create_variant(
        &app,
        &stranger,
        &product,
        json!({ "name": "Not yours", "price_delta_poisha": 0, "stock": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger create: {body}");

    // 2. Create S (same price) and L (+200 taka), then reprice
    // L by PATCH to +250 taka under a new name — the merged
    // delta is what the final price follows.
    let (status, small) = create_variant(
        &app,
        &seller,
        &product,
        json!({ "name": "S", "price_delta_poisha": 0, "stock": 5, "sort": 0 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create S: {small}");
    assert_eq!(small["final_price_poisha"], 100_000);
    let small_id = small["id"].as_str().expect("S id").to_string();

    let (status, large) = create_variant(
        &app,
        &seller,
        &product,
        json!({ "name": "L", "price_delta_poisha": 20_000, "stock": 3, "sort": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create L: {large}");
    let large_id = large["id"].as_str().expect("L id").to_string();

    let uri = format!("/v1/seller/products/{product}/variants/{large_id}");
    let (status, patched) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "name": "Large", "price_delta_poisha": 25_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch L: {patched}");
    assert_eq!(patched["name"], "Large");
    assert_eq!(patched["final_price_poisha"], 125_000);

    // A reprice that would zero the final price is refused too.
    let (status, body) = call(
        &app,
        Method::PATCH,
        &uri,
        Some(json!({ "price_delta_poisha": -100_000 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "patch to zero price: {body}");

    // 3. A third variant, deactivated via DELETE: gone from
    // the public detail, still listed for the seller as
    // inactive (soft delete — the row stays).
    let (status, old) = create_variant(
        &app,
        &seller,
        &product,
        json!({ "name": "Old", "price_delta_poisha": 0, "stock": 9, "sort": 2 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create Old: {old}");
    let old_id = old["id"].as_str().expect("Old id").to_string();
    let uri = format!("/v1/seller/products/{product}/variants/{old_id}");
    let (status, deactivated) = call(&app, Method::DELETE, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "deactivate Old: {deactivated}");
    assert_eq!(deactivated["is_active"], false);

    let seller_list = list_variants(&app, &seller, &product).await;
    assert_eq!(seller_list.as_array().map(Vec::len), Some(3));
    assert_eq!(variant_by_name(&seller_list, "Old")["is_active"], false);

    // 4. The public detail: only the ACTIVE variants, in
    // picker order, at final prices; the product stock is the
    // SUM of active variant stocks (5 + 3 = 8, not the stored
    // 7 and not counting the deactivated 9). The list view
    // carries has_variants and no variant rows.
    let detail = get_product_detail(&app, None, &product).await;
    assert_eq!(detail["has_variants"], true, "detail: {detail}");
    assert_eq!(detail["stock"], 8, "detail stock is the variant sum: {detail}");
    let variants = detail["variants"].as_array().expect("detail variants");
    assert_eq!(variants.len(), 2, "detail variants: {detail}");
    assert_eq!(variants[0]["name"], "S");
    assert_eq!(variants[0]["final_price_poisha"], 100_000);
    assert_eq!(variants[0]["stock"], 5);
    assert_eq!(variants[1]["name"], "Large");
    assert_eq!(variants[1]["final_price_poisha"], 125_000);
    assert_eq!(variants[1]["stock"], 3);

    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/products?store_slug=wave33-store",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "product list: {list}");
    let listed = &list["items"][0];
    assert_eq!(listed["has_variants"], true, "list item: {list}");
    assert_eq!(listed["stock"], 8, "list stock is the variant sum: {list}");
    assert!(
        listed.get("variants").is_none(),
        "list items never embed variants: {list}"
    );

    // 5. Cart rules: no variant chosen -> 400 asking for one;
    // an unknown variant id -> 400; the deactivated variant
    // -> 400. Then the real adds: S x2 and Large x1 as two
    // lines priced at their final prices.
    let (status, body) = add_to_cart_variant(&app, &buyer, &product, 1, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "add without variant: {body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("বেছে নিন"),
        "the 400 asks for a variant choice: {body}"
    );
    let (status, body) =
        add_to_cart_variant(&app, &buyer, &product, 1, Some(&Uuid::new_v4().to_string())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "add unknown variant: {body}");
    let (status, body) = add_to_cart_variant(&app, &buyer, &product, 1, Some(&old_id)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "add inactive variant: {body}");

    let (status, cart) = add_to_cart_variant(&app, &buyer, &product, 2, Some(&small_id)).await;
    assert_eq!(status, StatusCode::CREATED, "add S: {cart}");
    let (status, cart) = add_to_cart_variant(&app, &buyer, &product, 1, Some(&large_id)).await;
    assert_eq!(status, StatusCode::CREATED, "add Large: {cart}");
    let lines = cart["items"].as_array().expect("cart lines");
    assert_eq!(lines.len(), 2, "two variant lines: {cart}");
    let s_line = lines
        .iter()
        .find(|line| line["variant_id"] == small_id)
        .expect("S line");
    assert_eq!(s_line["variant_name"], "S");
    assert_eq!(s_line["unit_price_poisha"], 100_000);
    assert_eq!(s_line["stock"], 5);
    let l_line = lines
        .iter()
        .find(|line| line["variant_id"] == large_id)
        .expect("Large line");
    assert_eq!(l_line["variant_name"], "Large");
    assert_eq!(l_line["unit_price_poisha"], 125_000);
    assert_eq!(cart["goods_total_poisha"], 325_000, "cart goods: {cart}");

    // 6. Checkout: goods 325_000; three units at the default
    // 1 kg each bill 3 kg inside Dhaka (60 + 2 x 20 = 100
    // taka = 10_000); grand 335_000. The VARIANT stocks move
    // (S 5 -> 3, Large 3 -> 2); the product column stays 7;
    // the order item snapshots the variant name and price.
    let order = checkout_cod(&app, &buyer).await;
    assert_eq!(order["goods_total_poisha"], 325_000, "order goods: {order}");
    assert_eq!(order["delivery_total_poisha"], 10_000, "order delivery: {order}");
    assert_eq!(order["grand_total_poisha"], 335_000, "order grand: {order}");
    let items = order["sub_orders"][0]["items"].as_array().expect("order items");
    let s_item = items
        .iter()
        .find(|item| item["variant_id"] == small_id)
        .expect("S order item");
    assert_eq!(s_item["variant_name"], "S");
    assert_eq!(s_item["unit_price_poisha"], 100_000);
    let l_item = items
        .iter()
        .find(|item| item["variant_id"] == large_id)
        .expect("Large order item");
    assert_eq!(l_item["variant_name"], "Large");
    assert_eq!(l_item["unit_price_poisha"], 125_000);

    let seller_list = list_variants(&app, &seller, &product).await;
    assert_eq!(variant_by_name(&seller_list, "S")["stock"], 3);
    assert_eq!(variant_by_name(&seller_list, "Large")["stock"], 2);
    let stored_stock: i32 = sqlx::query_scalar("SELECT stock FROM products WHERE id = $1")
        .bind(Uuid::parse_str(&product).expect("product uuid"))
        .fetch_one(&pool)
        .await
        .expect("read stored product stock");
    assert_eq!(stored_stock, 7, "checkout never touches the product column");
    let detail = get_product_detail(&app, None, &product).await;
    assert_eq!(detail["stock"], 5, "view stock after checkout: {detail}");

    // The audit trail: two 'order' rows carrying variant ids,
    // with the VARIANTS' post-move counts as their snapshots.
    let history = get_history(&app, &seller, &product).await;
    let order_rows: Vec<&Value> = history["items"]
        .as_array()
        .expect("history items")
        .iter()
        .filter(|row| row["reason"] == "order")
        .collect();
    assert_eq!(order_rows.len(), 2, "order audit rows: {history}");
    let s_row = order_rows
        .iter()
        .find(|row| row["variant_id"] == small_id)
        .expect("S audit row");
    assert_eq!(s_row["delta"], -2);
    assert_eq!(s_row["new_stock"], 3);
    let l_row = order_rows
        .iter()
        .find(|row| row["variant_id"] == large_id)
        .expect("Large audit row");
    assert_eq!(l_row["delta"], -1);
    assert_eq!(l_row["new_stock"], 2);

    // 7. Insufficient variant stock: 4 x Large against a stock
    // of 2 fails the whole checkout with a 409 naming the
    // product and the variant; the cart survives (clear it
    // by hand for the next step).
    let (status, _) = add_to_cart_variant(&app, &buyer, &product, 4, Some(&large_id)).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = checkout_raw(&app, &buyer).await;
    assert_eq!(status, StatusCode::CONFLICT, "oversell checkout: {body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("Wave33 Shirt (Large)"),
        "the 409 names product + variant: {body}"
    );
    let (status, _) = call(&app, Method::DELETE, "/v1/cart", None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK);

    // 8. Buyer cancel restocks THE VARIANT: buy S x1 (3 -> 2),
    // cancel the order, and S is back to 3 with a
    // cancel_restock audit row carrying the variant id.
    let (status, _) = add_to_cart_variant(&app, &buyer, &product, 1, Some(&small_id)).await;
    assert_eq!(status, StatusCode::CREATED);
    let order2 = checkout_cod(&app, &buyer).await;
    let order2_id = order2["id"].as_str().expect("order2 id").to_string();
    let seller_list = list_variants(&app, &seller, &product).await;
    assert_eq!(variant_by_name(&seller_list, "S")["stock"], 2);
    let uri = format!("/v1/orders/{order2_id}/cancel");
    let (status, cancelled) = call(&app, Method::POST, &uri, None, Some(&buyer)).await;
    assert_eq!(status, StatusCode::OK, "buyer cancel: {cancelled}");
    let seller_list = list_variants(&app, &seller, &product).await;
    assert_eq!(variant_by_name(&seller_list, "S")["stock"], 3);
    let history = get_history(&app, &seller, &product).await;
    let restock_row = history["items"]
        .as_array()
        .expect("history items")
        .iter()
        .find(|row| row["reason"] == "cancel_restock")
        .expect("cancel restock row");
    assert_eq!(restock_row["variant_id"], small_id);
    assert_eq!(restock_row["delta"], 1);
    assert_eq!(restock_row["new_stock"], 3);

    // 9. Per-variant adjust-stock: +4 with a seller reason
    // writes an audit row carrying the variant id; a stranger
    // gets a 404; below zero is a 400. And the product-level
    // adjust-stock now refuses — stock lives on the variants.
    let uri = format!("/v1/seller/products/{product}/variants/{small_id}/adjust-stock");
    let (status, adjusted) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "delta": 4, "reason": "restock" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "variant adjust-stock: {adjusted}");
    assert_eq!(adjusted["variant"]["stock"], 7);
    assert_eq!(adjusted["adjustment"]["variant_id"], small_id);
    assert_eq!(adjusted["adjustment"]["new_stock"], 7);
    assert_eq!(adjusted["adjustment"]["actor"], "seller");

    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "delta": 1, "reason": "restock" })),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger variant adjust: {body}");

    let (status, body) = call(
        &app,
        Method::POST,
        &uri,
        Some(json!({ "delta": -100, "reason": "damage" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "variant adjust below zero: {body}");

    let (status, body) = adjust_stock_guard(&app, &seller, &product).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "product adjust refused: {body}");

    // 10. The variant-less regression: the exact Wave 9
    // two-store cart (245_000 + 200_000 goods, two 1 kg
    // parcels inside Dhaka at 6_000 each) still totals
    // 445_000 + 12_000 = 457_000, and product stocks move.
    let plain_a = create_product(&app, &seller, &store, "Wave33 Plain A", 200_000, 10).await;
    let plain_b = create_product(&app, &stranger, &other_store, "Wave33 Plain B", 245_000, 10).await;
    add_to_cart(&app, &buyer, &plain_a, 1).await;
    add_to_cart(&app, &buyer, &plain_b, 1).await;
    let order3 = checkout_cod(&app, &buyer).await;
    assert_eq!(order3["goods_total_poisha"], 445_000, "order3 goods: {order3}");
    assert_eq!(order3["delivery_total_poisha"], 12_000, "order3 delivery: {order3}");
    assert_eq!(order3["grand_total_poisha"], 457_000, "order3 grand: {order3}");
    let detail_a = get_product_detail(&app, None, &plain_a).await;
    assert_eq!(detail_a["stock"], 9, "plain product stock moved: {detail_a}");
    assert_eq!(detail_a["has_variants"], false);
}

/// Product-level adjust-stock call (the Wave 23 endpoint),
/// used here to prove it refuses while variants are active.
async fn adjust_stock_guard(
    app: &Router,
    token: &str,
    product_id: &str,
) -> (StatusCode, Value) {
    let uri = format!("/v1/seller/products/{product_id}/adjust-stock");
    call(
        app,
        Method::POST,
        &uri,
        Some(json!({ "delta": 1, "reason": "restock" })),
        Some(token),
    )
    .await
}
