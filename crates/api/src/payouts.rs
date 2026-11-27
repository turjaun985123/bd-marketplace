//! Seller dashboard, earnings and payout requests (Wave 9).
//!
//! The money rules, each enforced by exactly one function below:
//! - Earnings: a store earns from its DELIVERED sub-orders only —
//!   the sum of the `seller_payout_poisha` snapshots taken at
//!   checkout (`seller_earnings`). Wave 9 originally recomputed
//!   goods minus commission here; Wave 14 switched the aggregate
//!   to the snapshot column, because checkout now also nets the
//!   coupon discount out of the payout (goods − discount −
//!   commission) and a recompute would silently pay sellers for
//!   discounts they bore. For pre-coupon orders the two formulas
//!   are identical, so the switch changes nothing historically.
//!   The delivery charge share is courier and
//!   platform money and never earns; a `partial_delivered` shipment
//!   does not make its sub-order `delivered`, so it earns nothing
//!   until the sub-order itself is fully delivered. Conservative on
//!   purpose: money is only counted once it is certainly the
//!   seller's.
//! - Balance: available = earned − paid out (payouts in `approved`
//!   or `paid`) − pending (payouts still `requested`) − charges
//!   (the seller's `seller_charges` rows — Wave 26 promotion
//!   purchases), computed by the pure domain function plus the
//!   charges term and clamped at zero at this boundary (see
//!   `available_balance`). Charges belong to the SELLER, not to
//!   one store: a promotion may target a product in any of the
//!   seller's stores while money settles through the one payout
//!   store, so the aggregate subtracts the store owner's charges
//!   whichever store it summarises. Before Wave 26 no charges
//!   existed, so the term was zero and the equation was exactly
//!   Wave 9's.
//! - Payout requests: the seller must first save a payout account
//!   (validated by the domain), and the amount must satisfy the
//!   domain minimum (500 taka) and fit the available balance. The
//!   payout row SNAPSHOTS the account fields, so editing the
//!   account later never rewrites where an earlier payout goes.
//! - Lifecycle: sellers create `requested` payouts here. Approval,
//!   rejection and payment are platform operations driven by the
//!   admin endpoints (Wave 10, `admin` module) through the same
//!   domain transition table (`can_transition_payout`).
//! - KYC gate (Wave 21): when the `kyc_payout_gate` platform
//!   setting is on, payout creation first demands a verified
//!   store (at least the `basic` tier — see the `verification`
//!   module). The gate defaults to off, and with it off every
//!   rule above behaves exactly as it did in Wave 9.
//! - Idempotency (Wave 44): a payout request may carry a
//!   client `Idempotency-Key` header (validated by
//!   `domain::validate_idempotency_key`, stored on the row,
//!   unique per store among keyed rows — migration 0038).
//!   A retry with the same key and the same amount returns
//!   the ORIGINAL payout instead of creating a duplicate;
//!   the same key with a different amount is a 409. No key
//!   means exactly the pre-Wave-44 behaviour. This is the
//!   third element of the audit's payout prescription
//!   (CR-10), deferred by Wave 39 until a client contract
//!   existed; see [`request_payout`] for the race structure.
//!
//! Scoping: every query starts from the authenticated user. The
//! payout store is the seller's earliest-created store
//! (`stores::get_store_by_owner`; a user without a store gets a
//! 404). Counts and the recent-orders list describe the seller's
//! whole selling activity across their stores — the same scope as
//! the seller orders page, whose shared list function the dashboard
//! reuses — while earnings and payouts are strictly per payout
//! store, because money must be exact per store.
//!
//! Wave 34 layers team access over this: a store named by
//! `?store_id=` resolves through the caller's role in it — the
//! dashboard, balance and payout list read it under
//! `view_finances`, while the payout account and payout requests
//! stay `manage_payouts`, a right only the owner holds. The
//! payout store itself never resolves for a member.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    compute_available_balance, validate_idempotency_key, validate_payout_account,
    validate_payout_amount_with_min,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::access::StoreScopeQuery;
