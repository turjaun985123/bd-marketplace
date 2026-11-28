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
//! Admin stores remake: the list carries decision context
//! (verification tier, effective subscription plan, delivered
//! GMV, pending-payout exposure, open returns) with the owner's
//! phone MASKED — the pre-remake list leaked the full phone,
//! against the Wave 28 list canon. Suspension now REQUIRES a
//! note (`domain::validate_suspension_note`): it is stored on
//! the store row (migration 0075), written into the audit
//! detail, shown to the seller on their dashboard, and quoted
//! in the `store_suspended` notice the transition enqueues in
//! its own transaction. A transition to the status the store
//! already holds is a 409, never a silent no-op with a phantom
//! audit row (security audit LO-93). Suspension does NOT pause
//! subscription billing and does NOT cancel payouts — payout
//! decisions on a suspended store are frozen (see
//! `transition_payout`), and that is all.
//!
//! Admin orders remake: the orders surface is READ-ONLY
//! oversight — no admin power over an order's lifecycle
//! exists or is added here (transitions belong to the
//! buyer/seller flows; refunds to the returns surface).
//! The queue list is a purpose-built row (masked delivery
//! phone, sub-order chips, snapshot money) assembled
//! set-wise; the pre-remake list returned the buyer's full
//! order view per row through the shared loader, leaking
//! full delivery PII into a list and costing a query per
//! order part. The detail returns the buyer's own view
//! (full delivery block — the payouts detail precedent)
//! plus the masked account phone and the receipt's COD
//! due. The list summary restates the overview metrics'
//! advance populations and today figures so the two
//! surfaces agree.
//!
//! Admin products remake: the products surface is new —
//! before it, an admin could not see the catalogue
//! cross-store at all, and the only lever on a bad
//! listing was suspending the seller's whole store. The
//! queue is the shared product view (products are public
//! data — nothing here needs masking) plus store context;
//! the detail adds the gallery, ALL variants, a sales
//! block and the product's audit history (real here,
//! unlike orders: takedown actions DO target products).
//! The one admin power is takedown/restore: a platform
//! FLAG on the product row (migration 0078), orthogonal
//! to the seller's own status, so restore returns the
//! product to exactly its seller-chosen state. Takedown
//! REQUIRES a reason and restore REQUIRES a note (the
//! restore-note requirement consciously diverges from
//! store reactivation: a takedown is a content
//! determination, and the note is the only record of why
//! another admin reversed it). Every public read path
//! filters the flag; checkout refuses a taken-down line
//! in its locked validation loop; the full blast-radius
//! list is in the README's remake note.
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
    bn_to_en_digits, can_transition_payout, default_shipment_aged_hours,
    is_valid_payout_status, mask_phone, normalize_bd_phone, parse_setting_i64,
    search_like_pattern, shipment_aged_setting_key, to_local_phone,
    validate_admin_product_list_sort, validate_advance_bps, validate_advance_threshold,
    validate_commission_bps, validate_fulfillment_sla_hours, validate_order_list_sort,
    validate_payout_list_sort, validate_payout_rejection_reason, validate_payment_reference,
    validate_plan_free_product_cap, validate_product_restore_note,
    validate_product_takedown_reason, validate_promotion_price_per_day,
    validate_shipment_list_sort, validate_store_list_sort, validate_subscription_price,
    validate_suspension_note, OrderSort, PayoutSort, ProductSort, ShipmentSort, StoreSort,
    ADVANCE_PERCENT_BPS_KEY, ADVANCE_THRESHOLD_KEY, COMMISSION_BPS_KEY, KYC_PAYOUT_GATE_KEY,
    MIN_PAYOUT_KEY, MIN_PAYOUT_POISHA, NOTIFICATION_PRODUCT_RESTORED,
    NOTIFICATION_PRODUCT_TAKEN_DOWN, NOTIFICATION_STORE_REACTIVATED, NOTIFICATION_STORE_SUSPENDED,
    PLAN_BUSINESS, PLAN_FREE_PRODUCT_CAP_KEY, PLAN_PRO, PROMOTION_PRICE_PER_DAY_KEY,
    SUBSCRIPTION_PRICE_BUSINESS_KEY, SUBSCRIPTION_PRICE_PRO_KEY,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::audit::{list_audit_for_target, record_admin_action, AdminAuditEntry};
use crate::auth::{load_user, AuthUser, User};
use crate::metrics::STALE_SHIPMENT_MINUTES;
use crate::orders::{load_order, Order};
use crate::payouts::{Earnings, Payout};
use crate::shipments::{
    courier_availability, manual_courier_availability, refresh_shipment_by_id_audited,
    CourierAvailability, RefreshOutcome, Shipment, POLL_TERMINAL_STATUSES,
};
use crate::timeline::{sub_order_steps, sub_order_timeline, TimelineEvent, TimelineStep};
use courier::CouriersConfig;
use crate::products::{
    clamp_pagination, get_product_by_id, with_stock_status, Product, EFFECTIVE_STOCK_SQL,
    PRODUCT_COLUMNS, PRODUCT_RATING_JOIN,
};
use crate::stores::{get_store_by_id, Store};
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

/// The raw admin store row, before the owner's phone is
/// masked. Shared by the queue list and the single-store
/// detail — one projection, so the two can never disagree
/// about what a store row carries. The full phone exists
/// ONLY inside this struct; every serialized view is built
/// from it through [`AdminStore::from_row`], which masks.
#[derive(Debug, FromRow)]
struct AdminStoreRow {
    id: Uuid,
    owner_user_id: Uuid,
    owner_phone: String,
    owner_name: Option<String>,
    slug: String,
    name: String,
    description: Option<String>,
    status: String,
    verified_tier: String,
    /// The EFFECTIVE plan: the subscription's plan while a
    /// subscription is live (`active` and unexpired), else
    /// `free` — the same live-plan rule the subscription
    /// module applies, computed in SQL so a lapsed paid plan
    /// never presents as current.
    plan: String,
    product_count: i64,
    logo_url: Option<String>,
    follower_count: i64,
    order_count: i64,
    /// Delivered GMV in poisha: SUM(goods − discount +
    /// delivery) over DELIVERED sub-orders — the overview
    /// remake's canonical GMV definition.
    delivered_gmv_poisha: i64,
    /// Payouts in `requested` — the only status that is
    /// still the platform's decision (the payouts canon).
    pending_payout_count: i64,
    pending_payout_poisha: i64,
    /// Return requests still open (`requested`/`approved`)
    /// — the metrics canon.
    open_returns_count: i64,
    /// The admin's reason for the CURRENT suspension
    /// (migration 0075); `None` while the store is active.
    suspension_note: Option<String>,
    /// When the store's status last changed, either way.
    status_changed_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

/// The admin store projection shared by the list query and
/// the detail read: one SELECT text, so a column added for
/// one surface cannot be forgotten by the other.
const ADMIN_STORE_PROJECTION: &str = "SELECT s.id, s.owner_user_id, \
    u.phone AS owner_phone, u.name AS owner_name, s.slug, s.name, \
    s.description, s.status, s.verified_tier, \
    CASE WHEN ss.status = 'active' AND ss.expires_at > now() \
         THEN ss.plan ELSE 'free' END AS plan, \
    (SELECT COUNT(*) FROM products p WHERE p.store_id = s.id) AS product_count, \
    s.logo_url, \
    (SELECT COUNT(*) FROM store_follows f WHERE f.store_id = s.id) AS follower_count, \
    (SELECT COUNT(*) FROM sub_orders so WHERE so.store_id = s.id) AS order_count, \
    COALESCE((SELECT SUM(so.goods_subtotal_poisha - so.discount_poisha \
        + so.delivery_charge_poisha)::bigint FROM sub_orders so \
        WHERE so.store_id = s.id AND so.status = 'delivered'), 0) \
        AS delivered_gmv_poisha, \
    (SELECT COUNT(*) FROM payouts po \
        WHERE po.store_id = s.id AND po.status = 'requested') AS pending_payout_count, \
    COALESCE((SELECT SUM(po.amount_poisha)::bigint FROM payouts po \
        WHERE po.store_id = s.id AND po.status = 'requested'), 0) \
        AS pending_payout_poisha, \
    (SELECT COUNT(*) FROM return_requests rr \
        WHERE rr.store_id = s.id AND rr.status IN ('requested', 'approved')) \
        AS open_returns_count, \
    s.suspension_note, s.status_changed_at, s.created_at \
    FROM stores s \
    JOIN users u ON u.id = s.owner_user_id \
    LEFT JOIN store_subscriptions ss ON ss.store_id = s.id";

/// A store as the admin sees it: identity, tier + effective
/// plan, the decision aggregates (products, followers,
/// orders, delivered GMV, pending-payout exposure, open
/// returns), and the suspension metadata. The owner's phone
/// is MASKED (admin stores remake — the pre-remake list
/// carried the full phone, the leak the payouts remake
/// removed from its own list). No admin store response
/// carries the full phone: unlike a payout, no store
/// decision is executed BY phone, so no detail-level reveal
/// is justified here.
#[derive(Debug, Serialize)]
pub struct AdminStore {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub owner_name: Option<String>,
    /// The store owner's phone, masked via
    /// `domain::mask_phone`.
    pub owner_phone_masked: String,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub verified_tier: String,
    pub plan: String,
    pub product_count: i64,
    /// The store's logo URL (Wave 51 extension) — the
    /// admin list shows the mark it moderates.
    pub logo_url: Option<String>,
    /// The store's follower count (Wave 54) — the admin
    /// list shows the audience a moderation decision
    /// affects.
    pub follower_count: i64,
    pub order_count: i64,
    pub delivered_gmv_poisha: i64,
    pub pending_payout_count: i64,
    pub pending_payout_poisha: i64,
    pub open_returns_count: i64,
    pub suspension_note: Option<String>,
    pub status_changed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl AdminStore {
    fn from_row(row: AdminStoreRow) -> Self {
        AdminStore {
            id: row.id,
            owner_user_id: row.owner_user_id,
            owner_name: row.owner_name,
            owner_phone_masked: mask_phone(&row.owner_phone),
            slug: row.slug,
            name: row.name,
            description: row.description,
            status: row.status,
            verified_tier: row.verified_tier,
            plan: row.plan,
            product_count: row.product_count,
            logo_url: row.logo_url,
            follower_count: row.follower_count,
            order_count: row.order_count,
            delivered_gmv_poisha: row.delivered_gmv_poisha,
            pending_payout_count: row.pending_payout_count,
            pending_payout_poisha: row.pending_payout_poisha,
            open_returns_count: row.open_returns_count,
            suspension_note: row.suspension_note,
            status_changed_at: row.status_changed_at,
            created_at: row.created_at,
        }
    }
}

/// Whole-queue store counts for the admin list header
/// (admin stores remake): counts by status and by
/// verification tier over ALL stores, deliberately
/// UNFILTERED — the summary describes the platform, not
/// the page (the payout summary's rule).
#[derive(Debug, Serialize)]
pub struct AdminStoreSummary {
    pub total: i64,
    pub active: i64,
    pub suspended: i64,
    pub unverified: i64,
    pub basic: i64,
    pub verified: i64,
}

/// A page of admin stores plus the total count and the
/// whole-queue summary.
#[derive(Debug, Serialize)]
pub struct AdminStoreList {
    pub items: Vec<AdminStore>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
    pub summary: AdminStoreSummary,
}

/// The whole-queue summary query (admin stores remake), in
/// the payout summary's FILTER-count style.
async fn store_summary(pool: &PgPool) -> Result<AdminStoreSummary, ApiError> {
    #[derive(FromRow)]
    struct StoreSummaryRow {
        total: i64,
        active: i64,
        suspended: i64,
        unverified: i64,
        basic: i64,
        verified: i64,
    }
    let row: StoreSummaryRow = sqlx::query_as(
        "SELECT COUNT(*)::bigint AS total, \
           (COUNT(*) FILTER (WHERE status = 'active'))::bigint AS active, \
           (COUNT(*) FILTER (WHERE status = 'suspended'))::bigint AS suspended, \
           (COUNT(*) FILTER (WHERE verified_tier = 'unverified'))::bigint AS unverified, \
           (COUNT(*) FILTER (WHERE verified_tier = 'basic'))::bigint AS basic, \
           (COUNT(*) FILTER (WHERE verified_tier = 'verified'))::bigint AS verified \
         FROM stores",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("admin store summary", e))?;
    Ok(AdminStoreSummary {
        total: row.total,
        active: row.active,
        suspended: row.suspended,
        unverified: row.unverified,
        basic: row.basic,
        verified: row.verified,
    })
}

/// The ORDER BY fragment for a store-list sort — a fixed
/// string per variant (user input never reaches the SQL
/// text), every arm ending in the `id` tiebreaker the
/// deterministic-order rule requires.
fn store_order_by(sort: StoreSort) -> &'static str {
    match sort {
        StoreSort::Newest => "ORDER BY s.created_at DESC, s.id DESC",
        StoreSort::Oldest => "ORDER BY s.created_at ASC, s.id ASC",
        StoreSort::NameAsc => "ORDER BY s.name ASC, s.id DESC",
        StoreSort::GmvDesc => "ORDER BY delivered_gmv_poisha DESC, s.id DESC",
        StoreSort::FollowersDesc => "ORDER BY follower_count DESC, s.id DESC",
    }
}

/// List stores for the admin queue (admin stores remake):
/// `q` matches name or slug (LIKE-escaped via
/// `domain::search_like_pattern` — the pre-remake query
/// interpolated the raw text, so `%` and `_` acted as
/// wildcards); `status` and `tier` filter exactly and are
/// validated fail-closed (an unknown value is a 400, never
/// a silently empty page); `sort` is validated by the
/// domain. The total and the items run the SAME joins and
/// filters (the pre-remake count query lacked the users
/// join, so a store whose owner row was missing counted
/// toward the total but never appeared).
#[allow(clippy::too_many_arguments)]
pub async fn list_stores(
    pool: &PgPool,
    q: Option<&str>,
    status: Option<&str>,
    tier: Option<&str>,
    sort: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminStoreList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let pattern: Option<String> = q
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(search_like_pattern);
    let status: Option<&str> = match status.map(str::trim) {
        None | Some("") => None,
        Some("active") => Some("active"),
        Some("suspended") => Some("suspended"),
        Some(_) => {
            return Err(ApiError::bad_request(
                "status must be one of: active, suspended",
            ))
        }
    };
    let tier: Option<&str> = match tier.map(str::trim) {
        None | Some("") => None,
        Some("unverified") => Some("unverified"),
        Some("basic") => Some("basic"),
        Some("verified") => Some("verified"),
        Some(_) => {
            return Err(ApiError::bad_request(
                "tier must be one of: unverified, basic, verified",
            ))
        }
    };
    let sort = validate_store_list_sort(sort).map_err(ApiError::bad_request)?;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM stores s \
         JOIN users u ON u.id = s.owner_user_id \
         WHERE ($1::text IS NULL OR s.name ILIKE $1 OR s.slug ILIKE $1) \
           AND ($2::text IS NULL OR s.status = $2) \
           AND ($3::text IS NULL OR s.verified_tier = $3)",
    )
    .bind(&pattern)
    .bind(status)
    .bind(tier)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin stores", e))?;

