//! Buyer address book (Wave 55): the signed-in buyer's
//! saved delivery addresses, plus the public district
//! table both the book and checkout read.
//!
//! The rules, each enforced in exactly one place:
//! - Every write passes the domain's one validation
//!   (`validate_address_input`): real BD phone (stored
//!   canonical), a district from the domain's 64-district
//!   table, and every text field present within its cap.
//!   The zone is NEVER accepted from the client and never
//!   stored — it derives from the district through the
//!   domain table at read time, so a mapping change moves
//!   every saved address at once.
//! - A buyer keeps at most `MAX_ADDRESSES_PER_USER`
//!   addresses (the domain cap).
//! - Exactly one address may be the default (the partial
//!   unique index in migration 0057 makes it a database
//!   fact; the writes below switch it inside one
//!   transaction). The FIRST address a buyer saves
//!   becomes the default automatically — a one-address
//!   book with no default would make the checkout picker
//!   pointless. Deleting the default leaves NO default
//!   (no silent promotion of an address the buyer did
//!   not choose).
//! - Rows are the owner's alone: another buyer's id is a
//!   404, exactly like buyer orders — the book must not
//!   become an oracle for other people's addresses.
//! - The list orders default-first, then most recently
//!   USED at checkout (`last_used_at`, stamped by
//!   `place_order`), then newest — the picker shows the
//!   address the buyer actually ships to at the top.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    compose_delivery_address, district_by_name, search_districts, validate_address_count,
    validate_address_input, AddressInput, District, DISTRICTS,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One saved address as the API serves it: the stored
