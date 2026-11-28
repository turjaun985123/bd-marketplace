//! Platform admin (Wave 10): overview metrics, store suspension,
//! payout processing, user admin flags and platform settings.
//!
//! Admin is a FLAG on the user row (`users.is_admin`, migration
//! 0013), not a role: an admin keeps their buyer/seller role, and
//! the flag alone unlocks every endpoint in this module. Every
//! handler starts from the ONE guard, `require_admin`, which
//! re-reads the caller's user row so a revoked flag takes effect
//! immediately instead of at JWT expiry. There is deliberately NO
//! endpoint that creates the first admin: promotion happens by
//! SQL (see the README), after which admins can toggle the flag
//! on other users — never on themselves, so the platform cannot
//! lock itself out through the API.
//!
//! Suspension: setting a store's status to `suspended` hides its
//! products from the public listing and detail paths (enforced in
//! the shared product queries in the `products` module) and makes
//! checkout refuse carts containing its products with a 409
//! (enforced in `orders::place_order`). The seller can still sign
//! in and manage the catalogue; reactivation restores everything.
//!
//! Settings: rows in `platform_settings` override startup values
//! at read time — `commission_bps` beats the startup
//! `COMMISSION_RATE_BPS`, and `min_payout_poisha` beats the domain
//! constant. Wave 17 adds the COD advance pair under the same
//! discipline: `advance_percent_bps` beats `ADVANCE_PERCENT_BPS`
//! and `advance_threshold_poisha` beats
//! `ADVANCE_THRESHOLD_POISHA`. Wave 21 adds the KYC payout gate
//! (`kyc_payout_gate`, stored as the text `on` / `off`): a flag
//! with no startup fallback, on only when the stored value reads
//! exactly `on`. Wave 26 adds the sponsored-promotion price:
//! `promotion_price_per_day_poisha` beats the startup
//! `PROMOTION_PRICE_PER_DAY_POISHA` (itself defaulting to the
//! domain's 100 taka a day) for NEW promotions. Wave 36 adds
//! the subscription knobs under the same discipline: the two
//! plan prices (`subscription_price_pro_poisha`,
//! `subscription_price_business_poisha`) beat their startup
//! values for NEW subscriptions and renewals, and
//! `plan_free_product_cap` beats `PLAN_FREE_PRODUCT_CAP` for
//! stores on the free plan. The effective-value helpers below
//! are the ONLY readers; checkout and payout creation call them,
//! never the raw values, so the precedence (setting, then
//! startup, then built-in default) lives in exactly one place. A
//! stored value that no longer parses or is out of range is
//! ignored on read.
//!
//! Wave 28: the consequential writes in this module (payout
//! transitions, store status changes, settings updates) run
//! in ONE transaction together with their admin audit row —
//! see the `audit` module for the action vocabulary and the
//! same-transaction rule.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    can_transition_payout, is_valid_payout_status, mask_phone, parse_setting_i64,
    search_like_pattern, to_local_phone, validate_advance_bps, validate_advance_threshold,
    validate_commission_bps, validate_payout_list_sort, validate_payout_rejection_reason,
    validate_payment_reference, validate_plan_free_product_cap, validate_promotion_price_per_day,
    validate_subscription_price, PayoutSort, ADVANCE_PERCENT_BPS_KEY, ADVANCE_THRESHOLD_KEY,
    COMMISSION_BPS_KEY, KYC_PAYOUT_GATE_KEY, MIN_PAYOUT_KEY, MIN_PAYOUT_POISHA, PLAN_BUSINESS,
    PLAN_FREE_PRODUCT_CAP_KEY, PLAN_PRO, PROMOTION_PRICE_PER_DAY_KEY, SUBSCRIPTION_PRICE_BUSINESS_KEY,
    SUBSCRIPTION_PRICE_PRO_KEY,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::audit::record_admin_action;
use crate::auth::{load_user, AuthUser, User};
use crate::orders::{load_order, OrderList};
use crate::payouts::Payout;
use crate::products::clamp_pagination;
use crate::stores::Store;
use crate::{ApiError, AppState};

/// The platform's public name, reported by the settings endpoint.
pub const PLATFORM_NAME: &str = "BD Marketplace";

/// Highest minimum-payout setting an admin may store (100,000
/// taka). The floor exists so a typo cannot price every seller
/// out of ever being paid; zero is allowed (no minimum).
const MAX_MIN_PAYOUT_POISHA: i64 = 10_000_000;

/// The payout column list shared by the transition UPDATE's
/// RETURNING clause (matches the `Payout` view in `payouts`).
const PAYOUT_COLUMNS: &str = "id, store_id, amount_poisha, method, account_name, \
    account_number, status, note, created_at, processed_at, approved_at, paid_at, \
    rejected_at, payment_reference, idempotency_key";

// ---------------------------------------------------------------------------
// The admin guard + settings readers
// ---------------------------------------------------------------------------

/// THE admin guard: load the caller's user row and demand the
/// admin flag. Returns the full row (handlers use it for the
/// self-demote check). 403 for every non-admin, including sellers.
pub async fn require_admin(state: &AppState, user: &AuthUser) -> Result<User, ApiError> {
    let pool = state.require_pool()?;
    let row = load_user(pool, user.user_id).await?;
    if !row.is_admin {
        return Err(ApiError::forbidden("admin access required"));
    }
    Ok(row)
}

/// Read one platform setting as an integer: the stored text run
/// through the pure domain parser. A missing row, an unparseable
/// value, or a database problem all read as "no override" — a bad
/// settings row must never take checkout down.
pub async fn platform_setting_i64(pool: &PgPool, key: &str) -> Option<i64> {
    let raw: Option<Option<String>> =
        sqlx::query_scalar("SELECT value FROM platform_settings WHERE key = $1")
            .bind(key)
            .fetch_optional(pool)
            .await
            .ok();
    raw.flatten().and_then(|value| parse_setting_i64(&value))
}

/// Is `value` a storable minimum-payout setting?
fn valid_min_payout_setting(value: i64) -> bool {
    (0..=MAX_MIN_PAYOUT_POISHA).contains(&value)
}

/// The commission rate checkout must use right now: the admin
/// setting when one is stored and valid, else the startup value
/// (`COMMISSION_RATE_BPS`, itself defaulting to 800).
pub async fn effective_commission_bps(state: &AppState) -> u32 {
    if let Some(pool) = &state.pool {
        if let Some(value) = platform_setting_i64(pool, COMMISSION_BPS_KEY).await {
            if validate_commission_bps(value) {
                return value as u32;
            }
        }
    }
    state.commission_rate_bps
}

/// The COD advance rate checkout must use right now (Wave 17):
/// the admin setting when one is stored and valid, else the
/// startup value (`ADVANCE_PERCENT_BPS`, itself defaulting to
/// 2,000). Mirrors [`effective_commission_bps`] exactly.
pub async fn effective_advance_bps(state: &AppState) -> u32 {
    if let Some(pool) = &state.pool {
        if let Some(value) = platform_setting_i64(pool, ADVANCE_PERCENT_BPS_KEY).await {
            if validate_advance_bps(value) {
                return value as u32;
            }
        }
    }
    state.advance_percent_bps
}

/// The COD advance threshold checkout must use right now
/// (Wave 17): the admin setting when one is stored and valid,
/// else the startup value (`ADVANCE_THRESHOLD_POISHA`, itself
/// defaulting to 200,000). A stored 0 is valid and disables
/// threshold-based advances.
pub async fn effective_advance_threshold(state: &AppState) -> i64 {
    if let Some(pool) = &state.pool {
        if let Some(value) = platform_setting_i64(pool, ADVANCE_THRESHOLD_KEY).await {
            if validate_advance_threshold(value) {
                return value;
            }
        }
    }
    state.advance_threshold_poisha
}

/// The minimum payout for new requests right now: the admin
/// setting when one is stored and in range, else the built-in
/// platform constant.
pub async fn effective_min_payout(state: &AppState) -> i64 {
    if let Some(pool) = &state.pool {
        if let Some(value) = platform_setting_i64(pool, MIN_PAYOUT_KEY).await {
            if valid_min_payout_setting(value) {
                return value;
            }
        }
    }
    MIN_PAYOUT_POISHA
}

/// Read one platform setting as raw text (trimmed). The text
/// sibling of [`platform_setting_i64`], for the settings that
/// are flags rather than numbers; a missing row or a database
/// problem reads as "no override", exactly like the integer
/// reader.
pub async fn platform_setting_text(pool: &PgPool, key: &str) -> Option<String> {
    let raw: Option<Option<String>> =
        sqlx::query_scalar("SELECT value FROM platform_settings WHERE key = $1")
            .bind(key)
            .fetch_optional(pool)
            .await
            .ok();
    raw.flatten().map(|value| value.trim().to_string())
}

/// Is the KYC payout gate on right now (Wave 21)? The stored
/// `kyc_payout_gate` setting must read exactly `on`; anything
/// else — `off`, a missing row, a garbled value — is OFF, so
/// the gate can only ever be enabled deliberately, and payout
/// creation (its one caller) falls back to the Wave 9
/// behaviour whenever the setting is not positively on. There
/// is no startup/env fallback for this one: the gate is a
/// runtime platform decision, not a deployment parameter.
pub async fn effective_kyc_payout_gate(state: &AppState) -> bool {
    if let Some(pool) = &state.pool {
        if let Some(value) = platform_setting_text(pool, KYC_PAYOUT_GATE_KEY).await {
            return value == "on";
        }
    }
    false
}

