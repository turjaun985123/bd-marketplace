//! Inventory management & the stock audit trail (Wave 23).
//!
//! The rule of this wave: after a product is created, its
//! `products.stock` NEVER changes without a `stock_adjustments`
//! row written in the same transaction (migration 0024). The
//! writers are exactly four:
//!
//! - checkout (`orders` module) — actor `system`, reason
//!   `order`, one row per ordered item with a negative delta;
//! - the shared restock helper (`orders::restock_sub_order_items`)
//!   — actor `system`, with the reason threaded in by the
//!   caller: `cancel_restock` for buyer and seller cancels,
//!   `return_restock` for a received buyer return, `rto_restock`
//!   for a courier-driven return;
//! - the seller adjust-stock endpoint below — actor `seller`,
//!   the seller's user id, and a reason from the domain
//!   vocabulary (`restock`, `correction`, `damage`,
//!   `count_fix`, `other`).
//!
//! Wave 33 adds the variant dimension: movements that touch
//! a VARIANT's stock (variant checkout lines, their restocks,
//! and the per-variant adjust-stock endpoint in the
//! `variants` module) write through the same single writer
//! with `variant_id` set and `new_stock` snapshotting the
//! VARIANT's count. A product with active variants refuses
//! product-level adjust-stock — its stock lives on the
//! variants, and the overview reads the effective (summed)
//! stock so health and alerts follow the same pool buyers
//! draw from.
//!
//! Product creation writes NO row: the history starts at the
//! creation stock. The product PATCH endpoint no longer accepts
//! a `stock` field at all (Wave 23 removed it from
//! `UpdateProductInput`), so there is no unaudited write path
//! left — a seller who wants to change stock uses adjust-stock
//! and leaves a reason behind.
//!
//! Each row snapshots `new_stock` (the on-hand count right
//! after the movement), so the trail alone replays a product's
//! stock history. Low-stock health is the pure domain rule
//! (`domain::stock_status`) over the product's own threshold
//! or the platform default when the threshold is NULL.
//!
//! Access: stock data is seller-private. Every endpoint here
//! resolves the product through the caller's own stores, and a
//! product that is missing OR belongs to another seller is the
//! same 404 — the no-leak convention the invoice endpoints use.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    stock_status, validate_low_stock_threshold, validate_stock_adjustment, LOW_STOCK_DEFAULT,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::products::{clamp_pagination, get_product_by_id, Product};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One stock movement, as stored in `stock_adjustments` and as
/// returned by the history endpoint.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct StockAdjustment {
    pub id: Uuid,
    pub product_id: Uuid,
    /// The variant whose stock moved (Wave 33), or null for
    /// a product-level movement. A variant movement still
    /// carries its product id, so the per-product history
    /// shows it in place, labelled by this id.
    pub variant_id: Option<Uuid>,
    pub store_id: Uuid,
    /// `seller` (a manual adjustment) or `system` (checkout,
    /// cancel, return and RTO movements).
    pub actor: String,
    /// The seller behind a manual adjustment; null for system
    /// movements, which have no human author.
    pub actor_user_id: Option<Uuid>,
    pub delta: i32,
    /// On-hand stock immediately after this movement.
    pub new_stock: i32,
    pub reason: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A page of stock movements plus the total count.