/// fields plus the derived facts (district's Bangla
/// name, its zone, and the composed one-line address an
/// order would store).
#[derive(Debug, Serialize)]
pub struct AddressView {
    pub id: Uuid,
    pub label: String,
    pub recipient_name: String,
    /// Canonical phone (`+8801…`).
    pub phone: String,
    /// Canonical English district name.
    pub district: String,
    /// The district's Bangla name; null only if a stored
    /// district somehow fell out of the domain table
    /// (writes validate against it, so this is defensive).
    pub district_bn: Option<String>,
    /// The district's delivery zone (derived, never
    /// stored): inside_dhaka | dhaka_suburb | outside_dhaka.
    pub zone: String,
    pub area: String,
    pub address_line: String,
    /// `address_line, area, district` — the exact string
    /// checkout stores on the order.
    pub full_address: String,
    pub is_default: bool,
    /// When an order last shipped to this address
    /// (stamped by checkout); null until first use.
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The address book page: every address the buyer keeps,
/// in picker order (default, recently used, newest).
#[derive(Debug, Serialize)]
pub struct AddressList {
    pub items: Vec<AddressView>,
    pub total: i64,
}

/// One district of the public table (Wave 55).
#[derive(Debug, Serialize)]
pub struct DistrictView {
    pub name_en: String,
    pub name_bn: String,
    /// inside_dhaka | dhaka_suburb | outside_dhaka.
    pub zone: String,
}

/// The public district table (Wave 55).
#[derive(Debug, Serialize)]
pub struct DistrictList {
    pub items: Vec<DistrictView>,
    pub total: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct AddressRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub label: String,
    pub recipient_name: String,
    pub phone: String,
    pub district: String,
    pub area: String,
    pub address_line: String,
    pub is_default: bool,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const ADDRESS_COLUMNS: &str = "id, user_id, label, recipient_name, phone, district, \
    area, address_line, is_default, last_used_at, created_at, updated_at";

impl AddressRow {
    /// The served view: stored fields + the district's
    /// derived facts.
    pub fn to_view(&self) -> AddressView {
        let district = district_by_name(&self.district);
        AddressView {
            id: self.id,
            label: self.label.clone(),
            recipient_name: self.recipient_name.clone(),
            phone: self.phone.clone(),
            district: self.district.clone(),
            district_bn: district.map(|d| d.name_bn.to_string()),
            zone: district
                .map(|d| d.zone.as_str().to_string())
                // A stored district outside the table can
                // only be legacy data; the zone falls back
                // to the widest one rather than guessing
                // a cheaper zone the district never earned.
                .unwrap_or_else(|| "outside_dhaka".to_string()),
            area: self.area.clone(),
            address_line: self.address_line.clone(),
            full_address: compose_delivery_address(
                &self.address_line,
                &self.area,
                &self.district,
            ),
            is_default: self.is_default,
            last_used_at: self.last_used_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Service functions (one per operation)
// ---------------------------------------------------------------------------

/// Load one of the buyer's own addresses by id (used by
/// the writes below and by checkout's address path).
pub async fn load_own_address(
    pool: &PgPool,
    user_id: Uuid,
    address_id: Uuid,
) -> Result<Option<AddressRow>, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {ADDRESS_COLUMNS} FROM buyer_addresses WHERE id = $1 AND user_id = $2"
    ))
    .bind(address_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load buyer address", e))
}

/// The buyer's whole book, in picker order.
pub async fn list_addresses(pool: &PgPool, user: &AuthUser) -> Result<AddressList, ApiError> {
    let rows: Vec<AddressRow> = sqlx::query_as(&format!(
        "SELECT {ADDRESS_COLUMNS} FROM buyer_addresses WHERE user_id = $1 \
         ORDER BY is_default DESC, last_used_at DESC NULLS LAST, created_at DESC"
    ))
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list buyer addresses", e))?;
    let total = rows.len() as i64;
    Ok(AddressList {
        items: rows.iter().map(AddressRow::to_view).collect(),
        total,
    })
}

/// Create one address. `make_default` (or an empty book)
/// makes it the default, switching the flag off the
/// previous default inside the same transaction.
pub async fn create_address(
    pool: &PgPool,
    user: &AuthUser,
    input: &AddressInput,
    make_default: bool,
) -> Result<AddressView, ApiError> {
    let validated = validate_address_input(input).map_err(ApiError::bad_request)?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin address transaction", e))?;
    // Serialize the count check + insert per buyer by
    // locking the buyer's own user row (the same lock
    // checkout's loyalty path takes): two concurrent
    // creates cannot both pass the cap check.
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("lock buyer for address create", e))?;
    let current: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM buyer_addresses WHERE user_id = $1")
            .bind(user.user_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("count buyer addresses", e))?;
    validate_address_count(current).map_err(ApiError::bad_request)?;
    let is_default = make_default || current == 0;
    if is_default {
        sqlx::query(
            "UPDATE buyer_addresses SET is_default = false, updated_at = now() \
             WHERE user_id = $1 AND is_default",
        )
        .bind(user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("clear previous default address", e))?;
    }
    let row: AddressRow = sqlx::query_as(&format!(
        "INSERT INTO buyer_addresses \
         (user_id, label, recipient_name, phone, district, area, address_line, is_default) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {ADDRESS_COLUMNS}"
    ))
    .bind(user.user_id)
    .bind(&validated.label)
    .bind(&validated.recipient_name)
    .bind(&validated.phone)
    .bind(&validated.district)
    .bind(&validated.area)
    .bind(&validated.address_line)
    .bind(is_default)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("insert buyer address", e))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit address create", e))?;
    Ok(row.to_view())
}

/// Update one of the buyer's addresses. The patch merges
/// over the stored row and the MERGED address passes
/// the one domain validation — a patch can never leave
/// a half-valid row behind. `is_default: true` switches
/// the default in the same transaction; `false` simply
/// unsets it (leaving the book without a default).
pub async fn update_address(
    pool: &PgPool,
    user: &AuthUser,
    address_id: Uuid,
    patch: &AddressPatch,
) -> Result<AddressView, ApiError> {
    let existing = load_own_address(pool, user.user_id, address_id)
        .await?
        .ok_or_else(|| ApiError::not_found("address not found"))?;
    let merged = AddressInput {
        label: patch.label.clone().unwrap_or(existing.label.clone()),
        recipient_name: patch
            .recipient_name
            .clone()
            .unwrap_or(existing.recipient_name.clone()),
        phone: patch.phone.clone().unwrap_or(existing.phone.clone()),
        district: patch.district.clone().unwrap_or(existing.district.clone()),
        area: patch.area.clone().unwrap_or(existing.area.clone()),
        address_line: patch
            .address_line
            .clone()
            .unwrap_or(existing.address_line.clone()),
    };
    let validated = validate_address_input(&merged).map_err(ApiError::bad_request)?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin address update", e))?;
    if patch.is_default == Some(true) {
        sqlx::query(
            "UPDATE buyer_addresses SET is_default = false, updated_at = now() \
             WHERE user_id = $1 AND is_default AND id <> $2",
        )
        .bind(user.user_id)
        .bind(address_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("clear previous default address", e))?;
    }
    let row: AddressRow = sqlx::query_as(&format!(
        "UPDATE buyer_addresses SET label = $1, recipient_name = $2, phone = $3, \
         district = $4, area = $5, address_line = $6, \
         is_default = CASE WHEN $7::boolean IS NULL THEN is_default ELSE $7 END, \
         updated_at = now() \
         WHERE id = $8 AND user_id = $9 RETURNING {ADDRESS_COLUMNS}"
    ))
    .bind(&validated.label)
    .bind(&validated.recipient_name)
    .bind(&validated.phone)
    .bind(&validated.district)
    .bind(&validated.area)
    .bind(&validated.address_line)
    .bind(patch.is_default)
    .bind(address_id)
    .bind(user.user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("update buyer address", e))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit address update", e))?;
    Ok(row.to_view())
}

/// Make one of the buyer's addresses the default,
/// switching the previous one off in the same
/// transaction. Idempotent: setting the current default
/// again returns it unchanged.
pub async fn set_default_address(
    pool: &PgPool,
    user: &AuthUser,
    address_id: Uuid,
) -> Result<AddressView, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin set-default transaction", e))?;
    let exists: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM buyer_addresses WHERE id = $1 AND user_id = $2")
            .bind(address_id)
            .bind(user.user_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("load address for set-default", e))?;
    if exists.is_none() {
        return Err(ApiError::not_found("address not found"));
    }
    sqlx::query(
        "UPDATE buyer_addresses SET is_default = (id = $2), updated_at = now() \
         WHERE user_id = $1 AND (is_default OR id = $2)",
    )
    .bind(user.user_id)
    .bind(address_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("switch default address", e))?;
    let row: AddressRow = sqlx::query_as(&format!(
        "SELECT {ADDRESS_COLUMNS} FROM buyer_addresses WHERE id = $1"
    ))
    .bind(address_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("reload default address", e))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit set-default", e))?;
    Ok(row.to_view())
}

