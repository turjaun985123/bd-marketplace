//! Product variants (Wave 33): a product gains purchasable
//! variants — sizes, colours, capacities — each with its own
//! price delta and its own stock.
//!
//! The model in one paragraph: a variant's price is never
//! stored outright; it is the product's base price plus the
//! variant's `price_delta_poisha` (the pure domain rule
//! `variant_final_price`), and the combination is refused
//! everywhere it could arise — variant create, variant
//! reprice, and a base-price PATCH that would strand an
//! existing delta — when the final price would not be
//! positive. While a product has at least one ACTIVE
//! variant, stock lives on the variants: the product view's
//! stock is their sum (see `products::PRODUCT_COLUMNS`),
//! checkout draws from the variant rows, and the product's
//! own `products.stock` is left untouched by every write
//! path. With no active variants, the product behaves
//! exactly as it did before this wave.
//!
//! Deactivation is the only delete: `DELETE` flips
//! `is_active` to false and returns the row. A variant is
//! never hard-deleted, so order items can always reference
//! the variant that was bought, and reactivation is a PATCH.
//!
//! A variant's starting stock (set at creation) writes NO
//! audit row — the same rule as product creation in Wave 23:
//! the history starts there. Every later movement goes
//! through the one audit writer (`inventory` module) with
//! the variant's id on the row: checkout decrements,
//! restocks, and the per-variant adjust-stock endpoint below
//! (the seller-reason vocabulary is the domain's, shared
//! with product adjust-stock).
//!
//! Access: variants are seller-private to manage. Every
//! endpoint here resolves the product through the caller's
//! own stores, and a product or variant that is missing OR
//! belongs to another seller is the same 404 — the no-leak
//! convention the inventory endpoints use.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    validate_stock_adjustment, validate_variant_delta, validate_variant_name, variant_final_price,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::inventory::{record_stock_adjustment, StockAdjustment, StockMovement};
use crate::products::{get_product_by_id, Product};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One variant row as stored, plus its computed final price.
/// The seller management shape: inactive variants included,
/// because managing them is the point of the surface.
#[derive(Debug, Clone, Serialize)]
pub struct ProductVariant {
    pub id: Uuid,
    pub product_id: Uuid,
    pub name: String,
    pub sku: Option<String>,
    pub price_delta_poisha: i64,
    /// Base price + delta, via the domain rule. The write
    /// paths keep this positive for every active variant; the
    /// field is computed at read time, never stored, so it
    /// can never drift from the base price.
    pub final_price_poisha: i64,
    pub stock: i32,
    pub is_active: bool,
    pub sort: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One variant as a buyer sees it on the product detail:
/// active variants only, with the final price ready to
/// render and the variant's own stock.
#[derive(Debug, Clone, Serialize)]
pub struct PublicVariant {
    pub id: Uuid,
    pub name: String,
    pub sku: Option<String>,
    pub price_delta_poisha: i64,
    pub final_price_poisha: i64,
    pub stock: i32,
}

/// What the variant adjust-stock endpoint returns: the
/// variant after the move and the audit row the move wrote
/// (the mirror of the product-level `AdjustStockResult`).
#[derive(Debug, Serialize)]
pub struct VariantAdjustStockResult {
    pub variant: ProductVariant,
    pub adjustment: StockAdjustment,
}

/// A variant row as stored (the final price is computed in
/// Rust from the product's base price — it is not a column).
#[derive(Debug, Clone, FromRow)]
struct VariantRow {
    id: Uuid,
    product_id: Uuid,
    name: String,
    sku: Option<String>,
    price_delta_poisha: i64,
    stock: i32,
    is_active: bool,
    sort: i32,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const VARIANT_COLUMNS: &str = "id, product_id, name, sku, price_delta_poisha, stock, \
    is_active, sort, created_at, updated_at";

impl VariantRow {
    /// Build the seller view: the row plus its final price
    /// against the product's CURRENT base price.
    fn into_view(self, base_price_poisha: i64) -> ProductVariant {
        let final_price_poisha = variant_final_price(base_price_poisha, self.price_delta_poisha)
            .unwrap_or_else(|| base_price_poisha.saturating_add(self.price_delta_poisha));
        ProductVariant {
            id: self.id,
            product_id: self.product_id,
            name: self.name,
            sku: self.sku,
            price_delta_poisha: self.price_delta_poisha,
            final_price_poisha,
            stock: self.stock,
            is_active: self.is_active,
            sort: self.sort,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Loaders + ownership
// ---------------------------------------------------------------------------

/// Resolve a product the caller may work for the variant
/// endpoints (Wave 34: the store's owner, or a member whose
/// role carries `manage_catalog` — every member role does):
/// missing and someone else's are both a 404 (see the module
/// doc).
async fn owned_product(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
) -> Result<Product, ApiError> {
    let product = get_product_by_id(pool, product_id).await?;
    match crate::access::store_role(pool, user.user_id, product.store_id).await? {
        Some(role) => {
            crate::access::require_role(&role, domain::STORE_ACTION_MANAGE_CATALOG)?;
        }
        None => return Err(ApiError::not_found("product not found")),
    }
    Ok(product)
}

/// Load one variant OF one product; a variant id that
/// belongs to a different product is a 404, exactly like a
/// missing one — variant ids must not leak across products.
async fn load_variant(
    pool: &PgPool,
    product_id: Uuid,
    variant_id: Uuid,
) -> Result<VariantRow, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {VARIANT_COLUMNS} FROM product_variants \
         WHERE id = $1 AND product_id = $2"
    ))
    .bind(variant_id)
    .bind(product_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load variant", e))?
    .ok_or_else(|| ApiError::not_found("variant not found"))
}

/// The ACTIVE variants of one product, in buyer-pick order
/// (sort, then name) — the product detail's picker data.
/// A variant whose final price somehow fails the domain
/// rule is omitted rather than shown unbuyable; the write
/// paths keep that from happening, so this is a belt on
/// top of braces.
pub async fn load_public_variants(
    pool: &PgPool,
    product: &Product,
) -> Result<Vec<PublicVariant>, ApiError> {
    let rows: Vec<VariantRow> = sqlx::query_as(&format!(
        "SELECT {VARIANT_COLUMNS} FROM product_variants \
         WHERE product_id = $1 AND is_active \
         ORDER BY sort ASC, name ASC, created_at ASC"
    ))
    .bind(product.id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load public variants", e))?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            variant_final_price(product.price_poisha, row.price_delta_poisha).map(|price| {
                PublicVariant {
                    id: row.id,
                    name: row.name,
                    sku: row.sku,
                    price_delta_poisha: row.price_delta_poisha,
                    final_price_poisha: price,
                    stock: row.stock,
                }
            })
        })
        .collect())
}

