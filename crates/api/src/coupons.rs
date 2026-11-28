//! Coupons and promotions (Wave 14): platform and store coupons,
//! a stateless preview, and the shared coupon rules checkout
//! enforces inside its transaction.
//!
//! The money design (README, Wave 14 — decided once, applied
//! everywhere through the pure domain functions):
//!
//! - Scope: a coupon with no store (platform coupon) discounts
//!   the whole order's goods and the PLATFORM bears it —
//!   commission stays on the full goods. A store coupon discounts
//!   only its store's goods and the SELLER bears it — commission
//!   is computed on the discounted goods.
//! - The order-level discount is allocated across the in-scope
//!   sub-orders proportionally to goods (`allocate_discount`), so
//!   the shares sum exactly to the discount, and every
//!   sub-order's payout snapshot is goods − discount − commission.
//! - The buyer always pays goods − discount + delivery, whoever
//!   bears the discount.
//!
//! One rejection rule (`coupon_rejection_reason`) serves both the
//! preview endpoint and checkout, so a coupon the preview calls
//! valid cannot fail checkout for a different reason (time and
//! usage races aside — checkout re-checks under a row lock).
//! There is deliberately NO per-buyer usage limit in this wave:
//! the optional global `usage_limit` is the only cap.
//!
//! Admin coupons remake: the admin surface was rebuilt to the
//! remake standard. The admin list is a filtered, sorted,
//! searched queue with a whole-queue summary and a derived
//! lifecycle status ([`domain::coupon_status`]); a detail
//! view carries usage aggregates computed ONLY from the
//! redemption/order snapshots, recent redemptions with masked
//! buyer phones, and the coupon's audit history. Admin
//! mutations follow the remake canon — validation (400),
//! row lock, legality (no-op 409), one transaction with ONE
//! audit row: create is audited; deactivation REQUIRES a
//! reason (stored on the coupon row, migration 0081, and —
//! for a store coupon — noticed to the store's owner);
//! reactivation copies the outgoing reason into its audit
//! detail before clearing the stamps. Terms are editable
//! ONLY while a platform coupon has never been redeemed
//! (the same first-redemption line the delete FK draws):
//! orders snapshot discount amounts, never terms, so an
//! edit after a redemption could never be reconstructed.
//! There is still no admin DELETE — deactivation is the
//! lever, and it is reversible. The SELLER paths below are
//! unchanged: a seller's own create/toggle/delete is
//! routine self-management, not a platform action, and is
//! not audited (the audit module's standing rule).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    compute_coupon_discount, coupon_status, normalize_coupon_code,
    validate_coupon_deactivation_reason, validate_coupon_input, validate_coupon_list_sort,
    CouponSort,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::access::{resolve_scoped_store, StoreScopeQuery};
use crate::admin::require_admin;
use crate::audit::{list_audit_for_target, record_admin_action, AdminAuditEntry};
use crate::auth::AuthUser;
use crate::notices::enqueue_coupon_status_notice;
use crate::products::clamp_pagination;
use crate::stores::Store;
use crate::{ApiError, AppState};

/// Resolve the store a seller coupon endpoint acts on
/// (Wave 34): the caller's own store by default, or the
/// store named by `?store_id=` under a `manage_marketing`
/// role — coupons are marketing work, so a manager may run
/// them and staff may not (a staff attempt is the matrix's
/// 403; a stranger's is a 404).
async fn seller_coupon_store(
    pool: &PgPool,
    user: &AuthUser,
    scope: &StoreScopeQuery,
) -> Result<Store, ApiError> {
    let (store, _role) = resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        domain::STORE_ACTION_MANAGE_MARKETING,
    )
    .await?;
    Ok(store)
}

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// A coupon; also a `coupons` row. `store_id` is `None` for
/// platform coupons. The code is stored normalized (upper-case).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Coupon {
    pub id: Uuid,
    pub code: String,
    pub store_id: Option<Uuid>,
    pub discount_type: String,
    pub discount_value: i64,
    pub min_order_poisha: i64,
    pub max_discount_poisha: Option<i64>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<i32>,
    pub used_count: i32,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    /// Admin coupons remake (migration 0081): when an ADMIN
    /// deactivated this coupon, the moment of that act; NULL
    /// while the coupon is active or was deactivated by its
    /// own seller. The marker that distinguishes a platform
    /// deactivation from the seller's own.
    pub admin_deactivated_at: Option<DateTime<Utc>>,
    /// The admin who deactivated the coupon (migration
    /// 0081); NULL-cleared if that admin's account is
    /// deleted, and cleared on any reactivation.
    pub admin_deactivated_by_user_id: Option<Uuid>,
    /// The admin's required deactivation reason (migration
    /// 0081), shown to the seller on their coupons page;
    /// cleared on any reactivation, after the outgoing
    /// value is copied into the reactivation's audit
    /// detail.
    pub admin_deactivated_reason: Option<String>,
}

/// The column list matching [`Coupon`], shared by every query in
/// this module and by checkout's locking read in `orders`.
pub const COUPON_COLUMNS: &str = "id, code, store_id, discount_type, discount_value, \
    min_order_poisha, max_discount_poisha, starts_at, ends_at, usage_limit, \
    used_count, active, created_at, admin_deactivated_at, \
    admin_deactivated_by_user_id, admin_deactivated_reason";