/// Delete one of the buyer's addresses (404 for another
/// buyer's). Returns the remaining book so the client
/// re-renders the server's truth. Deleting the default
/// leaves NO default — see the module docs.
pub async fn delete_address(
    pool: &PgPool,
    user: &AuthUser,
    address_id: Uuid,
) -> Result<AddressList, ApiError> {
    let result = sqlx::query("DELETE FROM buyer_addresses WHERE id = $1 AND user_id = $2")
        .bind(address_id)
        .bind(user.user_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("delete buyer address", e))?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("address not found"));
    }
    list_addresses(pool, user).await
}

/// District views for a slice of the table.
fn district_views(districts: &[&District]) -> Vec<DistrictView> {
    districts
        .iter()
        .map(|district| DistrictView {
            name_en: district.name_en.to_string(),
            name_bn: district.name_bn.to_string(),
            zone: district.zone.as_str().to_string(),
        })
        .collect()
}

/// The public district table: the domain's 64 districts
/// verbatim, in table order (alphabetical).
pub fn district_list() -> DistrictList {
    let all: Vec<&District> = DISTRICTS.iter().collect();
    DistrictList {
        total: DISTRICTS.len() as i64,
        items: district_views(&all),
    }
}

/// Query for `GET /v1/districts`: an optional free-text
/// `q` narrows the table through the domain search.
#[derive(Debug, Default, Deserialize)]
pub struct DistrictQuery {
    pub q: Option<String>,
}

