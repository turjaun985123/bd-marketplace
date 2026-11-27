//! The wishlist (Wave 24) — the signed-in user's saved-for-later
//! list. It is account data, so it lives server-side (migration
//! 0025) and follows the user to any device they sign in on;
//! the Wave 24 recently-viewed rail is the deliberate opposite
//! (device-local localStorage, never sent to the server).
//!
//! Semantics:
//!
//! - Add requires the product to be PUBLICLY visible
//!   (`products::get_public_product` — active product of an
//!   active store). A draft, an archived product or a
//!   suspended store's product is a 404: a wishlist must not
//!   become a way to hold on to products the marketplace has
//!   hidden, and the 404 leaks nothing about which case it
//!   was.
//! - Add and remove are both IDEMPOTENT. Re-adding a saved
//!   product is a 200, not an error (the composite primary
//!   key plus ON CONFLICT DO NOTHING does this in one
//!   statement), and removing an absent product is a 200
//!   too — the response always states the resulting truth:
//!   `{wishlisted, product_id}`.
//! - The list returns the rows the user saved, newest first,
//!   as full shared product views. It does not re-filter by
//!   current visibility: the list is the user's own record of
//!   what they saved, and the product page itself enforces
//!   visibility when they click through.

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::products::{get_public_product, with_stock_status, Product, PRODUCT_COLUMNS, PRODUCT_RATING_JOIN};
use crate::{ApiError, AppState};

/// The answer to an add or a remove: the resulting state of
/// this product on the caller's wishlist.
#[derive(Debug, Serialize)]
pub struct WishlistToggle {
    pub wishlisted: bool,
    pub product_id: Uuid,
}

/// The caller's saved products, newest first, as full
/// product views (the shared shape every product surface
/// returns, rating summary and stock health included).
#[derive(Debug, Serialize)]
pub struct WishlistList {
    pub items: Vec<Product>,
}

/// Just the saved product ids — the lightweight read the
/// heart toggles on product cards need, without paying for
/// full product views on every page.
#[derive(Debug, Serialize)]
pub struct WishlistIds {
    pub product_ids: Vec<Uuid>,
}

/// Add one product to the user's wishlist. The product must
/// be publicly visible (see the module doc); the insert is
/// a single idempotent statement.
pub async fn add_to_wishlist(
    pool: &PgPool,
    user_id: Uuid,
    product_id: Uuid,
) -> Result<WishlistToggle, ApiError> {
    get_public_product(pool, product_id).await?;
    sqlx::query(
        "INSERT INTO wishlist_items (user_id, product_id) VALUES ($1, $2) \
         ON CONFLICT (user_id, product_id) DO NOTHING",
    )
    .bind(user_id)
    .bind(product_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("add to wishlist", e))?;
    Ok(WishlistToggle {
        wishlisted: true,
        product_id,
    })
}

/// Remove one product from the user's wishlist. Absent is
/// fine — the delete simply matches no row.
pub async fn remove_from_wishlist(
    pool: &PgPool,
    user_id: Uuid,
    product_id: Uuid,
) -> Result<WishlistToggle, ApiError> {
    sqlx::query("DELETE FROM wishlist_items WHERE user_id = $1 AND product_id = $2")
        .bind(user_id)
        .bind(product_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("remove from wishlist", e))?;
    Ok(WishlistToggle {
        wishlisted: false,
        product_id,
    })
}

/// The user's saved products as full product views, newest
/// first. Rows are the shared `PRODUCT_COLUMNS` plus the one
/// rating aggregate, joined through the wishlist table — the
/// same materialisation every other product surface uses.
pub async fn list_wishlist(pool: &PgPool, user_id: Uuid) -> Result<WishlistList, ApiError> {
    let mut items: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM wishlist_items w \
         JOIN products p ON p.id = w.product_id \
         JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE w.user_id = $1 ORDER BY w.created_at DESC"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list wishlist", e))?
    .into_iter()
    .map(with_stock_status)
    .collect();
    // Wave 52: wishlist rows carry live flash prices like
    // every other product surface.
    crate::merch::attach_flash_prices(pool, &mut items, chrono::Utc::now()).await?;
    Ok(WishlistList { items })
}

/// The user's saved product ids, newest first — nothing else.
pub async fn list_wishlist_ids(pool: &PgPool, user_id: Uuid) -> Result<WishlistIds, ApiError> {
    let product_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT product_id FROM wishlist_items WHERE user_id = $1 \
         ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list wishlist ids", e))?;
    Ok(WishlistIds { product_ids })
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn add_to_wishlist_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
) -> Result<Json<WishlistToggle>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        add_to_wishlist(pool, user.user_id, product_id).await?,
    ))
}

pub async fn remove_from_wishlist_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
) -> Result<Json<WishlistToggle>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        remove_from_wishlist(pool, user.user_id, product_id).await?,
    ))
}

pub async fn list_wishlist_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<WishlistList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(list_wishlist(pool, user.user_id).await?))
}

pub async fn list_wishlist_ids_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<WishlistIds>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(list_wishlist_ids(pool, user.user_id).await?))
}
