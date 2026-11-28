//! Product search & discovery (Wave 15, faceted in
//! Wave 53) — Postgres-native. There is no external search
//! service: matching is pg_trgm similarity plus ILIKE over
//! the product text (the indexes landed in migrations 0004
//! and 0017), and ranking is one ORDER BY expression in the
//! items query below. The endpoints here are deliberately
//! the whole discovery contract — if the catalogue ever
//! outgrows Postgres, a dedicated engine (Meilisearch or
//! similar) can be put behind these same endpoints without
//! clients changing.
//!
//! Visibility is the public rule, stricter than the
//! listings: search is an unauthenticated discovery
//! surface, so it shows only `active` products of `active`
//! stores — there is no owner exception here (sellers
//! manage drafts through the listing endpoints, not
//! through search). Result rows are the shared
//! `products::Product` view, rating aggregate included, so
//! search results can never drift from every other product
//! shape.
//!
//! Wave 26 — sponsored placement: products inside a live
//! promotion window (`is_promoted`, computed in the shared
//! product columns) sort FIRST under the default orderings
//! — relevance (with or without search text) and newest —
//! ahead of the relevance rank itself: sponsored placement
//! is the product the platform sells, and it is always
//! labelled with the স্পন্সরড badge in the UI. Explicit
//! buyer-chosen sorts stay pure: price ascending/descending,
//! rating and (Wave 53) best-sellers order exactly by their
//! key, promoted or not — a buyer who asks for the cheapest
//! first gets the cheapest first.
//!
//! Wave 53 — the faceted browse. `GET /v1/search` grows
//! from text + a few filters into the browse endpoint
//! behind `/browse`: multi-value category and store
//! filters, a minimum rating, in-stock and on-sale flags,
//! a `sold` sort, the `limit` page-size spelling, and a
//! `facets` block in every response. The rules that keep
//! it honest:
//!
//! - PRICES ARE EFFECTIVE. Every price filter, price sort
//!   and the price facet price by
//!   `merch::EFFECTIVE_PRICE_SQL` — the live flash price
//!   when the SQL twin of the Wave 52 resolution resolves
//!   one, else the base price — so discovery can never
//!   quote a different price from the product view, the
//!   cart or checkout on the same rows.
//! - FACET COUNTS EXCLUDE THEIR OWN FILTER. The category
//!   facet counts the set filtered by everything EXCEPT
//!   the category filter (so the buyer can see what
//!   widening that group would add); likewise stores,
//!   price bounds and rating. The result `total` and the
//!   items always apply ALL filters. This is the standard
//!   faceted-search convention, stated here because the
//!   counts are otherwise surprising.
//! - An unrated product counts as 0 stars (the
//!   `COALESCE(rv.rating_avg, 0)` in the conditions), for
//!   the rating filter and the rating facet alike.
//!
//! Wave 53 also adds the search-analytics log (migration
//! 0049): every search appends its text, its result count
//! and a fingerprint of its filter set — never a user id —
//! and `GET /v1/admin/search-analytics` reads the top and
//! zero-result queries back for the operator.

use std::collections::HashMap;

use axum::{extract::State, http::HeaderMap, Json};
// Wave 53 fix: repeated filter params (?category=a&category=b)
// need axum-extra's htmlform-backed Query; axum's built-in
// serde_urlencoded Query rejects them before the handler runs.
use axum_extra::extract::Query;
use chrono::{DateTime, Utc};
use domain::{
    search_like_pattern, search_prefix_pattern, validate_browse_filters, BrowseFilterInput,
    BrowseFilters, SearchSort, RATING_FACET_THRESHOLDS,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::admin::require_admin;
use crate::auth::AuthUser;
use crate::merch::{EFFECTIVE_PRICE_SQL, FLASH_PRICE_JOIN};
use crate::products::{
    public_cache_headers, with_stock_status, Product, EFFECTIVE_STOCK_SQL, PRODUCT_COLUMNS,
    PRODUCT_RATING_JOIN,
};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Response shapes
// ---------------------------------------------------------------------------

/// One page of browse results plus the total match count
/// and the facet block. The envelope mirrors
/// `products::ProductList`, with `per_page` naming the
/// page size (the search contract's own word since
/// Wave 15) and `limit` carrying the same value under the
/// Wave 53 browse spelling.
#[derive(Debug, Serialize)]
pub struct SearchResults {
    pub items: Vec<Product>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub limit: i64,
    pub facets: SearchFacets,
}

/// The facet block of a browse response (Wave 53): counts
/// for widening each filter group, each computed against
/// the result set filtered by every OTHER group (see the
/// module doc for the convention).
#[derive(Debug, Serialize)]
pub struct SearchFacets {
    pub categories: Vec<CategoryFacet>,
    pub stores: Vec<StoreFacet>,
    pub price: PriceFacet,
    pub ratings: Vec<RatingFacet>,
}

/// One category's count in the facet block: how many
/// products the OTHER filters leave in this category.
#[derive(Debug, Serialize, FromRow)]
pub struct CategoryFacet {
    pub category: String,
    pub count: i64,
}

/// One store's count in the facet block.
#[derive(Debug, Serialize, FromRow)]
pub struct StoreFacet {
    pub slug: String,
    pub name: String,
    pub count: i64,
}

/// The effective-price span of the set filtered by
/// everything except the price bounds themselves — the
/// range the price inputs can meaningfully offer. Null
/// bounds when the set is empty.
#[derive(Debug, Serialize)]
pub struct PriceFacet {
    pub min_poisha: Option<i64>,
    pub max_poisha: Option<i64>,
    /// The span split into price bands with a count each
    /// (the histogram the filter panel renders): bands are
    /// inclusive `[min_poisha, max_poisha]` ranges planned
    /// by the domain crate over the same exclude-price set
    /// as the span, counted on the effective price.
    pub histogram: Vec<PriceHistogramBucket>,
}

/// One band of the price facet's histogram.
#[derive(Debug, Serialize)]
pub struct PriceHistogramBucket {
    pub min_poisha: i64,
    pub max_poisha: i64,
    pub count: i64,
}

/// One rating threshold's count ("4★ & up" and friends);
/// thresholds are [`RATING_FACET_THRESHOLDS`].
#[derive(Debug, Serialize)]
pub struct RatingFacet {
    pub min_rating: f64,
    pub count: i64,
}

// ---------------------------------------------------------------------------
// The browse query builder
// ---------------------------------------------------------------------------

/// Which filter group a facet query leaves out (the
/// module doc's convention). The items and count queries
/// exclude nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FacetExclusion {
    /// Apply every filter (items + total).
    None,
    /// Leave out the category filter.
    Categories,
    /// Leave out the store filter.
    Stores,
    /// Leave out both price bounds.
    Price,
    /// Leave out the minimum-rating filter.
    Rating,
}

