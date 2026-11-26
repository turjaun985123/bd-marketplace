//! Sponsored product promotions (Wave 26).
//!
//! The platform's one advertising product: a seller prepays a
//! whole number of days (1..=30, the domain cap) at the
//! platform's per-day price to sponsor ONE product. While the
//! promotion window is live the product carries `is_promoted`
//! in the shared product view, sorts first in the default and
//! relevance listings, and is labelled স্পন্সরড in the UI —
//! explicit price and rating sorts stay pure, so sponsorship
//! never disguises itself as a buyer's own ordering.
//!
//! Money rules, each enforced by exactly one function below:
//! - Price: the EFFECTIVE per-day price (`admin` module —
//!   platform setting, then the startup
//!   `PROMOTION_PRICE_PER_DAY_POISHA`, then the domain's
//!   100 taka a day). It is read at purchase time; a bought
//!   promotion keeps its cost snapshot and stored window, so
//!   a later price change never re-bills or extends it.
//! - Payment: from the seller's EARNINGS BALANCE only. The
//!   purchase runs in ONE transaction that locks the seller's
//!   payout-store row, reads the one shared earnings
//!   aggregate through `payouts::seller_earnings_on` (the same
//!   numbers the balance endpoint shows), refuses when the
//!   available balance cannot cover the cost, and writes the
//!   promotion plus its `seller_charges` row together — so a
//!   promotion is paid exactly once, and the charge then
//!   reduces the same available balance that gates payouts.
//!   There is no cash top-up: a seller who has not sold yet
//!   has no balance and cannot promote.
//! - Overlap: at most one live window per product. A second
//!   purchase while one is live (status `active`, window not
//!   yet ended) is a 409; buying again after expiry or
//!   cancellation is a fresh promotion.
//! - Cancellation: the seller may cancel a live promotion;
//!   placement stops immediately and the charge STANDS — no
//!   refund, ever. That policy is stated in the seller UI
//!   before purchase and in the README, so the cancel path
//!   here simply flips the status. Cancelling a cancelled or
//!   expired promotion is a 409.
//!
//! Activity rule: the domain's `promotion_is_active`
//! (status `active` and now inside [starts_at, ends_at)) is
//! the single definition. The SQL `is_promoted` flag in the
//! shared product columns implements it over the stored
//! timestamps with the database clock; the seller-facing
//! `display_status` here is derived through the domain
//! function itself over unix seconds, so the two can never
//! disagree about the rule — only about whose clock reads
//! it, by at most the request's own duration.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    promotion_cost_poisha, promotion_is_active, validate_promotion_days, PROMOTION_MAX_DAYS,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::admin::effective_promotion_price_per_day;
use crate::auth::AuthUser;
use crate::payouts::seller_earnings_on;
use crate::products::get_product_by_id;
use crate::stores::{get_store_by_id, get_store_by_owner};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One promotion row as stored (migration 0027).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Promotion {
    pub id: Uuid,
    pub product_id: Uuid,
    pub store_id: Uuid,
    pub seller_user_id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub days: i32,
    pub cost_poisha: i64,
    /// `active` or `cancelled` — expiry is not a status; it
    /// is read off the window (see `display_status`).
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// A promotion in the seller's own list: the stored row plus
/// the product title and the derived display status.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SellerPromotion {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product_title: String,
    pub store_id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub days: i32,
    pub cost_poisha: i64,
    pub status: String,
    /// Derived by the domain activity rule: `active` (live
    /// right now), `expired` (window over, never cancelled)
    /// or `cancelled`. Filled by `with_display_status` after
    /// the row loads — never stored, so it cannot drift.
    #[sqlx(default)]
    pub display_status: String,
    pub created_at: DateTime<Utc>,
}

/// The seller's promotion list envelope.
#[derive(Debug, Serialize)]
pub struct PromotionList {
    pub items: Vec<SellerPromotion>,
}

/// The public promotion policy: what a day of sponsorship
/// costs right now and the longest window for sale.
#[derive(Debug, Serialize)]
pub struct PromotionPolicy {
    pub price_per_day_poisha: i64,
    pub max_days: i64,
    /// All amounts are poisha — 100 poisha = 1 taka (BDT).
    pub currency_note: String,
}

/// The purchase receipt: the promotion as stored, what it
/// cost, and the seller's available balance right after the
/// charge — the same aggregate the balance endpoint reports.
#[derive(Debug, Serialize)]
pub struct PromoteResult {
    pub promotion: Promotion,
    pub cost_poisha: i64,
    pub available_after_poisha: i64,
}

/// Column list for a promotion row (matches [`Promotion`]).
const PROMOTION_COLUMNS: &str = "id, product_id, store_id, seller_user_id, \
    starts_at, ends_at, days, cost_poisha, status, created_at";

