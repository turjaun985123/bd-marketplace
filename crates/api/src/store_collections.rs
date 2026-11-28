//! Store collections (Wave 54 extension): a seller's own
//! curated product groups on their storefront — distinct
//! from the platform collections of Wave 52 (admin-curated
//! and marketplace-wide). A store collection belongs to
//! exactly one store, its slug is unique within that
//! store, and its membership is an explicit ORDERED list
//! the seller arranges (migration 0053).
//!
//! The rules, each enforced in exactly one place:
//! - Ownership: every seller write resolves the caller's
//!   OWN store first (the branding/followers convention)
//!   and the collection must belong to it — a collection
//!   of another store is a 403, like every seller
//!   mutation.
//! - Shape: title/slug/description pass the domain
//!   validators; the per-store collection cap and the
//!   per-collection product cap are the domain's counts.
//! - Membership is a SET the seller replaces wholesale:
//!   duplicates in the submitted list collapse (first
//!   occurrence keeps its position), every product must
//!   belong to the seller's store (a foreign id is a 400
//!   naming the rule, never a silent skip), and the
//!   replace happens in one transaction.
//! - Public reads show ACTIVE member products only, in
//!   the seller's position order; a draft stays a member
//!   (the seller still sees it) but is invisible to
//!   visitors until it publishes — the listing-visibility
//!   rule applied to a shelf.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::products::{Product, PRODUCT_COLUMNS, PRODUCT_RATING_JOIN};
use crate::stores::get_store_by_slug;
use crate::{ApiError, AppState};

