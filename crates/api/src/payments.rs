//! Payments: method availability, gateway webhook callbacks, retry,
//! and the dev simulator helper (Wave 5).
//!
//! Honesty rules for this module: a payment only becomes `paid` when
//! a signature-verified gateway callback says so with the exact
//! amount. Real gateway signature verification is NOT implemented —
//! it cannot be, without each gateway's merchant credentials and
//! integration documentation — so callbacks for non-dev payments are
//! logged and rejected (401) rather than trusted. The DEV simulator's
//! callbacks are verified with the HMAC scheme in the `payments`
//! crate, the single implementation shared by signer and verifier.
//! COD orders have no payment row at all: their
//! `orders.payment_status` stays `pay_on_delivery`.
//!
//! Wave 17: a COD order can carry ONE payment row after all —
//! its advance (see the checkout block in the `orders` module).
//! The advance settles through this same callback path: the
//! branch inside [`apply_payment_callback`] recognises an
//! advance payment (a COD order whose required advance equals
//! the payment amount, still `advance_pending`) and moves the
//! order to `advance_paid` with `advance_paid_poisha` set —
//! never to `paid`, because the balance is still collected in
//! cash on delivery. A failed advance attempt leaves the order
//! `advance_pending`: the advance is still owed, and the buyer
//! retries the same payment row.

use axum::{
    extract::{Path, State},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{can_transition_payment, validate_callback_amount};
use payments::{
    is_dev_reference, parse_payment_method, sign_dev_callback, verify_dev_callback,
    PaymentMethod, PaymentsConfig,
};
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One payment attempt against an order (a row of `payments`).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Payment {
    pub id: Uuid,
    pub order_id: Uuid,
    pub method: String,
    pub amount_poisha: i64,
    pub status: String,
    pub gateway_ref: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub paid_at: Option<DateTime<Utc>>,
    /// Wave 41 (audit HI-52): the running total of return
    /// refunds recorded against this payment by the admin
    /// refund-done path (migration 0037). Zero until a refund
    /// is recorded; the status flips to `refunded` (and
    /// `refunded_at` is stamped) only when the order is fully
    /// refunded — see the `returns` module.
    pub refund_amount_poisha: i64,
    pub refunded_at: Option<DateTime<Utc>>,
}

/// The latest payment attempt, embedded in the order view.
#[derive(Debug, Clone, Serialize)]
pub struct PaymentSummary {
    pub id: Uuid,
    pub method: String,
    pub status: String,
    pub gateway_ref: Option<String>,
    /// Local dev-simulator redirect path, present only for payments
    /// the DEV simulator created (their refs start with `DEV-`).
    pub dev_redirect_url: Option<String>,
}

/// Availability of one payment method, as the checkout page renders
/// it. The API is the single source of truth for availability — the
/// web app never decides this itself.
#[derive(Debug, Serialize)]
pub struct MethodAvailability {
    pub method: String,
    pub available: bool,
    /// `cod` for cash on delivery, `live` when merchant credentials
    /// are present, `dev` when the DEV simulator would serve it.
    pub mode: Option<String>,
    /// Machine-readable reason when unavailable (`not_configured`).
    pub reason: Option<String>,
}

const PAYMENT_COLUMNS: &str = "id, order_id, method, amount_poisha, status, gateway_ref, \
    created_at, updated_at, paid_at, refund_amount_poisha, refunded_at";

// ---------------------------------------------------------------------------
// Loaders + writers (shared by checkout, webhook, retry and simulate)
// ---------------------------------------------------------------------------

/// Load one payment attempt by id.
pub async fn load_payment(pool: &PgPool, payment_id: Uuid) -> Result<Payment, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM payments WHERE id = $1"
    ))
    .bind(payment_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load payment", e))?
    .ok_or_else(|| ApiError::not_found("payment not found"))
}

/// The most recent payment attempt for an order, as a summary for the
/// order view (with the dev redirect derived from a `DEV-` ref).
pub async fn load_latest_payment_summary(
    pool: &PgPool,
    order_id: Uuid,
) -> Result<Option<PaymentSummary>, ApiError> {
    let row: Option<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, method, status, gateway_ref FROM payments \
         WHERE order_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load latest payment", e))?;
    Ok(row.map(|(id, method, status, gateway_ref)| {
        let dev_redirect_url = gateway_ref
            .as_deref()
            .filter(|reference| is_dev_reference(reference))
            .map(|reference| format!("/dev-pay/{reference}"));
        PaymentSummary {
            id,
            method,
            status,
            gateway_ref,
            dev_redirect_url,
        }
    }))
}