    let rows: Vec<AdminStoreRow> = sqlx::query_as(&format!(
        "{ADMIN_STORE_PROJECTION} \
         WHERE ($1::text IS NULL OR s.name ILIKE $1 OR s.slug ILIKE $1) \
           AND ($2::text IS NULL OR s.status = $2) \
           AND ($3::text IS NULL OR s.verified_tier = $3) \
         {} LIMIT $4 OFFSET $5",
        store_order_by(sort)
    ))
    .bind(&pattern)
    .bind(status)
    .bind(tier)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin stores", e))?;

    let summary = store_summary(pool).await?;

    Ok(AdminStoreList {
        items: rows.into_iter().map(AdminStore::from_row).collect(),
        total,
        page,
        limit,
        summary,
    })
}

/// One of a store owner's stores, on the admin store
/// detail: the verification detail's rollup shape PLUS the
/// store's own status — a suspended sibling is exactly
/// what a reviewer of THIS store needs to see.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminOwnerStoreSummary {
    pub store_id: Uuid,
    pub store_slug: String,
    pub store_name: String,
    pub status: String,
    pub tier: String,
    /// The store's verification application status, or
    /// `none` when it never applied.
    pub verification_status: String,
}

/// One store in full, for the admin who must decide on it
/// (admin stores remake): the queue row (flattened) plus
/// the subscription view, the canonical earnings, the
/// returns exposure (the payout detail's own block), the
/// verification summary, the owner's other stores, and the
/// store's audit history. Composed from the canonical
/// readers — no aggregate is re-derived here. The owner
/// phone stays MASKED even here (see [`AdminStore`]).
#[derive(Debug, Serialize)]
pub struct AdminStoreDetail {
    #[serde(flatten)]
    pub store: AdminStore,
    pub subscription: crate::subscriptions::SubscriptionView,
    pub earnings: Earnings,
    pub returns_exposure: PayoutReturnsExposure,
    pub verification: crate::verification::SellerVerificationView,
    pub owner_stores: Vec<AdminOwnerStoreSummary>,
    pub audit_history: Vec<crate::audit::AdminAuditEntry>,
}

