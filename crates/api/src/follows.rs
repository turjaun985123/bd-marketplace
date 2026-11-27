//! Store follows (Wave 54): a signed-in user follows a
//! store to collect it on their `/following` page and to
//! receive the Wave 29 outbox notice when the store
//! publishes a new product (the fan-out itself lives in
//! `notices::enqueue_store_new_product_notices`; this
//! module owns the follow relationship).
//!
//! The rules, each enforced in exactly one place below:
//! - Following is idempotent: the (user, store) primary
//!   key plus `ON CONFLICT DO NOTHING` make a second
//!   follow a successful no-op returning the same state.
//! - Unfollowing is idempotent the same way: removing a
//!   follow that does not exist is a success — the end
//!   state the caller asked for already holds.
//! - A store's OWNER may not follow their own store
//!   (`domain::can_follow_store`): the public follower
//!   count must never include the seller themself.
//! - One user follows at most `domain::MAX_FOLLOWED_STORES`
//!   stores (`domain::validate_follow_count`); re-following
//!   an already-followed store never trips the cap.
//! - A suspended store can still be followed/unfollowed
//!   (the relationship is the user's data), but it does
//!   not appear on the /following page while suspended —
//!   the same visibility rule as its products.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::products::{Product, PRODUCT_COLUMNS, PRODUCT_RATING_JOIN};
use crate::stores::{get_store_by_slug, Store};
use crate::{ApiError, AppState};

/// How many of a followed store's newest products the
/// /following page embeds per store.
const NEWEST_PRODUCTS_PER_STORE: i64 = 4;

/// The follow state of one store for one viewer: whether
/// THEY follow it, the store's total follower count, and
/// (Wave 54 extension) whether the follow's new-product
/// notices are on. Returned by the follow/unfollow/mute
/// writes so the client renders the server's truth, not
/// an optimistic guess.
#[derive(Debug, Serialize)]
pub struct FollowState {
    pub following: bool,
    pub follower_count: i64,
    pub notify_new_products: bool,
}

/// The store's total follower count (0 for a store nobody
/// follows — the count is a live aggregate, never a stored
/// counter that could drift).
pub async fn follower_count(pool: &PgPool, store_id: Uuid) -> Result<i64, ApiError> {
    sqlx::query_scalar("SELECT COUNT(*) FROM store_follows WHERE store_id = $1")
        .bind(store_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count store followers", e))
}

/// Does this user follow this store?
pub async fn is_following(
    pool: &PgPool,
    store_id: Uuid,
    user_id: Uuid,
) -> Result<bool, ApiError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM store_follows WHERE store_id = $1 AND user_id = $2)",
    )
    .bind(store_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("check store follow", e))?;
    Ok(exists)
}

/// The [`FollowState`] of one store for one viewer, from
/// the live reads above. A non-follower's notify flag
/// reads as the default (true — a fresh follow subscribes
/// to notices).
pub async fn follow_state(
    pool: &PgPool,
    store_id: Uuid,
    user_id: Uuid,
) -> Result<FollowState, ApiError> {
    let notify: Option<bool> = sqlx::query_scalar(
        "SELECT notify_new_products FROM store_follows \
         WHERE store_id = $1 AND user_id = $2",
    )
    .bind(store_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("read follow notify flag", e))?;
    Ok(FollowState {
        // The flag row exists exactly when the follow does.
        following: notify.is_some(),
        follower_count: follower_count(pool, store_id).await?,
        notify_new_products: notify.unwrap_or(true),
    })
}

/// Set the caller's new-product-notice flag on a store
/// they follow (Wave 54 extension): muting keeps the
/// follow — the shelf and the feed — and silences only
/// the fan-out. A store the caller does not follow is a
/// 404 (there is no follow to mute), as is a missing
/// store.
pub async fn set_follow_notify(
    pool: &PgPool,
    user: &AuthUser,
    slug: &str,
    notify: bool,
) -> Result<FollowState, ApiError> {
    let store = get_store_by_slug(pool, slug).await?;
    let result = sqlx::query(
        "UPDATE store_follows SET notify_new_products = $1 \
         WHERE store_id = $2 AND user_id = $3",
    )
    .bind(notify)
    .bind(store.id)
    .bind(user.user_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("set follow notify flag", e))?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found(
            "আপনি এই স্টোরটি ফলো করেন না — you do not follow this store",
        ));
    }
    follow_state(pool, store.id, user.user_id).await
}