use crate::auth::AuthUser;
use crate::orders::SellerSubOrder;
use crate::products::clamp_pagination;
use crate::stores::{get_store_by_owner, Store};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// A store's money summary: what it earned from delivered
/// sub-orders, what is already paid or committed out, what sits in
/// requested payouts, what the seller has spent from the balance
/// (Wave 26 charges), and what can still be requested.
#[derive(Debug, Clone, Serialize)]
pub struct Earnings {
    pub earned_poisha: i64,
    /// Payouts in `approved` (committed) or `paid` (gone).
    pub paid_out_poisha: i64,
    /// Payouts still in `requested`, waiting for the platform.
    pub pending_poisha: i64,
    /// The seller's `seller_charges` total (Wave 26): money
    /// taken from the balance outside the payout flow —
    /// promotion purchases. Zero until the seller buys one.
    pub charges_total_poisha: i64,
    /// earned − paid out − pending − charges, clamped at zero.
    pub available_poisha: i64,
}

/// Sub-order counts per fulfilment status (all six, zero-filled).
#[derive(Debug, Serialize)]
pub struct SubOrderStatusCounts {
    pub placed: i64,
    pub confirmed: i64,
    pub shipped: i64,
    pub delivered: i64,
    pub cancelled: i64,
    pub returned: i64,
}

/// The dashboard's count block.
#[derive(Debug, Serialize)]
pub struct SellerCounts {
    pub products: i64,
    pub sub_orders_by_status: SubOrderStatusCounts,
}

/// Everything the seller home page needs in one response.
#[derive(Debug, Serialize)]
pub struct SellerDashboard {
    pub store: Store,
    pub counts: SellerCounts,
    pub earnings: Earnings,
    pub recent_sub_orders: Vec<SellerSubOrder>,
    /// Products at or below their low-stock threshold
    /// (Wave 23), worst first, at most ten — the dashboard's
    /// alert strip. Empty when nothing needs attention.
    pub low_stock_items: Vec<crate::inventory::LowStockItem>,
    /// Product questions across the seller's stores that
    /// still need an answer (Wave 25). Hidden questions are
    /// excluded — hiding is how a seller deals with spam, so
    /// a hidden question must not keep counting as work.
    pub unanswered_questions: i64,
    /// The fulfillment center's tallies over the dashboard's
    /// scope (Wave 40): how much work is waiting in each
    /// bucket and how much of it has breached its SLA — the
    /// dashboard card that links into the queue. Computed by
    /// the ONE counts function the fulfillment list itself
    /// uses, so the card and the queue always agree.
    pub fulfillment: crate::fulfillment::FulfillmentCounts,
}

/// Where a store's payout money goes; also a `payout_accounts` row.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct PayoutAccount {
    pub id: Uuid,
    pub store_id: Uuid,
    pub method: String,
    pub account_name: String,
    pub account_number: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One payout request, with the account snapshot taken at request
/// time; also a `payouts` row.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Payout {
    pub id: Uuid,
    pub store_id: Uuid,
    pub amount_poisha: i64,
    pub method: String,
    pub account_name: String,
    pub account_number: String,
    pub status: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
    /// The client's `Idempotency-Key` when the request carried
    /// one (Wave 44); `None` for keyless requests, which keep
    /// the pre-Wave-44 semantics. Echoed so a seller (or a
    /// support lookup) can match a retry to the row it made.
    pub idempotency_key: Option<String>,
}

