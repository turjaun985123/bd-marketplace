//! Loyalty points (Wave 35): the buyer points ledger and the
//! hooks every points movement hangs off.
//!
//! The rules live in the pure domain functions (earn 1 point
//! per 100 taka of net goods on a delivered sub-order; redeem
//! at 1 point = 100 poisha, capped at half the order's
//! post-coupon goods). This module owns the ledger reads and
//! writes around them:
//!
//! - [`loyalty_balance_on`] — THE balance computation, one
//!   aggregate query over the ledger. Available points are the
//!   unexpired earn sum plus every redeem/adjust movement,
//!   floored at zero. Deliberate simplification, documented
//!   here once: redemptions and adjustments are netted against
//!   the unexpired earn pool AS A WHOLE — there is no per-point
//!   FIFO tracking of which earn lot a redemption consumed, so
//!   when an old earn lot expires, points already spent are not
//!   resurrected and points spent "from" a newer lot are not
//!   clawed back either; the pool is one number.
//! - [`earn_for_sub_order`] — the earn hook, called inside the
//!   two transactions that move a sub-order to `delivered`
//!   (the seller status PATCH in `orders` and the shared
//!   shipment sync in `shipments` — the same funnels that fire
//!   the Wave 29 delivered notification). The partial UNIQUE
//!   index on earn rows makes a repeat call a no-op.
//! - [`refund_for_cancelled_sub_order`] — the cancel hook: a
//!   cancelled sub-order gives back the points its allocated
//!   share of the checkout redemption bought (share / 100,
//!   floored; the allocation remainder stays with the platform,
//!   see the module note in `orders`). Called from the buyer
//!   whole-order cancel and the seller single-sub-order cancel,
//!   inside their transactions. Both cancels are one-way
//!   transitions, so the refund can be attempted at most once.
//! - [`clawback_for_returned_sub_order`] — the return hook,
//!   called from `returns::mark_return_received`: goods that
//!   came back un-earn their points. Also a one-way transition.
//!
//! The platform funds every redemption: nothing in this module
//! touches `seller_payout_poisha` or the commission basis — a
//! seller is paid exactly as if the buyer had paid cash.

use axum::{extract::State, Json};
use chrono::{DateTime, Utc};
use domain::{
    loyalty_points_for_goods, LOYALTY_EXPIRY_DAYS, LOYALTY_POISHA_PER_POINT,
    LOYALTY_POINT_VALUE_POISHA,
};
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Balance
// ---------------------------------------------------------------------------

/// A buyer's points position, computed from the ledger.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct LoyaltyBalance {
    /// Points spendable right now (never negative).
    pub available_points: i64,
    /// Every point ever earned, including expired ones.
    pub lifetime_earned: i64,
    /// Every point ever redeemed at checkout.
    pub lifetime_redeemed: i64,
}

/// THE balance query — one aggregate over the caller's ledger
/// rows. Executor-generic so checkout can run it inside its
/// transaction (under the buyer's user-row lock, which
/// serializes concurrent checkouts) and the balance endpoint
/// can run it on the pool.
pub async fn loyalty_balance_on<'e, E>(
    executor: E,
    user_id: Uuid,
) -> Result<LoyaltyBalance, ApiError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let row: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
           COALESCE(SUM(delta) FILTER \
               (WHERE reason = 'earn' AND expires_at > now()), 0)::bigint \
               AS unexpired_earn, \
           COALESCE(SUM(delta) FILTER \
               (WHERE reason IN ('redeem', 'adjust')), 0)::bigint \
               AS net_moves, \
           COALESCE(SUM(delta) FILTER (WHERE reason = 'earn'), 0)::bigint \
               AS lifetime_earned, \
           COALESCE(-SUM(delta) FILTER (WHERE reason = 'redeem'), 0)::bigint \
               AS lifetime_redeemed \
         FROM loyalty_points WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(executor)
    .await
    .map_err(|e| ApiError::internal("load loyalty balance", e))?;
    Ok(LoyaltyBalance {
        available_points: (row.0 + row.1).max(0),
        lifetime_earned: row.2,
        lifetime_redeemed: row.3,
    })
}