/// Is this database error a unique-constraint violation
/// (Postgres SQLSTATE 23505)? Wave 41 uses it to tell an honest
/// conflict (audit HI-53's uniqueness rules) apart from a real
/// storage failure.
fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(|db_err| db_err.code())
        .as_deref()
        == Some("23505")
}

/// Insert a `pending` payment attempt inside the caller's transaction
/// (checkout). The order row is updated separately by checkout's
/// insert, which already stores `payment_status = 'pending'`.
///
/// Wave 41 (audit HI-53): migration 0036 makes `gateway_ref`
/// unique (partial, where present) and allows at most one
/// `pending` attempt per order. A collision is therefore a
/// CONFLICT the caller can reason about — a duplicate attempt
/// against the same reference or order — never a silent second
/// settleable row, so it maps to a typed 409 instead of the
/// generic internal error.
pub async fn insert_pending_payment(
    conn: &mut PgConnection,
    order_id: Uuid,
    method: &str,
    amount_poisha: i64,
    gateway_ref: &str,
) -> Result<Uuid, ApiError> {
    sqlx::query_scalar(
        "INSERT INTO payments (order_id, method, amount_poisha, status, gateway_ref) \
         VALUES ($1, $2, $3, 'pending', $4) RETURNING id",
    )
    .bind(order_id)
    .bind(method)
    .bind(amount_poisha)
    .bind(gateway_ref)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict(
                "a pending payment already exists for this order or gateway reference \
                 (এই অর্ডার বা রেফারেন্সের জন্য ইতিমধ্যে একটি পেমেন্ট অপেক্ষমাণ আছে)",
            )
        } else {
            ApiError::internal("insert payment", e)
        }
    })
}

/// Availability of every payment method under the current config:
/// COD always; an online method only when its merchant credentials
/// are present (mode `live`) or the DEV simulator is on (mode `dev`).
pub fn payment_methods(config: &PaymentsConfig) -> Vec<MethodAvailability> {
    let mut methods = vec![MethodAvailability {
        method: PaymentMethod::Cod.as_str().to_string(),
        available: true,
        mode: Some("cod".to_string()),
        reason: None,
    }];
    for method in [
        PaymentMethod::Bkash,
        PaymentMethod::Nagad,
        PaymentMethod::SslCommerz,
        PaymentMethod::ShurjoPay,
    ] {
        let availability = if config.credentials_present(method) {
            MethodAvailability {
                method: method.as_str().to_string(),
                available: true,
                mode: Some("live".to_string()),
                reason: None,
            }
        } else if config.dev_mode {
            MethodAvailability {
                method: method.as_str().to_string(),
                available: true,
                mode: Some("dev".to_string()),
                reason: None,
            }
        } else {
            MethodAvailability {
                method: method.as_str().to_string(),
                available: false,
                mode: None,
                reason: Some("not_configured".to_string()),
            }
        };
        methods.push(availability);
    }
    methods
}

/// Can any gateway collect a COD advance right now (Wave 17)?
/// True when at least one online method has complete merchant
/// credentials, or the DEV simulator is on (it serves every
/// method). Checkout's advance decision reads this ONE rule: no
/// available gateway means the advance cannot be enforced and
/// the order degrades to flagged plain COD (see `orders`).
pub fn advance_gateway_available(config: &PaymentsConfig) -> bool {
    config.dev_mode
        || [
            PaymentMethod::Bkash,
            PaymentMethod::Nagad,
            PaymentMethod::SslCommerz,
            PaymentMethod::ShurjoPay,
        ]
        .iter()
        .any(|method| config.credentials_present(*method))
}

/// The public COD advance policy (Wave 17): the EFFECTIVE rate
/// and threshold checkout will apply (admin setting over startup
/// value over built-in default — the `admin` module's readers).
/// Public because checkout shows it to buyers before they order;
/// it carries no secrets, only the platform's published terms.
/// A buyer's personal risk-forced advance is NOT visible here —
/// only the server knows a buyer's history.
#[derive(Debug, Serialize)]
pub struct AdvancePolicy {
    pub percent_bps: u32,
    pub threshold_poisha: i64,
}

/// Read the effective advance policy.
pub async fn advance_policy_view(state: &AppState) -> AdvancePolicy {
    AdvancePolicy {
        percent_bps: crate::admin::effective_advance_bps(state).await,
        threshold_poisha: crate::admin::effective_advance_threshold(state).await,
    }
}