/// A page of payout requests plus the total count.
#[derive(Debug, Serialize)]
pub struct PayoutList {
    pub items: Vec<Payout>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

// ---------------------------------------------------------------------------
// Service functions
// ---------------------------------------------------------------------------

/// The caller's payout store: their earliest-created store, or a
/// 404 with a clear message when they have none. Every handler in
/// this module starts here, so "no store" behaves identically
/// everywhere.
async fn seller_store(pool: &PgPool, user: &AuthUser) -> Result<Store, ApiError> {
    get_store_by_owner(pool, user.user_id).await
}

/// The available balance for an earnings summary: the raw domain
/// arithmetic (earned − paid out − pending) minus the seller's
/// charges, clamped at zero. With consistent inputs the raw
/// value is never negative (a payout — and, since Wave 26, a
/// promotion — can only be charged against the available
/// balance), so the clamp only guards the display against
/// inconsistent history.
pub fn available_balance(earnings: &Earnings) -> i64 {
    (compute_available_balance(
        earnings.earned_poisha,
        earnings.paid_out_poisha,
        earnings.pending_poisha,
    ) - earnings.charges_total_poisha)
        .max(0)
}

/// THE earnings aggregate for a store — dashboard, balance and
/// payout creation all read money through this one function, so
/// the numbers can never disagree between endpoints. Runs on
/// any executor so the promotion purchase (Wave 26) can read
/// the SAME aggregate inside its transaction, under the payout
/// store's row lock, instead of racing a pool read against its
/// own charge insert.
pub async fn seller_earnings_on<'e, E>(executor: E, store_id: Uuid) -> Result<Earnings, ApiError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let row: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT COALESCE(SUM(seller_payout_poisha), 0)::bigint \
              FROM sub_orders \
             WHERE store_id = $1 AND status = 'delivered') AS earned_poisha, \
           (SELECT COALESCE(SUM(amount_poisha), 0)::bigint \
              FROM payouts \
             WHERE store_id = $1 AND status IN ('approved', 'paid')) AS paid_out_poisha, \
           (SELECT COALESCE(SUM(amount_poisha), 0)::bigint \
              FROM payouts \
             WHERE store_id = $1 AND status = 'requested') AS pending_poisha, \
           (SELECT COALESCE(SUM(amount_poisha), 0)::bigint \
              FROM seller_charges \
             WHERE seller_user_id = \
                   (SELECT owner_user_id FROM stores WHERE id = $1)) \
             AS charges_total_poisha",
    )
    .bind(store_id)
    .fetch_one(executor)
    .await
    .map_err(|e| ApiError::internal("load seller earnings", e))?;

    let mut earnings = Earnings {
        earned_poisha: row.0,
        paid_out_poisha: row.1,
        pending_poisha: row.2,
        charges_total_poisha: row.3,
        available_poisha: 0,
    };
    earnings.available_poisha = available_balance(&earnings);
    Ok(earnings)
}

/// [`seller_earnings_on`] on the pool — the form every
/// non-transactional caller uses.
pub async fn seller_earnings(pool: &PgPool, store_id: Uuid) -> Result<Earnings, ApiError> {
    seller_earnings_on(pool, store_id).await
}

/// THE available-balance read for a seller: resolve the
/// seller's payout store (their earliest store — 404 when they
/// have none, exactly like the balance endpoint) and read the
/// one earnings aggregate for it. Dashboard, balance, payout
/// creation and promotion purchase all end at this number or
/// at [`seller_earnings_on`] over the same store, so the
/// balance a seller sees is the balance every money gate
/// enforces.
pub async fn available_balance_poisha(pool: &PgPool, user_id: Uuid) -> Result<i64, ApiError> {
    let store = get_store_by_owner(pool, user_id).await?;
    let earnings = seller_earnings(pool, store.id).await?;
    Ok(earnings.available_poisha)
}

/// The seller dashboard: payout store, activity counts, earnings,
/// and the five most recent sub-orders (the shared seller list).
/// The default scope is the caller's whole selling activity —
/// every store they own — with earnings read for the payout
/// store, exactly as before Wave 34.
pub async fn get_dashboard(
    pool: &PgPool,
    user: &AuthUser,
    sla: &crate::fulfillment::FulfillmentSla,
) -> Result<SellerDashboard, ApiError> {
    let store = seller_store(pool, user).await?;
    let owned = crate::access::owned_store_ids(pool, user.user_id).await?;
    build_dashboard(pool, store, &owned, sla).await
}

