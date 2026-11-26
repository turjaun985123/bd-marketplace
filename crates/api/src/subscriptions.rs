//! Seller subscription plans (Wave 36).
//!
//! A store sits on one of three plans — `free`, `pro`,
//! `business` (the domain's plan table) — and a paid plan
//! buys two things for a 30-day period: a discount off the
//! platform commission rate for THAT store's sub-orders
//! (100 bps on pro, 200 on business), and a higher product
//! cap for the store's catalogue (500 on pro, unlimited on
//! business; the free cap is the tunable one).
//!
//! Money rules, each enforced by exactly one function below:
//! - Payment: from the seller's EARNINGS BALANCE only, like
//!   promotions (Wave 26). Subscribing runs in ONE
//!   transaction that locks the payout-store row, reads the
//!   one shared earnings aggregate through
//!   `payouts::seller_earnings_on`, refuses when the
//!   available balance cannot cover the effective price, and
//!   writes the subscription row plus its `seller_charges`
//!   row (kind `subscription`) together — so a plan is paid
//!   exactly once, and the charge then reduces the same
//!   available balance that gates payouts. There is no cash
//!   top-up and no external payment rail for platform
//!   billing: a seller who has not sold yet has no balance
//!   and cannot buy a plan.
//! - Price: the EFFECTIVE price for the plan (`admin`
//!   module — platform setting, then the startup value, then
//!   the domain default). Read at purchase and renewal time;
//!   a period already bought is never re-priced.
//! - Switching: subscribing while another paid plan is live
//!   replaces it — the new plan's FULL price is charged and
//!   a fresh 30-day period starts now. No proration, no
//!   credit for the unused remainder (stated here and in the
//!   README so the rule is never a surprise). Switching to
//!   `free` cancels the paid plan EFFECTIVE IMMEDIATELY, also
//!   with no refund: the row is marked `cancelled` and the
//!   store's benefits stop at once.
//! - Cancelling renewal (`/cancel`): `auto_renew` flips off
//!   and nothing else changes — the plan stays live until
//!   its paid period ends, then lapses. Cancelling is NOT
//!   switching to free.
//! - Activity: a plan's benefits apply only while its row is
//!   `active` AND `expires_at` is still ahead — the single
//!   rule [`live_plan`] — so a lapsed period stops discounting
//!   commission the moment it ends, whether or not the
//!   worker's sweep has marked the row `expired` yet.
//! - Renewal: the worker calls [`process_subscriptions`]
//!   each cycle. A live row whose period has ended renews
//!   when `auto_renew` is on and the balance covers the
//!   current price: the charge is written and `expires_at`
//!   extends 30 days FROM THE OLD EXPIRY, so a late sweep
//!   never steals paid days (a store more than one period
//!   behind catches up one period per sweep). Otherwise the
//!   row is marked `expired` and the store falls back to
//!   free until the seller subscribes again.
//!
//! Scoping: a subscription belongs to a STORE, but the money
//! settles through the seller's payout store, so the
//! endpoints resolve exactly like the payout endpoints
//! (Wave 34): reading takes `view_finances` (owner or
//! manager) on the payout store by default, or on a store
//! named by `?store_id=`; subscribing and cancelling take
//! `manage_payouts` — a right only the owner holds, because
//! a subscription spends the earnings balance.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    plan_commission_discount_bps, plan_product_cap, validate_plan_key, PLAN_FREE,
    SUBSCRIPTION_PERIOD_DAYS, SUBSCRIPTION_PLANS,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::access::{resolve_scoped_store, StoreScopeQuery};