/// Read one store's admin detail. The store row is read
/// first and 404s before any sub-read runs (the payout
/// detail's order).
pub async fn get_admin_store_detail(
    state: &AppState,
    store_id: Uuid,
) -> Result<AdminStoreDetail, ApiError> {
    let pool = state.require_pool()?;
    let row: AdminStoreRow = sqlx::query_as(&format!(
        "{ADMIN_STORE_PROJECTION} WHERE s.id = $1"
    ))
    .bind(store_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load admin store detail", e))?
    .ok_or_else(|| ApiError::not_found("store not found"))?;
    let owner_user_id = row.owner_user_id;
    let verified_tier = row.verified_tier.clone();
    let store = AdminStore::from_row(row);

    let subscription = crate::subscriptions::get_subscription_view(state, store_id).await?;
    let earnings = crate::payouts::seller_earnings_on(pool, store_id).await?;

    #[derive(FromRow)]
    struct StoreReturnsExposureRow {
        in_flight_count: i64,
        at_risk_payout_poisha: i64,
        received_refund_pending_count: i64,
        received_refund_pending_poisha: i64,
    }
    let exposure: StoreReturnsExposureRow = sqlx::query_as(
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
    .map_err(|e| ApiError::internal("load store returns exposure", e))?;
    let returns_exposure = PayoutReturnsExposure {
        in_flight_count: exposure.in_flight_count,
        at_risk_payout_poisha: exposure.at_risk_payout_poisha,
        received_refund_pending_count: exposure.received_refund_pending_count,
        received_refund_pending_poisha: exposure.received_refund_pending_poisha,
    };

    let verification =
        crate::verification::admin_verification_summary(pool, store_id, &verified_tier).await?;

    let owner_stores: Vec<AdminOwnerStoreSummary> = sqlx::query_as(
        "SELECT s.id AS store_id, s.slug AS store_slug, s.name AS store_name, \
         s.status, s.verified_tier AS tier, \
         COALESCE(v.status, 'none') AS verification_status \
         FROM stores s LEFT JOIN store_verifications v ON v.store_id = s.id \
         WHERE s.owner_user_id = $1 \
         ORDER BY s.created_at ASC",
    )
    .bind(owner_user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load admin owner stores", e))?;

    let audit_history =
        crate::audit::list_audit_for_target(pool, "store", &store_id.to_string(), 20).await?;

    Ok(AdminStoreDetail {
        store,
        subscription,
        earnings,
        returns_exposure,
        verification,
        owner_stores,
        audit_history,
    })
}

/// The Bangla label of a store status, for error prose —
/// the same words the web's `storeStatusLabel` shows.
fn store_status_label_bn(status: &str) -> &str {
    match status {
        "active" => "চালু",
        "suspended" => "স্থগিত",
        _ => status,
    }
}

/// Set a store's status (`active` or `suspended`; anything
/// else is a 400). Returns the shared store view.
/// Suspending is reversible and deletes nothing — see the
/// module docs for what it blocks.
///
/// Admin stores remake — the gates, in the order they fire
/// (the payouts/verifications precedence):
///
/// 1. INPUT (400): the status value, then — for a suspend —
///    the REQUIRED note
///    ([`validate_suspension_note`]). Reactivation takes
///    no note; one sent anyway is ignored, never stored.
/// 2. LOCK: the store row is read `FOR UPDATE` inside the
///    transition transaction (404 when absent) — the
///    store-first lock order every money path here uses.
/// 3. NO-OP (409): a transition to the status the store
///    already holds is refused, naming the current status.
///    The pre-remake code updated unconditionally, so a
///    repeated suspend wrote a phantom `store.suspend`
///    audit row every time and told the admin it had acted
///    (security audit LO-93 prescribes exactly this 409).
///    Nothing is written: no stamp bump, no audit row, no
///    notice, and the stored note is untouched.
/// 4. WRITE + AUDIT + NOTICE, one transaction: the status,
///    the note (set on suspend, cleared on reactivate),
///    the `status_changed_at` stamp and the acting admin
///    (migration 0075); ONE audit row whose detail carries
///    the note on suspend; and the `store_suspended` /
///    `store_reactivated` notice to the owner, enqueued
///    through the one funnel in `notices`.
pub async fn set_store_status(
    pool: &PgPool,
    admin_id: Uuid,
    store_id: Uuid,
    status: &str,
    note: Option<&str>,
) -> Result<Store, ApiError> {
    if status != "active" && status != "suspended" {
        return Err(ApiError::bad_request(
            "store status must be 'active' or 'suspended'",
        ));
    }
    let note: Option<String> = if status == "suspended" {
        Some(
            validate_suspension_note(note.unwrap_or(""))
                .map_err(ApiError::bad_request)?,
        )
    } else {
        None
    };
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin set store status", e))?;
    let current: Option<(String, String)> = sqlx::query_as(
        "SELECT status, name FROM stores WHERE id = $1 FOR UPDATE",
    )
    .bind(store_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock store for status change", e))?;
    let (current_status, store_name) =
        current.ok_or_else(|| ApiError::not_found("store not found"))?;
    if current_status == status {
        return Err(ApiError::conflict(format!(
            "স্টোরটি ইতিমধ্যে {} আছে — the store is already {}",
            store_status_label_bn(&current_status),
            current_status
        )));
    }
    let store: Store = sqlx::query_as(
        "UPDATE stores SET status = $2, suspension_note = $3, \
         status_changed_at = now(), status_changed_by_user_id = $4 \
         WHERE id = $1 \
         RETURNING id, owner_user_id, slug, name, description, status, \
         facebook_page_id, facebook_page_name, sidra_auto_reply, \
         theme_key, brand_color, logo_url, banner_url, announcement, verified_tier, \
         about, return_policy, created_at",
    )
    .bind(store_id)
    .bind(status)
    .bind(&note)
    .bind(admin_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("set store status", e))?;
    let (action, notice_kind) = if status == "suspended" {
        ("store.suspend", NOTIFICATION_STORE_SUSPENDED)
    } else {
        ("store.reactivate", NOTIFICATION_STORE_REACTIVATED)
    };
    let detail = match &note {
        Some(note) => serde_json::json!({ "status": status, "note": note }),
        None => serde_json::json!({ "status": status }),
    };
    record_admin_action(
        &mut *tx,
        admin_id,
        action,
        "store",
        Some(&store_id.to_string()),
        Some(detail),
    )
    .await?;
    crate::notices::enqueue_store_status_notice(
        &mut tx,
        store_id,
        &store_name,
        notice_kind,
        note.as_deref(),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit set store status", e))?;
    Ok(store)
}

// ---------------------------------------------------------------------------
// Orders: oversight queue + detail (read-only)
// ---------------------------------------------------------------------------

/// One sub-order as the admin queue shows it (admin orders
/// remake): which store, which pipeline status, how much
/// goods money. Items, timelines and shipment detail stay
/// in the detail view — a queue row is for triage.
#[derive(Debug, Serialize)]
pub struct AdminOrderSubOrder {
    pub id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub store_name: String,
    pub status: String,
    pub goods_subtotal_poisha: i64,
}

#[derive(Debug, FromRow)]
struct AdminOrderSubOrderRow {
    id: Uuid,
    order_id: Uuid,
    store_id: Uuid,
    store_slug: String,
    store_name: String,
    status: String,
    goods_subtotal_poisha: i64,
}

/// One order in the admin queue (admin orders remake). The
/// pre-remake list returned the buyer's full [`Order`] view
/// per row — full delivery name, phone, address and note,
/// plus every sub-order's items, timeline and steps —
/// assembled by running the shared order loader once per
/// row (a page of 25 cost roughly 350 queries). The queue
/// row carries only what triage needs, with the delivery
/// phone MASKED (the Wave 28 list canon): the full delivery
/// block lives in [`AdminOrderDetail`] only. Money fields
/// are the stored checkout snapshots, under the same names
/// the buyer view uses.
#[derive(Debug, Serialize)]
pub struct AdminOrderRow {
    pub id: Uuid,
    /// Parent status: `placed` or `cancelled` (the parent
    /// status is write-once; the pipeline lives on the
    /// sub-orders below).
    pub status: String,
    pub payment_method: String,
    pub payment_status: String,
    pub advance_required_poisha: i64,
    pub advance_paid_poisha: i64,
    /// The CR-15 degradation flag, derived exactly as the
    /// overview's `advance_unfulfilled` block defines it:
    /// an advance was required, none could be collected,
    /// and the order proceeded as plain COD.
    pub advance_unfulfilled: bool,
    pub delivery_name: String,
    /// The delivery phone, masked via `domain::mask_phone`.
    pub delivery_phone_masked: String,
    pub delivery_zone: String,
    pub delivery_district: Option<String>,
    pub goods_total_poisha: i64,
    pub discount_total_poisha: i64,
    pub loyalty_discount_poisha: i64,
    pub gift_card_discount_poisha: i64,
    pub delivery_total_poisha: i64,
    pub grand_total_poisha: i64,
    pub created_at: DateTime<Utc>,
    pub sub_orders: Vec<AdminOrderSubOrder>,
}

#[derive(Debug, FromRow)]
struct AdminOrderRowRow {
    id: Uuid,
    status: String,
    payment_method: String,
    payment_status: String,
    advance_required_poisha: i64,
    advance_paid_poisha: i64,
    delivery_name: String,
    delivery_phone: String,
    delivery_zone: String,
    delivery_district: Option<String>,
    goods_total_poisha: i64,
    discount_total_poisha: i64,
    loyalty_discount_poisha: i64,
    gift_card_discount_poisha: i64,
    delivery_total_poisha: i64,
    grand_total_poisha: i64,
    created_at: DateTime<Utc>,
}

impl AdminOrderRowRow {
    fn into_view(self, sub_orders: Vec<AdminOrderSubOrder>) -> AdminOrderRow {
        AdminOrderRow {
            advance_unfulfilled: self.payment_status == "pay_on_delivery"
                && self.advance_required_poisha > self.advance_paid_poisha
                && self.status != "cancelled",
            delivery_phone_masked: mask_phone(&self.delivery_phone),
            id: self.id,
            status: self.status,
            payment_method: self.payment_method,
            payment_status: self.payment_status,
            advance_required_poisha: self.advance_required_poisha,
            advance_paid_poisha: self.advance_paid_poisha,
            delivery_name: self.delivery_name,
            delivery_zone: self.delivery_zone,
            delivery_district: self.delivery_district,
            goods_total_poisha: self.goods_total_poisha,
            discount_total_poisha: self.discount_total_poisha,
            loyalty_discount_poisha: self.loyalty_discount_poisha,
            gift_card_discount_poisha: self.gift_card_discount_poisha,
            delivery_total_poisha: self.delivery_total_poisha,
            grand_total_poisha: self.grand_total_poisha,
            created_at: self.created_at,
            sub_orders,
        }
    }
}

/// Whole-queue order counts for the admin list header
/// (admin orders remake), deliberately UNFILTERED — the
/// summary describes the platform, not the page (the
/// payout/store summary rule). The advance blocks and the
/// today figures use the overview metrics' own populations
/// and formulas (see `metrics::get_admin_metrics`), so the
/// queue header and the overview can never disagree: the
/// advance-pending population excludes cancelled orders,
/// the per-payment-status counts do not (exactly the
/// metrics `orders_by_payment_status` semantics).
#[derive(Debug, Serialize)]
pub struct AdminOrderSummary {
    pub total: i64,
    pub placed: i64,
    pub cancelled: i64,
    pub payment_pending: i64,
    pub payment_paid: i64,
    pub payment_failed: i64,
    pub payment_refunded: i64,
    pub payment_pay_on_delivery: i64,
    pub payment_advance_pending: i64,
    pub payment_advance_paid: i64,
    /// `payment_status = 'advance_pending'` and not
    /// cancelled — the metrics `advance_pending` block.
    pub advance_pending_count: i64,
    /// Advance money still to collect over that
    /// population: `advance_required - advance_paid`.
    pub advance_outstanding_poisha: i64,
    /// The CR-15 degraded population (plain-COD payment
    /// status, an advance required but uncollected, not
    /// cancelled) — the metrics `advance_unfulfilled`
    /// block.
    pub advance_unfulfilled_count: i64,
    /// The uncollected advance over that population.
    pub advance_unfulfilled_poisha: i64,
    /// Orders placed on the current Asia/Dhaka calendar
    /// day, all statuses — the metrics `orders_today`
    /// count.
    pub orders_today: i64,
    /// Today's GMV — the metrics `orders_today` formula:
    /// goods − discount + delivery over today's
    /// non-cancelled sub-orders.
    pub gmv_today_poisha: i64,
}

/// A page of admin orders plus the total count and the
/// whole-queue summary.
#[derive(Debug, Serialize)]
pub struct AdminOrderList {
    pub items: Vec<AdminOrderRow>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
    pub summary: AdminOrderSummary,
}

/// The whole-queue summary (admin orders remake): one
/// FILTER-count pass over `orders` plus the one sub-order
/// aggregate for today's GMV — the metrics definitions,
/// restated once here so the two surfaces agree by
/// construction and by test.
async fn order_summary(pool: &PgPool) -> Result<AdminOrderSummary, ApiError> {
    #[derive(FromRow)]
    struct OrderSummaryRow {
        total: i64,
        placed: i64,
        cancelled: i64,
        payment_pending: i64,
        payment_paid: i64,
        payment_failed: i64,
        payment_refunded: i64,
        payment_pay_on_delivery: i64,
        payment_advance_pending: i64,
        payment_advance_paid: i64,
        advance_pending_count: i64,
        advance_outstanding_poisha: i64,
        advance_unfulfilled_count: i64,
        advance_unfulfilled_poisha: i64,
        orders_today: i64,
    }
    let row: OrderSummaryRow = sqlx::query_as(
        "SELECT COUNT(*)::bigint AS total, \
           (COUNT(*) FILTER (WHERE status = 'placed'))::bigint AS placed, \
           (COUNT(*) FILTER (WHERE status = 'cancelled'))::bigint AS cancelled, \
           (COUNT(*) FILTER (WHERE payment_status = 'pending'))::bigint \
             AS payment_pending, \
           (COUNT(*) FILTER (WHERE payment_status = 'paid'))::bigint \
             AS payment_paid, \
           (COUNT(*) FILTER (WHERE payment_status = 'failed'))::bigint \
             AS payment_failed, \
           (COUNT(*) FILTER (WHERE payment_status = 'refunded'))::bigint \
             AS payment_refunded, \
           (COUNT(*) FILTER (WHERE payment_status = 'pay_on_delivery'))::bigint \
             AS payment_pay_on_delivery, \
           (COUNT(*) FILTER (WHERE payment_status = 'advance_pending'))::bigint \
             AS payment_advance_pending, \
           (COUNT(*) FILTER (WHERE payment_status = 'advance_paid'))::bigint \
             AS payment_advance_paid, \
           (COUNT(*) FILTER (WHERE payment_status = 'advance_pending' \
             AND status <> 'cancelled'))::bigint AS advance_pending_count, \
           COALESCE(SUM(advance_required_poisha - advance_paid_poisha) FILTER \
             (WHERE payment_status = 'advance_pending' \
             AND status <> 'cancelled'), 0)::bigint \
             AS advance_outstanding_poisha, \
           (COUNT(*) FILTER (WHERE payment_status = 'pay_on_delivery' \
             AND advance_required_poisha > advance_paid_poisha \
             AND status <> 'cancelled'))::bigint AS advance_unfulfilled_count, \
           COALESCE(SUM(advance_required_poisha - advance_paid_poisha) FILTER \
             (WHERE payment_status = 'pay_on_delivery' \
             AND advance_required_poisha > advance_paid_poisha \
             AND status <> 'cancelled'), 0)::bigint \
             AS advance_unfulfilled_poisha, \
           (COUNT(*) FILTER (WHERE (created_at AT TIME ZONE 'Asia/Dhaka')::date \
             = (now() AT TIME ZONE 'Asia/Dhaka')::date))::bigint AS orders_today \
         FROM orders",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("admin order summary", e))?;

    let gmv_today_poisha: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(goods_subtotal_poisha - discount_poisha \
           + delivery_charge_poisha), 0)::bigint \
         FROM sub_orders \
         WHERE (created_at AT TIME ZONE 'Asia/Dhaka')::date \
             = (now() AT TIME ZONE 'Asia/Dhaka')::date \
           AND status <> 'cancelled'",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("admin order summary gmv", e))?;

    Ok(AdminOrderSummary {
        total: row.total,
        placed: row.placed,
        cancelled: row.cancelled,
        payment_pending: row.payment_pending,
        payment_paid: row.payment_paid,
        payment_failed: row.payment_failed,
        payment_refunded: row.payment_refunded,
        payment_pay_on_delivery: row.payment_pay_on_delivery,
        payment_advance_pending: row.payment_advance_pending,
        payment_advance_paid: row.payment_advance_paid,
        advance_pending_count: row.advance_pending_count,
        advance_outstanding_poisha: row.advance_outstanding_poisha,
        advance_unfulfilled_count: row.advance_unfulfilled_count,
        advance_unfulfilled_poisha: row.advance_unfulfilled_poisha,
        orders_today: row.orders_today,
        gmv_today_poisha,
    })
}

/// The ORDER BY fragment for an order-list sort — a fixed
/// string per variant (user input never reaches the SQL
/// text), every arm ending in the `id` tiebreaker the
/// deterministic-order rule requires.
fn order_order_by(sort: OrderSort) -> &'static str {
    match sort {
        OrderSort::Newest => "ORDER BY o.created_at DESC, o.id DESC",
        OrderSort::Oldest => "ORDER BY o.created_at ASC, o.id ASC",
        OrderSort::AmountDesc => "ORDER BY o.grand_total_poisha DESC, o.id DESC",
        OrderSort::AmountAsc => "ORDER BY o.grand_total_poisha ASC, o.id DESC",
    }
}

/// The filter predicates shared by the count and the page
/// query (they must never drift — the stores remake's
/// count/items lesson). Parameters, in order: status,
/// payment_status, sub_status, store_id, advance_state,
/// phone (canonical), raw phone text, id prefix pattern,
/// text pattern.
const ADMIN_ORDER_FILTERS: &str = "($1::text IS NULL OR o.status = $1) \
    AND ($2::text IS NULL OR o.payment_status = $2) \
    AND ($3::text IS NULL OR EXISTS (SELECT 1 FROM sub_orders sx \
          WHERE sx.order_id = o.id AND sx.status = $3)) \
    AND ($4::uuid IS NULL OR EXISTS (SELECT 1 FROM sub_orders sy \
          WHERE sy.order_id = o.id AND sy.store_id = $4)) \
    AND ($5::text IS NULL \
          OR ($5 = 'pending' AND o.payment_status = 'advance_pending' \
              AND o.status <> 'cancelled') \
          OR ($5 = 'unfulfilled' AND o.payment_status = 'pay_on_delivery' \
              AND o.advance_required_poisha > o.advance_paid_poisha \
              AND o.status <> 'cancelled')) \
    AND ($7::text IS NULL OR o.delivery_phone = $6 OR u.phone = $6 \
          OR o.delivery_phone = $7 \
          OR o.id::text LIKE $8 \
          OR o.delivery_name ILIKE $9 \
          OR EXISTS (SELECT 1 FROM sub_orders sz \
              JOIN stores st ON st.id = sz.store_id \
              WHERE sz.order_id = o.id \
                AND (st.name ILIKE $9 OR st.slug ILIKE $9)))";

/// List orders for the admin oversight queue (admin
/// orders remake): a purpose-built row (see
/// [`AdminOrderRow`]) assembled set-wise — one page query,
/// one grouped sub-order query for the page's ids, one
/// count and the summary; the pre-remake per-row
/// `load_order` loop is gone from this path (the buyer
/// loader itself is untouched).
///
/// Filters, all validated fail-closed (an unknown value
/// is a 400, never a silently empty page): `status`
/// (parent: placed/cancelled), `payment_status` (the
/// seven-value vocabulary), `sub_status` (orders having
/// at least one sub-order in that pipeline status),
/// `store_id`, and `advance_state` — `pending` /
/// `unfulfilled`, the overview metrics' two advance
/// populations, for the attention queues the overview
/// links here. `q` searches, following the platform's
/// phone discipline: a BD phone in any typed form
/// (Bengali digits transliterated first) matches the
/// delivery phone or the buyer's account phone EXACTLY —
/// phone numbers are never substring-searched; an
/// order-id prefix (4+ hex/dash characters) matches by
/// id prefix; any other text matches the delivery name
/// or a store's name/slug as a LIKE-escaped substring.
#[allow(clippy::too_many_arguments)]
pub async fn list_orders(
    pool: &PgPool,
    q: Option<&str>,
    status: Option<&str>,
    payment_status: Option<&str>,
    sub_status: Option<&str>,
    store_id: Option<Uuid>,
    advance_state: Option<&str>,
    sort: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminOrderList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let status: Option<&str> = match status.map(str::trim) {
        None | Some("") => None,
        Some("placed") => Some("placed"),
        Some("cancelled") => Some("cancelled"),
        Some(_) => {
            return Err(ApiError::bad_request(
                "status must be one of: placed, cancelled",
            ))
        }
    };
    let payment_status: Option<&str> = match payment_status.map(str::trim) {
        None | Some("") => None,
        Some(s @ ("pending" | "paid" | "failed" | "refunded" | "pay_on_delivery"
        | "advance_pending" | "advance_paid")) => Some(s),
        Some(_) => {
            return Err(ApiError::bad_request(
                "payment_status must be one of: pending, paid, failed, refunded, \
                 pay_on_delivery, advance_pending, advance_paid",
            ))
        }
    };
    let sub_status: Option<&str> = match sub_status.map(str::trim) {
        None | Some("") => None,
        Some(s @ ("placed" | "confirmed" | "shipped" | "delivered" | "cancelled"
        | "returned")) => Some(s),
        Some(_) => {
            return Err(ApiError::bad_request(
                "sub_status must be one of: placed, confirmed, shipped, delivered, \
                 cancelled, returned",
            ))
        }
    };
    let advance_state: Option<&str> = match advance_state.map(str::trim) {
        None | Some("") => None,
        Some("pending") => Some("pending"),
        Some("unfulfilled") => Some("unfulfilled"),
        Some(_) => {
            return Err(ApiError::bad_request(
                "advance_state must be one of: pending, unfulfilled",
            ))
        }
    };
    let sort = validate_order_list_sort(sort).map_err(ApiError::bad_request)?;

    let q_trim: Option<String> = q
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(bn_to_en_digits);
    let phone_exact: Option<String> =
        q_trim.as_deref().and_then(normalize_bd_phone);
    let id_prefix: Option<String> = q_trim.as_deref().and_then(|text| {
        let lowered = text.to_lowercase();
        if lowered.len() >= 4
            && lowered
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == '-')
        {
            // Hex digits and dashes carry no LIKE wildcards,
            // so the prefix pattern needs no escaping.
            Some(format!("{lowered}%"))
        } else {
            None
        }
    });
    let text_pattern: Option<String> = q_trim.as_deref().map(search_like_pattern);

    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM orders o \
         JOIN users u ON u.id = o.buyer_user_id \
         WHERE {ADMIN_ORDER_FILTERS}"
    ))
    .bind(status)
    .bind(payment_status)
    .bind(sub_status)
    .bind(store_id)
    .bind(advance_state)
    .bind(&phone_exact)
    .bind(&q_trim)
    .bind(&id_prefix)
    .bind(&text_pattern)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin orders", e))?;

    let rows: Vec<AdminOrderRowRow> = sqlx::query_as(&format!(
        "SELECT o.id, o.status, o.payment_method, o.payment_status, \
           o.advance_required_poisha, o.advance_paid_poisha, \
           o.delivery_name, o.delivery_phone, o.delivery_zone, \
           o.delivery_district, o.goods_total_poisha, \
           o.discount_total_poisha, o.loyalty_discount_poisha, \
           o.gift_card_discount_poisha, o.delivery_total_poisha, \
           o.grand_total_poisha, o.created_at \
         FROM orders o \
         JOIN users u ON u.id = o.buyer_user_id \
         WHERE {ADMIN_ORDER_FILTERS} \
         {} LIMIT $10 OFFSET $11",
        order_order_by(sort)
    ))
    .bind(status)
    .bind(payment_status)
    .bind(sub_status)
    .bind(store_id)
    .bind(advance_state)
    .bind(&phone_exact)
    .bind(&q_trim)
    .bind(&id_prefix)
    .bind(&text_pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin orders", e))?;

    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let sub_rows: Vec<AdminOrderSubOrderRow> = sqlx::query_as(
        "SELECT so.id, so.order_id, so.store_id, st.slug AS store_slug, \
           st.name AS store_name, so.status, so.goods_subtotal_poisha \
         FROM sub_orders so \
         JOIN stores st ON st.id = so.store_id \
         WHERE so.order_id = ANY($1) \
         ORDER BY so.created_at, so.id",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin order sub-orders", e))?;
    let mut sub_by_order: std::collections::HashMap<Uuid, Vec<AdminOrderSubOrder>> =
        std::collections::HashMap::new();
    for sub in sub_rows {
        sub_by_order.entry(sub.order_id).or_default().push(
            AdminOrderSubOrder {
                id: sub.id,
                store_id: sub.store_id,
                store_slug: sub.store_slug,
                store_name: sub.store_name,
                status: sub.status,
                goods_subtotal_poisha: sub.goods_subtotal_poisha,
            },
        );
    }

    let summary = order_summary(pool).await?;

    Ok(AdminOrderList {
        items: rows
            .into_iter()
            .map(|row| {
                let subs = sub_by_order.remove(&row.id).unwrap_or_default();
                row.into_view(subs)
            })
            .collect(),
        total,
        page,
        limit,
        summary,
    })
}

/// One order in full, for the admin working a support
/// case (admin orders remake): the buyer's own [`Order`]
/// view — items, per-sub-order timelines and steps,
/// shipment summaries, the full delivery block — plus the
/// two admin-only extras below. The full delivery phone
/// and address exist ONLY here, never in the queue list
/// (the payouts detail precedent: the detail is where the
/// work happens). The read is not reason-gated or
/// separately audited: a delivery phone is operational
/// data the seller already sees to ship the parcel, not a
/// national credential like the verification NID (whose
/// reveal IS gated — see `verification`).
#[derive(Debug, Serialize)]
pub struct AdminOrderDetail {
    #[serde(flatten)]
    pub order: Order,
    /// The buyer's ACCOUNT phone, masked — it can differ
    /// from the delivery phone on the order, and the
    /// masked form is enough to confirm identity on a
    /// support call without a second full number.
    pub buyer_phone_masked: String,
    /// Cash the courier still collects on delivery — the
    /// receipt's own figure (`build_receipt`): the grand
    /// total minus any settled advance, for COD orders;
    /// zero for online-paid orders.
    pub cod_amount_due_poisha: i64,
}

/// Read one order's admin detail. The shared loader runs
/// first and 404s before any extra read (the payout/store
/// detail order).
pub async fn get_admin_order_detail(
    pool: &PgPool,
    order_id: Uuid,
) -> Result<AdminOrderDetail, ApiError> {
    let order = load_order(pool, order_id).await?;
    let buyer_phone: String =
        sqlx::query_scalar("SELECT phone FROM users WHERE id = $1")
            .bind(order.buyer_user_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("load order buyer phone", e))?;
    let cod_amount_due_poisha = if order.payment_method == "cod" {
        order.grand_total_poisha - order.advance_paid_poisha
    } else {
        0
    };
    Ok(AdminOrderDetail {
        buyer_phone_masked: mask_phone(&buyer_phone),
        cod_amount_due_poisha,
        order,
    })
}

// ---------------------------------------------------------------------------
// Products: cross-store queue + takedown/restore (admin products remake)
// ---------------------------------------------------------------------------

/// One product in the admin queue: the shared product
/// view (the same materialisation every product surface
/// uses — effective stock, rating, sold count, live flash
/// price, and the takedown state on the row itself) plus
/// the store it belongs to. Products are public data, so
/// nothing here is masked; no seller phone is carried at
/// all (the store detail is one click away for that).
#[derive(Debug, Serialize)]
pub struct AdminProduct {
    #[serde(flatten)]
    pub product: Product,
    pub store_name: String,
    pub store_status: String,
}

/// Whole-queue counts for the admin product list (admin
/// products remake): by seller status, the taken-down
/// worklist size, live-catalogue stock alerts and
/// image-less listings, over ALL products — deliberately
/// UNFILTERED, in the store summary's style: the summary
/// describes the platform, not the page. `out_of_stock`
/// counts only products a buyer could otherwise see
/// (active and not taken down), matching the overview
/// metrics' stock alerts.
#[derive(Debug, Serialize)]
pub struct AdminProductSummary {
    pub total: i64,
    pub draft: i64,
    pub active: i64,
    pub archived: i64,
    pub taken_down: i64,
    pub out_of_stock: i64,
    pub no_image: i64,
}

/// A page of admin products plus the total count and the
/// whole-queue summary.
#[derive(Debug, Serialize)]
pub struct AdminProductList {
    pub items: Vec<AdminProduct>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
    pub summary: AdminProductSummary,
}

/// The whole-queue summary query (admin products
/// remake), in the payout summary's FILTER-count style.
async fn product_summary(pool: &PgPool) -> Result<AdminProductSummary, ApiError> {
    #[derive(FromRow)]
    struct ProductSummaryRow {
        total: i64,
        draft: i64,
        active: i64,
        archived: i64,
        taken_down: i64,
        out_of_stock: i64,
        no_image: i64,
    }
    let row: ProductSummaryRow = sqlx::query_as(&format!(
        "SELECT COUNT(*)::bigint AS total, \
           (COUNT(*) FILTER (WHERE p.status = 'draft'))::bigint AS draft, \
           (COUNT(*) FILTER (WHERE p.status = 'active'))::bigint AS active, \
           (COUNT(*) FILTER (WHERE p.status = 'archived'))::bigint AS archived, \
           (COUNT(*) FILTER (WHERE p.admin_hidden_at IS NOT NULL))::bigint \
             AS taken_down, \
           (COUNT(*) FILTER (WHERE p.status = 'active' \
             AND p.admin_hidden_at IS NULL AND {EFFECTIVE_STOCK_SQL} <= 0))::bigint \
             AS out_of_stock, \
           (COUNT(*) FILTER (WHERE NOT EXISTS \
             (SELECT 1 FROM product_images pi \
              WHERE pi.product_id = p.id)))::bigint AS no_image \
         FROM products p"
    ))
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("admin product summary", e))?;
    Ok(AdminProductSummary {
        total: row.total,
        draft: row.draft,
        active: row.active,
        archived: row.archived,
        taken_down: row.taken_down,
        out_of_stock: row.out_of_stock,
        no_image: row.no_image,
    })
}

/// The filter predicates shared by the admin product
/// list's COUNT and page queries (the orders remake's
/// shared-filters rule: the total must count exactly the
/// set the page draws from). Static fragments only — all
/// values stay bound parameters. $1 = LIKE-escaped search
/// pattern, $2 = seller status, $3 = store id,
/// $4 = source, $5 = taken-down flag, $6 = stock state,
/// $7 = has-images flag.
const ADMIN_PRODUCT_FILTERS: &str = "($1::text IS NULL OR p.title ILIKE $1 \
    OR p.title_bn ILIKE $1 OR s.name ILIKE $1 OR s.slug ILIKE $1) \
    AND ($2::text IS NULL OR p.status = $2) \
    AND ($3::uuid IS NULL OR p.store_id = $3) \
    AND ($4::text IS NULL OR p.source = $4) \
    AND ($5::boolean IS NULL OR (p.admin_hidden_at IS NOT NULL) = $5)";

/// The stock-state half of the admin product filters,
/// over the shared effective-stock expression (the
/// variant-sum CASE — never the raw stored column, which
/// is only a display sum once variants exist). $6 is the
/// validated stock-state literal.
fn admin_product_stock_filter() -> String {
    format!(
        "($6::text IS NULL \
         OR ($6 = 'out_of_stock' AND {EFFECTIVE_STOCK_SQL} <= 0) \
         OR ($6 = 'low' AND {EFFECTIVE_STOCK_SQL} \
               BETWEEN 1 AND COALESCE(p.low_stock_threshold, 5)) \
         OR ($6 = 'in_stock' AND {EFFECTIVE_STOCK_SQL} \
               > COALESCE(p.low_stock_threshold, 5))) \
         AND ($7::boolean IS NULL OR EXISTS \
               (SELECT 1 FROM product_images pi \
                WHERE pi.product_id = p.id) = $7)"
    )
}

/// The ORDER BY fragment for a product-list sort — a
/// fixed string per variant (user input never reaches
/// the SQL text), every arm ending in the `id`
/// tiebreaker the deterministic-order rule requires.
/// Price sorts use the BASE price (the seller's listed
/// price — what an admin judges a listing by); the live
/// flash price rides on each row for display.
fn product_order_by(sort: ProductSort) -> &'static str {
    match sort {
        ProductSort::Newest => "ORDER BY p.created_at DESC, p.id DESC",
        ProductSort::Oldest => "ORDER BY p.created_at ASC, p.id ASC",
        ProductSort::PriceAsc => "ORDER BY p.price_poisha ASC, p.id DESC",
        ProductSort::PriceDesc => "ORDER BY p.price_poisha DESC, p.id DESC",
        ProductSort::SoldDesc => "ORDER BY sold_count DESC, p.id DESC",
        ProductSort::RatingDesc => {
            "ORDER BY rating_avg DESC NULLS LAST, rating_count DESC, p.id DESC"
        }
    }
}

