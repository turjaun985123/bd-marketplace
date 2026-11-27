//! Wave 40 integration test: the seller fulfillment center —
//! the work queue's bucket classification across the whole
//! sub-order lifecycle (placed → to_confirm, confirmed →
//! to_ship, booked → in_transit, delivered → done, returned →
//! attention), SLA breaches driven by backdating `created_at`
//! (the one clock `sub_orders` carries) and by the platform
//! setting override, the 7-day Done retention, bulk booking
//! with mixed per-item outcomes through the single-booking
//! path, labels for booked sub-orders only (with named skips),
//! and the Wave 34 staff role working the queue while a
//! stranger is refused with this module family's 403.
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

/// Buy one unit and return the new sub-order's id (left `placed`).
async fn buy_one(app: &Router, buyer: &str, product_id: &str) -> String {
    add_to_cart(app, buyer, product_id, 1).await;
    let order = checkout_cod(app, buyer).await;
    order["sub_orders"][0]["id"]
        .as_str()
        .expect("sub-order id")
        .to_string()
}

/// Buy one unit and confirm it as the seller; returns the sub-order id.
async fn buy_and_confirm(app: &Router, buyer: &str, seller: &str, product_id: &str) -> String {
    let sub_order_id = buy_one(app, buyer, product_id).await;
    let (status, body) = set_sub_order_status(app, seller, &sub_order_id, "confirmed").await;
    assert_eq!(status, StatusCode::OK, "confirm: {body}");
    sub_order_id
}

/// Book the DEV courier for a confirmed sub-order; returns the shipment.
async fn book_dev(app: &Router, seller: &str, sub_order_id: &str) -> Value {
    let uri = format!("/v1/seller/sub-orders/{sub_order_id}/shipment");
    let (status, shipment) = call(app, Method::POST, &uri, Some(json!({})), Some(seller)).await;
    assert_eq!(status, StatusCode::CREATED, "book shipment: {shipment}");
    shipment
}

/// Poll a shipment's courier status once.
async fn refresh(app: &Router, token: &str, shipment_id: &str) {
    let uri = format!("/v1/shipments/{shipment_id}/refresh");
    let (status, body) = call(app, Method::POST, &uri, None, Some(token)).await;
    assert_eq!(status, StatusCode::OK, "refresh: {body}");
}

/// GET the fulfillment queue; returns (status, body).
async fn get_fulfillment(app: &Router, token: &str, store_id: Option<&str>) -> (StatusCode, Value) {
    let uri = match store_id {
        Some(id) => format!("/v1/seller/fulfillment?store_id={id}"),
        None => "/v1/seller/fulfillment".to_string(),
    };
    call(app, Method::GET, &uri, None, Some(token)).await
}

/// Find one queue item by sub-order id.
fn queue_item<'a>(queue: &'a Value, sub_order_id: &str) -> Option<&'a Value> {
    queue["items"]
        .as_array()
        .expect("items array")
        .iter()
        .find(|item| item["sub_order_id"] == sub_order_id)
}

