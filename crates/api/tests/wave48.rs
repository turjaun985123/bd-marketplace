//! Wave 48 integration test: the notification reaper.
//! `process_pending` claims a batch (status `sending`,
//! `claimed_at` stamped, migration 0039) and commits BEFORE
//! sending, so a worker that dies mid-batch strands its
//! rows in `sending`. `reap_stale_sending` recovers them:
//! a claim older than the staleness threshold returns to
//! `pending` (consuming one attempt — the lost claim counts
//! against the MAX_ATTEMPTS budget, which is what stops a
//! crash-looping row from cycling forever), and a claim
//! that spends the last attempt lands in `failed` instead.
//! A FRESH claim is never touched.
//!
//! Unlike most wave suites this one drives the `notify`
//! crate directly on the pool — the exact calls the worker
//! binary makes each cycle (`reap_stale_sending`, then
//! `process_pending` through `sender_from_env(true)`, the
//! dev log sender) — because the reaper has no HTTP surface
//! by design. Setup follows the usual api-test canon:
//! DATABASE_URL gate, `run_migrations`, full truncate.
//!
//! Gated on `DATABASE_URL`: without it the test skips
//! cleanly so a plain local `cargo test` (no database)
//! still passes. CI provides a Postgres 16 service and
//! sets `DATABASE_URL` for the rust job.

use api::run_migrations;
use notify::{process_pending, reap_stale_sending, sender_from_env, DEFAULT_BATCH};
use sqlx::PgPool;
use uuid::Uuid;

/// Seed one notification directly in `sending`, as a
/// worker's claim would have left it: `attempts` failed
/// sends already recorded, claimed `claimed_minutes_ago`
/// minutes in the past. Returns the row id.
async fn seed_sending(
    pool: &PgPool,
    user_id: Uuid,
    attempts: i32,
    claimed_minutes_ago: i64,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO notifications \
             (user_id, phone, kind, body, status, attempts, claimed_at) \
         VALUES ($1, '+8801711111111', 'order_placed', 'wave48 seeded body', \
                 'sending', $2, now() - make_interval(mins => $3::int)) \
         RETURNING id",
    )
    .bind(user_id)
    .bind(attempts)
    .bind(claimed_minutes_ago)
    .fetch_one(pool)
    .await
    .expect("seed sending notification")
}

/// (status, attempts, last_error, sent_at is set)
async fn load_row(pool: &PgPool, id: Uuid) -> (String, i32, Option<String>, bool) {
    sqlx::query_as(
        "SELECT status, attempts, last_error, sent_at IS NOT NULL \
         FROM notifications WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("notification row loads")
}

#[tokio::test]
async fn wave48_reaper_recovers_stale_sending_rows() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping Wave 48 integration test");
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
         payments, order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    let user_id: Uuid =
        sqlx::query_scalar("INSERT INTO users (phone) VALUES ('+8801711111111') RETURNING id")
            .fetch_one(&pool)
            .await
            .expect("insert user");

    // The worker's dev-mode sender (sends succeed by
    // printing), exactly as the worker builds it.
    let sender = sender_from_env(true);

    // Three stranded-or-live claims, all judged against a
    // 10-minute staleness threshold:
    //   fresh: claimed just now — a live worker's row.
    //   stale: claimed 30 minutes ago with one failed send
    //          already recorded — recoverable.
    //   spent: claimed 30 minutes ago with all 3 attempts
    //          already recorded — the reap must fail it,
    //          not resurrect it.
    let fresh = seed_sending(&pool, user_id, 0, 0).await;
    let stale = seed_sending(&pool, user_id, 1, 30).await;
    let spent = seed_sending(&pool, user_id, 3, 30).await;

    // 1. One reap over the mixed batch: exactly one row
    // back to pending, exactly one marked failed, and the
    // fresh claim untouched.
    let report = reap_stale_sending(&pool, 10)
        .await
        .expect("reap mixed batch");
    assert_eq!(report.returned_to_pending, 1, "report: {report:?}");
    assert_eq!(report.marked_failed, 1, "report: {report:?}");
    assert_eq!(report.reaped(), 2);

    let (status, attempts, last_error, _) = load_row(&pool, fresh).await;
    assert_eq!(status, "sending", "fresh claim must not be reaped");
    assert_eq!(attempts, 0);
    assert_eq!(last_error, None);

    // The stale row is pending again; the lost claim
    // consumed one attempt (1 recorded failure + 1 reap = 2)
    // and the reap explains itself in last_error.
    let (status, attempts, last_error, _) = load_row(&pool, stale).await;
    assert_eq!(status, "pending");
    assert_eq!(attempts, 2);
    assert!(
        last_error
            .as_deref()
            .is_some_and(|e| e.starts_with("reaped:")),
        "last_error: {last_error:?}"
    );

    // The spent row is failed, not resurrected — and stays
    // visible with the reap note.
    let (status, attempts, last_error, _) = load_row(&pool, spent).await;
    assert_eq!(status, "failed");
    assert_eq!(attempts, 4, "the reap consumes one attempt");
    assert!(
        last_error
            .as_deref()
            .is_some_and(|e| e.starts_with("reaped:")),
        "last_error: {last_error:?}"
    );

    // 2. The same cycle's drain (the worker runs the reaper
    // first precisely so this works) delivers the recovered
    // row through the dev log sender: sent, stamped, and
    // the reap note cleared by the successful write-back.
    let report = process_pending(&pool, sender.as_ref(), DEFAULT_BATCH)
        .await
        .expect("process after reap");
    assert_eq!(report.sent, 1, "report: {report:?}");
    assert_eq!(report.processed(), 1);
    let (status, _, last_error, sent) = load_row(&pool, stale).await;
    assert_eq!(status, "sent");
    assert!(sent, "sent_at is set");
    assert_eq!(last_error, None);

    // 3. Nothing else moves: the failed row is never
    // claimed again, the fresh claim is still a live
    // `sending` row (not pending, so not claimed), and a
    // second reap at the same threshold finds nothing
    // stale.
    let report = process_pending(&pool, sender.as_ref(), DEFAULT_BATCH)
        .await
        .expect("process again");
    assert_eq!(report.processed(), 0);
    let report = reap_stale_sending(&pool, 10).await.expect("reap again");
    assert_eq!(report.reaped(), 0, "report: {report:?}");
    let (status, _, _, _) = load_row(&pool, fresh).await;
    assert_eq!(status, "sending");

    // 4. The fresh claim ages out like any other: backdate
    // its claim stamp past the threshold and it recovers
    // through the same path — pending with one attempt
    // consumed, then delivered by the next drain.
    sqlx::query(
        "UPDATE notifications \
         SET claimed_at = now() - interval '60 minutes' WHERE id = $1",
    )
    .bind(fresh)
    .execute(&pool)
    .await
    .expect("backdate fresh claim");
    let report = reap_stale_sending(&pool, 10)
        .await
        .expect("reap backdated claim");
    assert_eq!(report.returned_to_pending, 1, "report: {report:?}");
    assert_eq!(report.marked_failed, 0);
    let (status, attempts, _, _) = load_row(&pool, fresh).await;
    assert_eq!(status, "pending");
    assert_eq!(attempts, 1);

    let report = process_pending(&pool, sender.as_ref(), DEFAULT_BATCH)
        .await
        .expect("process backdated row");
    assert_eq!(report.sent, 1, "report: {report:?}");
    let (status, _, _, sent) = load_row(&pool, fresh).await;
    assert_eq!(status, "sent");
    assert!(sent, "sent_at is set");
}
