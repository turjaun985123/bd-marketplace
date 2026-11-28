//! Store content (Wave 59): the custom-pages and blog
//! system — Shopify's content half, which this platform's
//! stores lacked entirely (Wave 18/54 gave them about and
//! return-policy TEXT FIELDS on the storefront, but no
//! standalone pages and no blog).
//!
//! The rules, each enforced in exactly one place:
//!
//! - Access: every seller write resolves the store
//!   through the BRANDING rule (Wave 34
//!   `manage_marketing` via [`resolve_scoped_store`]) —
//!   the same gate as the branding write and the Wave 58
//!   layout: anonymous is a 401, a stranger naming the
//!   store gets the scoped 404, staff are refused with a
//!   403 (they run catalog, orders and questions, not
//!   marketing), managers and the owner may write.
//! - Shape: titles, bodies, slugs, tags and excerpts
//!   pass the domain validators; the per-store page and
//!   post caps are the domain's counts. Slug uniqueness
//!   is per store (the DB constraint is the backstop; a
//!   collision surfaces as a bilingual 409).
//! - Lifecycle: the domain content state machine — any
//!   status may move to any other, staying put is a 400.
//!   Publishing a post stamps `published_at` (the
//!   seller's chosen moment, or now); a future stamp is
//!   a SCHEDULED post, committed but not yet public.
//!   Unpublishing clears the stamp; archiving keeps it
//!   as the record of when the post was last live. One
//!   deliberate exception to the no-op refusal:
//!   re-publishing an already-published post WITH a new
//!   `published_at` is a reschedule (a stamp change, not
//!   a transition) and is allowed.
//! - Visibility: public reads resolve the store by slug
//!   and a suspended store's content 404s exactly like
//!   its products do. A page is public when published; a
//!   post is public when published AND its moment has
//!   arrived (domain [`content_is_live`]). Anything else
//!   is the uniform 404 — drafts and scheduled posts are
//!   indistinguishable from missing ones.
//! - Bodies are PLAIN TEXT end to end: stored verbatim
//!   (edge-trimmed), rendered by the web app as
//!   paragraphs. No HTML is stored, honoured or emitted
//!   from seller input anywhere in this module — the
//!   Atom feed escapes every interpolation through the
//!   domain's `xml_escape`, so seller text reaches feed
//!   readers as text, never markup.
//! - Views: a post read is counted by an anonymous
//!   beacon into the aggregate-only events table
//!   (migration 0069 — no user, no session, no IP), the
//!   Wave 58 pattern. The seller stats read counts rows;
//!   it never sees people.