/// The per-day sponsored-promotion price right now (Wave 26):
/// the admin setting when one is stored and valid, else the
/// startup value (`PROMOTION_PRICE_PER_DAY_POISHA`, itself
/// defaulting to the domain's 100 taka a day). Mirrors
/// [`effective_commission_bps`] exactly. The price applies to
/// NEW promotions only — a bought promotion keeps its cost
/// snapshot and its stored window.
pub async fn effective_promotion_price_per_day(state: &AppState) -> i64 {
    if let Some(pool) = &state.pool {
        if let Some(value) = platform_setting_i64(pool, PROMOTION_PRICE_PER_DAY_KEY).await {
            if validate_promotion_price_per_day(value) {
                return value;
            }
        }
    }
    state.promotion_price_per_day_poisha
}

/// The price of one subscription period of `plan` right now
/// (Wave 36): the admin setting for that plan when one is
/// stored and valid, else the startup value
/// (`SUBSCRIPTION_PRICE_PRO_POISHA` /
/// `SUBSCRIPTION_PRICE_BUSINESS_POISHA`, themselves defaulting
/// to the domain's 499 / 1,499 taka). The free plan costs
/// nothing and never reaches the charge paths. Mirrors
/// [`effective_promotion_price_per_day`] exactly. The price
/// applies to NEW subscriptions and renewals — a period
/// already bought keeps the price it was charged.
pub async fn effective_subscription_price(state: &AppState, plan: &str) -> i64 {
    let (key, fallback) = match plan {
        PLAN_PRO => (
            SUBSCRIPTION_PRICE_PRO_KEY,
            state.subscription_price_pro_poisha,
        ),
        PLAN_BUSINESS => (
            SUBSCRIPTION_PRICE_BUSINESS_KEY,
            state.subscription_price_business_poisha,
        ),
        _ => return 0,
    };
    if let Some(pool) = &state.pool {
        if let Some(value) = platform_setting_i64(pool, key).await {
            if validate_subscription_price(value) {
                return value;
            }
        }
    }
    fallback
}

/// The free plan's product cap right now (Wave 36): the admin
/// setting when one is stored and valid, else the startup
/// value (`PLAN_FREE_PRODUCT_CAP`, itself defaulting to the
/// domain's 50). Only the FREE cap is tunable — the paid caps
/// are part of what the plans sell, so they stay domain
/// constants ([`domain::plan_product_cap`]).
pub async fn effective_plan_free_product_cap(state: &AppState) -> i64 {
    if let Some(pool) = &state.pool {
        if let Some(value) = platform_setting_i64(pool, PLAN_FREE_PRODUCT_CAP_KEY).await {
            if validate_plan_free_product_cap(value) {
                return value;
            }
        }
    }
    state.plan_free_product_cap
}

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

/// The platform at a glance. Money definitions are the
/// platform-canonical ones (Wave 27, restated in the domain
/// crate and the README): GMV is goods minus discount plus
/// delivery over sub-orders that were not cancelled (a
/// cancelled sub-order never moved money), and commission is
/// EARNED only when a sub-order is delivered — the snapshot
/// `commission_poisha` summed over delivered sub-orders,
/// exactly the seller-earnings definition. Everything is a
/// bigint aggregate, cast where Postgres would return numeric.
///
/// The GMV field was renamed in the overview remake
/// (`gmv_goods_poisha` → `gmv_poisha`): the old name carried a
/// pre-Wave-27 goods-only sum that disagreed with every other
/// GMV on the platform while sharing its label.
#[derive(Debug, Serialize)]
pub struct AdminOverview {
    pub users: i64,
    pub stores: i64,
    pub products: i64,
    pub orders: i64,
    pub gmv_poisha: i64,
    pub commission_earned_poisha: i64,
    pub payouts_pending: PendingPayouts,
    pub payments_pending_count: i64,
}

/// Payout requests waiting for a platform decision.
#[derive(Debug, Serialize)]
pub struct PendingPayouts {
    pub count: i64,
    pub amount_poisha: i64,
}

#[derive(FromRow)]
struct OverviewRow {
    users: i64,
    stores: i64,
    products: i64,
    orders: i64,
    gmv_poisha: i64,
    commission_earned_poisha: i64,
    payouts_pending_count: i64,
    payouts_pending_amount_poisha: i64,
    payments_pending_count: i64,
}

/// Compute the overview in one aggregate query.
pub async fn get_overview(pool: &PgPool) -> Result<AdminOverview, ApiError> {
    let row: OverviewRow = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM users) AS users, \
           (SELECT COUNT(*) FROM stores) AS stores, \
           (SELECT COUNT(*) FROM products) AS products, \
           (SELECT COUNT(*) FROM orders) AS orders, \
           (SELECT COALESCE(SUM(goods_subtotal_poisha - discount_poisha \
              + delivery_charge_poisha), 0)::bigint \
              FROM sub_orders WHERE status <> 'cancelled') AS gmv_poisha, \
           (SELECT COALESCE(SUM(commission_poisha), 0)::bigint \
              FROM sub_orders WHERE status = 'delivered') AS commission_earned_poisha, \
           (SELECT COUNT(*) FROM payouts WHERE status = 'requested') \
              AS payouts_pending_count, \
           (SELECT COALESCE(SUM(amount_poisha), 0)::bigint \
              FROM payouts WHERE status = 'requested') AS payouts_pending_amount_poisha, \
           (SELECT COUNT(*) FROM payments WHERE status = 'pending') \
              AS payments_pending_count",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("load admin overview", e))?;

    Ok(AdminOverview {
        users: row.users,
        stores: row.stores,
        products: row.products,
        orders: row.orders,
        gmv_poisha: row.gmv_poisha,
        commission_earned_poisha: row.commission_earned_poisha,
        payouts_pending: PendingPayouts {
            count: row.payouts_pending_count,
            amount_poisha: row.payouts_pending_amount_poisha,
        },
        payments_pending_count: row.payments_pending_count,
    })
}

// ---------------------------------------------------------------------------
// Stores: list + suspension
// ---------------------------------------------------------------------------

/// A store as the admin sees it: the public store fields plus the
/// owner's phone and the product count.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminStore {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub owner_phone: String,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub product_count: i64,
    /// The store's logo URL (Wave 51 extension) — the
    /// admin list shows the mark it moderates.
    pub logo_url: Option<String>,
    /// The store's follower count (Wave 54) — the admin
    /// list shows the audience a moderation decision
    /// affects.
    pub follower_count: i64,
    pub created_at: DateTime<Utc>,
}

/// A page of admin stores plus the total count.
#[derive(Debug, Serialize)]
pub struct AdminStoreList {
    pub items: Vec<AdminStore>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// List stores, newest first; `q` matches name or slug.
pub async fn list_stores(
    pool: &PgPool,
    q: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminStoreList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let pattern: Option<String> = q
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{s}%"));

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM stores s \
         WHERE ($1::text IS NULL OR s.name ILIKE $1 OR s.slug ILIKE $1)",
    )
    .bind(&pattern)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin stores", e))?;

    let items: Vec<AdminStore> = sqlx::query_as(
        "SELECT s.id, s.owner_user_id, u.phone AS owner_phone, s.slug, s.name, \
         s.description, s.status, \
         (SELECT COUNT(*) FROM products p WHERE p.store_id = s.id) AS product_count, \
         s.logo_url, \
         (SELECT COUNT(*) FROM store_follows f WHERE f.store_id = s.id) AS follower_count, \
         s.created_at \
         FROM stores s JOIN users u ON u.id = s.owner_user_id \
         WHERE ($1::text IS NULL OR s.name ILIKE $1 OR s.slug ILIKE $1) \
         ORDER BY s.created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(&pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin stores", e))?;

    Ok(AdminStoreList {
        items,
        total,
        page,
        limit,
    })
}

/// Set a store's status (`active` or `suspended`; anything else is
/// a 400). Returns the shared store view. Suspending is reversible
/// and deletes nothing — see the module docs for what it blocks.
/// Wave 28: the update and its audit row (`store.suspend` /
/// `store.reactivate`) commit in one transaction.
pub async fn set_store_status(
    pool: &PgPool,
    admin_id: Uuid,
    store_id: Uuid,
    status: &str,
) -> Result<Store, ApiError> {
    if status != "active" && status != "suspended" {
        return Err(ApiError::bad_request(
            "store status must be 'active' or 'suspended'",
        ));
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin set store status", e))?;
    let store: Store = sqlx::query_as(
        "UPDATE stores SET status = $2 WHERE id = $1 \
         RETURNING id, owner_user_id, slug, name, description, status, \
         facebook_page_id, facebook_page_name, sidra_auto_reply, \
         theme_key, brand_color, logo_url, banner_url, announcement, verified_tier, \
         about, return_policy, created_at",
    )
    .bind(store_id)
    .bind(status)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("set store status", e))?
    .ok_or_else(|| ApiError::not_found("store not found"))?;
    let action = if status == "suspended" {
        "store.suspend"
    } else {
        "store.reactivate"
    };
    record_admin_action(
        &mut *tx,
        admin_id,
        action,
        "store",
        Some(&store_id.to_string()),
        Some(serde_json::json!({ "status": status })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit set store status", e))?;
    Ok(store)
}

// ---------------------------------------------------------------------------
// Orders: list (read-only oversight)
// ---------------------------------------------------------------------------

/// List every order on the platform, newest first, optionally
/// filtered by the order-level status. Rows are built by the ONE
/// shared order loader, so the admin sees exactly what buyers see.
pub async fn list_orders(
    pool: &PgPool,
    status: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<OrderList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let status = status.map(str::trim).filter(|s| !s.is_empty());

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM orders WHERE ($1::text IS NULL OR status = $1)",
    )
    .bind(status)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin orders", e))?;

    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM orders WHERE ($1::text IS NULL OR status = $1) \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(status)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin orders", e))?;

    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        items.push(load_order(pool, id).await?);
    }
    Ok(OrderList {
        items,
        total,
        page,
        limit,
    })
}

