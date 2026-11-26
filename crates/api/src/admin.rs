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
    can_transition_payout, parse_setting_i64, validate_advance_bps, validate_advance_threshold,
    validate_commission_bps, validate_plan_free_product_cap, validate_promotion_price_per_day,
    validate_subscription_price, ADVANCE_PERCENT_BPS_KEY, ADVANCE_THRESHOLD_KEY, COMMISSION_BPS_KEY,
    KYC_PAYOUT_GATE_KEY, MIN_PAYOUT_KEY, MIN_PAYOUT_POISHA, PLAN_BUSINESS, PLAN_FREE_PRODUCT_CAP_KEY,
    PLAN_PRO, PROMOTION_PRICE_PER_DAY_KEY, SUBSCRIPTION_PRICE_BUSINESS_KEY,
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
    account_number, status, note, created_at, processed_at";

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

/// The platform at a glance. Money definitions (README, Wave 10):
/// GMV and commission sum over sub-orders that were not cancelled
/// (a cancelled sub-order never moved money); everything is a
/// bigint aggregate, cast where Postgres would return numeric.
#[derive(Debug, Serialize)]
pub struct AdminOverview {
    pub users: i64,
    pub stores: i64,
    pub products: i64,
    pub orders: i64,
    pub gmv_goods_poisha: i64,
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
    gmv_goods_poisha: i64,
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
           (SELECT COALESCE(SUM(goods_subtotal_poisha), 0)::bigint \
              FROM sub_orders WHERE status <> 'cancelled') AS gmv_goods_poisha, \
           (SELECT COALESCE(SUM(commission_poisha), 0)::bigint \
              FROM sub_orders WHERE status <> 'cancelled') AS commission_earned_poisha, \
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
        gmv_goods_poisha: row.gmv_goods_poisha,
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
         theme_key, brand_color, logo_url, banner_url, announcement, verified_tier, created_at",
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

/// A payout as the admin sees it: the seller's payout view plus
/// which store it belongs to and the owner's phone.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminPayout {
    pub id: Uuid,
    pub store_id: Uuid,
    pub store_name: String,
    pub owner_phone: String,
    pub amount_poisha: i64,
    pub method: String,
    pub account_name: String,
    pub account_number: String,
    pub status: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
}

/// A page of admin payouts plus the total count.
#[derive(Debug, Serialize)]
pub struct AdminPayoutList {
    pub items: Vec<AdminPayout>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// The payout processing queue, newest first, optionally filtered
/// by lifecycle status.
pub async fn list_payouts(
    pool: &PgPool,
    status: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminPayoutList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let status = status.map(str::trim).filter(|s| !s.is_empty());

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM payouts WHERE ($1::text IS NULL OR status = $1)",
    )
    .bind(status)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin payouts", e))?;

    let items: Vec<AdminPayout> = sqlx::query_as(
        "SELECT p.id, p.store_id, s.name AS store_name, u.phone AS owner_phone, \
         p.amount_poisha, p.method, p.account_name, p.account_number, \
         p.status, p.note, p.created_at, p.processed_at \
         FROM payouts p \
         JOIN stores s ON s.id = p.store_id \
         JOIN users u ON u.id = s.owner_user_id \
         WHERE ($1::text IS NULL OR p.status = $1) \
         ORDER BY p.created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(status)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin payouts", e))?;

    Ok(AdminPayoutList {
        items,
        total,
        page,
        limit,
    })
}

/// THE payout processing function: approve, reject and mark-paid
/// are three thin handlers over this one service. The domain
/// transition table decides legality (an illegal move is a 409);
/// a legal move stamps `processed_at`, and a supplied note (the
/// rejection reason) replaces the stored note. Wave 28: the
/// whole move — status read, update, and the audit row
/// (`payout.approve` / `payout.reject` / `payout.mark_paid`) —
/// runs in ONE transaction, so the audit log and the payout
/// can never disagree.
pub async fn transition_payout(
    pool: &PgPool,
    admin_id: Uuid,
    payout_id: Uuid,
    target_status: &str,
    note: Option<&str>,
) -> Result<Payout, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin payout transition", e))?;
    let current: Option<String> =
        sqlx::query_scalar("SELECT status FROM payouts WHERE id = $1")
            .bind(payout_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("load payout for transition", e))?;
    let current = current.ok_or_else(|| ApiError::not_found("payout not found"))?;
    if !can_transition_payout(&current, target_status) {
        return Err(ApiError::conflict(format!(
            "cannot move a payout from '{current}' to '{target_status}'"
        )));
    }

    let payout: Payout = sqlx::query_as(&format!(
        "UPDATE payouts SET status = $2, processed_at = now(), \
         note = COALESCE($3, note) WHERE id = $1 RETURNING {PAYOUT_COLUMNS}"
    ))
    .bind(payout_id)
    .bind(target_status)
    .bind(note)
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
    if let Some(note) = note {
        detail["note"] = serde_json::Value::String(note.to_string());
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
    // Wave 29: a payout that reaches `paid` also queues the
    // seller's payout_paid SMS in the same transaction (the
    // outbox rule — see the `notices` module).
    if target_status == "paid" {
        crate::notices::enqueue_payout_paid_notice(&mut tx, payout.store_id, payout.amount_poisha)
            .await?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit payout transition", e))?;
    Ok(payout)
}

// ---------------------------------------------------------------------------
// Users: list + admin flag
// ---------------------------------------------------------------------------

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
    pub platform_name: &'static str,
}

/// The current effective settings.
pub async fn get_settings(state: &AppState) -> AdminSettings {
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
    if let Some(free_cap) = input.plan_free_product_cap {
        if !validate_plan_free_product_cap(free_cap) {
            return Err(ApiError::bad_request(
                "plan_free_product_cap must be between 0 and 1000000",
            ));
        }
        save_setting(&mut *tx, PLAN_FREE_PRODUCT_CAP_KEY, free_cap).await?;
        changed_keys.push(PLAN_FREE_PRODUCT_CAP_KEY);
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
    pub note: Option<String>,
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
        list_payouts(pool, query.status.as_deref(), query.page, query.limit).await?,
    ))
}

pub async fn approve_payout_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Payout>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        transition_payout(pool, admin.id, id, "approved", None).await?,
    ))
}

pub async fn reject_payout_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<RejectPayoutInput>>,
) -> Result<Json<Payout>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let note = body.and_then(|Json(input)| input.note);
    Ok(Json(
        transition_payout(pool, admin.id, id, "rejected", note.as_deref()).await?,
    ))
}

pub async fn mark_payout_paid_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Payout>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        transition_payout(pool, admin.id, id, "paid", None).await?,
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
