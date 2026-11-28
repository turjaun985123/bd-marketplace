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
    /// How many signed-in users marked this review
    /// helpful (Wave 54 extension) — the live count of
    /// `review_helpful_votes`, materialised by the shared
    /// review SELECT so every surface agrees.
    pub helpful_count: i64,
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
    /// Wave 54 extension: the helpful-vote count subquery.
    helpful_count: i64,
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
            helpful_count: self.helpful_count,
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
    rp.created_at AS reply_created_at, rp.updated_at AS reply_updated_at, \
    (SELECT COUNT(*) FROM review_helpful_votes hv WHERE hv.review_id = r.id) \
        AS helpful_count";

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

/// The ORDER BY fragment for one validated review sort
/// (Wave 54). `Photos` leads with the photographed reviews
/// (the Wave 51 photo is the most informative thing on the
/// page) and falls back to newest inside each group — a
/// sort, never a filter.
fn review_order_by(sort: domain::ReviewSort) -> &'static str {
    match sort {
        domain::ReviewSort::Newest => "ORDER BY r.created_at DESC",
        domain::ReviewSort::Highest => "ORDER BY r.rating DESC, r.created_at DESC",
        domain::ReviewSort::Lowest => "ORDER BY r.rating ASC, r.created_at DESC",
        domain::ReviewSort::Photos => {
            "ORDER BY (r.image_storage_key IS NOT NULL) DESC, r.created_at DESC"
        }
        domain::ReviewSort::Helpful => {
            "ORDER BY (SELECT COUNT(*) FROM review_helpful_votes hv \
             WHERE hv.review_id = r.id) DESC, r.created_at DESC"
        }
    }
}