// ---------------------------------------------------------------------------
// Payouts: queue + processing
// ---------------------------------------------------------------------------

/// A payout as the admin sees it in the queue: the seller's
/// payout view plus which store it belongs to and the context
/// a decision needs. Phone and account number are MASKED at
/// this level (the Wave 28 list canon) — the full values live
/// only in the single-payout detail ([`AdminPayoutDetail`]),
/// which is where money is actually sent from.
#[derive(Debug, Serialize)]
pub struct AdminPayout {
    pub id: Uuid,
    pub store_id: Uuid,
    pub store_name: String,
    /// The store owner's phone, masked via
    /// `domain::mask_phone`.
    pub owner_phone_masked: String,
    pub amount_poisha: i64,
    pub method: String,
    pub account_name: String,
    /// The destination account number, masked to its last
    /// four characters (for bkash/nagad the number IS a
    /// phone number). Full value: the detail view only.
    pub account_number_masked: String,
    pub status: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
    /// Per-stage decision stamps (migration 0072); `None`
    /// for a stage the payout has not reached — and, for
    /// payouts decided before the columns existed, when the
    /// backfill could not know (never fabricated).
    pub approved_at: Option<DateTime<Utc>>,
    pub paid_at: Option<DateTime<Utc>>,
    pub rejected_at: Option<DateTime<Utc>>,
    /// The acting admins' user ids for each stage; the
    /// detail view's audit history names them by masked
    /// phone.
    pub approved_by_user_id: Option<Uuid>,
    pub paid_by_user_id: Option<Uuid>,
    pub rejected_by_user_id: Option<Uuid>,
    /// The payment reference recorded at mark-paid (the
    /// seller's reconciliation proof — not a secret, so the
    /// queue shows it for paid rows).
    pub payment_reference: Option<String>,
    /// The client's `Idempotency-Key` when the request carried
    /// one (Wave 44) — an admin handling a "charged twice?"
    /// support question can see at a glance whether the two
    /// rows a seller is worried about share a key (a recognised
    /// retry) or not.
    pub idempotency_key: Option<String>,
    /// The store's current lifecycle status (`active` /
    /// `suspended`): a suspended store's payouts are frozen
    /// (see [`transition_payout`]), and the queue badges them.
    pub store_status: String,
    /// The store's current verification tier
    /// (`unverified` / `basic` / `verified`).
    pub store_verified_tier: String,
    /// The store's UNCLAMPED available balance right now
    /// (earned − paid out − pending − charges), for rows
    /// still in flight (`requested` / `approved`); `None`
    /// for terminal rows, where coverage no longer decides
    /// anything. Negative means the store's commitments
    /// exceed its earnings — the hole the approve/mark-paid
    /// re-check refuses to deepen.
    pub coverage_unclamped_poisha: Option<i64>,
    /// `coverage_unclamped_poisha >= 0`, for in-flight rows;
    /// `None` for terminal rows. A snapshot for triage —
    /// the transition re-checks inside its transaction and
    /// remains the authority.
    pub covered: Option<bool>,
    /// Another in-flight payout exists for the same store
    /// and the SAME amount (a possible keyless double
    /// submit — the one duplicate shape the idempotency key
    /// cannot rule out). A warning to look before approving,
    /// never proof: two equal legitimate requests exist.
    pub possible_duplicate: bool,
}

/// One status slice of the whole payout queue (admin payouts
/// remake): the same triple the overview's `pending_payouts`
/// block reports for `requested` — count, total amount, and
/// the age of the oldest row in the slice, always since the
/// row was FILED (`created_at`), so the surfaces can never
/// disagree about what the number means.
#[derive(Debug, Serialize)]
pub struct PayoutStatusBucket {
    pub count: i64,
    pub amount_poisha: i64,
    pub oldest_age_secs: Option<i64>,
}

/// Per-status totals over the WHOLE queue — never the
/// filtered page — in the admin payout list response. The
/// `paid` / `rejected` slices are lifetime totals; the
/// `requested` slice is identical to the overview's
/// `pending_payouts` by construction of the same query
/// shape (and pinned equal by test).
#[derive(Debug, Serialize)]
pub struct PayoutStatusSummary {
    pub requested: PayoutStatusBucket,
    pub approved: PayoutStatusBucket,
    pub paid: PayoutStatusBucket,
    pub rejected: PayoutStatusBucket,
}

/// A page of admin payouts plus the total count and the
/// whole-queue status summary.
#[derive(Debug, Serialize)]
pub struct AdminPayoutList {
    pub items: Vec<AdminPayout>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
    pub summary: PayoutStatusSummary,
}

/// The raw queue row, before masking and coverage are
/// applied. Carries the full phone / account number (the
/// detail view needs them) and the store fields the list
/// mapping does not surface.
#[derive(Debug, FromRow)]
struct AdminPayoutRow {
    id: Uuid,
    store_id: Uuid,
    store_name: String,
    store_slug: String,
    store_status: String,
    store_verified_tier: String,
    owner_user_id: Uuid,
    owner_phone: String,
    amount_poisha: i64,
    method: String,
    account_name: String,
    account_number: String,
    status: String,
    note: Option<String>,
    created_at: DateTime<Utc>,
    processed_at: Option<DateTime<Utc>>,
    approved_at: Option<DateTime<Utc>>,
    paid_at: Option<DateTime<Utc>>,
    rejected_at: Option<DateTime<Utc>>,
    approved_by_user_id: Option<Uuid>,
    paid_by_user_id: Option<Uuid>,
    rejected_by_user_id: Option<Uuid>,
    payment_reference: Option<String>,
    idempotency_key: Option<String>,
    possible_duplicate: bool,
}

/// The queue projection shared by the list, the detail and
/// the detail's store history: one SELECT so the three can
/// never drift apart. `$1` is the caller's WHERE tail; the
/// projection itself is constant.
const ADMIN_PAYOUT_PROJECTION: &str = "p.id, p.store_id, s.name AS store_name, \
    s.slug AS store_slug, s.status AS store_status, \
    s.verified_tier AS store_verified_tier, s.owner_user_id, \
    u.phone AS owner_phone, p.amount_poisha, p.method, p.account_name, \
    p.account_number, p.status, p.note, p.created_at, p.processed_at, \
    p.approved_at, p.paid_at, p.rejected_at, p.approved_by_user_id, \
    p.paid_by_user_id, p.rejected_by_user_id, p.payment_reference, \
    p.idempotency_key, \
    EXISTS (SELECT 1 FROM payouts d \
             WHERE d.store_id = p.store_id AND d.id <> p.id \
               AND d.amount_poisha = p.amount_poisha \
               AND d.status IN ('requested', 'approved')) AS possible_duplicate \
    FROM payouts p \
    JOIN stores s ON s.id = p.store_id \
    JOIN users u ON u.id = s.owner_user_id";

/// Mask an account number to its last four characters —
/// enough to recognise a destination, never enough to send
/// to it.
fn mask_account_number(number: &str) -> String {
    let chars: Vec<char> = number.chars().collect();
    if chars.len() > 4 {
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("••••{tail}")
    } else {
        "••••".to_string()
    }
}

/// Map raw rows to queue views, attaching the per-store
/// coverage snapshot to in-flight rows in ONE batched
/// earnings read (never a query per row).
async fn payout_views(
    pool: &PgPool,
    rows: Vec<AdminPayoutRow>,
) -> Result<Vec<AdminPayout>, ApiError> {
    let in_flight_stores: Vec<Uuid> = rows
        .iter()
        .filter(|row| row.status == "requested" || row.status == "approved")
        .map(|row| row.store_id)
        .collect();
    let earnings_by_store = if in_flight_stores.is_empty() {
        std::collections::HashMap::new()
    } else {
        crate::payouts::seller_earnings_for_stores(pool, &in_flight_stores).await?
    };

    Ok(rows
        .into_iter()
        .map(|row| {
            let (coverage_unclamped_poisha, covered) =
                match earnings_by_store.get(&row.store_id) {
                    Some(earnings)
                        if row.status == "requested" || row.status == "approved" =>
                    {
                        let unclamped = earnings.earned_poisha
                            - earnings.paid_out_poisha
                            - earnings.pending_poisha
                            - earnings.charges_total_poisha;
                        (Some(unclamped), Some(unclamped >= 0))
                    }
                    _ => (None, None),
                };
            AdminPayout {
                id: row.id,
                store_id: row.store_id,
                store_name: row.store_name,
                owner_phone_masked: mask_phone(&row.owner_phone),
                amount_poisha: row.amount_poisha,
                method: row.method,
                account_name: row.account_name,
                account_number_masked: mask_account_number(&row.account_number),
                status: row.status,
                note: row.note,
                created_at: row.created_at,
                processed_at: row.processed_at,
                approved_at: row.approved_at,
                paid_at: row.paid_at,
                rejected_at: row.rejected_at,
                approved_by_user_id: row.approved_by_user_id,
                paid_by_user_id: row.paid_by_user_id,
                rejected_by_user_id: row.rejected_by_user_id,
                payment_reference: row.payment_reference,
                idempotency_key: row.idempotency_key,
                store_status: row.store_status,
                store_verified_tier: row.store_verified_tier,
                coverage_unclamped_poisha,
                covered,
                possible_duplicate: row.possible_duplicate,
            }
        })
        .collect())
}