/// Push the FROM clause every browse query shares: the
/// products/stores join, the shared rating aggregate and
/// the flash-price lateral (the SQL twin of the Wave 52
/// resolution). Every browse query — items, count and all
/// four facets — reads through this one FROM, so they can
/// never disagree about visibility, rating or price.
fn push_browse_from(qb: &mut QueryBuilder<'_, Postgres>) {
    qb.push(" FROM products p JOIN stores s ON s.id = p.store_id ");
    qb.push(PRODUCT_RATING_JOIN);
    qb.push(" ");
    qb.push(FLASH_PRICE_JOIN);
}

/// Push the WHERE conditions for `filters`, honouring
/// `exclude` (a facet leaves its own group out). All
/// values are bound parameters — the only text pushed is
/// fixed fragments and the fixed effective-price /
/// effective-stock expressions.
///
/// Wave 56: `taxonomy_ids` is the category-slug subtree
/// filter, resolved by the handler from the taxonomy (it
/// deliberately lives OUTSIDE `BrowseFilters`, so the
/// validated filter shape, the analytics fingerprint and
/// the saved-search form stay byte-identical). It belongs
/// to the CATEGORIES facet group: it is applied everywhere
/// the text category filter is applied, and left out
/// exactly where that filter is left out — the Wave 53
/// convention, unchanged.
fn push_browse_conditions(
    qb: &mut QueryBuilder<'_, Postgres>,
    filters: &BrowseFilters,
    exclude: FacetExclusion,
    taxonomy_ids: Option<&[Uuid]>,
) {
    qb.push(" WHERE p.status = 'active' AND s.status = 'active' AND p.admin_hidden_at IS NULL");
    if let Some(q) = &filters.q {
        let pattern = search_like_pattern(q);
        qb.push(" AND (p.title ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR p.title_bn ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR p.description ILIKE ");
        qb.push_bind(pattern);
        qb.push(" OR similarity(p.title || ' ' || COALESCE(p.title_bn, ''), ");
        qb.push_bind(q.clone());
        qb.push(") >= 0.2)");
    }
    if exclude != FacetExclusion::Categories && !filters.categories.is_empty() {
        let lowered: Vec<String> = filters
            .categories
            .iter()
            .map(|category| category.to_lowercase())
            .collect();
        qb.push(" AND lower(p.category) = ANY(");
        qb.push_bind(lowered);
        qb.push(")");
    }
    if exclude != FacetExclusion::Categories {
        if let Some(ids) = taxonomy_ids {
            qb.push(" AND p.category_id = ANY(");
            qb.push_bind(ids.to_vec());
            qb.push(")");
        }
    }
    if exclude != FacetExclusion::Stores && !filters.store_slugs.is_empty() {
        qb.push(" AND s.slug = ANY(");
        qb.push_bind(filters.store_slugs.clone());
        qb.push(")");
    }
    if exclude != FacetExclusion::Price {
        if let Some(min) = filters.min_price_poisha {
            qb.push(" AND ");
            qb.push(EFFECTIVE_PRICE_SQL);
            qb.push(" >= ");
            qb.push_bind(min);
        }
        if let Some(max) = filters.max_price_poisha {
            qb.push(" AND ");
            qb.push(EFFECTIVE_PRICE_SQL);
            qb.push(" <= ");
            qb.push_bind(max);
        }
    }
    if exclude != FacetExclusion::Rating {
        if let Some(min_rating) = filters.min_rating {
            qb.push(" AND COALESCE(rv.rating_avg, 0) >= ");
            qb.push_bind(min_rating);
        }
    }
    if filters.in_stock {
        qb.push(" AND (");
        qb.push(EFFECTIVE_STOCK_SQL);
        qb.push(") > 0");
    }
    if filters.on_sale {
        qb.push(
            " AND (fx.flash_price_poisha IS NOT NULL \
             OR (p.compare_at_poisha IS NOT NULL \
                 AND p.compare_at_poisha > p.price_poisha))",
        );
    }
}