/// List a product's reviews, paginated like every other
/// list in the API. The default order is newest first
/// (the pre-Wave-54 behaviour); Wave 54 adds the `sort`
/// parameter (`newest` / `highest` / `lowest` / `photos`,
/// validated by the domain rule — an unknown value is a
/// 400, never a silent default). A missing product is a
/// 404, not an empty list.
pub async fn list_product_reviews(
    pool: &PgPool,
    product_id: Uuid,
    page: Option<i64>,
    limit: Option<i64>,
    sort: Option<&str>,
) -> Result<ReviewList, ApiError> {
    let sort = domain::validate_review_sort(sort).map_err(ApiError::bad_request)?;
    let product = get_product_by_id(pool, product_id).await?;
    // Admin products remake: a taken-down product's
    // reviews leave the public surface with the product —
    // the same uniform 404 as a missing product. (Draft
    // and archived products keep today's behaviour: this
    // seals the platform-removal case only.)
    if product.admin_hidden_at.is_some() {
        return Err(ApiError::not_found("product not found"));
    }
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reviews WHERE product_id = $1")
        .bind(product_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count reviews", e))?;

    let rows: Vec<ReviewRow> = sqlx::query_as(&format!(
        "SELECT {REVIEW_COLUMNS} {REVIEW_FROM} \
         WHERE r.product_id = $1 {} LIMIT $2 OFFSET $3",
        review_order_by(sort)
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

/// One star level of a rating distribution (Wave 54): how
/// many reviews gave it, and its floored whole-percent
/// share of the total (the domain's `rating_share_percent`
/// — the bars can sum to under 100, never over).
#[derive(Debug, Serialize)]
pub struct RatingLevel {
    pub stars: i16,
    pub count: i64,
    pub percent: i64,
}

/// A rating summary with its distribution (Wave 54): the
/// average (the platform's one ROUND-to-one-decimal rule),
/// the total, how many reviews carry a photo, and the
/// five distribution levels, 5 stars first. Every number
/// is a live aggregate over real reviews — an unreviewed
/// subject reports count 0, a null average and five zero
/// levels, never an invented rating.
#[derive(Debug, Serialize)]
pub struct RatingSummary {
    pub rating_avg: Option<f64>,
    pub rating_count: i64,
    pub with_photos_count: i64,
    pub distribution: Vec<RatingLevel>,
}

/// The aggregate row behind [`RatingSummary`]: one query
/// with FILTER counts per star level.
#[derive(Debug, FromRow)]
struct RatingSummaryRow {
    rating_avg: Option<f64>,
    rating_count: i64,
    with_photos_count: i64,
    five: i64,
    four: i64,
    three: i64,
    two: i64,
    one: i64,
}

impl RatingSummaryRow {
    fn into_summary(self) -> RatingSummary {
        let total = self.rating_count;
        let level = |stars: i16, count: i64| RatingLevel {
            stars,
            count,
            percent: domain::rating_share_percent(count, total),
        };
        RatingSummary {
            rating_avg: self.rating_avg,
            rating_count: total,
            with_photos_count: self.with_photos_count,
            distribution: vec![
                level(5, self.five),
                level(4, self.four),
                level(3, self.three),
                level(2, self.two),
                level(1, self.one),
            ],
        }
    }
}

/// The shared summary SELECT list (Wave 54): average,
/// total, photo count and the per-star FILTER counts —
/// used verbatim by the product and the store summaries
/// so the two can never compute a distribution differently.
const RATING_SUMMARY_COLUMNS: &str = "CAST(ROUND(AVG(r.rating)::numeric, 1) AS float8) AS rating_avg, \
    COUNT(*)::bigint AS rating_count, \
    COUNT(*) FILTER (WHERE r.image_storage_key IS NOT NULL)::bigint AS with_photos_count, \
    COUNT(*) FILTER (WHERE r.rating = 5)::bigint AS five, \
    COUNT(*) FILTER (WHERE r.rating = 4)::bigint AS four, \
    COUNT(*) FILTER (WHERE r.rating = 3)::bigint AS three, \
    COUNT(*) FILTER (WHERE r.rating = 2)::bigint AS two, \
    COUNT(*) FILTER (WHERE r.rating = 1)::bigint AS one";

/// One product's rating summary (Wave 54). A missing
/// product is a 404, matching the review list.
pub async fn get_product_rating_summary(
    pool: &PgPool,
    product_id: Uuid,
) -> Result<RatingSummary, ApiError> {
    let product = get_product_by_id(pool, product_id).await?;
    // Admin products remake: same takedown seal as the
    // review list above.
    if product.admin_hidden_at.is_some() {
        return Err(ApiError::not_found("product not found"));
    }
    let row: RatingSummaryRow = sqlx::query_as(&format!(
        "SELECT {RATING_SUMMARY_COLUMNS} FROM reviews r WHERE r.product_id = $1"
    ))
    .bind(product_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("load product rating summary", e))?;
    Ok(row.into_summary())
}

/// One store's rating summary (Wave 54): the same
/// distribution over the reviews of ALL the store's
/// products — the storefront's trust block and the store
/// reviews page read it. A missing store is a 404.
pub async fn get_store_rating_summary(
    pool: &PgPool,
    slug: &str,
) -> Result<RatingSummary, ApiError> {
    let store = crate::stores::get_store_by_slug(pool, slug).await?;
    let row: RatingSummaryRow = sqlx::query_as(&format!(
        "SELECT {RATING_SUMMARY_COLUMNS} FROM reviews r \
         JOIN products p ON p.id = r.product_id \
         WHERE p.store_id = $1 AND p.admin_hidden_at IS NULL"
    ))
    .bind(store.id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("load store rating summary", e))?;
    Ok(row.into_summary())
}

/// One review on a store's all-reviews page (Wave 54):
/// the shared review view plus the title of the product
/// it reviews (the page spans the whole catalogue, so
/// each review names its product).
#[derive(Debug, Serialize)]
pub struct StoreReview {
    #[serde(flatten)]
    pub review: Review,
    pub product_title: String,
}

/// Flat row for the store review SELECT: the review
/// columns plus the product's title.
#[derive(Debug, FromRow)]
struct StoreReviewRow {
    id: Uuid,
    product_id: Uuid,
    buyer_id: Uuid,
    buyer_name: Option<String>,
    buyer_phone: String,
    order_id: Uuid,
    rating: i16,
    body: Option<String>,
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
    helpful_count: i64,
    product_title: String,
}

impl StoreReviewRow {
    fn into_view(self) -> StoreReview {
        let product_title = self.product_title.clone();
        let review = ReviewRow {
            id: self.id,
            product_id: self.product_id,
            buyer_id: self.buyer_id,
            buyer_name: self.buyer_name,
            buyer_phone: self.buyer_phone,
            order_id: self.order_id,
            rating: self.rating,
            body: self.body,
            image_storage_key: self.image_storage_key,
            image_width: self.image_width,
            image_height: self.image_height,
            created_at: self.created_at,
            updated_at: self.updated_at,
            reply_id: self.reply_id,
            reply_store_id: self.reply_store_id,
            reply_body: self.reply_body,
            reply_created_at: self.reply_created_at,
            reply_updated_at: self.reply_updated_at,
            helpful_count: self.helpful_count,
        }
        .into_view();
        StoreReview {
            review,
            product_title,
        }
    }
}

/// A page of a store's reviews plus the total — the same
/// envelope as the product review list.
#[derive(Debug, Serialize)]
pub struct StoreReviewList {
    pub items: Vec<StoreReview>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// Every review of every product of one store (Wave 54),
/// with the same sorts as the product list. A missing
/// store is a 404. Reviews of DRAFT/archived products are
/// included — they are real reviews buyers wrote; hiding
/// them would rewrite the store's history when a product
/// is retired. Reviews of products the PLATFORM has
/// taken down (admin products remake) are the exception:
/// a removal for cause takes the product's reviews and
/// title off the public store page with it.
pub async fn list_store_reviews(
    pool: &PgPool,
    slug: &str,
    page: Option<i64>,
    limit: Option<i64>,
    sort: Option<&str>,
) -> Result<StoreReviewList, ApiError> {
    let sort = domain::validate_review_sort(sort).map_err(ApiError::bad_request)?;
    let store = crate::stores::get_store_by_slug(pool, slug).await?;
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reviews r \
         JOIN products p ON p.id = r.product_id \
         WHERE p.store_id = $1 AND p.admin_hidden_at IS NULL",
    )
    .bind(store.id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count store reviews", e))?;

    let rows: Vec<StoreReviewRow> = sqlx::query_as(&format!(
        "SELECT {REVIEW_COLUMNS}, p.title AS product_title {REVIEW_FROM} \
         JOIN products p ON p.id = r.product_id \
         WHERE p.store_id = $1 AND p.admin_hidden_at IS NULL {} LIMIT $2 OFFSET $3",
        review_order_by(sort)
    ))
    .bind(store.id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list store reviews", e))?;

    Ok(StoreReviewList {
        items: rows.into_iter().map(StoreReviewRow::into_view).collect(),
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
    /// Wave 54: `newest` (default) / `highest` / `lowest`
    /// / `photos` — validated by the domain rule.
    pub sort: Option<String>,
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
        list_product_reviews(pool, product_id, query.page, query.limit, query.sort.as_deref())
            .await?,
    ))
}

/// GET /v1/products/:id/rating-summary (Wave 54) — the
/// distribution behind the product page's rating bars.
pub async fn product_rating_summary_handler(
    State(state): State<AppState>,
    Path(product_id): Path<Uuid>,
) -> Result<Json<RatingSummary>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_product_rating_summary(pool, product_id).await?))
}

/// GET /v1/stores/:slug/rating-summary (Wave 54) — the
/// store-wide distribution behind the storefront's trust
/// block and its reviews page.
pub async fn store_rating_summary_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<RatingSummary>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_store_rating_summary(pool, &slug).await?))
}