/// Whether the product currently has at least one ACTIVE
/// variant — the switch that decides whether stock and
/// prices live on the variants (cart add and checkout ask
/// this on their own connections too; this pool variant
/// serves read paths).
pub async fn product_has_active_variants(
    pool: &PgPool,
    product_id: Uuid,
) -> Result<bool, ApiError> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM product_variants \
         WHERE product_id = $1 AND is_active)",
    )
    .bind(product_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("check active variants", e))
}

// ---------------------------------------------------------------------------
// Service functions
// ---------------------------------------------------------------------------

/// Normalize a variant SKU for storage: trimmed, blank
/// becomes `None`, at most 80 characters.
fn normalize_sku(sku: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(trimmed) = sku.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if trimmed.chars().count() > 80 {
        return Err(ApiError::bad_request(
            "variant sku must be at most 80 characters",
        ));
    }
    Ok(Some(trimmed.to_string()))
}

/// Validate a variant stock figure from a payload: a
/// non-negative count that fits the stored int.
fn validate_variant_stock(stock: i64) -> Result<i32, ApiError> {
    if stock < 0 {
        return Err(ApiError::bad_request("variant stock must be >= 0"));
    }
    i32::try_from(stock).map_err(|_| ApiError::bad_request("variant stock is too large"))
}

/// The final-price gate shared by variant create and
/// reprice: the merged delta against the product's current
/// base price must leave a positive final price.
fn require_positive_final_price(base_price_poisha: i64, delta_poisha: i64) -> Result<i64, ApiError> {
    variant_final_price(base_price_poisha, delta_poisha).ok_or_else(|| {
        ApiError::bad_request(format!(
            "variant price must stay positive: base price {base_price_poisha} poisha \
             with a difference of {delta_poisha} poisha leaves no positive price"
        ))
    })
}

/// Create one variant on a product the caller owns. The
/// starting stock is the variant's creation stock — like
/// product creation it writes no audit row (Wave 23's rule:
/// the history starts there).
pub async fn create_variant_for_seller(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    input: CreateVariantInput,
) -> Result<ProductVariant, ApiError> {
    let product = owned_product(pool, user, product_id).await?;
    let name = validate_variant_name(&input.name).map_err(ApiError::bad_request)?;
    let delta = validate_variant_delta(input.price_delta_poisha.unwrap_or(0))
        .map_err(ApiError::bad_request)?;
    let sku = normalize_sku(input.sku.as_deref())?;
    let stock = validate_variant_stock(input.stock.unwrap_or(0))?;
    require_positive_final_price(product.price_poisha, delta)?;

    let row: VariantRow = sqlx::query_as(&format!(
        "INSERT INTO product_variants (product_id, name, sku, price_delta_poisha, stock, sort) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {VARIANT_COLUMNS}"
    ))
    .bind(product_id)
    .bind(&name)
    .bind(&sku)
    .bind(delta)
    .bind(stock)
    .bind(input.sort.unwrap_or(0))
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("create variant", e))?;
    Ok(row.into_view(product.price_poisha))
}

