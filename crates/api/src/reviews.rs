//! Reviews, ratings and seller replies (Wave 8).
//!
//! The rules, each enforced by exactly one function below:
//! - Eligibility: a buyer may review a product only after receiving
//!   it in a DELIVERED sub-order. `find_delivered_order_for_review`
//!   is the single query behind this; it also yields the order id
//!   stored on the review as proof.
//! - One review per buyer per product (database UNIQUE); a second
//!   create is a 409, editing goes through PATCH on the same row.
//! - Only the author can edit or delete a review; anyone else gets a
//!   404 (the review's existence is not leaked).
//! - The seller who owns the product's store may post ONE reply per
//!   review and edit it; other sellers get a 404.
//! - Public views never expose a buyer's full phone number: the
//!   display name is the buyer's name when set, otherwise their
//!   masked phone (see `domain::mask_phone`).
//!
//! Input rules (rating range, body lengths) are the pure domain
//! validators, shared with nothing else because nothing else needs
//! them yet.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{mask_phone, validate_rating, validate_reply_body, validate_review_body};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::products::{clamp_pagination, get_product_by_id};
use crate::stores::get_store_by_id;
use crate::{ApiError, AppState};

/// A seller's reply to a review; also a `review_replies` row.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ReviewReply {
    pub id: Uuid,
    pub review_id: Uuid,
    pub store_id: Uuid,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The public review shape: the row plus its computed buyer display
/// string and its seller reply when one exists.
#[derive(Debug, Serialize)]
pub struct Review {
    pub id: Uuid,
    pub product_id: Uuid,
    pub buyer_id: Uuid,
    /// The buyer's name when they have one, else their masked phone.
    pub buyer_display: String,
    /// The delivered order that made this review eligible.
    pub order_id: Uuid,
    pub rating: i16,
    pub body: Option<String>,
    /// The review's photo (Wave 51), when the author
    /// attached one: the gallery display URL, derived from
    /// the stored key by the media module's one URL rule.
    /// Null for the (majority) photo-less reviews.
    pub image_url: Option<String>,
    /// The review photo's original pixel dimensions (Wave 51),
    /// so clients can reserve its layout space before it loads;
    /// null when there is no photo.
    pub image_width: Option<i32>,
    pub image_height: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub reply: Option<ReviewReply>,
}

