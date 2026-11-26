//! Products. Every query is scoped by store (tenancy), and every seller
//! mutation checks store ownership (403 when the caller is not the
//! store's owner).
//!
//! Listing is ONE service function — `list_products` — shared by the
//! marketplace endpoint and the storefront endpoint; they differ only in
//! parameters, never in duplicated query logic. Public listings show only
//! `active` products, except that a signed-in owner also sees their own
//! store's non-active products (drafts/archived) so sellers can manage
//! their catalogue.
//!
//! Wave 8: the product view also carries the review rating summary
//! (`rating_avg` / `rating_count`), produced by the single aggregate
//! join `PRODUCT_RATING_JOIN` shared by every product query here.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{validate_product_input, ProductInput};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::{authenticate_optional, AuthUser};
use crate::stores::get_store_by_id;
use crate::{ApiError, AppState};

/// A product row joined with its store's slug; the public product shape.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Product {
    pub id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub title: String,
    pub title_bn: Option<String>,
    pub description: Option<String>,
    pub price_poisha: i64,
    pub compare_at_poisha: Option<i64>,
    pub stock: i32,
    pub status: String,
    /// Weight of one unit in grams (Wave 4 — drives delivery charges).
    pub weight_grams: i32,
    /// Mean review rating rounded to one decimal (Wave 8); null when
    /// the product has no reviews. Computed live by the aggregate in
    /// `PRODUCT_RATING_JOIN` — there is no cached counter to drift.
    pub rating_avg: Option<f64>,
    /// Number of reviews (Wave 8); 0 when there are none.
    pub rating_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const PRODUCT_COLUMNS: &str = "p.id, p.store_id, s.slug AS store_slug, p.title, p.title_bn, \
    p.description, p.price_poisha, p.compare_at_poisha, p.stock, p.status, \
    p.weight_grams, rv.rating_avg, rv.rating_count, p.created_at, p.updated_at";

/// Rating summary aggregate (Wave 8): THE one place ratings are
/// summarised. Every query that selects `PRODUCT_COLUMNS` joins this,
/// so the marketplace list, the storefront list and the product
/// detail can never disagree. The lateral subquery always yields one
/// row per product: `rating_avg` null and `rating_count` 0 when the
/// product has no reviews. The average is rounded in SQL to one
/// decimal and cast to float8 for JSON output.
const PRODUCT_RATING_JOIN: &str = "LEFT JOIN LATERAL (\
    SELECT CAST(ROUND(AVG(r.rating)::numeric, 1) AS float8) AS rating_avg, \
           COUNT(*)::bigint AS rating_count \
    FROM reviews r WHERE r.product_id = p.id\
    ) rv ON true";

/// Visibility + search predicate shared by the listing SELECT and COUNT
/// queries (static fragments only — all values stay bound parameters).
/// $1 = ILIKE pattern (or NULL), $2 = store slug (or NULL),
/// $3 = viewer user id (or NULL; owners additionally see own non-active).
/// Wave 10: a SUSPENDED store's products are hidden from everyone
/// except the store's owner (who still manages their catalogue).
const PRODUCT_WHERE: &str = "($1::text IS NULL OR p.title ILIKE $1 OR p.title_bn ILIKE $1) \
    AND ($2::text IS NULL OR s.slug = $2) \
    AND (p.status = 'active' OR s.owner_user_id = $3) \
    AND (s.status = 'active' OR s.owner_user_id = $3)";