/// The queue's ORDER BY fragment for a validated sort —
/// a fixed string per variant (user input never reaches
/// the SQL text). Every order is TOTAL (the `id` tail
/// breaks timestamp ties) so pages can neither repeat nor
/// skip a row.
fn payout_order_by(sort: PayoutSort) -> &'static str {
    match sort {
        PayoutSort::Newest => "ORDER BY p.created_at DESC, p.id DESC",
        PayoutSort::Oldest => "ORDER BY p.created_at ASC, p.id ASC",
        PayoutSort::AmountDesc => {
            "ORDER BY p.amount_poisha DESC, p.created_at DESC, p.id DESC"
        }
        PayoutSort::AmountAsc => {
            "ORDER BY p.amount_poisha ASC, p.created_at DESC, p.id DESC"
        }
    }
}

/// The whole-queue per-status summary (see
/// [`PayoutStatusSummary`]): one FILTER scan over `payouts`,
/// in the gift-cards/analytics aggregate style. Deliberately
/// unfiltered — the summary describes the queue, not the
/// page.
async fn payout_status_summary(pool: &PgPool) -> Result<PayoutStatusSummary, ApiError> {
    #[derive(FromRow)]
    struct PayoutSummaryRow {
        requested_count: i64,
        requested_amount_poisha: i64,
        requested_oldest_age_secs: Option<i64>,
        approved_count: i64,
        approved_amount_poisha: i64,
        approved_oldest_age_secs: Option<i64>,
        paid_count: i64,
        paid_amount_poisha: i64,
        paid_oldest_age_secs: Option<i64>,
        rejected_count: i64,
        rejected_amount_poisha: i64,
        rejected_oldest_age_secs: Option<i64>,
    }

    let row: PayoutSummaryRow = sqlx::query_as(
        "SELECT \
           (COUNT(*) FILTER (WHERE status = 'requested'))::bigint AS requested_count, \
           COALESCE(SUM(amount_poisha) FILTER (WHERE status = 'requested'), 0)::bigint \
             AS requested_amount_poisha, \
           EXTRACT(EPOCH FROM (now() - MIN(created_at) \
             FILTER (WHERE status = 'requested')))::bigint AS requested_oldest_age_secs, \
           (COUNT(*) FILTER (WHERE status = 'approved'))::bigint AS approved_count, \
           COALESCE(SUM(amount_poisha) FILTER (WHERE status = 'approved'), 0)::bigint \
             AS approved_amount_poisha, \
           EXTRACT(EPOCH FROM (now() - MIN(created_at) \
             FILTER (WHERE status = 'approved')))::bigint AS approved_oldest_age_secs, \
           (COUNT(*) FILTER (WHERE status = 'paid'))::bigint AS paid_count, \
           COALESCE(SUM(amount_poisha) FILTER (WHERE status = 'paid'), 0)::bigint \
             AS paid_amount_poisha, \
           EXTRACT(EPOCH FROM (now() - MIN(created_at) \
             FILTER (WHERE status = 'paid')))::bigint AS paid_oldest_age_secs, \
           (COUNT(*) FILTER (WHERE status = 'rejected'))::bigint AS rejected_count, \
           COALESCE(SUM(amount_poisha) FILTER (WHERE status = 'rejected'), 0)::bigint \
             AS rejected_amount_poisha, \
           EXTRACT(EPOCH FROM (now() - MIN(created_at) \
             FILTER (WHERE status = 'rejected')))::bigint AS rejected_oldest_age_secs \
         FROM payouts",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("payout status summary", e))?;

    Ok(PayoutStatusSummary {
        requested: PayoutStatusBucket {
            count: row.requested_count,
            amount_poisha: row.requested_amount_poisha,
            oldest_age_secs: row.requested_oldest_age_secs,
        },
        approved: PayoutStatusBucket {
            count: row.approved_count,
            amount_poisha: row.approved_amount_poisha,
            oldest_age_secs: row.approved_oldest_age_secs,
        },
        paid: PayoutStatusBucket {
            count: row.paid_count,
            amount_poisha: row.paid_amount_poisha,
            oldest_age_secs: row.paid_oldest_age_secs,
        },
        rejected: PayoutStatusBucket {
            count: row.rejected_count,
            amount_poisha: row.rejected_amount_poisha,
            oldest_age_secs: row.rejected_oldest_age_secs,
        },
    })
}

/// The payout processing queue (admin payouts remake):
/// filterable by lifecycle status and a search term,
/// sortable, and carrying the whole-queue status summary.
///
/// - `status`: one of the four lifecycle values; an unknown
///   value is a 400, never a silently empty queue (an admin
///   filtering by a typo must not conclude there is nothing
///   to pay).
/// - `q`: matches the store name, the owner's phone and the
///   destination account number (substring, LIKE-escaped via
///   the domain helper), a phone in local form (the stored
///   canonical form contains it as a substring), and the
///   idempotency key EXACTLY or by prefix (case-sensitive —
///   key equality in this system is byte-for-byte, so the
///   search is too).
/// - `sort`: validated by the domain; the ORDER BY fragment
///   is a fixed string per variant.
pub async fn list_payouts(
    pool: &PgPool,
    status: Option<&str>,
    q: Option<&str>,
    sort: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminPayoutList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let status = status.map(str::trim).filter(|s| !s.is_empty());
    if let Some(status) = status {
        if !is_valid_payout_status(status) {
            return Err(ApiError::bad_request(
                "status must be one of: requested, approved, paid, rejected",
            ));
        }
    }
    let sort = validate_payout_list_sort(sort).map_err(ApiError::bad_request)?;

    let q = q.map(str::trim).filter(|s| !s.is_empty());
    let contains_pattern = q.map(search_like_pattern);
    let local_pattern = q
        .and_then(to_local_phone)
        .map(|local| search_like_pattern(&local));
    let raw_q = q.map(str::to_owned);

    let summary = payout_status_summary(pool).await?;

    // The COUNT carries the same joins and predicates as the
    // page query — the two must never diverge.
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM payouts p \
         JOIN stores s ON s.id = p.store_id \
         JOIN users u ON u.id = s.owner_user_id \
         WHERE ($1::text IS NULL OR p.status = $1) \
           AND ($2::text IS NULL OR s.name ILIKE $2 OR u.phone ILIKE $2 \
                OR p.account_number ILIKE $2 \
                OR ($3::text IS NOT NULL \
                    AND (u.phone ILIKE $3 OR p.account_number ILIKE $3)) \
                OR ($4::text IS NOT NULL \
                    AND (p.idempotency_key = $4 \
                         OR starts_with(p.idempotency_key, $4))))",
    )
    .bind(status)
    .bind(&contains_pattern)
    .bind(&local_pattern)
    .bind(&raw_q)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin payouts", e))?;

    let rows: Vec<AdminPayoutRow> = sqlx::query_as(&format!(
        "SELECT {ADMIN_PAYOUT_PROJECTION} \
         WHERE ($1::text IS NULL OR p.status = $1) \
           AND ($2::text IS NULL OR s.name ILIKE $2 OR u.phone ILIKE $2 \
                OR p.account_number ILIKE $2 \
                OR ($3::text IS NOT NULL \
                    AND (u.phone ILIKE $3 OR p.account_number ILIKE $3)) \
                OR ($4::text IS NOT NULL \
                    AND (p.idempotency_key = $4 \
                         OR starts_with(p.idempotency_key, $4)))) \
         {} LIMIT $5 OFFSET $6",
        payout_order_by(sort)
    ))
    .bind(status)
    .bind(&contains_pattern)
    .bind(&local_pattern)
    .bind(&raw_q)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin payouts", e))?;

    let items = payout_views(pool, rows).await?;

    Ok(AdminPayoutList {
        items,
        total,
        page,
        limit,
        summary,
    })
}

/// The Bangla label of a payout status, for error prose —
/// the same words the web's `payoutStatusLabel` shows.
fn payout_status_label_bn(status: &str) -> &str {
    match status {
        "requested" => "অনুরোধ করা হয়েছে",
        "approved" => "অনুমোদিত",
        "paid" => "পরিশোধিত",
        "rejected" => "প্রত্যাখ্যাত",
        _ => status,
    }
}