/// One page of reviews plus the total count — the same shape as the
/// product and order lists.
#[derive(Debug, Serialize)]
pub struct ReviewList {
    pub items: Vec<Review>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// Flat row for the review SELECT: review columns, the buyer's
/// identity columns, and the (nullable) reply columns.
#[derive(Debug, FromRow)]
struct ReviewRow {
    id: Uuid,
    product_id: Uuid,
    buyer_id: Uuid,
    buyer_name: Option<String>,
    buyer_phone: String,
    order_id: Uuid,
    rating: i16,
    body: Option<String>,
    /// Wave 51: the review photo's stored base key and the
    /// original's dimensions (migration 0041); all null
    /// when the review has no photo.
    image_storage_key: Option<String>,
    image_width: Option<i32>,
    image_height: Option<i32>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    reply_id: Option<Uuid>,
    reply_store_id: Option<Uuid>,
    reply_body: Option<String>,
    reply_created_at: Option<DateTime<Utc>>,
    reply_updated_at: Option<DateTime<Utc>>,
}

impl ReviewRow {
    fn into_view(self) -> Review {
        let reply = match (
            self.reply_id,
            self.reply_store_id,
            self.reply_body,
            self.reply_created_at,
            self.reply_updated_at,
        ) {
            (Some(id), Some(store_id), Some(body), Some(created_at), Some(updated_at)) => {
                Some(ReviewReply {
                    id,
                    review_id: self.id,
                    store_id,
                    body,
                    created_at,
                    updated_at,
                })
            }
            _ => None,
        };
        Review {
            id: self.id,
            product_id: self.product_id,
            buyer_id: self.buyer_id,
            buyer_display: buyer_display(&self.buyer_name, &self.buyer_phone),
            order_id: self.order_id,
            rating: self.rating,
            body: self.body,
            image_url: self
                .image_storage_key
                .as_deref()
                .map(|key| crate::media::sized_url(key, self.image_width, domain::GALLERY_IMAGE_WIDTH)),
            image_width: self.image_width,
            image_height: self.image_height,
            created_at: self.created_at,
            updated_at: self.updated_at,
            reply,
        }
    }
}

/// Review columns + reply columns; every review query selects this
/// from `REVIEW_FROM` so all views are built identically.
const REVIEW_COLUMNS: &str = "r.id, r.product_id, r.buyer_id, \
    u.name AS buyer_name, u.phone AS buyer_phone, \
    r.order_id, r.rating, r.body, r.image_storage_key, r.image_width, \
    r.image_height, r.created_at, r.updated_at, \
    rp.id AS reply_id, rp.store_id AS reply_store_id, rp.body AS reply_body, \
    rp.created_at AS reply_created_at, rp.updated_at AS reply_updated_at";

const REVIEW_FROM: &str = "FROM reviews r \
    JOIN users u ON u.id = r.buyer_id \
    LEFT JOIN review_replies rp ON rp.review_id = r.id";

/// Display name for a buyer in public views: their name when set,
/// otherwise their masked phone — never the full number.
fn buyer_display(name: &Option<String>, phone: &str) -> String {
    match name {
        Some(name) if !name.trim().is_empty() => name.clone(),
        _ => mask_phone(phone),
    }
}

/// True when a SQLx error is a Postgres unique violation (23505).
fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .map(|db_err| db_err.code().as_deref() == Some("23505"))
        .unwrap_or(false)
}

/// Normalise an optional free-text body: trimmed, empty becomes None.
fn clean_body(body: Option<&str>) -> Option<String> {
    body.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Service functions
// ---------------------------------------------------------------------------

/// THE eligibility query: the id of a delivered order in which this
/// buyer received this product, if any. A sub-order counts only when
/// its status is `delivered` — shipped or returned does not qualify.
/// The newest qualifying order wins.
pub async fn find_delivered_order_for_review(
    pool: &PgPool,
    buyer_id: Uuid,
    product_id: Uuid,
) -> Result<Option<Uuid>, ApiError> {
    sqlx::query_scalar(
        "SELECT o.id FROM orders o \
         JOIN sub_orders so ON so.order_id = o.id \
         JOIN order_items oi ON oi.sub_order_id = so.id \
         WHERE o.buyer_user_id = $1 AND oi.product_id = $2 \
           AND so.status = 'delivered' \
         ORDER BY o.created_at DESC LIMIT 1",
    )
    .bind(buyer_id)
    .bind(product_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("review eligibility", e))
}

/// Boolean form of [`find_delivered_order_for_review`] — derived from
/// the same single query, never a second one.
pub async fn buyer_has_delivered_product(
    pool: &PgPool,
    buyer_id: Uuid,
    product_id: Uuid,
) -> Result<bool, ApiError> {
    Ok(find_delivered_order_for_review(pool, buyer_id, product_id)
        .await?
        .is_some())
}

/// Load one review (with its reply) in public view shape.
pub async fn get_review_by_id(pool: &PgPool, id: Uuid) -> Result<Review, ApiError> {
    let row: ReviewRow = sqlx::query_as(&format!(
        "SELECT {REVIEW_COLUMNS} {REVIEW_FROM} WHERE r.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load review", e))?
    .ok_or_else(|| ApiError::not_found("review not found"))?;
    Ok(row.into_view())
}

/// Create the caller's review of a product. Order of checks: input
/// (400), product exists (404), delivered eligibility (403), one per
/// buyer per product (409).
pub async fn create_review(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    rating: i64,
    body: Option<&str>,
) -> Result<Review, ApiError> {
    if !validate_rating(rating) {
        return Err(ApiError::bad_request("rating must be between 1 and 5"));
    }
    validate_review_body(body.unwrap_or("")).map_err(ApiError::bad_request)?;
    get_product_by_id(pool, product_id).await?;
    let order_id = find_delivered_order_for_review(pool, user.user_id, product_id)
        .await?
        .ok_or_else(|| {
            ApiError::forbidden("You can review only products you received")
        })?;

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO reviews (product_id, buyer_id, order_id, rating, body) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(product_id)
    .bind(user.user_id)
    .bind(order_id)
    .bind(rating as i16)
    .bind(clean_body(body))
    .fetch_one(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict("you have already reviewed this product")
        } else {
            ApiError::internal("create review", e)
        }
    })?;

    get_review_by_id(pool, id).await
}