use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, SecondsFormat, Utc};
use domain::{
    can_transition_content_status, content_is_live, count_words, derive_post_excerpt,
    is_valid_content_status, normalize_post_tags, reading_minutes_for_words,
    validate_content_slug, validate_external_image_url, validate_post_excerpt,
    validate_store_page_body, validate_store_page_count, validate_store_page_title,
    validate_store_post_body, validate_store_post_count, validate_store_post_title,
    xml_escape, CONTENT_STATUS_PUBLISHED, STORE_POST_FEED_ITEMS, STORE_POST_STATS_WINDOW_DAYS,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::access::{resolve_scoped_store, StoreScopeQuery};
use crate::admin::require_admin;
use crate::auth::AuthUser;
use crate::media;
use crate::product_images;
use crate::stores::{get_store_by_slug, Store};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Rows and views
// ---------------------------------------------------------------------------

/// One row of `store_pages`; also the page's JSON shape
/// on every surface (the seller's list and editor, the
/// public page read). A page has no audience-dependent
/// fields — the public read simply refuses unpublished
/// rows — so one view serves both.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct StorePage {
    pub id: Uuid,
    pub store_id: Uuid,
    pub slug: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub show_in_nav: bool,
    pub position: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One nav entry of the public pages list: the slug and
/// title the storefront frame links with. Bodies do not
/// ride the nav read — the frame renders on every store
/// page and stays light.
#[derive(Debug, Serialize, FromRow)]
pub struct PublicPageSummary {
    pub slug: String,
    pub title: String,
}

/// The public pages list envelope: the nav entries plus
/// the store's live blog-post count, so the storefront
/// frame can decide the Blog link in the same round
/// trip (it renders the link only when the blog has at
/// least one live post).
#[derive(Debug, Serialize)]
pub struct PublicPageList {
    pub items: Vec<PublicPageSummary>,
    pub total: i64,
    pub blog_post_count: i64,
}

/// One row of `store_posts`, before it becomes a view:
/// the body rides the row (the effective excerpt derives
/// from it) but is only serialised on the detail views.
#[derive(Debug, Clone, FromRow)]
struct PostRow {
    id: Uuid,
    store_id: Uuid,
    slug: String,
    title: String,
    body: String,
    excerpt: Option<String>,
    tags: Vec<String>,
    cover_image_url: Option<String>,
    status: String,
    published_at: Option<DateTime<Utc>>,
    word_count: i32,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl PostRow {
    /// The excerpt a reader sees: the seller's own when
    /// written, otherwise derived from the body by the
    /// domain rule.
    fn effective_excerpt(&self) -> String {
        match &self.excerpt {
            Some(excerpt) => excerpt.clone(),
            None => derive_post_excerpt(&self.body),
        }
    }

    fn reading_minutes(&self) -> i64 {
        reading_minutes_for_words(self.word_count as i64)
    }
}

/// A post as the seller's list shows it: everything but
/// the body, plus the excerpt in effect and the
/// aggregate view count.
#[derive(Debug, Serialize)]
pub struct SellerPost {
    pub id: Uuid,
    pub store_id: Uuid,
    pub slug: String,
    pub title: String,
    pub excerpt: String,
    pub has_custom_excerpt: bool,
    pub tags: Vec<String>,
    pub cover_image_url: Option<String>,
    pub status: String,
    pub published_at: Option<DateTime<Utc>>,
    pub word_count: i32,
    pub reading_minutes: i64,
    pub view_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&PostRow> for SellerPost {
    fn from(row: &PostRow) -> Self {
        SellerPost {
            id: row.id,
            store_id: row.store_id,
            slug: row.slug.clone(),
            title: row.title.clone(),
            excerpt: row.effective_excerpt(),
            has_custom_excerpt: row.excerpt.is_some(),
            tags: row.tags.clone(),
            cover_image_url: row.cover_image_url.clone(),
            status: row.status.clone(),
            published_at: row.published_at,
            word_count: row.word_count,
            reading_minutes: row.reading_minutes(),
            view_count: 0,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// The seller's detail of one post: the list shape plus
/// the body and the raw stored excerpt (the editor's
/// starting state — it edits the seller's text, not the
/// derived teaser).
#[derive(Debug, Serialize)]
pub struct SellerPostDetail {
    #[serde(flatten)]
    pub post: SellerPost,
    pub body: String,
    pub custom_excerpt: Option<String>,
}

/// A post as public list surfaces show it (blog index,
/// tag filter, discovery): no body, the excerpt in
/// effect, and the store identity the cross-store
/// discovery surfaces need (the per-store lists simply
/// repeat their own store).
#[derive(Debug, Serialize)]
pub struct PublicPost {
    pub id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub store_name: String,
    pub slug: String,
    pub title: String,
    pub excerpt: String,
    pub tags: Vec<String>,
    pub cover_image_url: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub reading_minutes: i64,
}

/// The public post page: the list shape plus the full
/// body, the previous/next navigation in the blog's
/// publish order, and the related posts ranked by
/// shared tags.
#[derive(Debug, Serialize)]
pub struct PublicPostDetail {
    #[serde(flatten)]
    pub post: PublicPost,
    pub body: String,
    pub previous: Option<PostNavLink>,
    pub next: Option<PostNavLink>,
    pub related: Vec<PublicPost>,
}

/// One end of the prev/next navigation: just enough to
/// link and label.
#[derive(Debug, Serialize, FromRow)]
pub struct PostNavLink {
    pub slug: String,
    pub title: String,
}

/// The paginated envelope the post lists share.
#[derive(Debug, Serialize)]
pub struct PostList {
    pub items: Vec<PublicPost>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// The paginated envelope the seller's post list uses.
#[derive(Debug, Serialize)]
pub struct SellerPostList {
    pub items: Vec<SellerPost>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// One post's line in the seller stats: its identity and
/// its view counts (all-time and the trailing window).
#[derive(Debug, Serialize, FromRow)]
pub struct PostStatLine {
    pub post_id: Uuid,
    pub slug: String,
    pub title: String,
    pub status: String,
    pub total_views: i64,
    pub window_views: i64,
}

/// The seller's blog statistics (Wave 59): store-wide
/// totals plus one line per post, most-viewed first.
/// Aggregate-only — counts of reads, never readers.
#[derive(Debug, Serialize)]
pub struct PostStats {
    pub total_views: i64,
    pub window_views: i64,
    pub window_days: i64,
    pub posts: Vec<PostStatLine>,
}

const PAGE_COLUMNS: &str = "id, store_id, slug, title, body, status, show_in_nav, \
    position, created_at, updated_at";
const POST_COLUMNS: &str = "id, store_id, slug, title, body, excerpt, tags, \
    cover_image_url, status, published_at, word_count, created_at, updated_at";

/// Is this database error a unique-violation (SQLSTATE
/// 23505)? The (store, slug) uniqueness surfaces as one;
/// the API translates it into a 409 instead of leaking a
/// 500 (the store-collections rule, copied deliberately).
fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(|db| db.code())
        .is_some_and(|code| code == "23505")
}

/// The one live-post WHERE fragment every public post
/// read shares: published, stamped, and the stamp
/// arrived. Kept beside the domain's `content_is_live`
/// (the same rule in SQL) so the two can be compared at
/// a glance.
const LIVE_POST_WHERE: &str = "status = 'published' AND published_at IS NOT NULL \
    AND published_at <= now()";

/// Resolve the store a seller content request acts on:
/// the branding rule (Wave 34 `manage_marketing`),
/// exactly as the branding write and the Wave 58 layout
/// resolve theirs.
async fn resolve_content_store(
    pool: &PgPool,
    user: &AuthUser,
    scope: &StoreScopeQuery,
) -> Result<Store, ApiError> {
    let (store, _role) = resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        domain::STORE_ACTION_MANAGE_MARKETING,
    )
    .await?;
    Ok(store)
}

/// Resolve a store for a PUBLIC content read: by slug,
/// and a suspended store answers the same 404 as a
/// missing one — its content hides with the store, like
/// its products (the store-collections convention).
async fn public_store(pool: &PgPool, slug: &str) -> Result<Store, ApiError> {
    let store = get_store_by_slug(pool, slug).await?;
    if store.status != "active" {
        return Err(ApiError::not_found("store not found"));
    }
    Ok(store)
}

// ---------------------------------------------------------------------------
// Pages — seller writes
// ---------------------------------------------------------------------------

/// Every page of one store, in the seller's arrangement
/// (position, then creation). All statuses: the seller
/// sees their drafts and archives too.
pub async fn list_seller_pages(pool: &PgPool, store_id: Uuid) -> Result<Vec<StorePage>, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {PAGE_COLUMNS} FROM store_pages WHERE store_id = $1 \
         ORDER BY position ASC, created_at ASC, id ASC"
    ))
    .bind(store_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list store pages", e))
}

/// Load one page of one store (404 when it is not the
/// store's — the load is scoped, so a foreign id and a
/// missing id are the same answer).
async fn load_seller_page(
    pool: &PgPool,
    store_id: Uuid,
    page_id: Uuid,
) -> Result<StorePage, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {PAGE_COLUMNS} FROM store_pages WHERE store_id = $1 AND id = $2"
    ))
    .bind(store_id)
    .bind(page_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load store page", e))?
    .ok_or_else(|| ApiError::not_found("page not found"))
}

/// The page create/update payload: on create, slug,
/// title and body are required (the service enforces);
/// on update every field is optional and merges (absent
/// keeps). Status never rides this payload — it moves
/// through the status action, where the state machine
/// guards it.
#[derive(Debug, Deserialize)]
pub struct PageInput {
    pub slug: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub show_in_nav: Option<bool>,
}

/// Create one page for the store: validated shape, the
/// domain cap on the store's page count, appended at
/// the end of the seller's arrangement.
pub async fn create_page(
    pool: &PgPool,
    store: &Store,
    input: &PageInput,
) -> Result<StorePage, ApiError> {
    let slug = validate_content_slug(input.slug.as_deref().unwrap_or("").trim())
        .map_err(ApiError::bad_request)?;
    let title =
        validate_store_page_title(input.title.as_deref().unwrap_or("")).map_err(ApiError::bad_request)?;
    let body =
        validate_store_page_body(input.body.as_deref().unwrap_or("")).map_err(ApiError::bad_request)?;
    let current: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM store_pages WHERE store_id = $1")
        .bind(store.id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count store pages", e))?;
    validate_store_page_count(current).map_err(ApiError::bad_request)?;
    let page: StorePage = sqlx::query_as(&format!(
        "INSERT INTO store_pages (store_id, slug, title, body, show_in_nav, position) \
         VALUES ($1, $2, $3, $4, $5, \
                 (SELECT COALESCE(MAX(position), -1) + 1 FROM store_pages \
                  WHERE store_id = $1)) \
         RETURNING {PAGE_COLUMNS}"
    ))
    .bind(store.id)
    .bind(&slug)
    .bind(&title)
    .bind(&body)
    .bind(input.show_in_nav.unwrap_or(false))
    .fetch_one(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict(
                "এই স্লাগে এই স্টোরের একটি পেজ আগেই আছে — this store already \
                 has a page with that slug",
            )
        } else {
            ApiError::internal("create store page", e)
        }
    })?;
    Ok(page)
}

/// Update one of the store's pages (merge semantics —
/// see [`PageInput`]).
pub async fn update_page(
    pool: &PgPool,
    store: &Store,
    page_id: Uuid,
    input: &PageInput,
) -> Result<StorePage, ApiError> {
    let existing = load_seller_page(pool, store.id, page_id).await?;
    let slug = match input.slug.as_deref() {
        Some(raw) => validate_content_slug(raw.trim()).map_err(ApiError::bad_request)?,
        None => existing.slug.clone(),
    };
    let title = match input.title.as_deref() {
        Some(raw) => validate_store_page_title(raw).map_err(ApiError::bad_request)?,
        None => existing.title.clone(),
    };
    let body = match input.body.as_deref() {
        Some(raw) => validate_store_page_body(raw).map_err(ApiError::bad_request)?,
        None => existing.body.clone(),
    };
    let show_in_nav = input.show_in_nav.unwrap_or(existing.show_in_nav);
    sqlx::query(
        "UPDATE store_pages SET slug = $3, title = $4, body = $5, \
         show_in_nav = $6, updated_at = now() \
         WHERE store_id = $1 AND id = $2",
    )
    .bind(store.id)
    .bind(page_id)
    .bind(&slug)
    .bind(&title)
    .bind(&body)
    .bind(show_in_nav)
    .execute(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict(
                "এই স্লাগে এই স্টোরের একটি পেজ আগেই আছে — this store already \
                 has a page with that slug",
            )
        } else {
            ApiError::internal("update store page", e)
        }
    })?;
    load_seller_page(pool, store.id, page_id).await
}

/// Delete one of the store's pages. Deleting a page
/// that is not the store's is a 404 — a mistyped id
/// must not look like a success.
pub async fn delete_page(pool: &PgPool, store: &Store, page_id: Uuid) -> Result<(), ApiError> {
    let result = sqlx::query("DELETE FROM store_pages WHERE store_id = $1 AND id = $2")
        .bind(store.id)
        .bind(page_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("delete store page", e))?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("page not found"));
    }
    Ok(())
}

/// The reorder payload: the store's page ids in their
/// new order — the WHOLE set, exactly once each (the
/// membership-replace convention from store
/// collections: a partial list would silently strand
/// the unlisted pages at the end, so it is refused).
#[derive(Debug, Deserialize)]
pub struct ReorderPagesInput {
    pub page_ids: Vec<Uuid>,
}

/// Rewrite the store's page arrangement from the
/// submitted id list, in one transaction: the list
/// must be exactly the store's current page set (a
/// stale editor's list is a bilingual 400, nothing is
/// rewritten), then positions are restamped 0..n.
pub async fn reorder_pages(
    pool: &PgPool,
    store: &Store,
    input: &ReorderPagesInput,
) -> Result<Vec<StorePage>, ApiError> {
    let existing: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM store_pages WHERE store_id = $1")
            .bind(store.id)
            .fetch_all(pool)
            .await
            .map_err(|e| ApiError::internal("load store page ids", e))?;
    let mut submitted_sorted = input.page_ids.clone();
    submitted_sorted.sort();
    let mut existing_sorted = existing.clone();
    existing_sorted.sort();
    let mut deduped = submitted_sorted.clone();
    deduped.dedup();
    if deduped.len() != input.page_ids.len() || submitted_sorted != existing_sorted {
        return Err(ApiError::bad_request(
            "তালিকাটি স্টোরের সব পেজের হুবহু সেট হতে হবে — the reorder list \
             must be exactly the store's page set, each page once; reload \
             the editor and try again",
        ));
    }
    let mut tx = pool.begin().await.map_err(|e| ApiError::internal("begin", e))?;
    for (position, page_id) in input.page_ids.iter().enumerate() {
        sqlx::query(
            "UPDATE store_pages SET position = $3, updated_at = now() \
             WHERE store_id = $1 AND id = $2",
        )
        .bind(store.id)
        .bind(page_id)
        .bind(position as i32)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("reorder store pages", e))?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit", e))?;
    list_seller_pages(pool, store.id).await
}

