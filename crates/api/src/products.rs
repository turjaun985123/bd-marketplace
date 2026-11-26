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
//!
//! Wave 33: products gain variants (the `variants` module). The
//! view's `stock` becomes the EFFECTIVE stock, computed in
//! `PRODUCT_COLUMNS` itself: while a product has at least one
//! ACTIVE variant, availability lives on the variants and the
//! field is the SUM of the active variants' stocks (and
//! `stock_status` derives from that sum, through the same
//! `with_stock_status`); with no active variants it is the
//! stored `products.stock`, exactly as before. The stored
//! column is never overwritten with the sum — write paths that
//! need the raw column read it themselves.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{stock_status, validate_category, validate_product_input, ProductInput};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::{authenticate_optional, AuthUser};
use crate::stores::get_store_by_id;
use crate::variants::{load_public_variants, PublicVariant};
use crate::{ApiError, AppState};

/// Wave 32: the cache policy for public catalogue reads. Thirty
/// seconds is deliberately short — prices and stock change
/// without invalidating anything, so a shared cache may serve a
/// price that is at most half a minute stale; that tradeoff is
/// accepted for the catalogue (never for carts, orders or
/// balances) because it takes the hottest read traffic off
/// Postgres.
pub const PUBLIC_CACHE_CONTROL: &str = "public, max-age=30";

/// The response headers for a catalogue read: the public cache
/// header when the read is ANONYMOUS, and no cache header at
/// all when a viewer is signed in. The catalogue payload is
/// viewer-dependent (a signed-in owner also sees their own
/// non-active products), so an authenticated response must
/// never land in a shared cache — the rule is "no token, no
/// public cache entry", enforced here at the handler, not by
/// path-matching middleware.
pub fn public_cache_headers(anonymous: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if anonymous {
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static(PUBLIC_CACHE_CONTROL),
        );
    }
    headers
}

/// A product row joined with its store's slug; the public product shape.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Product {
    pub id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub title: String,
    pub title_bn: Option<String>,
    pub description: Option<String>,
    /// Free-text category label (Wave 15); null when the seller
    /// did not set one. Search filters on it (case-insensitive
    /// exact match).
    pub category: Option<String>,
    pub price_poisha: i64,
    pub compare_at_poisha: Option<i64>,
    /// The EFFECTIVE stock (Wave 33): the sum of the active
    /// variants' stocks while the product has active variants,
    /// otherwise the stored `products.stock`. Computed by
    /// `PRODUCT_COLUMNS` — see the module doc.
    pub stock: i32,
    /// Whether the product has at least one ACTIVE variant
    /// (Wave 33). List views carry only this flag (never the
    /// variant rows — payload size); the detail view adds the
    /// variants themselves (see `ProductDetail`).
    pub has_variants: bool,
    /// The seller's per-product low-stock warning level
    /// (Wave 23); null means the platform default applies.
    pub low_stock_threshold: Option<i32>,
    /// Stock health from the domain rule (`in_stock`, `low`,
    /// `out_of_stock`), computed in Rust by
    /// `with_stock_status` after the row loads — it is not a
    /// stored column, so it can never drift from the rule.
    #[sqlx(default)]
    pub stock_status: String,
    pub status: String,
    /// Weight of one unit in grams (Wave 4 — drives delivery charges).
    pub weight_grams: i32,
    /// Where the product came from (Wave 12): `manual` for the
    /// seller form, `facebook` for a post import.
    pub source: String,
    /// The origin's own id (Wave 12): for Facebook imports, the
    /// post id the seller supplied — the key that makes
    /// re-importing the same post a no-op. Null for manual rows.
    pub source_ref: Option<String>,
    /// Mean review rating rounded to one decimal (Wave 8); null when
    /// the product has no reviews. Computed live by the aggregate in
    /// `PRODUCT_RATING_JOIN` — there is no cached counter to drift.
    pub rating_avg: Option<f64>,
    /// Number of reviews (Wave 8); 0 when there are none.
    pub rating_count: i64,
    /// Whether the product is inside a live sponsored-promotion
    /// window right now (Wave 26). Computed by the EXISTS in
    /// `PRODUCT_COLUMNS` — the same rule as the domain's
    /// `promotion_is_active` (status still `active`, now inside
    /// [starts_at, ends_at)) evaluated by the database clock, so
    /// every product surface agrees and the flag can never be a
    /// stale stored value.
    pub is_promoted: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The effective-stock expression (Wave 33), aliased `stock`