use crate::admin::{
    effective_commission_bps, effective_plan_free_product_cap, effective_subscription_price,
};
use crate::auth::AuthUser;
use crate::payouts::seller_earnings_on;
use crate::stores::Store;
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One `store_subscriptions` row as stored (migration 0034).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Subscription {
    pub id: Uuid,
    pub store_id: Uuid,
    /// `free`, `pro` or `business` (the domain plan keys).
    pub plan: String,
    /// `active`, `cancelled` or `expired` — see the migration
    /// doc for the vocabulary. Benefits additionally require
    /// the period to be unexpired ([`live_plan`]).
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub auto_renew: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One plan in the public catalogue: its effective price for
/// one period, the commission discount it earns, and the
/// product cap it carries (the free cap is the effective,
/// tunable one; business is `i64::MAX` — unlimited).
#[derive(Debug, Serialize)]
pub struct PlanInfo {
    pub key: String,
    pub price_poisha: i64,
    pub commission_discount_bps: i64,
    pub product_cap: i64,
    pub period_days: i64,
}

/// The plans catalogue envelope.
#[derive(Debug, Serialize)]
pub struct PlanCatalogue {
    pub items: Vec<PlanInfo>,
}

/// A store's subscription as the seller surfaces show it: the
/// EFFECTIVE plan right now (a lapsed or cancelled paid plan
/// reads as `free`), the stored row's status (`free` when the
/// store has never subscribed), and the benefit figures the
/// effective plan currently carries.
#[derive(Debug, Serialize)]
pub struct SubscriptionView {
    pub store_id: Uuid,
    /// The plan whose benefits apply right now.
    pub plan: String,
    /// The stored status, or `free` when no row exists.
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub auto_renew: bool,
    /// Commission discount (bps) currently applied to this
    /// store's checkout slices — 0 unless a paid plan is live.
    pub commission_discount_bps: i64,
    /// Product cap currently enforced for this store.
    pub product_cap: i64,
    /// One period's price for the effective plan (0 on free).
    pub price_poisha: i64,
}

/// The subscribe receipt: the resulting view, what this
/// purchase charged (0 when switching to free), and the
/// seller's available balance right after — the same
/// aggregate the balance endpoint reports.
#[derive(Debug, Serialize)]
pub struct SubscribeResult {
    pub subscription: SubscriptionView,
    pub charged_poisha: i64,
    pub available_after_poisha: i64,
}

/// The tallies of one [`process_subscriptions`] run: how many
/// lapsed live rows were picked up (`checked`), how many
/// renewed (`renewed`), and how many were marked `expired`
/// (`expired`). A row the sweep found already handled by a
/// concurrent subscribe counts only in `checked`, so
/// `renewed + expired` may be less than `checked`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SubReport {
    pub checked: usize,
    pub renewed: usize,
    pub expired: usize,
}

/// Column list for a subscription row (matches [`Subscription`]).
const SUBSCRIPTION_COLUMNS: &str = "id, store_id, plan, status, started_at, expires_at, \
    auto_renew, created_at, updated_at";

// ---------------------------------------------------------------------------
// Service functions
// ---------------------------------------------------------------------------

/// Load a store's subscription row over any executor (the
/// pool for reads, a transaction for the guarded writes).
async fn load_subscription_on<'e, E>(
    executor: E,
    store_id: Uuid,
) -> Result<Option<Subscription>, ApiError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_as(&format!(
        "SELECT {SUBSCRIPTION_COLUMNS} FROM store_subscriptions WHERE store_id = $1"
    ))
    .bind(store_id)
    .fetch_optional(executor)
    .await
    .map_err(|e| ApiError::internal("load store subscription", e))
}

/// THE activity rule: the plan whose benefits apply from a
/// stored row at `now` — the row's plan when it is `active`
/// and its paid period has not ended, else `None` (the store
/// is effectively on the free plan). Checkout, the product
/// cap and the seller view all decide through this one rule,
/// so a lapsed period stops conferring benefits at its end
/// even before the worker's sweep marks the row `expired`.
fn live_plan(row: &Subscription, now: DateTime<Utc>) -> Option<String> {
    if row.status == "active" && row.expires_at > now {
        Some(row.plan.clone())
    } else {
        None
    }
}

