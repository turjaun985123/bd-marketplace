//! Stores: a seller's storefront. Creating a store is what makes a user
//! a seller — there is no separate "become a seller" endpoint; the first
//! store creation upgrades the user's role to `seller` (admins keep
//! their role).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::is_valid_slug;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::{ApiError, AppState};

/// A row of the `stores` table; also the public store JSON shape.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Store {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

const INSERT_STORE_SQL: &str = "INSERT INTO stores (owner_user_id, slug, name, description) \
    VALUES ($1, $2, $3, $4) \
    RETURNING id, owner_user_id, slug, name, description, created_at";
const SELECT_STORE_BY_SLUG_SQL: &str =
    "SELECT id, owner_user_id, slug, name, description, created_at FROM stores WHERE slug = $1";
const SELECT_STORE_BY_ID_SQL: &str =
    "SELECT id, owner_user_id, slug, name, description, created_at FROM stores WHERE id = $1";

/// Create a store for the authenticated user. Validates the slug with
/// the pure domain function, maps a duplicate slug to 409, and upgrades
/// the owner's role to `seller`.
pub async fn create_store_for_user(
    pool: &PgPool,
    owner: &AuthUser,
    name: &str,
    slug: &str,
    description: Option<&str>,
) -> Result<Store, ApiError> {
    if name.trim().is_empty() {
        return Err(ApiError::bad_request("store name must not be empty"));
    }
    if !is_valid_slug(slug) {
        return Err(ApiError::bad_request(
            "slug must be 3-40 chars of lowercase letters, digits and hyphens",
        ));
    }

    let store: Store = sqlx::query_as(INSERT_STORE_SQL)
        .bind(owner.user_id)
        .bind(slug)
        .bind(name.trim())
        .bind(description)
        .fetch_one(pool)
    .await
    .map_err(|e| {
        // Postgres unique-violation (e.g. duplicate slug) -> 409.
        let is_unique_violation = e
            .as_database_error()
            .map(|db_err| db_err.code().as_deref() == Some("23505"))
            .unwrap_or(false);
        if is_unique_violation {
            ApiError::conflict(format!("a store with slug '{slug}' already exists"))
        } else {
            ApiError::internal("create store", e)
        }
    })?;

    // Store creation implies seller: upgrade buyers, never demote admins.
    sqlx::query("UPDATE users SET role = 'seller' WHERE id = $1 AND role = 'buyer'")
        .bind(owner.user_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("upgrade user role", e))?;

    Ok(store)
}

/// Fetch a store by its slug (public storefront identity).
pub async fn get_store_by_slug(pool: &PgPool, slug: &str) -> Result<Store, ApiError> {
    sqlx::query_as(SELECT_STORE_BY_SLUG_SQL)
        .bind(slug)
        .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load store", e))?
    .ok_or_else(|| ApiError::not_found("store not found"))
}

/// Fetch a store by id (used for product ownership checks).
pub async fn get_store_by_id(pool: &PgPool, id: Uuid) -> Result<Store, ApiError> {
    sqlx::query_as(SELECT_STORE_BY_ID_SQL)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load store", e))?
        .ok_or_else(|| ApiError::not_found("store not found"))
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateStoreRequest {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
}

pub async fn create_store(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateStoreRequest>,
) -> Result<(StatusCode, Json<Store>), ApiError> {
    let pool = state.require_pool()?;
    let store = create_store_for_user(
        pool,
        &user,
        &req.name,
        &req.slug,
        req.description.as_deref(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(store)))
}

pub async fn get_store(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Store>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_store_by_slug(pool, &slug).await?))
}