/// Follow a store by slug. Check order: the store exists
/// (404), the caller is not its owner (400 — the domain
/// rule), then the cap — but only for a NEW follow: an
/// already-followed store short-circuits to its current
/// state before the cap is consulted, so a user at the
/// cap can still re-affirm an existing follow.
pub async fn follow_store(
    pool: &PgPool,
    user: &AuthUser,
    slug: &str,
) -> Result<FollowState, ApiError> {
    let store = get_store_by_slug(pool, slug).await?;
    if !domain::can_follow_store(store.owner_user_id == user.user_id) {
        return Err(ApiError::bad_request(domain::SELF_FOLLOW_ERROR));
    }
    if is_following(pool, store.id, user.user_id).await? {
        return follow_state(pool, store.id, user.user_id).await;
    }
    let current: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM store_follows WHERE user_id = $1")
            .bind(user.user_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("count user follows", e))?;
    domain::validate_follow_count(current).map_err(ApiError::bad_request)?;
    sqlx::query(
        "INSERT INTO store_follows (user_id, store_id) VALUES ($1, $2) \
         ON CONFLICT (user_id, store_id) DO NOTHING",
    )
    .bind(user.user_id)
    .bind(store.id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("follow store", e))?;
    follow_state(pool, store.id, user.user_id).await
}

/// Unfollow a store by slug: the row is deleted when it
/// exists; when it does not, the requested end state
/// already holds and the current state is returned as a
/// success. A missing store is still a 404 — unfollowing
/// a store that does not exist is a client error, not a
/// no-op.
pub async fn unfollow_store(
    pool: &PgPool,
    user: &AuthUser,
    slug: &str,
) -> Result<FollowState, ApiError> {
    let store = get_store_by_slug(pool, slug).await?;
    sqlx::query("DELETE FROM store_follows WHERE user_id = $1 AND store_id = $2")
        .bind(user.user_id)
        .bind(store.id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("unfollow store", e))?;
    follow_state(pool, store.id, user.user_id).await
}

/// One store on the caller's /following page: the store
/// itself, the caller's follow date, its follower and
/// active-product counts, and its newest active products
/// (the shared product view, flash prices attached) so
/// the page shows what is new since the follow without a
/// second round-trip per store.
#[derive(Debug, Serialize)]
pub struct FollowedStore {
    #[serde(flatten)]
    pub store: Store,
    pub followed_at: DateTime<Utc>,
    pub follower_count: i64,
    pub product_count: i64,
    /// Whether this follow's new-product notices are on
    /// (Wave 54 extension) — the /following page's mute
    /// toggle renders from this.
    pub notify_new_products: bool,
    pub newest_products: Vec<Product>,
}

/// Flat row for the followed-stores read: the store
/// columns plus the follow's own timestamp and the two
/// counts.
#[derive(Debug, FromRow)]
struct FollowedStoreRow {
    id: Uuid,
    followed_at: DateTime<Utc>,
    follower_count: i64,
    product_count: i64,
    notify_new_products: bool,
}

/// The caller's followed stores, most recently followed
/// first. Suspended stores are excluded (their products
/// are hidden platform-wide; the follow row itself is
/// kept, so the store reappears here if reactivated).
/// Newest products are picked per store by ONE windowed
/// query over all followed stores — never a query per
/// store — and capped at [`NEWEST_PRODUCTS_PER_STORE`].
pub async fn list_follows(pool: &PgPool, user: &AuthUser) -> Result<Vec<FollowedStore>, ApiError> {
    let rows: Vec<FollowedStoreRow> = sqlx::query_as(
        "SELECT s.id, f.created_at AS followed_at, \
         f.notify_new_products, \
         (SELECT COUNT(*) FROM store_follows fc WHERE fc.store_id = s.id) \
            AS follower_count, \
         (SELECT COUNT(*) FROM products p \
          WHERE p.store_id = s.id AND p.status = 'active') AS product_count \
         FROM store_follows f JOIN stores s ON s.id = f.store_id \
         WHERE f.user_id = $1 AND s.status = 'active' \
         ORDER BY f.created_at DESC, s.id ASC",
    )
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list followed stores", e))?;

    if rows.is_empty() {
        return Ok(Vec::new());
    }

    // The stores themselves, loaded through the shared
    // store read so the embedded shape is the store view.
    let mut stores: Vec<Store> = Vec::with_capacity(rows.len());
    for row in &rows {
        stores.push(crate::stores::get_store_by_id(pool, row.id).await?);
    }

    // Newest active products across all followed stores:
    // a ROW_NUMBER window per store keeps the read at one
    // query no matter how many stores are followed.
    let store_ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let products: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products p \
         JOIN stores s ON s.id = p.store_id {PRODUCT_RATING_JOIN} \
         JOIN (SELECT id, ROW_NUMBER() OVER (\
             PARTITION BY store_id ORDER BY created_at DESC, id DESC) AS rn \
             FROM products WHERE status = 'active' AND store_id = ANY($1)) picked \
           ON picked.id = p.id AND picked.rn <= $2 \
         ORDER BY p.store_id ASC, p.created_at DESC"
    ))
    .bind(&store_ids)
    .bind(NEWEST_PRODUCTS_PER_STORE)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list followed stores' newest products", e))?;
    let mut products: Vec<Product> = products
        .into_iter()
        .map(crate::products::with_stock_status)
        .collect();
    crate::merch::attach_flash_prices(pool, &mut products, Utc::now()).await?;

    Ok(rows
        .into_iter()
        .zip(stores)
        .map(|(row, store)| FollowedStore {
            newest_products: products
                .iter()
                .filter(|product| product.store_id == row.id)
                .cloned()
                .collect(),
            store,
            followed_at: row.followed_at,
            follower_count: row.follower_count,
            product_count: row.product_count,
            notify_new_products: row.notify_new_products,
        })
        .collect())
}