/// A page of coupons plus the total count.
#[derive(Debug, Serialize)]
pub struct CouponList {
    pub items: Vec<Coupon>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// One seller group's goods, for scope arithmetic: which store,
/// and how much of the order's goods are theirs.
#[derive(Debug, Clone, Copy)]
pub struct GroupGoods {
    pub store_id: Uuid,
    pub goods_subtotal_poisha: i64,
}

/// The preview answer. `valid` is the whole story for the buyer;
/// `reason` explains an invalid coupon in words; `scope` and
/// `store_id` tell the client WHICH goods the coupon applies to
/// (a store coupon's discount depends on that store's goods in
/// the cart, which only the client knows before checkout).
#[derive(Debug, Serialize)]
pub struct CouponPreview {
    pub valid: bool,
    pub code: String,
    pub scope: Option<String>,
    pub store_id: Option<Uuid>,
    pub discount_poisha: i64,
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared coupon rules (preview + checkout)
// ---------------------------------------------------------------------------

/// Load a coupon by its code (any case — the lookup normalizes).
pub async fn load_coupon_by_code(
    pool: &PgPool,
    code: &str,
) -> Result<Option<Coupon>, ApiError> {
    sqlx::query_as(&format!("SELECT {COUPON_COLUMNS} FROM coupons WHERE code = $1"))
        .bind(normalize_coupon_code(code))
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load coupon by code", e))
}

/// The goods a coupon may discount, given the order's per-store
/// groups: everything for a platform coupon, only the coupon's
/// own store's goods for a store coupon (zero when that store is
/// not in the order at all).
pub fn eligible_subtotal_for(coupon: &Coupon, groups: &[GroupGoods]) -> i64 {
    match coupon.store_id {
        None => groups.iter().map(|g| g.goods_subtotal_poisha).sum(),
        Some(store_id) => groups
            .iter()
            .filter(|g| g.store_id == store_id)
            .map(|g| g.goods_subtotal_poisha)
            .sum(),
    }
}

/// Why a coupon cannot be used on an order whose eligible goods
/// subtotal is `eligible_subtotal_poisha` — or `None` when it can.
/// THE rejection rule: the preview endpoint and checkout both
/// call this, so they can never disagree about validity. The
/// checks run in the order a buyer would ask about them: state,
/// window, usage, applicability, minimum.
pub fn coupon_rejection_reason(
    coupon: &Coupon,
    now: DateTime<Utc>,
    eligible_subtotal_poisha: i64,
) -> Option<&'static str> {
    if !coupon.active {
        return Some("coupon is not active");
    }
    if let Some(starts_at) = coupon.starts_at {
        if now < starts_at {
            return Some("coupon has not started yet");
        }
    }
    if let Some(ends_at) = coupon.ends_at {
        if now > ends_at {
            return Some("coupon has expired");
        }
    }
    if let Some(limit) = coupon.usage_limit {
        if coupon.used_count >= limit {
            return Some("coupon usage limit reached");
        }
    }
    if eligible_subtotal_poisha <= 0 {
        return Some("coupon does not apply to this order");
    }
    if eligible_subtotal_poisha < coupon.min_order_poisha {
        return Some("order is below the coupon minimum");
    }
    None
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// Preview input. `goods_total_poisha` is the cart's whole goods
/// total (the platform scope). For a store coupon the eligible
/// goods are the coupon's store's share of the cart, which the
/// server cannot see from totals alone: the client passes it as
/// `store_goods_poisha` (with `store_id` naming whose goods those
/// are — a store coupon for a DIFFERENT store is not applicable).
/// Checkout needs none of this: it reads the real cart.
#[derive(Deserialize)]
pub struct CouponPreviewInput {
    pub code: String,
    pub goods_total_poisha: i64,
    pub store_id: Option<Uuid>,
    pub store_goods_poisha: Option<i64>,
}

/// Evaluate a coupon against client-supplied totals. Stateless —
/// nothing is written and nothing is reserved; checkout remains
/// the authority and re-checks everything under a row lock.
pub async fn preview_coupon(
    pool: &PgPool,
    input: &CouponPreviewInput,
) -> Result<CouponPreview, ApiError> {
    let code = normalize_coupon_code(&input.code);
    let coupon = load_coupon_by_code(pool, &input.code).await?;
    let Some(coupon) = coupon else {
        return Ok(CouponPreview {
            valid: false,
            code,
            scope: None,
            store_id: None,
            discount_poisha: 0,
            reason: Some("coupon not found".to_string()),
        });
    };

    let scope = if coupon.store_id.is_some() {
        "store"
    } else {
        "platform"
    };
    let eligible = match coupon.store_id {
        None => input.goods_total_poisha.max(0),
        Some(store_id) => match input.store_id {
            // The client priced a different store's goods: this
            // coupon cannot apply to them.
            Some(other) if other != store_id => 0,
            _ => input.store_goods_poisha.unwrap_or(0).max(0),
        },
    };

    if let Some(reason) = coupon_rejection_reason(&coupon, Utc::now(), eligible) {
        return Ok(CouponPreview {
            valid: false,
            code,
            scope: Some(scope.to_string()),
            store_id: coupon.store_id,
            discount_poisha: 0,
            reason: Some(reason.to_string()),
        });
    }
    let discount = compute_coupon_discount(
        &coupon.discount_type,
        coupon.discount_value,
        coupon.max_discount_poisha,
        eligible,
    );
    if discount <= 0 {
        return Ok(CouponPreview {
            valid: false,
            code,
            scope: Some(scope.to_string()),
            store_id: coupon.store_id,
            discount_poisha: 0,
            reason: Some("coupon does not reduce this order".to_string()),
        });
    }
    Ok(CouponPreview {
        valid: true,
        code,
        scope: Some(scope.to_string()),
        store_id: coupon.store_id,
        discount_poisha: discount,
        reason: None,
    })
}

// ---------------------------------------------------------------------------
// Create + list + toggle + delete (seller and admin share the core)
// ---------------------------------------------------------------------------

/// The create payload for seller and admin coupons alike. The
/// SCOPE is not in the payload: the seller endpoint stamps the
/// caller's store, the admin endpoint stamps none (platform).
#[derive(Deserialize)]
pub struct CreateCouponInput {
    pub code: String,
    pub discount_type: String,
    pub discount_value: i64,
    #[serde(default)]
    pub min_order_poisha: i64,
    pub max_discount_poisha: Option<i64>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<i32>,
}

/// True when a SQLx error is a Postgres unique violation (23505).
fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .map(|db_err| db_err.code().as_deref() == Some("23505"))
        .unwrap_or(false)
}

/// True when a SQLx error is a Postgres foreign-key violation
/// (23503) — e.g. deleting a coupon that has redemptions.
fn is_fk_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .map(|db_err| db_err.code().as_deref() == Some("23503"))
        .unwrap_or(false)
}

/// The term fields an admin may edit on a platform coupon
/// (admin coupons remake): the create payload minus the
/// code and the scope, which never change. The edit is
/// gated to coupons that have never been redeemed — see
/// [`update_admin_coupon_terms`].
#[derive(Deserialize)]
pub struct UpdateCouponTermsInput {
    pub discount_type: String,
    pub discount_value: i64,
    #[serde(default)]
    pub min_order_poisha: i64,
    pub max_discount_poisha: Option<i64>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub usage_limit: Option<i32>,
}