/// The status-action payload: the target status. The
/// post variant also carries the publish moment.
#[derive(Debug, Deserialize)]
pub struct PageStatusInput {
    pub status: String,
}

/// Move one page to a new status through the domain
/// state machine: an unknown target and a no-op are
/// both bilingual 400s, and a refused move changes
/// nothing.
pub async fn set_page_status(
    pool: &PgPool,
    store: &Store,
    page_id: Uuid,
    input: &PageStatusInput,
) -> Result<StorePage, ApiError> {
    let existing = load_seller_page(pool, store.id, page_id).await?;
    let target = input.status.trim();
    if !is_valid_content_status(target) {
        return Err(ApiError::bad_request(
            "অবস্থাটি draft, published বা archived হতে হবে — the status \
             must be draft, published or archived",
        ));
    }
    if !can_transition_content_status(&existing.status, target) {
        return Err(ApiError::bad_request(format!(
            "পেজটি ইতিমধ্যে '{target}' অবস্থায় আছে — the page is already \
             '{target}'"
        )));
    }
    sqlx::query(
        "UPDATE store_pages SET status = $3, updated_at = now() \
         WHERE store_id = $1 AND id = $2",
    )
    .bind(store.id)
    .bind(page_id)
    .bind(target)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("set page status", e))?;
    load_seller_page(pool, store.id, page_id).await
}

// ---------------------------------------------------------------------------
// Posts — seller writes
// ---------------------------------------------------------------------------

/// Load one post row of one store (404 when it is not
/// the store's).
async fn load_post_row(
    pool: &PgPool,
    store_id: Uuid,
    post_id: Uuid,
) -> Result<PostRow, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {POST_COLUMNS} FROM store_posts WHERE store_id = $1 AND id = $2"
    ))
    .bind(store_id)
    .bind(post_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load store post", e))?
    .ok_or_else(|| ApiError::not_found("post not found"))
}

/// Load one post row by store + slug (seller reads).
async fn load_post_row_by_slug(
    pool: &PgPool,
    store_id: Uuid,
    slug: &str,
) -> Result<PostRow, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {POST_COLUMNS} FROM store_posts WHERE store_id = $1 AND slug = $2"
    ))
    .bind(store_id)
    .bind(slug)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load store post by slug", e))?
    .ok_or_else(|| ApiError::not_found("post not found"))
}

/// The seller detail of one post (list shape + body +
/// the raw stored excerpt).
fn seller_detail(row: &PostRow, view_count: i64) -> SellerPostDetail {
    let mut post = SellerPost::from(row);
    post.view_count = view_count;
    SellerPostDetail {
        custom_excerpt: row.excerpt.clone(),
        body: row.body.clone(),
        post,
    }
}

/// One post's all-time view count (the seller list and
/// detail carry it; the stats endpoint is the full
/// picture).
async fn post_view_count(pool: &PgPool, post_id: Uuid) -> Result<i64, ApiError> {
    sqlx::query_scalar("SELECT COUNT(*) FROM store_post_events WHERE post_id = $1")
        .bind(post_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count post views", e))
}

/// The seller's post list: one store's posts, newest
/// activity first, optionally filtered by status and a
/// title/slug search, paginated. Bodies never ride the
/// list; each item carries its view count.
pub async fn list_seller_posts(
    pool: &PgPool,
    store: &Store,
    status: Option<&str>,
    q: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<SellerPostList, ApiError> {
    if let Some(status) = status {
        if !is_valid_content_status(status) {
            return Err(ApiError::bad_request(
                "অবস্থাটি draft, published বা archived হতে হবে — the status \
                 filter must be draft, published or archived",
            ));
        }
    }
    let (page, limit) = crate::products::clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let needle = q.map(str::trim).filter(|s| !s.is_empty());
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_posts WHERE store_id = $1 \
         AND ($2::text IS NULL OR status = $2) \
         AND ($3::text IS NULL OR title ILIKE '%' || $3 || '%' OR slug ILIKE '%' || $3 || '%')",
    )
    .bind(store.id)
    .bind(status)
    .bind(needle)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count seller posts", e))?;
    let rows: Vec<PostRow> = sqlx::query_as(&format!(
        "SELECT {POST_COLUMNS} FROM store_posts WHERE store_id = $1 \
         AND ($2::text IS NULL OR status = $2) \
         AND ($3::text IS NULL OR title ILIKE '%' || $3 || '%' OR slug ILIKE '%' || $3 || '%') \
         ORDER BY updated_at DESC, id ASC LIMIT $4 OFFSET $5"
    ))
    .bind(store.id)
    .bind(status)
    .bind(needle)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list seller posts", e))?;
    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut item = SellerPost::from(row);
        item.view_count = post_view_count(pool, row.id).await?;
        items.push(item);
    }
    Ok(SellerPostList {
        items,
        total,
        page,
        limit,
    })
}

/// The post create/update payload: on create, slug,
/// title and body are required; on update every field
/// is optional and merges — absent keeps, and for the
/// two nullable texts (excerpt, cover) a blank string
/// CLEARS to NULL (the branding convention: a blank
/// excerpt means "derive it", a blank cover means "no
/// cover"). Status and the publish moment move through
/// the status action, never this payload — except at
/// create, where `status` may start the post published
/// (with `published_at` as its moment, past or future).
#[derive(Debug, Deserialize)]
pub struct PostInput {
    pub slug: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub excerpt: Option<String>,
    pub tags: Option<Vec<String>>,
    pub cover_image_url: Option<String>,
    pub status: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
}

/// Validate the cover field of a post payload: absent
/// keeps (the caller substitutes), blank clears, a
/// value must pass the platform's external-image-URL
/// rule (the Wave 51 validator — http(s) only, no
/// whitespace or control characters).
fn validate_cover_field(raw: Option<&str>) -> Result<Option<String>, ApiError> {
    match raw {
        None => Ok(None),
        Some(value) if value.trim().is_empty() => Ok(None),
        Some(value) => validate_external_image_url(value)
            .map(Some)
            .map_err(ApiError::bad_request),
    }
}

/// Create one post for the store: validated shape, the
/// domain cap on the store's post count, the word count
/// computed at write time, and the publish stamp set
/// when the post starts published.
pub async fn create_post(
    pool: &PgPool,
    store: &Store,
    input: &PostInput,
) -> Result<SellerPostDetail, ApiError> {
    let slug = validate_content_slug(input.slug.as_deref().unwrap_or("").trim())
        .map_err(ApiError::bad_request)?;
    let title =
        validate_store_post_title(input.title.as_deref().unwrap_or("")).map_err(ApiError::bad_request)?;
    let body =
        validate_store_post_body(input.body.as_deref().unwrap_or("")).map_err(ApiError::bad_request)?;
    let excerpt = validate_post_excerpt(input.excerpt.as_deref()).map_err(ApiError::bad_request)?;
    let tags = normalize_post_tags(input.tags.as_deref().unwrap_or(&[]))
        .map_err(ApiError::bad_request)?;
    let cover = validate_cover_field(input.cover_image_url.as_deref())?;
    let status = match input.status.as_deref() {
        Some(raw) if is_valid_content_status(raw.trim()) => raw.trim().to_string(),
        Some(_) => {
            return Err(ApiError::bad_request(
                "অবস্থাটি draft, published বা archived হতে হবে — the status \
                 must be draft, published or archived",
            ))
        }
        None => domain::CONTENT_STATUS_DRAFT.to_string(),
    };
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin create post", e))?;
    let current: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM store_posts WHERE store_id = $1")
        .bind(store.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("count store posts", e))?;
    validate_store_post_count(current).map_err(ApiError::bad_request)?;
    let published_at = if status == CONTENT_STATUS_PUBLISHED {
        Some(input.published_at.unwrap_or_else(Utc::now))
    } else {
        None
    };
    let word_count = count_words(&body) as i32;
    let row: PostRow = sqlx::query_as(&format!(
        "INSERT INTO store_posts (store_id, slug, title, body, excerpt, tags, \
         cover_image_url, status, published_at, word_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING {POST_COLUMNS}"
    ))
    .bind(store.id)
    .bind(&slug)
    .bind(&title)
    .bind(&body)
    .bind(&excerpt)
    .bind(&tags)
    .bind(&cover)
    .bind(&status)
    .bind(published_at)
    .bind(word_count)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict(
                "এই স্লাগে এই স্টোরের একটি পোস্ট আগেই আছে — this store \
                 already has a post with that slug",
            )
        } else {
            ApiError::internal("create store post", e)
        }
    })?;
    // Born live (published with a moment that has already
    // arrived): the followers hear about it in the same
    // transaction. A scheduled birth (future moment) is
    // silent — and stays silent when the moment passes,
    // because nothing writes then (see notices.rs).
    if content_is_live(
        &row.status,
        row.published_at.map(|at| at.timestamp()),
        Utc::now().timestamp(),
    ) {
        crate::notices::enqueue_store_new_post_notices(&mut tx, row.id).await?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit create post", e))?;
    Ok(seller_detail(&row, 0))
}

