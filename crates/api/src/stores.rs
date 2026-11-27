//! Stores: a seller's storefront. Creating a store is what makes a user
//! a seller — there is no separate "become a seller" endpoint; the first
//! store creation upgrades the user's role to `seller` (admins keep
//! their role).

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    is_valid_slug, mask_phone, normalize_bd_phone, validate_announcement, validate_branding_url,
    validate_hex_color, validate_store_about, validate_store_description,
    validate_store_return_policy, validate_store_role, validate_theme_key,
    STORE_ACTION_MANAGE_MARKETING, STORE_ACTION_MANAGE_MEMBERS,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::access::{require_store_action, resolve_scoped_store, seller_access, StoreScopeQuery};
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
/// [`update_store_branding_for_owner`]). `verified_tier`
/// (Wave 21) is the store's KYC trust tier — `unverified`,
/// `basic` or `verified` — earned through document review (see
/// the `verification` module); it is display metadata here, and
/// the payout gate reads it at payout creation time.
/// `about` / `return_policy` (Wave 54) are the seller's two
/// long profile texts — the store's story and its own returns
/// wording — written through the same branding write as the
/// look (domain `validate_store_about` /
/// `validate_store_return_policy`); null when unwritten, and
/// the storefront hides an unwritten text's section entirely.
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
    pub verified_tier: String,
    pub about: Option<String>,
    pub return_policy: Option<String>,
    pub created_at: DateTime<Utc>,
}

const INSERT_STORE_SQL: &str = "INSERT INTO stores (owner_user_id, slug, name, description) \
    VALUES ($1, $2, $3, $4) \
    RETURNING id, owner_user_id, slug, name, description, status, \
    facebook_page_id, facebook_page_name, sidra_auto_reply, \
    theme_key, brand_color, logo_url, banner_url, announcement, verified_tier, \
    about, return_policy, created_at";
const SELECT_STORE_BY_SLUG_SQL: &str =
    "SELECT id, owner_user_id, slug, name, description, status, \
     facebook_page_id, facebook_page_name, sidra_auto_reply, \
     theme_key, brand_color, logo_url, banner_url, announcement, verified_tier, \
     about, return_policy, created_at \
     FROM stores WHERE slug = $1";
const SELECT_STORE_BY_ID_SQL: &str =
    "SELECT id, owner_user_id, slug, name, description, status, \
     facebook_page_id, facebook_page_name, sidra_auto_reply, \
     theme_key, brand_color, logo_url, banner_url, announcement, verified_tier, \
     about, return_policy, created_at \
     FROM stores WHERE id = $1";
const SELECT_STORE_BY_OWNER_SQL: &str = "SELECT id, owner_user_id, slug, name, description, \
    status, facebook_page_id, facebook_page_name, sidra_auto_reply, \
    theme_key, brand_color, logo_url, banner_url, announcement, verified_tier, \
    about, return_policy, created_at \
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
    /// Wave 54: the store's long "about" text; same
    /// convention as `description` (absent keeps, blank
    /// clears, a value is validated and stored).
    pub about: Option<String>,
    /// Wave 54: the store's own return-policy text; same
    /// convention as `about`.
    pub return_policy: Option<String>,
}