/// One store collection: the row plus its member count
/// and cover image. `product_count` counts ACTIVE members
/// on public reads and ALL members on the seller's own
/// reads — each audience's honest number (what a visitor
/// can buy vs what the seller curated); the handlers
/// document which they serve. The cover is the first
/// member product's primary image (position order), the
/// same derivation the category tiles use.
#[derive(Debug, Serialize)]
pub struct StoreCollection {
    pub id: Uuid,
    pub store_id: Uuid,
    pub slug: String,
    pub title: String,
    pub description: Option<String>,
    pub product_count: i64,
    pub cover_image_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Flat row for a collection read: the collection columns
/// plus the count and the cover image's source columns.
#[derive(Debug, FromRow)]
struct StoreCollectionRow {
    id: Uuid,
    store_id: Uuid,
    slug: String,
    title: String,
    description: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    product_count: i64,
    img_storage_key: Option<String>,
    img_external_url: Option<String>,
    img_width: Option<i32>,
}

impl From<StoreCollectionRow> for StoreCollection {
    fn from(row: StoreCollectionRow) -> Self {
        StoreCollection {
            cover_image_url: crate::media::product_image_url(
                row.img_storage_key.as_deref(),
                row.img_external_url.as_deref(),
                row.img_width,
            ),
            id: row.id,
            store_id: row.store_id,
            slug: row.slug,
            title: row.title,
            description: row.description,
            product_count: row.product_count,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// The collection SELECT shared by every read: the count
/// is over members whose product passes the caller's
/// visibility (`$ACTIVE_ONLY` is chosen by the caller —
/// public reads count active products of the store,
/// seller reads count every member), and the cover
/// lateral picks the first VISIBLE member's primary
/// image in position order.
fn collection_select(active_only: bool) -> String {
    let count_filter = if active_only {
        " AND p.status = 'active'"
    } else {
        ""
    };
    format!(
        "SELECT c.id, c.store_id, c.slug, c.title, c.description, \
         c.created_at, c.updated_at, \
         (SELECT COUNT(*) FROM store_collection_products cp \
          JOIN products p ON p.id = cp.product_id \
          WHERE cp.collection_id = c.id{count_filter}) AS product_count, \
         img.storage_key AS img_storage_key, \
         img.external_url AS img_external_url, img.width AS img_width \
         FROM store_collections c \
         LEFT JOIN LATERAL ( \
             SELECT pi.storage_key, pi.external_url, pi.width \
             FROM store_collection_products cp \
             JOIN products p ON p.id = cp.product_id \
             JOIN product_images pi ON pi.product_id = p.id \
             WHERE cp.collection_id = c.id{count_filter} \
             ORDER BY cp.position ASC, pi.is_primary DESC, pi.position ASC \
             LIMIT 1 \
         ) img ON true"
    )
}

/// Is this database error a unique-violation (SQLSTATE
/// 23505)? The (store, slug) uniqueness surfaces as one;
/// the API translates it into a 409 instead of leaking a
/// 500 (the merch module's rule, copied deliberately).
fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(|db| db.code())
        .is_some_and(|code| code == "23505")
}

/// Load one collection of one store by id (seller reads).
/// Wave 58: `pub(crate)` — the storefront section
/// resolver loads a shelf's named collection through the
/// same read, so its counts and cover match this
/// module's surfaces exactly.
pub(crate) async fn load_collection(
    pool: &PgPool,
    store_id: Uuid,
    collection_id: Uuid,
    active_only: bool,
) -> Result<StoreCollection, ApiError> {
    let sql = format!(
        "{} WHERE c.store_id = $1 AND c.id = $2",
        collection_select(active_only)
    );
    sqlx::query_as::<_, StoreCollectionRow>(&sql)
        .bind(store_id)
        .bind(collection_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load store collection", e))?
        .map(StoreCollection::from)
        .ok_or_else(|| ApiError::not_found("collection not found"))
}

/// Load one collection of one store by slug.
async fn load_collection_by_slug(
    pool: &PgPool,
    store_id: Uuid,
    slug: &str,
    active_only: bool,
) -> Result<StoreCollection, ApiError> {
    let sql = format!(
        "{} WHERE c.store_id = $1 AND c.slug = $2",
        collection_select(active_only)
    );
    sqlx::query_as::<_, StoreCollectionRow>(&sql)
        .bind(store_id)
        .bind(slug)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load store collection by slug", e))?
        .map(StoreCollection::from)
        .ok_or_else(|| ApiError::not_found("collection not found"))
}

/// Every collection of one store, newest first.
/// Wave 58: `pub(crate)` — the storefront section
/// resolver renders the all-collections shelf from the
/// same read the public collections route serves.
pub(crate) async fn list_collections(
    pool: &PgPool,
    store_id: Uuid,
    active_only: bool,
) -> Result<Vec<StoreCollection>, ApiError> {
    let sql = format!(
        "{} WHERE c.store_id = $1 ORDER BY c.created_at DESC, c.id ASC",
        collection_select(active_only)
    );
    let rows: Vec<StoreCollectionRow> = sqlx::query_as(&sql)
        .bind(store_id)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("list store collections", e))?;
    Ok(rows.into_iter().map(StoreCollection::from).collect())
}

/// The collection list envelope.
#[derive(Debug, Serialize)]
pub struct StoreCollectionList {
    pub items: Vec<StoreCollection>,
    pub total: i64,
}

// ---------------------------------------------------------------------------
// Seller writes
// ---------------------------------------------------------------------------

/// The create/update payload: on create, slug + title are
/// required (the handler enforces); on update every field
/// is optional and merges (absent keeps, blank description
/// clears — the branding convention).
#[derive(Debug, Deserialize)]
pub struct StoreCollectionInput {
    pub slug: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
}

/// Create one collection for the caller's own store.
pub async fn create_collection(
    pool: &PgPool,
    user: &AuthUser,
    input: &StoreCollectionInput,
) -> Result<StoreCollection, ApiError> {
    let store = crate::stores::get_store_by_owner(pool, user.user_id).await?;
    let slug_raw = input.slug.as_deref().unwrap_or("");
    let slug = domain::validate_store_collection_slug(slug_raw).map_err(ApiError::bad_request)?;
    let title_raw = input.title.as_deref().unwrap_or("");
    let title = domain::validate_store_collection_title(title_raw).map_err(ApiError::bad_request)?;
    let description = domain::validate_store_collection_description(input.description.as_deref())
        .map_err(ApiError::bad_request)?;
    let current: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM store_collections WHERE store_id = $1")
            .bind(store.id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("count store collections", e))?;
    domain::validate_store_collection_count(current).map_err(ApiError::bad_request)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO store_collections (store_id, slug, title, description) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(store.id)
    .bind(&slug)
    .bind(&title)
    .bind(&description)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict(
                "এই স্লাগে এই স্টোরের একটি কালেকশন আগেই আছে — this store \
                 already has a collection with that slug",
            )
        } else {
            ApiError::internal("create store collection", e)
        }
    })?;
    load_collection(pool, store.id, id, false).await
}