/// The product cap a plan carries right now: the free cap is
/// the tunable one (platform setting, then startup value,
/// then the domain default); the paid caps are domain
/// constants — part of what the plans sell.
pub async fn effective_product_cap_for_plan(state: &AppState, plan: &str) -> i64 {
    if plan == PLAN_FREE {
        effective_plan_free_product_cap(state).await
    } else {
        plan_product_cap(plan)
    }
}

/// The product cap enforced for one store right now: its
/// live plan's cap, the free cap when no paid plan is live.
/// The product create path gates on this.
pub async fn product_cap_for_store(state: &AppState, store_id: Uuid) -> Result<i64, ApiError> {
    let pool = state.require_pool()?;
    let row = load_subscription_on(pool, store_id).await?;
    let plan = row
        .as_ref()
        .and_then(|row| live_plan(row, Utc::now()))
        .unwrap_or_else(|| PLAN_FREE.to_string());
    Ok(effective_product_cap_for_plan(state, &plan).await)
}

/// The commission discount (bps) a store's live paid plan
/// earns right now; 0 when no paid plan is live.
pub async fn active_plan_discount_bps(pool: &PgPool, store_id: Uuid) -> Result<i64, ApiError> {
    let plan: Option<String> = sqlx::query_scalar(
        "SELECT plan FROM store_subscriptions \
         WHERE store_id = $1 AND status = 'active' AND expires_at > now()",
    )
    .bind(store_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load store plan discount", e))?;
    Ok(plan
        .as_deref()
        .map(plan_commission_discount_bps)
        .unwrap_or(0))
}

/// THE commission rate for one store's checkout slices: the
/// platform's effective rate minus the store's live plan
/// discount, floored at zero. Checkout resolves this per
/// seller group and hands it to the domain totals function
/// as the group's own rate, so a split order charges each
/// store at its own plan's rate and the sub-order snapshots
/// the figure it was actually charged.
pub async fn effective_commission_bps_for_store(
    state: &AppState,
    store_id: Uuid,
) -> Result<u32, ApiError> {
    let base = i64::from(effective_commission_bps(state).await);
    let discount = match &state.pool {
        Some(pool) => active_plan_discount_bps(pool, store_id).await?,
        None => 0,
    };
    Ok((base - discount).max(0) as u32)
}

/// Build the seller-facing view of a store's subscription
/// from its stored row (or its absence).
async fn build_view(
    state: &AppState,
    store_id: Uuid,
    row: Option<Subscription>,
) -> SubscriptionView {
    let live = row.as_ref().and_then(|row| live_plan(row, Utc::now()));
    let plan = live
        .clone()
        .unwrap_or_else(|| PLAN_FREE.to_string());
    SubscriptionView {
        store_id,
        commission_discount_bps: live
            .as_deref()
            .map(plan_commission_discount_bps)
            .unwrap_or(0),
        product_cap: effective_product_cap_for_plan(state, &plan).await,
        price_poisha: effective_subscription_price(state, &plan).await,
        status: row
            .as_ref()
            .map(|row| row.status.clone())
            .unwrap_or_else(|| "free".to_string()),
        started_at: row.as_ref().map(|row| row.started_at),
        expires_at: row.as_ref().map(|row| row.expires_at),
        auto_renew: row.as_ref().map(|row| row.auto_renew).unwrap_or(false),
        plan,
    }
}

/// A store's subscription view, loading its row.
pub async fn get_subscription_view(
    state: &AppState,
    store_id: Uuid,
) -> Result<SubscriptionView, ApiError> {
    let pool = state.require_pool()?;
    let row = load_subscription_on(pool, store_id).await?;
    Ok(build_view(state, store_id, row).await)
}

/// The plans catalogue at effective prices and caps.
pub async fn list_plans(state: &AppState) -> PlanCatalogue {
    let mut items = Vec::with_capacity(SUBSCRIPTION_PLANS.len());
    for key in SUBSCRIPTION_PLANS {
        items.push(PlanInfo {
            key: key.to_string(),
            price_poisha: effective_subscription_price(state, key).await,
            commission_discount_bps: plan_commission_discount_bps(key),
            product_cap: effective_product_cap_for_plan(state, key).await,
            period_days: SUBSCRIPTION_PERIOD_DAYS,
        });
    }
    PlanCatalogue { items }
}

/// Subscribe a store to a plan, prepaid from the seller's
/// earnings balance. See the module doc for the money rules;
/// the transaction below is the whole enforcement. The
/// caller (the handler) has already resolved `store` under
/// `manage_payouts` — the owner alone may spend the balance.
pub async fn subscribe(
    pool: &PgPool,
    state: &AppState,
    store: &Store,
    plan_raw: &str,
) -> Result<SubscribeResult, ApiError> {
    let plan = validate_plan_key(plan_raw).map_err(ApiError::bad_request)?;

    // Switching to free: the paid plan is cancelled effective
    // immediately, with no refund and no charge. A store with
    // no row is already on free — the same view answers, with
    // nothing written.
    if plan == PLAN_FREE {
        sqlx::query(
            "UPDATE store_subscriptions \
             SET plan = 'free', status = 'cancelled', auto_renew = false, \
                 expires_at = now(), updated_at = now() \
             WHERE store_id = $1",
        )
        .bind(store.id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("cancel plan for free switch", e))?;
        let earnings = seller_earnings_on(pool, store.id).await?;
        return Ok(SubscribeResult {
            subscription: get_subscription_view(state, store.id).await?,
            charged_poisha: 0,
            available_after_poisha: earnings.available_poisha,
        });
    }

    let price = effective_subscription_price(state, plan).await;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin subscribe transaction", e))?;

    // Lock the store's row for the rest of the transaction:
    // two concurrent subscribes (or a subscribe racing a
    // payout request) now queue instead of both reading the
    // same available balance and both charging against it.
    sqlx::query("SELECT id FROM stores WHERE id = $1 FOR UPDATE")
        .bind(store.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("lock store for subscription", e))?;

    // The balance gate, read through the ONE earnings
    // aggregate inside this transaction: the charge below
    // and this read commit or roll back together.
    let earnings = seller_earnings_on(&mut *tx, store.id).await?;
    if earnings.available_poisha < price {
        return Err(ApiError::bad_request(format!(
            "insufficient earnings balance: the {plan} plan costs {price} poisha \
             but only {} poisha is available — \
             ব্যালেন্সে পর্যাপ্ত টাকা নেই",
            earnings.available_poisha
        )));
    }

    // Upsert the store's one subscription row: a fresh 30-day
    // period from now, renewal on. Switching plans takes the
    // same path — full price, new period, no proration.
    let period_days = i32::try_from(SUBSCRIPTION_PERIOD_DAYS)
        .map_err(|_| ApiError::bad_request("subscription period out of range"))?;
    let subscription: Subscription = sqlx::query_as(&format!(
        "INSERT INTO store_subscriptions \
           (store_id, plan, status, started_at, expires_at, auto_renew) \
         VALUES ($1, $2, 'active', now(), now() + make_interval(days => $3), true) \
         ON CONFLICT (store_id) DO UPDATE SET \
           plan = EXCLUDED.plan, status = 'active', \
           started_at = EXCLUDED.started_at, expires_at = EXCLUDED.expires_at, \
           auto_renew = true, updated_at = now() \
         RETURNING {SUBSCRIPTION_COLUMNS}"
    ))
    .bind(store.id)
    .bind(plan)
    .bind(period_days)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("upsert store subscription", e))?;

    // The charge: the row that makes the earnings aggregate
    // subtract this purchase from the seller's balance from
    // now on. Charges belong to the seller (the store's
    // owner), like promotion charges.
    sqlx::query(
        "INSERT INTO seller_charges (seller_user_id, amount_poisha, kind, ref_id) \
         VALUES ($1, $2, 'subscription', $3)",
    )
    .bind(store.owner_user_id)
    .bind(price)
    .bind(subscription.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("record subscription charge", e))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit subscribe transaction", e))?;

    Ok(SubscribeResult {
        subscription: build_view(state, store.id, Some(subscription)).await,
        charged_poisha: price,
        available_after_poisha: earnings.available_poisha - price,
    })
}