/// One follower of the caller's own store, for the seller
/// followers view: who they are (name when set, else the
/// masked phone — the full number is the follower's own
/// data, exactly the review-display rule) and since when.
#[derive(Debug, Serialize, FromRow)]
pub struct StoreFollower {
    pub user_id: Uuid,
    pub display: String,
    pub followed_at: DateTime<Utc>,
    /// Whether this follower still receives new-product
    /// and flash-sale notices (Wave 54 extension) — the
    /// seller and the admin both see who muted.
    pub notify_new_products: bool,
}

/// Flat follower row before the display name is derived.
#[derive(Debug, FromRow)]
struct StoreFollowerRow {
    user_id: Uuid,
    name: Option<String>,
    phone: String,
    followed_at: DateTime<Utc>,
    notify_new_products: bool,
}

/// A page of one store's followers plus the total.
#[derive(Debug, Serialize)]
pub struct StoreFollowerList {
    pub items: Vec<StoreFollower>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// The followers of ONE store, newest first, paginated
/// like every other list. Callers gate ownership before
/// calling (the seller handler resolves the caller's own
/// store; a store's follower list is the seller's data,
/// never public).
pub async fn list_store_followers(
    pool: &PgPool,
    store_id: Uuid,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<StoreFollowerList, ApiError> {
    let (page, limit) = crate::products::clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM store_follows WHERE store_id = $1")
            .bind(store_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("count store followers", e))?;
    let rows: Vec<StoreFollowerRow> = sqlx::query_as(
        "SELECT f.user_id, u.name, u.phone, f.created_at AS followed_at, \
                f.notify_new_products \
         FROM store_follows f JOIN users u ON u.id = f.user_id \
         WHERE f.store_id = $1 ORDER BY f.created_at DESC, f.user_id ASC \
         LIMIT $2 OFFSET $3",
    )
    .bind(store_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list store followers", e))?;
    Ok(StoreFollowerList {
        items: rows
            .into_iter()
            .map(|row| StoreFollower {
                display: match row.name {
                    Some(name) if !name.trim().is_empty() => name,
                    _ => domain::mask_phone(&row.phone),
                },
                user_id: row.user_id,
                followed_at: row.followed_at,
                notify_new_products: row.notify_new_products,
            })
            .collect(),
        total,
        page,
        limit,
    })
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn follow_store_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(slug): Path<String>,
) -> Result<Json<FollowState>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(follow_store(pool, &user, &slug).await?))
}

pub async fn unfollow_store_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(slug): Path<String>,
) -> Result<Json<FollowState>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(unfollow_store(pool, &user, &slug).await?))
}

/// The buyer's follows list in the shared list envelope
/// (`items` + `total`; the list is unpaginated — the
/// 200-store cap bounds it — so there is no page/limit).
#[derive(Debug, Serialize)]
pub struct FollowList {
    pub items: Vec<FollowedStore>,
    pub total: i64,
}

/// The mute-toggle payload (Wave 54 extension).
#[derive(serde::Deserialize)]
pub struct SetFollowNotifyInput {
    pub notify_new_products: bool,
}

pub async fn set_follow_notify_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(slug): Path<String>,
    Json(input): Json<SetFollowNotifyInput>,
) -> Result<Json<FollowState>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        set_follow_notify(pool, &user, &slug, input.notify_new_products).await?,
    ))
}

