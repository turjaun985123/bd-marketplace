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
//!   or `paid`) − pending (payouts still `requested`), computed by
//!   the pure domain function and clamped at zero at this boundary
//!   (see `available_balance`).
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
//!
//! Scoping: every query starts from the authenticated user. The
//! payout store is the seller's earliest-created store
//! (`stores::get_store_by_owner`; a user without a store gets a
//! 404). Counts and the recent-orders list describe the seller's
//! whole selling activity across their stores — the same scope as
//! the seller orders page, whose shared list function the dashboard
//! reuses — while earnings and payouts are strictly per payout
//! store, because money must be exact per store.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    compute_available_balance, validate_payout_account, validate_payout_amount_with_min,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::orders::{list_seller_sub_orders, SellerSubOrder};
use crate::products::clamp_pagination;
use crate::stores::{get_store_by_owner, Store};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// A store's money summary: what it earned from delivered
/// sub-orders, what is already paid or committed out, what sits in
/// requested payouts, and what can still be requested.
#[derive(Debug, Clone, Serialize)]
pub struct Earnings {
    pub earned_poisha: i64,
    /// Payouts in `approved` (committed) or `paid` (gone).
    pub paid_out_poisha: i64,
    /// Payouts still in `requested`, waiting for the platform.
    pub pending_poisha: i64,
    /// earned − paid out − pending, clamped at zero.
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
/// arithmetic, clamped at zero. With consistent inputs the raw
/// value is never negative (a payout can only be requested against
/// the available balance), so the clamp only guards the display
/// against inconsistent history.
pub fn available_balance(earnings: &Earnings) -> i64 {
    compute_available_balance(
        earnings.earned_poisha,
        earnings.paid_out_poisha,
        earnings.pending_poisha,
    )
    .max(0)
}

/// THE earnings aggregate for a store — dashboard, balance and
/// payout creation all read money through this one function, so
/// the numbers can never disagree between endpoints.
pub async fn seller_earnings(pool: &PgPool, store_id: Uuid) -> Result<Earnings, ApiError> {
    let row: (i64, i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT COALESCE(SUM(seller_payout_poisha), 0)::bigint \
              FROM sub_orders \
             WHERE store_id = $1 AND status = 'delivered') AS earned_poisha, \
           (SELECT COALESCE(SUM(amount_poisha), 0)::bigint \
              FROM payouts \
             WHERE store_id = $1 AND status IN ('approved', 'paid')) AS paid_out_poisha, \
           (SELECT COALESCE(SUM(amount_poisha), 0)::bigint \
              FROM payouts \
             WHERE store_id = $1 AND status = 'requested') AS pending_poisha",
    )
    .bind(store_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("load seller earnings", e))?;

    let mut earnings = Earnings {
        earned_poisha: row.0,
        paid_out_poisha: row.1,
        pending_poisha: row.2,
        available_poisha: 0,
    };
    earnings.available_poisha = available_balance(&earnings);
    Ok(earnings)
}

/// The seller dashboard: payout store, activity counts, earnings,
/// and the five most recent sub-orders (the shared seller list).
pub async fn get_dashboard(pool: &PgPool, user: &AuthUser) -> Result<SellerDashboard, ApiError> {
    let store = seller_store(pool, user).await?;

    let products: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM products p JOIN stores s ON s.id = p.store_id \
         WHERE s.owner_user_id = $1",
    )
    .bind(user.user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count seller products", e))?;

    let status_rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT so.status, COUNT(*) FROM sub_orders so \
         JOIN stores s ON s.id = so.store_id \
         WHERE s.owner_user_id = $1 GROUP BY so.status",
    )
    .bind(user.user_id)
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
    let recent = list_seller_sub_orders(pool, user, Some(1), Some(5)).await?;
    let low_stock_items =
        crate::inventory::low_stock_items_for_seller(pool, user.user_id).await?;

    Ok(SellerDashboard {
        store,
        counts: SellerCounts {
            products,
            sub_orders_by_status: by_status,
        },
        earnings,
        recent_sub_orders: recent.items,
        low_stock_items,
    })
}

