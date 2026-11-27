//! Category taxonomy (Wave 56): the managed category tree
//! behind `categories` + `products.category_id` (migrations
//! 0060–0062), its admin API, and the public tree surfaces.
//!
//! THE MODEL. Until this wave a product's category was only
//! a free-text label (`products.category`, Wave 15). The
//! label STAYS — it is the denormalised display name every
//! existing surface reads (search facets group it, receipts
//! and exports print it) — and this module adds the curated
//! tree next to it. The two are kept in sync by exactly one
//! rule set, implemented here and in the domain crate:
//!
//! - Setting a product's `category_id` (create or patch)
//!   validates the node (exists + active) and writes the
//!   node's English name into the text column.
//! - Sending only text resolves a link through
//!   `domain::resolve_category_link` — an EXACT match
//!   against an active node's slug, English name or Bangla
//!   name — and never rewrites the text. No match means the
//!   product stays free-text with `category_id` NULL: the
//!   taxonomy curates, it does not gatekeep.
//! - The Wave 56 backfill migration applied the same
//!   resolution rule once, at migration time.
//!
//! COMPATIBILITY. The legacy surfaces are byte-identical:
//! `GET /v1/categories` still returns the free-text
//! aggregate in its `categories` field (the very same
//! `merch::load_categories` aggregate Wave 53 pinned), and
//! the home feed's `categories` tiles are untouched. The
//! taxonomy arrives ADDITIVELY: a `tree` field on the
//! categories response (suppressed with `?flat=1`), a
//! `taxonomy` field on the home feed, `category_id` +
//! `category_path` on product views. The one behavioural
//! addition to search is a NEW optional filter,
//! `category_slug`, which joins the categories facet group
//! (it is excluded exactly when the categories facet is
//! computed — the Wave 53 convention, unchanged).
//!
//! ADMIN. Category writes are admin-only and audited
//! through the one funnel (`category.create` /
//! `category.update` / `category.delete` /
//! `category.reorder` / `category.merge` /
//! `category.import`). Deletes are guarded by the domain
//! rule: children or linked products block with a bilingual
//! 409 naming the counts; merge is the bulk alternative
//! (products re-link, children re-root, then the source
//! goes). Depth is capped at 3 and reparenting can never
//! cycle — both validated over the freshly loaded tree
//! inside the write transaction.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::admin::require_admin;
use crate::audit::record_admin_action;
use crate::auth::{authenticate_optional, AuthUser};
use crate::media;
use crate::merch;
use crate::products::{
    clamp_pagination, list_products, public_cache_headers, with_stock_status,
    ListProductsQuery, Product, ProductList, PRODUCT_COLUMNS, PRODUCT_RATING_JOIN,
};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Rows, views and the shared loaders
// ---------------------------------------------------------------------------

/// One `categories` row.
#[derive(Debug, Clone, FromRow)]
pub struct CategoryRow {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub slug: String,
    pub name_en: String,
    pub name_bn: Option<String>,
    pub icon_key: String,
    pub sort_order: i32,
    pub is_active: bool,
    pub is_featured: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const CATEGORY_COLUMNS: &str = "id, parent_id, slug, name_en, name_bn, icon_key, \
    sort_order, is_active, is_featured, created_at, updated_at";

impl CategoryRow {
    /// The domain crate's flat form (string ids at the
    /// boundary, so the pure tree functions stay
    /// storage-agnostic).
    pub fn to_flat(&self) -> domain::CategoryFlat {
        domain::CategoryFlat {
            id: self.id.to_string(),
            parent_id: self.parent_id.map(|parent| parent.to_string()),
            slug: self.slug.clone(),
            name_en: self.name_en.clone(),
            name_bn: self.name_bn.clone(),
            icon_key: self.icon_key.clone(),
            sort_order: i64::from(self.sort_order),
            is_active: self.is_active,
            is_featured: self.is_featured,
        }
    }
}

/// Convert loaded rows to the domain flat form.
pub fn rows_to_flat(rows: &[CategoryRow]) -> Vec<domain::CategoryFlat> {
    rows.iter().map(CategoryRow::to_flat).collect()
}

/// Load the whole taxonomy (every node, active or not) in a
/// stable order. The tree is small by design (the depth cap
/// and human curation keep it so), so every taxonomy read
/// loads it whole and shapes it in Rust through the domain
/// functions — one implementation of the tree rules.
/// Executor-generic so write transactions validate against
/// the tree as it stands inside their own transaction.
pub async fn load_taxonomy_rows<'e, E>(executor: E) -> Result<Vec<CategoryRow>, ApiError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_as(&format!(
        "SELECT {CATEGORY_COLUMNS} FROM categories ORDER BY sort_order ASC, name_en ASC"
    ))
    .fetch_all(executor)
    .await
    .map_err(|e| ApiError::internal("load categories", e))
}

/// Load one category by id (404 when absent).
pub async fn load_category<'e, E>(executor: E, id: Uuid) -> Result<CategoryRow, ApiError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_as(&format!(
        "SELECT {CATEGORY_COLUMNS} FROM categories WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await
    .map_err(|e| ApiError::internal("load category", e))?
    .ok_or_else(|| ApiError::not_found("category not found — ক্যাটাগরিটি পাওয়া যায়নি"))
}

/// One breadcrumb entry on a product view or a category
/// detail: the node's identity and names, root first.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CategoryPathEntry {
    pub id: Uuid,
    pub slug: String,
    pub name_en: String,
    pub name_bn: Option<String>,
}

/// Build the breadcrumb entries for a node id from flat
/// rows (empty when the id is unknown).
pub fn path_entries(flats: &[domain::CategoryFlat], id: &str) -> Vec<CategoryPathEntry> {
    domain::category_path(flats, id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|flat| {
            Uuid::parse_str(&flat.id).ok().map(|uuid| CategoryPathEntry {
                id: uuid,
                slug: flat.slug,
                name_en: flat.name_en,
                name_bn: flat.name_bn,
            })
        })
        .collect()
}