/// Update one of the caller's own collections (merge
/// semantics — see [`StoreCollectionInput`]).
pub async fn update_collection(
    pool: &PgPool,
    user: &AuthUser,
    collection_id: Uuid,
    input: &StoreCollectionInput,
) -> Result<StoreCollection, ApiError> {
    let store = crate::stores::get_store_by_owner(pool, user.user_id).await?;
    let existing = load_collection(pool, store.id, collection_id, false).await?;
    let slug = match input.slug.as_deref() {
        Some(raw) => domain::validate_store_collection_slug(raw).map_err(ApiError::bad_request)?,
        None => existing.slug.clone(),
    };
    let title = match input.title.as_deref() {
        Some(raw) => {
            domain::validate_store_collection_title(raw).map_err(ApiError::bad_request)?
        }
        None => existing.title.clone(),
    };
    let description = match input.description.as_deref() {
        Some(raw) => domain::validate_store_collection_description(Some(raw))
            .map_err(ApiError::bad_request)?,
        None => existing.description.clone(),
    };
    sqlx::query(
        "UPDATE store_collections SET slug = $1, title = $2, description = $3, \
         updated_at = now() WHERE id = $4 AND store_id = $5",
    )
    .bind(&slug)
    .bind(&title)
    .bind(&description)
    .bind(collection_id)
    .bind(store.id)
    .execute(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict(
                "এই স্লাগে এই স্টোরের একটি কালেকশন আগেই আছে — this store \
                 already has a collection with that slug",
            )
        } else {
            ApiError::internal("update store collection", e)
        }
    })?;
    load_collection(pool, store.id, collection_id, false).await
}

/// Delete one of the caller's own collections; its
/// membership rows go with it (FK cascade), its products
/// are untouched.
pub async fn delete_collection(
    pool: &PgPool,
    user: &AuthUser,
    collection_id: Uuid,
) -> Result<(), ApiError> {
    let store = crate::stores::get_store_by_owner(pool, user.user_id).await?;
    let result = sqlx::query("DELETE FROM store_collections WHERE id = $1 AND store_id = $2")
        .bind(collection_id)
        .bind(store.id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("delete store collection", e))?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("collection not found"));
    }
    Ok(())
}

/// The membership replace payload.
#[derive(Debug, Deserialize)]
pub struct SetCollectionProductsInput {
    pub product_ids: Vec<Uuid>,
}

/// Replace one collection's membership with the submitted
/// list: duplicates collapse (first occurrence wins the
/// position), every id must be a product of the caller's
/// store, the deduped count passes the domain cap, and
/// the swap is one transaction — a refused replace
/// changes nothing.
pub async fn set_collection_products(
    pool: &PgPool,
    user: &AuthUser,
    collection_id: Uuid,
    input: &SetCollectionProductsInput,
) -> Result<StoreCollection, ApiError> {
    let store = crate::stores::get_store_by_owner(pool, user.user_id).await?;
    let _existing = load_collection(pool, store.id, collection_id, false).await?;

    let mut deduped: Vec<Uuid> = Vec::with_capacity(input.product_ids.len());
    for id in &input.product_ids {
        if !deduped.contains(id) {
            deduped.push(*id);
        }
    }
    domain::validate_collection_product_count(deduped.len() as i64)
        .map_err(ApiError::bad_request)?;
    if !deduped.is_empty() {
        let owned: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM products WHERE store_id = $1 AND id = ANY($2)",
        )
        .bind(store.id)
        .bind(&deduped)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("verify collection products", e))?;
        if owned != deduped.len() as i64 {
            return Err(ApiError::bad_request(
                "সব পণ্য এই স্টোরের হতে হবে — every product in a collection \
                 must belong to the store",
            ));
        }
    }

    let mut tx = pool.begin().await.map_err(|e| ApiError::internal("begin", e))?;
    sqlx::query("DELETE FROM store_collection_products WHERE collection_id = $1")
        .bind(collection_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("clear collection products", e))?;
    for (position, product_id) in deduped.iter().enumerate() {
        sqlx::query(
            "INSERT INTO store_collection_products (collection_id, product_id, position) \
             VALUES ($1, $2, $3)",
        )
        .bind(collection_id)
        .bind(product_id)
        .bind(position as i32)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("set collection products", e))?;
    }
    sqlx::query("UPDATE store_collections SET updated_at = now() WHERE id = $1")
        .bind(collection_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("touch store collection", e))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit", e))?;
    load_collection(pool, store.id, collection_id, false).await
}

/// The seller's detail view of one collection: the
/// collection plus its member product ids IN POSITION
/// ORDER — the membership editor's starting state (the
/// list views carry counts, not memberships).
#[derive(Debug, Serialize)]
pub struct SellerCollectionDetail {
    #[serde(flatten)]
    pub collection: StoreCollection,
    pub product_ids: Vec<Uuid>,
}