/// The seven term values of a coupon-shaped payload,
/// shared by the create payload and the terms-edit
/// payload so validation and the audit before/after
/// snapshots read exactly one shape.
struct CouponTerms {
    discount_type: String,
    discount_value: i64,
    min_order_poisha: i64,
    max_discount_poisha: Option<i64>,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    usage_limit: Option<i32>,
}

impl CreateCouponInput {
    fn terms(&self) -> CouponTerms {
        CouponTerms {
            discount_type: self.discount_type.clone(),
            discount_value: self.discount_value,
            min_order_poisha: self.min_order_poisha,
            max_discount_poisha: self.max_discount_poisha,
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            usage_limit: self.usage_limit,
        }
    }
}

impl UpdateCouponTermsInput {
    fn terms(&self) -> CouponTerms {
        CouponTerms {
            discount_type: self.discount_type.clone(),
            discount_value: self.discount_value,
            min_order_poisha: self.min_order_poisha,
            max_discount_poisha: self.max_discount_poisha,
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            usage_limit: self.usage_limit,
        }
    }
}

impl Coupon {
    fn terms(&self) -> CouponTerms {
        CouponTerms {
            discount_type: self.discount_type.clone(),
            discount_value: self.discount_value,
            min_order_poisha: self.min_order_poisha,
            max_discount_poisha: self.max_discount_poisha,
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            usage_limit: self.usage_limit,
        }
    }
}

/// The terms as a JSON object, for the terms-edit audit
/// detail's before/after snapshots.
fn terms_json(terms: &CouponTerms) -> serde_json::Value {
    json!({
        "discount_type": terms.discount_type,
        "discount_value": terms.discount_value,
        "min_order_poisha": terms.min_order_poisha,
        "max_discount_poisha": terms.max_discount_poisha,
        "starts_at": terms.starts_at,
        "ends_at": terms.ends_at,
        "usage_limit": terms.usage_limit,
    })
}

/// Validate one set of coupon terms against `code`: the
/// ONE validation every coupon write passes — create
/// (both scopes) and the admin terms edit alike, so the
/// paths can never diverge. All field rules come from
/// the domain validator plus the two cross-field checks
/// that live in this service layer (usage-limit
/// positivity, window ordering). Deliberately NOT a rule
/// here: a window in the past — creating an
/// already-expired coupon is pinned behaviour (wave14's
/// OLD10), because rejection lives in
/// [`coupon_rejection_reason`], not at creation.
fn validate_coupon_terms(code: &str, terms: &CouponTerms) -> Result<(), ApiError> {
    validate_coupon_input(
        code,
        &terms.discount_type,
        terms.discount_value,
        terms.min_order_poisha,
        terms.max_discount_poisha,
    )
    .map_err(ApiError::bad_request)?;
    if terms.usage_limit.is_some_and(|limit| limit <= 0) {
        return Err(ApiError::bad_request("usage_limit must be positive"));
    }
    if terms
        .starts_at
        .zip(terms.ends_at)
        .is_some_and(|(starts_at, ends_at)| ends_at <= starts_at)
    {
        return Err(ApiError::bad_request("ends_at must be after starts_at"));
    }
    Ok(())
}

/// Create a coupon for `store_id` (`None` = platform). All field
/// rules come from the one domain validator; the code is stored
/// normalized, so a code that already exists in ANY scope is a
/// 409 — codes are platform-global, a buyer types one code at
/// checkout and it must mean exactly one coupon.
///
/// Takes any executor (admin coupons remake): the seller
/// handler passes the pool; the admin handler passes its
/// transaction, so the audited create commits the coupon
/// and its audit row together.
pub async fn create_coupon(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    store_id: Option<Uuid>,
    input: &CreateCouponInput,
) -> Result<Coupon, ApiError> {
    let terms = input.terms();
    validate_coupon_terms(&input.code, &terms)?;

    sqlx::query_as(&format!(
        "INSERT INTO coupons (code, store_id, discount_type, discount_value, \
         min_order_poisha, max_discount_poisha, starts_at, ends_at, usage_limit) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {COUPON_COLUMNS}"
    ))
    .bind(normalize_coupon_code(&input.code))
    .bind(store_id)
    .bind(&terms.discount_type)
    .bind(terms.discount_value)
    .bind(terms.min_order_poisha)
    .bind(terms.max_discount_poisha)
    .bind(terms.starts_at)
    .bind(terms.ends_at)
    .bind(terms.usage_limit)
    .fetch_one(executor)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict("a coupon with this code already exists")
        } else {
            ApiError::internal("create coupon", e)
        }
    })
}

/// List coupons, newest first. `store_filter` scopes the list:
/// `Some(id)` for a seller's own coupons, `None` for the admin's
/// platform-wide list.
pub async fn list_coupons(
    pool: &PgPool,
    store_filter: Option<Uuid>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<CouponList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM coupons WHERE ($1::uuid IS NULL OR store_id = $1)",
    )
    .bind(store_filter)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count coupons", e))?;

    let items: Vec<Coupon> = sqlx::query_as(&format!(
        "SELECT {COUPON_COLUMNS} FROM coupons \
         WHERE ($1::uuid IS NULL OR store_id = $1) \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(store_filter)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list coupons", e))?;

    Ok(CouponList {
        items,
        total,
        page,
        limit,
    })
}

/// Activate or deactivate a coupon. `store_scope` restricts the
/// update to one store's coupons (the seller path — another
/// store's coupon is a 404, never a distinguishable error); the
/// admin path passes `None`.
pub async fn set_coupon_active(
    pool: &PgPool,
    coupon_id: Uuid,
    store_scope: Option<Uuid>,
    active: bool,
) -> Result<Coupon, ApiError> {
    // Admin coupons remake: ANY reactivation clears the
    // admin-deactivation stamps (migration 0081) — a
    // coupon that is live again is not "admin-deactivated"
    // whatever path reactivated it. (The admin path does
    // its own clearing inside its audited transition,
    // after copying the outgoing reason into the audit
    // detail; this shared seller path has no audit row to
    // preserve it in, and the seller is the actor here.)
    sqlx::query_as(&format!(
        "UPDATE coupons SET active = $3, \
         admin_deactivated_at = CASE WHEN $3 THEN NULL ELSE admin_deactivated_at END, \
         admin_deactivated_by_user_id = CASE WHEN $3 THEN NULL \
             ELSE admin_deactivated_by_user_id END, \
         admin_deactivated_reason = CASE WHEN $3 THEN NULL \
             ELSE admin_deactivated_reason END \
         WHERE id = $1 \
         AND ($2::uuid IS NULL OR store_id = $2) RETURNING {COUPON_COLUMNS}"
    ))
    .bind(coupon_id)
    .bind(store_scope)
    .bind(active)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("set coupon active", e))?
    .ok_or_else(|| ApiError::not_found("coupon not found"))
}