/// The balance on the pool — the form read endpoints use.
pub async fn loyalty_balance(pool: &PgPool, user_id: Uuid) -> Result<LoyaltyBalance, ApiError> {
    loyalty_balance_on(pool, user_id).await
}

// ---------------------------------------------------------------------------
// Ledger hooks (each runs inside the caller's transaction)
// ---------------------------------------------------------------------------

/// Insert one ledger row on the caller's connection. The one
/// writer — every hook below goes through it, so the ledger's
/// shape (delta sign conventions, ref semantics) lives here.
async fn insert_ledger_row(
    conn: &mut PgConnection,
    user_id: Uuid,
    delta: i64,
    reason: &str,
    ref_id: Uuid,
    expires_at: Option<DateTime<Utc>>,
) -> Result<(), ApiError> {
    let delta_i32 = i32::try_from(delta)
        .map_err(|_| ApiError::internal("loyalty delta out of range", sqlx::Error::RowNotFound))?;
    sqlx::query(
        "INSERT INTO loyalty_points (user_id, delta, reason, ref_id, expires_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(user_id)
    .bind(delta_i32)
    .bind(reason)
    .bind(ref_id)
    .bind(expires_at)
    .execute(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("insert loyalty row", e))?;
    Ok(())
}

/// The earn hook: credit the buyer for one sub-order that has
/// just reached `delivered`, inside the delivery transaction.
/// Points come from the sub-order's NET goods (goods subtotal
/// minus its coupon discount share — the domain rule); a
/// sub-order under 100 taka of net goods earns nothing and
/// writes no row. The insert runs ON CONFLICT DO NOTHING
/// against the earn-once partial index, so even if a funnel
/// were ever reached twice the buyer is credited exactly once.
/// The buyer is the parent order's `buyer_user_id` — checkout
/// is authenticated, so every order has an account to credit.
pub async fn earn_for_sub_order(
    conn: &mut PgConnection,
    sub_order_id: Uuid,
) -> Result<(), ApiError> {
    let row: Option<(Uuid, i64, i64)> = sqlx::query_as(
        "SELECT o.buyer_user_id, so.goods_subtotal_poisha, so.discount_poisha \
         FROM sub_orders so JOIN orders o ON o.id = so.order_id WHERE so.id = $1",
    )
    .bind(sub_order_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load sub-order for loyalty earn", e))?;
    let Some((buyer_user_id, goods_poisha, discount_poisha)) = row else {
        return Ok(());
    };
    let points = loyalty_points_for_goods(goods_poisha - discount_poisha);
    if points <= 0 {
        return Ok(());
    }
    let points_i32 = i32::try_from(points).map_err(|_| {
        ApiError::internal("loyalty points out of range", sqlx::Error::RowNotFound)
    })?;
    sqlx::query(
        "INSERT INTO loyalty_points (user_id, delta, reason, ref_id, expires_at) \
         VALUES ($1, $2, 'earn', $3, now() + make_interval(days => $4)) \
         ON CONFLICT (ref_id) WHERE reason = 'earn' DO NOTHING",
    )
    .bind(buyer_user_id)
    .bind(points_i32)
    .bind(sub_order_id)
    .bind(LOYALTY_EXPIRY_DAYS as i32)
    .execute(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("insert loyalty earn", e))?;
    Ok(())
}

/// The cancel hook: give back the points a cancelled
/// sub-order's share of the checkout redemption bought. The
/// share is read from the sub-order's snapshot column; the
/// refund is the share's whole points (share / 100, floored —
/// the redemption total was a multiple of 100 poisha but its
/// per-sub-order allocation shares need not be, and the few
/// remainder poisha stay with the platform). A sub-order that
/// carried no share writes no row. One-way-transition guard:
/// see the module docs.
pub async fn refund_for_cancelled_sub_order(
    conn: &mut PgConnection,
    sub_order_id: Uuid,
) -> Result<(), ApiError> {
    let share: Option<(Uuid, i64)> = sqlx::query_as(
        "SELECT o.buyer_user_id, so.loyalty_discount_poisha \
         FROM sub_orders so JOIN orders o ON o.id = so.order_id WHERE so.id = $1",
    )
    .bind(sub_order_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load sub-order for loyalty refund", e))?;
    let Some((buyer_user_id, share_poisha)) = share else {
        return Ok(());
    };
    let refund_points = share_poisha / LOYALTY_POINT_VALUE_POISHA;
    if refund_points <= 0 {
        return Ok(());
    }
    insert_ledger_row(
        conn,
        buyer_user_id,
        refund_points,
        "adjust",
        sub_order_id,
        None,
    )
    .await
}

/// The return hook: claw back a returned sub-order's earn.
/// Reads the earn row (at most one exists — the earn-once
/// index) and writes the mirror `adjust` row; a sub-order that
/// earned nothing (below the 100-taka floor) writes no row.
/// The buyer's balance may already have spent the points —
/// the balance formula floors at zero, so the clawback simply
/// consumes future earns first. One-way-transition guard: see
/// the module docs.
pub async fn clawback_for_returned_sub_order(
    conn: &mut PgConnection,
    buyer_user_id: Uuid,
    sub_order_id: Uuid,
) -> Result<(), ApiError> {
    let earned: Option<i32> = sqlx::query_scalar(
        "SELECT delta FROM loyalty_points WHERE reason = 'earn' AND ref_id = $1",
    )
    .bind(sub_order_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load loyalty earn for clawback", e))?;
    let Some(earned) = earned.filter(|delta| *delta > 0) else {
        return Ok(());
    };
    insert_ledger_row(
        conn,
        buyer_user_id,
        -i64::from(earned),
        "adjust",
        sub_order_id,
        None,
    )
    .await
}

// ---------------------------------------------------------------------------
// Balance + history endpoint
// ---------------------------------------------------------------------------

/// One ledger row as the buyer sees it in their history.
#[derive(Debug, Serialize, FromRow)]
pub struct LoyaltyEntry {
    pub delta: i32,
    /// earn | redeem | adjust
    pub reason: String,
    pub ref_id: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// The buyer's loyalty view: the balance, the programme rules
/// (so the client never hardcodes them) and the recent ledger.
#[derive(Debug, Serialize)]
pub struct LoyaltyView {
    pub available_points: i64,
    pub lifetime_earned: i64,
    pub lifetime_redeemed: i64,
    /// Net goods poisha that earn one point (100 taka).
    pub earn_poisha_per_point: i64,
    /// Poisha one redeemed point buys (1 taka).
    pub point_value_poisha: i64,
    /// The redemption cap, as a percent of post-coupon goods.
    pub max_redeem_percent_of_goods: i64,
    /// Days an earned point stays spendable.
    pub expiry_days: i64,
    pub history: Vec<LoyaltyEntry>,
}

/// GET /v1/loyalty — the caller's points balance and their
/// last 20 ledger movements, newest first.
pub async fn get_loyalty(pool: &PgPool, user: &AuthUser) -> Result<LoyaltyView, ApiError> {
    let balance = loyalty_balance(pool, user.user_id).await?;
    let history: Vec<LoyaltyEntry> = sqlx::query_as(
        "SELECT delta, reason, ref_id, expires_at, created_at \
         FROM loyalty_points WHERE user_id = $1 \
         ORDER BY created_at DESC, id DESC LIMIT 20",
    )
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load loyalty history", e))?;
    Ok(LoyaltyView {
        available_points: balance.available_points,
        lifetime_earned: balance.lifetime_earned,
        lifetime_redeemed: balance.lifetime_redeemed,
        earn_poisha_per_point: LOYALTY_POISHA_PER_POINT,
        point_value_poisha: LOYALTY_POINT_VALUE_POISHA,
        max_redeem_percent_of_goods: 50,
        expiry_days: LOYALTY_EXPIRY_DAYS,
        history,
    })
}

pub async fn get_loyalty_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<LoyaltyView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_loyalty(pool, &user).await?))
}