/// Fill the `category_path` field of product views from
/// their `category_id` links: one taxonomy load for the
/// whole page of products, paths computed by the domain
/// rule. Products without a link (or whose node vanished —
/// the FK makes that impossible, but the fill stays total)
/// keep the empty path. Called by the product list/detail
/// reads and the faceted browse; card rails that do not
/// render breadcrumbs do not pay for it.
pub async fn attach_category_paths(pool: &PgPool, items: &mut [Product]) -> Result<(), ApiError> {
    if items.iter().all(|item| item.category_id.is_none()) {
        return Ok(());
    }
    let rows = load_taxonomy_rows(pool).await?;
    let flats = rows_to_flat(&rows);
    for item in items.iter_mut() {
        item.category_path = match item.category_id {
            Some(id) => path_entries(&flats, &id.to_string()),
            None => Vec::new(),
        };
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tree views (public + admin share the shape)
// ---------------------------------------------------------------------------

/// One node of the taxonomy tree as the storefront renders
/// it: the node's own fields, its depth, the VISIBLE product
/// counts (active products of active stores — the same
/// visibility rule as every public surface), a cover image,
/// and its children.
///
/// `product_count` rolls the whole subtree up; `own_product_count`
/// counts only products linked directly to this node. The
/// cover is the node's own best-selling imaged product,
/// else the first cover found down the subtree (a parent
/// tile is never blank while a child has imagery).
#[derive(Debug, Clone, Serialize)]
pub struct CategoryTreeNode {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub slug: String,
    pub name_en: String,
    pub name_bn: Option<String>,
    pub icon_key: String,
    pub sort_order: i32,
    pub is_active: bool,
    pub is_featured: bool,
    pub depth: i64,
    pub product_count: i64,
    pub own_product_count: i64,
    pub image_url: Option<String>,
    pub children: Vec<CategoryTreeNode>,
}

/// Per-node visible-product facts: direct counts and cover
/// image URLs, computed in two aggregate queries for the
/// whole tree at once.
struct SubtreeStats {
    own_counts: HashMap<Uuid, i64>,
    covers: HashMap<Uuid, String>,
}

async fn load_subtree_stats(pool: &PgPool) -> Result<SubtreeStats, ApiError> {
    let count_rows: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT p.category_id, COUNT(*)::bigint FROM products p \
         JOIN stores s ON s.id = p.store_id \
         WHERE p.status = 'active' AND s.status = 'active' \
           AND p.category_id IS NOT NULL \
         GROUP BY p.category_id",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("count category products", e))?;
    let own_counts: HashMap<Uuid, i64> = count_rows.into_iter().collect();

    // The cover query mirrors the home feed's category
    // aggregate (Wave 52/53): the best-selling imaged
    // product wins, newest breaks ties.
    #[derive(FromRow)]
    struct CoverRow {
        id: Uuid,
        storage_key: Option<String>,
        external_url: Option<String>,
        width: Option<i32>,
    }
    let cover_rows: Vec<CoverRow> = sqlx::query_as(
        "SELECT c.id, img.storage_key, img.external_url, img.width \
         FROM categories c \
         LEFT JOIN LATERAL ( \
             SELECT pi.storage_key, pi.external_url, pi.width \
             FROM products p \
             JOIN stores s ON s.id = p.store_id \
             JOIN product_images pi ON pi.product_id = p.id \
             WHERE p.category_id = c.id \
               AND p.status = 'active' AND s.status = 'active' \
             ORDER BY ( \
                 SELECT COALESCE(SUM(oi.quantity), 0) FROM order_items oi \
                 JOIN sub_orders so ON so.id = oi.sub_order_id \
                 WHERE oi.product_id = p.id AND so.status = 'delivered' \
             ) DESC, p.created_at DESC \
             LIMIT 1 \
         ) img ON true",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load category covers", e))?;
    let mut covers = HashMap::new();
    for row in cover_rows {
        if let Some(url) = media::product_image_url(
            row.storage_key.as_deref(),
            row.external_url.as_deref(),
            row.width,
        ) {
            covers.insert(row.id, url);
        }
    }
    Ok(SubtreeStats { own_counts, covers })
}

/// Assemble tree views for a row set (the caller decides
/// the set: active-only for the public tree, everything for
/// the admin tree).
async fn build_tree_views(
    pool: &PgPool,
    rows: &[CategoryRow],
) -> Result<Vec<CategoryTreeNode>, ApiError> {
    let stats = load_subtree_stats(pool).await?;
    let flats = rows_to_flat(rows);
    let domain_tree = domain::build_category_tree(&flats);
    let mut views: Vec<CategoryTreeNode> = domain_tree
        .into_iter()
        .map(|node| tree_node_view(node, &flats, &stats))
        .collect();
    for view in views.iter_mut() {
        fill_tree_image(view);
    }
    Ok(views)
}

fn tree_node_view(
    node: domain::CategoryNode,
    flats: &[domain::CategoryFlat],
    stats: &SubtreeStats,
) -> CategoryTreeNode {
    let id = Uuid::parse_str(&node.id).unwrap_or_default();
    let product_count = domain::subtree_ids(flats, &node.id)
        .iter()
        .filter_map(|member| Uuid::parse_str(member).ok())
        .map(|member| stats.own_counts.get(&member).copied().unwrap_or(0))
        .sum();
    CategoryTreeNode {
        id,
        parent_id: node.parent_id.as_deref().and_then(|p| Uuid::parse_str(p).ok()),
        slug: node.slug,
        name_en: node.name_en,
        name_bn: node.name_bn,
        icon_key: node.icon_key,
        sort_order: i32::try_from(node.sort_order).unwrap_or(i32::MAX),
        is_active: node.is_active,
        is_featured: node.is_featured,
        depth: node.depth,
        product_count,
        own_product_count: stats.own_counts.get(&id).copied().unwrap_or(0),
        image_url: stats.covers.get(&id).cloned(),
        children: node
            .children
            .into_iter()
            .map(|child| tree_node_view(child, flats, stats))
            .collect(),
    }
}

/// Post-order cover inheritance: a node with no cover of its
/// own takes the first cover found among its children
/// (recursively), so a parent tile is never blank while its
/// subtree has imagery. Returns the node's effective cover.
fn fill_tree_image(node: &mut CategoryTreeNode) -> Option<String> {
    let mut inherited: Option<String> = None;
    for child in node.children.iter_mut() {
        let child_image = fill_tree_image(child);
        if inherited.is_none() {
            inherited = child_image;
        }
    }
    if node.image_url.is_none() {
        node.image_url = inherited;
    }
    node.image_url.clone()
}

/// The public tree: ACTIVE nodes only. An inactive node's
/// active children surface at the top level (the domain
/// builder's orphan rule) rather than vanishing with it.
pub async fn load_public_tree(pool: &PgPool) -> Result<Vec<CategoryTreeNode>, ApiError> {
    let rows = load_taxonomy_rows(pool).await?;
    let active: Vec<CategoryRow> =
        rows.into_iter().filter(|row| row.is_active).collect();
    build_tree_views(pool, &active).await
}

/// The admin tree: every node, active or not, with the same
/// visible-count semantics (the admin list view carries the
/// all-statuses counts the delete guard uses).
pub async fn load_admin_tree(pool: &PgPool) -> Result<Vec<CategoryTreeNode>, ApiError> {
    let rows = load_taxonomy_rows(pool).await?;
    build_tree_views(pool, &rows).await
}

/// Find one node view inside a built tree by slug.
pub fn find_tree_node<'a>(
    tree: &'a [CategoryTreeNode],
    slug: &str,
) -> Option<&'a CategoryTreeNode> {
    for node in tree {
        if node.slug == slug {
            return Some(node);
        }
        if let Some(found) = find_tree_node(&node.children, slug) {
            return Some(found);
        }
    }
    None
}

/// Resolve a category slug to the id set of its subtree
/// (itself + all descendants) for the browse filter. The
/// named node must be ACTIVE — an inactive node's page is
/// gone, so its slug scopes nothing (`None`, which the
/// caller turns into an empty result, the unknown-label
/// precedent). Descendants are included as linked,
/// regardless of their own active flag: the products under
/// them are real, and the buyer's intent was the subtree.
pub async fn resolve_category_subtree_ids(
    pool: &PgPool,
    slug: &str,
) -> Result<Option<Vec<Uuid>>, ApiError> {
    let rows = load_taxonomy_rows(pool).await?;
    let flats = rows_to_flat(&rows);
    let Some(node) = domain::category_by_slug(&flats, slug) else {
        return Ok(None);
    };
    if !node.is_active {
        return Ok(None);
    }
    let ids = domain::subtree_ids(&flats, &node.id)
        .iter()
        .filter_map(|member| Uuid::parse_str(member).ok())
        .collect();
    Ok(Some(ids))
}

// ---------------------------------------------------------------------------
// The product-write resolution (the sync rule)
// ---------------------------------------------------------------------------

/// Resolve what a product write stores, from the two inputs
/// a create/patch may carry. THE sync rule:
///
/// - `category_id` present: the node must exist and be
///   active (bilingual 400 otherwise — a seller must never
///   file a product under a node the admin has retired);
///   the stored text becomes the node's English name.
/// - Only text present: the text is stored exactly as
///   typed, and the link is whatever
///   `domain::resolve_category_link` finds (possibly none).
/// - Neither: `(None, None)` — the caller merges/persists.
///
/// Leaf-ness is deliberately NOT required: a product may
/// sit on an interior node (a saree is a "Women's Fashion"
/// product before it is a "Saree" one), and subtree browse
/// rolls interior products up to every ancestor anyway.
pub async fn resolve_product_category(
    pool: &PgPool,
    category_id: Option<Uuid>,
    category_text: Option<&str>,
) -> Result<(Option<Uuid>, Option<String>), ApiError> {
    match category_id {
        Some(id) => {
            let row = sqlx::query_as(&format!(
                "SELECT {CATEGORY_COLUMNS} FROM categories WHERE id = $1"
            ))
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("load category for product", e))?
            .ok_or_else(|| {
                ApiError::bad_request(
                    "category not found — ক্যাটাগরিটি পাওয়া যায়নি",
                )
            })?;
            if !row.is_active {
                return Err(ApiError::bad_request(
                    "this category is not active — pick another one — \
                     এই ক্যাটাগরিটি সক্রিয় নয় — অন্য একটি বেছে নিন",
                ));
            }
            Ok((Some(row.id), Some(row.name_en)))
        }
        None => match category_text {
            Some(text) => Ok((resolve_link_for_text(pool, text).await?, Some(text.to_string()))),
            None => Ok((None, None)),
        },
    }
}