pub async fn list_follows_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<FollowList>, ApiError> {
    let pool = state.require_pool()?;
    let items = list_follows(pool, &user).await?;
    let total = items.len() as i64;
    Ok(Json(FollowList { items, total }))
}

#[derive(serde::Deserialize)]
pub struct ListFollowersQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

/// GET /v1/seller/followers — the followers of the
/// caller's OWN store (their earliest store, the payout
/// store convention). A caller without a store gets the
/// store read's 404.
pub async fn list_seller_followers_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListFollowersQuery>,
) -> Result<Json<StoreFollowerList>, ApiError> {
    let pool = state.require_pool()?;
    let store = crate::stores::get_store_by_owner(pool, user.user_id).await?;
    Ok(Json(
        list_store_followers(pool, store.id, query.page, query.limit).await?,
    ))
}

/// GET /v1/admin/stores/:id/followers — the moderation
/// view of any store's follower list (Wave 54 extension):
/// the same masked list the seller sees, behind the one
/// admin guard. A missing store 404s through the store
/// read. Display names and masked phones only — the same
/// privacy shape as the seller's own view.
pub async fn list_admin_store_followers_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(store_id): Path<Uuid>,
    Query(query): Query<ListFollowersQuery>,
) -> Result<Json<StoreFollowerList>, ApiError> {
    crate::admin::require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let _store = crate::stores::get_store_by_id(pool, store_id).await?;
    Ok(Json(
        list_store_followers(pool, store_id, query.page, query.limit).await?,
    ))
}

// ---------------------------------------------------------------------------
// The follows feed + store suggestions + seller stats
// (Wave 54 extension)
// ---------------------------------------------------------------------------

/// How many products the follows feed returns at most:
/// one rail's worth, newest first — the feed is a
/// discovery strip, not a catalogue (the catalogue has
/// the browse machinery).
pub const FOLLOW_FEED_LIMIT: i64 = 24;

/// The follows feed: the newest ACTIVE products across
/// every store the caller follows (active stores only),
/// newest first — the home page's "new from stores you
/// follow" rail. One query over the follow join; flash
/// prices attach like every public product surface.
pub async fn follows_feed(pool: &PgPool, user: &AuthUser) -> Result<Vec<Product>, ApiError> {
    let mut products: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM store_follows f \
         JOIN stores s ON s.id = f.store_id AND s.status = 'active' \
         JOIN products p ON p.store_id = s.id AND p.status = 'active' \
         {PRODUCT_RATING_JOIN} \
         WHERE f.user_id = $1 \
         ORDER BY p.created_at DESC, p.id DESC LIMIT $2"
    ))
    .bind(user.user_id)
    .bind(FOLLOW_FEED_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load follows feed", e))?
    .into_iter()
    .map(crate::products::with_stock_status)
    .collect();
    crate::merch::attach_flash_prices(pool, &mut products, Utc::now()).await?;
    Ok(products)
}

/// The follows feed envelope (the rail's products + their
/// count, in the list shape).
#[derive(Debug, Serialize)]
pub struct FollowFeed {
    pub items: Vec<Product>,
    pub total: i64,
}

pub async fn follows_feed_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<FollowFeed>, ApiError> {
    let pool = state.require_pool()?;
    let items = follows_feed(pool, &user).await?;
    let total = items.len() as i64;
    Ok(Json(FollowFeed { items, total }))
}

/// One suggested store: the store plus the two public
/// signals the suggestion ranks by (followers, rating).
#[derive(Debug, Serialize)]
pub struct SuggestedStore {
    #[serde(flatten)]
    pub store: Store,
    pub follower_count: i64,
    pub rating_avg: Option<f64>,
    pub rating_count: i64,
    pub product_count: i64,
}

/// Flat row for the suggestions read.
#[derive(Debug, FromRow)]
struct SuggestedStoreRow {
    id: Uuid,
    follower_count: i64,
    rating_avg: Option<f64>,
    rating_count: i64,
    product_count: i64,
}

/// How many stores the suggestions return at most.
pub const SUGGESTED_STORES_LIMIT: i64 = 10;