/// The Wave 34 store-scoped dashboard: everything (counts,
/// earnings, recents, alerts) describes ONE store — the one
/// the handler resolved through the caller's role in it
/// (any role may view a dashboard for a store they serve).
pub async fn get_dashboard_for_store(
    pool: &PgPool,
    store: Store,
    sla: &crate::fulfillment::FulfillmentSla,
) -> Result<SellerDashboard, ApiError> {
    let scope = [store.id];
    build_dashboard(pool, store, &scope, sla).await
}

/// The dashboard assembly shared by both scopes: counts and
/// alerts over `scope`, earnings over `store` itself. The
/// Wave 40 fulfillment tallies are computed over the same
/// `scope` by the fulfillment module's one counts function,
/// judged against the SLA pair the handler resolved.
async fn build_dashboard(
    pool: &PgPool,
    store: Store,
    scope: &[Uuid],
    sla: &crate::fulfillment::FulfillmentSla,
) -> Result<SellerDashboard, ApiError> {
    let products: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM products p WHERE p.store_id = ANY($1)",
    )
    .bind(scope)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count seller products", e))?;

    let status_rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT so.status, COUNT(*) FROM sub_orders so \
         WHERE so.store_id = ANY($1) GROUP BY so.status",
    )
    .bind(scope)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("count seller sub-orders", e))?;
    let mut by_status = SubOrderStatusCounts {
        placed: 0,
        confirmed: 0,
        shipped: 0,
        delivered: 0,
        cancelled: 0,
        returned: 0,
    };
    for (status, count) in status_rows {
        match status.as_str() {
            "placed" => by_status.placed = count,
            "confirmed" => by_status.confirmed = count,
            "shipped" => by_status.shipped = count,
            "delivered" => by_status.delivered = count,
            "cancelled" => by_status.cancelled = count,
            "returned" => by_status.returned = count,
            _ => {}
        }
    }

    let earnings = seller_earnings(pool, store.id).await?;
    let recent =
        crate::orders::list_seller_sub_orders_scoped(pool, scope, Some(1), Some(5)).await?;
    let low_stock_items = crate::inventory::low_stock_items_for_seller(pool, scope).await?;
    let unanswered_questions = crate::qna::count_unanswered_questions(pool, scope).await?;
    let fulfillment = crate::fulfillment::fulfillment_counts(pool, scope, sla).await?;

    Ok(SellerDashboard {
        store,
        counts: SellerCounts {
            products,
            sub_orders_by_status: by_status,
        },
        earnings,
        recent_sub_orders: recent.items,
        low_stock_items,
        unanswered_questions,
        fulfillment,
    })
}

/// The balance endpoint's whole job: the caller's store earnings.
pub async fn get_balance(pool: &PgPool, user: &AuthUser) -> Result<Earnings, ApiError> {
    let store = seller_store(pool, user).await?;
    seller_earnings(pool, store.id).await
}

/// One store's payout account, or `None` when none is saved
/// yet. The store arrives resolved by the handler (Wave 34:
/// the owner's own store by default; a store named by
/// `?store_id=` only ever resolves for its OWNER here —
/// payout coordinates are `manage_payouts`, which no member
/// role carries).
pub async fn get_payout_account(
    pool: &PgPool,
    store: &Store,
) -> Result<Option<PayoutAccount>, ApiError> {
    sqlx::query_as(
        "SELECT id, store_id, method, account_name, account_number, created_at, updated_at \
         FROM payout_accounts WHERE store_id = $1",
    )
    .bind(store.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load payout account", e))
}

/// Create or replace a store's payout account. The domain
/// validator decides what a legal account is per method; the
/// stored fields are trimmed copies of what the seller sent.
pub async fn save_payout_account(
    pool: &PgPool,
    store: &Store,
    method: &str,
    account_name: &str,
    account_number: &str,
) -> Result<PayoutAccount, ApiError> {
    validate_payout_account(method, account_name, account_number)
        .map_err(ApiError::bad_request)?;

    sqlx::query_as(
        "INSERT INTO payout_accounts (store_id, method, account_name, account_number) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (store_id) DO UPDATE SET \
           method = EXCLUDED.method, \
           account_name = EXCLUDED.account_name, \
           account_number = EXCLUDED.account_number, \
           updated_at = now() \
         RETURNING id, store_id, method, account_name, account_number, created_at, updated_at",
    )
    .bind(store.id)
    .bind(method)
    .bind(account_name.trim())
    .bind(account_number.trim())
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("save payout account", e))
}