/// The Bangla label of a product status, for error
/// prose — the same words the web's product status
/// labels show.
fn product_status_label_bn(status: &str) -> &str {
    match status {
        "draft" => "খসড়া",
        "active" => "সক্রিয়",
        "archived" => "আর্কাইভ",
        _ => status,
    }
}

/// List products for the admin queue (admin products
/// remake): the cross-store catalogue admins never had.
/// `q` matches title, Bangla title, store name or slug
/// (LIKE-escaped via `domain::search_like_pattern`);
/// `status`, `source`, `visibility` and `stock_state`
/// filter exactly and are validated fail-closed (an
/// unknown value is a 400 naming the allowed set, never
/// a silently empty page); `has_images` takes `true` or
/// `false`; `sort` is validated by the domain. The total
/// and the items run the SAME joins and filters.
#[allow(clippy::too_many_arguments)]
pub async fn list_products(
    pool: &PgPool,
    q: Option<&str>,
    status: Option<&str>,
    store_id: Option<Uuid>,
    source: Option<&str>,
    visibility: Option<&str>,
    stock_state: Option<&str>,
    has_images: Option<&str>,
    sort: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminProductList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let pattern: Option<String> = q
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(search_like_pattern);
    let status: Option<&str> = match status.map(str::trim) {
        None | Some("") => None,
        Some("draft") => Some("draft"),
        Some("active") => Some("active"),
        Some("archived") => Some("archived"),
        Some(_) => {
            return Err(ApiError::bad_request(
                "status must be one of: draft, active, archived",
            ))
        }
    };
    let source: Option<&str> = match source.map(str::trim) {
        None | Some("") => None,
        Some("manual") => Some("manual"),
        Some("facebook") => Some("facebook"),
        Some("bulk") => Some("bulk"),
        Some(_) => {
            return Err(ApiError::bad_request(
                "source must be one of: manual, facebook, bulk",
            ))
        }
    };
    let taken_down: Option<bool> = match visibility.map(str::trim) {
        None | Some("") => None,
        Some("visible") => Some(false),
        Some("taken_down") => Some(true),
        Some(_) => {
            return Err(ApiError::bad_request(
                "visibility must be one of: visible, taken_down",
            ))
        }
    };
    let stock_state: Option<&str> = match stock_state.map(str::trim) {
        None | Some("") => None,
        Some("in_stock") => Some("in_stock"),
        Some("low") => Some("low"),
        Some("out_of_stock") => Some("out_of_stock"),
        Some(_) => {
            return Err(ApiError::bad_request(
                "stock_state must be one of: in_stock, low, out_of_stock",
            ))
        }
    };
    let has_images: Option<bool> = match has_images.map(str::trim) {
        None | Some("") => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(_) => {
            return Err(ApiError::bad_request(
                "has_images must be one of: true, false",
            ))
        }
    };
    let sort = validate_admin_product_list_sort(sort).map_err(ApiError::bad_request)?;

    let filters = format!("{ADMIN_PRODUCT_FILTERS} AND {}", admin_product_stock_filter());
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM products p \
         JOIN stores s ON s.id = p.store_id \
         WHERE {filters}"
    ))
    .bind(&pattern)
    .bind(status)
    .bind(store_id)
    .bind(source)
    .bind(taken_down)
    .bind(stock_state)
    .bind(has_images)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin products", e))?;

    let mut items: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products p \
         JOIN stores s ON s.id = p.store_id {PRODUCT_RATING_JOIN} \
         WHERE {filters} {} LIMIT $8 OFFSET $9",
        product_order_by(sort)
    ))
    .bind(&pattern)
    .bind(status)
    .bind(store_id)
    .bind(source)
    .bind(taken_down)
    .bind(stock_state)
    .bind(has_images)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin products", e))?
    .into_iter()
    .map(with_stock_status)
    .collect();
    // Live flash prices attach like every product surface.
    crate::merch::attach_flash_prices(pool, &mut items, Utc::now()).await?;

    // Store context for the page's rows, in one query —
    // the rows' own store ids are the only ones read.
    let page_store_ids: Vec<Uuid> = items.iter().map(|item| item.store_id).collect();
    let store_rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, name, status FROM stores WHERE id = ANY($1)",
    )
    .bind(&page_store_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load admin product stores", e))?;
    let store_by_id: std::collections::HashMap<Uuid, (String, String)> = store_rows
        .into_iter()
        .map(|(id, name, status)| (id, (name, status)))
        .collect();

    let summary = product_summary(pool).await?;

    Ok(AdminProductList {
        items: items
            .into_iter()
            .map(|product| {
                let (store_name, store_status) = store_by_id
                    .get(&product.store_id)
                    .cloned()
                    .unwrap_or_default();
                AdminProduct {
                    product,
                    store_name,
                    store_status,
                }
            })
            .collect(),
        total,
        page,
        limit,
        summary,
    })
}

/// The store context on the admin product detail: the
/// payout detail's slim-summary precedent (identity,
/// status, tier) — the full store picture is the admin
/// store detail's job, one link away.
#[derive(Debug, Serialize)]
pub struct AdminProductStoreSummary {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub status: String,
    pub verified_tier: String,
}

/// The sales block on the admin product detail: what the
/// ledger says this product has done. `goods_poisha` is
/// the SUM of the order lines' stored totals over
/// NON-CANCELLED sub-orders (the analytics top-products
/// population): ordered goods value from the checkout
/// snapshots — NOT earnings (commission, discounts and
/// gift cards all move the seller's take away from this
/// figure; the seller's balance is the earnings truth).
/// Delivered units are the product view's own
/// `sold_count`, the platform's one sold definition.
#[derive(Debug, Serialize)]
pub struct AdminProductSales {
    pub goods_poisha: i64,
    pub units_ordered: i64,
}

/// One product's admin detail: the queue row plus the
/// gallery, ALL variants (inactive included — a
/// deactivated variant is part of the listing being
/// judged), the store summary, the sales block, and the
/// product's audit history (takedowns and restores DO
/// target products, so the history is real signal here,
/// not the theatre the orders remake cut).
#[derive(Debug, Serialize)]
pub struct AdminProductDetail {
    #[serde(flatten)]
    pub item: AdminProduct,
    pub store: AdminProductStoreSummary,
    pub images: Vec<crate::product_images::GalleryImageView>,
    pub variants: Vec<crate::variants::AdminVariantView>,
    pub sales: AdminProductSales,
    pub audit_history: Vec<crate::audit::AdminAuditEntry>,
}

/// Read one product's admin detail. The shared product
/// loader runs first and 404s before any extra read
/// (the payout/store detail order) — and it is the
/// UNSHARED-visibility loader: admins see drafts,
/// archived products and taken-down products alike.
pub async fn get_admin_product_detail(
    pool: &PgPool,
    product_id: Uuid,
) -> Result<AdminProductDetail, ApiError> {
    let product = get_product_by_id(pool, product_id).await?;
    let store = get_store_by_id(pool, product.store_id).await?;
    let images = crate::product_images::load_product_gallery(pool, product_id).await?;
    let variants = crate::variants::load_all_variants(pool, &product).await?;
    let (goods_poisha, units_ordered): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(oi.line_total_poisha), 0)::bigint, \
         COALESCE(SUM(oi.quantity), 0)::bigint \
         FROM order_items oi \
         JOIN sub_orders so ON so.id = oi.sub_order_id \
         WHERE oi.product_id = $1 AND so.status <> 'cancelled'",
    )
    .bind(product_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("load admin product sales", e))?;
    let audit_history =
        crate::audit::list_audit_for_target(pool, "product", &product_id.to_string(), 20)
            .await?;
    Ok(AdminProductDetail {
        item: AdminProduct {
            store_name: store.name.clone(),
            store_status: store.status.clone(),
            product,
        },
        store: AdminProductStoreSummary {
            id: store.id,
            slug: store.slug,
            name: store.name,
            status: store.status,
            verified_tier: store.verified_tier,
        },
        images,
        variants,
        sales: AdminProductSales {
            goods_poisha,
            units_ordered,
        },
        audit_history,
    })
}