// ---------------------------------------------------------------------------
// Webhook callback application — THE one code path (webhook + dev
// simulate both end here; there is no second apply implementation)
// ---------------------------------------------------------------------------

/// Apply one gateway callback to the payment it references.
///
/// The order of checks is deliberate and every received callback is
/// written to `payment_events` (with its raw body and signature
/// verdict) before any rejection, so rejected callbacks are auditable
/// too:
///
/// A callback for an unknown reference is a 404 (there is no payment
/// to attach an event to). A callback whose method segment disagrees
/// with the payment row is logged and rejected 400. A bad signature
/// is logged and rejected 401 with no state change. An amount that
/// differs from the payment amount is logged and rejected 400 with
/// no state change. A duplicate delivery of an already-applied
/// result returns the payment unchanged (200, idempotent). Any other
/// illegal move is rejected 409 by the domain transition table.
/// On success the payment and its order's `payment_status` move
/// together.
///
/// Wave 39 (security audit CR-02 in the audit's full list —
/// non-atomic settlement; CR-08 — a late webhook settling a
/// cancelled order; HI-50 — DEV references verified with dev mode
/// off):
///
/// * The audit row is written FIRST, on the pool, and commits on
///   its own — deliberately OUTSIDE the settlement transaction,
///   so a rejected callback stays auditable exactly as before.
/// * Everything that moves money state — the payment row update,
///   the order `payment_status` update and the Wave 17 advance
///   branch — then commits in ONE transaction that locks the
///   order row and the payment row (`FOR UPDATE`, in that order,
///   the same order the cancel path locks them) and re-reads the
///   payment's status under the lock. A failure anywhere in the
///   settlement leaves no partial state, and two racing callbacks
///   can no longer interleave their updates.
/// * If the locked order is `cancelled`, the callback is logged
///   (above) and the payment is returned UNCHANGED: no money
///   state moves for an order that no longer exists as a sale.
///   (The buyer-cancel path also voids the pending payment row,
///   so a settleable row should not normally survive a cancel —
///   this guard is the backstop for races and older rows.)
/// * DEV references verify ONLY while dev mode is on. With dev
///   mode off a DEV reference is unverifiable (401) — previously
///   it was checked against the configured secret anyway, which
///   for a default/empty secret meant an empty-key HMAC anyone
///   could compute.
///
/// Wave 41 (security audit HI-51 + HI-53):
///
/// * The amount that may settle is re-derived from the locked
///   ORDER inside the settlement transaction: an online payment
///   must equal the order's grand total, and a COD order can
///   only ever settle its recognised advance — a payment row
///   whose amount drifted from the order's expectation is
///   rejected (400, event already logged, nothing moves)
///   instead of marking the order `paid` for the wrong amount.
/// * The reference lookup is exact: migration 0036 makes
///   `gateway_ref` unique, so there is no newest-wins choice
///   between duplicate rows.
#[allow(clippy::too_many_arguments)]
pub async fn apply_payment_callback(
    pool: &PgPool,
    config: &PaymentsConfig,
    method: PaymentMethod,
    payment_ref: &str,
    amount_poisha: i64,
    callback_status: &str,
    signature: &str,
    raw_body: &str,
) -> Result<Payment, ApiError> {
    // Wave 41 (audit HI-53): migration 0036's partial UNIQUE
    // index guarantees at most one row carries a given
    // reference, so this lookup is exact — the pre-0036
    // newest-first ORDER BY that resolved duplicates by
    // insertion order is gone, because duplicates can no
    // longer exist.
    let payment: Payment = sqlx::query_as(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM payments WHERE gateway_ref = $1"
    ))
    .bind(payment_ref)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("find payment by gateway ref", e))?
    .ok_or_else(|| ApiError::not_found("payment not found for gateway reference"))?;

    // Signature verdict. Only DEV simulator references can be verified
    // today (HMAC scheme in the payments crate) — and only while the
    // simulator is enabled (Wave 39, audit HI-50). Real gateway
    // verification is a deliberate TODO: it needs each gateway's
    // merchant credentials and documented signature scheme, neither of
    // which exists yet, so non-dev callbacks are treated as
    // unverifiable and rejected below instead of being trusted.
    let signature_valid = match payment.gateway_ref.as_deref() {
        Some(reference) if is_dev_reference(reference) && config.dev_mode => {
            verify_dev_callback(
                &config.dev_secret,
                reference,
                amount_poisha,
                callback_status,
                signature,
            )
        }
        _ => false,
    };

    let method_matches = payment.method == method.as_str();
    let event_type = if method_matches {
        format!("webhook_{callback_status}")
    } else {
        "webhook_method_mismatch".to_string()
    };
    sqlx::query(
        "INSERT INTO payment_events (payment_id, event_type, raw_body, signature_valid) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(payment.id)
    .bind(&event_type)
    .bind(raw_body)
    .bind(signature_valid)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("log payment event", e))?;

    if !method_matches {
        return Err(ApiError::bad_request(
            "callback method does not match the payment's method",
        ));
    }
    if !signature_valid {
        return Err(ApiError::unauthorized("invalid callback signature"));
    }
    let target = match callback_status {
        "success" => "paid",
        "failed" => "failed",
        _ => {
            return Err(ApiError::bad_request(
                "callback status must be 'success' or 'failed'",
            ))
        }
    };
    if !validate_callback_amount(payment.amount_poisha, amount_poisha) {
        return Err(ApiError::bad_request(
            "callback amount does not match the payment amount",
        ));
    }

    // -- Settlement: ONE transaction, both rows locked. ----------
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin settlement transaction", e))?;

    // The order row first (the cancel path locks it first too):
    // its status decides whether any money state may move at all.
    // Wave 41: the order's grand total rides along — the HI-51
    // expectation check below is derived from the ORDER, never
    // from the payment row alone.
    let order_facts: (String, String, String, i64, i64) = sqlx::query_as(
        "SELECT status, payment_method, payment_status, advance_required_poisha, \
         grand_total_poisha \
         FROM orders WHERE id = $1 FOR UPDATE",
    )
    .bind(payment.order_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock order for settlement", e))?;
    let (
        order_status,
        order_payment_method,
        order_payment_status,
        advance_required,
        grand_total,
    ) = order_facts;

    // The payment row under the lock: the status checked here —
    // not the unlocked read above — is the one the duplicate and
    // transition rules judge.
    let locked: Payment = sqlx::query_as(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM payments WHERE id = $1 FOR UPDATE"
    ))
    .bind(payment.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock payment for settlement", e))?;

    if order_status == "cancelled" {
        // Wave 39 (audit CR-08): a late callback for a cancelled
        // order is audit-logged (above) and otherwise a no-op —
        // the payment is returned exactly as it stands.
        return Ok(locked);
    }
    if locked.status == target {
        // Duplicate delivery of an already-applied result: success,
        // no double effects.
        return Ok(locked);
    }
    if !can_transition_payment(&locked.status, target) {
        return Err(ApiError::conflict(format!(
            "payment cannot move from '{}' to '{target}'",
            locked.status
        )));
    }

    // Wave 17 branch recognition, computed here from the LOCKED
    // facts (the branch itself runs after the payment update
    // below): is this payment the ADVANCE on a COD order? The
    // rule is deliberately narrow — the order is a COD order,
    // it required an advance, this payment's amount is exactly
    // that advance, and the order is still waiting for it. A
    // full online payment can never match (online orders
    // require no advance), and neither can a stale callback for
    // an order whose advance already settled (its status has
    // moved on, and the duplicate-delivery return above has
    // already handled a repeat of the same result).
    let is_advance_payment = order_payment_method == "cod"
        && advance_required > 0
        && locked.amount_poisha == advance_required
        && order_payment_status == "advance_pending";

    // Wave 41 (audit HI-51): the amount that may settle is
    // derived from the ORDER, never trusted from the payment
    // row alone. Wave 5 already proved the callback amount
    // equals the ROW amount; what was never checked is that
    // the row still equals what the order expects — a row
    // whose amount drifted (a checkout bug, a later totals
    // change, a partial-payment row, a manual DB edit) would
    // otherwise mark the whole order `paid` for a fraction of
    // its price. The expectation by payment kind:
    //   * online order — the row must equal the order's grand
    //     total (checkout snapshots exactly that into both);
    //   * COD order — the ONLY payment a COD order can carry
    //     is its advance, so the settlement must be the
    //     recognised advance above; anything else (a drifted
    //     amount, an order no longer `advance_pending`) is
    //     rejected outright instead of falling through to a
    //     generic `paid` the courier would collect ৳0 against.
    // The rejection is a 400 like the row-vs-callback mismatch
    // above; the callback stays in `payment_events` (written
    // before this transaction) and no money state moves.
    if order_payment_method == "cod" {
        if !is_advance_payment {
            return Err(ApiError::bad_request(
                "payment amount does not match the advance this order requires \
                 (পেমেন্টের পরিমাণ এই অর্ডারের প্রয়োজনীয় অগ্রিমের সঙ্গে মিলছে না)",
            ));
        }
    } else if locked.amount_poisha != grand_total {
        return Err(ApiError::bad_request(
            "payment amount does not match the order total \
             (পেমেন্টের পরিমাণ অর্ডারের মোট মূল্যের সঙ্গে মিলছে না)",
        ));
    }

    let paid_at: Option<DateTime<Utc>> = if target == "paid" {
        Some(Utc::now())
    } else {
        None
    };
    sqlx::query(
        "UPDATE payments SET status = $2, paid_at = COALESCE($3, paid_at), \
         updated_at = now() WHERE id = $1",
    )
    .bind(locked.id)
    .bind(target)
    .bind(paid_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("update payment status", e))?;

    // Wave 17 branch: apply the advance recognition computed
    // above from the locked facts (Wave 41's HI-51 check has
    // already rejected every COD settlement that is NOT this
    // recognised advance, so the branches below only ever see
    // a legitimate advance or a full online payment).
    if target == "paid" && is_advance_payment {
        // The advance settled; the order is NOT paid — the
        // balance is still COD. Record what was collected so the
        // receipt and the courier's collection both net it out.
        sqlx::query(
            "UPDATE orders SET payment_status = 'advance_paid', \
             advance_paid_poisha = $2 WHERE id = $1",
        )
        .bind(locked.order_id)
        .bind(locked.amount_poisha)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("update order advance status", e))?;
    } else if target == "failed" && is_advance_payment {
        // The advance ATTEMPT failed; the advance itself is
        // still owed, so the order stays `advance_pending` (the
        // buyer retries the same payment row) instead of wearing
        // a bare `failed` that would hide the obligation.
    } else {
        sqlx::query("UPDATE orders SET payment_status = $2 WHERE id = $1")
            .bind(locked.order_id)
            .bind(target)
            .execute(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("update order payment status", e))?;
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit settlement", e))?;

    load_payment(pool, locked.id).await
}