/// The outcome of ONE attempt at [`request_payout`]'s
/// transaction (Wave 44). `Done` carries the payout to return
/// and whether THIS attempt created it (`false` = the request
/// was an idempotent replay of an earlier one, and the handler
/// answers 200 instead of 201). `LostIdempotencyRace` means
/// the INSERT hit the partial unique index (SQLSTATE 23505):
/// a concurrent request with the same key committed first.
/// The attempt's transaction is dropped uncommitted, so the
/// caller retries the WHOLE operation once — the retry's
/// lookup then finds the winner's row and applies the same
/// replay/conflict rules, instead of patching a half-finished
/// transaction back together.
enum PayoutAttempt {
    // Boxed: the unit variant would otherwise inflate every
    // value of this enum to the size of a full `Payout`
    // (clippy::large_enum_variant).
    Done(Box<Payout>, bool),
    LostIdempotencyRace,
}

/// True when a SQLx error is a Postgres unique violation (23505).
fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .map(|db_err| db_err.code().as_deref() == Some("23505"))
        .unwrap_or(false)
}

/// Request a payout of `amount_poisha` from a store's available
/// balance. Requires a saved payout account (its fields are
/// snapshotted onto the payout row); the domain amount validator
/// enforces the platform minimum and the balance fit. The minimum
/// is the EFFECTIVE one (Wave 10): an admin-set platform setting
/// wins over the built-in [`domain::MIN_PAYOUT_POISHA`].
///
/// Wave 34: the store arrives resolved by the handler under
/// `manage_payouts` — a right only the owner holds — so this
/// function never runs for a member.
///
/// Wave 44: `idempotency_key` is the client's optional
/// `Idempotency-Key` (already validated by the handler against
/// `domain::validate_idempotency_key`). The return is the
/// payout plus whether this call created it. With a key, the
/// attempt's transaction first looks the key up for this
/// store: an existing row with the SAME amount is returned as
/// a replay (created = false) without touching the balance —
/// the request body carries only the amount (the account
/// fields are a server-side snapshot, not part of the
/// request's identity), so amount equality is the whole
/// comparison — and an existing row with a DIFFERENT amount
/// is a 409, because reusing a key for another request is a
/// client bug the seller must hear about, not a retry. With
/// no key, nothing is looked up and behaviour is exactly the
/// pre-Wave-44 semantics.
pub async fn request_payout(
    pool: &PgPool,
    state: &AppState,
    store: &Store,
    amount_poisha: i64,
    idempotency_key: Option<&str>,
) -> Result<(Payout, bool), ApiError> {
    // Wave 21 — the KYC payout gate: when the platform has
    // turned the `kyc_payout_gate` setting on, an unverified
    // store cannot move money out until its seller verifies
    // (NID review, at least the `basic` tier). The gate is
    // checked FIRST, before the account and balance rules, so
    // an unverified seller always hears the one thing they must
    // fix. With the gate off (the default) this block never
    // fires and behaviour is exactly Wave 9's.
    if crate::admin::effective_kyc_payout_gate(state).await
        && store.verified_tier == "unverified"
    {
        return Err(ApiError::bad_request(
            "payouts require a verified store: verify your store first \
             (submit your NID for verification) — \
             পে-আউট পেতে আগে স্টোর যাচাইকরণ সম্পন্ন করুন",
        ));
    }

    let min_payout_poisha = crate::admin::effective_min_payout(state).await;

    // One attempt, plus exactly ONE retry after a lost
    // unique-index race (see `PayoutAttempt`). The retry is
    // a fresh transaction from the top, so its idempotency
    // lookup re-reads the winner's committed row. Losing
    // twice should be impossible — the only inserter holds
    // the store lock the lookup serializes on — but if it
    // ever happened, a 409 naming the key is the honest exit:
    // the key IS taken, and the seller can see the payout in
    // their list.
    let mut attempt = 0;
    loop {
        attempt += 1;
        match request_payout_once(pool, store, amount_poisha, min_payout_poisha, idempotency_key)
            .await?
        {
            PayoutAttempt::Done(payout, created) => return Ok((*payout, created)),
            PayoutAttempt::LostIdempotencyRace => {
                if attempt == 2 {
                    return Err(ApiError::conflict(
                        "এই Idempotency-Key দিয়ে ইতিমধ্যে একটি পে-আউট তৈরি হয়েছে — \
                         পে-আউট তালিকা থেকে যাচাই করুন (a payout with this \
                         Idempotency-Key already exists — check the payout list)",
                    ));
                }
            }
        }
    }
}