/// Update one of the store's posts (merge semantics —
/// see [`PostInput`]). The body, when it changes,
/// recomputes the stored word count in the same write.
pub async fn update_post(
    pool: &PgPool,
    store: &Store,
    post_id: Uuid,
    input: &PostInput,
) -> Result<SellerPostDetail, ApiError> {
    let existing = load_post_row(pool, store.id, post_id).await?;
    let slug = match input.slug.as_deref() {
        Some(raw) => validate_content_slug(raw.trim()).map_err(ApiError::bad_request)?,
        None => existing.slug.clone(),
    };
    let title = match input.title.as_deref() {
        Some(raw) => validate_store_post_title(raw).map_err(ApiError::bad_request)?,
        None => existing.title.clone(),
    };
    let body = match input.body.as_deref() {
        Some(raw) => validate_store_post_body(raw).map_err(ApiError::bad_request)?,
        None => existing.body.clone(),
    };
    let excerpt = match input.excerpt.as_deref() {
        Some(raw) => validate_post_excerpt(Some(raw)).map_err(ApiError::bad_request)?,
        None => existing.excerpt.clone(),
    };
    let tags = match input.tags.as_ref() {
        Some(raw) => normalize_post_tags(raw).map_err(ApiError::bad_request)?,
        None => existing.tags.clone(),
    };
    let cover = match input.cover_image_url.as_deref() {
        Some(raw) => validate_cover_field(Some(raw))?,
        None => existing.cover_image_url.clone(),
    };
    let word_count = count_words(&body) as i32;
    sqlx::query(
        "UPDATE store_posts SET slug = $3, title = $4, body = $5, excerpt = $6, \
         tags = $7, cover_image_url = $8, word_count = $9, updated_at = now() \
         WHERE store_id = $1 AND id = $2",
    )
    .bind(store.id)
    .bind(post_id)
    .bind(&slug)
    .bind(&title)
    .bind(&body)
    .bind(&excerpt)
    .bind(&tags)
    .bind(&cover)
    .bind(word_count)
    .execute(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict(
                "এই স্লাগে এই স্টোরের একটি পোস্ট আগেই আছে — this store \
                 already has a post with that slug",
            )
        } else {
            ApiError::internal("update store post", e)
        }
    })?;
    let row = load_post_row(pool, store.id, post_id).await?;
    let views = post_view_count(pool, post_id).await?;
    Ok(seller_detail(&row, views))
}

/// Delete one of the store's posts: the row, its view
/// events (FK cascade), and — when the cover was an
/// upload — its stored objects. A hotlinked cover has
/// no objects; the delete call is keyed and idempotent
/// either way.
pub async fn delete_post(state: &AppState, store: &Store, post_id: Uuid) -> Result<(), ApiError> {
    let pool = state.require_pool()?;
    let existing = load_post_row(pool, store.id, post_id).await?;
    sqlx::query("DELETE FROM store_posts WHERE store_id = $1 AND id = $2")
        .bind(store.id)
        .bind(post_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("delete store post", e))?;
    if existing.cover_image_url.is_some() {
        media::delete_stored_image(
            state.media_store.as_ref(),
            &cover_base_key(store.id, post_id),
            None,
        )
        .await;
    }
    Ok(())
}

/// The status-action payload for a post: the target
/// status plus, for a publish (or a reschedule of a
/// live post), the publish moment.
#[derive(Debug, Deserialize)]
pub struct PostStatusInput {
    pub status: String,
    pub published_at: Option<DateTime<Utc>>,
}

/// Move one post through the lifecycle (see the module
/// doc for the stamp rules and the reschedule
/// exception).
pub async fn set_post_status(
    pool: &PgPool,
    store: &Store,
    post_id: Uuid,
    input: &PostStatusInput,
) -> Result<SellerPostDetail, ApiError> {
    let existing = load_post_row(pool, store.id, post_id).await?;
    let target = input.status.trim();
    if !is_valid_content_status(target) {
        return Err(ApiError::bad_request(
            "অবস্থাটি draft, published বা archived হতে হবে — the status \
             must be draft, published or archived",
        ));
    }
    if existing.status == target {
        // The one allowed no-op: re-publishing a live
        // post with a NEW moment is a reschedule.
        if target == CONTENT_STATUS_PUBLISHED {
            if let Some(moment) = input.published_at {
                // A reschedule that pulls a SCHEDULED
                // post into the present makes it live for
                // the first time — the followers hear
                // about it. Rescheduling a post that was
                // already live does not re-fire (they
                // heard at the first publish).
                let now = Utc::now();
                let was_live = content_is_live(
                    &existing.status,
                    existing.published_at.map(|at| at.timestamp()),
                    now.timestamp(),
                );
                let lands_live = moment <= now;
                let mut tx = pool
                    .begin()
                    .await
                    .map_err(|e| ApiError::internal("begin reschedule post", e))?;
                sqlx::query(
                    "UPDATE store_posts SET published_at = $3, updated_at = now() \
                     WHERE store_id = $1 AND id = $2",
                )
                .bind(store.id)
                .bind(post_id)
                .bind(moment)
                .execute(&mut *tx)
                .await
                .map_err(|e| ApiError::internal("reschedule store post", e))?;
                if !was_live && lands_live {
                    crate::notices::enqueue_store_new_post_notices(&mut tx, post_id).await?;
                }
                tx.commit()
                    .await
                    .map_err(|e| ApiError::internal("commit reschedule post", e))?;
                let row = load_post_row(pool, store.id, post_id).await?;
                let views = post_view_count(pool, post_id).await?;
                return Ok(seller_detail(&row, views));
            }
        }
        return Err(ApiError::bad_request(format!(
            "পোস্টটি ইতিমধ্যে '{target}' অবস্থায় আছে — the post is already \
             '{target}'"
        )));
    }
    if !can_transition_content_status(&existing.status, target) {
        return Err(ApiError::bad_request(format!(
            "'{}' থেকে '{target}' অবস্থায় যাওয়া যাবে না — cannot move \
             from '{}' to '{target}'",
            existing.status, existing.status
        )));
    }
    let published_at: Option<DateTime<Utc>> = if target == CONTENT_STATUS_PUBLISHED {
        Some(
            input
                .published_at
                .or(existing.published_at)
                .unwrap_or_else(Utc::now),
        )
    } else if target == domain::CONTENT_STATUS_DRAFT {
        None
    } else {
        existing.published_at
    };
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin set post status", e))?;
    sqlx::query(
        "UPDATE store_posts SET status = $3, published_at = $4, updated_at = now() \
         WHERE store_id = $1 AND id = $2",
    )
    .bind(store.id)
    .bind(post_id)
    .bind(target)
    .bind(published_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("set post status", e))?;
    // A move INTO live (from draft or archived, with a
    // moment that has arrived) fires the follower
    // notice in the same transaction; a move into a
    // future schedule stays silent.
    if target == CONTENT_STATUS_PUBLISHED
        && content_is_live(
            target,
            published_at.map(|at| at.timestamp()),
            Utc::now().timestamp(),
        )
    {
        crate::notices::enqueue_store_new_post_notices(&mut tx, post_id).await?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit set post status", e))?;
    let row = load_post_row(pool, store.id, post_id).await?;
    let views = post_view_count(pool, post_id).await?;
    Ok(seller_detail(&row, views))
}

/// The media base key of one post's cover upload: the
/// branding pattern (`stores/<store>/<kind>`) extended
/// per post, so one post's cover never collides with
/// another's or with the store's own images.
fn cover_base_key(store_id: Uuid, post_id: Uuid) -> String {
    format!("stores/{store_id}/posts/{post_id}/cover")
}

/// Upload (or replace) one post's cover through the
/// Wave 51 media pipeline: the bytes are sniffed,
/// re-encoded and stored under the post's key; the old
/// objects are removed first (the branding rule), and
/// the post's cover URL becomes the stored original's
/// public URL.
pub async fn upload_post_cover(
    state: &AppState,
    store: &Store,
    post_id: Uuid,
    bytes: &[u8],
    declared_content_type: Option<&str>,
) -> Result<SellerPostDetail, ApiError> {
    let pool = state.require_pool()?;
    let _existing = load_post_row(pool, store.id, post_id).await?;
    let base_key = cover_base_key(store.id, post_id);
    let processed = media::process_upload(&base_key, bytes, declared_content_type)?;
    media::delete_stored_image(state.media_store.as_ref(), &base_key, None).await;
    media::store_processed(state.media_store.as_ref(), &processed).await?;
    let url = media::public_url_for_key(&media::original_key(&base_key));
    sqlx::query(
        "UPDATE store_posts SET cover_image_url = $3, updated_at = now() \
         WHERE store_id = $1 AND id = $2",
    )
    .bind(store.id)
    .bind(post_id)
    .bind(&url)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("set post cover", e))?;
    let row = load_post_row(pool, store.id, post_id).await?;
    let views = post_view_count(pool, post_id).await?;
    Ok(seller_detail(&row, views))
}