// ---------------------------------------------------------------------------
// Retry + dev simulate (buyer-facing service functions)
// ---------------------------------------------------------------------------

/// Check the caller owns the payment's order. Another buyer's payment
/// is a 404, the same no-leak rule as the order endpoints.
async fn load_payment_for_owner(
    pool: &PgPool,
    user: &AuthUser,
    payment_id: Uuid,
) -> Result<Payment, ApiError> {
    let payment = load_payment(pool, payment_id).await?;
    let owner: Uuid = sqlx::query_scalar("SELECT buyer_user_id FROM orders WHERE id = $1")
        .bind(payment.order_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("load payment order owner", e))?;
    if owner != user.user_id {
        return Err(ApiError::not_found("payment not found"));
    }
    Ok(payment)
}

/// Retry a failed payment: `failed` returns to `pending` (the domain
/// transition table's only way back), reusing the same gateway
/// reference, and the order's `payment_status` follows.
///
/// Wave 39 (security audit HI-07 + CR-08): the two updates used to
/// run as separate unlocked statements, so a retry racing a
/// settlement could overwrite the order's settled `paid` back to
/// `pending`, and a retry on a cancelled order revived its money
/// state. Both updates now commit in ONE transaction that locks
/// the order row and the payment row (in that order — the same
/// order the settlement and cancel paths lock them), re-checks
/// the payment's status under the lock, and refuses outright
/// when the order is cancelled.
pub async fn retry_payment_for_buyer(
    pool: &PgPool,
    user: &AuthUser,
    payment_id: Uuid,
) -> Result<Payment, ApiError> {
    let payment = load_payment_for_owner(pool, user, payment_id).await?;
    if payment.status != "failed" || !can_transition_payment(&payment.status, "pending") {
        return Err(ApiError::conflict(
            "only a failed payment can be retried",
        ));
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin retry transaction", e))?;

    let order_facts: (String, String, i64) = sqlx::query_as(
        "SELECT status, payment_method, advance_required_poisha \
         FROM orders WHERE id = $1 FOR UPDATE",
    )
    .bind(payment.order_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock order for retry", e))?;
    let (order_status, order_payment_method, advance_required) = order_facts;

    let locked_status: String =
        sqlx::query_scalar("SELECT status FROM payments WHERE id = $1 FOR UPDATE")
            .bind(payment.id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("lock payment for retry", e))?;
    if locked_status != "failed" || !can_transition_payment(&locked_status, "pending") {
        return Err(ApiError::conflict(
            "only a failed payment can be retried",
        ));
    }
    if order_status == "cancelled" {
        return Err(ApiError::conflict(
            "অর্ডারটি বাতিল করা হয়েছে — এই পেমেন্ট আবার চেষ্টা করা যাবে না \
             (the order is cancelled — this payment can no longer be retried)",
        ));
    }

    sqlx::query("UPDATE payments SET status = 'pending', updated_at = now() WHERE id = $1")
        .bind(payment.id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("retry payment", e))?;
    // Wave 17: retrying an ADVANCE payment restores the order to
    // `advance_pending` (the advance is owed again), not the
    // generic `pending` — the same recognition rule as the
    // callback branch: a COD order whose required advance equals
    // this payment's amount. The facts come from the LOCKED order
    // row above.
    let order_payment_status = if order_payment_method == "cod"
        && advance_required > 0
        && advance_required == payment.amount_poisha
    {
        "advance_pending"
    } else {
        "pending"
    };
    sqlx::query("UPDATE orders SET payment_status = $2 WHERE id = $1")
        .bind(payment.order_id)
        .bind(order_payment_status)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("retry order payment status", e))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit retry", e))?;

    load_payment(pool, payment.id).await
}

/// DEV ONLY: simulate a successful gateway callback for one of the
/// caller's dev payments. Builds and signs the callback exactly as
/// the dev gateway would, then applies it through the SAME
/// [`apply_payment_callback`] the webhook endpoint uses. Returns 404
/// whenever dev mode is off, so in a non-dev deployment the route
/// behaves as if it did not exist.
pub async fn dev_simulate_success(
    pool: &PgPool,
    config: &PaymentsConfig,
    user: &AuthUser,
    payment_id: Uuid,
) -> Result<Payment, ApiError> {
    if !config.dev_mode {
        return Err(ApiError::not_found("payment not found"));
    }
    let payment = load_payment_for_owner(pool, user, payment_id).await?;
    let reference = payment
        .gateway_ref
        .clone()
        .filter(|r| is_dev_reference(r))
        .ok_or_else(|| ApiError::bad_request("payment was not created by the dev simulator"))?;
    let method = parse_payment_method(&payment.method)
        .ok_or_else(|| ApiError::bad_request("unknown payment method on payment"))?;
    let signature = sign_dev_callback(
        &config.dev_secret,
        &reference,
        payment.amount_poisha,
        "success",
    );
    let raw_body = serde_json::json!({
        "payment_ref": reference,
        "amount_poisha": payment.amount_poisha,
        "status": "success",
        "simulated": true,
    })
    .to_string();
    apply_payment_callback(
        pool,
        config,
        method,
        &reference,
        payment.amount_poisha,
        "success",
        &signature,
        &raw_body,
    )
    .await
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn list_methods(State(state): State<AppState>) -> Json<Vec<MethodAvailability>> {
    Json(payment_methods(&state.payments_config))
}

pub async fn advance_policy(State(state): State<AppState>) -> Json<AdvancePolicy> {
    Json(advance_policy_view(&state).await)
}

#[derive(serde::Deserialize)]
pub struct WebhookBody {
    pub payment_ref: String,
    pub amount_poisha: i64,
    pub status: String,
    pub signature: String,
}

pub async fn webhook(
    State(state): State<AppState>,
    Path(method_segment): Path<String>,
    Json(body): Json<WebhookBody>,
) -> Result<Json<Payment>, ApiError> {
    let pool = state.require_pool()?;
    let method = parse_payment_method(&method_segment).ok_or_else(|| {
        ApiError::bad_request(format!("unknown payment method '{method_segment}'"))
    })?;
    let raw_body = serde_json::json!({
        "payment_ref": body.payment_ref,
        "amount_poisha": body.amount_poisha,
        "status": body.status,
        "signature": body.signature,
    })
    .to_string();
    let payment = apply_payment_callback(
        pool,
        &state.payments_config,
        method,
        &body.payment_ref,
        body.amount_poisha,
        &body.status,
        &body.signature,
        &raw_body,
    )
    .await?;
    Ok(Json(payment))
}

pub async fn retry(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Payment>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(retry_payment_for_buyer(pool, &user, id).await?))
}

pub async fn dev_simulate(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Payment>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        dev_simulate_success(pool, &state.payments_config, &user, id).await?,
    ))
}