#[derive(Debug, Serialize)]
pub struct StockHistory {
    pub items: Vec<StockAdjustment>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

/// What the adjust-stock endpoint returns: the product after
/// the move (the shared seller product view) and the audit row
/// the move wrote.
#[derive(Debug, Serialize)]
pub struct AdjustStockResult {
    pub product: Product,
    pub adjustment: StockAdjustment,
}

/// One product in the seller inventory overview: its on-hand
/// count, its own threshold (null = platform default), the
/// effective threshold actually applied, and its health.
#[derive(Debug, Serialize)]
pub struct InventoryItem {
    pub product_id: Uuid,
    pub title: String,
    pub store_slug: String,
    pub stock: i32,
    pub low_stock_threshold: Option<i32>,
    pub effective_threshold: i64,
    /// `in_stock`, `low` or `out_of_stock` (domain rule).
    pub stock_status: String,
}

/// Counts over the seller's whole inventory: `low_stock`
/// counts only products in the `low` state; out-of-stock
/// products are their own, more urgent number.
#[derive(Debug, Serialize)]
pub struct InventorySummary {
    pub products: i64,
    pub out_of_stock: i64,
    pub low_stock: i64,
}

/// The inventory overview: every product of the seller's
/// stores with its stock health, plus the summary counts.
#[derive(Debug, Serialize)]
pub struct Inventory {
    pub items: Vec<InventoryItem>,
    pub summary: InventorySummary,
}

/// One low-stock alert row (dashboard): a product that needs
/// the seller's attention, worst first.
#[derive(Debug, Serialize)]
pub struct LowStockItem {
    pub product_id: Uuid,
    pub title: String,
    pub stock: i32,
    /// `low` or `out_of_stock` (domain rule).
    pub stock_status: String,
}

// ---------------------------------------------------------------------------
// The audit writer (shared with the orders / returns /
// shipments modules)
// ---------------------------------------------------------------------------

/// One stock movement to record. Callers build it at the exact
/// site where they update `products.stock`, inside the same
/// transaction, so the audit row and the stock change commit
/// or roll back together.
pub(crate) struct StockMovement<'a> {
    pub product_id: Uuid,
    /// The variant whose stock moved (Wave 33); `None` for a
    /// product-level movement. When set, `new_stock` is the
    /// VARIANT's on-hand count after the move — the trail
    /// replays whichever pool the movement touched.
    pub variant_id: Option<Uuid>,
    pub store_id: Uuid,
    /// `seller` or `system`.
    pub actor: &'a str,
    pub actor_user_id: Option<Uuid>,
    pub delta: i32,
    /// On-hand stock immediately after the movement.
    pub new_stock: i32,
    pub reason: &'a str,
    pub note: Option<&'a str>,
}

/// Insert one `stock_adjustments` row on the caller's
/// connection (always a transaction in practice) and return
/// it. THE writer for the audit trail: checkout, the restock
/// helper and the adjust-stock endpoint all record through
/// here, so a movement row is built in exactly one way.
pub(crate) async fn record_stock_adjustment(
    conn: &mut sqlx::PgConnection,
    movement: &StockMovement<'_>,
) -> Result<StockAdjustment, ApiError> {
    sqlx::query_as(
        "INSERT INTO stock_adjustments (product_id, variant_id, store_id, actor, \
         actor_user_id, delta, new_stock, reason, note) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         RETURNING id, product_id, variant_id, store_id, actor, actor_user_id, delta, \
         new_stock, reason, note, created_at",
    )
    .bind(movement.product_id)
    .bind(movement.variant_id)
    .bind(movement.store_id)
    .bind(movement.actor)
    .bind(movement.actor_user_id)
    .bind(movement.delta)
    .bind(movement.new_stock)
    .bind(movement.reason)
    .bind(movement.note)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("record stock adjustment", e))
}

// ---------------------------------------------------------------------------
// Service functions
// ---------------------------------------------------------------------------

/// A product row locked for a stock operation, with its
/// store's owner — the ownership fact every endpoint here
/// checks before touching anything.
#[derive(FromRow)]
struct LockedProductRow {
    store_id: Uuid,
    owner_user_id: Uuid,
    stock: i32,
}