/// Remove one post's cover: stored objects go when the
/// cover was an upload (its URL names this post's key);
/// a hotlinked cover is simply cleared.
pub async fn delete_post_cover(
    state: &AppState,
    store: &Store,
    post_id: Uuid,
) -> Result<SellerPostDetail, ApiError> {
    let pool = state.require_pool()?;
    let existing = load_post_row(pool, store.id, post_id).await?;
    let base_key = cover_base_key(store.id, post_id);
    let uploaded_url = media::public_url_for_key(&media::original_key(&base_key));
    if existing.cover_image_url.as_deref() == Some(uploaded_url.as_str()) {
        media::delete_stored_image(state.media_store.as_ref(), &base_key, None).await;
    }
    sqlx::query(
        "UPDATE store_posts SET cover_image_url = NULL, updated_at = now() \
         WHERE store_id = $1 AND id = $2",
    )
    .bind(store.id)
    .bind(post_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("clear post cover", e))?;
    let row = load_post_row(pool, store.id, post_id).await?;
    let views = post_view_count(pool, post_id).await?;
    Ok(seller_detail(&row, views))
}

/// The seller's blog statistics: store-wide totals and
/// one line per post (most-viewed first), over the
/// trailing [`STORE_POST_STATS_WINDOW_DAYS`]-day rolling
/// window plus all time.
pub async fn seller_post_stats(pool: &PgPool, store: &Store) -> Result<PostStats, ApiError> {
    let lines: Vec<PostStatLine> = sqlx::query_as(
        "SELECT p.id AS post_id, p.slug, p.title, p.status, \
         COUNT(e.id)::bigint AS total_views, \
         COUNT(e.id) FILTER (WHERE e.created_at >= now() - make_interval(days => $2))::bigint \
             AS window_views \
         FROM store_posts p \
         LEFT JOIN store_post_events e ON e.post_id = p.id \
         WHERE p.store_id = $1 \
         GROUP BY p.id, p.slug, p.title, p.status \
         ORDER BY total_views DESC, p.created_at DESC, p.id ASC",
    )
    .bind(store.id)
    .bind(STORE_POST_STATS_WINDOW_DAYS as i32)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load post stats", e))?;
    let total_views: i64 = lines.iter().map(|line| line.total_views).sum();
    let window_views: i64 = lines.iter().map(|line| line.window_views).sum();
    Ok(PostStats {
        total_views,
        window_views,
        window_days: STORE_POST_STATS_WINDOW_DAYS,
        posts: lines,
    })
}

// ---------------------------------------------------------------------------
// Admin moderation + data export (Wave 59)
// ---------------------------------------------------------------------------

/// One post in the platform admin's content list: the
/// post's identity and state plus the store it belongs
/// to (moderation works across stores).
#[derive(Debug, Serialize, FromRow)]
pub struct AdminPostRow {
    pub id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub store_name: String,
    pub slug: String,
    pub title: String,
    pub status: String,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One page in the platform admin's content list.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminPageRow {
    pub id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub store_name: String,
    pub slug: String,
    pub title: String,
    pub status: String,
    pub show_in_nav: bool,
    pub position: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The admin content list envelope (either kind).
#[derive(Debug, Serialize)]
pub struct AdminContentList<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

const ADMIN_POST_COLUMNS: &str = "p.id, p.store_id, s.slug AS store_slug, \
    s.name AS store_name, p.slug, p.title, p.status, p.published_at, \
    p.created_at, p.updated_at";

const ADMIN_PAGE_COLUMNS: &str = "p.id, p.store_id, s.slug AS store_slug, \
    s.name AS store_name, p.slug, p.title, p.status, p.show_in_nav, \
    p.position, p.created_at, p.updated_at";

/// Validate an optional admin status filter: absent is
/// "all", a present one must be a real content status.
fn admin_status_filter(raw: Option<&str>) -> Result<Option<String>, ApiError> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) if is_valid_content_status(value) => Ok(Some(value.to_string())),
        Some(_) => Err(ApiError::bad_request(
            "অবস্থাটি draft, published বা archived হতে হবে — the status \
             filter must be draft, published or archived",
        )),
    }
}

/// Every store's posts for the moderation queue,
/// newest activity first, with an optional status
/// filter and a title/slug/store search.
pub async fn list_admin_posts(
    pool: &PgPool,
    status: Option<&str>,
    q: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminContentList<AdminPostRow>, ApiError> {
    let status = admin_status_filter(status)?;
    let needle = q.map(str::trim).filter(|value| !value.is_empty());
    let (page, limit) = crate::products::clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_posts p JOIN stores s ON s.id = p.store_id \
         WHERE ($1::text IS NULL OR p.status = $1) \
         AND ($2::text IS NULL OR p.title ILIKE '%' || $2 || '%' \
              OR p.slug ILIKE '%' || $2 || '%' OR s.name ILIKE '%' || $2 || '%')",
    )
    .bind(&status)
    .bind(needle)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin posts", e))?;
    let items: Vec<AdminPostRow> = sqlx::query_as(&format!(
        "SELECT {ADMIN_POST_COLUMNS} FROM store_posts p \
         JOIN stores s ON s.id = p.store_id \
         WHERE ($1::text IS NULL OR p.status = $1) \
         AND ($2::text IS NULL OR p.title ILIKE '%' || $2 || '%' \
              OR p.slug ILIKE '%' || $2 || '%' OR s.name ILIKE '%' || $2 || '%') \
         ORDER BY p.updated_at DESC, p.id ASC LIMIT $3 OFFSET $4"
    ))
    .bind(&status)
    .bind(needle)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin posts", e))?;
    Ok(AdminContentList {
        items,
        total,
        page,
        limit,
    })
}

/// Every store's pages for the moderation queue.
pub async fn list_admin_pages(
    pool: &PgPool,
    status: Option<&str>,
    q: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminContentList<AdminPageRow>, ApiError> {
    let status = admin_status_filter(status)?;
    let needle = q.map(str::trim).filter(|value| !value.is_empty());
    let (page, limit) = crate::products::clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_pages p JOIN stores s ON s.id = p.store_id \
         WHERE ($1::text IS NULL OR p.status = $1) \
         AND ($2::text IS NULL OR p.title ILIKE '%' || $2 || '%' \
              OR p.slug ILIKE '%' || $2 || '%' OR s.name ILIKE '%' || $2 || '%')",
    )
    .bind(&status)
    .bind(needle)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin pages", e))?;
    let items: Vec<AdminPageRow> = sqlx::query_as(&format!(
        "SELECT {ADMIN_PAGE_COLUMNS} FROM store_pages p \
         JOIN stores s ON s.id = p.store_id \
         WHERE ($1::text IS NULL OR p.status = $1) \
         AND ($2::text IS NULL OR p.title ILIKE '%' || $2 || '%' \
              OR p.slug ILIKE '%' || $2 || '%' OR s.name ILIKE '%' || $2 || '%') \
         ORDER BY p.updated_at DESC, p.id ASC LIMIT $3 OFFSET $4"
    ))
    .bind(&status)
    .bind(needle)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin pages", e))?;
    Ok(AdminContentList {
        items,
        total,
        page,
        limit,
    })
}

/// Load one post's admin row by id (any store).
async fn load_admin_post(pool: &PgPool, post_id: Uuid) -> Result<AdminPostRow, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {ADMIN_POST_COLUMNS} FROM store_posts p \
         JOIN stores s ON s.id = p.store_id WHERE p.id = $1"
    ))
    .bind(post_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load admin post", e))?
    .ok_or_else(|| ApiError::not_found("post not found"))
}

/// Load one page's admin row by id (any store).
async fn load_admin_page(pool: &PgPool, page_id: Uuid) -> Result<AdminPageRow, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {ADMIN_PAGE_COLUMNS} FROM store_pages p \
         JOIN stores s ON s.id = p.store_id WHERE p.id = $1"
    ))
    .bind(page_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load admin page", e))?
    .ok_or_else(|| ApiError::not_found("page not found"))
}