/// Push the relevance rank expression (Wave 15): exact
/// title matches (English or Bangla) win outright, then
/// title prefixes, then title substrings, then a small
/// boost for description-only matches; the pg_trgm
/// similarity of the combined title text breaks the
/// remaining ties. The weights are far enough apart that
/// a higher tier always beats any combination of lower
/// tiers plus similarity (which is at most 1.0).
fn push_search_rank(qb: &mut QueryBuilder<'_, Postgres>, q: &str) {
    let pattern = search_like_pattern(q);
    let prefix = search_prefix_pattern(q);
    qb.push("(CASE WHEN lower(p.title) = lower(");
    qb.push_bind(q.to_string());
    qb.push(") OR lower(COALESCE(p.title_bn, '')) = lower(");
    qb.push_bind(q.to_string());
    qb.push(") THEN 8.0 ELSE 0.0 END + CASE WHEN p.title ILIKE ");
    qb.push_bind(prefix.clone());
    qb.push(" OR p.title_bn ILIKE ");
    qb.push_bind(prefix);
    qb.push(" THEN 4.0 ELSE 0.0 END + CASE WHEN p.title ILIKE ");
    qb.push_bind(pattern.clone());
    qb.push(" OR p.title_bn ILIKE ");
    qb.push_bind(pattern.clone());
    qb.push(" THEN 2.0 ELSE 0.0 END + CASE WHEN p.description ILIKE ");
    qb.push_bind(pattern);
    qb.push(" THEN 0.5 ELSE 0.0 END + similarity(p.title || ' ' || COALESCE(p.title_bn, ''), ");
    qb.push_bind(q.to_string());
    qb.push("))");
}

/// Push the ORDER BY for the items query. The text comes
/// from this fixed match on the validated sort enum —
/// never from user input. Wave 26's rule is preserved:
/// the default orderings (relevance, newest) put live
/// sponsored promotions first via the `is_promoted`
/// output column; the explicit price, rating and sold
/// sorts stay pure.
fn push_browse_order(qb: &mut QueryBuilder<'_, Postgres>, filters: &BrowseFilters) {
    match (filters.sort, &filters.q) {
        (SearchSort::Relevance, Some(q)) => {
            qb.push(" ORDER BY is_promoted DESC, ");
            push_search_rank(qb, q);
            qb.push(" DESC, p.created_at DESC");
        }
        (SearchSort::Relevance, None) | (SearchSort::Newest, _) => {
            qb.push(" ORDER BY is_promoted DESC, p.created_at DESC");
        }
        (SearchSort::PriceAsc, _) => {
            qb.push(" ORDER BY ");
            qb.push(EFFECTIVE_PRICE_SQL);
            qb.push(" ASC, p.created_at DESC");
        }
        (SearchSort::PriceDesc, _) => {
            qb.push(" ORDER BY ");
            qb.push(EFFECTIVE_PRICE_SQL);
            qb.push(" DESC, p.created_at DESC");
        }
        (SearchSort::Rating, _) => {
            qb.push(
                " ORDER BY rv.rating_avg DESC NULLS LAST, rv.rating_count DESC, \
                 p.created_at DESC",
            );
        }
        (SearchSort::Sold, _) => {
            qb.push(" ORDER BY sold_count DESC, p.created_at DESC");
        }
    }
}

// ---------------------------------------------------------------------------
// The browse service
// ---------------------------------------------------------------------------

/// Run one faceted browse. `filters` is already validated
/// by the domain crate ([`validate_browse_filters`]).
pub async fn search_products(
    pool: &PgPool,
    filters: &BrowseFilters,
) -> Result<SearchResults, ApiError> {
    search_products_scoped(pool, filters, None).await
}

/// Run one faceted browse, optionally scoped to a taxonomy
/// subtree (Wave 56: the `category_slug` filter, resolved
/// to category ids by the caller). The scoping joins the
/// categories facet group — see `push_browse_conditions`.
pub async fn search_products_scoped(
    pool: &PgPool,
    filters: &BrowseFilters,
    taxonomy_ids: Option<&[Uuid]>,
) -> Result<SearchResults, ApiError> {
    let offset = (filters.page - 1) * filters.per_page;

    let mut count_qb = QueryBuilder::<Postgres>::new("SELECT COUNT(*)");
    push_browse_from(&mut count_qb);
    push_browse_conditions(&mut count_qb, filters, FacetExclusion::None, taxonomy_ids);
    let total: i64 = count_qb
        .build_query_scalar()
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count browse results", e))?;

    let mut items_qb = QueryBuilder::<Postgres>::new("SELECT ");
    items_qb.push(PRODUCT_COLUMNS);
    push_browse_from(&mut items_qb);
    push_browse_conditions(&mut items_qb, filters, FacetExclusion::None, taxonomy_ids);
    push_browse_order(&mut items_qb, filters);
    items_qb.push(" LIMIT ");
    items_qb.push_bind(filters.per_page);
    items_qb.push(" OFFSET ");
    items_qb.push_bind(offset);
    let mut items: Vec<Product> = items_qb
        .build_query_as::<Product>()
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("browse products", e))?
        .into_iter()
        .map(with_stock_status)
        .collect();
    // Wave 52: browse results carry live flash prices like
    // every other product surface — the same resolution the
    // SQL twin priced the filters by, so the fields agree.
    crate::merch::attach_flash_prices(pool, &mut items, Utc::now()).await?;
    // Wave 56: and their taxonomy breadcrumb paths.
    crate::categories::attach_category_paths(pool, &mut items).await?;

    let facets = SearchFacets {
        categories: load_category_facet(pool, filters, taxonomy_ids).await?,
        stores: load_store_facet(pool, filters, taxonomy_ids).await?,
        price: load_price_facet(pool, filters, taxonomy_ids).await?,
        ratings: load_rating_facet(pool, filters, taxonomy_ids).await?,
    };

    Ok(SearchResults {
        items,
        total,
        page: filters.page,
        per_page: filters.per_page,
        limit: filters.per_page,
        facets,
    })
}