/// ONE attempt at the payout transaction: the Wave 39 body
/// (store lock, account read, earnings, insert) with the
/// Wave 44 idempotency lookup in front of the balance rules
/// and the key stored on the insert.
async fn request_payout_once(
    pool: &PgPool,
    store: &Store,
    amount_poisha: i64,
    min_payout_poisha: i64,
    idempotency_key: Option<&str>,
) -> Result<PayoutAttempt, ApiError> {
    // Wave 39 (security audit CR-10 in the audit's full list —
    // the payout double-spend race): the account read, the
    // earnings computation and the INSERT used to be three
    // separate pool operations with no lock, so two concurrent
    // requests could both read the same available balance, both
    // pass the balance check, and together pay out more than the
    // seller had earned. They now run in ONE transaction that
    // first locks the store row FOR UPDATE — the exact pattern
    // the promotion and subscription charges already use — so
    // concurrent payout requests for a store serialize on that
    // row and each one re-reads earnings that already include
    // every earlier request's pending amount.
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin payout request", e))?;
    sqlx::query("SELECT id FROM stores WHERE id = $1 FOR UPDATE")
        .bind(store.id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("lock store for payout request", e))?;

    // Wave 44: the idempotency lookup runs INSIDE the
    // transaction, after the store lock — so for requests
    // through this path the lookup already sees every
    // committed same-store payout, and a concurrent same-key
    // request resolves as a replay here rather than at the
    // unique index. The index (and the one retry in
    // `request_payout`) is the backstop for any path that
    // ever reaches the insert without this lock.
    if let Some(key) = idempotency_key {
        let existing: Option<Payout> = sqlx::query_as(
            "SELECT id, store_id, amount_poisha, method, account_name, account_number, \
             status, note, created_at, processed_at, idempotency_key \
             FROM payouts WHERE store_id = $1 AND idempotency_key = $2",
        )
        .bind(store.id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("look up payout idempotency key", e))?;
        if let Some(existing) = existing {
            if existing.amount_poisha != amount_poisha {
                return Err(ApiError::conflict(
                    "এই Idempotency-Key ইতিমধ্যে অন্য অঙ্কের পে-আউট অনুরোধে ব্যবহৃত \
                     হয়েছে — নতুন কী দিয়ে আবার চেষ্টা করুন (this Idempotency-Key \
                     was already used for a payout request of a different amount)",
                ));
            }
            // A true retry: hand back the original row. The
            // transaction has written nothing, so dropping it
            // (rollback) is the clean exit.
            return Ok(PayoutAttempt::Done(Box::new(existing), false));
        }
    }

    let account: Option<PayoutAccount> = sqlx::query_as(
        "SELECT id, store_id, method, account_name, account_number, created_at, updated_at \
         FROM payout_accounts WHERE store_id = $1",
    )
    .bind(store.id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("load payout account", e))?;
    let account = account.ok_or_else(|| {
        ApiError::bad_request("add a payout account first, then request a payout")
    })?;

    let earnings = seller_earnings_on(&mut *tx, store.id).await?;
    validate_payout_amount_with_min(
        amount_poisha,
        earnings.available_poisha,
        min_payout_poisha,
    )
    .map_err(ApiError::bad_request)?;

    let payout: Payout = match sqlx::query_as(
        "INSERT INTO payouts \
         (store_id, amount_poisha, method, account_name, account_number, idempotency_key) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING id, store_id, amount_poisha, method, account_name, account_number, \
                   status, note, created_at, processed_at, idempotency_key",
    )
    .bind(store.id)
    .bind(amount_poisha)
    .bind(&account.method)
    .bind(&account.account_name)
    .bind(&account.account_number)
    .bind(idempotency_key)
    .fetch_one(&mut *tx)
    .await
    {
        Ok(payout) => payout,
        Err(err) => {
            // Wave 44: a keyed insert that hits the partial
            // unique index lost the race the module docs
            // describe — report it as a lost race (the caller
            // retries once) instead of a 500. Keyless inserts
            // are outside the index, so their errors keep the
            // plain internal mapping.
            if idempotency_key.is_some() && is_unique_violation(&err) {
                return Ok(PayoutAttempt::LostIdempotencyRace);
            }
            return Err(ApiError::internal("create payout", err));
        }
    };

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit payout request", e))?;
    Ok(PayoutAttempt::Done(Box::new(payout), true))
}