/// Delete one of a store's coupons. A coupon that already has
/// redemptions cannot be deleted (its history prices real
/// orders) — the FK violation becomes a 409 pointing at
/// deactivation instead.
pub async fn delete_coupon(
    pool: &PgPool,
    coupon_id: Uuid,
    store_id: Uuid,
) -> Result<(), ApiError> {
    let deleted = sqlx::query("DELETE FROM coupons WHERE id = $1 AND store_id = $2")
        .bind(coupon_id)
        .bind(store_id)
        .execute(pool)
        .await
        .map_err(|e| {
            if is_fk_violation(&e) {
                ApiError::conflict(
                    "coupon has redemptions and cannot be deleted — deactivate it instead",
                )
            } else {
                ApiError::internal("delete coupon", e)
            }
        })?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::not_found("coupon not found"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin queue, detail, and audited mutations (admin coupons remake)
// ---------------------------------------------------------------------------

/// Query parameters for the ADMIN coupon list. Every
/// value is validated fail-closed in
/// [`list_admin_coupons`]: an unknown scope, status or
/// discount type is a 400 naming the allowed set, never
/// a silently unfiltered list.
#[derive(Deserialize)]
pub struct AdminListCouponsQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
    /// Code search text (matched with ILIKE over the
    /// escaped pattern — see [`domain::search_like_pattern`]).
    pub q: Option<String>,
    /// `platform` or `store`; absent means both scopes.
    pub scope: Option<String>,
    /// A derived lifecycle status ([`domain::coupon_status`]):
    /// `live`, `scheduled`, `expired`, `exhausted` or
    /// `inactive`.
    pub status: Option<String>,
    /// `percent` or `fixed`.
    pub discount_type: Option<String>,
    /// One store's coupons.
    pub store_id: Option<Uuid>,
    /// A [`CouponSort`] name; absent means `newest`.
    pub sort: Option<String>,
}

/// One row of the admin queue: the coupon, its store's
/// identity (NULL pair for a platform coupon), and the
/// derived lifecycle status.
#[derive(Debug, Serialize)]
pub struct AdminCoupon {
    #[serde(flatten)]
    pub coupon: Coupon,
    pub store_name: Option<String>,
    pub store_slug: Option<String>,
    pub status: String,
}

/// The raw admin-queue row (the [`AdminCoupon`] view is
/// built from it so the phone-free row never serializes
/// anything it should not — there is no phone here at
/// all: coupon surfaces carry none).
#[derive(Debug, FromRow)]
struct AdminCouponRow {
    id: Uuid,
    code: String,
    store_id: Option<Uuid>,
    discount_type: String,
    discount_value: i64,
    min_order_poisha: i64,
    max_discount_poisha: Option<i64>,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    usage_limit: Option<i32>,
    used_count: i32,
    active: bool,
    created_at: DateTime<Utc>,
    admin_deactivated_at: Option<DateTime<Utc>>,
    admin_deactivated_by_user_id: Option<Uuid>,
    admin_deactivated_reason: Option<String>,
    store_name: Option<String>,
    store_slug: Option<String>,
}

impl AdminCouponRow {
    fn into_admin_coupon(self, now: DateTime<Utc>) -> AdminCoupon {
        let status = coupon_status(
            self.active,
            self.starts_at.map(|t| t.timestamp()),
            self.ends_at.map(|t| t.timestamp()),
            self.usage_limit.map(i64::from),
            i64::from(self.used_count),
            now.timestamp(),
        )
        .to_string();
        AdminCoupon {
            coupon: Coupon {
                id: self.id,
                code: self.code,
                store_id: self.store_id,
                discount_type: self.discount_type,
                discount_value: self.discount_value,
                min_order_poisha: self.min_order_poisha,
                max_discount_poisha: self.max_discount_poisha,
                starts_at: self.starts_at,
                ends_at: self.ends_at,
                usage_limit: self.usage_limit,
                used_count: self.used_count,
                active: self.active,
                created_at: self.created_at,
                admin_deactivated_at: self.admin_deactivated_at,
                admin_deactivated_by_user_id: self.admin_deactivated_by_user_id,
                admin_deactivated_reason: self.admin_deactivated_reason,
            },
            store_name: self.store_name,
            store_slug: self.store_slug,
            status,
        }
    }
}

/// Whole-queue coupon counts for the admin list summary.
/// Deliberately UNFILTERED — the summary describes the
/// platform, not the page (the admin queue canon). The
/// lifecycle buckets partition the ACTIVE coupons by the
/// same precedence as [`domain::coupon_status`], computed
/// with the database clock inside one statement (the
/// gift-card stats pattern); `inactive_count` stands on
/// its own axis. Redemption figures are all-time and
/// GROSS: a redemption is never removed and `used_count`
/// never decremented, even when its order is later
/// cancelled — the net figure belongs to the per-coupon
/// detail, where the cancelled share is labelled.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminCouponSummary {
    pub total: i64,
    pub platform_count: i64,
    pub store_count: i64,
    pub active_count: i64,
    pub inactive_count: i64,
    pub live_count: i64,
    pub scheduled_count: i64,
    pub expired_count: i64,
    pub exhausted_count: i64,
    pub redemptions_all_time: i64,
    pub gross_discount_poisha_all_time: i64,
}

/// A page of the admin queue plus its whole-queue summary.
#[derive(Debug, Serialize)]
pub struct AdminCouponList {
    pub items: Vec<AdminCoupon>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
    pub summary: AdminCouponSummary,
}

