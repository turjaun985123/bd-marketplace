//! Notification delivery (Wave 29): the SMS senders and the
//! transactional outbox over the `notifications` table
//! (migration 0029).
//!
//! Two halves, each individually callable:
//!
//! - [`SmsSender`] — how one message reaches a phone. The
//!   implementations are [`LogSmsSender`] (dev mode: prints
//!   the message, delivers nothing) and
//!   [`UnconfiguredSmsSender`] (always fails with
//!   `sms_not_configured`). A real Bangladeshi SMS gateway
//!   joins later as a third implementation; [`sender_from_env`]
//!   is the ONE selection rule both the API (OTP codes) and
//!   the worker (outbox) use, so the two can never disagree
//!   about how messages are sent.
//! - The outbox — [`enqueue`] writes a `pending` row inside
//!   the caller's transaction (the body rendered on the spot
//!   by the pure `domain::render_body`), and
//!   [`process_pending`] claims a batch, sends each message,
//!   and records the outcome on its row. A row is retried
//!   until [`MAX_ATTEMPTS`] failed attempts, then stays
//!   `failed` — visible in the admin notifications view,
//!   never silently dropped.
//!
//! All SQL is runtime-checked sqlx, like the rest of the
//! workspace (no `query!` macros).

use std::fmt;

use async_trait::async_trait;
use domain::{render_body, NotificationVars};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// Failed send attempts after which a notification is marked
/// `failed` and no longer retried (Wave 29).
pub const MAX_ATTEMPTS: i32 = 3;

/// Default batch size for one [`process_pending`] call.
pub const DEFAULT_BATCH: i64 = 20;

// ---------------------------------------------------------------------------
// SMS senders
// ---------------------------------------------------------------------------

/// One failed send. The message is stored on the notification
/// row as `last_error`, so it is written for operators: the
/// unconfigured sender's message starts with the stable code
/// `sms_not_configured`.
#[derive(Debug, Clone)]
pub struct SmsError {
    pub message: String,
}

impl SmsError {
    pub fn new(message: impl Into<String>) -> Self {
        SmsError {
            message: message.into(),
        }
    }
}

impl fmt::Display for SmsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SmsError {}

/// How one SMS message reaches a phone. One method,
/// individually callable; the API uses it for OTP codes and
/// the worker for outbox notifications.
#[async_trait]
pub trait SmsSender: Send + Sync {
    async fn send_sms(&self, phone: &str, message: &str) -> Result<(), SmsError>;
}

/// Dev-mode sender: prints the message (including any OTP
/// code) to stderr and reports success. Only ever selected
/// when dev mode is explicitly enabled — see
/// [`sender_from_env`].
pub struct LogSmsSender;

#[async_trait]
impl SmsSender for LogSmsSender {
    async fn send_sms(&self, phone: &str, message: &str) -> Result<(), SmsError> {
        eprintln!("[dev SMS] to {phone}: {message}");
        Ok(())
    }
}

/// The honest stand-in for a real gateway that is not
/// configured: every send fails with `sms_not_configured`.
/// Outbox rows sent through it accumulate attempts and land
/// in `failed` with this error recorded — the failure is
/// visible in the admin view, never a pretend success.
pub struct UnconfiguredSmsSender;

#[async_trait]
impl SmsSender for UnconfiguredSmsSender {
    async fn send_sms(&self, _phone: &str, _message: &str) -> Result<(), SmsError> {
        Err(SmsError::new(
            "sms_not_configured: no SMS gateway is configured and SMS dev mode is off",
        ))
    }
}

/// The short name of the sender [`sender_from_env`] picks,
/// for startup logs: `log` in dev mode, `unconfigured`
/// otherwise.
pub fn sender_kind(dev_mode: bool) -> &'static str {
    if dev_mode {
        "log"
    } else {
        "unconfigured"
    }
}

/// THE sender selection rule (Wave 29): dev mode sends
/// through the log; anything else is unconfigured until a
/// real gateway implementation exists. Used by the API for
/// OTP codes and by the worker for the outbox, so both
/// always agree.
pub fn sender_from_env(dev_mode: bool) -> Box<dyn SmsSender> {
    if dev_mode {
        Box::new(LogSmsSender)
    } else {
        Box::new(UnconfiguredSmsSender)
    }
}

// ---------------------------------------------------------------------------
// Outbox
// ---------------------------------------------------------------------------

