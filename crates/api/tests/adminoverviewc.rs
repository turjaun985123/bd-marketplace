//! Admin overview remake test, part 3: gift-card stats
//! after the liability fix. `outstanding_poisha` counts
//! only balances a holder can still spend (active cards);
//! balances frozen on disabled cards and stranded on
//! expired cards are reported in their own fields, and the
//! ledger's admin adjustments are totalled so the books
//! reconcile: initial − redeemed + restored + adjusted =
//! the sum of all current balances. Fixtures are direct
//! SQL rows (the state is fully determined by the card and
//! ledger columns), read back through BOTH consumers of
//! the shared stats function — the admin metrics snapshot
//! and the seller's own stats endpoint. Helpers are copied
//! from the Wave 9 test on purpose.
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

/// Insert one gift-card fixture row; returns the card id.
#[allow(clippy::too_many_arguments)]
async fn insert_card(
    pool: &PgPool,
    store_id: &str,
    issuer_id: &str,
    code_hash: &str,
    initial: i64,
    balance: i64,
    expires_sql: &str,
    disabled: bool,
) -> String {
    let disabled_sql = if disabled { "now()" } else { "NULL" };
    let sql = format!(
        "INSERT INTO gift_cards \
         (store_id, code_hash, code_prefix, last4, initial_amount_poisha, \
          balance_poisha, issued_by_user_id, expires_at, disabled_at) \
         VALUES ($1::uuid, $2, 'bdgc', '0000', $3, $4, $5::uuid, \
                 {expires_sql}, {disabled_sql}) \
         RETURNING id::text"
    );
    sqlx::query_scalar(&sql)
        .bind(store_id)
        .bind(code_hash)
        .bind(initial)
        .bind(balance)
        .bind(issuer_id)
        .fetch_one(pool)
        .await
        .expect("insert gift card fixture")
}

/// Append one ledger row to a fixture card.
async fn insert_ledger(pool: &PgPool, card_id: &str, kind: &str, delta: i64, after: i64) {
    sqlx::query(
        "INSERT INTO gift_card_ledger \
         (gift_card_id, kind, delta_poisha, balance_after_poisha) \
         VALUES ($1::uuid, $2, $3, $4)",
    )
    .bind(card_id)
    .bind(kind)
    .bind(delta)
    .bind(after)
    .execute(pool)
    .await
    .expect("insert ledger fixture");
}

/// The shared assertions over one stats payload.
fn assert_stats(stats: &Value) {
    assert_eq!(stats["cards_total"], 4, "stats: {stats}");
    assert_eq!(stats["active_cards"], 2);
    assert_eq!(stats["disabled_cards"], 1);
    assert_eq!(stats["expired_cards"], 1);
    assert_eq!(stats["depleted_cards"], 0);
    assert_eq!(stats["initial_total_poisha"], 160_000);
    // Only the two spendable cards' balances are the live
    // liability: 40_000 (part-redeemed) + 10_000 (adjusted).
    assert_eq!(stats["outstanding_poisha"], 50_000);
    assert_eq!(stats["frozen_disabled_poisha"], 30_000);
    assert_eq!(stats["expired_unredeemed_poisha"], 25_000);
    assert_eq!(stats["redeemed_total_poisha"], 60_000);
    assert_eq!(stats["restored_total_poisha"], 0);
    assert_eq!(stats["adjusted_net_poisha"], 5_000);
    // Reconciliation: every poisha of balance is accounted
    // for by the issue/redeem/restore/adjust flows.
    let balances = stats["outstanding_poisha"].as_i64().expect("outstanding")
        + stats["frozen_disabled_poisha"].as_i64().expect("frozen")
        + stats["expired_unredeemed_poisha"].as_i64().expect("expired");
    let flows = stats["initial_total_poisha"].as_i64().expect("initial")
        - stats["redeemed_total_poisha"].as_i64().expect("redeemed")
        + stats["restored_total_poisha"].as_i64().expect("restored")
        + stats["adjusted_net_poisha"].as_i64().expect("adjusted");
    assert_eq!(balances, flows, "gift-card books reconcile: {stats}");
    assert_eq!(balances, 105_000);
}

#[tokio::test]
async fn adminoverview_gift_card_stats_reconcile() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin overview gift-card test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    // Clean slate so the test is re-runnable against the same database.
    sqlx::query(
        "TRUNCATE otp_request_log, gift_card_ledger, gift_cards, products, stores, \
         otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let state = AppState::new(
        Some(pool.clone()),
        "adminoverviewc-test-secret".to_string(),
        true,
    );
    let app = build_app(state);

    let admin = login(&app, "01711111111").await;
    let seller = login(&app, "01799999999").await;
    sqlx::query("UPDATE users SET is_admin = true WHERE phone = '+8801711111111'")
        .execute(&pool)
        .await
        .expect("promote admin by SQL");
    let store = create_store(&app, &seller, "aovc-store").await;
    let issuer_id: String =
        sqlx::query_scalar("SELECT id::text FROM users WHERE phone = '+8801799999999'")
            .fetch_one(&pool)
            .await
            .expect("seller id by phone");

    // Card A: active, issued 100_000, 60_000 redeemed.
    let card_a = insert_card(
        &pool, &store, &issuer_id, "aovc-hash-a", 100_000, 40_000, "NULL", false,
    )
    .await;
    insert_ledger(&pool, &card_a, "issue", 100_000, 100_000).await;
    insert_ledger(&pool, &card_a, "redeem", -60_000, 40_000).await;
    // Card B: expired yesterday with 25_000 stranded.
    let card_b = insert_card(
        &pool,
        &store,
        &issuer_id,
        "aovc-hash-b",
        25_000,
        25_000,
        "now() - interval '1 day'",
        false,
    )
    .await;
    insert_ledger(&pool, &card_b, "issue", 25_000, 25_000).await;
    // Card C: disabled with 30_000 frozen.
    let card_c = insert_card(
        &pool, &store, &issuer_id, "aovc-hash-c", 30_000, 30_000, "NULL", true,
    )
    .await;
    insert_ledger(&pool, &card_c, "issue", 30_000, 30_000).await;
    // Card D: active; issued 5_000, an admin adjustment
    // added another 5_000.
    let card_d = insert_card(
        &pool, &store, &issuer_id, "aovc-hash-d", 5_000, 10_000, "NULL", false,
    )
    .await;
    insert_ledger(&pool, &card_d, "issue", 5_000, 5_000).await;
    insert_ledger(&pool, &card_d, "admin_adjust", 5_000, 10_000).await;

    // The platform view (admin metrics) and the store's own
    // view (seller stats) read the same figures.
    let (status, metrics) = call(&app, Method::GET, "/v1/admin/metrics", None, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "metrics: {metrics}");
    assert_stats(&metrics["gift_cards"]);

    let (status, stats) = call(
        &app,
        Method::GET,
        "/v1/seller/gift-card-stats",
        None,
        Some(&seller),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "seller stats: {stats}");
    assert_stats(&stats);
}