/// List a product's reviews, newest first, paginated like every other
/// list in the API. A missing product is a 404, not an empty list.
pub async fn list_product_reviews(
    pool: &PgPool,
    product_id: Uuid,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<ReviewList, ApiError> {
    get_product_by_id(pool, product_id).await?;
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reviews WHERE product_id = $1")
        .bind(product_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count reviews", e))?;

    let rows: Vec<ReviewRow> = sqlx::query_as(&format!(
        "SELECT {REVIEW_COLUMNS} {REVIEW_FROM} \
         WHERE r.product_id = $1 ORDER BY r.created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(product_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list reviews", e))?;

    Ok(ReviewList {
        items: rows.into_iter().map(ReviewRow::into_view).collect(),
        total,
        page,
        limit,
    })
}

/// Edit the caller's own review. A review that does not exist — or
/// belongs to someone else — is a 404. Provided fields override the
/// stored ones; the merged result must pass the domain validators.
pub async fn update_own_review(
    pool: &PgPool,
    user: &AuthUser,
    review_id: Uuid,
    patch: UpdateReviewInput,
) -> Result<Review, ApiError> {
    let existing: Option<(Uuid, i16, Option<String>)> = sqlx::query_as(
        "SELECT buyer_id, rating, body FROM reviews WHERE id = $1",
    )
    .bind(review_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load review for update", e))?;
    let (buyer_id, old_rating, old_body) =
        existing.ok_or_else(|| ApiError::not_found("review not found"))?;
    if buyer_id != user.user_id {
        return Err(ApiError::not_found("review not found"));
    }

    let rating = patch.rating.unwrap_or(i64::from(old_rating));
    if !validate_rating(rating) {
        return Err(ApiError::bad_request("rating must be between 1 and 5"));
    }
    let body = match &patch.body {
        Some(raw) => {
            validate_review_body(raw).map_err(ApiError::bad_request)?;
            clean_body(Some(raw))
        }
        None => old_body,
    };

    sqlx::query(
        "UPDATE reviews SET rating = $2, body = $3, updated_at = now() WHERE id = $1",
    )
    .bind(review_id)
    .bind(rating as i16)
    .bind(body)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("update review", e))?;

    get_review_by_id(pool, review_id).await
}

/// Delete the caller's own review (its reply goes with it via the
/// foreign-key cascade). Not the author's review: 404. A Wave 51
/// review photo's stored objects are removed after the row is
/// gone (best-effort — an orphaned object is inert, a row pointing
/// at removed objects would not be).
pub async fn delete_own_review(
    state: &AppState,
    user: &AuthUser,
    review_id: Uuid,
) -> Result<(), ApiError> {
    let pool = state.require_pool()?;
    let photo: Option<(Option<String>, Option<i32>)> = sqlx::query_as(
        "SELECT image_storage_key, image_width FROM reviews WHERE id = $1 AND buyer_id = $2",
    )
    .bind(review_id)
    .bind(user.user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load review for delete", e))?;
    let (photo_key, photo_width) = photo.ok_or_else(|| ApiError::not_found("review not found"))?;
    sqlx::query("DELETE FROM reviews WHERE id = $1 AND buyer_id = $2")
        .bind(review_id)
        .bind(user.user_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("delete review", e))?;
    if let Some(key) = photo_key {
        crate::media::delete_stored_image(state.media_store.as_ref(), &key, photo_width).await;
    }
    Ok(())
}

/// Load a review's photo facts for its AUTHOR (Wave 51): a
/// review that does not exist — or belongs to someone else —
/// is the module's usual 404.
async fn own_review_photo_facts(
    pool: &PgPool,
    user: &AuthUser,
    review_id: Uuid,
) -> Result<(Option<String>, Option<i32>), ApiError> {
    let facts: Option<(Option<String>, Option<i32>)> = sqlx::query_as(
        "SELECT image_storage_key, image_width FROM reviews WHERE id = $1 AND buyer_id = $2",
    )
    .bind(review_id)
    .bind(user.user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load review photo facts", e))?;
    facts.ok_or_else(|| ApiError::not_found("review not found"))
}

/// Attach (or replace) the caller's photo on their own
/// review (Wave 51): ONE photo per review, through the same
/// media pipeline as product images, under the deterministic
/// key `reviews/<review_id>/photo` — a replacement clears the
/// old objects first (a smaller replacement must not leave a
/// stale larger derived size behind), stores the new ones,
/// and only then points the row at them.
pub async fn attach_review_image(
    state: &AppState,
    user: &AuthUser,
    review_id: Uuid,
    bytes: &[u8],
    declared_content_type: Option<&str>,
) -> Result<Review, ApiError> {
    let pool = state.require_pool()?;
    let (old_key, old_width) = own_review_photo_facts(pool, user, review_id).await?;
    let base_key = format!("reviews/{review_id}/photo");
    let processed = crate::media::process_upload(&base_key, bytes, declared_content_type)?;
    if old_key.is_some() {
        crate::media::delete_stored_image(state.media_store.as_ref(), &base_key, old_width)
            .await;
    }
    crate::media::store_processed(state.media_store.as_ref(), &processed).await?;
    sqlx::query(
        "UPDATE reviews SET image_storage_key = $2, image_width = $3, \
         image_height = $4, updated_at = now() WHERE id = $1",
    )
    .bind(review_id)
    .bind(&base_key)
    .bind(processed.width as i32)
    .bind(processed.height as i32)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("attach review image", e))?;
    get_review_by_id(pool, review_id).await
}

/// Remove the caller's photo from their own review (Wave
/// 51): the columns clear first, then the objects are
/// removed best-effort. A review with no photo is a
/// successful no-op — the end state is the same.
pub async fn delete_review_image(
    state: &AppState,
    user: &AuthUser,
    review_id: Uuid,
) -> Result<Review, ApiError> {
    let pool = state.require_pool()?;
    let (old_key, old_width) = own_review_photo_facts(pool, user, review_id).await?;
    sqlx::query(
        "UPDATE reviews SET image_storage_key = NULL, image_width = NULL, \
         image_height = NULL, updated_at = now() WHERE id = $1",
    )
    .bind(review_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("delete review image", e))?;
    if let Some(key) = old_key {
        crate::media::delete_stored_image(state.media_store.as_ref(), &key, old_width).await;
    }
    get_review_by_id(pool, review_id).await
}

/// Resolve a review to the store that sells its product, requiring
/// the caller to own that store. Every failure is a 404: a review
/// that is not the caller's to answer is indistinguishable from one
/// that does not exist.
async fn require_review_store(pool: &PgPool, user: &AuthUser, review_id: Uuid) -> Result<Uuid, ApiError> {
    let store_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT p.store_id FROM reviews r \
         JOIN products p ON p.id = r.product_id WHERE r.id = $1",
    )
    .bind(review_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load review store", e))?;
    let store_id = store_id.ok_or_else(|| ApiError::not_found("review not found"))?;
    let store = get_store_by_id(pool, store_id).await?;
    if store.owner_user_id != user.user_id {
        return Err(ApiError::not_found("review not found"));
    }
    Ok(store_id)
}

/// Post the store owner's reply to a review. One reply per review —
/// a second create is a 409; use the PATCH path to change it.
pub async fn create_reply(
    pool: &PgPool,
    user: &AuthUser,
    review_id: Uuid,
    body: &str,
) -> Result<ReviewReply, ApiError> {
    validate_reply_body(body).map_err(ApiError::bad_request)?;
    let store_id = require_review_store(pool, user, review_id).await?;

    sqlx::query_as(
        "INSERT INTO review_replies (review_id, store_id, body) \
         VALUES ($1, $2, $3) \
         RETURNING id, review_id, store_id, body, created_at, updated_at",
    )
    .bind(review_id)
    .bind(store_id)
    .bind(body.trim())
    .fetch_one(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict("this review already has a reply")
        } else {
            ApiError::internal("create reply", e)
        }
    })
}

/// Edit the reply on a review of the caller's own store's product.
/// No reply yet — or not the caller's store — is a 404.
pub async fn update_reply(
    pool: &PgPool,
    user: &AuthUser,
    review_id: Uuid,
    body: &str,
) -> Result<ReviewReply, ApiError> {
    validate_reply_body(body).map_err(ApiError::bad_request)?;
    require_review_store(pool, user, review_id).await?;

    sqlx::query_as(
        "UPDATE review_replies SET body = $2, updated_at = now() WHERE review_id = $1 \
         RETURNING id, review_id, store_id, body, created_at, updated_at",
    )
    .bind(review_id)
    .bind(body.trim())
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("update reply", e))?
    .ok_or_else(|| ApiError::not_found("reply not found"))
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateReviewInput {
    pub rating: i64,
    pub body: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateReviewInput {
    pub rating: Option<i64>,
    pub body: Option<String>,
}

#[derive(Deserialize)]
pub struct ReplyInput {
    pub body: String,
}

#[derive(Deserialize)]
pub struct ListReviewsQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn create_review_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(input): Json<CreateReviewInput>,
) -> Result<(StatusCode, Json<Review>), ApiError> {
    let pool = state.require_pool()?;
    let review = create_review(pool, &user, product_id, input.rating, input.body.as_deref()).await?;
    Ok((StatusCode::CREATED, Json(review)))
}

pub async fn list_reviews_handler(
    State(state): State<AppState>,
    Path(product_id): Path<Uuid>,
    Query(query): Query<ListReviewsQuery>,
) -> Result<Json<ReviewList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_product_reviews(pool, product_id, query.page, query.limit).await?,
    ))
}