#[tokio::test]
async fn wave40_fulfillment_center_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 40 integration test");
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
         product_variants, products, store_members, stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // SMS dev mode (OTP login) AND courier dev mode (bookings) on.
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave40-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    let app = build_app(state);

    // The cast: seller A with a store + product, seller B with
    // their own, a buyer, and a staff member seated on store A.
    // Every order below is one unit of a 200_000-poisha product
    // inside Dhaka: goods 200_000 + delivery 6_000, so a COD
    // parcel collects exactly 206_000.
    let seller_a = login(&app, "01711111111").await;
    let store_a = create_store(&app, &seller_a, "wave40-store-a").await;
    let product_a = create_product(&app, &seller_a, &store_a, "Wave40 Product A", 200_000, 100).await;
    let seller_b = login(&app, "01644444444").await;
    let store_b = create_store(&app, &seller_b, "wave40-store-b").await;
    let product_b = create_product(&app, &seller_b, &store_b, "Wave40 Product B", 200_000, 100).await;
    let buyer = login(&app, "01555555555").await;
    let staff = login(&app, "01933333333").await;

    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/stores/{store_a}/members"),
        Some(json!({ "phone": "01933333333", "role": "staff" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "seat staff: {body}");

    // ---------------------------------------------------------------
    // 1. One sub-order in each bucket, built through the real
    // flows. The returned one rides a MANUAL courier: the DEV
    // courier only walks forward, so the return path is driven
    // by seller-entered manual statuses.
    // ---------------------------------------------------------------
    let sub_placed = buy_one(&app, &buyer, &product_a).await;
    let sub_toship = buy_and_confirm(&app, &buyer, &seller_a, &product_a).await;

    let sub_transit = buy_and_confirm(&app, &buyer, &seller_a, &product_a).await;
    book_dev(&app, &seller_a, &sub_transit).await;

    let sub_done = buy_and_confirm(&app, &buyer, &seller_a, &product_a).await;
    let done_shipment = book_dev(&app, &seller_a, &sub_done).await;
    let done_shipment_id = done_shipment["id"].as_str().expect("shipment id").to_string();
    for _ in 0..4 {
        refresh(&app, &seller_a, &done_shipment_id).await;
    }

    let sub_attention = buy_and_confirm(&app, &buyer, &seller_a, &product_a).await;
    let (status, manual) = call(
        &app,
        Method::POST,
        &format!("/v1/seller/sub-orders/{sub_attention}/shipment"),
        Some(json!({
            "courier": "manual",
            "manual_courier": "sundarban",
            "manual_tracking_id": "SB-W40-1"
        })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "manual book: {manual}");
    let manual_shipment_id = manual["id"].as_str().expect("shipment id").to_string();
    for target in ["picked_up", "in_transit", "out_for_delivery", "partial_delivered", "returned"] {
        let (status, body) = call(
            &app,
            Method::POST,
            &format!("/v1/seller/shipments/{manual_shipment_id}/manual-status"),
            Some(json!({ "status": target })),
            Some(&seller_a),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "manual status {target}: {body}");
    }

    // Seller B's own confirmed sub-order — never in A's queue.
    let sub_b_toship = buy_and_confirm(&app, &buyer, &seller_b, &product_b).await;

    let (status, queue) = get_fulfillment(&app, &seller_a, None).await;
    assert_eq!(status, StatusCode::OK, "fulfillment: {queue}");
    assert_eq!(queue["store_id"], store_a);
    assert_eq!(queue["sla"]["confirm_hours"], 24);
    assert_eq!(queue["sla"]["ship_hours"], 48);
    assert_eq!(queue["counts"]["to_confirm"], 1, "counts: {queue}");
    assert_eq!(queue["counts"]["to_ship"], 1);
    assert_eq!(queue["counts"]["in_transit"], 1);
    assert_eq!(queue["counts"]["done"], 1);
    assert_eq!(queue["counts"]["attention"], 1);
    assert_eq!(queue["counts"]["breached"], 0);

    let item = queue_item(&queue, &sub_placed).expect("placed item queued");
    assert_eq!(item["bucket"], "to_confirm");
    assert_eq!(item["status"], "placed");
    assert_eq!(item["sla_breached"], false);
    assert_eq!(item["items_count"], 1);
    assert_eq!(item["goods_subtotal_poisha"], 200_000);
    assert_eq!(item["delivery_charge_poisha"], 6_000);
    // Unbooked: the COD figure is the booking computation's preview.
    assert_eq!(item["cod_due_poisha"], 206_000);
    assert!(item["shipment"].is_null());

    let item = queue_item(&queue, &sub_toship).expect("to-ship item queued");
    assert_eq!(item["bucket"], "to_ship");

    let item = queue_item(&queue, &sub_transit).expect("in-transit item queued");
    assert_eq!(item["bucket"], "in_transit");
    assert_eq!(item["shipment"]["status"], "created");
    assert!(
        item["shipment"]["consignment_id"]
            .as_str()
            .expect("consignment id")
            .starts_with("DEV-"),
        "dev consignment: {item}"
    );
    // Booked: the COD figure is the shipment's stored amount.
    assert_eq!(item["cod_due_poisha"], 206_000);

    let item = queue_item(&queue, &sub_done).expect("done item queued");
    assert_eq!(item["bucket"], "done");

    let item = queue_item(&queue, &sub_attention).expect("attention item queued");
    assert_eq!(item["bucket"], "attention");
    assert_eq!(item["status"], "returned");

    // FIFO: the first-created sub-order precedes the last-created.
    let items = queue["items"].as_array().expect("items");
    let pos = |id: &str| {
        items
            .iter()
            .position(|item| item["sub_order_id"] == id)
            .expect("item present")
    };
    assert!(pos(&sub_placed) < pos(&sub_attention), "oldest first: {queue}");

    // Seller B's sub-order is not in A's queue; B's own queue
    // holds exactly it.
    assert!(queue_item(&queue, &sub_b_toship).is_none());
    let (status, queue_b) = get_fulfillment(&app, &seller_b, None).await;
    assert_eq!(status, StatusCode::OK, "fulfillment B: {queue_b}");
    assert_eq!(queue_b["counts"]["to_ship"], 1);
    assert!(queue_item(&queue_b, &sub_b_toship).is_some());
    assert!(queue_item(&queue_b, &sub_placed).is_none());

    // ---------------------------------------------------------------
    // 2. SLA breaches: age comes from `created_at` (the one
    // clock sub_orders carries), so backdating drives it. The
    // confirm SLA (24h) bites before the ship SLA (48h), and a
    // platform setting override re-judges the same rows.
    // ---------------------------------------------------------------
    sqlx::query("UPDATE sub_orders SET created_at = now() - interval '30 hours' WHERE id = $1")
        .bind(&sub_placed)
        .execute(&pool)
        .await
        .expect("backdate placed");
    sqlx::query("UPDATE sub_orders SET created_at = now() - interval '30 hours' WHERE id = $1")
        .bind(&sub_toship)
        .execute(&pool)
        .await
        .expect("backdate to-ship");

    let (status, queue) = get_fulfillment(&app, &seller_a, None).await;
    assert_eq!(status, StatusCode::OK, "fulfillment: {queue}");
    let item = queue_item(&queue, &sub_placed).expect("placed item");
    assert_eq!(item["age_hours"], 30, "age: {item}");
    assert_eq!(item["sla_breached"], true, "30h > 24h confirm SLA: {item}");
    let item = queue_item(&queue, &sub_toship).expect("to-ship item");
    assert_eq!(item["sla_breached"], false, "30h < 48h ship SLA: {item}");
    assert_eq!(queue["counts"]["breached"], 1);

    sqlx::query("UPDATE sub_orders SET created_at = now() - interval '50 hours' WHERE id = $1")
        .bind(&sub_toship)
        .execute(&pool)
        .await
        .expect("backdate to-ship further");
    let (status, queue) = get_fulfillment(&app, &seller_a, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queue["counts"]["breached"], 2, "both breached: {queue}");

    // The admin setting widens the confirm SLA to 100 hours:
    // the 30-hour placed item is judged again and passes.
    sqlx::query(
        "INSERT INTO platform_settings (key, value) VALUES ('fulfillment_confirm_sla_hours', '100')",
    )
    .execute(&pool)
    .await
    .expect("set confirm SLA override");
    let (status, queue) = get_fulfillment(&app, &seller_a, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queue["sla"]["confirm_hours"], 100);
    let item = queue_item(&queue, &sub_placed).expect("placed item");
    assert_eq!(item["sla_breached"], false, "override re-judges: {item}");
    assert_eq!(queue["counts"]["breached"], 1);
    sqlx::query("DELETE FROM platform_settings WHERE key LIKE 'fulfillment_%'")
        .execute(&pool)
        .await
        .expect("clear SLA overrides");

    // ---------------------------------------------------------------
    // 3. Done retention: a delivered sub-order backdated past
    // seven days leaves the queue (and its count).
    // ---------------------------------------------------------------
    sqlx::query("UPDATE sub_orders SET created_at = now() - interval '8 days' WHERE id = $1")
        .bind(&sub_done)
        .execute(&pool)
        .await
        .expect("backdate done");
    let (status, queue) = get_fulfillment(&app, &seller_a, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(queue_item(&queue, &sub_done).is_none(), "old done leaves: {queue}");
    assert_eq!(queue["counts"]["done"], 0);

    // ---------------------------------------------------------------
    // 4. Bulk booking: a mixed batch reports per-item truth —
    // two book, the still-placed one fails with the booking
    // path's own conflict, and nothing rolls back.
    // ---------------------------------------------------------------
    let sub_bulk = buy_and_confirm(&app, &buyer, &seller_a, &product_a).await;
    let (status, bulk) = call(
        &app,
        Method::POST,
        "/v1/seller/fulfillment/bulk-book",
        Some(json!({
            "sub_order_ids": [sub_bulk, sub_toship, sub_placed],
            "courier": "auto"
        })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bulk book: {bulk}");
    assert_eq!(bulk["booked"], 2, "bulk: {bulk}");
    assert_eq!(bulk["failed"], 1);
    let results = bulk["results"].as_array().expect("results");
    let result_for = |id: &str| {
        results
            .iter()
            .find(|r| r["sub_order_id"] == id)
            .expect("result present")
    };
    assert_eq!(result_for(&sub_bulk)["ok"], true);
    assert!(
        result_for(&sub_bulk)["shipment"]["consignment_id"]
            .as_str()
            .expect("consignment")
            .starts_with("DEV-"),
        "bulk shipment: {bulk}"
    );
    assert_eq!(result_for(&sub_toship)["ok"], true);
    let placed_result = result_for(&sub_placed);
    assert_eq!(placed_result["ok"], false);
    assert!(
        placed_result["error"]
            .as_str()
            .expect("error message")
            .contains("confirmed"),
        "the booking path's own guard speaks: {bulk}"
    );

    // Another store's id in A's batch: a per-item error, not a
    // batch failure and never a booking.
    let (status, bulk) = call(
        &app,
        Method::POST,
        "/v1/seller/fulfillment/bulk-book",
        Some(json!({ "sub_order_ids": [sub_b_toship], "courier": "auto" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "foreign id batch: {bulk}");
    assert_eq!(bulk["booked"], 0);
    assert_eq!(bulk["failed"], 1);
    assert!(
        bulk["results"][0]["error"]
            .as_str()
            .expect("error")
            .contains("different store"),
        "foreign id error: {bulk}"
    );

    // But naming a store the caller has no relationship with
    // IS a whole-request scope failure (403).
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/fulfillment/bulk-book",
        Some(json!({
            "store_id": store_b,
            "sub_order_ids": [sub_b_toship],
            "courier": "auto"
        })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "foreign store scope: {body}");

    // Request-shape errors fail before anything books: an
    // oversized batch, an empty batch, a bad manual courier.
    let too_many: Vec<String> = (0..21)
        .map(|_| uuid::Uuid::new_v4().to_string())
        .collect();
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/fulfillment/bulk-book",
        Some(json!({ "sub_order_ids": too_many, "courier": "auto" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "21 ids: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/fulfillment/bulk-book",
        Some(json!({ "sub_order_ids": [], "courier": "auto" })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty batch: {body}");
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/fulfillment/bulk-book",
        Some(json!({
            "sub_order_ids": [sub_placed],
            "courier": "manual",
            "manual_courier": "bogus"
        })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bad manual courier: {body}");

    // A manual bulk batch books through the same dispatch and
    // stores the receipt number as the consignment id.
    let sub_manual_bulk = buy_and_confirm(&app, &buyer, &seller_a, &product_a).await;
    let (status, bulk) = call(
        &app,
        Method::POST,
        "/v1/seller/fulfillment/bulk-book",
        Some(json!({
            "sub_order_ids": [sub_manual_bulk],
            "courier": "manual",
            "manual_courier": "karatoa",
            "manual_tracking_id": "KT-W40-9"
        })),
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "manual bulk: {bulk}");
    assert_eq!(bulk["booked"], 1);
    assert_eq!(bulk["results"][0]["shipment"]["courier"], "manual");
    assert_eq!(bulk["results"][0]["shipment"]["consignment_id"], "KT-W40-9");

    // ---------------------------------------------------------------
    // 5. Labels: booked sub-orders only; the unbooked and the
    // foreign id are named in `skipped` with their reasons.
    // ---------------------------------------------------------------
    let sub_unbooked = buy_one(&app, &buyer, &product_a).await;
    let ids = format!("{sub_transit},{sub_unbooked},{sub_b_toship}");
    let (status, labels) = call(
        &app,
        Method::GET,
        &format!("/v1/seller/fulfillment/labels?ids={ids}"),
        None,
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "labels: {labels}");
    let label_rows = labels["labels"].as_array().expect("labels array");
    assert_eq!(label_rows.len(), 1, "one booked label: {labels}");
    let label = &label_rows[0];
    assert_eq!(label["sub_order_id"], sub_transit);
    assert_eq!(label["store_name"], "Store wave40-store-a");
    assert_eq!(label["seller_phone"], "+8801711111111");
    assert_eq!(label["delivery_name"], "Test Buyer");
    // Checkout stores the delivery phone in canonical +880 form.
    assert_eq!(label["delivery_phone"], "+8801999999999");
    assert_eq!(label["cod_amount_poisha"], 206_000);
    assert!(
        label["consignment_id"]
            .as_str()
            .expect("consignment")
            .starts_with("DEV-"),
        "label consignment: {label}"
    );
    let skipped = labels["skipped"].as_array().expect("skipped array");
    assert_eq!(skipped.len(), 2, "two skips: {labels}");
    let skip_for = |id: &str| {
        skipped
            .iter()
            .find(|s| s["sub_order_id"] == id)
            .expect("skip present")
    };
    assert!(
        skip_for(&sub_unbooked)["reason"]
            .as_str()
            .expect("reason")
            .contains("no shipment booked"),
        "unbooked skip: {labels}"
    );
    assert!(
        skip_for(&sub_b_toship)["reason"]
            .as_str()
            .expect("reason")
            .contains("different store"),
        "foreign skip: {labels}"
    );

    // ---------------------------------------------------------------
    // 6. Roles: staff (manage_orders) reads the queue and
    // bulk-books; a user with no relationship gets the module
    // family's 403 on a named store and a 404 with no store.
    // ---------------------------------------------------------------
    let (status, queue) = get_fulfillment(&app, &staff, Some(&store_a)).await;
    assert_eq!(status, StatusCode::OK, "staff reads queue: {queue}");
    assert!(queue["counts"]["to_confirm"].as_i64().expect("count") >= 1);

    let sub_staff_bulk = buy_and_confirm(&app, &buyer, &seller_a, &product_a).await;
    let (status, bulk) = call(
        &app,
        Method::POST,
        "/v1/seller/fulfillment/bulk-book",
        Some(json!({
            "store_id": store_a,
            "sub_order_ids": [sub_staff_bulk],
            "courier": "auto"
        })),
        Some(&staff),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "staff bulk book: {bulk}");
    assert_eq!(bulk["booked"], 1);

    let (status, body) = get_fulfillment(&app, &buyer, Some(&store_a)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "stranger on named store: {body}");
    let (status, body) = get_fulfillment(&app, &buyer, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no own store: {body}");

    // ---------------------------------------------------------------
    // 7. The dashboard carries the same tallies: work waiting
    // (the two placed sub-orders), the returned one in
    // attention, and the backdated placed one still breached
    // after the setting override was deleted.
    // ---------------------------------------------------------------
    let (status, dashboard) = call(
        &app,
        Method::GET,
        "/v1/seller/dashboard",
        None,
        Some(&seller_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "dashboard: {dashboard}");
    let fulfillment = &dashboard["fulfillment"];
    assert!(
        fulfillment["to_confirm"].as_i64().expect("to_confirm") >= 1,
        "dashboard fulfillment: {dashboard}"
    );
    assert_eq!(fulfillment["attention"], 1);
    assert!(
        fulfillment["breached"].as_i64().expect("breached") >= 1,
        "dashboard breached: {dashboard}"
    );
}