/// The category facet: counts per category over the set
/// filtered by everything except the category filter.
/// Uncategorised products form no facet entry (there is
/// no category to widen into).
async fn load_category_facet(
    pool: &PgPool,
    filters: &BrowseFilters,
    taxonomy_ids: Option<&[Uuid]>,
) -> Result<Vec<CategoryFacet>, ApiError> {
    let mut qb = QueryBuilder::<Postgres>::new(
        "SELECT p.category AS category, COUNT(*)::bigint AS count",
    );
    push_browse_from(&mut qb);
    push_browse_conditions(&mut qb, filters, FacetExclusion::Categories, taxonomy_ids);
    qb.push(" AND p.category IS NOT NULL AND btrim(p.category) <> ''");
    qb.push(" GROUP BY p.category ORDER BY count DESC, p.category ASC");
    qb.build_query_as::<CategoryFacet>()
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("browse category facet", e))
}

/// The store facet: counts per store over the set
/// filtered by everything except the store filter.
async fn load_store_facet(
    pool: &PgPool,
    filters: &BrowseFilters,
    taxonomy_ids: Option<&[Uuid]>,
) -> Result<Vec<StoreFacet>, ApiError> {
    let mut qb = QueryBuilder::<Postgres>::new(
        "SELECT s.slug AS slug, s.name AS name, COUNT(*)::bigint AS count",
    );
    push_browse_from(&mut qb);
    push_browse_conditions(&mut qb, filters, FacetExclusion::Stores, taxonomy_ids);
    qb.push(" GROUP BY s.slug, s.name ORDER BY count DESC, s.slug ASC");
    qb.build_query_as::<StoreFacet>()
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("browse store facet", e))
}

/// The price facet: the effective-price span of the set
/// filtered by everything except the price bounds.
async fn load_price_facet(
    pool: &PgPool,
    filters: &BrowseFilters,
    taxonomy_ids: Option<&[Uuid]>,
) -> Result<PriceFacet, ApiError> {
    let mut qb = QueryBuilder::<Postgres>::new("SELECT MIN(");
    qb.push(EFFECTIVE_PRICE_SQL);
    qb.push(")::bigint AS min_poisha, MAX(");
    qb.push(EFFECTIVE_PRICE_SQL);
    qb.push(")::bigint AS max_poisha");
    push_browse_from(&mut qb);
    push_browse_conditions(&mut qb, filters, FacetExclusion::Price, taxonomy_ids);
    let (min_poisha, max_poisha): (Option<i64>, Option<i64>) = qb
        .build_query_as::<(Option<i64>, Option<i64>)>()
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("browse price facet", e))?;
    let histogram = match (min_poisha, max_poisha) {
        (Some(min), Some(max)) => {
            load_price_histogram(pool, filters, taxonomy_ids, min, max).await?
        }
        _ => Vec::new(),
    };
    Ok(PriceFacet {
        min_poisha,
        max_poisha,
        histogram,
    })
}

/// Count the exclude-price set into the histogram bands
/// the domain planner lays over the facet span — one
/// conditional-aggregation query, effective prices only,
/// so a band's count always matches the band the price
/// filter itself would admit.
async fn load_price_histogram(
    pool: &PgPool,
    filters: &BrowseFilters,
    taxonomy_ids: Option<&[Uuid]>,
    min_poisha: i64,
    max_poisha: i64,
) -> Result<Vec<PriceHistogramBucket>, ApiError> {
    let ranges = domain::price_histogram_ranges(min_poisha, max_poisha);
    // One row, one bigint[] column: the bands' counts in
    // planner order.
    let mut qb = QueryBuilder::<Postgres>::new("SELECT ARRAY[");
    for (index, (lo, hi)) in ranges.iter().enumerate() {
        if index > 0 {
            qb.push(", ");
        }
        qb.push("COUNT(*) FILTER (WHERE ");
        qb.push(EFFECTIVE_PRICE_SQL);
        qb.push(" >= ");
        qb.push_bind(*lo);
        qb.push(" AND ");
        qb.push(EFFECTIVE_PRICE_SQL);
        qb.push(" < ");
        qb.push_bind(*hi);
        qb.push(")");
    }
    qb.push("]::bigint[]");
    push_browse_from(&mut qb);
    push_browse_conditions(&mut qb, filters, FacetExclusion::Price, taxonomy_ids);
    let row: Vec<i64> = qb
        .build_query_scalar::<Vec<i64>>()
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("browse price histogram", e))?;
    Ok(ranges
        .into_iter()
        .zip(row)
        .map(|((lo, hi), count)| PriceHistogramBucket {
            min_poisha: lo,
            // The planner's ranges are half-open; the wire
            // shape is the inclusive band the shopper picks.
            max_poisha: hi - 1,
            count,
        })
        .collect())
}

/// The rating facet: one count per threshold in
/// [`RATING_FACET_THRESHOLDS`], over the set filtered by
/// everything except the rating filter, in a single
/// conditional-aggregation query. Unrated counts as 0 —
/// the same convention as the rating filter itself.
async fn load_rating_facet(
    pool: &PgPool,
    filters: &BrowseFilters,
    taxonomy_ids: Option<&[Uuid]>,
) -> Result<Vec<RatingFacet>, ApiError> {
    let mut qb = QueryBuilder::<Postgres>::new("SELECT ");
    for (index, threshold) in RATING_FACET_THRESHOLDS.iter().enumerate() {
        if index > 0 {
            qb.push(", ");
        }
        qb.push("COUNT(*) FILTER (WHERE COALESCE(rv.rating_avg, 0) >= ");
        qb.push_bind(*threshold);
        qb.push(")::bigint");
    }
    push_browse_from(&mut qb);
    push_browse_conditions(&mut qb, filters, FacetExclusion::Rating, taxonomy_ids);
    let counts: (i64, i64, i64) = qb
        .build_query_as::<(i64, i64, i64)>()
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("browse rating facet", e))?;
    Ok(RATING_FACET_THRESHOLDS
        .iter()
        .copied()
        .zip([counts.0, counts.1, counts.2])
        .map(|(min_rating, count)| RatingFacet { min_rating, count })
        .collect())
}