/// The SQL fragment deriving a coupon row's lifecycle
/// status with the database clock, mirroring
/// [`domain::coupon_status`] EXACTLY, precedence included
/// (the gift-card STATUS_FILTER pattern). `c` is the
/// coupons table alias. Boundary semantics are the
/// coupon rule's: scheduled strictly before the start,
/// expired strictly after the end.
const COUPON_STATUS_SQL: &str = "CASE \
    WHEN NOT c.active THEN 'inactive' \
    WHEN c.starts_at IS NOT NULL AND now() < c.starts_at THEN 'scheduled' \
    WHEN c.ends_at IS NOT NULL AND now() > c.ends_at THEN 'expired' \
    WHEN c.usage_limit IS NOT NULL AND c.used_count >= c.usage_limit THEN 'exhausted' \
    ELSE 'live' END";

/// The admin-queue projection: every coupon column plus
/// the store identity, over alias `c` with a LEFT JOIN —
/// platform coupons have no store, and an inner join
/// would silently drop every one of them.
const ADMIN_COUPON_PROJECTION: &str = "c.id, c.code, c.store_id, c.discount_type, \
    c.discount_value, c.min_order_poisha, c.max_discount_poisha, c.starts_at, \
    c.ends_at, c.usage_limit, c.used_count, c.active, c.created_at, \
    c.admin_deactivated_at, c.admin_deactivated_by_user_id, \
    c.admin_deactivated_reason, st.name AS store_name, st.slug AS store_slug";

async fn admin_coupon_summary(pool: &PgPool) -> Result<AdminCouponSummary, ApiError> {
    sqlx::query_as(
        "SELECT COUNT(*)::bigint AS total, \
         (COUNT(*) FILTER (WHERE store_id IS NULL))::bigint AS platform_count, \
         (COUNT(*) FILTER (WHERE store_id IS NOT NULL))::bigint AS store_count, \
         (COUNT(*) FILTER (WHERE active))::bigint AS active_count, \
         (COUNT(*) FILTER (WHERE NOT active))::bigint AS inactive_count, \
         (COUNT(*) FILTER (WHERE active AND NOT (ends_at IS NOT NULL AND ends_at < now()) \
             AND NOT (starts_at IS NOT NULL AND starts_at > now()) \
             AND (usage_limit IS NULL OR used_count < usage_limit)))::bigint AS live_count, \
         (COUNT(*) FILTER (WHERE active AND starts_at IS NOT NULL AND starts_at > now() \
             AND NOT (ends_at IS NOT NULL AND ends_at < now())))::bigint AS scheduled_count, \
         (COUNT(*) FILTER (WHERE active AND ends_at IS NOT NULL \
             AND ends_at < now()))::bigint AS expired_count, \
         (COUNT(*) FILTER (WHERE active \
             AND NOT (ends_at IS NOT NULL AND ends_at < now()) \
             AND NOT (starts_at IS NOT NULL AND starts_at > now()) \
             AND usage_limit IS NOT NULL AND used_count >= usage_limit))::bigint \
             AS exhausted_count, \
         (SELECT COUNT(*) FROM coupon_redemptions)::bigint AS redemptions_all_time, \
         (SELECT COALESCE(SUM(discount_poisha), 0) FROM coupon_redemptions)::bigint \
             AS gross_discount_poisha_all_time \
         FROM coupons",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("admin coupon summary", e))
}

/// The admin coupon queue: filtered, sorted, searched,
/// paginated, with the whole-queue summary. Filters are
/// validated fail-closed BEFORE any query runs.
pub async fn list_admin_coupons(
    pool: &PgPool,
    query: &AdminListCouponsQuery,
) -> Result<AdminCouponList, ApiError> {
    let sort = validate_coupon_list_sort(query.sort.as_deref()).map_err(ApiError::bad_request)?;
    let scope = match query.scope.as_deref() {
        None => None,
        Some(scope @ ("platform" | "store")) => Some(scope),
        Some(_) => {
            return Err(ApiError::bad_request(
                "scope must be one of: platform, store",
            ))
        }
    };
    let status = match query.status.as_deref() {
        None => None,
        Some(status @ ("live" | "scheduled" | "expired" | "exhausted" | "inactive")) => {
            Some(status)
        }
        Some(_) => {
            return Err(ApiError::bad_request(
                "status must be one of: live, scheduled, expired, exhausted, inactive",
            ))
        }
    };
    let discount_type = match query.discount_type.as_deref() {
        None => None,
        Some(kind @ ("percent" | "fixed")) => Some(kind),
        Some(_) => {
            return Err(ApiError::bad_request(
                "discount_type must be one of: percent, fixed",
            ))
        }
    };
    let (page, limit) = clamp_pagination(query.page, query.limit);
    let offset = (page - 1) * limit;
    let pattern = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(domain::search_like_pattern);
    let order_by = match sort {
        CouponSort::Newest => "c.created_at DESC, c.id DESC",
        CouponSort::Oldest => "c.created_at ASC, c.id ASC",
        CouponSort::CodeAsc => "c.code ASC, c.id ASC",
        CouponSort::MostUsed => "c.used_count DESC, c.created_at DESC, c.id DESC",
        CouponSort::ValueDesc => "c.discount_value DESC, c.created_at DESC, c.id DESC",
    };
    // One WHERE for the count and the page, so the two can
    // never disagree about what the filter means. Every
    // parameter is always bound; a NULL parameter disables
    // its condition.
    let where_sql = "($1::text IS NULL OR c.code ILIKE $1) \
        AND ($2::text IS NULL OR ($2 = 'platform' AND c.store_id IS NULL) \
            OR ($2 = 'store' AND c.store_id IS NOT NULL)) \
        AND ($3::text IS NULL OR c.discount_type = $3) \
        AND ($4::uuid IS NULL OR c.store_id = $4)";

    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM coupons c WHERE {where_sql} \
         AND ($5::text IS NULL OR ({COUPON_STATUS_SQL}) = $5)"
    ))
    .bind(&pattern)
    .bind(scope)
    .bind(discount_type)
    .bind(query.store_id)
    .bind(status)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin coupons", e))?;

    let rows: Vec<AdminCouponRow> = sqlx::query_as(&format!(
        "SELECT {ADMIN_COUPON_PROJECTION} FROM coupons c \
         LEFT JOIN stores st ON st.id = c.store_id \
         WHERE {where_sql} \
         AND ($5::text IS NULL OR ({COUPON_STATUS_SQL}) = $5) \
         ORDER BY {order_by} LIMIT $6 OFFSET $7"
    ))
    .bind(&pattern)
    .bind(scope)
    .bind(discount_type)
    .bind(query.store_id)
    .bind(status)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin coupons", e))?;

    let now = Utc::now();
    let summary = admin_coupon_summary(pool).await?;
    Ok(AdminCouponList {
        items: rows.into_iter().map(|row| row.into_admin_coupon(now)).collect(),
        total,
        page,
        limit,
        summary,
    })
}