/// Stores the caller might follow: ACTIVE stores they do
/// not own and do not already follow, ranked by follower
/// count, then by rating (reviewed stores first), then
/// newest. The ranking signals are the platform's real
/// aggregates — popularity and buyer verdict — never a
/// paid placement (promotions are a separate, labelled
/// surface).
pub async fn suggested_stores(
    pool: &PgPool,
    user: &AuthUser,
) -> Result<Vec<SuggestedStore>, ApiError> {
    let rows: Vec<SuggestedStoreRow> = sqlx::query_as(
        "SELECT s.id, \
         (SELECT COUNT(*) FROM store_follows f WHERE f.store_id = s.id) \
            AS follower_count, \
         (SELECT ROUND(AVG(r.rating)::numeric, 1)::float8 FROM reviews r \
          JOIN products p ON p.id = r.product_id WHERE p.store_id = s.id) \
            AS rating_avg, \
         (SELECT COUNT(*) FROM reviews r \
          JOIN products p ON p.id = r.product_id WHERE p.store_id = s.id) \
            AS rating_count, \
         (SELECT COUNT(*) FROM products p \
          WHERE p.store_id = s.id AND p.status = 'active') AS product_count \
         FROM stores s \
         WHERE s.status = 'active' AND s.owner_user_id <> $1 \
           AND NOT EXISTS (SELECT 1 FROM store_follows f \
                           WHERE f.store_id = s.id AND f.user_id = $1) \
         ORDER BY follower_count DESC, rating_count DESC, s.created_at DESC \
         LIMIT $2",
    )
    .bind(user.user_id)
    .bind(SUGGESTED_STORES_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load suggested stores", e))?;
    let mut stores: Vec<SuggestedStore> = Vec::with_capacity(rows.len());
    for row in rows {
        let store = crate::stores::get_store_by_id(pool, row.id).await?;
        stores.push(SuggestedStore {
            store,
            follower_count: row.follower_count,
            rating_avg: row.rating_avg,
            rating_count: row.rating_count,
            product_count: row.product_count,
        });
    }
    Ok(stores)
}

/// The suggestions envelope.
#[derive(Debug, Serialize)]
pub struct SuggestedStoreList {
    pub items: Vec<SuggestedStore>,
    pub total: i64,
}

pub async fn suggested_stores_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<SuggestedStoreList>, ApiError> {
    let pool = state.require_pool()?;
    let items = suggested_stores(pool, &user).await?;
    let total = items.len() as i64;
    Ok(Json(SuggestedStoreList { items, total }))
}

/// One day of the seller's follower-growth series.
#[derive(Debug, Serialize, FromRow)]
pub struct FollowerDay {
    /// The day, `YYYY-MM-DD` (Asia/Dhaka dates, like the
    /// sales analytics series).
    pub date: String,
    pub new_followers: i64,
}

/// The seller's follower statistics: the total, the last
/// 7 and 30 days' gains, and the 30-day daily series
/// (zero-filled — a quiet day is a data point, not a
/// gap).
#[derive(Debug, Serialize)]
pub struct FollowerStats {
    pub total: i64,
    pub last_7_days: i64,
    pub last_30_days: i64,
    pub daily: Vec<FollowerDay>,
}

/// Follower statistics for ONE store. The daily series is
/// generated in SQL (a generate_series spine left-joined
/// to the day's follows) so days with no follows still
/// appear with a zero.
pub async fn follower_stats(pool: &PgPool, store_id: Uuid) -> Result<FollowerStats, ApiError> {
    let total = follower_count(pool, store_id).await?;
    let daily: Vec<FollowerDay> = sqlx::query_as(
        "SELECT to_char(d.day, 'YYYY-MM-DD') AS date, \
         COUNT(f.user_id)::bigint AS new_followers \
         FROM generate_series( \
             (now() AT TIME ZONE 'Asia/Dhaka')::date - 29, \
             (now() AT TIME ZONE 'Asia/Dhaka')::date, \
             interval '1 day') AS d(day) \
         LEFT JOIN store_follows f ON f.store_id = $1 \
             AND (f.created_at AT TIME ZONE 'Asia/Dhaka')::date = d.day \
         GROUP BY d.day ORDER BY d.day ASC",
    )
    .bind(store_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load follower stats", e))?;
    let last_7_days: i64 = daily.iter().rev().take(7).map(|day| day.new_followers).sum();
    let last_30_days: i64 = daily.iter().map(|day| day.new_followers).sum();
    Ok(FollowerStats {
        total,
        last_7_days,
        last_30_days,
        daily,
    })
}

pub async fn seller_follower_stats_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<FollowerStats>, ApiError> {
    let pool = state.require_pool()?;
    let store = crate::stores::get_store_by_owner(pool, user.user_id).await?;
    Ok(Json(follower_stats(pool, store.id).await?))
}