/// The admin moderation write for a post: the SAME
/// state machine and stamp rules as the seller's own
/// status action (a no-op is refused; publish stamps,
/// unpublish clears, archive keeps), plus the audit
/// row every admin action on this platform writes —
/// and, when the move lands the post live, the same
/// follower notice the seller's publish fires (the
/// post becoming live is the event, whoever moved it).
pub async fn set_admin_post_status(
    pool: &PgPool,
    admin_id: Uuid,
    post_id: Uuid,
    target: &str,
) -> Result<AdminPostRow, ApiError> {
    let target = target.trim();
    if !is_valid_content_status(target) {
        return Err(ApiError::bad_request(
            "অবস্থাটি draft, published বা archived হতে হবে — the status \
             must be draft, published or archived",
        ));
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin admin post status", e))?;
    let existing: PostRow = sqlx::query_as(&format!(
        "SELECT {POST_COLUMNS} FROM store_posts WHERE id = $1 FOR UPDATE"
    ))
    .bind(post_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("load post for moderation", e))?
    .ok_or_else(|| ApiError::not_found("post not found"))?;
    if existing.status == target {
        return Err(ApiError::bad_request(format!(
            "পোস্টটি ইতিমধ্যে '{target}' অবস্থায় আছে — the post is already \
             '{target}'"
        )));
    }
    if !can_transition_content_status(&existing.status, target) {
        return Err(ApiError::bad_request(format!(
            "'{}' থেকে '{target}' অবস্থায় যাওয়া যাবে না — cannot move \
             from '{}' to '{target}'",
            existing.status, existing.status
        )));
    }
    let published_at: Option<DateTime<Utc>> = if target == CONTENT_STATUS_PUBLISHED {
        Some(existing.published_at.unwrap_or_else(Utc::now))
    } else if target == domain::CONTENT_STATUS_DRAFT {
        None
    } else {
        existing.published_at
    };
    sqlx::query(
        "UPDATE store_posts SET status = $2, published_at = $3, updated_at = now() \
         WHERE id = $1",
    )
    .bind(post_id)
    .bind(target)
    .bind(published_at)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("moderate post status", e))?;
    if target == CONTENT_STATUS_PUBLISHED
        && content_is_live(
            target,
            published_at.map(|at| at.timestamp()),
            Utc::now().timestamp(),
        )
    {
        crate::notices::enqueue_store_new_post_notices(&mut tx, post_id).await?;
    }
    crate::audit::record_admin_action(
        &mut *tx,
        admin_id,
        "content_post_status",
        "store_post",
        Some(&post_id.to_string()),
        Some(serde_json::json!({
            "from": existing.status,
            "to": target,
            "store_id": existing.store_id.to_string(),
        })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit admin post status", e))?;
    load_admin_post(pool, post_id).await
}

/// The admin moderation write for a page: the same
/// state machine as the seller's action, plus the
/// audit row. Pages carry no stamps or notices.
pub async fn set_admin_page_status(
    pool: &PgPool,
    admin_id: Uuid,
    page_id: Uuid,
    target: &str,
) -> Result<AdminPageRow, ApiError> {
    let target = target.trim();
    if !is_valid_content_status(target) {
        return Err(ApiError::bad_request(
            "অবস্থাটি draft, published বা archived হতে হবে — the status \
             must be draft, published or archived",
        ));
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin admin page status", e))?;
    let existing: StorePage = sqlx::query_as(&format!(
        "SELECT {PAGE_COLUMNS} FROM store_pages WHERE id = $1 FOR UPDATE"
    ))
    .bind(page_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("load page for moderation", e))?
    .ok_or_else(|| ApiError::not_found("page not found"))?;
    if existing.status == target {
        return Err(ApiError::bad_request(format!(
            "পেজটি ইতিমধ্যে '{target}' অবস্থায় আছে — the page is already \
             '{target}'"
        )));
    }
    if !can_transition_content_status(&existing.status, target) {
        return Err(ApiError::bad_request(format!(
            "'{}' থেকে '{target}' অবস্থায় যাওয়া যাবে না — cannot move \
             from '{}' to '{target}'",
            existing.status, existing.status
        )));
    }
    sqlx::query("UPDATE store_pages SET status = $2, updated_at = now() WHERE id = $1")
        .bind(page_id)
        .bind(target)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("moderate page status", e))?;
    crate::audit::record_admin_action(
        &mut *tx,
        admin_id,
        "content_page_status",
        "store_page",
        Some(&page_id.to_string()),
        Some(serde_json::json!({
            "from": existing.status,
            "to": target,
            "store_id": existing.store_id.to_string(),
        })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit admin page status", e))?;
    load_admin_page(pool, page_id).await
}

/// One post row as the seller data export carries it:
/// the full stored content (it is the seller's own
/// writing — the export is their backup).
#[derive(Debug, Serialize, FromRow)]
pub struct ExportPostRow {
    pub id: Uuid,
    pub store_id: Uuid,
    pub slug: String,
    pub title: String,
    pub body: String,
    pub excerpt: Option<String>,
    pub tags: Vec<String>,
    pub cover_image_url: Option<String>,
    pub status: String,
    pub published_at: Option<DateTime<Utc>>,
    pub word_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The content half of the seller data export (Wave 31
/// surface, Wave 59 content): every page and post of
/// the exported stores, in stable order.
pub async fn export_content(
    pool: &PgPool,
    store_ids: &[Uuid],
) -> Result<(Vec<StorePage>, Vec<ExportPostRow>), ApiError> {
    let pages: Vec<StorePage> = sqlx::query_as(&format!(
        "SELECT {PAGE_COLUMNS} FROM store_pages WHERE store_id = ANY($1) \
         ORDER BY store_id ASC, position ASC, created_at ASC"
    ))
    .bind(store_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export store pages", e))?;
    let posts: Vec<ExportPostRow> = sqlx::query_as(&format!(
        "SELECT {POST_COLUMNS} FROM store_posts WHERE store_id = ANY($1) \
         ORDER BY store_id ASC, created_at ASC"
    ))
    .bind(store_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export store posts", e))?;
    Ok((pages, posts))
}

// ---------------------------------------------------------------------------
// Public reads
// ---------------------------------------------------------------------------

/// The public pages list: the store's published,
/// nav-flagged pages in the seller's order, plus the
/// live blog-post count the storefront frame needs.
pub async fn list_public_pages(pool: &PgPool, slug: &str) -> Result<PublicPageList, ApiError> {
    let store = public_store(pool, slug).await?;
    let items: Vec<PublicPageSummary> = sqlx::query_as(
        "SELECT slug, title FROM store_pages \
         WHERE store_id = $1 AND status = 'published' AND show_in_nav = true \
         ORDER BY position ASC, created_at ASC, id ASC",
    )
    .bind(store.id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list public pages", e))?;
    let blog_post_count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM store_posts WHERE store_id = $1 AND {LIVE_POST_WHERE}"
    ))
    .bind(store.id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count live posts", e))?;
    let total = items.len() as i64;
    Ok(PublicPageList {
        items,
        total,
        blog_post_count,
    })
}

/// One public page: published pages only — a draft, an
/// archived page and a missing slug are the same 404.
pub async fn get_public_page(
    pool: &PgPool,
    slug: &str,
    page_slug: &str,
) -> Result<StorePage, ApiError> {
    let store = public_store(pool, slug).await?;
    sqlx::query_as(&format!(
        "SELECT {PAGE_COLUMNS} FROM store_pages \
         WHERE store_id = $1 AND slug = $2 AND status = 'published'"
    ))
    .bind(store.id)
    .bind(page_slug)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load public page", e))?
    .ok_or_else(|| ApiError::not_found("page not found"))
}

/// Build the public view of one post row: the row plus
/// its store's public identity (slug + name), which the
/// cross-store discovery surfaces need and the per-store
/// surfaces simply repeat.
fn public_post(row: &PostRow, store_slug: &str, store_name: &str) -> PublicPost {
    PublicPost {
        id: row.id,
        store_id: row.store_id,
        store_slug: store_slug.to_string(),
        store_name: store_name.to_string(),
        slug: row.slug.clone(),
        title: row.title.clone(),
        excerpt: row.effective_excerpt(),
        tags: row.tags.clone(),
        cover_image_url: row.cover_image_url.clone(),
        published_at: row.published_at,
        reading_minutes: row.reading_minutes(),
    }
}

/// The public blog list of one store: live posts only,
/// newest first, optionally filtered to one tag (the
/// filter value passes the domain tag normaliser, so a
/// shopper's "Eid Collection" finds the stored
/// `eid-collection`; a value that cannot be a tag at
/// all simply matches nothing).
pub async fn list_public_posts(
    pool: &PgPool,
    slug: &str,
    tag: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<PostList, ApiError> {
    let store = public_store(pool, slug).await?;
    let (page, limit) = crate::products::clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let tag_filter: Option<String> = match tag {
        Some(raw) => match normalize_post_tags(&[raw.to_string()]) {
            Ok(mut tags) => tags.pop(),
            Err(_) => None,
        },
        None => None,
    };
    // An unnormalisable tag can match nothing; answer
    // with the honest empty list rather than an error
    // or (worse) the unfiltered list.
    if tag.is_some() && tag_filter.is_none() {
        return Ok(PostList {
            items: vec![],
            total: 0,
            page,
            limit,
        });
    }
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM store_posts \
         WHERE store_id = $1 AND {LIVE_POST_WHERE} \
         AND ($2::text IS NULL OR $2 = ANY(tags))"
    ))
    .bind(store.id)
    .bind(&tag_filter)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count public posts", e))?;
    let rows: Vec<PostRow> = sqlx::query_as(&format!(
        "SELECT {POST_COLUMNS} FROM store_posts \
         WHERE store_id = $1 AND {LIVE_POST_WHERE} \
         AND ($2::text IS NULL OR $2 = ANY(tags)) \
         ORDER BY published_at DESC, id ASC LIMIT $3 OFFSET $4"
    ))
    .bind(store.id)
    .bind(&tag_filter)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list public posts", e))?;
    Ok(PostList {
        items: rows
            .iter()
            .map(|row| public_post(row, &store.slug, &store.name))
            .collect(),
        total,
        page,
        limit,
    })
}

/// Load one LIVE post row of one store by slug, or the
/// uniform 404: the liveness check runs the domain
/// rule itself (status + schedule), so a draft, a
/// scheduled post, an archived post and a missing slug
/// are indistinguishable.
async fn load_live_post(
    pool: &PgPool,
    store: &Store,
    post_slug: &str,
) -> Result<PostRow, ApiError> {
    let row = load_post_row_by_slug(pool, store.id, post_slug).await?;
    let live = content_is_live(
        &row.status,
        row.published_at.map(|at| at.timestamp()),
        Utc::now().timestamp(),
    );
    if !live {
        return Err(ApiError::not_found("post not found"));
    }
    Ok(row)
}

/// One neighbour of a post in the blog's publish order.
async fn post_neighbour(
    pool: &PgPool,
    store: &Store,
    current: &PostRow,
    newer: bool,
) -> Result<Option<PostNavLink>, ApiError> {
    let published_at = current.published_at;
    let sql = if newer {
        "SELECT slug, title FROM store_posts \
         WHERE store_id = $1 AND status = 'published' \
         AND published_at IS NOT NULL AND published_at <= now() \
         AND (published_at, id) > ($2, $3) \
         ORDER BY published_at ASC, id ASC LIMIT 1"
    } else {
        "SELECT slug, title FROM store_posts \
         WHERE store_id = $1 AND status = 'published' \
         AND published_at IS NOT NULL AND published_at <= now() \
         AND (published_at, id) < ($2, $3) \
         ORDER BY published_at DESC, id DESC LIMIT 1"
    };
    sqlx::query_as(sql)
        .bind(store.id)
        .bind(published_at)
        .bind(current.id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load post neighbour", e))
}

/// The posts related to one post: same store, live,
/// sharing at least one tag, ranked by how many tags
/// they share (then newest). A post with no tags has
/// no related posts — the surface hides the rail
/// rather than filling it with unrelated reads.
async fn related_posts(
    pool: &PgPool,
    store: &Store,
    current: &PostRow,
) -> Result<Vec<PublicPost>, ApiError> {
    if current.tags.is_empty() {
        return Ok(vec![]);
    }
    let rows: Vec<PostRow> = sqlx::query_as(&format!(
        "SELECT {POST_COLUMNS} FROM store_posts \
         WHERE store_id = $1 AND {LIVE_POST_WHERE} AND id <> $3 \
         AND tags && $2 \
         ORDER BY (SELECT COUNT(*) FROM unnest(tags) AS t WHERE t = ANY($2)) DESC, \
                  published_at DESC, id ASC \
         LIMIT 4"
    ))
    .bind(store.id)
    .bind(&current.tags)
    .bind(current.id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load related posts", e))?;
    Ok(rows
        .iter()
        .map(|row| public_post(row, &store.slug, &store.name))
        .collect())
}

/// The public post page: the post, its prev/next
/// navigation, and its related posts.
pub async fn get_public_post(
    pool: &PgPool,
    slug: &str,
    post_slug: &str,
) -> Result<PublicPostDetail, ApiError> {
    let store = public_store(pool, slug).await?;
    let row = load_live_post(pool, &store, post_slug).await?;
    let previous = post_neighbour(pool, &store, &row, false).await?;
    let next = post_neighbour(pool, &store, &row, true).await?;
    let related = related_posts(pool, &store, &row).await?;
    Ok(PublicPostDetail {
        body: row.body.clone(),
        previous,
        next,
        related,
        post: public_post(&row, &store.slug, &store.name),
    })
}

/// Record one read of a live post (the anonymous view
/// beacon): the post must be public — a beacon for a
/// draft or a missing post is the same 404 as reading
/// it, and records nothing.
pub async fn record_post_view(
    pool: &PgPool,
    slug: &str,
    post_slug: &str,
) -> Result<(), ApiError> {
    let store = public_store(pool, slug).await?;
    let row = load_live_post(pool, &store, post_slug).await?;
    sqlx::query("INSERT INTO store_post_events (post_id, kind) VALUES ($1, 'post_view')")
        .bind(row.id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("record post view", e))?;
    Ok(())
}

/// One discovery row: a post row joined with its
/// store's public identity.
#[derive(Debug, FromRow)]
struct DiscoveryRow {
    #[sqlx(flatten)]
    post: PostRow,
    store_slug: String,
    store_name: String,
}

impl DiscoveryRow {
    fn into_public(self) -> PublicPost {
        public_post(&self.post, &self.store_slug, &self.store_name)
    }
}

const DISCOVERY_POST_COLUMNS: &str = "p.id, p.store_id, p.slug, p.title, p.body, \
    p.excerpt, p.tags, p.cover_image_url, p.status, p.published_at, p.word_count, \
    p.created_at, p.updated_at, s.slug AS store_slug, s.name AS store_name";

/// The platform blog discovery list: the newest live
/// posts across every ACTIVE store, newest first. The
/// marketplace's blog page renders this — content
/// discovery follows the same rule as product
/// discovery (what is public and live, newest first),
/// with no paid placement anywhere in the ranking.
pub async fn list_recent_posts(pool: &PgPool, limit: Option<i64>) -> Result<PostList, ApiError> {
    let limit = limit.unwrap_or(20).clamp(1, 50);
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM store_posts p JOIN stores s ON s.id = p.store_id \
         WHERE s.status = 'active' AND p.{LIVE_POST_WHERE}"
    ))
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count recent posts", e))?;
    let rows: Vec<DiscoveryRow> = sqlx::query_as(&format!(
        "SELECT {DISCOVERY_POST_COLUMNS} FROM store_posts p \
         JOIN stores s ON s.id = p.store_id \
         WHERE s.status = 'active' AND p.{LIVE_POST_WHERE} \
         ORDER BY p.published_at DESC, p.id ASC LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list recent posts", e))?;
    Ok(PostList {
        items: rows.into_iter().map(DiscoveryRow::into_public).collect(),
        total,
        page: 1,
        limit,
    })
}

/// The followed-stores blog list: the newest live posts
/// from the stores the CALLER follows (Wave 54's
/// follows), newest first, paginated — the reading
/// counterpart of the follows product feed.
pub async fn list_followed_posts(
    pool: &PgPool,
    user_id: Uuid,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<PostList, ApiError> {
    let (page, limit) = crate::products::clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM store_posts p \
         JOIN stores s ON s.id = p.store_id \
         JOIN store_follows f ON f.store_id = p.store_id AND f.user_id = $1 \
         WHERE s.status = 'active' AND p.{LIVE_POST_WHERE}"
    ))
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count followed posts", e))?;
    let rows: Vec<DiscoveryRow> = sqlx::query_as(&format!(
        "SELECT {DISCOVERY_POST_COLUMNS} FROM store_posts p \
         JOIN stores s ON s.id = p.store_id \
         JOIN store_follows f ON f.store_id = p.store_id AND f.user_id = $1 \
         WHERE s.status = 'active' AND p.{LIVE_POST_WHERE} \
         ORDER BY p.published_at DESC, p.id ASC LIMIT $2 OFFSET $3"
    ))
    .bind(user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list followed posts", e))?;
    Ok(PostList {
        items: rows.into_iter().map(DiscoveryRow::into_public).collect(),
        total,
        page,
        limit,
    })
}

/// Format one timestamp for the feed: RFC 3339 in UTC,
/// the Atom date shape.
fn feed_date(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Build the store blog's Atom feed: the newest
/// [`STORE_POST_FEED_ITEMS`] live posts, every
/// interpolation escaped through the domain's
/// `xml_escape`. Links are path-absolute (the platform
/// serves storefronts under `/{store}/blog/...` on
/// whatever host fronts it — the feed's ids are URNs,
/// stable across hosts; see the README's honest
/// limits).
pub async fn post_feed(pool: &PgPool, slug: &str) -> Result<String, ApiError> {
    let store = public_store(pool, slug).await?;
    let rows: Vec<PostRow> = sqlx::query_as(&format!(
        "SELECT {POST_COLUMNS} FROM store_posts \
         WHERE store_id = $1 AND {LIVE_POST_WHERE} \
         ORDER BY published_at DESC, id ASC LIMIT $2"
    ))
    .bind(store.id)
    .bind(STORE_POST_FEED_ITEMS)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load feed posts", e))?;
    let updated = rows
        .first()
        .and_then(|row| row.published_at)
        .unwrap_or(store.created_at);
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str("<feed xmlns=\"http://www.w3.org/2005/Atom\">\n");
    xml.push_str(&format!(
        "  <title>{} — ব্লগ</title>\n",
        xml_escape(&store.name)
    ));
    if let Some(description) = &store.description {
        xml.push_str(&format!(
            "  <subtitle>{}</subtitle>\n",
            xml_escape(description)
        ));
    }
    xml.push_str(&format!(
        "  <id>urn:bd-marketplace:store:{}:blog</id>\n",
        store.id
    ));
    xml.push_str(&format!(
        "  <link href=\"/{}/blog\"/>\n",
        xml_escape(&store.slug)
    ));
    xml.push_str(&format!("  <updated>{}</updated>\n", feed_date(updated)));
    for row in &rows {
        let published = row.published_at.unwrap_or(row.created_at);
        xml.push_str("  <entry>\n");
        xml.push_str(&format!("    <title>{}</title>\n", xml_escape(&row.title)));
        xml.push_str(&format!(
            "    <id>urn:bd-marketplace:post:{}</id>\n",
            row.id
        ));
        xml.push_str(&format!(
            "    <link href=\"/{}/blog/{}\"/>\n",
            xml_escape(&store.slug),
            xml_escape(&row.slug)
        ));
        xml.push_str(&format!(
            "    <published>{}</published>\n",
            feed_date(published)
        ));
        xml.push_str(&format!(
            "    <updated>{}</updated>\n",
            feed_date(row.updated_at)
        ));
        xml.push_str(&format!(
            "    <summary>{}</summary>\n",
            xml_escape(&row.effective_excerpt())
        ));
        for tag in &row.tags {
            xml.push_str(&format!(
                "    <category term=\"{}\"/>\n",
                xml_escape(tag)
            ));
        }
        xml.push_str("  </entry>\n");
    }
    xml.push_str("</feed>\n");
    Ok(xml)
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn list_seller_pages_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<Vec<StorePage>>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    Ok(Json(list_seller_pages(pool, store.id).await?))
}

pub async fn create_page_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Json(input): Json<PageInput>,
) -> Result<(StatusCode, Json<StorePage>), ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    let page = create_page(pool, &store, &input).await?;
    Ok((StatusCode::CREATED, Json(page)))
}

pub async fn get_seller_page_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Path(page_id): Path<Uuid>,
) -> Result<Json<StorePage>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    Ok(Json(load_seller_page(pool, store.id, page_id).await?))
}

pub async fn update_page_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Path(page_id): Path<Uuid>,
    Json(input): Json<PageInput>,
) -> Result<Json<StorePage>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    Ok(Json(update_page(pool, &store, page_id, &input).await?))
}

pub async fn delete_page_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Path(page_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    delete_page(pool, &store, page_id).await?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

pub async fn reorder_pages_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Json(input): Json<ReorderPagesInput>,
) -> Result<Json<Vec<StorePage>>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    Ok(Json(reorder_pages(pool, &store, &input).await?))
}

pub async fn set_page_status_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Path(page_id): Path<Uuid>,
    Json(input): Json<PageStatusInput>,
) -> Result<Json<StorePage>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    Ok(Json(set_page_status(pool, &store, page_id, &input).await?))
}