// ---------------------------------------------------------------------------
// Suggestions (Wave 15, enriched in Wave 53)
// ---------------------------------------------------------------------------

/// One suggested product (Wave 15; Wave 53 adds the
/// effective `price_poisha`, the primary `image_url` and
/// the `category`, so the header dropdown can render a
/// real result row — thumbnail, price, category — instead
/// of a bare title).
#[derive(Debug, Serialize)]
pub struct SuggestedProduct {
    pub id: Uuid,
    pub title: String,
    /// The price a buyer pays right now: the live flash
    /// price when one resolves, else the base price — the
    /// same effective-price rule as the browse.
    pub price_poisha: i64,
    pub image_url: Option<String>,
    pub category: Option<String>,
}

/// The suggestion query's raw row: the product facts plus
/// the primary image's stored facts, which
/// [`search_suggestions`] folds into `image_url` through
/// the media module's one URL rule.
#[derive(Debug, FromRow)]
struct SuggestedProductRow {
    id: Uuid,
    title: String,
    price_poisha: i64,
    category: Option<String>,
    image_key: Option<String>,
    image_external_url: Option<String>,
    image_width: Option<i32>,
}

/// One suggested store (Wave 15 suggestions).
#[derive(Debug, Serialize, FromRow)]
pub struct SuggestedStore {
    pub slug: String,
    pub name: String,
}

/// One suggested category (Wave 53): a category label
/// carried by products matching the fragment, with how
/// many visible products carry it — the dropdown's "in
/// category" row, which lands on the browse filtered to
/// that label.
#[derive(Debug, Serialize, FromRow)]
pub struct SuggestedCategory {
    pub category: String,
    pub product_count: i64,
}

/// One suggested TAXONOMY node (Wave 56): a suggested
/// category label that resolves into the managed tree,
/// with the node's identity, names and breadcrumb path —
/// the dropdown can land the buyer on the category's
/// landing page instead of a text-filtered browse. Labels
/// that resolve to nothing produce no node (the text
/// suggestion above still stands on its own).
#[derive(Debug, Serialize)]
pub struct SuggestedCategoryNode {
    pub id: Uuid,
    pub slug: String,
    pub name_en: String,
    pub name_bn: Option<String>,
    pub icon_key: String,
    /// The label the node was resolved from.
    pub label: String,
    pub product_count: i64,
    pub path: Vec<crate::categories::CategoryPathEntry>,
}

/// The suggestion payload: up to 8 products, 5 categories
/// and 4 stores. Wave 56 adds `category_nodes` — the
/// taxonomy resolution of the text categories, additive;
/// the Wave 53 fields are untouched.
#[derive(Debug, Serialize)]
pub struct SuggestResults {
    pub products: Vec<SuggestedProduct>,
    pub categories: Vec<SuggestedCategory>,
    pub stores: Vec<SuggestedStore>,
    pub category_nodes: Vec<SuggestedCategoryNode>,
}

/// Minimum suggestion text length, in characters: below this the
/// endpoint answers with two empty lists rather than guessing.
const MIN_SUGGEST_CHARS: usize = 2;

