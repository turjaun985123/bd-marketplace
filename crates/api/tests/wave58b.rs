//! Wave 58 integration test: the PUT round-trip — a
//! custom layout saves, reads back through the seller
//! view, and resolves publicly:
//! - a manual rail renders its products in the SELLER'S
//!   saved order (not creation order);
//! - a rail product under a live flash sale carries the
//!   flash price (the shared attach, like every product
//!   surface) — the sale is a REAL admin flash sale;
//! - the hero renders its own headline/link instead of
//!   the branding fallbacks;
//! - an FAQ section resolves its pairs;
//! - the stored row exists and the seller view reports
//!   `is_custom: true` with the write's timestamp.
//!
//! Gated on `DATABASE_URL`: <redacted>
//! plain local `cargo test` (no database) still passes. CI provides
//! a Postgres 16 service and sets `DATABASE_URL` for the rust job.

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

/// Build the test app (dev couriers on), or None when no
/// database is configured. Truncates every table EXCEPT
/// `categories` (shared seed — see wave56.rs).
async fn test_app() -> Option<(Router, PgPool)> {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 58 integration tests");
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
         store_collections, store_collection_products, \
         product_images, product_variants, products, store_members, store_follows, \
         stores, otp_codes, users, home_banners, collections, collection_products, \
         flash_sales, flash_sale_items, merch_events, saved_searches, \
         search_queries CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");
    let dev_couriers = CouriersConfig {
        dev_mode: true,
        ..CouriersConfig::default()
    };
    let state = AppState::new(Some(pool.clone()), "wave58-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
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

/// POST one active product; returns its id.
async fn create_product(
    app: &Router,
    token: &str,
    store_id: &str,
    title: &str,
    price_poisha: i64,
) -> String {
    let (status, created) = call(
        app,
        Method::POST,
        "/v1/products",
        Some(json!({
            "store_id": store_id,
            "title": title,
            "price_poisha": price_poisha,
            "stock": 10,
            "status": "active"
        })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create product {title}: {created}");
    created["id"].as_str().expect("product id").to_string()
}

/// Find one section in a layout body by id.
fn section_by_id<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["sections"]
        .as_array()?
        .iter()
        .find(|section| section["id"].as_str() == Some(id))
}

/// Format unix seconds as an RFC 3339 UTC timestamp (the
/// civil-from-days conversion), so the test can schedule
/// the flash window relative to the database clock
/// without a date library. (Verbatim from wave52.rs.)
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

/// The current unix time, read from the database itself
/// (the clock every stored window is compared against).
async fn db_now_unix(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM now())::bigint")
        .fetch_one(pool)
        .await
        .expect("read database clock")
}

#[tokio::test]
async fn wave58_put_round_trip_with_manual_rail_and_flash_price() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store = create_store(&app, &seller, "w58-custom").await;
    let store_id = store["id"].as_str().expect("store id").to_string();

    let p1 = create_product(&app, &seller, &store_id, "Flash Target", 200_000).await;
    let p2 = create_product(&app, &seller, &store_id, "Second Pick", 150_000).await;
    let p3 = create_product(&app, &seller, &store_id, "First Pick", 100_000).await;

    // A REAL flash sale over p1, run by a promoted admin
    // (login first — the promotion updates the user row
    // the login created).
    let admin = login(&app, "01799999999").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801799999999'")
        .execute(&pool)
        .await
        .expect("promote admin");
    let t0 = db_now_unix(&pool).await;
    let (status, sale) = call(
        &app,
        Method::POST,
        "/v1/admin/flash-sales",
        Some(json!({
            "title": "Wave 58 Flash",
            "starts_at": rfc3339_from_unix(t0 - 3_600),
            "ends_at": rfc3339_from_unix(t0 + 7_200)
        })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create flash sale: {sale}");
    let sale_id = sale["id"].as_str().expect("sale id").to_string();
    let (status, body) = call(
        &app,
        Method::PUT,
        &format!("/v1/admin/flash-sales/{sale_id}/items"),
        Some(json!({ "items": [{ "product_id": p1, "sale_price_poisha": 120_000 }] })),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set flash items: {body}");

    // The custom layout: hero with its own headline and
    // link, a manual rail in REVERSE creation order, and
    // an FAQ.
    let (status, saved) = call(
        &app,
        Method::PUT,
        "/v1/seller/store/layout",
        Some(json!({
            "version": 1,
            "sections": [
                {
                    "id": "hero",
                    "type": "hero",
                    "config": {
                        "headline": "আমার নিজের হিরো",
                        "link_url": "/w58-custom/shop",
                        "link_label": "কিনুন"
                    }
                },
                {
                    "id": "my-picks",
                    "type": "product_rail",
                    "config": {
                        "source": "manual",
                        "title": "আমার পছন্দ",
                        "product_ids": [p3, p1, p2]
                    }
                },
                {
                    "id": "faq",
                    "type": "faq",
                    "config": {
                        "items": [
                            { "question": "ডেলিভারি কতদিনে?", "answer": "ঢাকায় ১–২ দিন।" }
                        ]
                    }
                }
            ]
        })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "put layout: {saved}");
    assert_eq!(saved["is_custom"], json!(true));
    assert!(saved["updated_at"].is_string(), "write timestamp: {saved}");
    // The rail's limit was defaulted by normalisation.
    let rail = section_by_id(&saved, "my-picks").expect("rail in saved view");
    assert_eq!(rail["config"]["limit"], json!(10));
    assert_eq!(rail["config"]["source"], json!("manual"));

    // The row is really stored.
    let stored_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_layouts WHERE store_id = $1::uuid",
    )
    .bind(&store_id)
    .fetch_one(&pool)
    .await
    .expect("count stored layouts");
    assert_eq!(stored_rows, 1);

    // The seller read agrees with the write response.
    let (status, fetched) = call(
        &app,
        Method::GET,
        "/v1/seller/store/layout",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "get layout: {fetched}");
    assert_eq!(fetched["is_custom"], json!(true));
    let rail = section_by_id(&fetched, "my-picks").expect("rail in fetched view");
    assert_eq!(
        rail["config"]["product_ids"],
        json!([p3, p1, p2]),
        "the seller's order is stored verbatim"
    );

    // The public read resolves: hero copy is the
    // section's own, the manual rail is in the seller's
    // order, and p1 carries its live flash price.
    let (status, public) = call(
        &app,
        Method::GET,
        "/v1/stores/w58-custom/layout",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public layout: {public}");
    assert_eq!(public["is_custom"], json!(true));
    let hero = section_by_id(&public, "hero").expect("hero section");
    assert_eq!(hero["hero"]["headline"], json!("আমার নিজের হিরো"));
    assert_eq!(hero["hero"]["link_url"], json!("/w58-custom/shop"));
    assert_eq!(hero["hero"]["link_label"], json!("কিনুন"));
    let rail = section_by_id(&public, "my-picks").expect("rail section");
    let items = rail["products"].as_array().expect("rail products");
    let ids: Vec<&str> = items
        .iter()
        .map(|item| item["id"].as_str().expect("product id"))
        .collect();
    assert_eq!(ids, vec![p3.as_str(), p1.as_str(), p2.as_str()], "seller order");
    let flash_item = &items[1];
    assert_eq!(flash_item["price_poisha"], json!(200_000));
    assert_eq!(
        flash_item["flash_price_poisha"],
        json!(120_000),
        "the live flash price rides the card: {flash_item}"
    );
    let faq = section_by_id(&public, "faq").expect("faq section");
    assert_eq!(faq["faq_items"][0]["question"], json!("ডেলিভারি কতদিনে?"));
    assert_eq!(faq["faq_items"][0]["answer"], json!("ঢাকায় ১–২ দিন।"));
}
