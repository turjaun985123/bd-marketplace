//! Recommendations (Wave 24) — three discovery rails, all of
//! them plain SQL over data the marketplace already has. There
//! is no ML model and no event tracking behind any of this:
//! the rankings are documented rules, and a surface that has
//! nothing honest to show answers with an empty list.
//!
//! Related products (`GET /v1/products/:id/related`) — one
//! query scores every other publicly visible product against
//! the base product:
//!
//! - +4 — same category (both non-null, case-insensitive);
//! - +2 — same store;
//! - +1 — well reviewed (rating average at least 4.0).
//!
//! Ordering is score descending, then rating average
//! descending (unreviewed last), then review count
//! descending, then newest first. The base product must
//! itself be publicly visible, else 404 — the same rule the
//! wishlist add uses.
//!
//! Also bought (`GET /v1/products/:id/also-bought`) —
//! ORDER-LEVEL co-purchase: a product counts when it sits in
//! a DIFFERENT sub-order of an order that also contains the
//! base product (the split checkout puts each store's goods
//! in its own sub-order under one order, so this is the
//! cross-store "bought together" signal). `co_count` is the
//! number of DISTINCT orders the pair shares. Only publicly
//! visible companions are returned, ordered by `co_count`
//! descending, then rating. A product nobody has ordered
//! yet honestly returns an empty list — co-purchases are
//! counted, never invented. The base product must be
//! publicly visible, else 404, exactly like related.
//!
//! Personalised (`GET /v1/recommendations`) — the caller's
//! signals are the CATEGORIES of two things: products they
//! received in a delivered sub-order, and products on their
//! wishlist, each category weighted by its most recent
//! signal. Candidates are publicly visible products in a
//! signal category that the caller has NOT bought (any
//! non-cancelled sub-order) and has NOT wishlisted —
//! recommendations are for discovering new products, not
//! re-showing saved or owned ones. Ordering is signal
//! recency descending, then rating. A caller with no
//! signals at all gets the documented fallback — the
//! marketplace's top-rated publicly visible products — and
//! the response says so: `personalized` is false. A caller
//! WITH signals whose candidates are all excluded gets
//! `personalized: true` with an empty list, not the
//! fallback dressed up as personalisation.
//!
//! Every rail's page size goes through the one domain rule,
//! `recommendation_limit` (default 8, clamped to 1..=24).

use axum::{
    extract::{Path, Query, State},
    Json,
};
use domain::recommendation_limit;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::products::{
    get_public_product, with_stock_status, Product, PRODUCT_COLUMNS, PRODUCT_RATING_JOIN,
};
use crate::{ApiError, AppState};

/// Products related to one base product (the related rail's
/// envelope: just the items, in rank order).
#[derive(Debug, Serialize)]
pub struct RelatedList {
    pub items: Vec<Product>,
}

/// One also-bought companion: the full shared product view
/// plus how many distinct orders pair it with the base
/// product.
#[derive(Debug, Serialize)]
pub struct AlsoBoughtItem {
    #[serde(flatten)]
    pub product: Product,
    /// Distinct orders containing both this product (in a
    /// different sub-order) and the base product.
    pub co_count: i64,
}

/// The also-bought rail's envelope.
#[derive(Debug, Serialize)]
pub struct AlsoBoughtList {
    pub items: Vec<AlsoBoughtItem>,
}

/// The personalised rail's envelope. `personalized` is
/// false exactly when the answer is the top-rated fallback
/// (the caller has no category signals yet).
#[derive(Debug, Serialize)]
pub struct Recommendations {
    pub personalized: bool,
    pub items: Vec<Product>,
}

/// Row shape for the also-bought query: the shared product
/// columns plus the pair's distinct-order count.
#[derive(Debug, FromRow)]
struct AlsoBoughtRow {
    #[sqlx(flatten)]
    product: Product,
    co_count: i64,
}

/// "The caller has not bought this product" — a product
/// counts as bought when it appears in any of the caller's
/// NON-cancelled sub-orders (a cancelled order never
/// delivered anything, so its products stay recommendable).
/// $1 = the caller's user id; `p` is the candidate product.
const NOT_BOUGHT_BY_CALLER: &str = "NOT EXISTS (\
    SELECT 1 FROM order_items oi \
    JOIN sub_orders so ON so.id = oi.sub_order_id \
    JOIN orders o ON o.id = so.order_id \
    WHERE o.buyer_user_id = $1 AND oi.product_id = p.id \
      AND so.status <> 'cancelled')";