/// Column list for a seller-list row (matches
/// [`SellerPromotion`]; the title comes from the product join).
const SELLER_PROMOTION_COLUMNS: &str = "pr.id, pr.product_id, p.title AS product_title, \
    pr.store_id, pr.starts_at, pr.ends_at, pr.days, pr.cost_poisha, pr.status, \
    pr.created_at";

/// Fill a loaded row's `display_status` from the domain
/// activity rule. Every query site that materialises a
/// `SellerPromotion` runs its rows through here.
fn with_display_status(mut promotion: SellerPromotion) -> SellerPromotion {
    promotion.display_status = if promotion.status == "cancelled" {
        "cancelled".to_string()
    } else if promotion_is_active(
        promotion.starts_at.timestamp(),
        promotion.ends_at.timestamp(),
        &promotion.status,
        Utc::now().timestamp(),
    ) {
        "active".to_string()
    } else {
        "expired".to_string()
    };
    promotion
}

// ---------------------------------------------------------------------------
// Service functions
// ---------------------------------------------------------------------------

/// The promotion policy at the effective price.
pub async fn get_policy(state: &AppState) -> PromotionPolicy {
    PromotionPolicy {
        price_per_day_poisha: effective_promotion_price_per_day(state).await,
        max_days: PROMOTION_MAX_DAYS,
        currency_note: "All amounts are in poisha — 100 poisha = 1 taka (BDT).".to_string(),
    }
}

/// Buy a promotion for one of the caller's own ACTIVE
/// products, prepaid from the caller's earnings balance. See
/// the module doc for the money rules; the transaction below
/// is the whole enforcement.
pub async fn promote_product(
    pool: &PgPool,
    state: &AppState,
    user: &AuthUser,
    product_id: Uuid,
    days: i64,
) -> Result<PromoteResult, ApiError> {
    let days = validate_promotion_days(days).map_err(ApiError::bad_request)?;

    // Ownership and state gates, answered before any money
    // moves: someone else's product is a 404 (the platform's
    // no-leak convention for seller surfaces), and only an
    // active product may be sponsored — a draft has no public
    // placement to sell.
    //
    // Wave 34: the gate is the store's `manage_marketing`
    // role, not ownership alone — a manager may sponsor the
    // store's products. The MONEY still moves from the
    // CALLER's own earnings balance (the payout store below
    // is the caller's own, and the charge row names the
    // caller): a member's purchase is paid from the member's
    // own balance, never silently from the owner's. In
    // practice a member without earnings of their own cannot
    // complete a purchase — the balance gate below refuses.
    let product = get_product_by_id(pool, product_id).await?;
    let store = get_store_by_id(pool, product.store_id).await?;
    match crate::access::store_role(pool, user.user_id, store.id).await? {
        Some(role) => {
            crate::access::require_role(&role, domain::STORE_ACTION_MANAGE_MARKETING)?;
        }
        None => return Err(ApiError::not_found("product not found")),
    }
    if product.status != "active" {
        return Err(ApiError::bad_request(
            "only an active product can be promoted",
        ));
    }

    // The seller's money lives in the payout store (their
    // earliest store) — the store the balance endpoint and
    // payout creation read. Resolved before the transaction;
    // its row lock inside the transaction is what serialises
    // this seller's purchases against each other.
    let payout_store = get_store_by_owner(pool, user.user_id).await?;

    let price_per_day = effective_promotion_price_per_day(state).await;
    let cost = promotion_cost_poisha(days, price_per_day).ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "promotion cost could not be computed",
        )
    })?;
    let days_i32 =
        i32::try_from(days).map_err(|_| ApiError::bad_request("promotion days out of range"))?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin promote transaction", e))?;

    // Lock the payout store's row for the rest of the
    // transaction: two concurrent purchases by this seller
    // now queue instead of both reading the same available
    // balance and both charging against it.
    sqlx::query("SELECT id FROM stores WHERE id = $1 FOR UPDATE")
        .bind(payout_store.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("lock payout store for promotion", e))?;

    // One live window per product: an existing promotion
    // whose window has not ended blocks a second purchase.
    let overlapping: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM promotions \
         WHERE product_id = $1 AND status = 'active' AND ends_at > now())",
    )
    .bind(product_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("check live promotion", e))?;
    if overlapping {
        return Err(ApiError::conflict(
            "this product already has a live promotion",
        ));
    }

    // The balance gate, read through the ONE earnings
    // aggregate inside this transaction: the charge below
    // and this read commit or roll back together.
    let earnings = seller_earnings_on(&mut *tx, payout_store.id).await?;
    if earnings.available_poisha < cost {
        return Err(ApiError::bad_request(format!(
            "insufficient earnings balance: this promotion costs {cost} poisha \
             but only {} poisha is available",
            earnings.available_poisha
        )));
    }

    let promotion: Promotion = sqlx::query_as(&format!(
        "INSERT INTO promotions \
           (product_id, store_id, seller_user_id, starts_at, ends_at, days, cost_poisha) \
         VALUES ($1, $2, $3, now(), now() + make_interval(days => $4), $4, $5) \
         RETURNING {PROMOTION_COLUMNS}"
    ))
    .bind(product_id)
    .bind(product.store_id)
    .bind(user.user_id)
    .bind(days_i32)
    .bind(cost)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("create promotion", e))?;

    // The charge: the row that makes the earnings aggregate
    // subtract this purchase from the seller's balance from
    // now on — including after a cancellation (no refunds).
    sqlx::query(
        "INSERT INTO seller_charges (seller_user_id, amount_poisha, kind, ref_id) \
         VALUES ($1, $2, 'promotion', $3)",
    )
    .bind(user.user_id)
    .bind(cost)
    .bind(promotion.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("record promotion charge", e))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit promote transaction", e))?;

    Ok(PromoteResult {
        cost_poisha: cost,
        available_after_poisha: earnings.available_poisha - cost,
        promotion,
    })
}