#[derive(Deserialize)]
pub struct SellerPostListQuery {
    pub store_id: Option<Uuid>,
    pub status: Option<String>,
    pub q: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

impl SellerPostListQuery {
    fn scope(&self) -> StoreScopeQuery {
        StoreScopeQuery {
            store_id: self.store_id,
        }
    }
}

pub async fn list_seller_posts_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<SellerPostListQuery>,
) -> Result<Json<SellerPostList>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &query.scope()).await?;
    Ok(Json(
        list_seller_posts(
            pool,
            &store,
            query.status.as_deref(),
            query.q.as_deref(),
            query.page,
            query.limit,
        )
        .await?,
    ))
}

pub async fn create_post_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Json(input): Json<PostInput>,
) -> Result<(StatusCode, Json<SellerPostDetail>), ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    let post = create_post(pool, &store, &input).await?;
    Ok((StatusCode::CREATED, Json(post)))
}

pub async fn get_seller_post_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Path(post_id): Path<Uuid>,
) -> Result<Json<SellerPostDetail>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    let row = load_post_row(pool, store.id, post_id).await?;
    let views = post_view_count(pool, post_id).await?;
    Ok(Json(seller_detail(&row, views)))
}

pub async fn update_post_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Path(post_id): Path<Uuid>,
    Json(input): Json<PostInput>,
) -> Result<Json<SellerPostDetail>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    Ok(Json(update_post(pool, &store, post_id, &input).await?))
}

