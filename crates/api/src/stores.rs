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
use domain::{
    is_valid_slug, validate_announcement, validate_branding_url, validate_hex_color,
    validate_store_description, validate_theme_key,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::{ApiError, AppState};

/// A row of the `stores` table; also the public store JSON shape.
/// `status` (Wave 10) is `active` or `suspended` — suspension is
/// the admin kill switch enforced in the product listing, product
/// detail and checkout paths (see the `admin` module).
/// `facebook_page_id` / `facebook_page_name` (Wave 12) are the
/// seller's linked Facebook Page identity, or null when no page
/// is linked; no page tokens are ever stored (see the `facebook`
/// module). `sidra_auto_reply` (Wave 13) is the store's SIDRA
/// mode: when true, SIDRA's AI replies enter the conversation
/// directly; when false, they wait as suggestions for the seller
/// (see the `messaging` module). The Wave 18 branding fields
/// (`theme_key`, `brand_color`, `logo_url`, `banner_url`,
/// `announcement`) are the storefront's look — validated values
/// only, never seller-supplied markup (see
/// [`update_store_branding_for_owner`]).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Store {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub facebook_page_id: Option<String>,
    pub facebook_page_name: Option<String>,
    pub sidra_auto_reply: bool,
    pub theme_key: String,
    pub brand_color: Option<String>,
    pub logo_url: Option<String>,
    pub banner_url: Option<String>,
    pub announcement: Option<String>,
    pub created_at: DateTime<Utc>,
}

const INSERT_STORE_SQL: &str = "INSERT INTO stores (owner_user_id, slug, name, description) \
    VALUES ($1, $2, $3, $4) \
    RETURNING id, owner_user_id, slug, name, description, status, \
    facebook_page_id, facebook_page_name, sidra_auto_reply, \
    theme_key, brand_color, logo_url, banner_url, announcement, created_at";
const SELECT_STORE_BY_SLUG_SQL: &str =
    "SELECT id, owner_user_id, slug, name, description, status, \
     facebook_page_id, facebook_page_name, sidra_auto_reply, \
     theme_key, brand_color, logo_url, banner_url, announcement, created_at \
     FROM stores WHERE slug = $1";
const SELECT_STORE_BY_ID_SQL: &str =
    "SELECT id, owner_user_id, slug, name, description, status, \
     facebook_page_id, facebook_page_name, sidra_auto_reply, \
     theme_key, brand_color, logo_url, banner_url, announcement, created_at \
     FROM stores WHERE id = $1";
const SELECT_STORE_BY_OWNER_SQL: &str = "SELECT id, owner_user_id, slug, name, description, \
    status, facebook_page_id, facebook_page_name, sidra_auto_reply, \
    theme_key, brand_color, logo_url, banner_url, announcement, created_at \
    FROM stores WHERE owner_user_id = $1 ORDER BY created_at ASC LIMIT 1";

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

/// Fetch the store a seller operates under: their earliest-created
/// store, or a 404 when they have none. Wave 9 scopes earnings and
/// payouts to this one store (payout accounts and payouts are
/// per-store rows), which matches the platform's one-storefront-
/// per-seller model everywhere else.
pub async fn get_store_by_owner(pool: &PgPool, owner_user_id: Uuid) -> Result<Store, ApiError> {
    sqlx::query_as(SELECT_STORE_BY_OWNER_SQL)
        .bind(owner_user_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load seller store", e))?
        .ok_or_else(|| ApiError::not_found("you do not have a store yet"))
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

/// One branding write: every field is optional, following the
/// codebase's PATCH convention (the same one product updates
/// use). A field ABSENT from the JSON body — or sent as JSON
/// `null`, which deserializes identically — leaves the stored
/// value unchanged. A field sent as a string is validated by
/// the pure domain validators and stored; for the nullable
/// fields (`brand_color`, `logo_url`, `banner_url`,
/// `announcement`, `description`) an empty or whitespace-only
/// string CLEARS the field back to NULL — that is the domain
/// validators' blank-to-`None` normalization doing the clearing,
/// and it is the one supported way to remove a logo, a banner,
/// a colour or a text. `theme_key` is NOT NULL, so it can be
/// changed but never cleared (a blank theme is a 400).
#[derive(Debug, Deserialize)]
pub struct UpdateBrandingRequest {
    pub theme_key: Option<String>,
    pub brand_color: Option<String>,
    pub logo_url: Option<String>,
    pub banner_url: Option<String>,
    pub announcement: Option<String>,
    pub description: Option<String>,
}

/// Update the branding of the caller's own store (their
/// earliest-created store, via [`get_store_by_owner`] — the same
/// scoping the Facebook link and SIDRA toggle use). Every
/// provided value passes its domain validator first, so an
/// invalid write is a 400 and changes nothing; the merged row
/// is then written in one UPDATE and returned as the shared
/// store view.
pub async fn update_store_branding_for_owner(
    pool: &PgPool,
    user: &AuthUser,
    patch: UpdateBrandingRequest,
) -> Result<Store, ApiError> {
    let existing = get_store_by_owner(pool, user.user_id).await?;

    let theme_key = match patch.theme_key.as_deref() {
        Some(raw) => validate_theme_key(raw)
            .map_err(ApiError::bad_request)?
            .to_string(),
        None => existing.theme_key.clone(),
    };
    // A provided blank colour clears the accent; a provided
    // non-blank one must be a real #RRGGBB (the validator
    // normalizes it to uppercase for storage).
    let brand_color = match patch.brand_color.as_deref() {
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => Some(validate_hex_color(raw).map_err(ApiError::bad_request)?),
        None => existing.brand_color.clone(),
    };
    let logo_url = match patch.logo_url.as_deref() {
        Some(raw) => validate_branding_url(Some(raw)).map_err(ApiError::bad_request)?,
        None => existing.logo_url.clone(),
    };
    let banner_url = match patch.banner_url.as_deref() {
        Some(raw) => validate_branding_url(Some(raw)).map_err(ApiError::bad_request)?,
        None => existing.banner_url.clone(),
    };
    let announcement = match patch.announcement.as_deref() {
        Some(raw) => validate_announcement(Some(raw)).map_err(ApiError::bad_request)?,
        None => existing.announcement.clone(),
    };
    let description = match patch.description.as_deref() {
        Some(raw) => validate_store_description(Some(raw)).map_err(ApiError::bad_request)?,
        None => existing.description.clone(),
    };

    sqlx::query(
        "UPDATE stores SET theme_key = $2, brand_color = $3, logo_url = $4, \
         banner_url = $5, announcement = $6, description = $7 WHERE id = $1",
    )
    .bind(existing.id)
    .bind(&theme_key)
    .bind(&brand_color)
    .bind(&logo_url)
    .bind(&banner_url)
    .bind(&announcement)
    .bind(&description)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("update store branding", e))?;

    get_store_by_id(pool, existing.id).await
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

pub async fn update_store_branding(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<UpdateBrandingRequest>,
) -> Result<Json<Store>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        update_store_branding_for_owner(pool, &user, req).await?,
    ))
}