/// Load one admin-queue row by coupon id (404 source for
/// the detail and the mutation responses).
async fn load_admin_coupon(pool: &PgPool, coupon_id: Uuid) -> Result<AdminCoupon, ApiError> {
    let row: AdminCouponRow = sqlx::query_as(&format!(
        "SELECT {ADMIN_COUPON_PROJECTION} FROM coupons c \
         LEFT JOIN stores st ON st.id = c.store_id WHERE c.id = $1"
    ))
    .bind(coupon_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load admin coupon", e))?
    .ok_or_else(|| ApiError::not_found("coupon not found"))?;
    Ok(row.into_admin_coupon(Utc::now()))
}

/// Per-coupon usage, computed ONLY from the redemption
/// and order/sub-order snapshots (admin coupons remake):
/// the gross figures count every redemption ever written
/// (a cancelled order's redemption is never removed);
/// the cancelled and returned shares are labelled
/// separately, and the net is gross minus both — the two
/// subtractions are disjoint, because a cancelled order's
/// sub-orders are `cancelled`, never `returned`.
#[derive(Debug, Serialize)]
pub struct AdminCouponUsage {
    pub gross_redemptions: i64,
    pub gross_discount_poisha: i64,
    pub cancelled_redemptions: i64,
    pub cancelled_discount_poisha: i64,
    pub returned_sub_orders: i64,
    pub returned_discount_poisha: i64,
    pub net_effective_discount_poisha: i64,
    pub distinct_buyers: i64,
    pub first_redeemed_at: Option<DateTime<Utc>>,
    pub last_redeemed_at: Option<DateTime<Utc>>,
}

/// The usage query's raw row: the net is computed in
/// Rust during construction (the summary canon), not
/// re-derived in SQL.
#[derive(Debug, FromRow)]
struct AdminCouponUsageRow {
    gross_redemptions: i64,
    gross_discount_poisha: i64,
    cancelled_redemptions: i64,
    cancelled_discount_poisha: i64,
    distinct_buyers: i64,
    first_redeemed_at: Option<DateTime<Utc>>,
    last_redeemed_at: Option<DateTime<Utc>>,
}

/// One recent redemption on the detail: the order it
/// landed on, the discount it granted, the order's
/// current status, and the buyer's MASKED account phone.
#[derive(Debug, Serialize)]
pub struct AdminCouponRedemption {
    pub id: Uuid,
    pub order_id: Uuid,
    pub buyer_phone_masked: String,
    pub discount_poisha: i64,
    pub order_status: String,
    pub created_at: DateTime<Utc>,
}

/// The raw redemption row: the buyer's full phone is
/// selected only to be masked in [`AdminCouponRedemption`]
/// and never serializes (the admin orders row pattern).
#[derive(Debug, FromRow)]
struct AdminCouponRedemptionRow {
    id: Uuid,
    order_id: Uuid,
    buyer_phone: String,
    discount_poisha: i64,
    order_status: String,
    created_at: DateTime<Utc>,
}

/// The admin coupon detail: the queue row, its snapshot
/// usage, its recent redemptions, and its audit history
/// (admin actions DO target coupons in this remake, so
/// the history is real, not theatre).
#[derive(Debug, Serialize)]
pub struct AdminCouponDetail {
    #[serde(flatten)]
    pub coupon: AdminCoupon,
    pub usage: AdminCouponUsage,
    pub recent_redemptions: Vec<AdminCouponRedemption>,
    pub audit_history: Vec<AdminAuditEntry>,
}

/// The admin coupon detail. The coupon loads first and
/// 404s before any aggregate runs (the detail canon).
pub async fn get_admin_coupon_detail(
    pool: &PgPool,
    coupon_id: Uuid,
) -> Result<AdminCouponDetail, ApiError> {
    let coupon = load_admin_coupon(pool, coupon_id).await?;

    // Query A — the redemption/order axis. The join to
    // orders cannot fan out: coupon_redemptions is one
    // row per order (UNIQUE on order_id).
    let usage_row: AdminCouponUsageRow = sqlx::query_as(
        "SELECT COUNT(*)::bigint AS gross_redemptions, \
         COALESCE(SUM(cr.discount_poisha), 0)::bigint AS gross_discount_poisha, \
         (COUNT(*) FILTER (WHERE o.status = 'cancelled'))::bigint \
             AS cancelled_redemptions, \
         COALESCE(SUM(cr.discount_poisha) FILTER (WHERE o.status = 'cancelled'), 0)::bigint \
             AS cancelled_discount_poisha, \
         COUNT(DISTINCT cr.buyer_id)::bigint AS distinct_buyers, \
         MIN(cr.created_at) AS first_redeemed_at, \
         MAX(cr.created_at) AS last_redeemed_at \
         FROM coupon_redemptions cr \
         JOIN orders o ON o.id = cr.order_id \
         WHERE cr.coupon_id = $1",
    )
    .bind(coupon_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("admin coupon usage", e))?;

    // Query B — the returned share, kept separate:
    // joining sub_orders into Query A would multiply each
    // redemption's discount once per sub-order. The
    // returned share exists only as the allocated snapshot
    // on the sub-order rows.
    let (returned_sub_orders, returned_discount_poisha): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*)::bigint, COALESCE(SUM(so.discount_poisha), 0)::bigint \
         FROM sub_orders so \
         JOIN coupon_redemptions cr ON cr.order_id = so.order_id AND cr.coupon_id = $1 \
         WHERE so.status = 'returned'",
    )
    .bind(coupon_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("admin coupon returned share", e))?;

    let usage = AdminCouponUsage {
        net_effective_discount_poisha: usage_row.gross_discount_poisha
            - usage_row.cancelled_discount_poisha
            - returned_discount_poisha,
        gross_redemptions: usage_row.gross_redemptions,
        gross_discount_poisha: usage_row.gross_discount_poisha,
        cancelled_redemptions: usage_row.cancelled_redemptions,
        cancelled_discount_poisha: usage_row.cancelled_discount_poisha,
        returned_sub_orders,
        returned_discount_poisha,
        distinct_buyers: usage_row.distinct_buyers,
        first_redeemed_at: usage_row.first_redeemed_at,
        last_redeemed_at: usage_row.last_redeemed_at,
    };

    let redemption_rows: Vec<AdminCouponRedemptionRow> = sqlx::query_as(
        "SELECT cr.id, cr.order_id, u.phone AS buyer_phone, cr.discount_poisha, \
         o.status AS order_status, cr.created_at \
         FROM coupon_redemptions cr \
         JOIN orders o ON o.id = cr.order_id \
         JOIN users u ON u.id = cr.buyer_id \
         WHERE cr.coupon_id = $1 \
         ORDER BY cr.created_at DESC, cr.id DESC LIMIT 20",
    )
    .bind(coupon_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("admin coupon redemptions", e))?;
    let recent_redemptions = redemption_rows
        .into_iter()
        .map(|row| AdminCouponRedemption {
            id: row.id,
            order_id: row.order_id,
            buyer_phone_masked: domain::mask_phone(&row.buyer_phone),
            discount_poisha: row.discount_poisha,
            order_status: row.order_status,
            created_at: row.created_at,
        })
        .collect();

    let audit_history = list_audit_for_target(pool, "coupon", &coupon_id.to_string(), 20).await?;

    Ok(AdminCouponDetail {
        coupon,
        usage,
        recent_redemptions,
        audit_history,
    })
}