pub async fn delete_post_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Path(post_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = resolve_content_store(state.require_pool()?, &user, &scope).await?;
    delete_post(&state, &store, post_id).await?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

pub async fn set_post_status_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Path(post_id): Path<Uuid>,
    Json(input): Json<PostStatusInput>,
) -> Result<Json<SellerPostDetail>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    Ok(Json(set_post_status(pool, &store, post_id, &input).await?))
}

pub async fn upload_post_cover_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Path(post_id): Path<Uuid>,
    multipart: Multipart,
) -> Result<Json<SellerPostDetail>, ApiError> {
    let form = product_images::read_upload_form(multipart).await?;
    let store = resolve_content_store(state.require_pool()?, &user, &scope).await?;
    Ok(Json(
        upload_post_cover(
            &state,
            &store,
            post_id,
            &form.bytes,
            form.content_type.as_deref(),
        )
        .await?,
    ))
}

pub async fn delete_post_cover_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Path(post_id): Path<Uuid>,
) -> Result<Json<SellerPostDetail>, ApiError> {
    let store = resolve_content_store(state.require_pool()?, &user, &scope).await?;
    Ok(Json(delete_post_cover(&state, &store, post_id).await?))
}

pub async fn seller_post_stats_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<PostStats>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_content_store(pool, &user, &scope).await?;
    Ok(Json(seller_post_stats(pool, &store).await?))
}

pub async fn list_public_pages_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<PublicPageList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(list_public_pages(pool, &slug).await?))
}

pub async fn get_public_page_handler(
    State(state): State<AppState>,
    Path((slug, page_slug)): Path<(String, String)>,
) -> Result<Json<StorePage>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_public_page(pool, &slug, &page_slug).await?))
}

#[derive(Deserialize)]
pub struct PublicPostListQuery {
    pub tag: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn list_public_posts_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<PublicPostListQuery>,
) -> Result<Json<PostList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_public_posts(pool, &slug, query.tag.as_deref(), query.page, query.limit).await?,
    ))
}

pub async fn get_public_post_handler(
    State(state): State<AppState>,
    Path((slug, post_slug)): Path<(String, String)>,
) -> Result<Json<PublicPostDetail>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_public_post(pool, &slug, &post_slug).await?))
}

pub async fn record_post_view_handler(
    State(state): State<AppState>,
    Path((slug, post_slug)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pool = state.require_pool()?;
    record_post_view(pool, &slug, &post_slug).await?;
    Ok(Json(serde_json::json!({ "recorded": true })))
}

pub async fn post_feed_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<(HeaderMap, String), ApiError> {
    let pool = state.require_pool()?;
    let xml = post_feed(pool, &slug).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/atom+xml; charset=utf-8"),
    );
    Ok((headers, xml))
}

#[derive(Deserialize)]
pub struct RecentPostsQuery {
    pub limit: Option<i64>,
}

pub async fn list_recent_posts_handler(
    State(state): State<AppState>,
    Query(query): Query<RecentPostsQuery>,
) -> Result<Json<PostList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(list_recent_posts(pool, query.limit).await?))
}

#[derive(Deserialize)]
pub struct FollowedPostsQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn list_followed_posts_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<FollowedPostsQuery>,
) -> Result<Json<PostList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_followed_posts(pool, user.user_id, query.page, query.limit).await?,
    ))
}

#[derive(Deserialize)]
pub struct AdminContentQuery {
    pub status: Option<String>,
    pub q: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct AdminStatusInput {
    pub status: String,
}

pub async fn list_admin_posts_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminContentQuery>,
) -> Result<Json<AdminContentList<AdminPostRow>>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_admin_posts(
            pool,
            query.status.as_deref(),
            query.q.as_deref(),
            query.page,
            query.limit,
        )
        .await?,
    ))
}

pub async fn list_admin_pages_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminContentQuery>,
) -> Result<Json<AdminContentList<AdminPageRow>>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_admin_pages(
            pool,
            query.status.as_deref(),
            query.q.as_deref(),
            query.page,
            query.limit,
        )
        .await?,
    ))
}

pub async fn set_admin_post_status_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(post_id): Path<Uuid>,
    Json(input): Json<AdminStatusInput>,
) -> Result<Json<AdminPostRow>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        set_admin_post_status(pool, user.user_id, post_id, &input.status).await?,
    ))
}

pub async fn set_admin_page_status_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(page_id): Path<Uuid>,
    Json(input): Json<AdminStatusInput>,
) -> Result<Json<AdminPageRow>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        set_admin_page_status(pool, user.user_id, page_id, &input.status).await?,
    ))
}