/// Update a store's branding. Every provided value passes
/// its domain validator first, so an invalid write is a 400
/// and changes nothing; the merged row is then written in
/// one UPDATE and returned as the shared store view. Wave
/// 54 adds the two long profile texts (`about`,
/// `return_policy`) to the same write and the same
/// convention. The
/// store arrives already resolved by the handler: before
/// Wave 34 that was always the caller's own earliest store
/// (via [`get_store_by_owner`]); Wave 34 also lets a manager
/// brand a store they serve, named by `?store_id=` under a
/// `manage_marketing` role — this function only merges,
/// validates and writes.
pub async fn update_store_branding_for_store(
    pool: &PgPool,
    existing: &Store,
    patch: UpdateBrandingRequest,
) -> Result<Store, ApiError> {
    let existing = existing.clone();

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
    // Wave 54: the two long profile texts ride the same
    // write — one settings save covers the whole storefront
    // identity, and each text follows the same convention
    // (absent keeps, blank clears, value validates).
    let about = match patch.about.as_deref() {
        Some(raw) => validate_store_about(Some(raw)).map_err(ApiError::bad_request)?,
        None => existing.about.clone(),
    };
    let return_policy = match patch.return_policy.as_deref() {
        Some(raw) => validate_store_return_policy(Some(raw)).map_err(ApiError::bad_request)?,
        None => existing.return_policy.clone(),
    };

    sqlx::query(
        "UPDATE stores SET theme_key = $2, brand_color = $3, logo_url = $4, \
         banner_url = $5, announcement = $6, description = $7, \
         about = $8, return_policy = $9 WHERE id = $1",
    )
    .bind(existing.id)
    .bind(&theme_key)
    .bind(&brand_color)
    .bind(&logo_url)
    .bind(&banner_url)
    .bind(&announcement)
    .bind(&description)
    .bind(&about)
    .bind(&return_policy)
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

/// The PUBLIC store view (Wave 54): the store row plus
/// the facts a storefront header is built from — the live
/// follower count, whether the VIEWER follows the store,
/// and the store's trust aggregates (its products' rating
/// summary, its delivered units, its active catalogue
/// size). Every number is a live aggregate over the same
/// tables the surfaces themselves read — there is no
/// cached counter anywhere in this view.
#[derive(Debug, Serialize)]
pub struct PublicStore {
    #[serde(flatten)]
    pub store: Store,
    pub follower_count: i64,
    pub is_following: bool,
    pub rating_avg: Option<f64>,
    pub rating_count: i64,
    pub total_sold: i64,
    pub product_count: i64,
}

/// The aggregate half of [`PublicStore`], as one row.
#[derive(Debug, FromRow)]
struct StorePublicStats {
    rating_avg: Option<f64>,
    rating_count: i64,
    total_sold: i64,
    product_count: i64,
}

/// One store's public aggregates (Wave 54): rating over
/// the reviews of ALL its products (the same ROUND-to-one-
/// decimal rule as the product rating join), delivered
/// units over its products (the platform's one "sold"
/// definition), and its active product count. Four scalar
/// subqueries in one round-trip.
async fn store_public_stats(pool: &PgPool, store_id: Uuid) -> Result<StorePublicStats, ApiError> {
    sqlx::query_as(
        "SELECT \
         (SELECT CAST(ROUND(AVG(r.rating)::numeric, 1) AS float8) \
          FROM reviews r JOIN products p ON p.id = r.product_id \
          WHERE p.store_id = $1) AS rating_avg, \
         (SELECT COUNT(*) FROM reviews r \
          JOIN products p ON p.id = r.product_id \
          WHERE p.store_id = $1) AS rating_count, \
         (SELECT COALESCE(SUM(oi.quantity), 0)::bigint \
          FROM order_items oi \
          JOIN sub_orders so ON so.id = oi.sub_order_id \
          JOIN products p ON p.id = oi.product_id \
          WHERE p.store_id = $1 AND so.status = 'delivered') AS total_sold, \
         (SELECT COUNT(*) FROM products p \
          WHERE p.store_id = $1 AND p.status = 'active') AS product_count",
    )
    .bind(store_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("load store public stats", e))
}

/// Build the public view of one store for one viewer
/// (Wave 54): the store by slug (404 when missing), its
/// follower count, the viewer's own follow state (false
/// for anonymous viewers), and its aggregates.
pub async fn get_public_store(
    pool: &PgPool,
    slug: &str,
    viewer_user_id: Option<Uuid>,
) -> Result<PublicStore, ApiError> {
    let store = get_store_by_slug(pool, slug).await?;
    let follower_count = crate::follows::follower_count(pool, store.id).await?;
    let is_following = match viewer_user_id {
        Some(user_id) => crate::follows::is_following(pool, store.id, user_id).await?,
        None => false,
    };
    let stats = store_public_stats(pool, store.id).await?;
    Ok(PublicStore {
        store,
        follower_count,
        is_following,
        rating_avg: stats.rating_avg,
        rating_count: stats.rating_count,
        total_sold: stats.total_sold,
        product_count: stats.product_count,
    })
}

pub async fn get_store(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Result<Json<PublicStore>, ApiError> {
    let pool = state.require_pool()?;
    // Wave 54: the public view is viewer-aware (the
    // follow state) — an optional read, exactly like the
    // product detail's: a bad or absent token simply means
    // an anonymous viewer, never an error.
    let viewer = crate::auth::authenticate_optional(&state, &headers).map(|u| u.user_id);
    Ok(Json(get_public_store(pool, &slug, viewer).await?))
}

pub async fn update_store_branding(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Json(req): Json<UpdateBrandingRequest>,
) -> Result<Json<Store>, ApiError> {
    let pool = state.require_pool()?;
    // Wave 34: branding is marketing work — with `?store_id=`
    // a manager may brand a store they serve; without it the
    // caller's own store is branded, as before.
    let (store, _role) = resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        STORE_ACTION_MANAGE_MARKETING,
    )
    .await?;
    Ok(Json(
        update_store_branding_for_store(pool, &store, req).await?,
    ))
}

// ---------------------------------------------------------------------------
// Wave 34 — the seller's stores + store members
// ---------------------------------------------------------------------------

/// One store in the seller's store list: the store basics
/// plus the role the CALLER holds in it (`owner` for their
/// own stores, the member role for stores they serve).
#[derive(Debug, Serialize, FromRow)]
pub struct SellerStore {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub status: String,
    pub verified_tier: String,
    pub my_role: String,
}

/// Every store the caller owns or is a member of, owned
/// stores first (earliest first), then member stores
/// (earliest membership first). The store switcher and the
/// members page are driven by this one list.
pub async fn list_seller_stores(pool: &PgPool, user: &AuthUser) -> Result<Vec<SellerStore>, ApiError> {
    sqlx::query_as(
        "SELECT id, slug, name, status, verified_tier, my_role FROM ( \
           SELECT s.id, s.slug, s.name, s.status, s.verified_tier, \
                  'owner' AS my_role, 0 AS grp, s.created_at AS sort_at \
           FROM stores s WHERE s.owner_user_id = $1 \
           UNION ALL \
           SELECT s.id, s.slug, s.name, s.status, s.verified_tier, \
                  m.role AS my_role, 1 AS grp, m.created_at AS sort_at \
           FROM store_members m JOIN stores s ON s.id = m.store_id \
           WHERE m.user_id = $1 \
         ) combined ORDER BY grp, sort_at, id",
    )
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list seller stores", e))
}

/// One member of a store, as the members endpoints return
/// them: the member's user id, their phone MASKED (the full
/// number is the member's own data, not the team's), their
/// role and when they were seated.
#[derive(Debug, Serialize, FromRow)]
pub struct StoreMember {
    pub user_id: Uuid,
    pub phone_masked: String,
    pub role: String,
    pub created_at: DateTime<Utc>,
}

/// One member row before its phone is masked.
#[derive(Debug, FromRow)]
struct StoreMemberRow {
    user_id: Uuid,
    phone: String,
    role: String,
    created_at: DateTime<Utc>,
}

impl From<StoreMemberRow> for StoreMember {
    fn from(row: StoreMemberRow) -> Self {
        StoreMember {
            user_id: row.user_id,
            phone_masked: mask_phone(&row.phone),
            role: row.role,
            created_at: row.created_at,
        }
    }
}

const MEMBER_COLUMNS: &str = "m.user_id, u.phone, m.role, m.created_at";
const MEMBER_FROM: &str = "FROM store_members m JOIN users u ON u.id = m.user_id";

/// The members of one store, earliest seated first.
pub async fn list_store_members(
    pool: &PgPool,
    store_id: Uuid,
) -> Result<Vec<StoreMember>, ApiError> {
    let rows: Vec<StoreMemberRow> = sqlx::query_as(&format!(
        "SELECT {MEMBER_COLUMNS} {MEMBER_FROM} \
         WHERE m.store_id = $1 ORDER BY m.created_at ASC, m.user_id ASC"
    ))
    .bind(store_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list store members", e))?;
    Ok(rows.into_iter().map(StoreMember::from).collect())
}

/// Load one member row of a store (404 when the user is not
/// a member of it).
async fn load_store_member(
    pool: &PgPool,
    store_id: Uuid,
    member_user_id: Uuid,
) -> Result<StoreMember, ApiError> {
    let row: Option<StoreMemberRow> = sqlx::query_as(&format!(
        "SELECT {MEMBER_COLUMNS} {MEMBER_FROM} WHERE m.store_id = $1 AND m.user_id = $2"
    ))
    .bind(store_id)
    .bind(member_user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load store member", e))?;
    row.map(StoreMember::from)
        .ok_or_else(|| ApiError::not_found("member not found"))
}

/// Seat a new member in a store. The target is found by
/// phone (normalized by the domain rule): an unknown number
/// is a 404 — membership never creates an account silently,
/// the person must already be a platform user. Adding the
/// store's owner is a 400 (they already hold every right,
/// and `owner` is not a member role), an invalid role is a
/// 400 from the domain validator, and seating the same user
/// twice is a 409.
pub async fn add_store_member(
    pool: &PgPool,
    store_id: Uuid,
    phone: &str,
    role: &str,
) -> Result<StoreMember, ApiError> {
    let role = validate_store_role(role).map_err(ApiError::bad_request)?;
    let phone = normalize_bd_phone(phone).ok_or_else(|| {
        ApiError::bad_request("phone must be a valid Bangladeshi mobile number")
    })?;

    let target: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM users WHERE phone = $1")
            .bind(&phone)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("find member by phone", e))?;
    let (target_user_id,) = target.ok_or_else(|| {
        ApiError::not_found("এই নম্বরে কোনো অ্যাকাউন্ট নেই — no account exists for this phone number")
    })?;

    let store = get_store_by_id(pool, store_id).await?;
    if store.owner_user_id == target_user_id {
        return Err(ApiError::bad_request(
            "the store owner is already on the team with every right — \
             স্টোরের মালিক নিজে সদস্য হতে পারেন না",
        ));
    }

    let existing: Option<String> =
        sqlx::query_scalar("SELECT role FROM store_members WHERE store_id = $1 AND user_id = $2")
            .bind(store_id)
            .bind(target_user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("check existing membership", e))?;
    if existing.is_some() {
        return Err(ApiError::conflict(
            "this user is already a member of the store",
        ));
    }

    sqlx::query(
        "INSERT INTO store_members (store_id, user_id, role) VALUES ($1, $2, $3)",
    )
    .bind(store_id)
    .bind(target_user_id)
    .bind(role)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("add store member", e))?;

    load_store_member(pool, store_id, target_user_id).await
}

/// Change a member's role (owner only, enforced by the
/// handler's gate). A user who is not a member is a 404.
pub async fn update_store_member_role(
    pool: &PgPool,
    store_id: Uuid,
    member_user_id: Uuid,
    role: &str,
) -> Result<StoreMember, ApiError> {
    let role = validate_store_role(role).map_err(ApiError::bad_request)?;
    load_store_member(pool, store_id, member_user_id).await?;
    sqlx::query("UPDATE store_members SET role = $3 WHERE store_id = $1 AND user_id = $2")
        .bind(store_id)
        .bind(member_user_id)
        .bind(role)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("update store member role", e))?;
    load_store_member(pool, store_id, member_user_id).await
}