/// GET /v1/stores/:slug/reviews (Wave 54) — every review
/// of the store's products, paginated and sortable like
/// the product list.
pub async fn list_store_reviews_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<ListReviewsQuery>,
) -> Result<Json<StoreReviewList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_store_reviews(pool, &slug, query.page, query.limit, query.sort.as_deref()).await?,
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

// ---------------------------------------------------------------------------
// Helpful votes (Wave 54 extension)
// ---------------------------------------------------------------------------

/// One review's helpful state for one viewer: whether
/// THEY voted, and the total count. Returned by the vote
/// writes and the state read, so the client renders the
/// server's truth after every toggle.
#[derive(Debug, Serialize)]
pub struct HelpfulState {
    pub review_id: Uuid,
    pub helpful: bool,
    pub helpful_count: i64,
}

/// The helpful state of one review for one viewer (404
/// when the review does not exist).
pub async fn review_helpful_state(
    pool: &PgPool,
    user: &AuthUser,
    review_id: Uuid,
) -> Result<HelpfulState, ApiError> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM reviews WHERE id = $1)")
            .bind(review_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("check review exists", e))?;
    if !exists {
        return Err(ApiError::not_found("review not found"));
    }
    let helpful: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM review_helpful_votes \
         WHERE review_id = $1 AND user_id = $2)",
    )
    .bind(review_id)
    .bind(user.user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("check helpful vote", e))?;
    let helpful_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM review_helpful_votes WHERE review_id = $1",
    )
    .bind(review_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count helpful votes", e))?;
    Ok(HelpfulState {
        review_id,
        helpful,
        helpful_count,
    })
}