/// in `PRODUCT_COLUMNS`: the SUM of the active variants'
/// stocks (cast to int — the variants' own column is int)
/// while the product has at least one active variant, else
/// the stored `products.stock`. The inventory overview in
/// the `inventory` module runs the same expression over its
/// own `p` alias; this string is the canonical text.
pub(crate) const EFFECTIVE_STOCK_SQL: &str = "CASE WHEN EXISTS (\
    SELECT 1 FROM product_variants pv \
    WHERE pv.product_id = p.id AND pv.is_active) \
    THEN (SELECT COALESCE(SUM(pv2.stock), 0)::int FROM product_variants pv2 \
          WHERE pv2.product_id = p.id AND pv2.is_active) \
    ELSE p.stock END";

pub(crate) const PRODUCT_COLUMNS: &str = "p.id, p.store_id, s.slug AS store_slug, p.title, p.title_bn, \
    p.description, p.category, p.price_poisha, p.compare_at_poisha, \
    CASE WHEN EXISTS (SELECT 1 FROM product_variants pv \
                      WHERE pv.product_id = p.id AND pv.is_active) \
         THEN (SELECT COALESCE(SUM(pv2.stock), 0)::int FROM product_variants pv2 \
               WHERE pv2.product_id = p.id AND pv2.is_active) \
         ELSE p.stock END AS stock, \
    EXISTS (SELECT 1 FROM product_variants pv \
            WHERE pv.product_id = p.id AND pv.is_active) AS has_variants, \
    p.low_stock_threshold, p.status, \
    p.weight_grams, p.source, p.source_ref, rv.rating_avg, rv.rating_count, \
    p.created_at, p.updated_at, \
    EXISTS (SELECT 1 FROM promotions pr \
            WHERE pr.product_id = p.id AND pr.status = 'active' \
              AND pr.starts_at <= now() AND pr.ends_at > now()) AS is_promoted";

/// The product DETAIL shape (Wave 33): the shared product
/// view flattened, plus the ACTIVE variants a buyer can pick
/// (ordered by sort, then name — see the `variants` module).
/// Only the detail endpoint builds this; list views carry
/// `has_variants` and never the variant rows.
#[derive(Debug, Serialize)]
pub struct ProductDetail {
    #[serde(flatten)]
    pub product: Product,
    pub variants: Vec<PublicVariant>,
}

/// Fill a loaded product's computed `stock_status` from the
/// domain rule (Wave 23). Every query site that materialises
/// a `Product` runs its rows through here, so the field is
/// always the rule's answer for the row's stock and threshold.
pub(crate) fn with_stock_status(mut product: Product) -> Product {
    product.stock_status = stock_status(
        i64::from(product.stock),
        product.low_stock_threshold.map(i64::from),
    )
    .to_string();
    product
}

/// Rating summary aggregate (Wave 8): THE one place ratings are
/// summarised. Every query that selects `PRODUCT_COLUMNS` joins this,
/// so the marketplace list, the storefront list and the product
/// detail can never disagree. The lateral subquery always yields one
/// row per product: `rating_avg` null and `rating_count` 0 when the
/// product has no reviews. The average is rounded in SQL to one
/// decimal and cast to float8 for JSON output.
pub(crate) const PRODUCT_RATING_JOIN: &str = "LEFT JOIN LATERAL (\
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

    // Wave 26: this listing's only ordering is the default
    // (newest first), so live sponsored promotions sort ahead
    // of it — the same promoted-first rule search applies to
    // its default and relevance orderings. The flag is an
    // output column of PRODUCT_COLUMNS, so ORDER BY can name
    // it directly. Sponsored placement is always labelled in
    // the UI (the স্পন্সরড badge), never disguised as organic.
    let items: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products p JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE {PRODUCT_WHERE} ORDER BY is_promoted DESC, p.created_at DESC LIMIT $4 OFFSET $5"
    ))
    .bind(&pattern)
    .bind(store_slug)
    .bind(viewer_user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list products", e))?
    .into_iter()
    .map(with_stock_status)
    .collect();

    Ok(ProductList {
        items,
        total,
        page,
        limit,
    })
}

