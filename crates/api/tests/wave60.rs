//! Wave 60 integration test: gift-card issuance — the full
//! code is shown exactly once in the issue response, lists
//! and detail views are masked (prefix + last4, never the
//! code, never the hash), the public balance check answers
//! a holder and refuses unknown codes with ONE uniform 404,
//! issue validation enforces the documented caps, bulk
//! issue mints distinct codes, and the ledger sums to the
//! cached balance.
//!
//! Gated on `DATABASE_URL`: skips cleanly so a plain local
//! `cargo test` (no database) still passes. CI provides a
//! Postgres 16 service and sets `DATABASE_URL`.

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
            eprintln!("DATABASE_URL not set — skipping Wave 60 integration tests");
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
         store_pages, store_posts, store_post_events, \
         store_collections, store_collection_products, \
         product_images, product_variants, products, store_members, store_follows, \
         gift_card_ledger, gift_cards, \
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
    let state = AppState::new(Some(pool.clone()), "wave60-test-secret".to_string(), true)
        .with_couriers_config(dev_couriers);
    Some((build_app(state), pool))
}

/// POST a store for the token's user; returns the store id.
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

/// Issue one card for the caller's store; returns (view, code).
async fn issue_card(app: &Router, token: &str, amount_poisha: i64) -> (Value, String) {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": amount_poisha })),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "issue card: {body}");
    let card = body["cards"][0].clone();
    let code = card["code"]
        .as_str()
        .expect("full code shown once")
        .to_string();
    (card, code)
}

/// The public balance check for a code.
async fn balance_check(app: &Router, code: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        "/v1/gift-cards/balance",
        Some(json!({ "code": code })),
        None,
    )
    .await
}