/// Every variant of a product the caller owns, inactive
/// included, in management order (sort, then name).
pub async fn list_variants_for_seller(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
) -> Result<Vec<ProductVariant>, ApiError> {
    let product = owned_product(pool, user, product_id).await?;
    let rows: Vec<VariantRow> = sqlx::query_as(&format!(
        "SELECT {VARIANT_COLUMNS} FROM product_variants \
         WHERE product_id = $1 ORDER BY sort ASC, name ASC, created_at ASC"
    ))
    .bind(product_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list variants", e))?;
    Ok(rows
        .into_iter()
        .map(|row| row.into_view(product.price_poisha))
        .collect())
}

/// Update one variant of a product the caller owns.
/// Provided fields override the stored ones; the merged
/// delta must still leave a positive final price against
/// the product's current base price. Like the product
/// PATCH, `sku` is set-only: a provided blank value leaves
/// the stored SKU untouched rather than clearing it.
pub async fn update_variant_for_seller(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    variant_id: Uuid,
    patch: UpdateVariantInput,
) -> Result<ProductVariant, ApiError> {
    let product = owned_product(pool, user, product_id).await?;
    let existing = load_variant(pool, product_id, variant_id).await?;

    let name = match &patch.name {
        Some(raw) => validate_variant_name(raw).map_err(ApiError::bad_request)?,
        None => existing.name.clone(),
    };
    let delta = match patch.price_delta_poisha {
        Some(raw) => validate_variant_delta(raw).map_err(ApiError::bad_request)?,
        None => existing.price_delta_poisha,
    };
    require_positive_final_price(product.price_poisha, delta)?;
    let sku = match normalize_sku(patch.sku.as_deref())? {
        Some(value) => Some(value),
        None => existing.sku.clone(),
    };

    let row: VariantRow = sqlx::query_as(&format!(
        "UPDATE product_variants SET name = $3, sku = $4, price_delta_poisha = $5, \
         is_active = $6, sort = $7, updated_at = now() \
         WHERE id = $1 AND product_id = $2 RETURNING {VARIANT_COLUMNS}"
    ))
    .bind(variant_id)
    .bind(product_id)
    .bind(&name)
    .bind(&sku)
    .bind(delta)
    .bind(patch.is_active.unwrap_or(existing.is_active))
    .bind(patch.sort.unwrap_or(existing.sort))
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("update variant", e))?;
    Ok(row.into_view(product.price_poisha))
}

/// Deactivate one variant (the DELETE verb): the row stays
/// — order history references it — with `is_active` false.
/// Idempotent: deactivating an inactive variant returns it
/// unchanged. Reactivation is a PATCH with `is_active: true`.
pub async fn deactivate_variant_for_seller(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    variant_id: Uuid,
) -> Result<ProductVariant, ApiError> {
    let product = owned_product(pool, user, product_id).await?;
    load_variant(pool, product_id, variant_id).await?;
    let row: VariantRow = sqlx::query_as(&format!(
        "UPDATE product_variants SET is_active = false, updated_at = now() \
         WHERE id = $1 AND product_id = $2 RETURNING {VARIANT_COLUMNS}"
    ))
    .bind(variant_id)
    .bind(product_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("deactivate variant", e))?;
    Ok(row.into_view(product.price_poisha))
}