/// Fetch one product by id (joined with its store slug).
pub async fn get_product_by_id(pool: &PgPool, id: Uuid) -> Result<Product, ApiError> {
    let product = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products p JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE p.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load product", e))?
    .ok_or_else(|| ApiError::not_found("product not found"))?;
    Ok(with_stock_status(product))
}

/// Fetch one product under the PUBLIC visibility rule — the
/// stricter rule the discovery surfaces use (Wave 15 search,
/// Wave 24 wishlist / recommendations): the product must be
/// `active` and its store must be `active`, with no owner
/// exception. Anything else — missing, draft, archived, or a
/// suspended store's product — is the same 404, so a caller
/// cannot probe for hidden products through these endpoints.
pub async fn get_public_product(pool: &PgPool, id: Uuid) -> Result<Product, ApiError> {
    let product = get_product_by_id(pool, id).await?;
    if product.status != "active" {
        return Err(ApiError::not_found("product not found"));
    }
    let store = get_store_by_id(pool, product.store_id).await?;
    if store.status != "active" {
        return Err(ApiError::not_found("product not found"));
    }
    Ok(product)
}

/// Assert the caller may work the store's catalogue (Wave
/// 34): the owner, or a member whose role carries
/// `manage_catalog` — which every member role does, so in
/// practice this separates the store's team from strangers.
/// 404 if the store does not exist, 403 if the caller has no
/// relationship with it (this module's historical answer).
async fn require_store_catalog_access(
    pool: &PgPool,
    store_id: Uuid,
    user: &AuthUser,
) -> Result<(), ApiError> {
    let _store = get_store_by_id(pool, store_id).await?;
    match crate::access::store_role(pool, user.user_id, store_id).await? {
        Some(role) => crate::access::require_role(&role, domain::STORE_ACTION_MANAGE_CATALOG),
        None => Err(ApiError::forbidden("you do not own this store")),
    }
}

/// Every field of a new product row, including its provenance
/// (Wave 12): `source` is `manual` for the seller form or
/// `facebook` for a post import, and `source_ref` is the origin's
/// own id (for Facebook, the post id the seller supplied) or null.
#[derive(Debug, Clone)]
pub struct NewProduct {
    pub store_id: Uuid,
    pub title: String,
    pub title_bn: Option<String>,
    pub description: Option<String>,
    /// Category label (Wave 15); the Facebook import passes None.
    pub category: Option<String>,
    pub price_poisha: i64,
    pub compare_at_poisha: Option<i64>,
    pub stock: i32,
    pub status: String,
    pub weight_grams: i32,
    pub source: String,
    pub source_ref: Option<String>,
}

/// Insert one product row and return its id. THE insert path: the
/// create endpoint and the Facebook import both write product rows
/// through here, so a row is built in exactly one way. Takes any
/// sqlx executor — the pool for a single create, a transaction for
/// the import batch. Callers validate first (the domain validator
/// plus their own rules); this function only writes.
pub async fn insert_product<'e, E>(executor: E, input: &NewProduct) -> Result<Uuid, ApiError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_scalar(
        "INSERT INTO products (store_id, title, title_bn, description, category, \
         price_poisha, compare_at_poisha, stock, status, weight_grams, \
         source, source_ref) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
    )
    .bind(input.store_id)
    .bind(input.title.trim())
    .bind(&input.title_bn)
    .bind(&input.description)
    .bind(&input.category)
    .bind(input.price_poisha)
    .bind(input.compare_at_poisha)
    .bind(input.stock)
    .bind(&input.status)
    .bind(input.weight_grams)
    .bind(&input.source)
    .bind(&input.source_ref)
    .fetch_one(executor)
    .await
    .map_err(|e| ApiError::internal("create product", e))
}

/// Normalize a category label for storage: trimmed, with a blank
/// label becoming `None` (uncategorised). Validation is the domain
/// crate's `validate_category`, called by both write paths before
/// this helper stores anything.
fn normalize_category(category: Option<&str>) -> Option<String> {
    category
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_string)
}