/// Take one product down, or restore a taken-down one
/// (admin products remake). Returns the shared product
/// view.
///
/// The gates, in the order they fire (the
/// payouts/verifications/stores precedence):
///
/// 1. INPUT (400): the note — a takedown's REQUIRED
///    reason ([`validate_product_takedown_reason`]) or a
///    restore's REQUIRED note
///    ([`validate_product_restore_note`]; restore is
///    note-gated by deliberate divergence from store
///    reactivation — see the module doc).
/// 2. LOCKS: the product's store row, then the product
///    row, `FOR UPDATE` inside the transition
///    transaction (404 when the product is absent) — the
///    store-first lock order every money path here uses.
/// 3. NO-OP (409): taking down an already-taken-down
///    product, or restoring one that stands, is refused,
///    naming the current state. Nothing is written: no
///    stamp bump, no audit row, no notice — and a second
///    admin's restore can never silently erase a
///    re-takedown that landed after their screen loaded,
///    because the legality is re-read under the lock.
/// 4. WRITE + AUDIT + NOTICE, one transaction: the flag
///    columns (set on takedown; cleared on restore —
///    restore never touches the seller's `status`), ONE
///    audit row whose detail contractually carries the
///    reason (on restore: the note PLUS the outgoing
///    reason and hider, copied BEFORE the row is
///    cleared, so clearing never destroys the only
///    copy), and the `product_taken_down` /
///    `product_restored` notice to the store owner,
///    enqueued through the one funnel in `notices` with
///    the reason copied into its payload at takedown
///    time.
///
/// Takedown is allowed whatever the seller's status and
/// whatever the store's status: a draft about to be
/// published, and a suspended store's product that must
/// stay down after the store is reactivated, are exactly
/// the cases the flag exists for.
pub async fn set_product_takedown(
    pool: &PgPool,
    admin_id: Uuid,
    product_id: Uuid,
    taken_down: bool,
    note: Option<&str>,
) -> Result<Product, ApiError> {
    let note: String = if taken_down {
        validate_product_takedown_reason(note.unwrap_or(""))
            .map_err(ApiError::bad_request)?
    } else {
        validate_product_restore_note(note.unwrap_or("")).map_err(ApiError::bad_request)?
    };
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin product takedown", e))?;
    let store_id: Uuid = sqlx::query_scalar("SELECT store_id FROM products WHERE id = $1")
        .bind(product_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("load product store for takedown", e))?
        .ok_or_else(|| ApiError::not_found("product not found"))?;
    let store_name: String =
        sqlx::query_scalar("SELECT name FROM stores WHERE id = $1 FOR UPDATE")
            .bind(store_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("lock store for product takedown", e))?;
    #[derive(FromRow)]
    struct LockedProduct {
        title: String,
        status: String,
        admin_hidden_at: Option<DateTime<Utc>>,
        admin_hidden_reason: Option<String>,
        admin_hidden_by_user_id: Option<Uuid>,
    }
    let current: LockedProduct = sqlx::query_as(
        "SELECT title, status, admin_hidden_at, admin_hidden_reason, \
         admin_hidden_by_user_id FROM products WHERE id = $1 FOR UPDATE",
    )
    .bind(product_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock product for takedown", e))?;
    let is_hidden = current.admin_hidden_at.is_some();
    if taken_down && is_hidden {
        return Err(ApiError::conflict(
            "পণ্যটি ইতিমধ্যে প্ল্যাটফর্ম থেকে সরিয়ে নেওয়া আছে — this product is already taken down",
        ));
    }
    if !taken_down && !is_hidden {
        return Err(ApiError::conflict(format!(
            "পণ্যটি সরিয়ে নেওয়া হয়নি (বর্তমান অবস্থা: {}) — this product is not taken down",
            product_status_label_bn(&current.status)
        )));
    }
    if taken_down {
        sqlx::query(
            "UPDATE products SET admin_hidden_at = now(), \
             admin_hidden_by_user_id = $2, admin_hidden_reason = $3, \
             updated_at = now() WHERE id = $1",
        )
        .bind(product_id)
        .bind(admin_id)
        .bind(&note)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("take product down", e))?;
    } else {
        sqlx::query(
            "UPDATE products SET admin_hidden_at = NULL, \
             admin_hidden_by_user_id = NULL, admin_hidden_reason = NULL, \
             updated_at = now() WHERE id = $1",
        )
        .bind(product_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("restore product", e))?;
    }
    let (action, notice_kind, detail) = if taken_down {
        (
            "product.takedown",
            NOTIFICATION_PRODUCT_TAKEN_DOWN,
            serde_json::json!({
                "reason": note,
                "store_id": store_id.to_string(),
                "previous_status": current.status,
            }),
        )
    } else {
        (
            "product.restore",
            NOTIFICATION_PRODUCT_RESTORED,
            serde_json::json!({
                "note": note,
                "previous_reason": current.admin_hidden_reason,
                "previous_hidden_by": current
                    .admin_hidden_by_user_id
                    .map(|id| id.to_string()),
            }),
        )
    };
    record_admin_action(
        &mut *tx,
        admin_id,
        action,
        "product",
        Some(&product_id.to_string()),
        Some(detail),
    )
    .await?;
    crate::notices::enqueue_product_status_notice(
        &mut tx,
        store_id,
        &store_name,
        product_id,
        &current.title,
        notice_kind,
        taken_down.then_some(note.as_str()),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit product takedown", e))?;
    get_product_by_id(pool, product_id).await
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
    /// Status filter: `active` | `suspended` (admin stores
    /// remake); an unknown value is a 400.
    pub status: Option<String>,
    /// Verification-tier filter: `unverified` | `basic` |
    /// `verified`; an unknown value is a 400.
    pub tier: Option<String>,
    /// newest | oldest | name_asc | gmv_desc |
    /// followers_desc.
    pub sort: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct AdminOrdersQuery {
    /// Search: delivery/account phone (exact, any typed
    /// form), order-id prefix, delivery name, or store
    /// name/slug substring (admin orders remake).
    pub q: Option<String>,
    /// Parent status filter: `placed` | `cancelled`; an
    /// unknown value is a 400.
    pub status: Option<String>,
    /// Payment-status filter (the seven-value
    /// vocabulary); an unknown value is a 400.
    pub payment_status: Option<String>,
    /// Pipeline filter: orders having at least one
    /// sub-order in this status.
    pub sub_status: Option<String>,
    /// Restrict to orders containing this store.
    pub store_id: Option<Uuid>,
    /// The overview's advance queues: `pending` |
    /// `unfulfilled`.
    pub advance_state: Option<String>,
    /// newest | oldest | amount_desc | amount_asc.
    pub sort: Option<String>,
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
pub struct AdminProductsQuery {
    /// Search: product title / Bangla title, or store
    /// name / slug substring (LIKE-escaped).
    pub q: Option<String>,
    /// Seller-status filter: `draft` | `active` |
    /// `archived`; an unknown value is a 400.
    pub status: Option<String>,
    /// Restrict to one store's catalogue.
    pub store_id: Option<Uuid>,
    /// Origin filter: `manual` | `facebook` | `bulk`; an
    /// unknown value is a 400.
    pub source: Option<String>,
    /// Platform-visibility filter: `visible` |
    /// `taken_down`; an unknown value is a 400.
    pub visibility: Option<String>,
    /// Effective-stock filter: `in_stock` | `low` |
    /// `out_of_stock`; an unknown value is a 400.
    pub stock_state: Option<String>,
    /// Image filter: `true` | `false`; an unknown value
    /// is a 400.
    pub has_images: Option<String>,
    /// newest | oldest | price_asc | price_desc |
    /// sold_desc | rating_desc.
    pub sort: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct TakedownProductInput {
    /// The takedown reason. REQUIRED by the transition
    /// (a missing body, a missing field, or a blank
    /// reason are all the same 400) — the field stays
    /// `Option` here so an absent body reaches the
    /// service's validation instead of dying in the
    /// extractor, the `RejectPayoutInput` pattern.
    pub note: Option<String>,
}

#[derive(Deserialize)]
pub struct RestoreProductInput {
    /// The restore note. REQUIRED by the transition, for
    /// the same reason as the takedown reason — see
    /// `set_product_takedown`.
    pub note: Option<String>,
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
    /// The suspension note. REQUIRED when suspending (a
    /// missing body field, an absent note, or a blank note
    /// are all the same 400 — the field stays `Option` here
    /// so an absent note reaches the service's validation
    /// instead of dying in the extractor, the
    /// `RejectPayoutInput` pattern). Ignored on reactivation.
    pub note: Option<String>,
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
        list_stores(
            pool,
            query.q.as_deref(),
            query.status.as_deref(),
            query.tier.as_deref(),
            query.sort.as_deref(),
            query.page,
            query.limit,
        )
        .await?,
    ))
}

pub async fn get_admin_store_detail_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminStoreDetail>, ApiError> {
    require_admin(&state, &user).await?;
    Ok(Json(get_admin_store_detail(&state, id).await?))
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
        set_store_status(pool, admin.id, id, &input.status, input.note.as_deref()).await?,
    ))
}

pub async fn list_orders_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminOrdersQuery>,
) -> Result<Json<AdminOrderList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_orders(
            pool,
            query.q.as_deref(),
            query.status.as_deref(),
            query.payment_status.as_deref(),
            query.sub_status.as_deref(),
            query.store_id,
            query.advance_state.as_deref(),
            query.sort.as_deref(),
            query.page,
            query.limit,
        )
        .await?,
    ))
}

pub async fn get_order_detail_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminOrderDetail>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(get_admin_order_detail(pool, id).await?))
}

pub async fn list_products_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminProductsQuery>,
) -> Result<Json<AdminProductList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_products(
            pool,
            query.q.as_deref(),
            query.status.as_deref(),
            query.store_id,
            query.source.as_deref(),
            query.visibility.as_deref(),
            query.stock_state.as_deref(),
            query.has_images.as_deref(),
            query.sort.as_deref(),
            query.page,
            query.limit,
        )
        .await?,
    ))
}

pub async fn get_product_detail_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminProductDetail>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(get_admin_product_detail(pool, id).await?))
}

pub async fn takedown_product_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<TakedownProductInput>>,
) -> Result<Json<Product>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let note = body.and_then(|Json(input)| input.note);
    Ok(Json(
        set_product_takedown(pool, admin.id, id, true, note.as_deref()).await?,
    ))
}

