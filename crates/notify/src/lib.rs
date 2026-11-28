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
//!   never silently dropped. [`reap_stale_sending`]
//!   (Wave 48) recovers the one gap the claim-then-send
//!   shape leaves: rows a worker claimed and then died on,
//!   stranded in `sending`.
//!
//! All SQL is runtime-checked sqlx, like the rest of the
//! workspace (no `query!` macros).

use std::fmt;

use async_trait::async_trait;
use domain::{render_body, NotificationVars};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// Attempts after which a notification is marked `failed`
/// and no longer retried (Wave 29). An attempt is consumed
/// by a failed send — and, since Wave 48, by a claim that
/// was lost to a dead worker and recovered by
/// [`reap_stale_sending`]: the claim produced no delivery,
/// so it counts against the same budget (that is also what
/// stops a crash-looping row from being reaped and
/// re-claimed forever).
pub const MAX_ATTEMPTS: i32 = 3;

/// Default batch size for one [`process_pending`] call.
pub const DEFAULT_BATCH: i64 = 20;

/// Default age, in minutes, after which a `sending` row's
/// claim is treated as abandoned and the row is reaped
/// (Wave 48). The worker overrides it with
/// `NOTIFICATION_SENDING_STALE_MINUTES`. Ten minutes is
/// deliberately generous: a real send is one SMS HTTP call
/// measured in seconds, so a claim this old belongs to a
/// worker that is no longer coming back.
pub const DEFAULT_SENDING_STALE_MINUTES: i64 = 10;

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
/// `amount_poisha`, `status`, and (Wave 54) `product_title`.
/// Missing fields render as the domain defaults (empty /
/// none) — `render_body` has a defined output for every
/// combination.
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
        product_title: payload
            .get("product_title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        sale_title: payload
            .get("sale_title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        post_title: payload
            .get("post_title")
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
/// claim the same row, flipping them to `sending`, stamping
/// `claimed_at` (Wave 48, migration 0039), and committing
/// BEFORE any send happens — no row lock is held across a
/// network call. Each send's outcome is then written back
/// on its own row: success marks `sent` with `sent_at`;
/// failure increments `attempts`, records `last_error`,
/// and returns the row to `pending` while attempts remain,
/// or marks it `failed` at [`MAX_ATTEMPTS`]. Both
/// write-backs clear `claimed_at`, so the stamp is set
/// exactly while a claim is live.
///
/// A worker that dies mid-batch leaves its claimed rows in
/// `sending`; they are counted in the admin view (never
/// hidden) and, since Wave 48, recovered by
/// [`reap_stale_sending`], which the worker runs at the
/// top of every cycle.
pub async fn process_pending(
    pool: &PgPool,
    sender: &dyn SmsSender,
    batch: i64,
) -> Result<ProcessReport, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let claimed: Vec<ClaimedRow> = sqlx::query_as(
        "UPDATE notifications SET status = 'sending', claimed_at = now() \
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
                     SET status = 'sent', sent_at = now(), last_error = NULL, \
                         claimed_at = NULL \
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
                     SET status = $2, attempts = $3, last_error = $4, \
                         claimed_at = NULL \
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

/// What one [`reap_stale_sending`] call did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReapReport {
    /// Stale rows returned to `pending`, ready to be
    /// claimed again (by a later [`process_pending`] call —
    /// in the worker, the same cycle's).
    pub returned_to_pending: i64,
    /// Stale rows whose attempt budget the lost claim
    /// exhausted (now `failed`, kept for the admin view).
    pub marked_failed: i64,
}

impl ReapReport {
    /// How many rows this call reaped in total.
    pub fn reaped(&self) -> i64 {
        self.returned_to_pending + self.marked_failed
    }
}

/// The note written into `last_error` when the reaper
/// recovers a row, so the admin notifications view explains
/// why the row went back to `pending` (or to `failed`)
/// without any send having been recorded.
const REAPED_ERROR: &str = "reaped: worker claimed this notification but never \
    recorded an outcome (stale 'sending' — the claiming worker is assumed \
    to have died)";

/// Recover notifications stranded in `sending` by a worker
/// that died mid-batch (Wave 48, closing the Wave 29 edge).
/// The claim in [`process_pending`] commits before any send
/// happens — deliberately, so no row lock is held across a
/// network call — which means a worker that dies after
/// claiming leaves its rows in `sending` forever: nothing
/// else ever looks at that status. This is the look.
///
/// One UPDATE (a single statement, so a single transaction)
/// recovers every `sending` row whose claim — stamped into
/// `claimed_at` by the claim itself, migration 0039 — is
/// older than `stale_minutes`. A `sending` row with a NULL
/// `claimed_at` is stale by definition: the stamp has
/// existed since Wave 48, so an unstamped claim can only be
/// a pre-Wave-48 one, which is precisely the stuck
/// population this function exists for. (The reverse —
/// judging by `created_at` — would be wrong: that is the
/// EVENT's age, not the claim's, and a long-queued row
/// claimed seconds ago would be reaped out from under a
/// live worker.)
///
/// A recovered row consumes one attempt. The claim path
/// does NOT increment `attempts` (only a recorded send
/// failure does), so without this a row whose worker keeps
/// dying before any send completes would be reaped and
/// re-claimed forever at attempts 0. Counting the lost
/// claim against the same [`MAX_ATTEMPTS`] budget means a
/// stale row returns to `pending` while budget remains and
/// lands in `failed` — visible in the admin view, never
/// silently dropped and never looping — on the claim that
/// spends the last of it. The reap is recorded in
/// `last_error` either way.
///
/// Safety: a legitimate in-flight send is one SMS HTTP
/// call, measured in seconds, against a default staleness
/// floor of [`DEFAULT_SENDING_STALE_MINUTES`] minutes — a
/// false reap is practically impossible, and even one
/// could only make a later batch send the message a second
/// time. The outbox is at-least-once by design (a row is
/// never considered delivered until its send outcome is
/// written back), so the failure mode is a duplicate SMS,
/// never a lost one.
pub async fn reap_stale_sending(
    pool: &PgPool,
    stale_minutes: i64,
) -> Result<ReapReport, sqlx::Error> {
    let new_statuses: Vec<String> = sqlx::query_scalar(
        "UPDATE notifications \
         SET attempts = attempts + 1, \
             status = CASE WHEN attempts + 1 >= $3 \
                           THEN 'failed' ELSE 'pending' END, \
             last_error = $2, \
             claimed_at = NULL \
         WHERE status = 'sending' \
           AND (claimed_at IS NULL \
                OR claimed_at < now() - make_interval(mins => $1::int)) \
         RETURNING status",
    )
    .bind(stale_minutes)
    .bind(REAPED_ERROR)
    .bind(MAX_ATTEMPTS)
    .fetch_all(pool)
    .await?;
    let mut report = ReapReport::default();
    for status in new_statuses {
        match status.as_str() {
            "pending" => report.returned_to_pending += 1,
            "failed" => report.marked_failed += 1,
            _ => {}
        }
    }
    Ok(report)
}