#[tokio::test]
async fn wave60_issue_reveal_masking_and_balance_check() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01711111111").await;
    let store_id = create_store(&app, &seller, "w60-cards").await;

    // 1. Issue: 201, the full code exactly here, shaped like
    // a gift-card code; the view half is already masked.
    let (card, code) = issue_card(&app, &seller, 50_000).await;
    assert!(code.starts_with("bdgc_"), "code prefix: {code}");
    assert_eq!(code.len(), 29, "code length: {code}");
    assert_eq!(card["balance_poisha"], 50_000);
    assert_eq!(card["initial_amount_poisha"], 50_000);
    assert_eq!(card["redeemed_net_poisha"], 0);
    assert_eq!(card["status"], "active");
    assert_eq!(card["store_id"], store_id);
    assert_eq!(card["store_slug"], "w60-cards");
    assert_eq!(card["code_prefix"], &code[..12]);
    assert_eq!(card["last4"], &code[code.len() - 4..]);
    assert!(card["expires_at"].is_string(), "default validity sets an expiry");
    let card_id = card["id"].as_str().expect("card id").to_string();

    // 2. The list is masked: no `code`, no hash anywhere.
    let (status, list) = call(
        &app,
        Method::GET,
        "/v1/seller/gift-cards",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "list: {list}");
    assert_eq!(list["total"], 1);
    let listed = &list["items"][0];
    assert_eq!(listed["id"], card_id);
    assert!(listed.get("code").is_none(), "list never carries the code");
    assert!(listed.get("code_hash").is_none(), "list never carries the hash");
    assert_eq!(listed["code_prefix"], &code[..12]);
    assert_eq!(listed["last4"], &code[code.len() - 4..]);

    // 3. Detail carries the ledger: exactly the issue row,
    // and the ledger sums to the cached balance.
    let uri = format!("/v1/seller/gift-cards/{card_id}");
    let (status, detail) = call(&app, Method::GET, &uri, None, Some(&seller)).await;
    assert_eq!(status, StatusCode::OK, "detail: {detail}");
    assert_eq!(detail["ledger"].as_array().map(|rows| rows.len()), Some(1));
    assert_eq!(detail["ledger"][0]["kind"], "issue");
    assert_eq!(detail["ledger"][0]["delta_poisha"], 50_000);
    assert_eq!(detail["ledger"][0]["balance_after_poisha"], 50_000);
    let (ledger_sum, cached): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(delta_poisha), 0)::bigint, \
         (SELECT balance_poisha FROM gift_cards WHERE id = $1) \
         FROM gift_card_ledger WHERE gift_card_id = $1",
    )
    .bind(sqlx::types::Uuid::parse_str(&card_id).expect("uuid"))
    .fetch_one(&pool)
    .await
    .expect("ledger sum query");
    assert_eq!(ledger_sum, 50_000);
    assert_eq!(ledger_sum, cached, "ledger sums to the cached balance");

    // 4. The public balance check answers the holder — and
    // accepts the code as typed off a printed card
    // (lowercase, grouped), because normalisation is part
    // of the instrument.
    let (status, balance) = balance_check(&app, &code).await;
    assert_eq!(status, StatusCode::OK, "balance: {balance}");
    assert_eq!(balance["balance_poisha"], 50_000);
    assert_eq!(balance["status"], "active");
    assert_eq!(balance["store_slug"], "w60-cards");
    assert_eq!(balance["last4"], &code[code.len() - 4..]);
    let grouped = format!(
        "{}-{} {}-{} {}",
        code[..9].to_lowercase(),
        &code[9..13],
        &code[13..17],
        &code[17..21],
        &code[21..]
    );
    let (status, balance) = balance_check(&app, &grouped).await;
    assert_eq!(status, StatusCode::OK, "grouped code balance: {balance}");
    assert_eq!(balance["balance_poisha"], 50_000);

    // 5. Unknown and malformed codes get ONE uniform 404 —
    // the endpoint cannot be used to enumerate.
    let (status_unknown, body_unknown) =
        balance_check(&app, "bdgc_AAAAAAAAAAAAAAAAAAAAAAAA").await;
    assert_eq!(status_unknown, StatusCode::NOT_FOUND);
    let (status_bad, body_bad) = balance_check(&app, "not-a-card").await;
    assert_eq!(status_bad, StatusCode::NOT_FOUND);
    assert_eq!(
        body_unknown["error"], body_bad["error"],
        "unknown and malformed are indistinguishable"
    );

    // 6. Issue validation: the documented caps are real.
    for bad in [
        json!({ "amount_poisha": 999 }),
        json!({ "amount_poisha": 5_000_001 }),
        json!({ "amount_poisha": 0 }),
        json!({ "amount_poisha": 50_000, "validity_days": 0 }),
        json!({ "amount_poisha": 50_000, "validity_days": 3_651 }),
        json!({ "amount_poisha": 50_000, "validity_days": 30, "no_expiry": true }),
        json!({ "amount_poisha": 50_000, "count": 51 }),
        json!({ "amount_poisha": 50_000, "count": 0 }),
    ] {
        let (status, body) = call(
            &app,
            Method::POST,
            "/v1/seller/gift-cards",
            Some(bad.clone()),
            Some(&seller),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "issue {bad}: {body}");
    }

    // 7. A no-expiry card carries a null expiry and is live.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": 10_000, "no_expiry": true, "note": "ঈদ মোবারক" })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "no-expiry issue: {body}");
    assert!(body["cards"][0]["expires_at"].is_null());
    assert_eq!(body["cards"][0]["note"], "ঈদ মোবারক");
    assert_eq!(body["cards"][0]["status"], "active");

    // 8. Bulk issue: three cards, three DISTINCT codes, one
    // transaction's worth of ledger rows.
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(json!({ "amount_poisha": 20_000, "count": 3 })),
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "bulk issue: {body}");
    let cards = body["cards"].as_array().expect("cards array");
    assert_eq!(cards.len(), 3);
    let mut codes: Vec<&str> = cards
        .iter()
        .map(|card| card["code"].as_str().expect("code"))
        .collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), 3, "bulk codes are distinct");
    for card in cards {
        assert_eq!(card["balance_poisha"], 20_000);
    }

    // 9. Stats: five cards now (1 + 1 + 3), all active,
    // outstanding = 50_000 + 10_000 + 3 × 20_000.
    let (status, stats) = call(
        &app,
        Method::GET,
        "/v1/seller/gift-card-stats",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "stats: {stats}");
    assert_eq!(stats["cards_total"], 5);
    assert_eq!(stats["active_cards"], 5);
    assert_eq!(stats["initial_total_poisha"], 120_000);
    assert_eq!(stats["outstanding_poisha"], 120_000);
    assert_eq!(stats["redeemed_total_poisha"], 0);
    assert_eq!(stats["restored_total_poisha"], 0);
}
