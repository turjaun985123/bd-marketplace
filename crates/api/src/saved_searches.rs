//! Saved searches (Wave 53, migration 0048) — a signed-in
//! buyer bookmarks a faceted-browse filter set under a
//! name and re-applies it later from any device.
//!
//! Semantics:
//!
//! - ACCOUNT DATA, like the wishlist: it follows the
//!   user, so it lives server-side. (The Wave 24
//!   recently-viewed history is the deliberate opposite —
//!   device-local, never sent to the server.)
//! - The stored filter is the CANONICAL validated shape:
//!   the create path runs the raw input through the same
//!   domain validator as `GET /v1/search`
//!   (`validate_browse_filters`) and stores what the
//!   validator produced, rendered back to the filter JSON
//!   by [`SavedSearchFilters::from_validated`]. A stored
//!   row can therefore never carry a filter the live
//!   endpoint would refuse, and applying one is just
//!   building the browse URL from it.
//! - Pagination is NOT part of a saved search: a bookmark
//!   is a filter set, and applying it starts at page 1.
//! - The per-user cap ([`MAX_SAVED_SEARCHES_PER_USER`])
//!   is enforced in the create transaction's count, so the
//!   list stays a list.
//! - Delete is owner-only by construction (the DELETE
//!   carries the caller's user id); another user's saved
//!   search is a 404, indistinguishable from a missing
//!   one — the same no-leak rule as the wishlist's
//!   visibility 404s.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    search_sort_key, validate_browse_filters, validate_saved_search_name, BrowseFilterInput,
    BrowseFilters, MAX_SAVED_SEARCHES_PER_USER,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::{ApiError, AppState};

/// A saved search's filter set, as stored and as returned:
/// exactly the browse filters, minus pagination. This is
/// both the create body's `filters` shape and the stored
/// JSON's shape — one struct for the whole lifecycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedSearchFilters {
    /// The search text; null for a pure filter browse.
    pub q: Option<String>,
    /// Category labels to match (OR).
    #[serde(default)]
    pub categories: Vec<String>,
    /// Store slugs to match (OR).
    #[serde(default)]
    pub store_slugs: Vec<String>,
    /// Inclusive lower effective-price bound, in poisha.
    pub min_price_poisha: Option<i64>,
    /// Inclusive upper effective-price bound, in poisha.
    pub max_price_poisha: Option<i64>,
    /// Minimum rating, 0..=5.
    pub min_rating: Option<f64>,
    /// Keep only in-stock products.
    #[serde(default)]
    pub in_stock: bool,
    /// Keep only on-sale products.
    #[serde(default)]
    pub on_sale: bool,
    /// The sort key (`relevance`, `price_asc`, ...).
    pub sort: Option<String>,
}

impl SavedSearchFilters {
    /// The raw validator input this filter set represents:
    /// booleans rendered back to the query-string spelling
    /// the domain parser accepts, pagination absent (a
    /// saved search starts at page 1 by definition).
    fn to_input(&self) -> BrowseFilterInput {
        BrowseFilterInput {
            q: self.q.clone(),
            categories: self.categories.clone(),
            store_slugs: self.store_slugs.clone(),
            min_price_poisha: self.min_price_poisha,
            max_price_poisha: self.max_price_poisha,
            min_rating: self.min_rating,
            in_stock: Some(self.in_stock.to_string()),
            on_sale: Some(self.on_sale.to_string()),
            sort: self.sort.clone(),
            page: None,
            per_page: None,
            limit: None,
        }
    }

    /// Render a validated filter set back to the stored
    /// shape: normalised lists, the canonical sort key,
    /// and the sort spelled out (never null) so an applied
    /// search reproduces the saved ordering exactly, even
    /// if the endpoint's defaults ever move.
    pub(crate) fn from_validated(filters: &BrowseFilters) -> Self {
        SavedSearchFilters {
            q: filters.q.clone(),
            categories: filters.categories.clone(),
            store_slugs: filters.store_slugs.clone(),
            min_price_poisha: filters.min_price_poisha,
            max_price_poisha: filters.max_price_poisha,
            min_rating: filters.min_rating,
            in_stock: filters.in_stock,
            on_sale: filters.on_sale,
            sort: Some(search_sort_key(filters.sort).to_string()),
        }
    }

    /// Validate a raw stored-shape filter set through the
    /// one browse validator and return the canonical shape
    /// to store.
    fn validated(self) -> Result<SavedSearchFilters, ApiError> {
        let filters = validate_browse_filters(self.to_input()).map_err(ApiError::bad_request)?;
        Ok(SavedSearchFilters::from_validated(&filters))
    }
}