/// Cancel a store's subscription RENEWAL: `auto_renew` flips
/// off; the plan stays live until its paid period ends, then
/// lapses (the worker's sweep marks it `expired`). A store
/// with no live paid plan has nothing to cancel — a 400.
pub async fn cancel_subscription(
    pool: &PgPool,
    state: &AppState,
    store: &Store,
) -> Result<SubscriptionView, ApiError> {
    let row = load_subscription_on(pool, store.id)
        .await?
        .filter(|row| row.status == "active" && row.plan != PLAN_FREE)
        .ok_or_else(|| {
            ApiError::bad_request(
                "this store has no active paid subscription to cancel — \
                 এই স্টোরে বাতিল করার মতো সক্রিয় পেইড সাবস্ক্রিপশন নেই",
            )
        })?;

    sqlx::query(
        "UPDATE store_subscriptions SET auto_renew = false, updated_at = now() WHERE id = $1",
    )
    .bind(row.id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("cancel subscription renewal", e))?;

    get_subscription_view(state, store.id).await
}

// ---------------------------------------------------------------------------
// The renewal sweep (worker-facing)
// ---------------------------------------------------------------------------

/// What one candidate's processing did.
enum SweepOutcome {
    Renewed,
    Expired,
    /// The row changed under the sweep (a subscribe renewed
    /// or replaced it first) — nothing to do this run.
    Skipped,
}