/// The district list for a request: the full table, or
/// the search matches when `q` is present and non-blank
/// (`total` then counts the MATCHES, so a picker can say
/// how many districts it is showing).
pub fn district_list_filtered(query: Option<&str>) -> DistrictList {
    match query.map(str::trim) {
        Some(q) if !q.is_empty() => {
            let found = search_districts(q, DISTRICTS.len());
            DistrictList {
                total: found.len() as i64,
                items: district_views(&found),
            }
        }
        _ => district_list(),
    }
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateAddressRequest {
    pub label: String,
    pub recipient_name: String,
    pub phone: String,
    pub district: String,
    pub area: String,
    pub address_line: String,
    /// Make this address the default (the first address
    /// a buyer saves becomes the default regardless).
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Deserialize)]
pub struct AddressPatch {
    pub label: Option<String>,
    pub recipient_name: Option<String>,
    pub phone: Option<String>,
    pub district: Option<String>,
    pub area: Option<String>,
    pub address_line: Option<String>,
    pub is_default: Option<bool>,
}

pub async fn list_addresses_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<AddressList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(list_addresses(pool, &user).await?))
}

/// GET /v1/addresses/:id — one of the buyer's own
/// addresses (Wave 55): the same no-leak rule as every
/// other verb on the book — another buyer's id is a 404.
pub async fn get_address_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AddressView>, ApiError> {
    let pool = state.require_pool()?;
    let row = load_own_address(pool, user.user_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("address not found"))?;
    Ok(Json(row.to_view()))
}

/// The buyer's default address, or `None` when the book
/// has none (an empty book, or the default was deleted —
/// deletion never auto-promotes, see the module docs).
pub async fn default_address(pool: &PgPool, user: &AuthUser) -> Result<Option<AddressView>, ApiError> {
    let row = sqlx::query_as::<_, AddressRow>(&format!(
        "SELECT {ADDRESS_COLUMNS} FROM buyer_addresses \
         WHERE user_id = $1 AND is_default ORDER BY created_at DESC LIMIT 1"
    ))
    .bind(user.user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load default address", e))?;
    Ok(row.map(|row| row.to_view()))
}

/// GET /v1/buyer/default-address — the checkout prefill:
/// the buyer's default address, 404 when there is none
/// (the client then shows the manual form). It lives
/// under /v1/buyer/ because axum 0.7 cannot register a
/// static `default` segment next to `/v1/addresses/:id`.
pub async fn default_address_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<AddressView>, ApiError> {
    let pool = state.require_pool()?;
    default_address(pool, &user)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("no default address"))
}

pub async fn create_address_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateAddressRequest>,
) -> Result<(StatusCode, Json<AddressView>), ApiError> {
    let pool = state.require_pool()?;
    let input = AddressInput {
        label: req.label,
        recipient_name: req.recipient_name,
        phone: req.phone,
        district: req.district,
        area: req.area,
        address_line: req.address_line,
    };
    let view = create_address(pool, &user, &input, req.is_default).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

pub async fn update_address_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(patch): Json<AddressPatch>,
) -> Result<Json<AddressView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(update_address(pool, &user, id, &patch).await?))
}

pub async fn set_default_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AddressView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(set_default_address(pool, &user, id).await?))
}

pub async fn delete_address_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AddressList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(delete_address(pool, &user, id).await?))
}

/// GET /v1/districts — public: the district → zone table
/// is the platform's published delivery policy, the same
/// for every visitor (like the delivery estimate). `?q=`
/// narrows the list through the domain district search.
pub async fn districts_handler(Query(query): Query<DistrictQuery>) -> Json<DistrictList> {
    Json(district_list_filtered(query.q.as_deref()))
}