/// The link a free-text label resolves to, if any (the
/// domain rule over the freshly loaded taxonomy).
pub async fn resolve_link_for_text(
    pool: &PgPool,
    text: &str,
) -> Result<Option<Uuid>, ApiError> {
    let rows = load_taxonomy_rows(pool).await?;
    let flats = rows_to_flat(&rows);
    Ok(domain::resolve_category_link(text, &flats)
        .and_then(|id| Uuid::parse_str(&id).ok()))
}

// ---------------------------------------------------------------------------
// Admin views and write services (all audited)
// ---------------------------------------------------------------------------

/// The admin's flat view of one category: the row, its
/// depth, its child count, and the count of products linked
/// to it in ANY status — the delete guard's number (a draft
/// product blocks a delete exactly like a live one; the
/// seller must reassign it either way).
#[derive(Debug, Serialize)]
pub struct CategoryAdminView {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub slug: String,
    pub name_en: String,
    pub name_bn: Option<String>,
    pub icon_key: String,
    pub sort_order: i32,
    pub is_active: bool,
    pub is_featured: bool,
    pub depth: i64,
    pub child_count: i64,
    pub product_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One page of admin category views.
#[derive(Debug, Serialize)]
pub struct CategoryAdminList {
    pub items: Vec<CategoryAdminView>,
    pub total: i64,
}

/// Build admin views for a row set, in tree display order
/// (a depth-first walk, so a parent is immediately followed
/// by its children — the order the admin page renders).
async fn admin_views(
    pool: &PgPool,
    rows: &[CategoryRow],
) -> Result<Vec<CategoryAdminView>, ApiError> {
    let child_rows: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT parent_id, COUNT(*)::bigint FROM categories \
         WHERE parent_id IS NOT NULL GROUP BY parent_id",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("count category children", e))?;
    let child_counts: HashMap<Uuid, i64> = child_rows.into_iter().collect();
    let product_rows: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT category_id, COUNT(*)::bigint FROM products \
         WHERE category_id IS NOT NULL GROUP BY category_id",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("count linked products", e))?;
    let product_counts: HashMap<Uuid, i64> = product_rows.into_iter().collect();

    let flats = rows_to_flat(rows);
    let by_id: HashMap<String, &CategoryRow> =
        rows.iter().map(|row| (row.id.to_string(), row)).collect();
    let mut ordered: Vec<&CategoryFlat> = Vec::new();
    fn walk<'f>(
        nodes: &[domain::CategoryNode],
        flats: &'f [domain::CategoryFlat],
        out: &mut Vec<&'f domain::CategoryFlat>,
    ) {
        for node in nodes {
            if let Some(flat) = flats.iter().find(|f| f.id == node.id) {
                out.push(flat);
            }
            walk(&node.children, flats, out);
        }
    }
    walk(&domain::build_category_tree(&flats), &flats, &mut ordered);

