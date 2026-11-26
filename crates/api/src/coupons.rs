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

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{compute_coupon_discount, normalize_coupon_code, validate_coupon_input};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::access::{resolve_scoped_store, StoreScopeQuery};
use crate::admin::require_admin;
use crate::auth::AuthUser;
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
}

/// The column list matching [`Coupon`], shared by every query in
/// this module and by checkout's locking read in `orders`.
pub const COUPON_COLUMNS: &str = "id, code, store_id, discount_type, discount_value, \
    min_order_poisha, max_discount_poisha, starts_at, ends_at, usage_limit, \
    used_count, active, created_at";

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

/// Create a coupon for `store_id` (`None` = platform). All field
/// rules come from the one domain validator; the code is stored
/// normalized, so a code that already exists in ANY scope is a
/// 409 — codes are platform-global, a buyer types one code at
/// checkout and it must mean exactly one coupon.
pub async fn create_coupon(
    pool: &PgPool,
    store_id: Option<Uuid>,
    input: &CreateCouponInput,
) -> Result<Coupon, ApiError> {
    validate_coupon_input(
        &input.code,
        &input.discount_type,
        input.discount_value,
        input.min_order_poisha,
        input.max_discount_poisha,
    )
    .map_err(ApiError::bad_request)?;
    if input.usage_limit.is_some_and(|limit| limit <= 0) {
        return Err(ApiError::bad_request("usage_limit must be positive"));
    }
    if input
        .starts_at
        .zip(input.ends_at)
        .is_some_and(|(starts_at, ends_at)| ends_at <= starts_at)
    {
        return Err(ApiError::bad_request("ends_at must be after starts_at"));
    }

    sqlx::query_as(&format!(
        "INSERT INTO coupons (code, store_id, discount_type, discount_value, \
         min_order_poisha, max_discount_poisha, starts_at, ends_at, usage_limit) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {COUPON_COLUMNS}"
    ))
    .bind(normalize_coupon_code(&input.code))
    .bind(store_id)
    .bind(&input.discount_type)
    .bind(input.discount_value)
    .bind(input.min_order_poisha)
    .bind(input.max_discount_poisha)
    .bind(input.starts_at)
    .bind(input.ends_at)
    .bind(input.usage_limit)
    .fetch_one(pool)
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
    sqlx::query_as(&format!(
        "UPDATE coupons SET active = $3 WHERE id = $1 \
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
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let coupon = create_coupon(pool, None, &input).await?;
    Ok((StatusCode::CREATED, Json(coupon)))
}

pub async fn list_admin_coupons_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListCouponsQuery>,
) -> Result<Json<CouponList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_coupons(pool, None, query.page, query.limit).await?,
    ))
}

pub async fn set_admin_coupon_active_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<SetCouponActiveInput>,
) -> Result<Json<Coupon>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(set_coupon_active(pool, id, None, input.active).await?))
}