/// Load the seller detail of one of the caller's own
/// collections (404 when it is not theirs — the load is
/// scoped to their store).
pub async fn seller_collection_detail(
    pool: &PgPool,
    user: &AuthUser,
    collection_id: Uuid,
) -> Result<SellerCollectionDetail, ApiError> {
    let store = crate::stores::get_store_by_owner(pool, user.user_id).await?;
    let collection = load_collection(pool, store.id, collection_id, false).await?;
    let product_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT product_id FROM store_collection_products \
         WHERE collection_id = $1 ORDER BY position ASC, product_id ASC",
    )
    .bind(collection_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load collection membership", e))?;
    Ok(SellerCollectionDetail {
        collection,
        product_ids,
    })
}

// ---------------------------------------------------------------------------
// Public reads
// ---------------------------------------------------------------------------

/// One public collection page: the collection plus one
/// page of its ACTIVE member products in the seller's
/// position order (flash prices attached, like every
/// public product surface).
#[derive(Debug, Serialize)]
pub struct PublicStoreCollection {
    #[serde(flatten)]
    pub collection: StoreCollection,
    pub products: Vec<Product>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// The public collection page read: the store must be
/// active (a suspended store's shelves are hidden with
/// the store), the collection must be the store's, and
/// the products are its active members in position order.
pub async fn public_collection(
    pool: &PgPool,
    store_slug: &str,
    collection_slug: &str,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<PublicStoreCollection, ApiError> {
    let store = get_store_by_slug(pool, store_slug).await?;
    if store.status != "active" {
        return Err(ApiError::not_found("store not found"));
    }
    let collection = load_collection_by_slug(pool, store.id, collection_slug, true).await?;
    let (page, limit) = crate::products::clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_collection_products cp \
         JOIN products p ON p.id = cp.product_id \
         WHERE cp.collection_id = $1 AND p.status = 'active'",
    )
    .bind(collection.id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count collection products", e))?;
    let mut products: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM store_collection_products cp \
         JOIN products p ON p.id = cp.product_id \
         JOIN stores s ON s.id = p.store_id {PRODUCT_RATING_JOIN} \
         WHERE cp.collection_id = $1 AND p.status = 'active' \
         ORDER BY cp.position ASC, p.id ASC LIMIT $2 OFFSET $3"
    ))
    .bind(collection.id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list collection products", e))?
    .into_iter()
    .map(crate::products::with_stock_status)
    .collect();
    crate::merch::attach_flash_prices(pool, &mut products, Utc::now()).await?;
    Ok(PublicStoreCollection {
        collection,
        products,
        total,
        page,
        limit,
    })
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn list_seller_collections_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<StoreCollectionList>, ApiError> {
    let pool = state.require_pool()?;
    let store = crate::stores::get_store_by_owner(pool, user.user_id).await?;
    let items = list_collections(pool, store.id, false).await?;
    let total = items.len() as i64;
    Ok(Json(StoreCollectionList { items, total }))
}

pub async fn create_collection_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<StoreCollectionInput>,
) -> Result<(StatusCode, Json<StoreCollection>), ApiError> {
    let pool = state.require_pool()?;
    let created = create_collection(pool, &user, &input).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

pub async fn seller_collection_detail_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(collection_id): Path<Uuid>,
) -> Result<Json<SellerCollectionDetail>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        seller_collection_detail(pool, &user, collection_id).await?,
    ))
}

pub async fn update_collection_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(collection_id): Path<Uuid>,
    Json(input): Json<StoreCollectionInput>,
) -> Result<Json<StoreCollection>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(update_collection(pool, &user, collection_id, &input).await?))
}

pub async fn delete_collection_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(collection_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pool = state.require_pool()?;
    delete_collection(pool, &user, collection_id).await?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

pub async fn set_collection_products_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(collection_id): Path<Uuid>,
    Json(input): Json<SetCollectionProductsInput>,
) -> Result<Json<StoreCollection>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        set_collection_products(pool, &user, collection_id, &input).await?,
    ))
}

pub async fn list_public_collections_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<StoreCollectionList>, ApiError> {
    let pool = state.require_pool()?;
    let store = get_store_by_slug(pool, &slug).await?;
    if store.status != "active" {
        return Err(ApiError::not_found("store not found"));
    }
    let items = list_collections(pool, store.id, true).await?;
    let total = items.len() as i64;
    Ok(Json(StoreCollectionList { items, total }))
}

#[derive(Deserialize)]
pub struct PublicCollectionQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn public_collection_handler(
    State(state): State<AppState>,
    Path((slug, collection_slug)): Path<(String, String)>,
    Query(query): Query<PublicCollectionQuery>,
) -> Result<Json<PublicStoreCollection>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        public_collection(pool, &slug, &collection_slug, query.page, query.limit).await?,
    ))
}
