//! Product search (Wave 15) — Postgres-native. There is no
//! external search service: matching is pg_trgm similarity plus
//! ILIKE over the product text (the indexes landed in migrations
//! 0004 and 0017), and ranking is one ORDER BY expression in the
//! items query below. The two endpoints here are deliberately the
//! whole search contract — if the catalogue ever outgrows
//! Postgres, a dedicated engine (Meilisearch or similar) can be
//! put behind these same endpoints without clients changing.
//!
//! Visibility is the public rule, stricter than the listings:
//! search is an unauthenticated discovery surface, so it shows
//! only `active` products of `active` stores — there is no owner
//! exception here (sellers manage drafts through the listing
//! endpoints, not through search). Result rows are the shared
//! `products::Product` view, rating aggregate included, so search
//! results can never drift from every other product shape.
//!
//! Wave 26 — sponsored placement: products inside a live
//! promotion window (`is_promoted`, computed in the shared
//! product columns) sort FIRST under the default orderings —
//! relevance (with or without search text) and newest — ahead
//! of the relevance rank itself: sponsored placement is the
//! product the platform sells, and it is always labelled with
//! the স্পন্সরড badge in the UI. Explicit buyer-chosen sorts
//! stay pure: price ascending/descending and rating order
//! exactly by price or rating, promoted or not — a buyer who
//! asks for the cheapest first gets the cheapest first.

use axum::{extract::Query, extract::State, http::HeaderMap, Json};
use domain::{
    search_like_pattern, search_prefix_pattern, validate_search_params, SearchParams, SearchSort,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::products::{
    public_cache_headers, with_stock_status, Product, PRODUCT_COLUMNS, PRODUCT_RATING_JOIN,
};
use crate::{ApiError, AppState};

/// One page of search results plus the total match count. The
/// envelope mirrors `products::ProductList`, with `per_page`
/// naming the page size (the search contract's own word).
#[derive(Debug, Serialize)]
pub struct SearchResults {
    pub items: Vec<Product>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

/// The relevance rank expression, used only when search text is
/// present. Exact title matches (English or Bangla) win outright,
/// then title prefixes, then title substrings, then a small boost
/// for description-only matches; the pg_trgm similarity of the
/// combined title text breaks the remaining ties. The weights are
/// far enough apart that a higher tier always beats any
/// combination of lower tiers plus similarity (which is at most
/// 1.0). Binds: $1 = contains pattern, $2 = raw text,
/// $7 = prefix pattern.
const SEARCH_RANK: &str = "(\
    CASE WHEN lower(p.title) = lower($2) \
              OR lower(COALESCE(p.title_bn, '')) = lower($2) THEN 8.0 ELSE 0.0 END \
    + CASE WHEN p.title ILIKE $7 OR p.title_bn ILIKE $7 THEN 4.0 ELSE 0.0 END \
    + CASE WHEN p.title ILIKE $1 OR p.title_bn ILIKE $1 THEN 2.0 ELSE 0.0 END \
    + CASE WHEN p.description ILIKE $1 THEN 0.5 ELSE 0.0 END \
    + similarity(p.title || ' ' || COALESCE(p.title_bn, ''), $2))";

/// The match + filter predicate shared by the search COUNT and
/// items queries (static fragments only — all values stay bound
/// parameters). $1 = contains pattern (or NULL), $2 = raw text
/// (or NULL), $3 = store slug (or NULL), $4 = category (or NULL),
/// $5/$6 = price bounds (or NULL). With text present, a product
/// matches when the text appears in its title, Bangla title or
/// description, or when the combined title text is trigram-similar
/// to it (threshold 0.2 — the typo tolerance this wave offers).
const SEARCH_WHERE: &str = "p.status = 'active' AND s.status = 'active' \
    AND ($1::text IS NULL OR p.title ILIKE $1 OR p.title_bn ILIKE $1 \
         OR p.description ILIKE $1 \
         OR similarity(p.title || ' ' || COALESCE(p.title_bn, ''), $2) >= 0.2) \
    AND ($3::text IS NULL OR s.slug = $3) \
    AND ($4::text IS NULL OR lower(p.category) = lower($4)) \
    AND ($5::bigint IS NULL OR p.price_poisha >= $5) \
    AND ($6::bigint IS NULL OR p.price_poisha <= $6)";

/// Run one product search. `params` is already validated by the
/// domain crate; `store_slug` and `category` are the remaining raw
/// filters (trimmed here, blank means absent).
pub async fn search_products(
    pool: &PgPool,
    params: &SearchParams,
    store_slug: Option<&str>,
    category: Option<&str>,
) -> Result<SearchResults, ApiError> {
    let store_slug = store_slug.map(str::trim).filter(|s| !s.is_empty());
    let category = category.map(str::trim).filter(|s| !s.is_empty());
    let pattern = params.q.as_deref().map(search_like_pattern);
    let prefix_pattern = params.q.as_deref().map(search_prefix_pattern);
    let offset = (params.page - 1) * params.per_page;

    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM products p JOIN stores s ON s.id = p.store_id \
         WHERE {SEARCH_WHERE}"
    ))
    .bind(&pattern)
    .bind(&params.q)
    .bind(store_slug)
    .bind(category)
    .bind(params.min_price_poisha)
    .bind(params.max_price_poisha)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count search results", e))?;

    // The ORDER BY text comes from this fixed match on the
    // validated sort enum — never from user input. Wave 26:
    // the default orderings (relevance, newest) put live
    // sponsored promotions first via the `is_promoted` output
    // column; the explicit price and rating sorts stay pure.
    let order_by = match (params.sort, params.q.is_some()) {
        (SearchSort::Relevance, true) => {
            format!("ORDER BY is_promoted DESC, {SEARCH_RANK} DESC, p.created_at DESC")
        }
        (SearchSort::Relevance, false) | (SearchSort::Newest, _) => {
            "ORDER BY is_promoted DESC, p.created_at DESC".to_string()
        }
        (SearchSort::PriceAsc, _) => "ORDER BY p.price_poisha ASC, p.created_at DESC".to_string(),
        (SearchSort::PriceDesc, _) => {
            "ORDER BY p.price_poisha DESC, p.created_at DESC".to_string()
        }
        (SearchSort::Rating, _) => {
            "ORDER BY rv.rating_avg DESC NULLS LAST, rv.rating_count DESC, \
             p.created_at DESC"
                .to_string()
        }
    };

    let items: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products p JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE {SEARCH_WHERE} {order_by} LIMIT $8 OFFSET $9"
    ))
    .bind(&pattern)
    .bind(&params.q)
    .bind(store_slug)
    .bind(category)
    .bind(params.min_price_poisha)
    .bind(params.max_price_poisha)
    .bind(&prefix_pattern)
    .bind(params.per_page)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("search products", e))?
    .into_iter()
    .map(with_stock_status)
    .collect();

    Ok(SearchResults {
        items,
        total,
        page: params.page,
        per_page: params.per_page,
    })
}