/// The caller's payout requests (their payout store's),
/// newest first.
pub async fn list_payouts(
    pool: &PgPool,
    user: &AuthUser,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<PayoutList, ApiError> {
    let store = seller_store(pool, user).await?;
    list_payouts_for_store(pool, &store, page, limit).await
}

/// One store's payout requests, newest first — the shared
/// body behind [`list_payouts`] and the Wave 34 scoped list
/// (a manager with `view_finances` may READ a store's payout
/// history; requesting stays the owner's alone).
pub async fn list_payouts_for_store(
    pool: &PgPool,
    store: &Store,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<PayoutList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payouts WHERE store_id = $1")
        .bind(store.id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count payouts", e))?;

    let items: Vec<Payout> = sqlx::query_as(
        "SELECT id, store_id, amount_poisha, method, account_name, account_number, \
         status, note, created_at, processed_at, idempotency_key \
         FROM payouts WHERE store_id = $1 \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(store.id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list payouts", e))?;

    Ok(PayoutList {
        items,
        total,
        page,
        limit,
    })
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SavePayoutAccountInput {
    pub method: String,
    pub account_name: String,
    pub account_number: String,
}

#[derive(Deserialize)]
pub struct RequestPayoutInput {
    pub amount_poisha: i64,
}

#[derive(Deserialize)]
pub struct ListPayoutsQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn dashboard_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<SellerDashboard>, ApiError> {
    let pool = state.require_pool()?;
    // Wave 40: the fulfillment tallies on the dashboard are
    // judged against the SLA pair in force right now.
    let sla = crate::fulfillment::effective_fulfillment_sla(&state).await;
    // Wave 34: `?store_id=` views ONE store's dashboard —
    // any role the caller holds in it suffices (a dashboard
    // is a view); without it, the caller's own whole-activity
    // dashboard, exactly as before.
    match scope.store_id {
        Some(store_id) => {
            crate::access::seller_access(pool, user.user_id, store_id).await?;
            let store = crate::stores::get_store_by_id(pool, store_id).await?;
            Ok(Json(get_dashboard_for_store(pool, store, &sla).await?))
        }
        None => Ok(Json(get_dashboard(pool, &user, &sla).await?)),
    }
}

pub async fn balance_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<Earnings>, ApiError> {
    let pool = state.require_pool()?;
    // Wave 34: `?store_id=` reads that store's earnings
    // under `view_finances` (owner or manager); without it,
    // the caller's own payout-store balance, as before.
    match scope.store_id {
        Some(_) => {
            let (store, _role) = crate::access::resolve_scoped_store(
                pool,
                user.user_id,
                scope.store_id,
                domain::STORE_ACTION_VIEW_FINANCES,
            )
            .await?;
            Ok(Json(seller_earnings(pool, store.id).await?))
        }
        None => Ok(Json(get_balance(pool, &user).await?)),
    }
}

pub async fn get_payout_account_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<Option<PayoutAccount>>, ApiError> {
    let pool = state.require_pool()?;
    // Wave 34: payout coordinates are `manage_payouts` —
    // owner-only in the matrix, so a member naming a store
    // here gets the matrix's 403, and a stranger a 404.
    let (store, _role) = crate::access::resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        domain::STORE_ACTION_MANAGE_PAYOUTS,
    )
    .await?;
    Ok(Json(get_payout_account(pool, &store).await?))
}

pub async fn save_payout_account_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Json(input): Json<SavePayoutAccountInput>,
) -> Result<Json<PayoutAccount>, ApiError> {
    let pool = state.require_pool()?;
    let (store, _role) = crate::access::resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        domain::STORE_ACTION_MANAGE_PAYOUTS,
    )
    .await?;
    Ok(Json(
        save_payout_account(
            pool,
            &store,
            &input.method,
            &input.account_name,
            &input.account_number,
        )
        .await?,
    ))
}