pub async fn update_review_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(patch): Json<UpdateReviewInput>,
) -> Result<Json<Review>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(update_own_review(pool, &user, id, patch).await?))
}

pub async fn delete_review_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    delete_own_review(&state, &user, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /v1/reviews/:id/image (Wave 51) — attach or replace
/// the author's photo on their own review (multipart, the
/// shared upload form of the product image endpoints).
pub async fn attach_review_image_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    multipart: axum::extract::Multipart,
) -> Result<Json<Review>, ApiError> {
    let form = crate::product_images::read_upload_form(multipart).await?;
    Ok(Json(
        attach_review_image(&state, &user, id, &form.bytes, form.content_type.as_deref())
            .await?,
    ))
}

/// DELETE /v1/reviews/:id/image (Wave 51) — remove the
/// author's photo from their own review.
pub async fn delete_review_image_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Review>, ApiError> {
    Ok(Json(delete_review_image(&state, &user, id).await?))
}

pub async fn create_reply_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(review_id): Path<Uuid>,
    Json(input): Json<ReplyInput>,
) -> Result<(StatusCode, Json<ReviewReply>), ApiError> {
    let pool = state.require_pool()?;
    let reply = create_reply(pool, &user, review_id, &input.body).await?;
    Ok((StatusCode::CREATED, Json(reply)))
}

pub async fn update_reply_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(review_id): Path<Uuid>,
    Json(input): Json<ReplyInput>,
) -> Result<Json<ReviewReply>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(update_reply(pool, &user, review_id, &input.body).await?))
}