/// Renew or expire every lapsed live subscription (Wave 36).
/// A candidate is a row that is `active`, on a paid plan, and
/// whose `expires_at` has passed. Each candidate is settled
/// in its OWN transaction by [`settle_lapsed_subscription`]:
/// one store's failure is logged and counted only in
/// `checked` — it never stops the batch, and because a failed
/// settlement writes nothing, the row stays a candidate for
/// the next run. The worker calls this every cycle, beside
/// the shipment poller (Wave 30) whose shape it mirrors.
pub async fn process_subscriptions(state: &AppState) -> Result<SubReport, ApiError> {
    let pool = state.require_pool()?;
    let candidates: Vec<Subscription> = sqlx::query_as(&format!(
        "SELECT {SUBSCRIPTION_COLUMNS} FROM store_subscriptions \
         WHERE status = 'active' AND plan <> 'free' AND expires_at <= now() \
         ORDER BY expires_at ASC"
    ))
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load lapsed subscriptions", e))?;

    let mut report = SubReport {
        checked: candidates.len(),
        ..SubReport::default()
    };
    for candidate in &candidates {
        match settle_lapsed_subscription(state, candidate).await {
            Ok(SweepOutcome::Renewed) => report.renewed += 1,
            Ok(SweepOutcome::Expired) => report.expired += 1,
            Ok(SweepOutcome::Skipped) => {}
            Err(error) => {
                eprintln!(
                    "subscription sweep: settling store {} failed: {}",
                    candidate.store_id, error.message
                );
            }
        }
    }
    Ok(report)
}