/// The admin active-toggle (admin coupons remake): the
/// remade replacement for the pre-remake blind UPDATE.
///
/// Gates, in order: deactivation requires a reason
/// (validated first, 400 — reactivation takes none);
/// the coupon row is locked `FOR UPDATE` in one
/// transaction (404 when missing); setting the state the
/// coupon is already in is a 409 that writes NOTHING (no
/// phantom audit row — the LO-93 rule). The transition
/// writes the flag, the migration-0081 stamps (set on
/// deactivation, cleared on reactivation — after the
/// outgoing reason and actor are copied into the
/// reactivation's audit detail), ONE audit row, and —
/// for a store coupon only — the owner's notice, all in
/// the same transaction. A platform coupon has no seller
/// to notify; it is still audited.
pub async fn set_admin_coupon_active(
    pool: &PgPool,
    admin_id: Uuid,
    coupon_id: Uuid,
    active: bool,
    reason: Option<&str>,
) -> Result<Coupon, ApiError> {
    let reason = if active {
        None
    } else {
        Some(
            validate_coupon_deactivation_reason(reason.unwrap_or(""))
                .map_err(ApiError::bad_request)?,
        )
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin coupon toggle", e))?;
    let coupon: Coupon = sqlx::query_as(&format!(
        "SELECT {COUPON_COLUMNS} FROM coupons WHERE id = $1 FOR UPDATE"
    ))
    .bind(coupon_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock coupon", e))?
    .ok_or_else(|| ApiError::not_found("coupon not found"))?;

    if coupon.active == active {
        return Err(ApiError::conflict(if active {
            "কুপনটি ইতিমধ্যে চালু আছে (the coupon is already active)"
        } else {
            "কুপনটি ইতিমধ্যে বন্ধ আছে (the coupon is already inactive)"
        }));
    }

    let updated: Coupon = if active {
        sqlx::query_as(&format!(
            "UPDATE coupons SET active = true, admin_deactivated_at = NULL, \
             admin_deactivated_by_user_id = NULL, admin_deactivated_reason = NULL \
             WHERE id = $1 RETURNING {COUPON_COLUMNS}"
        ))
        .bind(coupon_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("reactivate coupon", e))?
    } else {
        sqlx::query_as(&format!(
            "UPDATE coupons SET active = false, admin_deactivated_at = now(), \
             admin_deactivated_by_user_id = $2, admin_deactivated_reason = $3 \
             WHERE id = $1 RETURNING {COUPON_COLUMNS}"
        ))
        .bind(coupon_id)
        .bind(admin_id)
        .bind(&reason)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("deactivate coupon", e))?
    };

    let (action, detail) = if active {
        (
            "coupon.reactivate",
            json!({
                "code": coupon.code,
                "store_id": coupon.store_id,
                "previous_reason": coupon.admin_deactivated_reason,
                "previous_deactivated_by": coupon.admin_deactivated_by_user_id,
            }),
        )
    } else {
        (
            "coupon.deactivate",
            json!({
                "code": coupon.code,
                "store_id": coupon.store_id,
                "reason": reason,
            }),
        )
    };
    record_admin_action(
        &mut *tx,
        admin_id,
        action,
        "coupon",
        Some(&coupon_id.to_string()),
        Some(detail),
    )
    .await?;

    if let Some(store_id) = coupon.store_id {
        let store_name: String = sqlx::query_scalar("SELECT name FROM stores WHERE id = $1")
            .bind(store_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("load coupon store name", e))?;
        let (kind, note) = if active {
            (domain::NOTIFICATION_COUPON_REACTIVATED, None)
        } else {
            (domain::NOTIFICATION_COUPON_DEACTIVATED, reason.as_deref())
        };
        enqueue_coupon_status_notice(
            &mut tx,
            store_id,
            &store_name,
            &coupon.code,
            kind,
            note,
        )
        .await?;
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit coupon toggle", e))?;
    Ok(updated)
}

/// Edit a platform coupon's terms (admin coupons
/// remake), gated to a coupon that has NEVER been
/// redeemed. Orders snapshot discount amounts, never
/// terms, so after a first redemption no record could
/// reconstruct why a redemption granted what it did —
/// the same first-redemption line the delete FK draws
/// for the neighbouring destructive operation. A
/// redeemed coupon's terms are locked: deactivate it and
/// create a new code. A STORE coupon's terms are the
/// seller's own (an admin's levers on a store coupon are
/// deactivation, with a reason, and reactivation).
///
/// The row is locked `FOR UPDATE` first (404); terms are
/// then validated by the ONE shared validator (400);
/// the legality gates follow (409): store scope,
/// redeemed, and a payload identical to the stored terms
/// (a no-op writes nothing, not even an audit row). The
/// update and ONE audit row — with the full before/after
/// term snapshots — commit together.
pub async fn update_admin_coupon_terms(
    pool: &PgPool,
    admin_id: Uuid,
    coupon_id: Uuid,
    input: &UpdateCouponTermsInput,
) -> Result<Coupon, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin coupon terms edit", e))?;
    let coupon: Coupon = sqlx::query_as(&format!(
        "SELECT {COUPON_COLUMNS} FROM coupons WHERE id = $1 FOR UPDATE"
    ))
    .bind(coupon_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock coupon", e))?
    .ok_or_else(|| ApiError::not_found("coupon not found"))?;

    let new_terms = input.terms();
    validate_coupon_terms(&coupon.code, &new_terms)?;

    if coupon.store_id.is_some() {
        return Err(ApiError::conflict(
            "স্টোর কুপনের শর্ত বিক্রেতার নিজের (a store coupon's terms belong to its seller — an admin may deactivate it, with a reason, but not rewrite them)",
        ));
    }
    if coupon.used_count > 0 {
        return Err(ApiError::conflict(
            "কুপনটি ইতিমধ্যে ব্যবহৃত হয়েছে, তাই এর শর্ত বদলানো যাবে না (the coupon has redemptions and its terms are locked — deactivate it and create a new code)",
        ));
    }
    let old_terms = coupon.terms();
    if old_terms.discount_type == new_terms.discount_type
        && old_terms.discount_value == new_terms.discount_value
        && old_terms.min_order_poisha == new_terms.min_order_poisha
        && old_terms.max_discount_poisha == new_terms.max_discount_poisha
        && old_terms.starts_at == new_terms.starts_at
        && old_terms.ends_at == new_terms.ends_at
        && old_terms.usage_limit == new_terms.usage_limit
    {
        return Err(ApiError::conflict(
            "কুপনটির শর্ত ইতিমধ্যে এমনই আছে (the coupon already has these terms — nothing changed)",
        ));
    }

    let updated: Coupon = sqlx::query_as(&format!(
        "UPDATE coupons SET discount_type = $2, discount_value = $3, \
         min_order_poisha = $4, max_discount_poisha = $5, starts_at = $6, \
         ends_at = $7, usage_limit = $8 \
         WHERE id = $1 RETURNING {COUPON_COLUMNS}"
    ))
    .bind(coupon_id)
    .bind(&new_terms.discount_type)
    .bind(new_terms.discount_value)
    .bind(new_terms.min_order_poisha)
    .bind(new_terms.max_discount_poisha)
    .bind(new_terms.starts_at)
    .bind(new_terms.ends_at)
    .bind(new_terms.usage_limit)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("update coupon terms", e))?;

    record_admin_action(
        &mut *tx,
        admin_id,
        "coupon.update_terms",
        "coupon",
        Some(&coupon_id.to_string()),
        Some(json!({
            "code": coupon.code,
            "before": terms_json(&old_terms),
            "after": terms_json(&new_terms),
        })),
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit coupon terms edit", e))?;
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Request shapes + handlers (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListCouponsQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct SetCouponActiveInput {
    pub active: bool,
    /// Admin coupons remake: the deactivation reason.
    /// Required by the ADMIN path when `active` is false
    /// (validated there); the seller path ignores it — a
    /// seller's own toggle takes no reason.
    pub reason: Option<String>,
}

pub async fn preview_handler(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(input): Json<CouponPreviewInput>,
) -> Result<Json<CouponPreview>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(preview_coupon(pool, &input).await?))
}

pub async fn create_seller_coupon_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Json(input): Json<CreateCouponInput>,
) -> Result<(StatusCode, Json<Coupon>), ApiError> {
    let pool = state.require_pool()?;
    let store = seller_coupon_store(pool, &user, &scope).await?;
    let coupon = create_coupon(pool, Some(store.id), &input).await?;
    Ok((StatusCode::CREATED, Json(coupon)))
}

pub async fn list_seller_coupons_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListCouponsQuery>,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<CouponList>, ApiError> {
    let pool = state.require_pool()?;
    let store = seller_coupon_store(pool, &user, &scope).await?;
    Ok(Json(
        list_coupons(pool, Some(store.id), query.page, query.limit).await?,
    ))
}

pub async fn set_seller_coupon_active_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(scope): Query<StoreScopeQuery>,
    Json(input): Json<SetCouponActiveInput>,
) -> Result<Json<Coupon>, ApiError> {
    let pool = state.require_pool()?;
    let store = seller_coupon_store(pool, &user, &scope).await?;
    Ok(Json(
        set_coupon_active(pool, id, Some(store.id), input.active).await?,
    ))
}