/// Fetch search suggestions for a text fragment. Products are
/// distinct titles of visible products matching the fragment
/// (title or Bangla title), best trigram similarity first;
/// stores are visible stores whose name or slug matches. The
/// same public visibility rule as `search_products` applies.
pub async fn search_suggestions(pool: &PgPool, q: &str) -> Result<SuggestResults, ApiError> {
    let trimmed = q.trim();
    if trimmed.chars().count() < MIN_SUGGEST_CHARS {
        return Ok(SuggestResults {
            products: Vec::new(),
            categories: Vec::new(),
            stores: Vec::new(),
            category_nodes: Vec::new(),
        });
    }
    let pattern = search_like_pattern(trimmed);

    let rows: Vec<SuggestedProductRow> = sqlx::query_as(
        "SELECT d.id, d.title, d.price_poisha, d.category, \
                d.image_key, d.image_external_url, d.image_width FROM (\
            SELECT DISTINCT ON (lower(p.title)) p.id AS id, p.title AS title, \
                   p.price_poisha AS price_poisha, p.category AS category, \
                   (SELECT pi.storage_key FROM product_images pi \
                     WHERE pi.product_id = p.id \
                     ORDER BY pi.is_primary DESC, pi.position ASC, pi.created_at ASC \
                     LIMIT 1) AS image_key, \
                   (SELECT pi.external_url FROM product_images pi \
                     WHERE pi.product_id = p.id \
                     ORDER BY pi.is_primary DESC, pi.position ASC, pi.created_at ASC \
                     LIMIT 1) AS image_external_url, \
                   (SELECT pi.width FROM product_images pi \
                     WHERE pi.product_id = p.id \
                     ORDER BY pi.is_primary DESC, pi.position ASC, pi.created_at ASC \
                     LIMIT 1) AS image_width, \
                   similarity(p.title || ' ' || COALESCE(p.title_bn, ''), $2) AS sim \
            FROM products p JOIN stores s ON s.id = p.store_id \
            WHERE p.status = 'active' AND s.status = 'active' AND p.admin_hidden_at IS NULL \
              AND (p.title ILIKE $1 OR p.title_bn ILIKE $1) \
            ORDER BY lower(p.title), sim DESC\
         ) d ORDER BY d.sim DESC LIMIT 8",
    )
    .bind(&pattern)
    .bind(trimmed)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("suggest products", e))?;

    // Wave 53: the suggested price is the EFFECTIVE price,
    // resolved by the same batch attach the product views
    // use (the suggestion rows are not product views, so
    // the resolution runs through the price map directly).
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let flash = crate::merch::flash_price_map(pool, &ids, Utc::now()).await?;
    let products = rows
        .into_iter()
        .map(|row| SuggestedProduct {
            price_poisha: flash
                .get(&row.id)
                .map(|resolved| resolved.sale_price_poisha)
                .unwrap_or(row.price_poisha),
            image_url: crate::media::product_image_url(
                row.image_key.as_deref(),
                row.image_external_url.as_deref(),
                row.image_width,
            ),
            id: row.id,
            title: row.title,
            category: row.category,
        })
        .collect();

    // The categories the matching products live in: the
    // same product predicate as the product suggestions,
    // grouped — a category the fragment's products do not
    // use is never suggested.
    let categories: Vec<SuggestedCategory> = sqlx::query_as(
        "SELECT p.category AS category, COUNT(*)::bigint AS product_count \
         FROM products p JOIN stores s ON s.id = p.store_id \
         WHERE p.status = 'active' AND s.status = 'active' AND p.admin_hidden_at IS NULL \
           AND p.category IS NOT NULL \
           AND (p.title ILIKE $1 OR p.title_bn ILIKE $1) \
         GROUP BY p.category \
         ORDER BY product_count DESC, p.category ASC LIMIT 5",
    )
    .bind(&pattern)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("suggest categories", e))?;

    let stores: Vec<SuggestedStore> = sqlx::query_as(
        "SELECT slug, name FROM stores \
         WHERE status = 'active' AND (name ILIKE $1 OR slug ILIKE $1) \
         ORDER BY similarity(name, $2) DESC LIMIT 4",
    )
    .bind(&pattern)
    .bind(trimmed)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("suggest stores", e))?;

    // Wave 56: resolve the suggested labels into taxonomy
    // nodes (the domain's exact-match rule over the active
    // tree) so the dropdown can offer the curated landing
    // page next to the text browse. Unresolvable labels
    // simply have no node.
    let category_nodes = resolve_suggested_category_nodes(pool, &categories).await?;

    Ok(SuggestResults {
        products,
        categories,
        stores,
        category_nodes,
    })
}

/// Resolve suggested text categories to taxonomy nodes
/// (Wave 56). One taxonomy load for the whole suggestion
/// set; resolution is the domain rule, so a label lands on
/// the same node a product write with that label would.
async fn resolve_suggested_category_nodes(
    pool: &PgPool,
    categories: &[SuggestedCategory],
) -> Result<Vec<SuggestedCategoryNode>, ApiError> {
    if categories.is_empty() {
        return Ok(Vec::new());
    }
    let rows = crate::categories::load_taxonomy_rows(pool).await?;
    let flats = crate::categories::rows_to_flat(&rows);
    let by_id: HashMap<Uuid, &crate::categories::CategoryRow> =
        rows.iter().map(|row| (row.id, row)).collect();
    let mut nodes = Vec::new();
    for suggested in categories {
        let Some(id_text) = domain::resolve_category_link(&suggested.category, &flats) else {
            continue;
        };
        let Some(id) = Uuid::parse_str(&id_text).ok() else {
            continue;
        };
        let Some(row) = by_id.get(&id).copied() else {
            continue;
        };
        nodes.push(SuggestedCategoryNode {
            id: row.id,
            slug: row.slug.clone(),
            name_en: row.name_en.clone(),
            name_bn: row.name_bn.clone(),
            icon_key: row.icon_key.clone(),
            label: suggested.category.clone(),
            product_count: suggested.product_count,
            path: crate::categories::path_entries(&flats, &id_text),
        });
    }
    Ok(nodes)
}

// ---------------------------------------------------------------------------
// Search analytics (Wave 53, migration 0049)
// ---------------------------------------------------------------------------

/// How many rows each analytics list returns. (The
/// window itself is the domain's rule:
/// `validate_analytics_window_days`, default
/// `DEFAULT_ANALYTICS_WINDOW_DAYS`.)
pub const SEARCH_ANALYTICS_LIMIT: i64 = 10;