/// Cast or retract the caller's helpful vote on a review.
/// The author rule is the domain's
/// (`can_vote_review_helpful`): voting on your own review
/// is a bilingual 400. Both directions are idempotent —
/// the (review, user) primary key plus ON CONFLICT DO
/// NOTHING make a repeated vote a no-op, and retracting a
/// vote that does not exist already holds the requested
/// end state.
pub async fn set_review_helpful(
    pool: &PgPool,
    user: &AuthUser,
    review_id: Uuid,
    helpful: bool,
) -> Result<HelpfulState, ApiError> {
    let author: Option<Uuid> =
        sqlx::query_scalar("SELECT buyer_id FROM reviews WHERE id = $1")
            .bind(review_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("load review author", e))?;
    let author = author.ok_or_else(|| ApiError::not_found("review not found"))?;
    if !domain::can_vote_review_helpful(author == user.user_id) {
        return Err(ApiError::bad_request(domain::OWN_REVIEW_VOTE_ERROR));
    }
    if helpful {
        sqlx::query(
            "INSERT INTO review_helpful_votes (review_id, user_id) VALUES ($1, $2) \
             ON CONFLICT (review_id, user_id) DO NOTHING",
        )
        .bind(review_id)
        .bind(user.user_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("cast helpful vote", e))?;
    } else {
        sqlx::query(
            "DELETE FROM review_helpful_votes WHERE review_id = $1 AND user_id = $2",
        )
        .bind(review_id)
        .bind(user.user_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("retract helpful vote", e))?;
    }
    review_helpful_state(pool, user, review_id).await
}

pub async fn review_helpful_state_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(review_id): Path<Uuid>,
) -> Result<Json<HelpfulState>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(review_helpful_state(pool, &user, review_id).await?))
}

pub async fn vote_helpful_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(review_id): Path<Uuid>,
) -> Result<Json<HelpfulState>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(set_review_helpful(pool, &user, review_id, true).await?))
}

pub async fn unvote_helpful_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(review_id): Path<Uuid>,
) -> Result<Json<HelpfulState>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(set_review_helpful(pool, &user, review_id, false).await?))
}