/// "The caller has not wishlisted this product" —
/// recommendations surface discoveries, not saved items.
/// $1 = the caller's user id; `p` is the candidate product.
const NOT_WISHLISTED_BY_CALLER: &str = "NOT EXISTS (\
    SELECT 1 FROM wishlist_items w \
    WHERE w.user_id = $1 AND w.product_id = p.id)";

/// Related products for one base product, ranked by the
/// scoring rule in the module doc. The base product is
/// loaded first (which is also its visibility check), so
/// the candidate query stays a single statement.
pub async fn related_products(
    pool: &PgPool,
    base_id: Uuid,
    limit: Option<i64>,
) -> Result<RelatedList, ApiError> {
    let base = get_public_product(pool, base_id).await?;
    let limit = recommendation_limit(limit);
    let mut items: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products p \
         JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE p.id <> $1 AND p.status = 'active' AND s.status = 'active' AND p.admin_hidden_at IS NULL \
         ORDER BY (\
             CASE WHEN $2::text IS NOT NULL AND p.category IS NOT NULL \
                       AND lower(p.category) = lower($2) THEN 4 ELSE 0 END \
             + CASE WHEN p.store_id = $3 THEN 2 ELSE 0 END \
             + CASE WHEN rv.rating_avg >= 4.0 THEN 1 ELSE 0 END \
         ) DESC, \
         rv.rating_avg DESC NULLS LAST, rv.rating_count DESC, p.created_at DESC \
         LIMIT $4"
    ))
    .bind(base_id)
    .bind(&base.category)
    .bind(base.store_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("related products", e))?
    .into_iter()
    .map(with_stock_status)
    .collect();
    // Wave 52: rails carry live flash prices like every
    // other product surface.
    crate::merch::attach_flash_prices(pool, &mut items, chrono::Utc::now()).await?;
    Ok(RelatedList { items })
}

/// Also-bought companions for one base product (the
/// co-purchase rule in the module doc). The count is
/// computed in a CTE keyed by product, then joined to the
/// shared product view so visibility and rating come from
/// the one materialisation.
pub async fn also_bought_products(
    pool: &PgPool,
    base_id: Uuid,
    limit: Option<i64>,
) -> Result<AlsoBoughtList, ApiError> {
    get_public_product(pool, base_id).await?;
    let limit = recommendation_limit(limit);
    let rows: Vec<AlsoBoughtRow> = sqlx::query_as(&format!(
        "WITH co AS (\
            SELECT oi2.product_id AS product_id, \
                   COUNT(DISTINCT so1.order_id) AS co_count \
            FROM order_items oi1 \
            JOIN sub_orders so1 ON so1.id = oi1.sub_order_id \
            JOIN sub_orders so2 ON so2.order_id = so1.order_id AND so2.id <> so1.id \
            JOIN order_items oi2 ON oi2.sub_order_id = so2.id \
            WHERE oi1.product_id = $1 AND oi2.product_id <> $1 \
            GROUP BY oi2.product_id\
         ) \
         SELECT {PRODUCT_COLUMNS}, co.co_count FROM co \
         JOIN products p ON p.id = co.product_id \
         JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE p.status = 'active' AND s.status = 'active' AND p.admin_hidden_at IS NULL \
         ORDER BY co.co_count DESC, \
         rv.rating_avg DESC NULLS LAST, rv.rating_count DESC, p.created_at DESC \
         LIMIT $2"
    ))
    .bind(base_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("also bought products", e))?;
    let mut items: Vec<AlsoBoughtItem> = rows
        .into_iter()
        .map(|row| AlsoBoughtItem {
            product: with_stock_status(row.product),
            co_count: row.co_count,
        })
        .collect();
    // Wave 52: rails carry live flash prices like every
    // other product surface.
    let mut refs: Vec<&mut Product> = items.iter_mut().map(|item| &mut item.product).collect();
    crate::merch::attach_flash_prices_refs(pool, &mut refs, chrono::Utc::now()).await?;
    Ok(AlsoBoughtList { items })
}