/// Append one search to the analytics log. Best-effort by
/// design: the log informs the operator, it never gates a
/// shopper's search — a failed insert is reported on
/// stderr and the search still answers. The row carries
/// the query text, the result count and the filter
/// fingerprint; there is deliberately NO user id, session
/// or address column in the table at all.
pub async fn log_search(pool: &PgPool, filters: &BrowseFilters, result_count: i64) {
    let fingerprint = domain::search_filters_fingerprint(filters);
    // The canonical filters as JSON (migration 0050): the
    // saved-search stored shape, so the analytics read can
    // show WHICH combinations the fingerprint groups.
    let filters_json =
        serde_json::to_value(crate::saved_searches::SavedSearchFilters::from_validated(filters))
            .unwrap_or(serde_json::Value::Null);
    if let Err(err) = sqlx::query(
        "INSERT INTO search_queries (query_text, result_count, filters_hash, filters_json) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(&filters.q)
    .bind(result_count)
    .bind(fingerprint)
    .bind(filters_json)
    .execute(pool)
    .await
    {
        eprintln!("search analytics: failed to log a search: {err}");
    }
}

/// One heavily-run query text in the analytics window.
#[derive(Debug, Serialize, FromRow)]
pub struct TopSearch {
    pub query_text: String,
    pub searches: i64,
    /// Mean result count across those searches, one
    /// decimal — a popular query returning almost nothing
    /// is a catalogue gap the operator can act on.
    pub avg_results: f64,
}

/// One query text that returned nothing in the window:
/// the catalogue's unmet demand, in the shoppers' own
/// words.
#[derive(Debug, Serialize, FromRow)]
pub struct ZeroResultSearch {
    pub query_text: String,
    pub searches: i64,
    pub last_searched_at: DateTime<Utc>,
}

/// One heavily-run filter combination in the analytics
/// window (Wave 53): the canonical filter set (the
/// saved-search shape), how often that exact combination
/// ran, and its mean result count. Grouped by the
/// fingerprint; the JSON is what makes the group readable.
#[derive(Debug, Serialize)]
pub struct TopFilterCombination {
    pub filters: crate::saved_searches::SavedSearchFilters,
    pub searches: i64,
    pub avg_results: f64,
}

/// The combinations query's raw row.
#[derive(Debug, FromRow)]
struct FilterCombinationRow {
    filters_json: serde_json::Value,
    searches: i64,
    avg_results: f64,
}

/// The admin search-analytics payload: the window's
/// totals, its most-run query texts and its zero-result
/// texts. Pure filter browses (no text) count toward the
/// total but appear in neither text list.
#[derive(Debug, Serialize)]
pub struct SearchAnalytics {
    pub window_days: i64,
    pub total_searches: i64,
    pub top_searches: Vec<TopSearch>,
    pub zero_result_searches: Vec<ZeroResultSearch>,
    pub top_filter_combinations: Vec<TopFilterCombination>,
}

/// Read the search analytics for the last `window_days`
/// days (validated by the handler through the domain's
/// window rule: the default when absent, 1..=90).
pub async fn search_analytics(
    pool: &PgPool,
    window_days: i64,
) -> Result<SearchAnalytics, ApiError> {
    let total_searches: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM search_queries \
         WHERE created_at >= now() - make_interval(days => $1::int)",
    )
    .bind(window_days)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count logged searches", e))?;

    let top_searches: Vec<TopSearch> = sqlx::query_as(
        "SELECT query_text, COUNT(*)::bigint AS searches, \
                CAST(ROUND(AVG(result_count)::numeric, 1) AS float8) AS avg_results \
         FROM search_queries \
         WHERE query_text IS NOT NULL \
           AND created_at >= now() - make_interval(days => $1::int) \
         GROUP BY query_text \
         ORDER BY searches DESC, query_text ASC LIMIT $2",
    )
    .bind(window_days)
    .bind(SEARCH_ANALYTICS_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("top searches", e))?;

    let zero_result_searches: Vec<ZeroResultSearch> = sqlx::query_as(
        "SELECT query_text, COUNT(*)::bigint AS searches, \
                MAX(created_at) AS last_searched_at \
         FROM search_queries \
         WHERE query_text IS NOT NULL AND result_count = 0 \
           AND created_at >= now() - make_interval(days => $1::int) \
         GROUP BY query_text \
         ORDER BY searches DESC, query_text ASC LIMIT $2",
    )
    .bind(window_days)
    .bind(SEARCH_ANALYTICS_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("zero-result searches", e))?;

    // Combinations cover rows logged since migration
    // 0050 (older rows carry no stored shape and are
    // excluded rather than guessed at).
    let combination_rows: Vec<FilterCombinationRow> = sqlx::query_as(
        "SELECT filters_json, COUNT(*)::bigint AS searches, \
                CAST(ROUND(AVG(result_count)::numeric, 1) AS float8) AS avg_results \
         FROM search_queries \
         WHERE filters_json IS NOT NULL \
           AND created_at >= now() - make_interval(days => $1::int) \
         GROUP BY filters_hash, filters_json \
         ORDER BY searches DESC, filters_hash ASC LIMIT $2",
    )
    .bind(window_days)
    .bind(SEARCH_ANALYTICS_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("top filter combinations", e))?;
    let mut top_filter_combinations = Vec::with_capacity(combination_rows.len());
    for row in combination_rows {
        let filters: crate::saved_searches::SavedSearchFilters =
            serde_json::from_value(row.filters_json).map_err(|e| {
                ApiError::internal_any("decode logged filters", e)
            })?;
        top_filter_combinations.push(TopFilterCombination {
            filters,
            searches: row.searches,
            avg_results: row.avg_results,
        });
    }

    Ok(SearchAnalytics {
        window_days,
        total_searches,
        top_searches,
        zero_result_searches,
        top_filter_combinations,
    })
}

/// One popular search text (Wave 53): a query text and
/// how often it ran inside the popular window.
#[derive(Debug, Serialize, FromRow)]
pub struct PopularSearch {
    pub query_text: String,
    pub searches: i64,
}

/// The public popular-searches payload: the window the
/// counts cover and the texts themselves.
#[derive(Debug, Serialize)]
pub struct PopularSearches {
    pub window_days: i64,
    pub searches: Vec<PopularSearch>,
}

/// How many popular searches the public read returns.
pub const POPULAR_SEARCHES_LIMIT: i64 = 8;