/// Create a product in a store the caller owns. Input rules come from
/// the pure domain validator — the same function the update path uses.
///
/// Wave 36: the store's subscription plan caps its catalogue.
/// The count is EVERY product row the store holds — products
/// are never hard-deleted (the lifecycle is draft / active /
/// archived), so each row keeps its slot until... forever;
/// archiving does not free a slot. The cap itself is the live
/// plan's (see the `subscriptions` module): the free cap is
/// tunable, the paid caps are part of what the plans sell.
/// The check is a count-then-insert on the pool, so two
/// creates racing in the same instant can overshoot the cap
/// by the number in flight — the cap is a plan boundary, not
/// a money rule, and the overshoot is bounded by concurrency.
pub async fn create_product_for_owner(
    state: &AppState,
    pool: &PgPool,
    user: &AuthUser,
    input: CreateProductInput,
) -> Result<Product, ApiError> {
    require_store_catalog_access(pool, input.store_id, user).await?;
    let validated = ProductInput {
        title: input.title.clone(),
        price_poisha: input.price_poisha,
        stock: input.stock,
        status: input.status.clone(),
        weight_grams: input.weight_grams,
    };
    validate_product_input(&validated).map_err(ApiError::bad_request)?;
    validate_category(input.category.as_deref()).map_err(ApiError::bad_request)?;
    if let Some(compare_at) = input.compare_at_poisha {
        if compare_at < 0 {
            return Err(ApiError::bad_request("compare_at_poisha must be >= 0"));
        }
    }

    // The domain validator has already proven 1..=50,000, so this fits.
    let weight_grams = i32::try_from(validated.weight_grams)
        .map_err(|_| ApiError::bad_request("weight_grams out of range"))?;

    // Wave 36: the plan cap gate (see the function doc). At
    // the cap the create is refused with the plan's numbers
    // named, in both languages, so the seller hears the one
    // thing that fixes it: upgrade the store's plan.
    let cap = crate::subscriptions::product_cap_for_store(state, input.store_id).await?;
    let product_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE store_id = $1")
            .bind(input.store_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("count store products for plan cap", e))?;
    if product_count >= cap {
        return Err(ApiError::forbidden(format!(
            "your store's current plan allows at most {cap} products — \
             upgrade the store's subscription to add more — \
             আপনার স্টোরের বর্তমান প্ল্যানে সর্বোচ্চ {cap}টি পণ্য রাখা যাবে, \
             আরও যোগ করতে সাবস্ক্রিপশন আপগ্রেড করুন"
        )));
    }

    let id = insert_product(
        pool,
        &NewProduct {
            store_id: input.store_id,
            title: input.title.clone(),
            title_bn: input.title_bn.clone(),
            description: input.description.clone(),
            category: normalize_category(input.category.as_deref()),
            price_poisha: input.price_poisha,
            compare_at_poisha: input.compare_at_poisha,
            stock: input.stock,
            status: input.status.clone(),
            weight_grams,
            source: "manual".to_string(),
            source_ref: None,
        },
    )
    .await?;

    get_product_by_id(pool, id).await
}