/// The promotions of the stores the caller may do
/// marketing for (Wave 34: own stores plus member stores
/// with `manage_marketing`), newest first, each with its
/// product title and derived display status. Before this
/// wave the list was scoped by the buying user; for owners
/// the two scopes are the same set (only the owner could
/// buy, for their own stores).
pub async fn list_seller_promotions(
    pool: &PgPool,
    user: &AuthUser,
) -> Result<PromotionList, ApiError> {
    let store_ids = crate::access::accessible_store_ids(
        pool,
        user.user_id,
        domain::STORE_ACTION_MANAGE_MARKETING,
    )
    .await?;
    let items: Vec<SellerPromotion> = sqlx::query_as(&format!(
        "SELECT {SELLER_PROMOTION_COLUMNS} FROM promotions pr \
         JOIN products p ON p.id = pr.product_id \
         WHERE pr.store_id = ANY($1) ORDER BY pr.created_at DESC"
    ))
    .bind(&store_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list seller promotions", e))?
    .into_iter()
    .map(with_display_status)
    .collect();
    Ok(PromotionList { items })
}

/// Cancel one of the caller's live promotions. Placement
/// stops immediately; the charge stands (no refund — the
/// module doc states the policy). Cancelling a promotion
/// that is already cancelled or already expired is a 409,
/// and someone else's promotion is a 404.
pub async fn cancel_promotion(
    pool: &PgPool,
    user: &AuthUser,
    promotion_id: Uuid,
) -> Result<SellerPromotion, ApiError> {
    let row: Option<SellerPromotion> = sqlx::query_as(&format!(
        "SELECT {SELLER_PROMOTION_COLUMNS} FROM promotions pr \
         JOIN products p ON p.id = pr.product_id \
         WHERE pr.id = $1"
    ))
    .bind(promotion_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load promotion for cancel", e))?;
    let promotion = row
        .map(with_display_status)
        .ok_or_else(|| ApiError::not_found("promotion not found"))?;
    // Wave 34: cancelling is marketing work on the
    // promotion's store — the owner or a manager of THAT
    // store may cancel (whoever bought it); no relationship
    // keeps the 404 above indistinguishable.
    match crate::access::store_role(pool, user.user_id, promotion.store_id).await? {
        Some(role) => {
            crate::access::require_role(&role, domain::STORE_ACTION_MANAGE_MARKETING)?;
        }
        None => return Err(ApiError::not_found("promotion not found")),
    }
    if promotion.display_status != "active" {
        return Err(ApiError::conflict(format!(
            "only a live promotion can be cancelled (this one is {})",
            promotion.display_status
        )));
    }

    sqlx::query("UPDATE promotions SET status = 'cancelled' WHERE id = $1")
        .bind(promotion_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("cancel promotion", e))?;

    Ok(SellerPromotion {
        status: "cancelled".to_string(),
        display_status: "cancelled".to_string(),
        ..promotion
    })
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PromoteInput {
    pub days: i64,
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn policy_handler(State(state): State<AppState>) -> Json<PromotionPolicy> {
    Json(get_policy(&state).await)
}

pub async fn promote_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<PromoteInput>,
) -> Result<(StatusCode, Json<PromoteResult>), ApiError> {
    let pool = state.require_pool()?;
    let result = promote_product(pool, &state, &user, id, input.days).await?;
    Ok((StatusCode::CREATED, Json(result)))
}

pub async fn list_promotions_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<PromotionList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(list_seller_promotions(pool, &user).await?))
}

pub async fn cancel_promotion_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SellerPromotion>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(cancel_promotion(pool, &user, id).await?))
}