/// One page of products plus the total match count.
#[derive(Debug, Serialize)]
pub struct ProductList {
    pub items: Vec<Product>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// Clamp pagination inputs to sane bounds (page >= 1, 1 <= limit <= 100).
/// Shared with the orders module — one pagination rule for the whole API.
pub(crate) fn clamp_pagination(page: Option<i64>, limit: Option<i64>) -> (i64, i64) {
    let page = page.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(20).clamp(1, 100);
    (page, limit)
}

/// THE listing function: marketplace search, storefront listing and any
/// future client all call this. `q` matches title / title_bn (ILIKE,
/// backed by the pg_trgm GIN indexes); `store_slug` restricts to one
/// store; `viewer_user_id` is the optional signed-in caller.
pub async fn list_products(
    pool: &PgPool,
    q: Option<&str>,
    store_slug: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
    viewer_user_id: Option<Uuid>,
) -> Result<ProductList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let pattern: Option<String> = q
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{s}%"));

    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM products p JOIN stores s ON s.id = p.store_id \
         WHERE {PRODUCT_WHERE}"
    ))
    .bind(&pattern)
    .bind(store_slug)
    .bind(viewer_user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count products", e))?;

    let items: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products p JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE {PRODUCT_WHERE} ORDER BY p.created_at DESC LIMIT $4 OFFSET $5"
    ))
    .bind(&pattern)
    .bind(store_slug)
    .bind(viewer_user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list products", e))?;

    Ok(ProductList {
        items,
        total,
        page,
        limit,
    })
}

/// Fetch one product by id (joined with its store slug).
pub async fn get_product_by_id(pool: &PgPool, id: Uuid) -> Result<Product, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products p JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE p.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load product", e))?
    .ok_or_else(|| ApiError::not_found("product not found"))
}

/// Assert the caller owns the store; 404 if the store does not exist,
/// 403 if it belongs to someone else.
async fn require_store_owner(pool: &PgPool, store_id: Uuid, user: &AuthUser) -> Result<(), ApiError> {
    let store = get_store_by_id(pool, store_id).await?;
    if store.owner_user_id != user.user_id {
        return Err(ApiError::forbidden("you do not own this store"));
    }
    Ok(())
}

/// Create a product in a store the caller owns. Input rules come from
/// the pure domain validator — the same function the update path uses.
pub async fn create_product_for_owner(
    pool: &PgPool,
    user: &AuthUser,
    input: CreateProductInput,
) -> Result<Product, ApiError> {
    require_store_owner(pool, input.store_id, user).await?;
    let validated = ProductInput {
        title: input.title.clone(),
        price_poisha: input.price_poisha,
        stock: input.stock,
        status: input.status.clone(),
        weight_grams: input.weight_grams,
    };
    validate_product_input(&validated).map_err(ApiError::bad_request)?;
    if let Some(compare_at) = input.compare_at_poisha {
        if compare_at < 0 {
            return Err(ApiError::bad_request("compare_at_poisha must be >= 0"));
        }
    }

    // The domain validator has already proven 1..=50,000, so this fits.
    let weight_grams = i32::try_from(validated.weight_grams)
        .map_err(|_| ApiError::bad_request("weight_grams out of range"))?;

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO products (store_id, title, title_bn, description, \
         price_poisha, compare_at_poisha, stock, status, weight_grams) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(input.store_id)
    .bind(input.title.trim())
    .bind(&input.title_bn)
    .bind(&input.description)
    .bind(input.price_poisha)
    .bind(input.compare_at_poisha)
    .bind(input.stock)
    .bind(&input.status)
    .bind(weight_grams)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("create product", e))?;

    get_product_by_id(pool, id).await
}