    Ok(ordered
        .into_iter()
        .filter_map(|flat| {
            let row = by_id.get(flat.id.as_str()).copied()?;
            Some(CategoryAdminView {
                id: row.id,
                parent_id: row.parent_id,
                slug: row.slug.clone(),
                name_en: row.name_en.clone(),
                name_bn: row.name_bn.clone(),
                icon_key: row.icon_key.clone(),
                sort_order: row.sort_order,
                is_active: row.is_active,
                is_featured: row.is_featured,
                depth: domain::category_depth(&flats, &flat.id).unwrap_or(1),
                child_count: child_counts.get(&row.id).copied().unwrap_or(0),
                product_count: product_counts.get(&row.id).copied().unwrap_or(0),
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
        })
        .collect())
}

/// One category's admin view (404 when absent).
pub async fn admin_view(pool: &PgPool, id: Uuid) -> Result<CategoryAdminView, ApiError> {
    let rows = load_taxonomy_rows(pool).await?;
    if !rows.iter().any(|row| row.id == id) {
        return Err(ApiError::not_found(
            "category not found — ক্যাটাগরিটি পাওয়া যায়নি",
        ));
    }
    let views = admin_views(pool, &rows).await?;
    views
        .into_iter()
        .find(|view| view.id == id)
        .ok_or_else(|| ApiError::not_found("category not found — ক্যাটাগরিটি পাওয়া যায়নি"))
}

/// Map a category write error: the slug UNIQUE constraint
/// is the one expected failure, and it is a friendly
/// bilingual 409, never a 500. (Writes pre-check the slug
/// inside their transaction; this is the race backstop.)
fn category_write_error(context: &'static str, err: sqlx::Error) -> ApiError {
    let unique = err
        .as_database_error()
        .is_some_and(|db| db.code().as_deref() == Some("23505"));
    if unique {
        ApiError::conflict(
            "a category with this slug already exists — \
             এই স্লাগে ইতিমধ্যে একটি ক্যাটাগরি আছে",
        )
    } else {
        ApiError::internal(context, err)
    }
}

/// Refuse a slug another node already owns (inside the
/// write transaction, so the check and the write are one
/// atomic step).
async fn ensure_slug_free(
    tx: &mut sqlx::PgConnection,
    slug: &str,
    exclude_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let clash: Option<Uuid> = sqlx::query_scalar("SELECT id FROM categories WHERE slug = $1")
        .bind(slug)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("check category slug", e))?;
    if let Some(existing) = clash {
        if Some(existing) != exclude_id {
            return Err(ApiError::conflict(
                "a category with this slug already exists — \
                 এই স্লাগে ইতিমধ্যে একটি ক্যাটাগরি আছে",
            ));
        }
    }
    Ok(())
}

// Create

/// The admin's create-category payload.
#[derive(Debug, Deserialize)]
pub struct CreateCategoryInput {
    pub slug: String,
    pub name_en: String,
    pub name_bn: Option<String>,
    pub parent_id: Option<Uuid>,
    pub icon_key: Option<String>,
    pub sort_order: Option<i32>,
    pub is_active: Option<bool>,
    pub is_featured: Option<bool>,
}

/// Create a category (optionally under a parent), audited
/// as `category.create`. The placement is validated over
/// the tree inside the same transaction that inserts.
pub async fn create_category(
    state: &AppState,
    admin_id: Uuid,
    input: CreateCategoryInput,
) -> Result<CategoryAdminView, ApiError> {
    let pool = state.require_pool()?;
    let slug = domain::validate_category_slug(&input.slug).map_err(ApiError::bad_request)?;
    let (name_en, name_bn) =
        domain::validate_category_names(&input.name_en, input.name_bn.as_deref())
            .map_err(ApiError::bad_request)?;
    let icon_key =
        domain::validate_category_icon(input.icon_key.as_deref()).map_err(ApiError::bad_request)?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin category create", e))?;
    let rows = load_taxonomy_rows(&mut *tx).await?;
    let flats = rows_to_flat(&rows);
    domain::validate_category_parent(
        &flats,
        None,
        input.parent_id.map(|id| id.to_string()).as_deref(),
    )
    .map_err(ApiError::bad_request)?;
    ensure_slug_free(&mut *tx, &slug, None).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO categories (slug, parent_id, name_en, name_bn, icon_key, \
         sort_order, is_active, is_featured) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(&slug)
    .bind(input.parent_id)
    .bind(&name_en)
    .bind(&name_bn)
    .bind(&icon_key)
    .bind(input.sort_order.unwrap_or(0))
    .bind(input.is_active.unwrap_or(true))
    .bind(input.is_featured.unwrap_or(false))
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| category_write_error("create category", e))?;
    record_admin_action(
        &mut *tx,
        admin_id,
        "category.create",
        "category",
        Some(&id.to_string()),
        Some(serde_json::json!({ "slug": slug })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit category create", e))?;
    admin_view(pool, id).await
}

// Update

/// The admin's patch-category payload. Every field is
/// optional; `parent_id` is tri-state — absent keeps the
/// parent, `null` moves the node to the top level, a value
/// reparents. Text fields clear by sending an empty string
/// (the Wave 18 convention: validation normalises blank to
/// null for `name_bn`).
#[derive(Debug, Deserialize)]
pub struct UpdateCategoryInput {
    pub slug: Option<String>,
    pub name_en: Option<String>,
    pub name_bn: Option<String>,
    #[serde(default)]
    pub parent_id: Option<Option<Uuid>>,
    pub icon_key: Option<String>,
    pub sort_order: Option<i32>,
    pub is_active: Option<bool>,
    pub is_featured: Option<bool>,
}

/// Patch a category, audited as `category.update` with the
/// CHANGED KEY NAMES in the detail (the settings-audit
/// pattern: the log names what moved, never values an
/// auditor does not need). The merged row passes the same
/// validation as a create; a reparent is validated over the
/// whole tree inside the write transaction.
pub async fn update_category(
    state: &AppState,
    admin_id: Uuid,
    id: Uuid,
    patch: UpdateCategoryInput,
) -> Result<CategoryAdminView, ApiError> {
    let pool = state.require_pool()?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin category update", e))?;
    let existing = load_category(&mut *tx, id).await?;
    let rows = load_taxonomy_rows(&mut *tx).await?;
    let flats = rows_to_flat(&rows);

    let slug = match patch.slug.as_deref() {
        Some(raw) => domain::validate_category_slug(raw).map_err(ApiError::bad_request)?,
        None => existing.slug.clone(),
    };
    let (name_en, name_bn) = domain::validate_category_names(
        patch.name_en.as_deref().unwrap_or(&existing.name_en),
        patch.name_bn.as_deref().or(existing.name_bn.as_deref()),
    )
    .map_err(ApiError::bad_request)?;
    let icon_key = match patch.icon_key.as_deref() {
        Some(raw) => domain::validate_category_icon(Some(raw)).map_err(ApiError::bad_request)?,
        None => existing.icon_key.clone(),
    };
    let parent_id = match patch.parent_id {
        Some(parent) => parent,
        None => existing.parent_id,
    };
    if patch.parent_id.is_some() {
        domain::validate_category_parent(
            &flats,
            Some(&id.to_string()),
            parent_id.map(|parent| parent.to_string()).as_deref(),
        )
        .map_err(ApiError::bad_request)?;
    }
    let sort_order = patch.sort_order.unwrap_or(existing.sort_order);
    let is_active = patch.is_active.unwrap_or(existing.is_active);
    let is_featured = patch.is_featured.unwrap_or(existing.is_featured);

    let mut changed_keys: Vec<&str> = Vec::new();
    if slug != existing.slug {
        changed_keys.push("slug");
    }
    if name_en != existing.name_en {
        changed_keys.push("name_en");
    }
    if name_bn != existing.name_bn {
        changed_keys.push("name_bn");
    }
    if parent_id != existing.parent_id {
        changed_keys.push("parent_id");
    }
    if icon_key != existing.icon_key {
        changed_keys.push("icon_key");
    }
    if sort_order != existing.sort_order {
        changed_keys.push("sort_order");
    }
    if is_active != existing.is_active {
        changed_keys.push("is_active");
    }
    if is_featured != existing.is_featured {
        changed_keys.push("is_featured");
    }

    if slug != existing.slug {
        ensure_slug_free(&mut *tx, &slug, Some(id)).await?;
    }
    sqlx::query(
        "UPDATE categories SET slug = $2, name_en = $3, name_bn = $4, parent_id = $5, \
         icon_key = $6, sort_order = $7, is_active = $8, is_featured = $9, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&slug)
    .bind(&name_en)
    .bind(&name_bn)
    .bind(parent_id)
    .bind(&icon_key)
    .bind(sort_order)
    .bind(is_active)
    .bind(is_featured)
    .execute(&mut *tx)
    .await
    .map_err(|e| category_write_error("update category", e))?;
    record_admin_action(
        &mut *tx,
        admin_id,
        "category.update",
        "category",
        Some(&id.to_string()),
        Some(serde_json::json!({ "changed_keys": changed_keys })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit category update", e))?;
    admin_view(pool, id).await
}

// Delete

/// Delete a category, audited as `category.delete`. The
/// domain guard refuses while the node has children or
/// linked products (bilingual 409 naming the counts); the
/// FK RESTRICT is the backstop.
pub async fn delete_category(
    state: &AppState,
    admin_id: Uuid,
    id: Uuid,
) -> Result<(), ApiError> {
    let pool = state.require_pool()?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin category delete", e))?;
    let existing = load_category(&mut *tx, id).await?;
    let child_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*)::bigint FROM categories WHERE parent_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("count category children", e))?;
    let product_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*)::bigint FROM products WHERE category_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("count category products", e))?;
    domain::validate_category_delete(child_count, product_count).map_err(ApiError::conflict)?;
    sqlx::query("DELETE FROM categories WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("delete category", e))?;
    record_admin_action(
        &mut *tx,
        admin_id,
        "category.delete",
        "category",
        Some(&id.to_string()),
        Some(serde_json::json!({ "slug": existing.slug })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit category delete", e))?;
    Ok(())
}

// Reorder

/// One row of a bulk reorder: the node and its new
/// `sort_order`.
#[derive(Debug, Deserialize)]
pub struct ReorderCategoryItem {
    pub id: Uuid,
    pub sort_order: i32,
}

/// The bulk-reorder payload.
#[derive(Debug, Deserialize)]
pub struct ReorderCategoriesInput {
    pub items: Vec<ReorderCategoryItem>,
}

/// The most rows one reorder call may carry (the whole seed
/// tree is ~50 nodes; the cap is headroom, not a target).
pub const MAX_REORDER_ITEMS: usize = 500;

/// Apply a bulk `sort_order` update in ONE transaction,
/// audited as `category.reorder`. An unknown id aborts the
/// whole reorder (the admin's ordering is applied wholly or
/// not at all — a half-applied order would be worse than a
/// clear error). Returns the full admin list afterwards.
pub async fn reorder_categories(
    state: &AppState,
    admin_id: Uuid,
    input: ReorderCategoriesInput,
) -> Result<CategoryAdminList, ApiError> {
    let pool = state.require_pool()?;
    if input.items.is_empty() || input.items.len() > MAX_REORDER_ITEMS {
        return Err(ApiError::bad_request(format!(
            "a reorder carries between 1 and {MAX_REORDER_ITEMS} categories — \
             রিঅর্ডারে ১ থেকে {MAX_REORDER_ITEMS}টি ক্যাটাগরি থাকতে হবে"
        )));
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin category reorder", e))?;
    for item in &input.items {
        let result = sqlx::query(
            "UPDATE categories SET sort_order = $2, updated_at = now() WHERE id = $1",
        )
        .bind(item.id)
        .bind(item.sort_order)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("reorder category", e))?;
        if result.rows_affected() == 0 {
            return Err(ApiError::bad_request(
                "reorder names a category that does not exist — \
                 রিঅর্ডারে এমন ক্যাটাগরি আছে যা বিদ্যমান নেই",
            ));
        }
    }
    record_admin_action(
        &mut *tx,
        admin_id,
        "category.reorder",
        "category",
        None,
        Some(serde_json::json!({ "count": input.items.len() })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit category reorder", e))?;
    let rows = load_taxonomy_rows(pool).await?;
    let items = admin_views(pool, &rows).await?;
    let total = items.len() as i64;
    Ok(CategoryAdminList { items, total })
}

// Merge

/// The merge payload: the node that survives.
#[derive(Debug, Deserialize)]
pub struct MergeCategoryInput {
    pub target_id: Uuid,
}

/// What a merge did, with the surviving node's admin view.
#[derive(Debug, Serialize)]
pub struct MergeCategoryReport {
    pub target: CategoryAdminView,
    pub products_moved: i64,
    pub children_moved: i64,
}

/// Merge `source_id` into `target_id`, audited as
/// `category.merge`: the source's products re-link to the
/// target (products whose text label was the source's
/// synced English name are re-synced to the target's name;
/// hand-typed labels are left exactly as typed), the
/// source's children re-root under the target, and the
/// source row is deleted. The domain rule validates the
/// move first (no self-merge, no merge into a descendant,
/// no depth overflow) inside the same transaction.
pub async fn merge_category(
    state: &AppState,
    admin_id: Uuid,
    source_id: Uuid,
    input: MergeCategoryInput,
) -> Result<MergeCategoryReport, ApiError> {
    let pool = state.require_pool()?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin category merge", e))?;
    let rows = load_taxonomy_rows(&mut *tx).await?;
    let flats = rows_to_flat(&rows);
    domain::validate_category_merge(
        &flats,
        &source_id.to_string(),
        &input.target_id.to_string(),
    )
    .map_err(ApiError::bad_request)?;
    let source = load_category(&mut *tx, source_id).await?;
    let target = load_category(&mut *tx, input.target_id).await?;
    let products = sqlx::query(
        "UPDATE products SET category_id = $1, \
         category = CASE WHEN category = $3 THEN $4 ELSE category END \
         WHERE category_id = $2",
    )
    .bind(input.target_id)
    .bind(source_id)
    .bind(&source.name_en)
    .bind(&target.name_en)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("merge category products", e))?;
    let children = sqlx::query(
        "UPDATE categories SET parent_id = $1, updated_at = now() WHERE parent_id = $2",
    )
    .bind(input.target_id)
    .bind(source_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("merge category children", e))?;
    sqlx::query("DELETE FROM categories WHERE id = $1")
        .bind(source_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("delete merged category", e))?;
    record_admin_action(
        &mut *tx,
        admin_id,
        "category.merge",
        "category",
        Some(&source_id.to_string()),
        Some(serde_json::json!({
            "target_slug": target.slug,
            "products_moved": products.rows_affected(),
            "children_moved": children.rows_affected(),
        })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit category merge", e))?;
    Ok(MergeCategoryReport {
        target: admin_view(pool, input.target_id).await?,
        products_moved: products.rows_affected() as i64,
        children_moved: children.rows_affected() as i64,
    })
}

// ---------------------------------------------------------------------------
// Taxonomy import / export (JSON round-trip)
// ---------------------------------------------------------------------------

/// One row of the taxonomy export/import format: nodes are
/// identified by SLUG (never by database id), so a tree can
/// move between environments. Parents are slug references.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxonomyPortRow {
    pub slug: Option<String>,
    pub parent_slug: Option<String>,
    pub name_en: String,
    pub name_bn: Option<String>,
    pub icon_key: Option<String>,
    pub sort_order: Option<i32>,
    pub is_active: Option<bool>,
    pub is_featured: Option<bool>,
}

/// The whole-tree export document.
#[derive(Debug, Serialize)]
pub struct TaxonomyExport {
    pub categories: Vec<TaxonomyPortRow>,
}

/// The import document (same row shape as the export, so
/// an export feeds straight back in).
#[derive(Debug, Deserialize)]
pub struct ImportTaxonomyInput {
    pub categories: Vec<TaxonomyPortRow>,
}

/// What an import did.
#[derive(Debug, Serialize)]
pub struct TaxonomyImportReport {
    pub created: i64,
    pub updated: i64,
    pub slugs: Vec<String>,
}

/// The most rows one import may carry.
pub const MAX_IMPORT_ROWS: usize = 500;

/// Export the whole taxonomy as slug-keyed rows, ordered
/// parents-before-children (depth, then display order) so
/// the document reads as the tree it describes.
pub async fn export_taxonomy(pool: &PgPool) -> Result<TaxonomyExport, ApiError> {
    let rows = load_taxonomy_rows(pool).await?;
    let flats = rows_to_flat(&rows);
    let slug_by_id: HashMap<String, &str> = rows
        .iter()
        .map(|row| (row.id.to_string(), row.slug.as_str()))
        .collect();
    let mut sorted: Vec<&CategoryRow> = rows.iter().collect();
    sorted.sort_by(|a, b| {
        let depth_a = domain::category_depth(&flats, &a.id.to_string()).unwrap_or(1);
        let depth_b = domain::category_depth(&flats, &b.id.to_string()).unwrap_or(1);
        depth_a
            .cmp(&depth_b)
            .then_with(|| domain::category_display_order(&a.to_flat(), &b.to_flat()))
    });
    Ok(TaxonomyExport {
        categories: sorted
            .into_iter()
            .map(|row| TaxonomyPortRow {
                slug: Some(row.slug.clone()),
                parent_slug: row
                    .parent_id
                    .and_then(|parent| slug_by_id.get(&parent.to_string()).copied())
                    .map(str::to_string),
                name_en: row.name_en.clone(),
                name_bn: row.name_bn.clone(),
                icon_key: Some(row.icon_key.clone()),
                sort_order: Some(row.sort_order),
                is_active: Some(row.is_active),
                is_featured: Some(row.is_featured),
            })
            .collect(),
    })
}

/// A normalised import row (validated, slug resolved).
struct PendingImport {
    slug: String,
    parent_slug: Option<String>,
    name_en: String,
    name_bn: Option<String>,
    icon_key: String,
    sort_order: i32,
    is_active: bool,
    is_featured: bool,
}

/// Import a taxonomy document: an UPSERT keyed by slug, in
/// ONE transaction, audited as `category.import`. Rows the
/// document names are full-state for those nodes (a null
/// `parent_slug` makes the node a root); nodes the document
/// does not name are untouched. A row without a slug gets
/// one derived from its English name
/// (`domain::slugify_category_name`); a row whose slug
/// cannot be derived or validated aborts the import, as
/// does any cycle/depth violation in the MERGED tree —
/// nothing is written unless the whole document lands.
pub async fn import_taxonomy(
    state: &AppState,
    admin_id: Uuid,
    input: ImportTaxonomyInput,
) -> Result<TaxonomyImportReport, ApiError> {
    let pool = state.require_pool()?;
    if input.categories.is_empty() || input.categories.len() > MAX_IMPORT_ROWS {
        return Err(ApiError::bad_request(format!(
            "an import carries between 1 and {MAX_IMPORT_ROWS} categories — \
             ইমপোর্টে ১ থেকে {MAX_IMPORT_ROWS}টি ক্যাটাগরি থাকতে হবে"
        )));
    }
    // Normalise + validate every row before any write.
    let mut pending: Vec<PendingImport> = Vec::new();
    for row in &input.categories {
        let slug = match row.slug.as_deref() {
            Some(raw) if !raw.trim().is_empty() => {
                domain::validate_category_slug(raw).map_err(ApiError::bad_request)?
            }
            _ => {
                let derived = domain::slugify_category_name(&row.name_en);
                domain::validate_category_slug(&derived).map_err(|_| {
                    ApiError::bad_request(format!(
                        "cannot derive a slug for category '{}' — give it one — \
                         '{}' ক্যাটাগরির জন্য স্লাগ তৈরি করা যায়নি — নিজে দিন",
                        row.name_en, row.name_en
                    ))
                })?
            }
        };
        if pending.iter().any(|p: &PendingImport| p.slug == slug) {
            return Err(ApiError::bad_request(format!(
                "the import names slug '{slug}' twice — \
                 ইমপোর্টে '{slug}' স্লাগটি দুইবার আছে"
            )));
        }
        let (name_en, name_bn) =
            domain::validate_category_names(&row.name_en, row.name_bn.as_deref())
                .map_err(ApiError::bad_request)?;
        let icon_key = domain::validate_category_icon(row.icon_key.as_deref())
            .map_err(ApiError::bad_request)?;
        pending.push(PendingImport {
            slug,
            parent_slug: row
                .parent_slug
                .as_deref()
                .map(str::trim)
                .filter(|parent| !parent.is_empty())
                .map(str::to_string),
            name_en,
            name_bn,
            icon_key,
            sort_order: row.sort_order.unwrap_or(0),
            is_active: row.is_active.unwrap_or(true),
            is_featured: row.is_featured.unwrap_or(false),
        });
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin category import", e))?;
    let existing = load_taxonomy_rows(&mut *tx).await?;
    let existing_flats = rows_to_flat(&existing);
    let existing_id_by_slug: HashMap<String, String> = existing
        .iter()
        .map(|row| (row.slug.clone(), row.id.to_string()))
        .collect();

    // The merged tree: existing rows with the pending rows
    // applied (pending parents resolved by slug), so cycle
    // and depth validation runs against the state the
    // import would actually produce.
    let mut merged: Vec<domain::CategoryFlat> = existing_flats.clone();
    for p in &pending {
        if let Some(flat) = merged.iter_mut().find(|flat| flat.slug == p.slug) {
            flat.name_en = p.name_en.clone();
            flat.name_bn = p.name_bn.clone();
            flat.icon_key = p.icon_key.clone();
            flat.sort_order = i64::from(p.sort_order);
            flat.is_active = p.is_active;
            flat.is_featured = p.is_featured;
        } else {
            merged.push(domain::CategoryFlat {
                id: format!("import:{}", p.slug),
                parent_id: None,
                slug: p.slug.clone(),
                name_en: p.name_en.clone(),
                name_bn: p.name_bn.clone(),
                icon_key: p.icon_key.clone(),
                sort_order: i64::from(p.sort_order),
                is_active: p.is_active,
                is_featured: p.is_featured,
            });
        }
    }
    let merged_id_by_slug: HashMap<String, String> = merged
        .iter()
        .map(|flat| (flat.slug.clone(), flat.id.clone()))
        .collect();
    for p in &pending {
        let parent_merged_id = match p.parent_slug.as_deref() {
            Some(parent_slug) => Some(
                merged_id_by_slug
                    .get(parent_slug)
                    .cloned()
                    .ok_or_else(|| {
                        ApiError::bad_request(format!(
                            "category '{}' names unknown parent slug '{}' — \
                             '{}' ক্যাটাগরির মূল স্লাগ '{}' অজানা",
                            p.slug, parent_slug, p.slug, parent_slug
                        ))
                    })?,
            ),
            None => None,
        };
        if let Some(flat) = merged.iter_mut().find(|flat| flat.slug == p.slug) {
            flat.parent_id = parent_merged_id;
        }
    }
    for p in &pending {
        let node_merged_id = merged_id_by_slug.get(&p.slug).cloned().unwrap_or_default();
        let parent_merged_id = p
            .parent_slug
            .as_deref()
            .and_then(|parent_slug| merged_id_by_slug.get(parent_slug).cloned());
        domain::validate_category_parent(
            &merged,
            Some(&node_merged_id),
            parent_merged_id.as_deref(),
        )
        .map_err(ApiError::bad_request)?;
    }

    // Apply: upsert the node fields first (parents resolved
    // in a second pass, once every new id exists).
    let mut real_id_by_slug: HashMap<String, Uuid> = existing
        .iter()
        .map(|row| (row.slug.clone(), row.id))
        .collect();
    let mut created: i64 = 0;
    let mut updated: i64 = 0;
    for p in &pending {
        if let Some(existing_id) = existing_id_by_slug.get(&p.slug) {
            let id = Uuid::parse_str(existing_id)
                .map_err(|e| ApiError::internal_any("import category id", e))?;
            sqlx::query(
                "UPDATE categories SET name_en = $2, name_bn = $3, icon_key = $4, \
                 sort_order = $5, is_active = $6, is_featured = $7, updated_at = now() \
                 WHERE id = $1",
            )
            .bind(id)
            .bind(&p.name_en)
            .bind(&p.name_bn)
            .bind(&p.icon_key)
            .bind(p.sort_order)
            .bind(p.is_active)
            .bind(p.is_featured)
            .execute(&mut *tx)
            .await
            .map_err(|e| category_write_error("import category", e))?;
            updated += 1;
        } else {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO categories (slug, name_en, name_bn, icon_key, sort_order, \
                 is_active, is_featured) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
            )
            .bind(&p.slug)
            .bind(&p.name_en)
            .bind(&p.name_bn)
            .bind(&p.icon_key)
            .bind(p.sort_order)
            .bind(p.is_active)
            .bind(p.is_featured)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| category_write_error("import category", e))?;
            real_id_by_slug.insert(p.slug.clone(), id);
            created += 1;
        }
    }
    for p in &pending {
        let id = real_id_by_slug.get(&p.slug).copied().unwrap_or_default();
        let parent_id = p
            .parent_slug
            .as_deref()
            .and_then(|parent_slug| real_id_by_slug.get(parent_slug).copied());
        sqlx::query("UPDATE categories SET parent_id = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(parent_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("import category parent", e))?;
    }
    let slugs: Vec<String> = pending.iter().map(|p| p.slug.clone()).collect();
    record_admin_action(
        &mut *tx,
        admin_id,
        "category.import",
        "category",
        None,
        Some(serde_json::json!({ "created": created, "updated": updated })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit category import", e))?;
    Ok(TaxonomyImportReport {
        created,
        updated,
        slugs,
    })
}

// ---------------------------------------------------------------------------
// Store category specialisation (public)
// ---------------------------------------------------------------------------

/// One of a store's top taxonomy categories: the node, its
/// breadcrumb path, and how many of the store's ACTIVE
/// products link into it. Computed from real product links
/// only — a store "specialises" in exactly what it sells.
#[derive(Debug, Serialize)]
pub struct StoreCategorySpecialty {
    pub id: Uuid,
    pub slug: String,
    pub name_en: String,
    pub name_bn: Option<String>,
    pub icon_key: String,
    pub product_count: i64,
    pub path: Vec<CategoryPathEntry>,
}

/// The most specialties a storefront shows.
pub const STORE_SPECIALTY_LIMIT: i64 = 8;

/// A store's top taxonomy categories by linked active
/// products (ties broken by display order). Inactive nodes
/// are skipped — their landing pages are gone, so a tile
/// could only link to a 404.
pub async fn store_category_specialties(
    pool: &PgPool,
    store_slug: &str,
) -> Result<Vec<StoreCategorySpecialty>, ApiError> {
    let store_id: Uuid =
        sqlx::query_scalar("SELECT id FROM stores WHERE slug = $1 AND status = 'active'")
            .bind(store_slug)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("load store for categories", e))?
            .ok_or_else(|| ApiError::not_found("store not found"))?;
    let count_rows: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT category_id, COUNT(*)::bigint FROM products \
         WHERE store_id = $1 AND status = 'active' AND category_id IS NOT NULL \
         GROUP BY category_id",
    )
    .bind(store_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("count store category products", e))?;
    let rows = load_taxonomy_rows(pool).await?;
    let flats = rows_to_flat(&rows);
    let by_id: HashMap<Uuid, &CategoryRow> = rows.iter().map(|row| (row.id, row)).collect();
    let mut specialties: Vec<StoreCategorySpecialty> = count_rows
        .into_iter()
        .filter_map(|(category_id, count)| {
            let row = by_id.get(&category_id).copied()?;
            if !row.is_active {
                return None;
            }
            Some(StoreCategorySpecialty {
                id: row.id,
                slug: row.slug.clone(),
                name_en: row.name_en.clone(),
                name_bn: row.name_bn.clone(),
                icon_key: row.icon_key.clone(),
                product_count: count,
                path: path_entries(&flats, &row.id.to_string()),
            })
        })
        .collect();
    specialties.sort_by(|a, b| {
        b.product_count
            .cmp(&a.product_count)
            .then_with(|| a.name_en.cmp(&b.name_en))
    });
    specialties.truncate(STORE_SPECIALTY_LIMIT as usize);
    Ok(specialties)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Query for `GET /v1/categories`: `flat=1` (or `true`)
/// asks for the legacy response only.
#[derive(Debug, Deserialize)]
pub struct CategoriesQuery {
    pub flat: Option<String>,
}

/// GET /v1/categories — the legacy free-text aggregate in
/// `categories` (byte-identical to the Wave 53 response,
/// from the same `merch::load_categories` aggregate) PLUS
/// the taxonomy `tree`, additively. `?flat=1` suppresses
/// the tree and returns exactly the pre-Wave-56 shape.
pub async fn list_categories_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CategoriesQuery>,
) -> Result<(HeaderMap, Json<serde_json::Value>), ApiError> {
    let pool = state.require_pool()?;
    let legacy = merch::load_categories(pool, merch::CATEGORIES_LIST_LIMIT).await?;
    let mut body = serde_json::json!({ "categories": legacy });
    let flat = matches!(query.flat.as_deref(), Some("1") | Some("true"));
    if !flat {
        let tree = load_public_tree(pool).await?;
        body["tree"] = serde_json::to_value(tree)
            .map_err(|e| ApiError::internal_any("serialise category tree", e))?;
    }
    Ok((
        public_cache_headers(headers.get("authorization").is_none()),
        Json(body),
    ))
}

/// GET /v1/categories/tree — the public taxonomy tree on
/// its own (the seller picker's data source).
pub async fn category_tree_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<serde_json::Value>), ApiError> {
    let pool = state.require_pool()?;
    let tree = load_public_tree(pool).await?;
    Ok((
        public_cache_headers(headers.get("authorization").is_none()),
        Json(serde_json::json!({ "tree": tree })),
    ))
}

/// GET /v1/categories/:slug — one active node's detail:
/// the tree node (children + rolled-up counts) and its
/// breadcrumb path. Inactive or unknown slugs 404, like
/// every retired public page.
pub async fn get_category_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Result<(HeaderMap, Json<serde_json::Value>), ApiError> {
    let pool = state.require_pool()?;
    let rows = load_taxonomy_rows(pool).await?;
    let active: Vec<CategoryRow> =
        rows.into_iter().filter(|row| row.is_active).collect();
    let node_row = active
        .iter()
        .find(|row| row.slug == slug)
        .ok_or_else(|| ApiError::not_found("category not found — ক্যাটাগরিটি পাওয়া যায়নি"))?;
    let flats = rows_to_flat(&active);
    let path = path_entries(&flats, &node_row.id.to_string());
    let tree = build_tree_views(pool, &active).await?;
    let node = find_tree_node(&tree, &slug)
        .ok_or_else(|| ApiError::not_found("category not found — ক্যাটাগরিটি পাওয়া যায়নি"))?;
    Ok((
        public_cache_headers(headers.get("authorization").is_none()),
        Json(serde_json::json!({ "category": node, "path": path })),
    ))
}

/// GET /v1/categories/:slug/products — the node's subtree
/// product list: the plain product list scoped to the
/// subtree's id set, for clients that page a category
/// without the full faceted browse. Unlike the list
/// endpoints' `category_slug` FILTER (where an unknown
/// slug scopes to nothing), addressing a category
/// directly 404s an unknown or retired slug — the same
/// rule as the node detail above. Visibility, sort and
/// pagination are the shared list's, unchanged.
pub async fn category_products_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(query): Query<ListProductsQuery>,
) -> Result<(HeaderMap, Json<ProductList>), ApiError> {
    let pool = state.require_pool()?;
    let viewer = authenticate_optional(&state, &headers).map(|user| user.user_id);
    let ids = resolve_category_subtree_ids(pool, &slug)
        .await?
        .ok_or_else(|| {
            ApiError::not_found("category not found — ক্যাটাগরিটি পাওয়া যায়নি")
        })?;
    let list = list_products(
        pool,
        query.q.as_deref(),
        query.store_slug.as_deref(),
        query.page,
        query.limit,
        viewer,
        query.sort.as_deref(),
        Some(ids),
    )
    .await?;
    Ok((public_cache_headers(viewer.is_none()), Json(list)))
}

/// GET /v1/stores/:slug/categories — the store's taxonomy
/// specialisation (its top linked categories).
pub async fn store_categories_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Result<(HeaderMap, Json<serde_json::Value>), ApiError> {
    let pool = state.require_pool()?;
    let categories = store_category_specialties(pool, &slug).await?;
    Ok((
        public_cache_headers(headers.get("authorization").is_none()),
        Json(serde_json::json!({ "store_slug": slug, "categories": categories })),
    ))
}

// Admin handlers (thin; every one behind the admin guard)

pub async fn admin_list_categories_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<CategoryAdminList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let rows = load_taxonomy_rows(pool).await?;
    let items = admin_views(pool, &rows).await?;
    let total = items.len() as i64;
    Ok(Json(CategoryAdminList { items, total }))
}

pub async fn admin_tree_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let tree = load_admin_tree(pool).await?;
    Ok(Json(serde_json::json!({ "tree": tree })))
}

pub async fn admin_get_category_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<CategoryAdminView>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(admin_view(pool, id).await?))
}