/// Resolve a product the caller owns for the inventory
/// endpoints: missing and someone else's are both a 404 (see
/// the module doc). Runs on the pool — read-only.
async fn owned_product_row(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
) -> Result<(Uuid, i32), ApiError> {
    let row: Option<(Uuid, Uuid, i32)> = sqlx::query_as(
        "SELECT p.store_id, s.owner_user_id, p.stock FROM products p \
         JOIN stores s ON s.id = p.store_id WHERE p.id = $1",
    )
    .bind(product_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load product for inventory", e))?;
    let (store_id, owner_user_id, stock) =
        row.ok_or_else(|| ApiError::not_found("product not found"))?;
    if owner_user_id != user.user_id {
        return Err(ApiError::not_found("product not found"));
    }
    Ok((store_id, stock))
}

/// Apply one manual stock adjustment for the product's owner.
/// Everything happens in ONE transaction: the product row is
/// locked, the domain validates the delta and reason, a move
/// that would drive stock negative is refused with a 400 and
/// writes nothing, and the stock update plus its audit row
/// commit together.
pub async fn adjust_stock_for_seller(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    delta: i64,
    reason: &str,
    note: Option<&str>,
) -> Result<AdjustStockResult, ApiError> {
    let reason = validate_stock_adjustment(delta, reason).map_err(ApiError::bad_request)?;
    let note = note
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty());
    if note.is_some_and(|text| text.chars().count() > 500) {
        return Err(ApiError::bad_request(
            "stock adjustment note must be at most 500 characters",
        ));
    }
    let delta_i32 =
        i32::try_from(delta).map_err(|_| ApiError::bad_request("stock adjustment is too large"))?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin adjust-stock transaction", e))?;

    let row: Option<LockedProductRow> = sqlx::query_as(
        "SELECT p.store_id, s.owner_user_id, p.stock FROM products p \
         JOIN stores s ON s.id = p.store_id WHERE p.id = $1 FOR UPDATE OF p",
    )
    .bind(product_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock product for adjust-stock", e))?;
    let row = row.ok_or_else(|| ApiError::not_found("product not found"))?;
    if row.owner_user_id != user.user_id {
        return Err(ApiError::not_found("product not found"));
    }

    // Wave 33: while a product has ACTIVE variants, its stock
    // lives on the variants — a product-level adjustment would
    // write a pool nothing reads. Refuse and point at the
    // per-variant endpoint instead of silently no-oping.
    let has_active_variants: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM product_variants \
         WHERE product_id = $1 AND is_active)",
    )
    .bind(product_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("check variants for adjust-stock", e))?;
    if has_active_variants {
        return Err(ApiError::bad_request(
            "এই পণ্যের স্টক ভ্যারিয়েন্ট অনুযায়ী পরিচালিত হয় — প্রতিটি ভ্যারিয়েন্টের স্টক আলাদা করে ঠিক করুন \
             (this product's stock is managed per variant — adjust each variant's stock instead)",
        ));
    }

    let new_stock = i64::from(row.stock) + delta;
    if new_stock < 0 {
        return Err(ApiError::bad_request(format!(
            "adjustment of {delta} would drive stock below zero (currently {})",
            row.stock
        )));
    }
    let new_stock_i32 = i32::try_from(new_stock)
        .map_err(|_| ApiError::bad_request("stock would exceed the maximum"))?;

    sqlx::query("UPDATE products SET stock = $2, updated_at = now() WHERE id = $1")
        .bind(product_id)
        .bind(new_stock_i32)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("apply stock adjustment", e))?;

    let adjustment = record_stock_adjustment(
        &mut tx,
        &StockMovement {
            product_id,
            variant_id: None,
            store_id: row.store_id,
            actor: "seller",
            actor_user_id: Some(user.user_id),
            delta: delta_i32,
            new_stock: new_stock_i32,
            reason,
            note,
        },
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit adjust-stock", e))?;

    let product = get_product_by_id(pool, product_id).await?;
    Ok(AdjustStockResult {
        product,
        adjustment,
    })
}

/// One page of a product's stock history, newest first. The
/// product must be the caller's own (404 otherwise).
pub async fn list_stock_history(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    page: Option<i64>,
    per_page: Option<i64>,
) -> Result<StockHistory, ApiError> {
    owned_product_row(pool, user, product_id).await?;
    let (page, per_page) = clamp_pagination(page, per_page);
    let offset = (page - 1) * per_page;

    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM stock_adjustments WHERE product_id = $1")
            .bind(product_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("count stock history", e))?;

    let items: Vec<StockAdjustment> = sqlx::query_as(
        "SELECT id, product_id, variant_id, store_id, actor, actor_user_id, delta, \
         new_stock, reason, note, created_at \
         FROM stock_adjustments WHERE product_id = $1 \
         ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3",
    )
    .bind(product_id)
    .bind(per_page)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list stock history", e))?;

    Ok(StockHistory {
        items,
        total,
        page,
        per_page,
    })
}

/// Set (or clear, with null) a product's low-stock threshold.
/// Only the stored override changes — the effective threshold
/// and the health status are computed at read time.
pub async fn set_low_stock_threshold_for_seller(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    threshold: Option<i64>,
) -> Result<Product, ApiError> {
    let threshold = validate_low_stock_threshold(threshold).map_err(ApiError::bad_request)?;
    owned_product_row(pool, user, product_id).await?;
    let threshold_i32 = threshold.map(|value| value as i32);

    sqlx::query(
        "UPDATE products SET low_stock_threshold = $2, updated_at = now() WHERE id = $1",
    )
    .bind(product_id)
    .bind(threshold_i32)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("set low-stock threshold", e))?;

    get_product_by_id(pool, product_id).await
}

/// One inventory row before its health is computed.
#[derive(FromRow)]
struct InventoryRow {
    product_id: Uuid,
    title: String,
    store_slug: String,
    stock: i32,
    low_stock_threshold: Option<i32>,
}