/// The most-run search texts of the trailing default
/// window, for the header search box's "popular" list.
/// Public on purpose: the data is the same aggregate the
/// admin analytics shows (texts + counts, no identifiers
/// exist in the log to leak), and it is what every large
/// marketplace shows an empty search box.
pub async fn popular_searches(pool: &PgPool) -> Result<PopularSearches, ApiError> {
    let searches: Vec<PopularSearch> = sqlx::query_as(
        "SELECT query_text, COUNT(*)::bigint AS searches \
         FROM search_queries \
         WHERE query_text IS NOT NULL \
           AND created_at >= now() - make_interval(days => $1::int) \
         GROUP BY query_text \
         ORDER BY searches DESC, query_text ASC LIMIT $2",
    )
    .bind(domain::DEFAULT_ANALYTICS_WINDOW_DAYS)
    .bind(POPULAR_SEARCHES_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("popular searches", e))?;
    Ok(PopularSearches {
        window_days: domain::DEFAULT_ANALYTICS_WINDOW_DAYS,
        searches,
    })
}

// ---------------------------------------------------------------------------
// Request shapes + handlers (thin)
// ---------------------------------------------------------------------------

/// The browse query string. Multi-value filters arrive as
/// repeated parameters (`category=a&category=b` — each may
/// also be comma-joined); `store_slug` is the Wave 15
/// single-store spelling, still honoured and merged into
/// the `store` list.
#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    #[serde(default)]
    pub category: Vec<String>,
    /// Wave 56: taxonomy slugs to scope the browse by
    /// SUBTREE (repeatable; the union of the named
    /// subtrees). Resolved to category ids by the handler
    /// and passed to the service OUTSIDE `BrowseFilters`,
    /// so the validated filter shape, the analytics
    /// fingerprint and the saved-search form are untouched.
    #[serde(default)]
    pub category_slug: Vec<String>,
    #[serde(default)]
    pub store: Vec<String>,
    pub store_slug: Option<String>,
    pub min_price_poisha: Option<i64>,
    pub max_price_poisha: Option<i64>,
    pub min_rating: Option<f64>,
    pub in_stock: Option<String>,
    pub on_sale: Option<String>,
    pub sort: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct SuggestQuery {
    pub q: Option<String>,
}

pub async fn search_handler(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<(HeaderMap, Json<SearchResults>), ApiError> {
    let pool = state.require_pool()?;
    let mut store_slugs = query.store.clone();
    if let Some(slug) = &query.store_slug {
        store_slugs.push(slug.clone());
    }
    // THE one validation call for the whole browse
    // contract (Wave 53): the domain judges the raw input,
    // the service below only ever sees validated filters.
    let filters = validate_browse_filters(BrowseFilterInput {
        q: query.q.clone(),
        categories: query.category.clone(),
        store_slugs,
        min_price_poisha: query.min_price_poisha,
        max_price_poisha: query.max_price_poisha,
        min_rating: query.min_rating,
        in_stock: query.in_stock.clone(),
        on_sale: query.on_sale.clone(),
        sort: query.sort.clone(),
        page: query.page,
        per_page: query.per_page,
        limit: query.limit,
    })
    .map_err(ApiError::bad_request)?;
    // Wave 56: resolve the category slugs to their subtree
    // id sets. An unknown (or inactive) slug contributes an
    // EMPTY set — the browse simply matches nothing for it,
    // the same answer an unknown text label has always
    // given, never an error that leaks the taxonomy's
    // contents.
    let taxonomy_ids: Option<Vec<Uuid>> = if query.category_slug.is_empty() {
        None
    } else {
        let mut ids: Vec<Uuid> = Vec::new();
        for slug in &query.category_slug {
            if let Some(mut subtree) =
                crate::categories::resolve_category_subtree_ids(pool, slug).await?
            {
                ids.append(&mut subtree);
            }
        }
        ids.sort();
        ids.dedup();
        Some(ids)
    };
    let results = search_products_scoped(pool, &filters, taxonomy_ids.as_deref()).await?;
    // Wave 53: the analytics log rides the handler (the
    // service stays a pure read); a logging failure never
    // fails the search.
    log_search(pool, &filters, results.total).await;
    // Wave 32: search has no authenticated variant at all
    // (see the module doc), so every response is the public
    // view and carries the public cache header.
    Ok((public_cache_headers(true), Json(results)))
}

pub async fn suggest_handler(
    State(state): State<AppState>,
    Query(query): Query<SuggestQuery>,
) -> Result<Json<SuggestResults>, ApiError> {
    let pool = state.require_pool()?;
    let results = search_suggestions(pool, query.q.as_deref().unwrap_or("")).await?;
    Ok(Json(results))
}

/// The admin analytics query: an optional window in days.
#[derive(Deserialize)]
pub struct AnalyticsQuery {
    pub days: Option<i64>,
}

/// `GET /v1/admin/search-analytics` — the operator's view
/// of what shoppers search for (Wave 53). Admin-only, like
/// every `/v1/admin/*` read. `?days=` widens or narrows
/// the window (the domain's 1..=90 rule; absent = 30).
pub async fn admin_search_analytics_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AnalyticsQuery>,
) -> Result<Json<SearchAnalytics>, ApiError> {
    require_admin(&state, &user).await?;
    let window_days =
        domain::validate_analytics_window_days(query.days).map_err(ApiError::bad_request)?;
    let pool = state.require_pool()?;
    Ok(Json(search_analytics(pool, window_days).await?))
}

/// `GET /v1/search/popular` — the most-run search texts
/// of the trailing window (Wave 53), for the search box's
/// popular list. Public aggregate data (see
/// [`popular_searches`]); carries the public cache header
/// like every discovery read.
pub async fn popular_searches_handler(
    State(state): State<AppState>,
) -> Result<(HeaderMap, Json<PopularSearches>), ApiError> {
    let pool = state.require_pool()?;
    let results = popular_searches(pool).await?;
    Ok((public_cache_headers(true), Json(results)))
}