pub async fn restore_product_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<RestoreProductInput>>,
) -> Result<Json<Product>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let note = body.and_then(|Json(input)| input.note);
    Ok(Json(
        set_product_takedown(pool, admin.id, id, false, note.as_deref()).await?,
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

// ---------------------------------------------------------------------------
// Admin shipments (admin shipments remake)
// ---------------------------------------------------------------------------
//
// The cross-store, cross-courier oversight queue the
// overview's Needs-attention block links to. Shipments
// were seller-facing only before this remake: no admin
// list, no admin detail, and a dead `/admin/shipments`
// link on the overview since its own remake.
//
// Two different "stuck" facts are kept apart throughout,
// because they have different owners:
//
// - UNREFRESHED / POLL OVERDUE are platform facts. A
//   shipment is unrefreshed when its status has not
//   changed in [`STALE_SHIPMENT_MINUTES`] — the exact
//   population `/v1/admin/metrics` reports as
//   `stale_shipments`, pinned equal in tests. It is
//   poll-overdue when no courier poll has been ATTEMPTED
//   in that window (`last_polled_at`, migration 0079):
//   a courier legitimately reporting the same status for
//   a day is unrefreshed but not poll-overdue; a dead
//   worker is both.
// - STATUS AGED is a courier/seller fact: the shipment
//   has sat in one non-terminal status longer than that
//   status's threshold (platform settings
//   `shipment_aged_<status>_hours`, domain defaults
//   otherwise). Manual shipments age too — a seller who
//   never types the next status is exactly this queue.
//
// Money shown here is the STORED `cod_amount_poisha`
// snapshot from booking — the courier's collection
// figure — never recomputed. "Delivered COD held" is an
// upper bound, not a ledger: no remittance tracking
// exists anywhere in the platform, so nothing here
// claims a courier has or has not settled.

/// The non-terminal unified statuses in canonical order —
/// the order the aged thresholds bind in. The domain aged
/// maps key off exactly these seven; the terminal three
/// (`delivered`, `returned`, `cancelled`) never age.
const SHIPMENT_NON_TERMINAL: [&str; 7] = [
    "created",
    "picked_up",
    "in_transit",
    "out_for_delivery",
    "partial_delivered",
    "return_in_transit",
    "unknown",
];

/// The effective per-status aged thresholds, echoed on
/// the list response so the page can say which bar each
/// flag was measured against.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ShipmentAgedThresholds {
    pub created: i64,
    pub picked_up: i64,
    pub in_transit: i64,
    pub out_for_delivery: i64,
    pub partial_delivered: i64,
    pub return_in_transit: i64,
    pub unknown: i64,
}

impl ShipmentAgedThresholds {
    /// The threshold for one status, or `None` for a
    /// terminal (or unknown) status.
    fn hours_for(&self, status: &str) -> Option<i64> {
        match status {
            "created" => Some(self.created),
            "picked_up" => Some(self.picked_up),
            "in_transit" => Some(self.in_transit),
            "out_for_delivery" => Some(self.out_for_delivery),
            "partial_delivered" => Some(self.partial_delivered),
            "return_in_transit" => Some(self.return_in_transit),
            "unknown" => Some(self.unknown),
            _ => None,
        }
    }

    /// The seven thresholds in [`SHIPMENT_NON_TERMINAL`]
    /// order, for binding into the aged-queue predicates.
    fn ordered(&self) -> [i64; 7] {
        let mut out = [0; 7];
        for (index, status) in SHIPMENT_NON_TERMINAL.iter().enumerate() {
            out[index] = self
                .hours_for(status)
                .expect("non-terminal status has a threshold");
        }
        out
    }
}

/// Resolve one status's aged threshold: the stored
/// setting when it holds a valid value (gated by the
/// fulfillment-SLA rule, 1..=720), else the domain
/// default. Two layers — a stored setting and the
/// constant; deliberately no env layer (the
/// delivery-estimates precedent).
async fn resolve_shipment_aged_hours(pool: &PgPool, status: &str) -> i64 {
    let key = shipment_aged_setting_key(status).expect("non-terminal status has an aged key");
    let default =
        default_shipment_aged_hours(status).expect("non-terminal status has an aged default");
    match platform_setting_i64(pool, key).await {
        Some(value) if validate_fulfillment_sla_hours(value) => value,
        _ => default,
    }
}

/// The whole effective threshold set (see
/// [`resolve_shipment_aged_hours`]).
pub async fn effective_shipment_aged_thresholds(pool: &PgPool) -> ShipmentAgedThresholds {
    ShipmentAgedThresholds {
        created: resolve_shipment_aged_hours(pool, "created").await,
        picked_up: resolve_shipment_aged_hours(pool, "picked_up").await,
        in_transit: resolve_shipment_aged_hours(pool, "in_transit").await,
        out_for_delivery: resolve_shipment_aged_hours(pool, "out_for_delivery").await,
        partial_delivered: resolve_shipment_aged_hours(pool, "partial_delivered").await,
        return_in_transit: resolve_shipment_aged_hours(pool, "return_in_transit").await,
        unknown: resolve_shipment_aged_hours(pool, "unknown").await,
    }
}

/// Is a shipment in `status`, last changed at `since`,
/// aged at `now` under `thresholds`? Terminal statuses
/// are never aged. Shared by the list rows and the
/// detail so the two never disagree.
fn shipment_is_aged(status: &str, since: DateTime<Utc>, thresholds: &ShipmentAgedThresholds, now: DateTime<Utc>) -> bool {
    if POLL_TERMINAL_STATUSES.contains(&status) {
        return false;
    }
    match thresholds.hours_for(status) {
        Some(hours) => since < now - chrono::Duration::hours(hours),
        None => false,
    }
}

/// Is a non-manual, non-terminal shipment unrefreshed at
/// `now` — the metrics `stale_shipments` predicate,
/// restated per row.
fn shipment_is_unrefreshed(
    courier: &str,
    status: &str,
    updated_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    courier != "manual"
        && !POLL_TERMINAL_STATUSES.contains(&status)
        && updated_at < now - chrono::Duration::minutes(STALE_SHIPMENT_MINUTES)
}

/// Is a non-manual, non-terminal shipment poll-overdue at
/// `now`: no adapter attempt recorded within the stale
/// window. A never-polled shipment's clock starts at its
/// booking (`created_at`) — the poller's debt starts
/// there too.
fn shipment_is_poll_overdue(
    courier: &str,
    status: &str,
    last_polled_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    courier != "manual"
        && !POLL_TERMINAL_STATUSES.contains(&status)
        && last_polled_at.unwrap_or(created_at)
            < now - chrono::Duration::minutes(STALE_SHIPMENT_MINUTES)
}

/// Private row for the admin shipment list: carries the
/// recipient's raw delivery phone, which must never be
/// serialised — the public view is built by
/// [`AdminShipmentRowRow::into_view`], which masks it
/// (the orders remake's row pattern).
#[derive(FromRow)]
struct AdminShipmentRowRow {
    id: Uuid,
    sub_order_id: Uuid,
    order_id: Uuid,
    store_id: Uuid,
    store_name: String,
    store_slug: String,
    courier: String,
    manual_courier_name: Option<String>,
    consignment_id: String,
    tracking_code: Option<String>,
    manual_tracking_id: Option<String>,
    status: String,
    sub_order_status: String,
    cod_amount_poisha: i64,
    delivery_name: String,
    delivery_phone: String,
    delivery_zone: String,
    delivery_district: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    last_polled_at: Option<DateTime<Utc>>,
    status_since: DateTime<Utc>,
    has_return_request: bool,
}

/// One row of the admin shipment queue. Recipient PII is
/// minimised for a scanning surface: the delivery NAME
/// (triage recognition, as on the orders queue) and a
/// MASKED phone; the address, the order note and the
/// buyer's account phone never appear here — the detail
/// carries the full delivery block.
#[derive(Debug, Serialize)]
pub struct AdminShipment {
    pub id: Uuid,
    pub sub_order_id: Uuid,
    pub order_id: Uuid,
    pub store_id: Uuid,
    pub store_name: String,
    pub store_slug: String,
    pub courier: String,
    pub manual_courier_name: Option<String>,
    pub consignment_id: String,
    pub tracking_code: Option<String>,
    pub manual_tracking_id: Option<String>,
    pub status: String,
    pub sub_order_status: String,
    pub cod_amount_poisha: i64,
    pub delivery_name: String,
    pub delivery_phone_masked: String,
    pub delivery_zone: String,
    pub delivery_district: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_polled_at: Option<DateTime<Utc>>,
    /// When the shipment entered its CURRENT status (the
    /// newest event in that status; the booking time when
    /// no such event exists).
    pub status_since: DateTime<Utc>,
    pub status_age_secs: i64,
    /// In one non-terminal status past its threshold.
    pub aged: bool,
    /// No poll attempted within the stale window.
    pub poll_overdue: bool,
    /// No status change within the stale window — the
    /// metrics `stale_shipments` population.
    pub unrefreshed: bool,
    pub has_return_request: bool,
}

impl AdminShipmentRowRow {
    fn into_view(self, thresholds: &ShipmentAgedThresholds, now: DateTime<Utc>) -> AdminShipment {
        let aged = shipment_is_aged(&self.status, self.status_since, thresholds, now);
        let unrefreshed =
            shipment_is_unrefreshed(&self.courier, &self.status, self.updated_at, now);
        let poll_overdue = shipment_is_poll_overdue(
            &self.courier,
            &self.status,
            self.last_polled_at,
            self.created_at,
            now,
        );
        AdminShipment {
            delivery_phone_masked: mask_phone(&self.delivery_phone),
            status_age_secs: (now - self.status_since).num_seconds().max(0),
            aged,
            poll_overdue,
            unrefreshed,
            id: self.id,
            sub_order_id: self.sub_order_id,
            order_id: self.order_id,
            store_id: self.store_id,
            store_name: self.store_name,
            store_slug: self.store_slug,
            courier: self.courier,
            manual_courier_name: self.manual_courier_name,
            consignment_id: self.consignment_id,
            tracking_code: self.tracking_code,
            manual_tracking_id: self.manual_tracking_id,
            status: self.status,
            sub_order_status: self.sub_order_status,
            cod_amount_poisha: self.cod_amount_poisha,
            delivery_name: self.delivery_name,
            delivery_zone: self.delivery_zone,
            delivery_district: self.delivery_district,
            created_at: self.created_at,
            updated_at: self.updated_at,
            last_polled_at: self.last_polled_at,
            status_since: self.status_since,
            has_return_request: self.has_return_request,
        }
    }
}

/// Per-status counts over the WHOLE queue (the summary is
/// never filter-shaped).
#[derive(Debug, Serialize)]
pub struct AdminShipmentStatusCounts {
    pub created: i64,
    pub picked_up: i64,
    pub in_transit: i64,
    pub out_for_delivery: i64,
    pub delivered: i64,
    pub partial_delivered: i64,
    pub return_in_transit: i64,
    pub returned: i64,
    pub cancelled: i64,
    pub unknown: i64,
}

/// Shipments whose parcel came back (courier RTO) on an
/// order where the buyer had already paid money in —
/// an advance or a full prepayment — with NO return
/// request on file: the population with no refund path
/// anywhere in the platform today (surfaced read-only;
/// creating the obligation is a returns/payments
/// decision). `advance_paid_poisha` sums DISTINCT
/// orders — one order's advance is counted once however
/// many of its parcels came back.
#[derive(Debug, Serialize)]
pub struct AdminShipmentAdvanceStranded {
    pub shipments: i64,
    pub orders: i64,
    pub advance_paid_poisha: i64,
}

/// One courier's slice of the platform: shipment counts
/// by status class (the classes partition the unified
/// status set, so they sum to `total`), its COD figures
/// and its oldest live parcel. Built by merging the SQL
/// rollup onto the availability list, so a courier with
/// zero shipments still appears — a missing courier is
/// itself a health fact.
#[derive(Debug, Serialize)]
pub struct AdminShipmentCourierRollup {
    /// The shipments-table courier value: an API courier
    /// key, or `manual` for the manual-counter rows.
    pub courier: String,
    /// The manual courier's canonical display name;
    /// `None` for API couriers (the web maps brand names
    /// for the keys).
    pub display_name: Option<String>,
    pub available: bool,
    /// `live` / `dev` / `manual`, as the availability
    /// service reports it.
    pub mode: Option<String>,
    pub reason: Option<String>,
    pub is_default: bool,
    pub total: i64,
    pub created: i64,
    pub in_flight: i64,
    pub delivered: i64,
    pub partial_delivered: i64,
    pub return_in_transit: i64,
    pub returned: i64,
    pub cancelled: i64,
    pub unknown: i64,
    pub non_terminal: i64,
    pub cod_delivered_held_poisha: i64,
    pub cod_in_flight_expected_poisha: i64,
    /// Rows carrying a DEV-simulator consignment id —
    /// row-level evidence of simulated bookings, whatever
    /// the current config says.
    pub dev_shipment_count: i64,
    pub oldest_non_terminal_at: Option<DateTime<Utc>>,
}

/// The whole-queue summary riding the list response.
#[derive(Debug, Serialize)]
pub struct AdminShipmentSummary {
    pub total: i64,
    pub non_terminal: i64,
    pub by_status: AdminShipmentStatusCounts,
    /// The metrics `stale_shipments` population, by
    /// construction (pinned equal in tests).
    pub unrefreshed: i64,
    pub poll_overdue: i64,
    pub status_aged: i64,
    /// Stored COD on `delivered` shipments: cash the
    /// couriers are presumed to hold, UNRECONCILED — an
    /// upper bound, not a ledger (no remittance tracking
    /// exists).
    pub cod_delivered_held_poisha: i64,
    /// Of the held figure, the part delivered more than
    /// 7 / 30 days ago (ageing measured on `updated_at`,
    /// the moment delivery was recorded).
    pub cod_delivered_over_7d_poisha: i64,
    pub cod_delivered_over_30d_poisha: i64,
    /// Stored COD on parcels in a courier's hands
    /// (picked up / in transit / out for delivery): what
    /// they are expected to collect.
    pub cod_in_flight_expected_poisha: i64,
    /// Stored COD on booked-but-uncollected parcels.
    pub cod_created_poisha: i64,
    /// Stored COD on partially delivered parcels: an
    /// UPPER BOUND (the platform records no
    /// collected-so-far amount).
    pub cod_partial_poisha: i64,
    /// Stored COD on `unknown` shipments: money attached
    /// to parcels whose state the platform cannot
    /// classify — a blind spot, shown rather than hidden.
    pub cod_unknown_poisha: i64,
    pub advance_stranded: AdminShipmentAdvanceStranded,
    /// True when the DEV courier simulator is the active
    /// mode: API-courier bookings and polls are simulated
    /// (manual bookings are real in every mode).
    pub couriers_dev_mode: bool,
    pub couriers: Vec<AdminShipmentCourierRollup>,
    pub aged_thresholds: ShipmentAgedThresholds,
}

/// The list envelope: `total` is the FILTERED count;
/// `summary` describes the whole queue.
#[derive(Debug, Serialize)]
pub struct AdminShipmentList {
    pub items: Vec<AdminShipment>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
    pub summary: AdminShipmentSummary,
}

/// The summary's main aggregate: one FILTER pass over the
/// whole shipments table (see [`shipment_summary`]).
#[derive(FromRow)]
struct ShipmentSummaryRow {
    total: i64,
    st_created: i64,
    st_picked_up: i64,
    st_in_transit: i64,
    st_out_for_delivery: i64,
    st_delivered: i64,
    st_partial_delivered: i64,
    st_return_in_transit: i64,
    st_returned: i64,
    st_cancelled: i64,
    st_unknown: i64,
    non_terminal: i64,
    unrefreshed: i64,
    poll_overdue: i64,
    status_aged: i64,
    cod_delivered_held: i64,
    cod_delivered_over_7d: i64,
    cod_delivered_over_30d: i64,
    cod_in_flight: i64,
    cod_created: i64,
    cod_partial: i64,
    cod_unknown: i64,
}

/// The stranded-advance aggregate (one row of three
/// subqueries; see [`AdminShipmentAdvanceStranded`]).
#[derive(FromRow)]
struct ShipmentStrandedRow {
    shipments: i64,
    orders: i64,
    advance_paid_poisha: i64,
}

/// One `(courier, manual_courier_name)` group of the
/// per-courier rollup.
#[derive(FromRow)]
struct ShipmentCourierGroupRow {
    courier: String,
    manual_courier_name: Option<String>,
    total: i64,
    cls_created: i64,
    cls_in_flight: i64,
    cls_delivered: i64,
    cls_partial: i64,
    cls_return_in_transit: i64,
    cls_returned: i64,
    cls_cancelled: i64,
    cls_unknown: i64,
    non_terminal: i64,
    cod_delivered_held: i64,
    cod_in_flight: i64,
    dev_count: i64,
    oldest_non_terminal_at: Option<DateTime<Utc>>,
}

fn empty_shipment_rollup(
    courier: String,
    display_name: Option<String>,
    availability: Option<&CourierAvailability>,
) -> AdminShipmentCourierRollup {
    AdminShipmentCourierRollup {
        courier,
        display_name,
        available: availability.is_some_and(|entry| entry.available),
        mode: availability.and_then(|entry| entry.mode.clone()),
        reason: availability.and_then(|entry| entry.reason.clone()),
        is_default: availability.is_some_and(|entry| entry.is_default),
        total: 0,
        created: 0,
        in_flight: 0,
        delivered: 0,
        partial_delivered: 0,
        return_in_transit: 0,
        returned: 0,
        cancelled: 0,
        unknown: 0,
        non_terminal: 0,
        cod_delivered_held_poisha: 0,
        cod_in_flight_expected_poisha: 0,
        dev_shipment_count: 0,
        oldest_non_terminal_at: None,
    }
}

fn fill_shipment_rollup(entry: &mut AdminShipmentCourierRollup, row: &ShipmentCourierGroupRow) {
    entry.total = row.total;
    entry.created = row.cls_created;
    entry.in_flight = row.cls_in_flight;
    entry.delivered = row.cls_delivered;
    entry.partial_delivered = row.cls_partial;
    entry.return_in_transit = row.cls_return_in_transit;
    entry.returned = row.cls_returned;
    entry.cancelled = row.cls_cancelled;
    entry.unknown = row.cls_unknown;
    entry.non_terminal = row.non_terminal;
    entry.cod_delivered_held_poisha = row.cod_delivered_held;
    entry.cod_in_flight_expected_poisha = row.cod_in_flight;
    entry.dev_shipment_count = row.dev_count;
    entry.oldest_non_terminal_at = row.oldest_non_terminal_at;
}

/// The whole-queue shipment summary. Takes only the pool
/// for DATA — `config` and `thresholds` are platform
/// configuration, never list filters, so the summary
/// structurally cannot become filter-shaped (the
/// payouts/orders canon).
async fn shipment_summary(
    pool: &PgPool,
    config: &CouriersConfig,
    thresholds: &ShipmentAgedThresholds,
) -> Result<AdminShipmentSummary, ApiError> {
    let t = thresholds.ordered();
    let row: ShipmentSummaryRow = sqlx::query_as(
        "SELECT COUNT(*) AS total, \
           COUNT(*) FILTER (WHERE status = 'created') AS st_created, \
           COUNT(*) FILTER (WHERE status = 'picked_up') AS st_picked_up, \
           COUNT(*) FILTER (WHERE status = 'in_transit') AS st_in_transit, \
           COUNT(*) FILTER (WHERE status = 'out_for_delivery') AS st_out_for_delivery, \
           COUNT(*) FILTER (WHERE status = 'delivered') AS st_delivered, \
           COUNT(*) FILTER (WHERE status = 'partial_delivered') AS st_partial_delivered, \
           COUNT(*) FILTER (WHERE status = 'return_in_transit') AS st_return_in_transit, \
           COUNT(*) FILTER (WHERE status = 'returned') AS st_returned, \
           COUNT(*) FILTER (WHERE status = 'cancelled') AS st_cancelled, \
           COUNT(*) FILTER (WHERE status = 'unknown') AS st_unknown, \
           COUNT(*) FILTER (WHERE status NOT IN ('delivered', 'returned', 'cancelled')) \
             AS non_terminal, \
           COUNT(*) FILTER (WHERE courier <> 'manual' \
             AND status NOT IN ('delivered', 'returned', 'cancelled') \
             AND updated_at < now() - make_interval(mins => $1::int)) AS unrefreshed, \
           COUNT(*) FILTER (WHERE courier <> 'manual' \
             AND status NOT IN ('delivered', 'returned', 'cancelled') \
             AND COALESCE(last_polled_at, created_at) \
               < now() - make_interval(mins => $1::int)) AS poll_overdue, \
           COUNT(*) FILTER (WHERE status NOT IN ('delivered', 'returned', 'cancelled') \
             AND updated_at < now() - (CASE status \
               WHEN 'created' THEN make_interval(hours => $2::int) \
               WHEN 'picked_up' THEN make_interval(hours => $3::int) \
               WHEN 'in_transit' THEN make_interval(hours => $4::int) \
               WHEN 'out_for_delivery' THEN make_interval(hours => $5::int) \
               WHEN 'partial_delivered' THEN make_interval(hours => $6::int) \
               WHEN 'return_in_transit' THEN make_interval(hours => $7::int) \
               WHEN 'unknown' THEN make_interval(hours => $8::int) END)) AS status_aged, \
           COALESCE(SUM(cod_amount_poisha) FILTER (WHERE status = 'delivered'), 0)::bigint \
             AS cod_delivered_held, \
           COALESCE(SUM(cod_amount_poisha) FILTER (WHERE status = 'delivered' \
             AND updated_at < now() - interval '7 days'), 0)::bigint \
             AS cod_delivered_over_7d, \
           COALESCE(SUM(cod_amount_poisha) FILTER (WHERE status = 'delivered' \
             AND updated_at < now() - interval '30 days'), 0)::bigint \
             AS cod_delivered_over_30d, \
           COALESCE(SUM(cod_amount_poisha) FILTER \
             (WHERE status IN ('picked_up', 'in_transit', 'out_for_delivery')), 0)::bigint \
             AS cod_in_flight, \
           COALESCE(SUM(cod_amount_poisha) FILTER (WHERE status = 'created'), 0)::bigint \
             AS cod_created, \
           COALESCE(SUM(cod_amount_poisha) FILTER (WHERE status = 'partial_delivered'), 0)::bigint \
             AS cod_partial, \
           COALESCE(SUM(cod_amount_poisha) FILTER (WHERE status = 'unknown'), 0)::bigint \
             AS cod_unknown \
         FROM shipments",
    )
    .bind(STALE_SHIPMENT_MINUTES)
    .bind(t[0])
    .bind(t[1])
    .bind(t[2])
    .bind(t[3])
    .bind(t[4])
    .bind(t[5])
    .bind(t[6])
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("summarise admin shipments", e))?;

    let stranded: ShipmentStrandedRow = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM shipments sh \
              JOIN sub_orders so ON so.id = sh.sub_order_id \
              JOIN orders o ON o.id = so.order_id \
            WHERE sh.status = 'returned' AND o.advance_paid_poisha > 0 \
              AND NOT EXISTS (SELECT 1 FROM return_requests rr \
                              WHERE rr.sub_order_id = so.id)) AS shipments, \
           (SELECT COUNT(*) FROM orders o \
            WHERE o.advance_paid_poisha > 0 AND EXISTS \
              (SELECT 1 FROM shipments sh \
                 JOIN sub_orders so ON so.id = sh.sub_order_id \
               WHERE so.order_id = o.id AND sh.status = 'returned' \
                 AND NOT EXISTS (SELECT 1 FROM return_requests rr \
                                 WHERE rr.sub_order_id = so.id))) AS orders, \
           (SELECT COALESCE(SUM(o.advance_paid_poisha), 0)::bigint FROM orders o \
            WHERE o.advance_paid_poisha > 0 AND EXISTS \
              (SELECT 1 FROM shipments sh \
                 JOIN sub_orders so ON so.id = sh.sub_order_id \
               WHERE so.order_id = o.id AND sh.status = 'returned' \
                 AND NOT EXISTS (SELECT 1 FROM return_requests rr \
                                 WHERE rr.sub_order_id = so.id))) AS advance_paid_poisha",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("summarise stranded advances", e))?;

    let groups: Vec<ShipmentCourierGroupRow> = sqlx::query_as(
        "SELECT courier, manual_courier_name, \
           COUNT(*) AS total, \
           COUNT(*) FILTER (WHERE status = 'created') AS cls_created, \
           COUNT(*) FILTER (WHERE status IN ('picked_up', 'in_transit', 'out_for_delivery')) \
             AS cls_in_flight, \
           COUNT(*) FILTER (WHERE status = 'delivered') AS cls_delivered, \
           COUNT(*) FILTER (WHERE status = 'partial_delivered') AS cls_partial, \
           COUNT(*) FILTER (WHERE status = 'return_in_transit') AS cls_return_in_transit, \
           COUNT(*) FILTER (WHERE status = 'returned') AS cls_returned, \
           COUNT(*) FILTER (WHERE status = 'cancelled') AS cls_cancelled, \
           COUNT(*) FILTER (WHERE status = 'unknown') AS cls_unknown, \
           COUNT(*) FILTER (WHERE status NOT IN ('delivered', 'returned', 'cancelled')) \
             AS non_terminal, \
           COALESCE(SUM(cod_amount_poisha) FILTER (WHERE status = 'delivered'), 0)::bigint \
             AS cod_delivered_held, \
           COALESCE(SUM(cod_amount_poisha) FILTER \
             (WHERE status IN ('picked_up', 'in_transit', 'out_for_delivery')), 0)::bigint \
             AS cod_in_flight, \
           COUNT(*) FILTER (WHERE consignment_id LIKE 'DEV-%') AS dev_count, \
           MIN(created_at) FILTER (WHERE status NOT IN ('delivered', 'returned', 'cancelled')) \
             AS oldest_non_terminal_at \
         FROM shipments GROUP BY courier, manual_courier_name",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("roll up admin shipments by courier", e))?;

    // Merge the SQL groups onto the availability list:
    // every configured courier appears, zero-volume ones
    // included; a group with no availability entry (a
    // manual row without a recorded name, say) is appended
    // rather than dropped, so counts never vanish.
    let mut couriers: Vec<AdminShipmentCourierRollup> = Vec::new();
    for entry in courier_availability(config) {
        couriers.push(empty_shipment_rollup(
            entry.courier.clone(),
            entry.display_name.clone(),
            Some(&entry),
        ));
    }
    for entry in manual_courier_availability() {
        couriers.push(empty_shipment_rollup(
            "manual".to_string(),
            entry.display_name.clone(),
            Some(&entry),
        ));
    }
    for group in &groups {
        let position = if group.courier == "manual" {
            couriers.iter().position(|entry| {
                entry.courier == "manual" && entry.display_name == group.manual_courier_name
            })
        } else {
            couriers
                .iter()
                .position(|entry| entry.courier == group.courier)
        };
        match position {
            Some(index) => fill_shipment_rollup(&mut couriers[index], group),
            None => {
                let mut entry = empty_shipment_rollup(
                    group.courier.clone(),
                    group.manual_courier_name.clone(),
                    None,
                );
                fill_shipment_rollup(&mut entry, group);
                couriers.push(entry);
            }
        }
    }

    Ok(AdminShipmentSummary {
        total: row.total,
        non_terminal: row.non_terminal,
        by_status: AdminShipmentStatusCounts {
            created: row.st_created,
            picked_up: row.st_picked_up,
            in_transit: row.st_in_transit,
            out_for_delivery: row.st_out_for_delivery,
            delivered: row.st_delivered,
            partial_delivered: row.st_partial_delivered,
            return_in_transit: row.st_return_in_transit,
            returned: row.st_returned,
            cancelled: row.st_cancelled,
            unknown: row.st_unknown,
        },
        unrefreshed: row.unrefreshed,
        poll_overdue: row.poll_overdue,
        status_aged: row.status_aged,
        cod_delivered_held_poisha: row.cod_delivered_held,
        cod_delivered_over_7d_poisha: row.cod_delivered_over_7d,
        cod_delivered_over_30d_poisha: row.cod_delivered_over_30d,
        cod_in_flight_expected_poisha: row.cod_in_flight,
        cod_created_poisha: row.cod_created,
        cod_partial_poisha: row.cod_partial,
        cod_unknown_poisha: row.cod_unknown,
        advance_stranded: AdminShipmentAdvanceStranded {
            shipments: stranded.shipments,
            orders: stranded.orders,
            advance_paid_poisha: stranded.advance_paid_poisha,
        },
        couriers_dev_mode: config.dev_mode,
        couriers,
        aged_thresholds: *thresholds,
    })
}