/// Apply one manual stock adjustment to a VARIANT, for its
/// product's owner. The mirror of the product-level
/// adjust-stock: ONE transaction locks the variant row, the
/// domain validates the delta and reason, a move that would
/// drive the variant's stock negative is refused with a 400
/// and writes nothing, and the stock update plus its audit
/// row (carrying the variant id) commit together.
pub async fn adjust_variant_stock_for_seller(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    variant_id: Uuid,
    delta: i64,
    reason: &str,
    note: Option<&str>,
) -> Result<VariantAdjustStockResult, ApiError> {
    let reason = validate_stock_adjustment(delta, reason).map_err(ApiError::bad_request)?;
    let note = note.map(str::trim).filter(|trimmed| !trimmed.is_empty());
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
        .map_err(|e| ApiError::internal("begin variant adjust-stock transaction", e))?;

    let row: Option<LockedVariantRow> = sqlx::query_as(
        "SELECT v.stock, p.store_id, s.owner_user_id FROM product_variants v \
         JOIN products p ON p.id = v.product_id \
         JOIN stores s ON s.id = p.store_id \
         WHERE v.id = $1 AND v.product_id = $2 FOR UPDATE OF v",
    )
    .bind(variant_id)
    .bind(product_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock variant for adjust-stock", e))?;
    let row = row.ok_or_else(|| ApiError::not_found("variant not found"))?;
    // Wave 34: the owner, or a member with `manage_catalog`;
    // no relationship keeps this module's 404.
    if row.owner_user_id != user.user_id {
        match crate::access::store_role_on(&mut *tx, user.user_id, row.store_id).await? {
            Some(role) => {
                crate::access::require_role(&role, domain::STORE_ACTION_MANAGE_CATALOG)?;
            }
            None => return Err(ApiError::not_found("variant not found")),
        }
    }

    let new_stock = i64::from(row.stock) + delta;
    if new_stock < 0 {
        return Err(ApiError::bad_request(format!(
            "adjustment of {delta} would drive variant stock below zero (currently {})",
            row.stock
        )));
    }
    let new_stock_i32 = i32::try_from(new_stock)
        .map_err(|_| ApiError::bad_request("variant stock would exceed the maximum"))?;

    sqlx::query(
        "UPDATE product_variants SET stock = $2, updated_at = now() WHERE id = $1",
    )
    .bind(variant_id)
    .bind(new_stock_i32)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("apply variant stock adjustment", e))?;

    let adjustment = record_stock_adjustment(
        &mut tx,
        &StockMovement {
            product_id,
            store_id: row.store_id,
            actor: "seller",
            actor_user_id: Some(user.user_id),
            delta: delta_i32,
            new_stock: new_stock_i32,
            reason,
            note,
            variant_id: Some(variant_id),
        },
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit variant adjust-stock", e))?;

    let product = get_product_by_id(pool, product_id).await?;
    let variant = load_variant(pool, product_id, variant_id)
        .await?
        .into_view(product.price_poisha);
    Ok(VariantAdjustStockResult {
        variant,
        adjustment,
    })
}

/// A variant row locked for a stock operation, with its
/// product's store and owner — the ownership facts the
/// adjust-stock transaction checks before touching anything.
#[derive(FromRow)]
struct LockedVariantRow {
    stock: i32,
    store_id: Uuid,
    owner_user_id: Uuid,
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateVariantInput {
    pub name: String,
    pub sku: Option<String>,
    /// Price difference against the product's base price, in
    /// poisha; 0 when omitted (a same-price variant).
    pub price_delta_poisha: Option<i64>,
    /// Starting stock; 0 when omitted. Creation stock writes
    /// no audit row (see the module doc).
    pub stock: Option<i64>,
    pub sort: Option<i32>,
}

#[derive(Deserialize)]
pub struct UpdateVariantInput {
    pub name: Option<String>,
    pub sku: Option<String>,
    pub price_delta_poisha: Option<i64>,
    pub is_active: Option<bool>,
    pub sort: Option<i32>,
}

#[derive(Deserialize)]
pub struct AdjustVariantStockInput {
    pub delta: i64,
    pub reason: String,
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn create_variant_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(input): Json<CreateVariantInput>,
) -> Result<(StatusCode, Json<ProductVariant>), ApiError> {
    let pool = state.require_pool()?;
    let variant = create_variant_for_seller(pool, &user, product_id, input).await?;
    Ok((StatusCode::CREATED, Json(variant)))
}

pub async fn list_variants_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
) -> Result<Json<Vec<ProductVariant>>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_variants_for_seller(pool, &user, product_id).await?,
    ))
}

pub async fn update_variant_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path((product_id, variant_id)): Path<(Uuid, Uuid)>,
    Json(patch): Json<UpdateVariantInput>,
) -> Result<Json<ProductVariant>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        update_variant_for_seller(pool, &user, product_id, variant_id, patch).await?,
    ))
}

pub async fn deactivate_variant_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path((product_id, variant_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ProductVariant>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        deactivate_variant_for_seller(pool, &user, product_id, variant_id).await?,
    ))
}

pub async fn adjust_variant_stock_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path((product_id, variant_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<AdjustVariantStockInput>,
) -> Result<Json<VariantAdjustStockResult>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        adjust_variant_stock_for_seller(
            pool,
            &user,
            product_id,
            variant_id,
            input.delta,
            &input.reason,
            input.note.as_deref(),
        )
        .await?,
    ))
}