/// THE payout processing function: approve, reject and mark-paid
/// are three thin handlers over this one service. The domain
/// transition table decides legality (an illegal move is a 409);
/// a legal move stamps `processed_at` AND its per-stage stamp +
/// actor (migration 0072). Wave 28: the whole move — status
/// read, update, and the audit row (`payout.approve` /
/// `payout.reject` / `payout.mark_paid`) — runs in ONE
/// transaction, so the audit log and the payout can never
/// disagree.
///
/// Admin payouts remake — the decision gates, in the order
/// they fire (the precedence is deliberate and pinned by
/// test):
///
/// 1. INPUT, before any database work (400): a rejection
///    requires a reason ([`validate_payout_rejection_reason`])
///    and mark-paid requires a payment reference
///    ([`validate_payment_reference`]).
/// 2. EXISTENCE (404): the payout's store is resolved by a
///    plain read before any lock is taken.
/// 3. LOCKS, in the platform's one order — store row first,
///    then the payout row — the same order payout creation,
///    promotions and subscriptions already use, so no two
///    money paths can deadlock against each other.
/// 4. LEGALITY (409): `can_transition_payout`.
/// 5. SUSPENSION (409): a suspended store's payouts are
///    frozen for approve and mark-paid. Reject is exempt —
///    it releases the seller's hold, and freezing it would
///    trap the amount with no exit.
/// 6. KYC (400, approve only): when the platform's KYC
///    payout gate is on, a store that is unverified NOW
///    cannot be approved — the creation-time gate alone let
///    a payout requested before the gate (or before a tier
///    loss) sail through. Same error class as creation's.
/// 7. COVERAGE (409): the Wave 39 re-check, extended from
///    approve to mark-paid. Earnings can drop after approval
///    (a delivered order is returned, a charge lands) and
///    `paid` is final — the last moment to catch an
///    uncovered payout is before the send is recorded. The
///    check uses the UNCLAMPED balance, and the refusal
///    carries the full earnings picture as structured
///    details, computed inside this transaction.
pub async fn transition_payout(
    state: &AppState,
    admin_id: Uuid,
    payout_id: Uuid,
    target_status: &str,
    note: Option<&str>,
    payment_reference: Option<&str>,
) -> Result<Payout, ApiError> {
    // Gate 1 — pure input, validated before the transaction
    // opens (the verification-reject precedent: a malformed
    // decision is a 400 whatever the payout's state).
    let rejection_reason: Option<String> = if target_status == "rejected" {
        Some(
            validate_payout_rejection_reason(note.unwrap_or(""))
                .map_err(ApiError::bad_request)?,
        )
    } else {
        None
    };
    let payment_reference: Option<String> = if target_status == "paid" {
        Some(
            validate_payment_reference(payment_reference.unwrap_or(""))
                .map_err(ApiError::bad_request)?,
        )
    } else {
        None
    };

    let pool = state.require_pool()?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin payout transition", e))?;

    // Gate 2 — existence, before locks.
    let store_id: Option<Uuid> =
        sqlx::query_scalar("SELECT store_id FROM payouts WHERE id = $1")
            .bind(payout_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("load payout for transition", e))?;
    let store_id = store_id.ok_or_else(|| ApiError::not_found("payout not found"))?;

    // Gate 3 — the locks, store first. The store row carries
    // the two facts the freeze and KYC gates read; reading
    // them under the lock serialises against `set_store_status`
    // and verification decisions taken concurrently.
    let (store_status, verified_tier): (String, String) = sqlx::query_as(
        "SELECT status, verified_tier FROM stores WHERE id = $1 FOR UPDATE",
    )
    .bind(store_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock store for payout transition", e))?;
    let (current, amount_poisha): (String, i64) =
        sqlx::query_as("SELECT status, amount_poisha FROM payouts WHERE id = $1 FOR UPDATE")
            .bind(payout_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("lock payout for transition", e))?;

    // Gate 4 — legality.
    if !can_transition_payout(&current, target_status) {
        return Err(ApiError::conflict(format!(
            "এই পে-আউটটি এখন {} ('{current}') অবস্থায় আছে — '{target_status}' করা যাবে না। \
             অন্য কোনো অ্যাডমিন হয়তো ইতিমধ্যে এটি প্রসেস করেছেন — তালিকা রিফ্রেশ করে \
             আবার দেখুন (this payout is now '{current}' — it cannot be moved to \
             '{target_status}'; another admin may have already processed it — refresh \
             the list and check again)",
            payout_status_label_bn(&current)
        )));
    }

    // Gate 5 — the suspension freeze.
    if (target_status == "approved" || target_status == "paid") && store_status == "suspended" {
        return Err(ApiError::conflict(
            "স্টোরটি সাসপেন্ডেড আছে — সাসপেন্ডেড থাকা অবস্থায় এই পে-আউট প্রসেস করা \
             যাবে না। আগে স্টোরটি সক্রিয় করুন (this store is suspended — its payouts \
             are frozen while suspended; reactivate the store first)",
        ));
    }

    // Gate 6 — the KYC gate, re-checked at approval. The
    // settings read goes through the pool, outside this
    // transaction (settings are platform-global, exactly as
    // at creation; a flip racing this approval is a policy
    // change mid-flight, not a money race).
    if target_status == "approved"
        && effective_kyc_payout_gate(state).await
        && verified_tier == "unverified"
    {
        return Err(ApiError::bad_request(
            "স্টোরটি যাচাইকৃত নয় — KYC গেট চালু থাকায় এই পে-আউট অনুমোদন করা যাবে না \
             (cannot approve this payout: payouts require a verified store while the \
             KYC payout gate is on — the seller must complete store verification first)",
        ));
    }

    // Gate 7 — coverage (Wave 39, extended to mark-paid).
    if target_status == "approved" || target_status == "paid" {
        let earnings = crate::payouts::seller_earnings_on(&mut *tx, store_id).await?;
        let unclamped_available = earnings.earned_poisha
            - earnings.paid_out_poisha
            - earnings.pending_poisha
            - earnings.charges_total_poisha;
        if unclamped_available < 0 {
            let message = if target_status == "approved" {
                "পে-আউট অনুমোদন করা যাচ্ছে না — স্টোরের বর্তমান ব্যালেন্স এই পে-আউটের \
                 জন্য যথেষ্ট নয় — হয় অনুরোধটি প্রত্যাখ্যান করুন, নয়তো ব্যালেন্স আবার \
                 যথেষ্ট হওয়া পর্যন্ত অপেক্ষা করুন (cannot approve: the store's current \
                 available balance no longer covers its pending payouts — reject the \
                 request, or wait until the balance covers it again)"
            } else {
                "পে-আউট পরিশোধিত চিহ্নিত করা যাচ্ছে না — স্টোরের বর্তমান ব্যালেন্স এই \
                 পে-আউটের জন্য যথেষ্ট নয় — হয় অনুরোধটি প্রত্যাখ্যান করুন, নয়তো \
                 ব্যালেন্স আবার যথেষ্ট হওয়া পর্যন্ত অপেক্ষা করুন (cannot mark paid: \
                 the store's current available balance no longer covers this payout — \
                 reject the request, or wait until the balance covers it again)"
            };
            return Err(ApiError::conflict(message).with_details(serde_json::json!({
                "kind": "payout_coverage",
                "payout_id": payout_id,
                "store_id": store_id,
                "amount_poisha": amount_poisha,
                "earned_poisha": earnings.earned_poisha,
                "paid_out_poisha": earnings.paid_out_poisha,
                "pending_poisha": earnings.pending_poisha,
                "charges_total_poisha": earnings.charges_total_poisha,
                "available_unclamped_poisha": unclamped_available,
                "shortfall_poisha": -unclamped_available,
            })));
        }
    }

    let payout: Payout = sqlx::query_as(&format!(
        "UPDATE payouts SET status = $2, processed_at = now(), \
         note = COALESCE($3, note), \
         approved_at = CASE WHEN $2 = 'approved' THEN now() ELSE approved_at END, \
         approved_by_user_id = CASE WHEN $2 = 'approved' THEN $4 \
                                    ELSE approved_by_user_id END, \
         paid_at = CASE WHEN $2 = 'paid' THEN now() ELSE paid_at END, \
         paid_by_user_id = CASE WHEN $2 = 'paid' THEN $4 ELSE paid_by_user_id END, \
         rejected_at = CASE WHEN $2 = 'rejected' THEN now() ELSE rejected_at END, \
         rejected_by_user_id = CASE WHEN $2 = 'rejected' THEN $4 \
                                    ELSE rejected_by_user_id END, \
         payment_reference = COALESCE($5, payment_reference) \
         WHERE id = $1 RETURNING {PAYOUT_COLUMNS}"
    ))
    .bind(payout_id)
    .bind(target_status)
    .bind(rejection_reason.as_deref())
    .bind(admin_id)
    .bind(payment_reference.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("transition payout", e))?;

    let action = match target_status {
        "approved" => "payout.approve",
        "rejected" => "payout.reject",
        "paid" => "payout.mark_paid",
        _ => "payout.transition",
    };
    let mut detail = serde_json::json!({ "from": current, "to": target_status });
    if let Some(reason) = &rejection_reason {
        detail["note"] = serde_json::Value::String(reason.clone());
    }
    if let Some(reference) = &payment_reference {
        detail["payment_reference"] = serde_json::Value::String(reference.clone());
    }
    record_admin_action(
        &mut *tx,
        admin_id,
        action,
        "payout",
        Some(&payout_id.to_string()),
        Some(detail),
    )
    .await?;
    // Wave 29 + remake: every decision queues the seller's
    // notice in the same transaction (the outbox rule — see
    // the `notices` module): approval announces the approval
    // (never a send), rejection carries the reason, payment
    // carries the recorded reference.
    let notice_kind = match target_status {
        "approved" => Some(domain::NOTIFICATION_PAYOUT_APPROVED),
        "rejected" => Some(domain::NOTIFICATION_PAYOUT_REJECTED),
        "paid" => Some(domain::NOTIFICATION_PAYOUT_PAID),
        _ => None,
    };
    if let Some(kind) = notice_kind {
        crate::notices::enqueue_payout_notice(
            &mut tx,
            payout.store_id,
            payout.amount_poisha,
            kind,
            rejection_reason.as_deref(),
            payment_reference.as_deref(),
        )
        .await?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit payout transition", e))?;
    Ok(payout)
}

/// The store block of the payout detail: the facts about
/// the store a decision depends on that the flattened queue
/// row does not carry (the name and id are already there).
#[derive(Debug, Serialize)]
pub struct PayoutStoreSummary {
    pub slug: String,
    pub status: String,
    pub verified_tier: String,
}

/// Money still at risk from returns, for the payout detail
/// (admin payouts remake): the two exposures are different
/// money and are never summed together.
#[derive(Debug, Serialize)]
pub struct PayoutReturnsExposure {
    /// Returns asked for or approved but not yet received.
    pub in_flight_count: i64,
    /// The seller-payout value of the still-delivered
    /// sub-orders those returns target: it sits inside
    /// `earned_poisha` today and leaves it the moment a
    /// return is received — the approve/mark-paid coverage
    /// hole, made visible before it lands.
    pub at_risk_payout_poisha: i64,
    /// Returns already received whose buyer refund is still
    /// pending. That money is already OUT of earnings; it
    /// is platform cash owed to buyers, shown for context.
    pub received_refund_pending_count: i64,
    pub received_refund_pending_poisha: i64,
}

/// One payout in full, for the admin who must decide on it
/// (admin payouts remake): the queue row (flattened) plus
/// the full destination (the ONLY admin payout response
/// that carries it), the earnings behind the amount, the
/// returns exposure, the seller's recent charges, the
/// verification state, the store's current account beside
/// the payout's snapshot, the store's other payouts, and
/// this payout's audit history.
///
/// The detail is an ADVISORY snapshot assembled from
/// sequential reads; the transition's in-transaction
/// re-checks remain the authority for every decision.
#[derive(Debug, Serialize)]
pub struct AdminPayoutDetail {
    #[serde(flatten)]
    pub payout: AdminPayout,
    /// The store owner's full phone — detail only.
    pub owner_phone: String,
    /// The destination account number in full — detail
    /// only; this is the number money is sent to (always
    /// the payout's request-time SNAPSHOT, never the
    /// store's current account).
    pub account_number: String,
    pub store: PayoutStoreSummary,
    /// The store's earnings, the one canonical aggregate.
    pub earnings: crate::payouts::Earnings,
    /// earned − paid out − pending − charges, unclamped.
    pub unclamped_available_poisha: i64,
    /// `unclamped_available_poisha >= 0` — the store's
    /// total position covers its commitments. (Whether
    /// THIS payout is covered is that same fact while it
    /// sits in `requested` / `approved`: its amount is
    /// inside the subtracted totals.)
    pub covered: bool,
    pub returns_exposure: PayoutReturnsExposure,
    /// The seller's most recent charges (all their stores —
    /// the scope the earnings aggregate subtracts).
    pub charges_recent: Vec<crate::payouts::SellerChargeView>,
    /// Copied from `earnings.charges_total_poisha` — never
    /// a second aggregate, so the block cannot disagree
    /// with the earnings above it.
    pub charges_total_poisha: i64,
    pub charges_count: i64,
    /// The store's verification state, masked by
    /// construction; a store that never applied reads
    /// status `none`.
    pub verification: crate::verification::SellerVerificationView,
    /// The store's CURRENT payout account, beside the
    /// snapshot this payout carries.
    pub current_account: Option<crate::payouts::PayoutAccount>,
    /// The current account's (method, name, number) differ
    /// from this payout's snapshot — including "no current
    /// account at all". A warning to re-confirm with the
    /// seller, NEVER a signal to pay the current account:
    /// payouts always go to their snapshot.
    pub account_diverged: bool,
    /// The store's other payouts, newest first, at most
    /// ten (the subject payout is excluded; it is the
    /// flattened row above).
    pub store_payout_history: Vec<AdminPayout>,
    /// Every audit entry recorded against this payout,
    /// newest first, at most twenty.
    pub audit_entries: Vec<crate::audit::AdminAuditEntry>,
    /// The ids of the other in-flight payouts behind
    /// `possible_duplicate`, for one-click comparison.
    pub duplicate_candidate_ids: Vec<Uuid>,
}

/// Assemble the payout detail (see [`AdminPayoutDetail`]).
/// The payout row is read first and alone: an unknown id is
/// a 404 before any sub-query runs.
pub async fn get_payout_detail(
    pool: &PgPool,
    payout_id: Uuid,
) -> Result<AdminPayoutDetail, ApiError> {
    let row: Option<AdminPayoutRow> = sqlx::query_as(&format!(
        "SELECT {ADMIN_PAYOUT_PROJECTION} WHERE p.id = $1"
    ))
    .bind(payout_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load admin payout detail", e))?;
    let row = row.ok_or_else(|| ApiError::not_found("payout not found"))?;

    let store_id = row.store_id;
    let owner_user_id = row.owner_user_id;
    let store_slug = row.store_slug.clone();
    let store_status = row.store_status.clone();
    let verified_tier = row.store_verified_tier.clone();
    let owner_phone = row.owner_phone.clone();
    let account_number = row.account_number.clone();
    let snapshot = (
        row.method.clone(),
        row.account_name.clone(),
        row.account_number.clone(),
    );

    let earnings = crate::payouts::seller_earnings(pool, store_id).await?;
    let unclamped_available_poisha = earnings.earned_poisha
        - earnings.paid_out_poisha
        - earnings.pending_poisha
        - earnings.charges_total_poisha;

    #[derive(FromRow)]
    struct ReturnsExposureRow {
        in_flight_count: i64,
        at_risk_payout_poisha: i64,
        received_refund_pending_count: i64,
        received_refund_pending_poisha: i64,
    }
    let exposure: ReturnsExposureRow = sqlx::query_as(
        "SELECT \
           (COUNT(*) FILTER (WHERE rr.status IN ('requested', 'approved')))::bigint \
             AS in_flight_count, \
           COALESCE(SUM(so.seller_payout_poisha) FILTER \
             (WHERE rr.status IN ('requested', 'approved') \
                AND so.status = 'delivered'), 0)::bigint AS at_risk_payout_poisha, \
           (COUNT(*) FILTER (WHERE rr.refund_status = 'pending'))::bigint \
             AS received_refund_pending_count, \
           COALESCE(SUM(rr.refund_poisha) FILTER \
             (WHERE rr.refund_status = 'pending'), 0)::bigint \
             AS received_refund_pending_poisha \
         FROM return_requests rr \
         JOIN sub_orders so ON so.id = rr.sub_order_id \
         WHERE rr.store_id = $1",
    )
    .bind(store_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("load payout returns exposure", e))?;

    let charges_recent =
        crate::payouts::list_recent_seller_charges(pool, owner_user_id, 10).await?;
    let charges_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM seller_charges WHERE seller_user_id = $1")
            .bind(owner_user_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("count seller charges", e))?;

    let verification =
        crate::verification::admin_verification_summary(pool, store_id, &verified_tier).await?;

    let current_account =
        crate::payouts::get_payout_account_for_store(pool, store_id).await?;
    let account_diverged = match &current_account {
        None => true,
        Some(account) => {
            account.method != snapshot.0
                || account.account_name != snapshot.1
                || account.account_number != snapshot.2
        }
    };

    let history_rows: Vec<AdminPayoutRow> = sqlx::query_as(&format!(
        "SELECT {ADMIN_PAYOUT_PROJECTION} \
         WHERE p.store_id = $1 AND p.id <> $2 \
         ORDER BY p.created_at DESC, p.id DESC LIMIT 10"
    ))
    .bind(store_id)
    .bind(payout_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list store payout history", e))?;
    let store_payout_history = payout_views(pool, history_rows).await?;

    let audit_entries =
        crate::audit::list_audit_for_target(pool, "payout", &payout_id.to_string(), 20).await?;

    let duplicate_candidate_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM payouts \
         WHERE store_id = $1 AND id <> $2 AND amount_poisha = $3 \
           AND status IN ('requested', 'approved') \
         ORDER BY created_at DESC LIMIT 5",
    )
    .bind(store_id)
    .bind(payout_id)
    .bind(row.amount_poisha)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load payout duplicate candidates", e))?;

    let mut views = payout_views(pool, vec![row]).await?;
    let payout = views.pop().ok_or_else(|| {
        ApiError::internal_any("assemble payout detail", "payout view missing")
    })?;

    Ok(AdminPayoutDetail {
        payout,
        owner_phone,
        account_number,
        store: PayoutStoreSummary {
            slug: store_slug,
            status: store_status,
            verified_tier: verified_tier.clone(),
        },
        unclamped_available_poisha,
        covered: unclamped_available_poisha >= 0,
        returns_exposure: PayoutReturnsExposure {
            in_flight_count: exposure.in_flight_count,
            at_risk_payout_poisha: exposure.at_risk_payout_poisha,
            received_refund_pending_count: exposure.received_refund_pending_count,
            received_refund_pending_poisha: exposure.received_refund_pending_poisha,
        },
        charges_recent,
        charges_total_poisha: earnings.charges_total_poisha,
        charges_count,
        verification,
        current_account,
        account_diverged,
        store_payout_history,
        audit_entries,
        duplicate_candidate_ids,
        earnings,
    })
}

/// A user as the admin sees them: the public user fields plus how
/// many stores they own.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminUser {
    pub id: Uuid,
    pub phone: String,
    pub name: Option<String>,
    pub role: String,
    pub is_admin: bool,
    pub store_count: i64,
    pub created_at: DateTime<Utc>,
}

/// A page of admin users plus the total count.
#[derive(Debug, Serialize)]
pub struct AdminUserList {
    pub items: Vec<AdminUser>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// List users in registration order; `q` matches a phone substring
/// (numbers are how people are found on this platform).
pub async fn list_users(
    pool: &PgPool,
    q: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminUserList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let pattern: Option<String> = q
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{s}%"));

    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE ($1::text IS NULL OR phone ILIKE $1)")
            .bind(&pattern)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("count admin users", e))?;

    let items: Vec<AdminUser> = sqlx::query_as(
        "SELECT u.id, u.phone, u.name, u.role, u.is_admin, \
         (SELECT COUNT(*) FROM stores s WHERE s.owner_user_id = u.id) AS store_count, \
         u.created_at \
         FROM users u WHERE ($1::text IS NULL OR u.phone ILIKE $1) \
         ORDER BY u.created_at ASC LIMIT $2 OFFSET $3",
    )
    .bind(&pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin users", e))?;

    Ok(AdminUserList {
        items,
        total,
        page,
        limit,
    })
}

/// Set or clear another user's admin flag. The caller may NOT
/// change their own flag (400): self-demotion could lock the
/// platform out of its last admin, and self-promotion is moot
/// (only admins can call this at all).
pub async fn set_user_admin(
    pool: &PgPool,
    caller: &User,
    target_user_id: Uuid,
    is_admin: bool,
) -> Result<User, ApiError> {
    if target_user_id == caller.id {
        return Err(ApiError::bad_request(
            "you cannot change your own admin flag",
        ));
    }
    sqlx::query_as(
        "UPDATE users SET is_admin = $2 WHERE id = $1 \
         RETURNING id, phone, name, role, is_admin, created_at",
    )
    .bind(target_user_id)
    .bind(is_admin)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("set user admin flag", e))?
    .ok_or_else(|| ApiError::not_found("user not found"))
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// The settings view: EFFECTIVE values (what checkout and payout
/// creation actually use), not raw rows.
#[derive(Debug, Serialize)]
pub struct AdminSettings {
    pub commission_bps: i64,
    pub min_payout_poisha: i64,
    /// COD advance rate in basis points (Wave 17).
    pub advance_percent_bps: i64,
    /// COD advance threshold in poisha (Wave 17); 0 disables
    /// threshold-based advances.
    pub advance_threshold_poisha: i64,
    /// KYC payout gate (Wave 21): when true, a store must hold
    /// at least the `basic` verification tier to request a
    /// payout. Defaults to false — see
    /// [`effective_kyc_payout_gate`].
    pub kyc_payout_gate: bool,
    /// Per-day sponsored-promotion price in poisha (Wave 26),
    /// charged to NEW promotions — see
    /// [`effective_promotion_price_per_day`].
    pub promotion_price_per_day_poisha: i64,
    /// Pro plan price per 30-day period in poisha (Wave 36),
    /// charged to NEW subscriptions and renewals — see
    /// [`effective_subscription_price`].
    pub subscription_price_pro_poisha: i64,
    /// Business plan price per 30-day period in poisha
    /// (Wave 36). See [`AdminSettings::subscription_price_pro_poisha`].
    pub subscription_price_business_poisha: i64,
    /// The free plan's product cap (Wave 36) — see
    /// [`effective_plan_free_product_cap`].
    pub plan_free_product_cap: i64,
    /// Per-product image cap (Wave 51 extension) — see
    /// `product_images::effective_media_limits`.
    pub media_max_images_per_product: i64,
    /// Per-store media quota in megabytes (Wave 51
    /// extension) — see
    /// `product_images::effective_media_limits`.
    pub media_store_quota_mb: i64,
    /// Delivery-time estimate for Inside Dhaka, in days
    /// (Wave 54): [earliest, latest] — see the
    /// `delivery_estimates` module. Serialized as a
    /// two-element array.
    pub delivery_days_inside_dhaka: (i64, i64),
    /// Delivery-time estimate for the Dhaka suburbs
    /// (Wave 54), [earliest, latest] days.
    pub delivery_days_dhaka_suburb: (i64, i64),
    /// Delivery-time estimate for outside Dhaka
    /// (Wave 54), [earliest, latest] days.
    pub delivery_days_outside_dhaka: (i64, i64),
    pub platform_name: &'static str,
}

/// The current effective settings.
pub async fn get_settings(state: &AppState) -> AdminSettings {
    // Wave 51 extension: the media limits live in the
    // product_images module (their enforcement home); the
    // settings view surfaces them like every other limit.
    let media_limits = match &state.pool {
        Some(pool) => crate::product_images::effective_media_limits(pool).await,
        None => crate::product_images::MediaLimits {
            max_images_per_product: domain::MAX_IMAGES_PER_PRODUCT,
            store_quota_bytes: domain::MAX_STORE_MEDIA_BYTES,
        },
    };
    // Wave 54 extension: the delivery-time estimates live
    // in the delivery_estimates module (their resolution
    // home); the settings view surfaces them like every
    // other rule.
    let delivery_ranges =
        crate::delivery_estimates::effective_delivery_day_ranges(state.pool.as_ref()).await;
    AdminSettings {
        commission_bps: i64::from(effective_commission_bps(state).await),
        min_payout_poisha: effective_min_payout(state).await,
        advance_percent_bps: i64::from(effective_advance_bps(state).await),
        advance_threshold_poisha: effective_advance_threshold(state).await,
        kyc_payout_gate: effective_kyc_payout_gate(state).await,
        promotion_price_per_day_poisha: effective_promotion_price_per_day(state).await,
        subscription_price_pro_poisha: effective_subscription_price(state, PLAN_PRO).await,
        subscription_price_business_poisha: effective_subscription_price(state, PLAN_BUSINESS)
            .await,
        plan_free_product_cap: effective_plan_free_product_cap(state).await,
        media_max_images_per_product: media_limits.max_images_per_product,
        media_store_quota_mb: media_limits.store_quota_bytes / (1024 * 1024),
        delivery_days_inside_dhaka: delivery_ranges[0].1,
        delivery_days_dhaka_suburb: delivery_ranges[1].1,
        delivery_days_outside_dhaka: delivery_ranges[2].1,
        platform_name: PLATFORM_NAME,
    }
}

/// Store one setting row (insert or overwrite). Takes any
/// executor so [`update_settings`] can run every save of a
/// patch inside its one transaction.
async fn save_setting(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    key: &str,
    value: i64,
) -> Result<(), ApiError> {
    save_setting_text(executor, key, &value.to_string()).await
}

/// Store one setting row as raw text (insert or overwrite) —
/// the shared writer behind [`save_setting`] and the flag
/// settings like the KYC payout gate.
async fn save_setting_text(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    key: &str,
    value: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO platform_settings (key, value) VALUES ($1, $2) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(key)
    .bind(value)
    .execute(executor)
    .await
    .map_err(|e| ApiError::internal("save platform setting", e))?;
    Ok(())
}

/// Apply a settings patch: each provided field is validated by
/// the same rules the readers enforce (commission via the domain
/// validator, minimum payout via the range check) and upserted;
/// absent fields keep their current effective value. Returns the
/// new effective settings. Wave 28: all saves of one patch run
/// in ONE transaction together with the audit row
/// (`settings.update`, whose detail names the changed keys —
/// never their values); a patch that changes nothing writes no
/// audit row.
pub async fn update_settings(
    state: &AppState,
    admin_id: Uuid,
    input: UpdateSettingsInput,
) -> Result<AdminSettings, ApiError> {
    let pool = state.require_pool()?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin settings update", e))?;
    let mut changed_keys: Vec<&str> = Vec::new();
    if let Some(bps) = input.commission_bps {
        if !validate_commission_bps(bps) {
            return Err(ApiError::bad_request(
                "commission_bps must be between 0 and 3000",
            ));
        }
        save_setting(&mut *tx, COMMISSION_BPS_KEY, bps).await?;
        changed_keys.push(COMMISSION_BPS_KEY);
    }
    if let Some(min_payout) = input.min_payout_poisha {
        if !valid_min_payout_setting(min_payout) {
            return Err(ApiError::bad_request(
                "min_payout_poisha must be between 0 and 10000000",
            ));
        }
        save_setting(&mut *tx, MIN_PAYOUT_KEY, min_payout).await?;
        changed_keys.push(MIN_PAYOUT_KEY);
    }
    if let Some(advance_bps) = input.advance_percent_bps {
        if !validate_advance_bps(advance_bps) {
            return Err(ApiError::bad_request(
                "advance_percent_bps must be between 0 and 10000",
            ));
        }
        save_setting(&mut *tx, ADVANCE_PERCENT_BPS_KEY, advance_bps).await?;
        changed_keys.push(ADVANCE_PERCENT_BPS_KEY);
    }
    if let Some(advance_threshold) = input.advance_threshold_poisha {
        if !validate_advance_threshold(advance_threshold) {
            return Err(ApiError::bad_request(
                "advance_threshold_poisha must be 0 or more",
            ));
        }
        save_setting(&mut *tx, ADVANCE_THRESHOLD_KEY, advance_threshold).await?;
        changed_keys.push(ADVANCE_THRESHOLD_KEY);
    }
    if let Some(kyc_gate) = input.kyc_payout_gate {
        save_setting_text(
            &mut *tx,
            KYC_PAYOUT_GATE_KEY,
            if kyc_gate { "on" } else { "off" },
        )
        .await?;
        changed_keys.push(KYC_PAYOUT_GATE_KEY);
    }
    if let Some(promotion_price) = input.promotion_price_per_day_poisha {
        if !validate_promotion_price_per_day(promotion_price) {
            return Err(ApiError::bad_request(
                "promotion_price_per_day_poisha must be between 1 and 10000000",
            ));
        }
        save_setting(&mut *tx, PROMOTION_PRICE_PER_DAY_KEY, promotion_price).await?;
        changed_keys.push(PROMOTION_PRICE_PER_DAY_KEY);
    }
    if let Some(pro_price) = input.subscription_price_pro_poisha {
        if !validate_subscription_price(pro_price) {
            return Err(ApiError::bad_request(
                "subscription_price_pro_poisha must be between 1 and 10000000",
            ));
        }
        save_setting(&mut *tx, SUBSCRIPTION_PRICE_PRO_KEY, pro_price).await?;
        changed_keys.push(SUBSCRIPTION_PRICE_PRO_KEY);
    }
    if let Some(business_price) = input.subscription_price_business_poisha {
        if !validate_subscription_price(business_price) {
            return Err(ApiError::bad_request(
                "subscription_price_business_poisha must be between 1 and 10000000",
            ));
        }
        save_setting(&mut *tx, SUBSCRIPTION_PRICE_BUSINESS_KEY, business_price).await?;
        changed_keys.push(SUBSCRIPTION_PRICE_BUSINESS_KEY);
    }
    if let Some(max_images) = input.media_max_images_per_product {
        if !domain::validate_media_max_images(max_images) {
            return Err(ApiError::bad_request(
                "media_max_images_per_product must be between 1 and 50",
            ));
        }
        save_setting(
            &mut *tx,
            crate::product_images::MEDIA_MAX_IMAGES_SETTING_KEY,
            max_images,
        )
        .await?;
        changed_keys.push(crate::product_images::MEDIA_MAX_IMAGES_SETTING_KEY);
    }
    if let Some(quota_mb) = input.media_store_quota_mb {
        if !domain::validate_media_store_quota_mb(quota_mb) {
            return Err(ApiError::bad_request(
                "media_store_quota_mb must be between 10 and 102400",
            ));
        }
        save_setting(
            &mut *tx,
            crate::product_images::MEDIA_STORE_QUOTA_MB_SETTING_KEY,
            quota_mb,
        )
        .await?;
        changed_keys.push(crate::product_images::MEDIA_STORE_QUOTA_MB_SETTING_KEY);
    }
    if let Some(free_cap) = input.plan_free_product_cap {
        if !validate_plan_free_product_cap(free_cap) {
            return Err(ApiError::bad_request(
                "plan_free_product_cap must be between 0 and 1000000",
            ));
        }
        save_setting(&mut *tx, PLAN_FREE_PRODUCT_CAP_KEY, free_cap).await?;
        changed_keys.push(PLAN_FREE_PRODUCT_CAP_KEY);
    }
    // Wave 54: the three delivery-time estimate ranges.
    // Each is validated by the domain rule and stored in
    // its `"<min>-<max>"` text form under its zone key —
    // the estimate endpoint resolves them back through
    // the same domain parser.
    for (range, key) in [
        (
            input.delivery_days_inside_dhaka,
            domain::DELIVERY_DAYS_INSIDE_DHAKA_KEY,
        ),
        (
            input.delivery_days_dhaka_suburb,
            domain::DELIVERY_DAYS_DHAKA_SUBURB_KEY,
        ),
        (
            input.delivery_days_outside_dhaka,
            domain::DELIVERY_DAYS_OUTSIDE_DHAKA_KEY,
        ),
    ] {
        if let Some((min_days, max_days)) = range {
            let (min_days, max_days) =
                domain::validate_delivery_day_range(min_days, max_days)
                    .map_err(ApiError::bad_request)?;
            save_setting_text(
                &mut *tx,
                key,
                &domain::format_delivery_day_range(min_days, max_days),
            )
            .await?;
            changed_keys.push(key);
        }
    }
    if !changed_keys.is_empty() {
        record_admin_action(
            &mut *tx,
            admin_id,
            "settings.update",
            "platform_settings",
            None,
            Some(serde_json::json!({ "changed_keys": changed_keys })),
        )
        .await?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit settings update", e))?;
    Ok(get_settings(state).await)
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AdminStoresQuery {
    pub q: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct AdminOrdersQuery {
    pub status: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct AdminPayoutsQuery {
    pub status: Option<String>,
    /// Search: store name / owner phone / account number
    /// substring, or an idempotency key (exact or prefix).
    pub q: Option<String>,
    /// newest | oldest | amount_desc | amount_asc.
    pub sort: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct AdminUsersQuery {
    pub q: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct SetStoreStatusInput {
    pub status: String,
}

#[derive(Deserialize)]
pub struct RejectPayoutInput {
    /// The rejection reason. REQUIRED by the transition
    /// (a missing body, a missing field, or a blank reason
    /// are all the same 400) — the field stays `Option` here
    /// so an absent body reaches the service's validation
    /// instead of dying in the extractor.
    pub note: Option<String>,
}

#[derive(Deserialize)]
pub struct MarkPayoutPaidInput {
    /// The payment reference proving the send (bKash/Nagad
    /// transaction id or bank reference). REQUIRED by the
    /// transition; `Option` here for the same reason as the
    /// reject note.
    pub payment_reference: Option<String>,
}

#[derive(Deserialize)]
pub struct SetUserAdminInput {
    pub is_admin: bool,
}

#[derive(Deserialize)]
pub struct UpdateSettingsInput {
    pub commission_bps: Option<i64>,
    pub min_payout_poisha: Option<i64>,
    /// Wave 17: COD advance rate override (basis points).
    pub advance_percent_bps: Option<i64>,
    /// Wave 17: COD advance threshold override (poisha).
    pub advance_threshold_poisha: Option<i64>,
    /// Wave 21: KYC payout gate — true stores `on`, false
    /// stores `off` (see [`effective_kyc_payout_gate`]).
    pub kyc_payout_gate: Option<bool>,
    /// Wave 26: per-day sponsored-promotion price override
    /// (poisha), validated by the domain rule.
    pub promotion_price_per_day_poisha: Option<i64>,
    /// Wave 36: pro plan price override (poisha per 30-day
    /// period), validated by the domain rule.
    pub subscription_price_pro_poisha: Option<i64>,
    /// Wave 36: business plan price override (poisha per
    /// 30-day period), validated by the domain rule.
    pub subscription_price_business_poisha: Option<i64>,
    /// Wave 36: free plan product cap override, validated by
    /// the domain rule.
    pub plan_free_product_cap: Option<i64>,
    /// Wave 51 extension: per-product image cap override,
    /// validated by the domain rule (1..=50).
    pub media_max_images_per_product: Option<i64>,
    /// Wave 51 extension: per-store media quota override
    /// in megabytes, validated by the domain rule.
    pub media_store_quota_mb: Option<i64>,
    /// Wave 54: Inside Dhaka delivery-time estimate
    /// override, [earliest, latest] days, validated by
    /// `domain::validate_delivery_day_range` and stored in
    /// the `"<min>-<max>"` text form.
    pub delivery_days_inside_dhaka: Option<(i64, i64)>,
    /// Wave 54: Dhaka suburb delivery-time estimate
    /// override, [earliest, latest] days.
    pub delivery_days_dhaka_suburb: Option<(i64, i64)>,
    /// Wave 54: outside-Dhaka delivery-time estimate
    /// override, [earliest, latest] days.
    pub delivery_days_outside_dhaka: Option<(i64, i64)>,
}

// ---------------------------------------------------------------------------
// Handlers (thin: guard -> call one service function -> respond)
// ---------------------------------------------------------------------------

pub async fn overview_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<AdminOverview>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(get_overview(pool).await?))
}

pub async fn list_stores_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminStoresQuery>,
) -> Result<Json<AdminStoreList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_stores(pool, query.q.as_deref(), query.page, query.limit).await?,
    ))
}

pub async fn set_store_status_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<SetStoreStatusInput>,
) -> Result<Json<Store>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        set_store_status(pool, admin.id, id, &input.status).await?,
    ))
}