/// One suggested product title (Wave 15 suggestions).
#[derive(Debug, Serialize, FromRow)]
pub struct SuggestedProduct {
    pub id: Uuid,
    pub title: String,
}

/// One suggested store (Wave 15 suggestions).
#[derive(Debug, Serialize, FromRow)]
pub struct SuggestedStore {
    pub slug: String,
    pub name: String,
}

/// The suggestion payload: up to 8 product titles and 4 stores.
#[derive(Debug, Serialize)]
pub struct SuggestResults {
    pub products: Vec<SuggestedProduct>,
    pub stores: Vec<SuggestedStore>,
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
            stores: Vec::new(),
        });
    }
    let pattern = search_like_pattern(trimmed);

    let products: Vec<SuggestedProduct> = sqlx::query_as(
        "SELECT d.id, d.title FROM (\
            SELECT DISTINCT ON (lower(p.title)) p.id AS id, p.title AS title, \
                   similarity(p.title || ' ' || COALESCE(p.title_bn, ''), $2) AS sim \
            FROM products p JOIN stores s ON s.id = p.store_id \
            WHERE p.status = 'active' AND s.status = 'active' \
              AND (p.title ILIKE $1 OR p.title_bn ILIKE $1) \
            ORDER BY lower(p.title), sim DESC\
         ) d ORDER BY d.sim DESC LIMIT 8",
    )
    .bind(&pattern)
    .bind(trimmed)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("suggest products", e))?;

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

    Ok(SuggestResults { products, stores })
}

// ---------------------------------------------------------------------------
// Request shapes + handlers (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub store_slug: Option<String>,
    pub category: Option<String>,
    pub min_price_poisha: Option<i64>,
    pub max_price_poisha: Option<i64>,
    pub sort: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
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
    let params = validate_search_params(
        query.q.as_deref(),
        query.min_price_poisha,
        query.max_price_poisha,
        query.sort.as_deref(),
        query.page,
        query.per_page,
    )
    .map_err(ApiError::bad_request)?;
    let results = search_products(
        pool,
        &params,
        query.store_slug.as_deref(),
        query.category.as_deref(),
    )
    .await?;
    // Wave 32: search has no authenticated variant at all (see
    // the module doc), so every response is the public view and
    // carries the public cache header.
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