pub async fn admin_create_category_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<CreateCategoryInput>,
) -> Result<(StatusCode, Json<CategoryAdminView>), ApiError> {
    let admin = require_admin(&state, &user).await?;
    let view = create_category(&state, admin.id, input).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

pub async fn admin_update_category_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(patch): Json<UpdateCategoryInput>,
) -> Result<Json<CategoryAdminView>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    Ok(Json(update_category(&state, admin.id, id, patch).await?))
}

pub async fn admin_delete_category_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    delete_category(&state, admin.id, id).await?;
    Ok(Json(serde_json::json!({ "deleted": true, "id": id })))
}

pub async fn admin_reorder_categories_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<ReorderCategoriesInput>,
) -> Result<Json<CategoryAdminList>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    Ok(Json(reorder_categories(&state, admin.id, input).await?))
}

pub async fn admin_merge_category_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<MergeCategoryInput>,
) -> Result<Json<MergeCategoryReport>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    Ok(Json(merge_category(&state, admin.id, id, input).await?))
}

/// Query for the admin category-products drill-in.
#[derive(Debug, Deserialize)]
pub struct CategoryProductsQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

/// GET /v1/admin/categories/:id/products — every product
/// linked into the node's SUBTREE, any status (the admin
/// view: drafts and suspended-store products included, so
/// the delete guard's count can be inspected, not just
/// asserted).
pub async fn admin_category_products_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(query): Query<CategoryProductsQuery>,
) -> Result<Json<ProductList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let rows = load_taxonomy_rows(pool).await?;
    let flats = rows_to_flat(&rows);
    if domain::category_by_id(&flats, &id.to_string()).is_none() {
        return Err(ApiError::not_found(
            "category not found — ক্যাটাগরিটি পাওয়া যায়নি",
        ));
    }
    let ids: Vec<Uuid> = domain::subtree_ids(&flats, &id.to_string())
        .iter()
        .filter_map(|member| Uuid::parse_str(member).ok())
        .collect();
    let (page, limit) = clamp_pagination(query.page, query.limit);
    let offset = (page - 1) * limit;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM products WHERE category_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count category products", e))?;
    let mut items: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products p \
         JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE p.category_id = ANY($1) \
         ORDER BY p.created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(&ids)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list category products", e))?
    .into_iter()
    .map(with_stock_status)
    .collect();
    attach_category_paths(pool, &mut items).await?;
    Ok(Json(ProductList {
        items,
        total,
        page,
        limit,
    }))
}

pub async fn admin_export_taxonomy_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<TaxonomyExport>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(export_taxonomy(pool).await?))
}

pub async fn admin_import_taxonomy_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<ImportTaxonomyInput>,
) -> Result<Json<TaxonomyImportReport>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    Ok(Json(import_taxonomy(&state, admin.id, input).await?))
}
