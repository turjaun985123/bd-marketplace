//! Wave 29 integration test for the notify crate's outbox:
//! `enqueue` renders the body from the payload via the domain
//! templates and stores the row as `pending`;
//! `process_pending` sends through the given sender and
//! records the outcome — success marks `sent` with `sent_at`,
//! failures increment `attempts` and return the row to
//! `pending`, and the third failure marks it `failed`.
//! `sender_from_env` picks the log sender in dev mode (sends
//! succeed by printing) and the unconfigured sender outside
//! it (sends fail with `sms_not_configured`).
//!
//! Gated on `DATABASE_URL`: without it the test skips cleanly
//! so a plain local `cargo test` (no database) still passes.
//! CI provides a Postgres 16 service and sets `DATABASE_URL`.

use async_trait::async_trait;
use notify::{enqueue, process_pending, sender_from_env, SmsError, SmsSender};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// A sender that always succeeds (and sends nothing).
struct OkSender;

#[async_trait]
impl SmsSender for OkSender {
    async fn send_sms(&self, _phone: &str, _message: &str) -> Result<(), SmsError> {
        Ok(())
    }
}

/// A sender that always fails with a recognisable error.
struct FailSender;

#[async_trait]
impl SmsSender for FailSender {
    async fn send_sms(&self, _phone: &str, _message: &str) -> Result<(), SmsError> {
        Err(SmsError::new("stub_failure"))
    }
}

/// (status, attempts, body, last_error, sent_at is set)
async fn load_row(pool: &PgPool, id: Uuid) -> (String, i32, String, Option<String>, bool) {
    sqlx::query_as(
        "SELECT status, attempts, body, last_error, sent_at IS NOT NULL \
         FROM notifications WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("notification row loads")
}

#[tokio::test]
async fn outbox_enqueue_process_and_retry_flow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping notify outbox integration test");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrations run");

    // Clean slate so the test is re-runnable against the same
    // database (the API suites do the same in their own runs).
    sqlx::query("TRUNCATE notifications, users CASCADE")
        .execute(&pool)
        .await
        .expect("truncate tables");

    let user_id: Uuid =
        sqlx::query_scalar("INSERT INTO users (phone) VALUES ('+8801711111111') RETURNING id")
            .fetch_one(&pool)
            .await
            .expect("insert user");

    // 1. Enqueue renders the body from the payload's template
    // variables (short order id, formatted amount) and stores
    // the row as pending with zero attempts.
    let payload = json!({
        "order_id": "abcdef12-3456-7890-abcd-ef1234567890",
        "amount_poisha": 206_000
    });
    let first = enqueue(&pool, user_id, "+8801711111111", "order_placed", &payload)
        .await
        .expect("enqueue order_placed");
    let (status, attempts, body, last_error, sent) = load_row(&pool, first).await;
    assert_eq!(status, "pending");
    assert_eq!(attempts, 0);
    assert!(body.contains("#abcdef12"), "body: {body}");
    assert!(body.contains("৳2060"), "body: {body}");
    assert_eq!(last_error, None);
    assert!(!sent);

    // 2. A succeeding sender: the row is sent, stamped, and
    // the report counts exactly it.
    let report = process_pending(&pool, &OkSender, 20)
        .await
        .expect("process with ok sender");
    assert_eq!(report.sent, 1, "report: {report:?}");
    assert_eq!(report.retried, 0);
    assert_eq!(report.failed, 0);
    let (status, _, _, _, sent) = load_row(&pool, first).await;
    assert_eq!(status, "sent");
    assert!(sent, "sent_at is set");

    // An empty outbox processes as an all-zero report.
    let report = process_pending(&pool, &OkSender, 20)
        .await
        .expect("process empty outbox");
    assert_eq!(report.processed(), 0);

    // 3. A failing sender: attempts increment and the row
    // returns to pending twice, then the third failure marks
    // it failed — and only that last report counts a failure.
    let second = enqueue(&pool, user_id, "+8801711111111", "payout_paid", &payload)
        .await
        .expect("enqueue payout_paid");

    let report = process_pending(&pool, &FailSender, 20)
        .await
        .expect("process failing once");
    assert_eq!(report.retried, 1, "report: {report:?}");
    assert_eq!(report.failed, 0);
    let (status, attempts, _, last_error, _) = load_row(&pool, second).await;
    assert_eq!(status, "pending");
    assert_eq!(attempts, 1);
    assert_eq!(last_error.as_deref(), Some("stub_failure"));

    let report = process_pending(&pool, &FailSender, 20)
        .await
        .expect("process failing twice");
    assert_eq!(report.retried, 1);
    let (status, attempts, _, _, _) = load_row(&pool, second).await;
    assert_eq!(status, "pending");
    assert_eq!(attempts, 2);

    let report = process_pending(&pool, &FailSender, 20)
        .await
        .expect("process failing third time");
    assert_eq!(report.failed, 1, "report: {report:?}");
    assert_eq!(report.retried, 0);
    let (status, attempts, _, last_error, sent) = load_row(&pool, second).await;
    assert_eq!(status, "failed");
    assert_eq!(attempts, 3);
    assert_eq!(last_error.as_deref(), Some("stub_failure"));
    assert!(!sent);

    // A failed row is never claimed again.
    let report = process_pending(&pool, &OkSender, 20)
        .await
        .expect("process after failure");
    assert_eq!(report.processed(), 0);

    // 4. The sender selection rule: dev mode's log sender
    // "sends" successfully (by printing); outside dev mode
    // the unconfigured sender fails with sms_not_configured.
    let dev_sender = sender_from_env(true);
    assert!(dev_sender
        .send_sms("+8801711111111", "test message")
        .await
        .is_ok());
    let prod_sender = sender_from_env(false);
    let error = prod_sender
        .send_sms("+8801711111111", "test message")
        .await
        .expect_err("unconfigured sender must fail");
    assert!(
        error.message.starts_with("sms_not_configured"),
        "error: {error}"
    );
}