/// Settle ONE lapsed subscription in its own transaction:
/// under the store row's lock the row is re-read (a subscribe
/// may have renewed or replaced it since the sweep listed
/// it); then, when renewal is on and the owner's available
/// balance covers the plan's current effective price, the
/// charge is written and the period extends 30 days FROM THE
/// OLD EXPIRY — otherwise the row is marked `expired` and
/// the store falls back to free.
async fn settle_lapsed_subscription(
    state: &AppState,
    candidate: &Subscription,
) -> Result<SweepOutcome, ApiError> {
    let pool = state.require_pool()?;
    let price = effective_subscription_price(state, &candidate.plan).await;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin subscription settlement", e))?;

    // Lock the store row: the same lock subscribe and payout
    // creation take, so a settlement never races a spend of
    // the same balance.
    sqlx::query("SELECT id FROM stores WHERE id = $1 FOR UPDATE")
        .bind(candidate.store_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("lock store for settlement", e))?;

    let current = load_subscription_on(&mut *tx, candidate.store_id).await?;
    let Some(current) = current else {
        // The store (and its row, by cascade) is gone.
        return Ok(SweepOutcome::Skipped);
    };
    if current.status != "active" || current.plan == PLAN_FREE || current.expires_at > Utc::now() {
        return Ok(SweepOutcome::Skipped);
    }

    if current.auto_renew {
        let earnings = seller_earnings_on(&mut *tx, current.store_id).await?;
        if earnings.available_poisha >= price {
            let period_days = i32::try_from(SUBSCRIPTION_PERIOD_DAYS)
                .map_err(|_| ApiError::bad_request("subscription period out of range"))?;
            sqlx::query(
                "INSERT INTO seller_charges (seller_user_id, amount_poisha, kind, ref_id) \
                 VALUES ((SELECT owner_user_id FROM stores WHERE id = $1), $2, \
                         'subscription', $3)",
            )
            .bind(current.store_id)
            .bind(price)
            .bind(current.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("record renewal charge", e))?;
            sqlx::query(
                "UPDATE store_subscriptions \
                 SET expires_at = expires_at + make_interval(days => $1), \
                     updated_at = now() \
                 WHERE id = $2",
            )
            .bind(period_days)
            .bind(current.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("extend subscription period", e))?;
            tx.commit()
                .await
                .map_err(|e| ApiError::internal("commit subscription renewal", e))?;
            return Ok(SweepOutcome::Renewed);
        }
    }

    sqlx::query(
        "UPDATE store_subscriptions SET status = 'expired', updated_at = now() WHERE id = $1",
    )
    .bind(current.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("expire subscription", e))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit subscription expiry", e))?;
    Ok(SweepOutcome::Expired)
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SubscribeInput {
    pub plan: String,
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn list_plans_handler(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Json<PlanCatalogue> {
    Json(list_plans(&state).await)
}

pub async fn get_subscription_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<SubscriptionView>, ApiError> {
    let pool = state.require_pool()?;
    // Wave 34 discipline: reading a store's subscription is
    // finance work — `view_finances` (owner or manager) on
    // the payout store by default, or on the named store.
    let (store, _role) = resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        domain::STORE_ACTION_VIEW_FINANCES,
    )
    .await?;
    Ok(Json(get_subscription_view(&state, store.id).await?))
}

pub async fn subscribe_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Json(input): Json<SubscribeInput>,
) -> Result<(StatusCode, Json<SubscribeResult>), ApiError> {
    let pool = state.require_pool()?;
    // Subscribing spends the earnings balance: `manage_payouts`,
    // a right only the owner holds — a manager naming the
    // store is refused with the matrix's 403 before any money
    // rule runs, exactly like a payout request.
    let (store, _role) = resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        domain::STORE_ACTION_MANAGE_PAYOUTS,
    )
    .await?;
    let result = subscribe(pool, &state, &store, &input.plan).await?;
    Ok((StatusCode::CREATED, Json(result)))
}

pub async fn cancel_subscription_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<SubscriptionView>, ApiError> {
    let pool = state.require_pool()?;
    // Cancelling renewal is the owner's alone, like
    // subscribing (see `subscribe_handler`).
    let (store, _role) = resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        domain::STORE_ACTION_MANAGE_PAYOUTS,
    )
    .await?;
    Ok(Json(cancel_subscription(pool, &state, &store).await?))
}