/// Remove a member from a store (owner only, enforced by the
/// handler's gate). Removing someone who is not a member is
/// a 404 — a mistyped id must not look like a success.
pub async fn remove_store_member(
    pool: &PgPool,
    store_id: Uuid,
    member_user_id: Uuid,
) -> Result<(), ApiError> {
    let deleted = sqlx::query("DELETE FROM store_members WHERE store_id = $1 AND user_id = $2")
        .bind(store_id)
        .bind(member_user_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("remove store member", e))?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::not_found("member not found"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Member handlers (thin)
// ---------------------------------------------------------------------------

pub async fn list_seller_stores_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<SellerStore>>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(list_seller_stores(pool, &user).await?))
}

#[derive(Deserialize)]
pub struct AddStoreMemberRequest {
    pub phone: String,
    pub role: String,
}

pub async fn add_store_member_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(store_id): Path<Uuid>,
    Json(req): Json<AddStoreMemberRequest>,
) -> Result<(StatusCode, Json<StoreMember>), ApiError> {
    let pool = state.require_pool()?;
    // Seating people is the owner's alone (manage_members —
    // even managers are refused, with a 403 from the matrix).
    require_store_action(pool, user.user_id, store_id, STORE_ACTION_MANAGE_MEMBERS).await?;
    let member = add_store_member(pool, store_id, &req.phone, &req.role).await?;
    Ok((StatusCode::CREATED, Json(member)))
}