/// One saved search, as the API serves it.
#[derive(Debug, Serialize)]
pub struct SavedSearch {
    pub id: Uuid,
    pub name: String,
    pub filters: SavedSearchFilters,
    pub created_at: DateTime<Utc>,
}

/// The stored row: `filters` rides as raw JSON and is
/// decoded by the service (a row that somehow fails to
/// decode is a server error, never a silent empty filter).
#[derive(Debug, FromRow)]
struct SavedSearchRow {
    id: Uuid,
    name: String,
    filters: serde_json::Value,
    created_at: DateTime<Utc>,
}

impl SavedSearchRow {
    fn into_view(self) -> Result<SavedSearch, ApiError> {
        let filters: SavedSearchFilters = serde_json::from_value(self.filters)
            .map_err(|e| ApiError::internal_any("decode saved search filters", e))?;
        Ok(SavedSearch {
            id: self.id,
            name: self.name,
            filters,
            created_at: self.created_at,
        })
    }
}

/// The caller's saved searches, newest first.
#[derive(Debug, Serialize)]
pub struct SavedSearchList {
    pub items: Vec<SavedSearch>,
    pub total: i64,
}

/// List the caller's saved searches.
pub async fn list_saved_searches(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<SavedSearchList, ApiError> {
    let rows: Vec<SavedSearchRow> = sqlx::query_as(
        "SELECT id, name, filters, created_at FROM saved_searches \
         WHERE user_id = $1 ORDER BY created_at DESC, id DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list saved searches", e))?;
    let total = rows.len() as i64;
    let items = rows
        .into_iter()
        .map(SavedSearchRow::into_view)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SavedSearchList { items, total })
}

/// Create one saved search for the caller: validate the
/// name and the filter set (through the one browse
/// validator), then insert under the per-user cap — the
/// count and the insert share one transaction so two
/// racing creates cannot both slip past the cap.
pub async fn create_saved_search(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    filters: SavedSearchFilters,
) -> Result<SavedSearch, ApiError> {
    let name = validate_saved_search_name(name).map_err(ApiError::bad_request)?;
    let filters = filters.validated()?;
    let filters_json = serde_json::to_value(&filters)
        .map_err(|e| ApiError::internal_any("encode saved search filters", e))?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin saved search create", e))?;
    // Lock the caller's own user row for the transaction:
    // the count below is then stable against a second
    // create racing from the same account (an aggregate
    // cannot carry FOR UPDATE itself).
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("lock saver row", e))?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM saved_searches WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("count saved searches", e))?;
    if count >= MAX_SAVED_SEARCHES_PER_USER {
        return Err(ApiError::bad_request(format!(
            "you can save at most {MAX_SAVED_SEARCHES_PER_USER} searches — \
             delete one before saving another — \
             সর্বোচ্চ {MAX_SAVED_SEARCHES_PER_USER}টি খোঁজ সংরক্ষণ করা যাবে, \
             নতুনটির আগে একটি মুছে ফেলুন"
        )));
    }
    let row: SavedSearchRow = sqlx::query_as(
        "INSERT INTO saved_searches (user_id, name, filters) \
         VALUES ($1, $2, $3) RETURNING id, name, filters, created_at",
    )
    .bind(user_id)
    .bind(&name)
    .bind(&filters_json)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("insert saved search", e))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit saved search create", e))?;
    row.into_view()
}

/// Delete one of the caller's saved searches. Another
/// user's row and a missing row are the same 404.
pub async fn delete_saved_search(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<(), ApiError> {
    let deleted = sqlx::query("DELETE FROM saved_searches WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("delete saved search", e))?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::not_found("saved search not found"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Request shapes + handlers (thin)
// ---------------------------------------------------------------------------

/// The create body: a name plus the filter set.
#[derive(Deserialize)]
pub struct CreateSavedSearchInput {
    pub name: String,
    pub filters: SavedSearchFilters,
}

pub async fn list_saved_searches_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<SavedSearchList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(list_saved_searches(pool, user.user_id).await?))
}

pub async fn create_saved_search_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<CreateSavedSearchInput>,
) -> Result<(StatusCode, Json<SavedSearch>), ApiError> {
    let pool = state.require_pool()?;
    let saved = create_saved_search(pool, user.user_id, &input.name, input.filters).await?;
    Ok((StatusCode::CREATED, Json(saved)))
}

pub async fn delete_saved_search_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let pool = state.require_pool()?;
    delete_saved_search(pool, user.user_id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