pub async fn request_payout_handler(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Query(scope): Query<StoreScopeQuery>,
    Json(input): Json<RequestPayoutInput>,
) -> Result<(StatusCode, Json<Payout>), ApiError> {
    // Wave 44: the optional `Idempotency-Key` header is read
    // and validated FIRST — before the store resolution and
    // every money rule. A malformed key is a plain client
    // error: answering it before any database work means a
    // bad key can neither mask nor be masked by a store or
    // balance error, and a key the client believes it sent is
    // never silently ignored (silence would recreate the
    // duplicate the key exists to prevent). A non-ASCII
    // header value fails `to_str` and lands in the same 400.
    let idempotency_key = match headers.get("idempotency-key") {
        None => None,
        Some(value) => {
            let key = value.to_str().map_err(|_| {
                ApiError::bad_request(
                    "Idempotency-Key-এর মান সঠিক নয় — 1 থেকে 128টি ছাপাযোগ্য ASCII \
                     অক্ষর হতে হবে, স্পেস ছাড়া (invalid Idempotency-Key: 1-128 \
                     printable ASCII characters, no spaces)",
                )
            })?;
            if !validate_idempotency_key(key) {
                return Err(ApiError::bad_request(
                    "Idempotency-Key-এর মান সঠিক নয় — 1 থেকে 128টি ছাপাযোগ্য ASCII \
                     অক্ষর হতে হবে, স্পেস ছাড়া (invalid Idempotency-Key: 1-128 \
                     printable ASCII characters, no spaces)",
                ));
            }
            Some(key)
        }
    };
    let pool = state.require_pool()?;
    // Wave 34: requesting a payout is `manage_payouts` —
    // the owner alone. A manager naming the store is refused
    // with the matrix's 403 before any money rule runs.
    let (store, _role) = crate::access::resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        domain::STORE_ACTION_MANAGE_PAYOUTS,
    )
    .await?;
    let (payout, created) =
        request_payout(pool, &state, &store, input.amount_poisha, idempotency_key).await?;
    // Wave 44: a fresh creation answers 201 as always; an
    // idempotent replay answers 200 with the ORIGINAL payout
    // — the standard signal that the retry was recognised,
    // not re-executed.
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(payout)))
}

pub async fn list_payouts_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListPayoutsQuery>,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<PayoutList>, ApiError> {
    let pool = state.require_pool()?;
    // Wave 34: `?store_id=` lists that store's payouts under
    // `view_finances` (owner or manager may read); without
    // it, the caller's own payout-store list, as before.
    match scope.store_id {
        Some(_) => {
            let (store, _role) = crate::access::resolve_scoped_store(
                pool,
                user.user_id,
                scope.store_id,
                domain::STORE_ACTION_VIEW_FINANCES,
            )
            .await?;
            Ok(Json(
                list_payouts_for_store(pool, &store, query.page, query.limit).await?,
            ))
        }
        None => Ok(Json(
            list_payouts(pool, &user, query.page, query.limit).await?,
        )),
    }
}