/// Update a product the caller owns. Provided fields override the stored
/// ones; the merged result must pass the same domain validator as create.
pub async fn update_product_for_owner(
    pool: &PgPool,
    user: &AuthUser,
    id: Uuid,
    patch: UpdateProductInput,
) -> Result<Product, ApiError> {
    let existing = get_product_by_id(pool, id).await?;
    require_store_owner(pool, existing.store_id, user).await?;

    let merged = ProductInput {
        title: patch.title.clone().unwrap_or(existing.title.clone()),
        price_poisha: patch.price_poisha.unwrap_or(existing.price_poisha),
        stock: patch.stock.unwrap_or(existing.stock),
        status: patch.status.clone().unwrap_or(existing.status.clone()),
        weight_grams: patch
            .weight_grams
            .unwrap_or(i64::from(existing.weight_grams)),
    };
    validate_product_input(&merged).map_err(ApiError::bad_request)?;
    // The domain validator has already proven 1..=50,000, so this fits.
    let merged_weight_grams = i32::try_from(merged.weight_grams)
        .map_err(|_| ApiError::bad_request("weight_grams out of range"))?;
    if let Some(compare_at) = patch.compare_at_poisha {
        if compare_at < 0 {
            return Err(ApiError::bad_request("compare_at_poisha must be >= 0"));
        }
    }

    sqlx::query(
        "UPDATE products SET title = $2, title_bn = $3, description = $4, \
         price_poisha = $5, compare_at_poisha = $6, stock = $7, status = $8, \
         weight_grams = $9, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(merged.title.trim())
    .bind(patch.title_bn.or(existing.title_bn))
    .bind(patch.description.or(existing.description))
    .bind(merged.price_poisha)
    .bind(patch.compare_at_poisha.or(existing.compare_at_poisha))
    .bind(merged.stock)
    .bind(&merged.status)
    .bind(merged_weight_grams)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("update product", e))?;

    get_product_by_id(pool, id).await
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateProductInput {
    pub store_id: Uuid,
    pub title: String,
    pub title_bn: Option<String>,
    pub description: Option<String>,
    pub price_poisha: i64,
    pub compare_at_poisha: Option<i64>,
    #[serde(default)]
    pub stock: i32,
    #[serde(default = "default_status")]
    pub status: String,
    /// Weight of one unit in grams; 1,000 when the payload omits it.
    #[serde(default = "default_weight_grams")]
    pub weight_grams: i64,
}

fn default_status() -> String {
    "draft".to_string()
}

fn default_weight_grams() -> i64 {
    domain::DEFAULT_WEIGHT_GRAMS
}

#[derive(Deserialize)]
pub struct UpdateProductInput {
    pub title: Option<String>,
    pub title_bn: Option<String>,
    pub description: Option<String>,
    pub price_poisha: Option<i64>,
    pub compare_at_poisha: Option<i64>,
    pub stock: Option<i32>,
    pub status: Option<String>,
    pub weight_grams: Option<i64>,
}

#[derive(Deserialize)]
pub struct ListProductsQuery {
    pub q: Option<String>,
    pub store_slug: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn create_product(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<CreateProductInput>,
) -> Result<(StatusCode, Json<Product>), ApiError> {
    let pool = state.require_pool()?;
    let product = create_product_for_owner(pool, &user, input).await?;
    Ok((StatusCode::CREATED, Json(product)))
}

pub async fn list_products_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListProductsQuery>,
) -> Result<Json<ProductList>, ApiError> {
    let pool = state.require_pool()?;
    let viewer = authenticate_optional(&state, &headers).map(|u| u.user_id);
    let list = list_products(
        pool,
        query.q.as_deref(),
        query.store_slug.as_deref(),
        query.page,
        query.limit,
        viewer,
    )
    .await?;
    Ok(Json(list))
}

pub async fn list_store_products(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(query): Query<ListProductsQuery>,
) -> Result<Json<ProductList>, ApiError> {
    let pool = state.require_pool()?;
    let viewer = authenticate_optional(&state, &headers).map(|u| u.user_id);
    let list = list_products(
        pool,
        query.q.as_deref(),
        Some(&slug),
        query.page,
        query.limit,
        viewer,
    )
    .await?;
    Ok(Json(list))
}

pub async fn get_product(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Product>, ApiError> {
    let pool = state.require_pool()?;
    let product = get_product_by_id(pool, id).await?;
    // Two visibility gates, both answered "404" so nothing leaks:
    // a non-active product is visible only to its store's owner,
    // and so is ANY product of a suspended store (Wave 10). The
    // owner always passes both gates (catalogue management).
    let store = get_store_by_id(pool, product.store_id).await?;
    let viewer = authenticate_optional(&state, &headers);
    let is_owner = viewer
        .map(|user| store.owner_user_id == user.user_id)
        .unwrap_or(false);
    if !is_owner && (product.status != "active" || store.status == "suspended") {
        return Err(ApiError::not_found("product not found"));
    }
    Ok(Json(product))
}

pub async fn update_product(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(patch): Json<UpdateProductInput>,
) -> Result<Json<Product>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(update_product_for_owner(pool, &user, id, patch).await?))
}