pub async fn list_orders_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminOrdersQuery>,
) -> Result<Json<OrderList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_orders(pool, query.status.as_deref(), query.page, query.limit).await?,
    ))
}

pub async fn list_payouts_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminPayoutsQuery>,
) -> Result<Json<AdminPayoutList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_payouts(
            pool,
            query.status.as_deref(),
            query.q.as_deref(),
            query.sort.as_deref(),
            query.page,
            query.limit,
        )
        .await?,
    ))
}

pub async fn get_payout_detail_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminPayoutDetail>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(get_payout_detail(pool, id).await?))
}

pub async fn approve_payout_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Payout>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    Ok(Json(
        transition_payout(&state, admin.id, id, "approved", None, None).await?,
    ))
}

pub async fn reject_payout_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<RejectPayoutInput>>,
) -> Result<Json<Payout>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let note = body.and_then(|Json(input)| input.note);
    Ok(Json(
        transition_payout(&state, admin.id, id, "rejected", note.as_deref(), None).await?,
    ))
}

pub async fn mark_payout_paid_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<MarkPayoutPaidInput>>,
) -> Result<Json<Payout>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let reference = body.and_then(|Json(input)| input.payment_reference);
    Ok(Json(
        transition_payout(&state, admin.id, id, "paid", None, reference.as_deref()).await?,
    ))
}

pub async fn list_users_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminUsersQuery>,
) -> Result<Json<AdminUserList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_users(pool, query.q.as_deref(), query.page, query.limit).await?,
    ))
}

pub async fn set_user_admin_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<SetUserAdminInput>,
) -> Result<Json<User>, ApiError> {
    let caller = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        set_user_admin(pool, &caller, id, input.is_admin).await?,
    ))
}

pub async fn get_settings_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<AdminSettings>, ApiError> {
    require_admin(&state, &user).await?;
    Ok(Json(get_settings(&state).await))
}

pub async fn update_settings_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<UpdateSettingsInput>,
) -> Result<Json<AdminSettings>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    Ok(Json(update_settings(&state, admin.id, input).await?))
}