/// The template variables one payload carries, read out of
/// the JSON the API funnels store: `order_id`, `store_name`,
/// `amount_poisha`, `status`. Missing fields render as the
/// domain defaults (empty / none) — `render_body` has a
/// defined output for every combination.
fn vars_from_payload(payload: &Value) -> NotificationVars {
    NotificationVars {
        order_id: payload
            .get("order_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        store_name: payload
            .get("store_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        amount_poisha: payload.get("amount_poisha").and_then(Value::as_i64),
        status: payload
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

/// Queue one notification: render its body from `payload`
/// (the template variables) via the pure domain renderer and
/// INSERT it as `pending`. Takes any sqlx executor so API
/// service functions pass their TRANSACTION (`&mut *tx`) —
/// the row then commits or rolls back together with the
/// state change it announces, which is the entire point of
/// the outbox. Returns the new row's id.
pub async fn enqueue(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    user_id: Uuid,
    phone: &str,
    kind: &str,
    payload: &Value,
) -> Result<Uuid, sqlx::Error> {
    let body = render_body(kind, &vars_from_payload(payload));
    // The payload is stored as JSON text cast to jsonb (the
    // workspace sqlx build has no `json` feature — the same
    // approach as the admin audit log).
    let payload_text = payload.to_string();
    sqlx::query_scalar(
        "INSERT INTO notifications (user_id, phone, kind, body, payload) \
         VALUES ($1, $2, $3, $4, $5::jsonb) RETURNING id",
    )
    .bind(user_id)
    .bind(phone)
    .bind(kind)
    .bind(body)
    .bind(payload_text)
    .fetch_one(executor)
    .await
}

/// What one [`process_pending`] call did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProcessReport {
    /// Rows whose send succeeded (now `sent`).
    pub sent: i64,
    /// Rows whose send failed but which have attempts left
    /// (back to `pending` for a later batch).
    pub retried: i64,
    /// Rows whose send failed on their last allowed attempt
    /// (now `failed`, kept for the admin view).
    pub failed: i64,
}

impl ProcessReport {
    /// How many rows this call claimed in total.
    pub fn processed(&self) -> i64 {
        self.sent + self.retried + self.failed
    }
}

/// One claimed outbox row.
#[derive(FromRow)]
struct ClaimedRow {
    id: Uuid,
    phone: String,
    body: String,
    attempts: i32,
}

/// Claim up to `batch` pending notifications and send each
/// one through `sender` (Wave 29). The claim is one
/// transaction: an UPDATE over the oldest pending rows,
/// picked `FOR UPDATE SKIP LOCKED` so two workers can never
/// claim the same row, flipping them to `sending` and
/// committing BEFORE any send happens — no row lock is held
/// across a network call. Each send's outcome is then
/// written back on its own row: success marks `sent` with
/// `sent_at`; failure increments `attempts`, records
/// `last_error`, and returns the row to `pending` while
/// attempts remain, or marks it `failed` at
/// [`MAX_ATTEMPTS`].
///
/// A worker that dies mid-batch leaves its claimed rows in
/// `sending`; they are counted in the admin view (never
/// hidden) and are a known, documented edge — no automatic
/// reaper exists yet.
pub async fn process_pending(
    pool: &PgPool,
    sender: &dyn SmsSender,
    batch: i64,
) -> Result<ProcessReport, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let claimed: Vec<ClaimedRow> = sqlx::query_as(
        "UPDATE notifications SET status = 'sending' \
         WHERE id IN ( \
             SELECT id FROM notifications \
             WHERE status = 'pending' \
             ORDER BY created_at \
             LIMIT $1 \
             FOR UPDATE SKIP LOCKED \
         ) \
         RETURNING id, phone, body, attempts",
    )
    .bind(batch)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut report = ProcessReport::default();
    for row in claimed {
        match sender.send_sms(&row.phone, &row.body).await {
            Ok(()) => {
                sqlx::query(
                    "UPDATE notifications \
                     SET status = 'sent', sent_at = now(), last_error = NULL \
                     WHERE id = $1",
                )
                .bind(row.id)
                .execute(pool)
                .await?;
                report.sent += 1;
            }
            Err(error) => {
                let attempts = row.attempts + 1;
                let exhausted = attempts >= MAX_ATTEMPTS;
                let status = if exhausted { "failed" } else { "pending" };
                sqlx::query(
                    "UPDATE notifications \
                     SET status = $2, attempts = $3, last_error = $4 \
                     WHERE id = $1",
                )
                .bind(row.id)
                .bind(status)
                .bind(attempts)
                .bind(error.message)
                .execute(pool)
                .await?;
                if exhausted {
                    report.failed += 1;
                } else {
                    report.retried += 1;
                }
            }
        }
    }
    Ok(report)
}