/// Update a product the caller owns. Provided fields override the stored
/// ones; the merged result must pass the same domain validator as create.
///
/// Wave 23: the PATCH payload no longer carries `stock`. Stock
/// only ever moves through the audited paths (checkout, the
/// restock helper, and the adjust-stock endpoint in the
/// `inventory` module), each writing its `stock_adjustments`
/// row in the same transaction; a silent PATCH write would be
/// an unaudited hole in that trail. The merged validator input
/// therefore always carries the stored stock, unchanged.
pub async fn update_product_for_owner(
    pool: &PgPool,
    user: &AuthUser,
    id: Uuid,
    patch: UpdateProductInput,
) -> Result<Product, ApiError> {
    let existing = get_product_by_id(pool, id).await?;
    require_store_catalog_access(pool, existing.store_id, user).await?;

    let merged = ProductInput {
        title: patch.title.clone().unwrap_or(existing.title.clone()),
        price_poisha: patch.price_poisha.unwrap_or(existing.price_poisha),
        stock: existing.stock,
        status: patch.status.clone().unwrap_or(existing.status.clone()),
        weight_grams: patch
            .weight_grams
            .unwrap_or(i64::from(existing.weight_grams)),
    };
    validate_product_input(&merged).map_err(ApiError::bad_request)?;
    validate_category(patch.category.as_deref()).map_err(ApiError::bad_request)?;
    // The domain validator has already proven 1..=50,000, so this fits.
    let merged_weight_grams = i32::try_from(merged.weight_grams)
        .map_err(|_| ApiError::bad_request("weight_grams out of range"))?;
    if let Some(compare_at) = patch.compare_at_poisha {
        if compare_at < 0 {
            return Err(ApiError::bad_request("compare_at_poisha must be >= 0"));
        }
    }
    // Like the other optional text fields, a PATCH can set the
    // category but not clear it back to NULL.
    let merged_category = normalize_category(patch.category.as_deref()).or(existing.category);

    // Wave 33: a price change must not strand a variant at a
    // zero or negative final price (base + delta). The variant
    // write paths validate their own delta against the current
    // base; this is the mirror guard for the base moving under
    // existing deltas.
    if merged.price_poisha != existing.price_poisha {
        let stranded: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM product_variants \
             WHERE product_id = $1 AND is_active AND $2 + price_delta_poisha <= 0",
        )
        .bind(id)
        .bind(merged.price_poisha)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("check variant prices for product update", e))?;
        if stranded > 0 {
            return Err(ApiError::bad_request(format!(
                "the new price would leave {stranded} variant price(s) at zero or below — \
                 adjust the variant price differences first"
            )));
        }
    }

    // Wave 33: the UPDATE no longer writes `stock` at all.
    // Since Wave 23 the merged value was the stored stock
    // passed through unchanged (PATCH cannot move stock), but
    // the loaded view's stock is now the EFFECTIVE stock (the
    // variant sum for variant products) — writing it back
    // would corrupt the stored fallback. Stock moves only
    // through the audited paths, so the column is simply not
    // this statement's business.
    sqlx::query(
        "UPDATE products SET title = $2, title_bn = $3, description = $4, \
         price_poisha = $5, compare_at_poisha = $6, status = $7, \
         weight_grams = $8, category = $9, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(merged.title.trim())
    .bind(patch.title_bn.or(existing.title_bn))
    .bind(patch.description.or(existing.description))
    .bind(merged.price_poisha)
    .bind(patch.compare_at_poisha.or(existing.compare_at_poisha))
    .bind(&merged.status)
    .bind(merged_weight_grams)
    .bind(&merged_category)
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
    /// Optional category label (Wave 15), validated by the domain.
    pub category: Option<String>,
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
    /// Optional category label (Wave 15); set-only, like title_bn.
    pub category: Option<String>,
    pub price_poisha: Option<i64>,
    pub compare_at_poisha: Option<i64>,
    /// No `stock` field (Wave 23): stock changes go through
    /// the audited adjust-stock endpoint, never this PATCH.
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
    let product = create_product_for_owner(&state, pool, &user, input).await?;
    Ok((StatusCode::CREATED, Json(product)))
}

pub async fn list_products_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListProductsQuery>,
) -> Result<(HeaderMap, Json<ProductList>), ApiError> {
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
    // Wave 32: publicly cacheable only for anonymous reads
    // (see `public_cache_headers`).
    Ok((public_cache_headers(viewer.is_none()), Json(list)))
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
) -> Result<(HeaderMap, Json<ProductDetail>), ApiError> {
    let pool = state.require_pool()?;
    let product = get_product_by_id(pool, id).await?;
    // Two visibility gates, both answered "404" so nothing leaks:
    // a non-active product is visible only to its store's owner,
    // and so is ANY product of a suspended store (Wave 10). The
    // owner always passes both gates (catalogue management).
    let store = get_store_by_id(pool, product.store_id).await?;
    let viewer = authenticate_optional(&state, &headers);
    // Wave 34: the "insider" who may see a non-active product
    // (or any product of a suspended store) is the store's
    // team — the owner or any member — not the owner alone:
    // staff manage the catalogue, drafts included.
    let is_owner = match viewer.as_ref() {
        Some(user) => crate::access::store_role(pool, user.user_id, store.id)
            .await?
            .is_some(),
        None => false,
    };
    if !is_owner && (product.status != "active" || store.status == "suspended") {
        return Err(ApiError::not_found("product not found"));
    }
    // Wave 33: the detail view carries the ACTIVE variants
    // (the list views never do — see `ProductDetail`).
    let variants = load_public_variants(pool, &product).await?;
    let detail = ProductDetail { product, variants };
    // Wave 32: an anonymous read that passed the gates is by
    // definition the public view, so it may be cached publicly;
    // a signed-in read (above all the owner's view of a draft)
    // gets no cache header (see `public_cache_headers`).
    Ok((public_cache_headers(viewer.is_none()), Json(detail)))
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