/// The filter predicates shared by the count and the page
/// query (they must never drift — the stores remake's
/// count/items lesson). Parameters, in order: status,
/// courier, store_id, queue, stale minutes, the seven
/// aged thresholds in [`SHIPMENT_NON_TERMINAL`] order,
/// canonical phone, raw search text, id prefix pattern,
/// text pattern.
const ADMIN_SHIPMENT_FILTERS: &str = "($1::text IS NULL OR sh.status = $1) \
    AND ($2::text IS NULL OR sh.courier = $2) \
    AND ($3::uuid IS NULL OR so.store_id = $3) \
    AND ($4::text IS NULL \
          OR ($4 = 'unrefreshed' AND sh.courier <> 'manual' \
              AND sh.status NOT IN ('delivered', 'returned', 'cancelled') \
              AND sh.updated_at < now() - make_interval(mins => $5::int)) \
          OR ($4 = 'poll_overdue' AND sh.courier <> 'manual' \
              AND sh.status NOT IN ('delivered', 'returned', 'cancelled') \
              AND COALESCE(sh.last_polled_at, sh.created_at) \
                < now() - make_interval(mins => $5::int)) \
          OR ($4 = 'status_aged' \
              AND sh.status NOT IN ('delivered', 'returned', 'cancelled') \
              AND sh.updated_at < now() - (CASE sh.status \
                    WHEN 'created' THEN make_interval(hours => $6::int) \
                    WHEN 'picked_up' THEN make_interval(hours => $7::int) \
                    WHEN 'in_transit' THEN make_interval(hours => $8::int) \
                    WHEN 'out_for_delivery' THEN make_interval(hours => $9::int) \
                    WHEN 'partial_delivered' THEN make_interval(hours => $10::int) \
                    WHEN 'return_in_transit' THEN make_interval(hours => $11::int) \
                    WHEN 'unknown' THEN make_interval(hours => $12::int) END))) \
    AND ($14::text IS NULL OR o.delivery_phone = $13 OR u.phone = $13 \
          OR o.delivery_phone = $14 \
          OR sh.id::text LIKE $15 OR so.id::text LIKE $15 OR o.id::text LIKE $15 \
          OR sh.consignment_id ILIKE $16 OR sh.tracking_code ILIKE $16 \
          OR sh.manual_tracking_id ILIKE $16 \
          OR o.delivery_name ILIKE $16 OR st.name ILIKE $16 OR st.slug ILIKE $16 \
          OR sh.manual_courier_name ILIKE $16)";

fn shipment_order_by(sort: ShipmentSort) -> &'static str {
    match sort {
        ShipmentSort::Newest => "ORDER BY sh.created_at DESC, sh.id DESC",
        ShipmentSort::Oldest => "ORDER BY sh.created_at ASC, sh.id ASC",
        ShipmentSort::StaleFirst => "ORDER BY sh.updated_at ASC, sh.id ASC",
        ShipmentSort::CodDesc => {
            "ORDER BY sh.cod_amount_poisha DESC, sh.created_at DESC, sh.id DESC"
        }
    }
}

/// List shipments for the admin oversight queue (admin
/// shipments remake): a purpose-built masked row (see
/// [`AdminShipment`]) assembled set-wise, plus the
/// whole-queue summary.
///
/// Filters, all validated fail-closed (an unknown value
/// is a 400, never a silently empty page): `status` (the
/// ten unified statuses), `courier` (the five API keys
/// or `manual`), `store_id`, and `queue` — `unrefreshed`
/// (the metrics stale population), `poll_overdue` (no
/// poll attempted in the stale window) or `status_aged`
/// (in one status past its threshold); a chosen queue
/// defaults the sort to stale-first. `q` searches,
/// following the platform's phone discipline: a BD phone
/// in any typed form (Bengali digits transliterated
/// first) matches the delivery phone or the buyer's
/// account phone EXACTLY — phones are never
/// substring-searched; a shipment / sub-order / order id
/// prefix (4+ hex/dash characters) matches by prefix;
/// any other text matches the courier references
/// (consignment, tracking code, manual receipt number —
/// operational strings printed on labels, substring-safe
/// unlike phones), the delivery name, the store's
/// name/slug or the manual courier's name, all
/// LIKE-escaped substrings.
#[allow(clippy::too_many_arguments)]
pub async fn list_shipments(
    pool: &PgPool,
    config: &CouriersConfig,
    q: Option<&str>,
    status: Option<&str>,
    courier: Option<&str>,
    store_id: Option<Uuid>,
    queue: Option<&str>,
    sort: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminShipmentList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let status: Option<&str> = match status.map(str::trim) {
        None | Some("") => None,
        Some(s @ ("created" | "picked_up" | "in_transit" | "out_for_delivery" | "delivered"
        | "partial_delivered" | "return_in_transit" | "returned" | "cancelled" | "unknown")) => {
            Some(s)
        }
        Some(_) => {
            return Err(ApiError::bad_request(
                "status must be one of: created, picked_up, in_transit, out_for_delivery, \
                 delivered, partial_delivered, return_in_transit, returned, cancelled, unknown",
            ))
        }
    };
    let courier: Option<&str> = match courier.map(str::trim) {
        None | Some("") => None,
        Some(s @ ("steadfast" | "pathao" | "redx" | "ecourier" | "paperfly" | "manual")) => {
            Some(s)
        }
        Some(_) => {
            return Err(ApiError::bad_request(
                "courier must be one of: steadfast, pathao, redx, ecourier, paperfly, manual",
            ))
        }
    };
    let queue: Option<&str> = match queue.map(str::trim) {
        None | Some("") => None,
        Some(s @ ("unrefreshed" | "poll_overdue" | "status_aged")) => Some(s),
        Some(_) => {
            return Err(ApiError::bad_request(
                "queue must be one of: unrefreshed, poll_overdue, status_aged",
            ))
        }
    };
    // A chosen queue is a work list: default to the
    // poller's own order (stalest first) unless the
    // caller picked a sort explicitly.
    let sort = match (queue.is_some(), sort) {
        (true, None) => ShipmentSort::StaleFirst,
        _ => validate_shipment_list_sort(sort).map_err(ApiError::bad_request)?,
    };

    let q_trim: Option<String> = q
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(bn_to_en_digits);
    let phone_exact: Option<String> = q_trim.as_deref().and_then(normalize_bd_phone);
    let id_prefix: Option<String> = q_trim.as_deref().and_then(|text| {
        let lowered = text.to_lowercase();
        if lowered.len() >= 4 && lowered.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            // Hex digits and dashes carry no LIKE wildcards,
            // so the prefix pattern needs no escaping.
            Some(format!("{lowered}%"))
        } else {
            None
        }
    });
    let text_pattern: Option<String> = q_trim.as_deref().map(search_like_pattern);

    let thresholds = effective_shipment_aged_thresholds(pool).await;
    let t = thresholds.ordered();

    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM shipments sh \
         JOIN sub_orders so ON so.id = sh.sub_order_id \
         JOIN orders o ON o.id = so.order_id \
         JOIN stores st ON st.id = so.store_id \
         JOIN users u ON u.id = o.buyer_user_id \
         WHERE {ADMIN_SHIPMENT_FILTERS}"
    ))
    .bind(status)
    .bind(courier)
    .bind(store_id)
    .bind(queue)
    .bind(STALE_SHIPMENT_MINUTES)
    .bind(t[0])
    .bind(t[1])
    .bind(t[2])
    .bind(t[3])
    .bind(t[4])
    .bind(t[5])
    .bind(t[6])
    .bind(&phone_exact)
    .bind(&q_trim)
    .bind(&id_prefix)
    .bind(&text_pattern)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin shipments", e))?;

    let rows: Vec<AdminShipmentRowRow> = sqlx::query_as(&format!(
        "SELECT sh.id, sh.sub_order_id, so.order_id, so.store_id, \
           st.name AS store_name, st.slug AS store_slug, \
           sh.courier, sh.manual_courier_name, sh.consignment_id, \
           sh.tracking_code, sh.manual_tracking_id, sh.status, \
           so.status AS sub_order_status, sh.cod_amount_poisha, \
           o.delivery_name, o.delivery_phone, o.delivery_zone, \
           o.delivery_district, sh.created_at, sh.updated_at, \
           sh.last_polled_at, \
           COALESCE((SELECT MAX(e.created_at) FROM shipment_events e \
                     WHERE e.shipment_id = sh.id AND e.status = sh.status), \
                    sh.created_at) AS status_since, \
           EXISTS (SELECT 1 FROM return_requests rr \
                   WHERE rr.sub_order_id = so.id) AS has_return_request \
         FROM shipments sh \
         JOIN sub_orders so ON so.id = sh.sub_order_id \
         JOIN orders o ON o.id = so.order_id \
         JOIN stores st ON st.id = so.store_id \
         JOIN users u ON u.id = o.buyer_user_id \
         WHERE {ADMIN_SHIPMENT_FILTERS} \
         {} LIMIT $17 OFFSET $18",
        shipment_order_by(sort)
    ))
    .bind(status)
    .bind(courier)
    .bind(store_id)
    .bind(queue)
    .bind(STALE_SHIPMENT_MINUTES)
    .bind(t[0])
    .bind(t[1])
    .bind(t[2])
    .bind(t[3])
    .bind(t[4])
    .bind(t[5])
    .bind(t[6])
    .bind(&phone_exact)
    .bind(&q_trim)
    .bind(&id_prefix)
    .bind(&text_pattern)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin shipments", e))?;

    let now = Utc::now();
    let items: Vec<AdminShipment> = rows
        .into_iter()
        .map(|row| row.into_view(&thresholds, now))
        .collect();
    let summary = shipment_summary(pool, config, &thresholds).await?;
    Ok(AdminShipmentList {
        items,
        total,
        page,
        limit,
        summary,
    })
}

