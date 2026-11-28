//! Wave 60 integration test: the settings-driven default
//! validity (a `gift_card_default_validity_days` platform
//! setting beats the 365-day constant, DB > const) and the
//! worker's gift-card expiry-reminder sweep — a card that
//! still holds a balance and expires within the 7-day
//! window earns its ISSUING seller exactly one outbox
//! reminder (`gift_card_expiring`), stamped on the card
//! row in the same transaction; disabled, depleted-window,
//! no-expiry, far-future and already-expired cards are
//! never reminded about, and a second sweep reminds nobody
//! again.
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
            eprintln!("DATABASE_URL not set — skipping Wave 60k integration tests");
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

/// Issue one card with an explicit body; returns the card view.
async fn issue_with(app: &Router, token: &str, body: Value) -> Value {
    let (status, response) = call(
        app,
        Method::POST,
        "/v1/seller/gift-cards",
        Some(body),
        Some(token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "issue card: {response}");
    response["cards"][0].clone()
}

#[tokio::test]
async fn wave60_settings_default_validity_and_expiry_reminder_sweep() {
    let Some((app, pool)) = test_app().await else {
        return;
    };
    let seller = login(&app, "01766666661").await;
    create_store(&app, &seller, "wave60k-store").await;
    let seller_id: String = sqlx::query_scalar(
        "SELECT id::text FROM users WHERE phone = '+8801766666661'",
    )
    .fetch_one(&pool)
    .await
    .expect("seller user id");

    // -- The platform setting drives the default validity:
    //    5 days instead of the 365-day constant.
    sqlx::query(
        "INSERT INTO platform_settings (key, value) \
         VALUES ('gift_card_default_validity_days', '5')",
    )
    .execute(&pool)
    .await
    .expect("seed validity setting");
    let card_a = issue_with(
        &app,
        &seller,
        json!({ "amount_poisha": 50_000 }),
    )
    .await;
    let card_a_id = card_a["id"].as_str().expect("card A id").to_string();
    let in_window: bool = sqlx::query_scalar(
        "SELECT expires_at > now() + interval '4 days' \
         AND expires_at < now() + interval '6 days' \
         FROM gift_cards WHERE id = $1::uuid",
    )
    .bind(&card_a_id)
    .fetch_one(&pool)
    .await
    .expect("card A expiry in the settings window");
    assert!(in_window, "settings default of 5 days applied: {card_a}");
    // Canon: a test that writes platform_settings deletes
    // its rows afterwards.
    sqlx::query("DELETE FROM platform_settings WHERE key = 'gift_card_default_validity_days'")
        .execute(&pool)
        .await
        .expect("clear validity setting");

    // -- The sweep's cast: one card per outcome class.
    // B: explicit 3-day validity — inside the 7-day window.
    let card_b = issue_with(
        &app,
        &seller,
        json!({ "amount_poisha": 40_000, "validity_days": 3 }),
    )
    .await;
    let card_b_id = card_b["id"].as_str().expect("card B id").to_string();
    // C: explicit 30-day validity — outside the window.
    let card_c = issue_with(
        &app,
        &seller,
        json!({ "amount_poisha": 40_000, "validity_days": 30 }),
    )
    .await;
    let card_c_id = card_c["id"].as_str().expect("card C id").to_string();
    // D: inside the window but DISABLED by its seller.
    let card_d = issue_with(
        &app,
        &seller,
        json!({ "amount_poisha": 40_000, "validity_days": 2 }),
    )
    .await;
    let card_d_id = card_d["id"].as_str().expect("card D id").to_string();
    let disable_uri = format!("/v1/seller/gift-cards/{card_d_id}/disable");
    let (status, body) = call(
        &app,
        Method::POST,
        &disable_uri,
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "disable card D: {body}");
    // E: already expired (backdated) — the moment to act
    // has passed, so no reminder.
    let card_e = issue_with(
        &app,
        &seller,
        json!({ "amount_poisha": 40_000, "validity_days": 2 }),
    )
    .await;
    let card_e_id = card_e["id"].as_str().expect("card E id").to_string();
    sqlx::query("UPDATE gift_cards SET expires_at = now() - interval '1 hour' WHERE id = $1::uuid")
        .bind(&card_e_id)
        .execute(&pool)
        .await
        .expect("backdate card E expiry");
    // F: no expiry at all — nothing to lapse, no reminder.
    let card_f = issue_with(
        &app,
        &seller,
        json!({ "amount_poisha": 40_000, "no_expiry": true }),
    )
    .await;
    let card_f_id = card_f["id"].as_str().expect("card F id").to_string();

    // -- Run the sweep exactly as the worker does.
    let sweep_state = AppState::new(Some(pool.clone()), "wave60-test-secret".to_string(), true);
    let report = api::gift_cards::sweep_gift_card_expiry_reminders(&sweep_state)
        .await
        .expect("first sweep runs");
    assert_eq!(report.reminded, 2, "only cards A and B qualify");

    // Exactly two outbox rows, both to the issuing seller,
    // each naming its card's last4 and balance.
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT user_id::text, kind, body FROM notifications \
         WHERE kind = 'gift_card_expiring' ORDER BY body",
    )
    .fetch_all(&pool)
    .await
    .expect("reminder rows");
    assert_eq!(rows.len(), 2, "two reminders enqueued: {rows:?}");
    for (user_id, _kind, body) in &rows {
        assert_eq!(user_id, &seller_id, "reminder goes to the issuer");
        assert!(body.contains("••"), "body masks the code: {body}");
    }
    let last4_a = card_a["last4"].as_str().expect("card A last4");
    let last4_b = card_b["last4"].as_str().expect("card B last4");
    assert!(
        rows.iter().any(|(_, _, body)| body.contains(last4_a)),
        "card A reminded: {rows:?}"
    );
    assert!(
        rows.iter().any(|(_, _, body)| body.contains(last4_b)),
        "card B reminded: {rows:?}"
    );

    // The stamps: A and B stamped, everyone else untouched.
    let stamped: Vec<String> = sqlx::query_scalar(
        "SELECT id::text FROM gift_cards WHERE expiry_reminded_at IS NOT NULL ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("stamped cards");
    assert_eq!(stamped.len(), 2, "two cards stamped: {stamped:?}");
    assert!(stamped.contains(&card_a_id));
    assert!(stamped.contains(&card_b_id));
    for unreminded in [&card_c_id, &card_d_id, &card_e_id, &card_f_id] {
        assert!(
            !stamped.contains(unreminded),
            "card {unreminded} must not be stamped"
        );
    }

    // -- A second sweep reminds nobody again (exactly-once).
    let report = api::gift_cards::sweep_gift_card_expiry_reminders(&sweep_state)
        .await
        .expect("second sweep runs");
    assert_eq!(report.reminded, 0, "the stamp makes the sweep idempotent");
    let reminder_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM notifications WHERE kind = 'gift_card_expiring'",
    )
    .fetch_one(&pool)
    .await
    .expect("reminder count after second sweep");
    assert_eq!(reminder_count, 2, "still exactly two reminders");
}