/// The balance endpoint's whole job: the caller's store earnings.
pub async fn get_balance(pool: &PgPool, user: &AuthUser) -> Result<Earnings, ApiError> {
    let store = seller_store(pool, user).await?;
    seller_earnings(pool, store.id).await
}

/// The caller's payout account, or `None` when they have not saved
/// one yet.
pub async fn get_payout_account(
    pool: &PgPool,
    user: &AuthUser,
) -> Result<Option<PayoutAccount>, ApiError> {
    let store = seller_store(pool, user).await?;
    sqlx::query_as(
        "SELECT id, store_id, method, account_name, account_number, created_at, updated_at \
         FROM payout_accounts WHERE store_id = $1",
    )
    .bind(store.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load payout account", e))
}

/// Create or replace the caller's payout account. The domain
/// validator decides what a legal account is per method; the
/// stored fields are trimmed copies of what the seller sent.
pub async fn save_payout_account(
    pool: &PgPool,
    user: &AuthUser,
    method: &str,
    account_name: &str,
    account_number: &str,
) -> Result<PayoutAccount, ApiError> {
    let store = seller_store(pool, user).await?;
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

/// Request a payout of `amount_poisha` from the caller's available
/// balance. Requires a saved payout account (its fields are
/// snapshotted onto the payout row); the domain amount validator
/// enforces the platform minimum and the balance fit. The minimum
/// is the EFFECTIVE one (Wave 10): an admin-set platform setting
/// wins over the built-in [`domain::MIN_PAYOUT_POISHA`].
pub async fn request_payout(
    pool: &PgPool,
    state: &AppState,
    user: &AuthUser,
    amount_poisha: i64,
) -> Result<Payout, ApiError> {
    let store = seller_store(pool, user).await?;

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

    let account: Option<PayoutAccount> = sqlx::query_as(
        "SELECT id, store_id, method, account_name, account_number, created_at, updated_at \
         FROM payout_accounts WHERE store_id = $1",
    )
    .bind(store.id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load payout account", e))?;
    let account = account.ok_or_else(|| {
        ApiError::bad_request("add a payout account first, then request a payout")
    })?;

    let earnings = seller_earnings(pool, store.id).await?;
    let min_payout_poisha = crate::admin::effective_min_payout(state).await;
    validate_payout_amount_with_min(
        amount_poisha,
        earnings.available_poisha,
        min_payout_poisha,
    )
    .map_err(ApiError::bad_request)?;

    sqlx::query_as(
        "INSERT INTO payouts (store_id, amount_poisha, method, account_name, account_number) \
         VALUES ($1, $2, $3, $4, $5) \
         RETURNING id, store_id, amount_poisha, method, account_name, account_number, \
                   status, note, created_at, processed_at",
    )
    .bind(store.id)
    .bind(amount_poisha)
    .bind(&account.method)
    .bind(&account.account_name)
    .bind(&account.account_number)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("create payout", e))
}

/// The caller's payout requests, newest first.
pub async fn list_payouts(
    pool: &PgPool,
    user: &AuthUser,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<PayoutList, ApiError> {
    let store = seller_store(pool, user).await?;
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payouts WHERE store_id = $1")
        .bind(store.id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count payouts", e))?;

    let items: Vec<Payout> = sqlx::query_as(
        "SELECT id, store_id, amount_poisha, method, account_name, account_number, \
         status, note, created_at, processed_at \
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
) -> Result<Json<SellerDashboard>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_dashboard(pool, &user).await?))
}

pub async fn balance_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Earnings>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_balance(pool, &user).await?))
}

pub async fn get_payout_account_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Option<PayoutAccount>>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_payout_account(pool, &user).await?))
}

pub async fn save_payout_account_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<SavePayoutAccountInput>,
) -> Result<Json<PayoutAccount>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        save_payout_account(
            pool,
            &user,
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
    Json(input): Json<RequestPayoutInput>,
) -> Result<(StatusCode, Json<Payout>), ApiError> {
    let pool = state.require_pool()?;
    let payout = request_payout(pool, &state, &user, input.amount_poisha).await?;
    Ok((StatusCode::CREATED, Json(payout)))
}

pub async fn list_payouts_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListPayoutsQuery>,
) -> Result<Json<PayoutList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_payouts(pool, &user, query.page, query.limit).await?,
    ))
}