pub async fn list_store_members_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(store_id): Path<Uuid>,
) -> Result<Json<Vec<StoreMember>>, ApiError> {
    let pool = state.require_pool()?;
    // Viewing the team list takes a seat at the table: the
    // owner or a manager (a manager runs the shop and must
    // know who they work with). Staff hold no such right in
    // the matrix, so a staff read is a 403 here.
    let role = seller_access(pool, user.user_id, store_id).await?;
    if role != "owner" && role != "manager" {
        return Err(ApiError::forbidden(
            "only the owner and managers can view the team — \
             শুধু মালিক ও ম্যানেজাররাই টিমের তালিকা দেখতে পারেন",
        ));
    }
    Ok(Json(list_store_members(pool, store_id).await?))
}

#[derive(Deserialize)]
pub struct UpdateStoreMemberRequest {
    pub role: String,
}

pub async fn update_store_member_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path((store_id, member_user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateStoreMemberRequest>,
) -> Result<Json<StoreMember>, ApiError> {
    let pool = state.require_pool()?;
    require_store_action(pool, user.user_id, store_id, STORE_ACTION_MANAGE_MEMBERS).await?;
    Ok(Json(
        update_store_member_role(pool, store_id, member_user_id, &req.role).await?,
    ))
}

pub async fn remove_store_member_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path((store_id, member_user_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let pool = state.require_pool()?;
    require_store_action(pool, user.user_id, store_id, STORE_ACTION_MANAGE_MEMBERS).await?;
    remove_store_member(pool, store_id, member_user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