/// Does the caller have any category signal at all — a
/// delivered purchase or a wishlist item whose product has
/// a category? This one boolean chooses between the
/// personalised query and the top-rated fallback.
async fn caller_has_signals(pool: &PgPool, user_id: Uuid) -> Result<bool, ApiError> {
    sqlx::query_scalar(
        "SELECT (\
            EXISTS (\
                SELECT 1 FROM orders o \
                JOIN sub_orders so ON so.order_id = o.id \
                JOIN order_items oi ON oi.sub_order_id = so.id \
                JOIN products p ON p.id = oi.product_id \
                WHERE o.buyer_user_id = $1 AND so.status = 'delivered' \
                  AND p.category IS NOT NULL\
            ) OR EXISTS (\
                SELECT 1 FROM wishlist_items w \
                JOIN products p ON p.id = w.product_id \
                WHERE w.user_id = $1 AND p.category IS NOT NULL\
            )\
        )",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("recommendation signals", e))
}

/// The personalised rail for one caller (see the module
/// doc for the signal, exclusion and fallback rules).
pub async fn recommendations_for_user(
    pool: &PgPool,
    user_id: Uuid,
    limit: Option<i64>,
) -> Result<Recommendations, ApiError> {
    let limit = recommendation_limit(limit);
    if !caller_has_signals(pool, user_id).await? {
        // The documented fallback: top-rated publicly
        // visible products, with the caller's bought and
        // wishlisted products still excluded (a caller can
        // have purchases without having category signals).
        let mut items: Vec<Product> = sqlx::query_as(&format!(
            "SELECT {PRODUCT_COLUMNS} FROM products p \
             JOIN stores s ON s.id = p.store_id \
             {PRODUCT_RATING_JOIN} \
             WHERE p.status = 'active' AND s.status = 'active' AND p.admin_hidden_at IS NULL \
             AND {NOT_BOUGHT_BY_CALLER} AND {NOT_WISHLISTED_BY_CALLER} \
             ORDER BY rv.rating_avg DESC NULLS LAST, rv.rating_count DESC, \
             p.created_at DESC \
             LIMIT $2"
        ))
        .bind(user_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("recommendation fallback", e))?
        .into_iter()
        .map(with_stock_status)
        .collect();
        crate::merch::attach_flash_prices(pool, &mut items, chrono::Utc::now()).await?;
        return Ok(Recommendations {
            personalized: false,
            items,
        });
    }

    // Signals: each category with the timestamp of its most
    // recent signal (a delivered purchase line, or a
    // wishlist add). Candidates join on the category and
    // inherit its recency for ordering.
    let mut items: Vec<Product> = sqlx::query_as(&format!(
        "WITH signals AS (\
            SELECT lower(sig.category) AS category, MAX(sig.at) AS last_at \
            FROM (\
                SELECT p.category AS category, o.created_at AS at \
                FROM orders o \
                JOIN sub_orders so ON so.order_id = o.id \
                JOIN order_items oi ON oi.sub_order_id = so.id \
                JOIN products p ON p.id = oi.product_id \
                WHERE o.buyer_user_id = $1 AND so.status = 'delivered' \
                  AND p.category IS NOT NULL \
                UNION ALL \
                SELECT p.category AS category, w.created_at AS at \
                FROM wishlist_items w \
                JOIN products p ON p.id = w.product_id \
                WHERE w.user_id = $1 AND p.category IS NOT NULL\
            ) sig \
            GROUP BY lower(sig.category)\
         ) \
         SELECT {PRODUCT_COLUMNS} FROM products p \
         JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         JOIN signals ON lower(p.category) = signals.category \
         WHERE p.status = 'active' AND s.status = 'active' AND p.admin_hidden_at IS NULL \
         AND {NOT_BOUGHT_BY_CALLER} AND {NOT_WISHLISTED_BY_CALLER} \
         ORDER BY signals.last_at DESC, \
         rv.rating_avg DESC NULLS LAST, rv.rating_count DESC, p.created_at DESC \
         LIMIT $2"
    ))
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("personalised recommendations", e))?
    .into_iter()
    .map(with_stock_status)
    .collect();
    crate::merch::attach_flash_prices(pool, &mut items, chrono::Utc::now()).await?;
    Ok(Recommendations {
        personalized: true,
        items,
    })
}

// ---------------------------------------------------------------------------
// Request shapes + handlers (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LimitQuery {
    pub limit: Option<i64>,
}

pub async fn related_handler(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<RelatedList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(related_products(pool, id, query.limit).await?))
}

pub async fn also_bought_handler(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<AlsoBoughtList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(also_bought_products(pool, id, query.limit).await?))
}

pub async fn recommendations_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<LimitQuery>,
) -> Result<Json<Recommendations>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        recommendations_for_user(pool, user.user_id, query.limit).await?,
    ))
}