/// Every product of the caller's stores with its stock
/// health, plus the summary counts — one query, statuses
/// computed by the domain rule in Rust so the API and the
/// dashboard alerts can never disagree.
pub async fn get_inventory(pool: &PgPool, user: &AuthUser) -> Result<Inventory, ApiError> {
    // Wave 33: the overview shows the EFFECTIVE stock (the
    // shared expression from the products module) — for a
    // variant product that is the sum of its variants'
    // stocks, which is the availability a buyer can draw on.
    let rows: Vec<InventoryRow> = sqlx::query_as(&format!(
        "SELECT p.id AS product_id, p.title, s.slug AS store_slug, \
         {EFFECTIVE_STOCK} AS stock, p.low_stock_threshold \
         FROM products p JOIN stores s ON s.id = p.store_id \
         WHERE s.owner_user_id = $1 \
         ORDER BY stock ASC, p.title ASC",
        EFFECTIVE_STOCK = crate::products::EFFECTIVE_STOCK_SQL
    ))
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load inventory", e))?;

    let mut out_of_stock: i64 = 0;
    let mut low_stock: i64 = 0;
    let items: Vec<InventoryItem> = rows
        .into_iter()
        .map(|row| {
            let threshold = row.low_stock_threshold.map(i64::from);
            let status = stock_status(i64::from(row.stock), threshold);
            match status {
                "out_of_stock" => out_of_stock += 1,
                "low" => low_stock += 1,
                _ => {}
            }
            InventoryItem {
                product_id: row.product_id,
                title: row.title,
                store_slug: row.store_slug,
                stock: row.stock,
                low_stock_threshold: row.low_stock_threshold,
                effective_threshold: threshold.unwrap_or(LOW_STOCK_DEFAULT),
                stock_status: status.to_string(),
            }
        })
        .collect();

    let summary = InventorySummary {
        products: items.len() as i64,
        out_of_stock,
        low_stock,
    };
    Ok(Inventory { items, summary })
}

/// Up to ten of the seller's products that need attention —
/// out-of-stock first, then the lowest counts — for the
/// dashboard alert strip. The SQL pre-filter uses the same
/// effective-threshold rule (own threshold, else the domain
/// default); the status label itself is computed by the
/// domain function, like everywhere else.
pub async fn low_stock_items_for_seller(
    pool: &PgPool,
    owner_user_id: Uuid,
) -> Result<Vec<LowStockItem>, ApiError> {
    // Wave 33: the alert filter runs on the EFFECTIVE stock
    // (see `get_inventory`) — a variant product whose variants
    // are nearly gone must alert on their sum, not on the
    // untouched product column.
    let rows: Vec<InventoryRow> = sqlx::query_as(&format!(
        "SELECT p.id AS product_id, p.title, s.slug AS store_slug, \
         {EFFECTIVE_STOCK} AS stock, p.low_stock_threshold \
         FROM products p JOIN stores s ON s.id = p.store_id \
         WHERE s.owner_user_id = $1 \
         AND ({EFFECTIVE_STOCK}) <= COALESCE(p.low_stock_threshold, $2) \
         ORDER BY (stock <= 0) DESC, stock ASC, p.created_at ASC \
         LIMIT 10",
        EFFECTIVE_STOCK = crate::products::EFFECTIVE_STOCK_SQL
    ))
    .bind(owner_user_id)
    .bind(LOW_STOCK_DEFAULT as i32)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load low-stock items", e))?;

    Ok(rows
        .into_iter()
        .map(|row| LowStockItem {
            product_id: row.product_id,
            title: row.title,
            stock: row.stock,
            stock_status: stock_status(
                i64::from(row.stock),
                row.low_stock_threshold.map(i64::from),
            )
            .to_string(),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AdjustStockInput {
    pub delta: i64,
    pub reason: String,
    pub note: Option<String>,
}

#[derive(Deserialize)]
pub struct SetLowStockThresholdInput {
    /// The new threshold, or null to clear the override back
    /// to the platform default.
    pub threshold: Option<i64>,
}

#[derive(Deserialize)]
pub struct StockHistoryQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    /// Alias for `per_page`, kept with the rest of the API's
    /// page/limit convention.
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn adjust_stock_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<AdjustStockInput>,
) -> Result<Json<AdjustStockResult>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        adjust_stock_for_seller(
            pool,
            &user,
            id,
            input.delta,
            &input.reason,
            input.note.as_deref(),
        )
        .await?,
    ))
}

pub async fn stock_history_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(query): Query<StockHistoryQuery>,
) -> Result<Json<StockHistory>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_stock_history(
            pool,
            &user,
            id,
            query.page,
            query.per_page.or(query.limit),
        )
        .await?,
    ))
}

pub async fn set_low_stock_threshold_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<SetLowStockThresholdInput>,
) -> Result<Json<Product>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        set_low_stock_threshold_for_seller(pool, &user, id, input.threshold).await?,
    ))
}

pub async fn inventory_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Inventory>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_inventory(pool, &user).await?))
}