pub async fn delete_seller_coupon_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<StatusCode, ApiError> {
    let pool = state.require_pool()?;
    let store = seller_coupon_store(pool, &user, &scope).await?;
    delete_coupon(pool, id, store.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_admin_coupon_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<CreateCouponInput>,
) -> Result<(StatusCode, Json<Coupon>), ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    // Admin coupons remake: the create and its audit row
    // commit together (the seller create stays a plain
    // pool call — routine self-management is not audited).
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin coupon create", e))?;
    let coupon = create_coupon(&mut *tx, None, &input).await?;
    record_admin_action(
        &mut *tx,
        admin.id,
        "coupon.create",
        "coupon",
        Some(&coupon.id.to_string()),
        Some(json!({
            "code": coupon.code,
            "store_id": coupon.store_id,
            "discount_type": coupon.discount_type,
            "discount_value": coupon.discount_value,
        })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit coupon create", e))?;
    Ok((StatusCode::CREATED, Json(coupon)))
}

pub async fn list_admin_coupons_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminListCouponsQuery>,
) -> Result<Json<AdminCouponList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(list_admin_coupons(pool, &query).await?))
}

pub async fn get_admin_coupon_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AdminCouponDetail>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(get_admin_coupon_detail(pool, id).await?))
}

pub async fn set_admin_coupon_active_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<SetCouponActiveInput>,
) -> Result<Json<Coupon>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        set_admin_coupon_active(pool, admin.id, id, input.active, input.reason.as_deref())
            .await?,
    ))
}

pub async fn update_admin_coupon_terms_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateCouponTermsInput>,
) -> Result<Json<Coupon>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        update_admin_coupon_terms(pool, admin.id, id, &input).await?,
    ))
}