/// One purchased line on the detail's sub-order.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminShipmentItem {
    pub title_snapshot: String,
    pub variant_name: Option<String>,
    pub quantity: i32,
    pub unit_price_poisha: i64,
}

/// One raw `shipment_events` row on the detail. Unlike
/// the folded timeline, this keeps `source` (who moved
/// it: the booking, a poll, the seller's manual entry)
/// and the courier's verbatim `raw_status` — the two
/// facts a courier escalation turns on.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminShipmentEvent {
    pub id: Uuid,
    pub status: String,
    pub raw_status: Option<String>,
    pub source: String,
    pub at: DateTime<Utc>,
}

/// The return request filed against this sub-order, if
/// any — a cross-reference only. Refund handling belongs
/// to the returns surface; this block exists so the two
/// different things both called "returned" (a courier
/// RTO and a buyer return) are never conflated here.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminShipmentReturnRef {
    pub id: Uuid,
    pub status: String,
    pub refund_status: String,
    pub refund_poisha: Option<i64>,
}

/// Private header row for the detail: carries the raw
/// phones; the view masks the buyer's account phone at
/// construction and never serialises this struct.
#[derive(FromRow)]
struct AdminShipmentDetailRow {
    id: Uuid,
    sub_order_id: Uuid,
    order_id: Uuid,
    store_id: Uuid,
    store_name: String,
    store_slug: String,
    courier: String,
    manual_courier_name: Option<String>,
    consignment_id: String,
    tracking_code: Option<String>,
    manual_tracking_id: Option<String>,
    status: String,
    note: Option<String>,
    sub_order_status: String,
    sub_order_created_at: DateTime<Utc>,
    cod_amount_poisha: i64,
    goods_subtotal_poisha: i64,
    discount_poisha: i64,
    delivery_charge_poisha: i64,
    order_status: String,
    payment_method: String,
    payment_status: String,
    advance_required_poisha: i64,
    advance_paid_poisha: i64,
    grand_total_poisha: i64,
    delivery_name: String,
    delivery_phone: String,
    delivery_address: String,
    delivery_zone: String,
    delivery_district: Option<String>,
    order_note: Option<String>,
    buyer_phone: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    last_polled_at: Option<DateTime<Utc>>,
    status_since: DateTime<Utc>,
}

/// The admin shipment detail. The full delivery block
/// (name, phone, address) is present — a stuck or failed
/// delivery cannot be worked with the courier without
/// it, and the seller and the courier already hold all
/// three (booking copies them into the courier request).
/// The buyer's ACCOUNT phone stays masked even here: it
/// is an identity credential, not a delivery fact. The
/// read is not reason-gated or audited — delivery data is
/// ephemeral operational data (the orders detail's
/// rationale), not a national credential like the NID,
/// whose reveal IS gated.
#[derive(Debug, Serialize)]
pub struct AdminShipmentDetail {
    pub id: Uuid,
    pub sub_order_id: Uuid,
    pub order_id: Uuid,
    pub store_id: Uuid,
    pub store_name: String,
    pub store_slug: String,
    pub courier: String,
    pub manual_courier_name: Option<String>,
    pub consignment_id: String,
    pub tracking_code: Option<String>,
    pub manual_tracking_id: Option<String>,
    pub status: String,
    pub note: Option<String>,
    pub sub_order_status: String,
    pub cod_amount_poisha: i64,
    pub goods_subtotal_poisha: i64,
    pub discount_poisha: i64,
    pub delivery_charge_poisha: i64,
    pub order_status: String,
    pub payment_method: String,
    pub payment_status: String,
    pub advance_required_poisha: i64,
    pub advance_paid_poisha: i64,
    pub grand_total_poisha: i64,
    pub delivery_name: String,
    pub delivery_phone: String,
    pub delivery_address: String,
    pub delivery_zone: String,
    pub delivery_district: Option<String>,
    pub order_note: Option<String>,
    pub buyer_phone_masked: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_polled_at: Option<DateTime<Utc>>,
    pub status_since: DateTime<Utc>,
    pub status_age_secs: i64,
    pub aged: bool,
    pub poll_overdue: bool,
    pub unrefreshed: bool,
    pub items: Vec<AdminShipmentItem>,
    pub events: Vec<AdminShipmentEvent>,
    /// The shared W55 timeline + steps for the
    /// sub-order — no surface assembles its own history.
    pub timeline: Vec<TimelineEvent>,
    pub steps: Vec<TimelineStep>,
    pub return_request: Option<AdminShipmentReturnRef>,
    /// This parcel came back on an order with money
    /// already paid in and no return request exists —
    /// the no-refund-path population (see the summary).
    pub advance_stranded: bool,
    pub audit_entries: Vec<AdminAuditEntry>,
}

/// One shipment's full oversight detail (admin shipments
/// remake). 404 for an unknown id.
pub async fn get_admin_shipment_detail(
    pool: &PgPool,
    shipment_id: Uuid,
) -> Result<AdminShipmentDetail, ApiError> {
    let row: Option<AdminShipmentDetailRow> = sqlx::query_as(
        "SELECT sh.id, sh.sub_order_id, so.order_id, so.store_id, \
           st.name AS store_name, st.slug AS store_slug, \
           sh.courier, sh.manual_courier_name, sh.consignment_id, \
           sh.tracking_code, sh.manual_tracking_id, sh.status, sh.note, \
           so.status AS sub_order_status, so.created_at AS sub_order_created_at, \
           sh.cod_amount_poisha, so.goods_subtotal_poisha, so.discount_poisha, \
           so.delivery_charge_poisha, \
           o.status AS order_status, o.payment_method, o.payment_status, \
           o.advance_required_poisha, o.advance_paid_poisha, o.grand_total_poisha, \
           o.delivery_name, o.delivery_phone, o.delivery_address, o.delivery_zone, \
           o.delivery_district, o.order_note, u.phone AS buyer_phone, \
           sh.created_at, sh.updated_at, sh.last_polled_at, \
           COALESCE((SELECT MAX(e.created_at) FROM shipment_events e \
                     WHERE e.shipment_id = sh.id AND e.status = sh.status), \
                    sh.created_at) AS status_since \
         FROM shipments sh \
         JOIN sub_orders so ON so.id = sh.sub_order_id \
         JOIN orders o ON o.id = so.order_id \
         JOIN stores st ON st.id = so.store_id \
         JOIN users u ON u.id = o.buyer_user_id \
         WHERE sh.id = $1",
    )
    .bind(shipment_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load admin shipment detail", e))?;
    let row = row.ok_or_else(|| ApiError::not_found("shipment not found"))?;

    let items: Vec<AdminShipmentItem> = sqlx::query_as(
        "SELECT title_snapshot, variant_name, quantity, unit_price_poisha \
         FROM order_items WHERE sub_order_id = $1 ORDER BY id",
    )
    .bind(row.sub_order_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load admin shipment items", e))?;

    let events: Vec<AdminShipmentEvent> = sqlx::query_as(
        "SELECT id, status, raw_status, source, created_at AS at \
         FROM shipment_events WHERE shipment_id = $1 \
         ORDER BY created_at ASC, id ASC",
    )
    .bind(shipment_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load admin shipment events", e))?;

    let return_request: Option<AdminShipmentReturnRef> = sqlx::query_as(
        "SELECT id, status, refund_status, refund_poisha \
         FROM return_requests WHERE sub_order_id = $1",
    )
    .bind(row.sub_order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load admin shipment return reference", e))?;

    let timeline = sub_order_timeline(
        pool,
        row.sub_order_id,
        &row.sub_order_status,
        row.sub_order_created_at,
    )
    .await?;
    let steps = sub_order_steps(&row.sub_order_status, Some(&row.status));
    let audit_entries = list_audit_for_target(pool, "shipment", &shipment_id.to_string(), 20)
        .await?;

    let thresholds = effective_shipment_aged_thresholds(pool).await;
    let now = Utc::now();
    let advance_stranded =
        row.status == "returned" && row.advance_paid_poisha > 0 && return_request.is_none();
    Ok(AdminShipmentDetail {
        buyer_phone_masked: mask_phone(&row.buyer_phone),
        status_age_secs: (now - row.status_since).num_seconds().max(0),
        aged: shipment_is_aged(&row.status, row.status_since, &thresholds, now),
        poll_overdue: shipment_is_poll_overdue(
            &row.courier,
            &row.status,
            row.last_polled_at,
            row.created_at,
            now,
        ),
        unrefreshed: shipment_is_unrefreshed(&row.courier, &row.status, row.updated_at, now),
        advance_stranded,
        id: row.id,
        sub_order_id: row.sub_order_id,
        order_id: row.order_id,
        store_id: row.store_id,
        store_name: row.store_name,
        store_slug: row.store_slug,
        courier: row.courier,
        manual_courier_name: row.manual_courier_name,
        consignment_id: row.consignment_id,
        tracking_code: row.tracking_code,
        manual_tracking_id: row.manual_tracking_id,
        status: row.status,
        note: row.note,
        sub_order_status: row.sub_order_status,
        cod_amount_poisha: row.cod_amount_poisha,
        goods_subtotal_poisha: row.goods_subtotal_poisha,
        discount_poisha: row.discount_poisha,
        delivery_charge_poisha: row.delivery_charge_poisha,
        order_status: row.order_status,
        payment_method: row.payment_method,
        payment_status: row.payment_status,
        advance_required_poisha: row.advance_required_poisha,
        advance_paid_poisha: row.advance_paid_poisha,
        grand_total_poisha: row.grand_total_poisha,
        delivery_name: row.delivery_name,
        delivery_phone: row.delivery_phone,
        delivery_address: row.delivery_address,
        delivery_zone: row.delivery_zone,
        delivery_district: row.delivery_district,
        order_note: row.order_note,
        created_at: row.created_at,
        updated_at: row.updated_at,
        last_polled_at: row.last_polled_at,
        status_since: row.status_since,
        items,
        events,
        timeline,
        steps,
        return_request,
        audit_entries,
    })
}

/// The admin refresh response: the shipment as the
/// shared core left it, plus the outcome as a stable
/// wire string (`advanced` / `unchanged`).
#[derive(Debug, Serialize)]
pub struct AdminShipmentRefresh {
    pub shipment: Shipment,
    pub outcome: String,
}

/// Refresh ONE shipment as an admin (admin shipments
/// remake): delegates to the shared refresh core — the
/// same function the participant endpoint and the worker
/// poller run — with the admin's id threaded through so
/// an advance is audited in the refresh transaction.
/// There is deliberately no bulk variant: one click, one
/// parcel, one courier call. A manual shipment returns
/// an honest `unchanged` (nothing exists to poll);
/// courier errors (not configured, gateway failure)
/// propagate as errors, never folded into `unchanged`.
pub async fn refresh_admin_shipment(
    state: &AppState,
    admin_id: Uuid,
    shipment_id: Uuid,
) -> Result<AdminShipmentRefresh, ApiError> {
    let (shipment, outcome) =
        refresh_shipment_by_id_audited(state, shipment_id, Some(admin_id)).await?;
    Ok(AdminShipmentRefresh {
        shipment,
        outcome: match outcome {
            RefreshOutcome::Advanced => "advanced".to_string(),
            RefreshOutcome::Unchanged => "unchanged".to_string(),
        },
    })
}

#[derive(Deserialize)]
pub struct AdminShipmentsQuery {
    /// Search: courier references (substring), delivery
    /// name / store (substring), phone (exact), or an id
    /// prefix — see [`list_shipments`].
    pub q: Option<String>,
    /// Unified-status filter; an unknown value is a 400.
    pub status: Option<String>,
    /// Courier filter: an API key or `manual`; an
    /// unknown value is a 400.
    pub courier: Option<String>,
    /// Restrict to one store's shipments.
    pub store_id: Option<Uuid>,
    /// Work-queue filter: `unrefreshed` | `poll_overdue`
    /// | `status_aged`; an unknown value is a 400.
    pub queue: Option<String>,
    /// newest | oldest | stale_first | cod_desc.
    pub sort: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn list_shipments_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminShipmentsQuery>,
) -> Result<Json<AdminShipmentList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_shipments(
            pool,
            &state.couriers_config,
            query.q.as_deref(),
            query.status.as_deref(),
            query.courier.as_deref(),
            query.store_id,
            query.queue.as_deref(),
            query.sort.as_deref(),
            query.page,
            query.limit,
        )
        .await?,
    ))
}

pub async fn get_shipment_detail_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminShipmentDetail>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(get_admin_shipment_detail(pool, id).await?))
}

pub async fn refresh_shipment_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminShipmentRefresh>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    Ok(Json(refresh_admin_shipment(&state, admin.id, id).await?))
}
