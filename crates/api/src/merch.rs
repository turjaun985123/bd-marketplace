//! Merchandising (Wave 52): home banners, curated collections,
//! flash sales, the home feed that assembles them, and the
//! append-only merchandising event counters.
//!
//! The storefront-parity program's data layer. Until this wave
//! the platform had no merchandising objects at all — the home
//! page could only list products — and no product price could
//! ever differ from the product's base price. This module adds
//! the three admin-curated objects (migrations 0044–0046) and
//! the one public aggregate the home page renders from.
//!
//! FLASH PRICES ARE MONEY. A flash sale item's price is not a
//! display sticker: it is the price the cart shows, the price
//! checkout snapshots into the order, and therefore the price
//! commission, seller payout, coupon allocation and COD
//! collection all derive from (they already derive from the
//! snapshot — this module makes the snapshot carry the sale
//! price). The resolution lives here, in exactly one place:
//!
//! - [`active_flash_price`] / [`active_flash_price_tx`]: the
//!   price for one product at one instant, pool and
//!   in-transaction forms. Checkout calls the tx form with the
//!   checkout transaction's own clock reading, AFTER the
//!   product rows are locked, so a sale ending mid-checkout
//!   resolves at the price in force when the order is placed
//!   and concurrent checkouts of the same product serialise
//!   on the product-row lock before either reads the cap.
//! - [`attach_flash_prices`]: the batch form that fills the
//!   shared product view's `flash_price_poisha` /
//!   `flash_ends_at` fields for lists and rails.
//!
//! Both run the same three facts through the domain rule
//! `resolve_flash_price`: the live sale (active flag +
//! half-open window — at most one sale can be live at an
//! instant, enforced at write time), the item's sale price,
//! and the units already sold at that price. "Sold at the
//! sale price" is ONE aggregate ([`FLASH_UNITS_SOLD_SQL`]):
//! order items in non-cancelled sub-orders, placed inside the
//! sale window, whose snapshot unit price equals the price
//! the resolution produced for that line — the sale price
//! itself, plus the variant's price delta for variant lines
//! (the flash price replaces the product's BASE price; a
//! variant's delta still applies on top, exactly as it does
//! over the base price). When the count reaches the item's
//! `stock_cap`, the flash price stops resolving and the
//! product sells at base everywhere. The definition is
//! deliberate and auditable: it can be recomputed from the
//! order ledger at any time, and the README states it.
//!
//! SOLD COUNTS follow the same philosophy (the aggregate
//! at the tail of `products::PRODUCT_COLUMNS`): units in
//! DELIVERED sub-orders, nothing else — a placed order is
//! not a sale.
//!
//! Admin writes sit behind the one `require_admin` guard
//! (and API keys are refused on the whole `/v1/admin/*`
//! prefix by the Wave 37 route exclusions, so merchandising
//! is JWT-admin only). Validation is the domain crate's;
//! handlers stay thin.

use std::collections::HashMap;

use axum::{
    extract::{Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgConnection, PgPool, Transaction};
use uuid::Uuid;

use crate::admin::require_admin;
use crate::auth::AuthUser;
use crate::media;
use crate::product_images;
use crate::products::{
    clamp_pagination, public_cache_headers, with_stock_status, Product, PRODUCT_COLUMNS,
    PRODUCT_RATING_JOIN,
};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Flash price resolution — the money-critical core
// ---------------------------------------------------------------------------

/// A resolved flash price: the sale it came from, the price
/// itself, and when the sale ends (the views' countdown).
#[derive(Debug, Clone, Copy)]
pub struct ResolvedFlashPrice {
    pub sale_id: Uuid,
    pub sale_price_poisha: i64,
    pub ends_at: DateTime<Utc>,
}

/// The live sale at an instant (at most one row can match —
/// overlapping active windows are refused at write time; the
/// ORDER BY keeps the answer deterministic even against rows
/// written outside the API).
#[derive(Debug, FromRow)]
struct LiveSaleRow {
    id: Uuid,
    ends_at: DateTime<Utc>,
}

/// One flash sale item's stored facts.
#[derive(Debug, FromRow)]
struct FlashItemRow {
    product_id: Uuid,
    sale_price_poisha: i64,
    stock_cap: Option<i32>,
}

/// THE cap-accounting aggregate (see the module doc for the
/// definition): units sold at the sale price, per product,
/// for one sale. $1 = sale id, $2 = product ids to count.
const FLASH_UNITS_SOLD_SQL: &str = "SELECT oi.product_id, \
    COALESCE(SUM(oi.quantity), 0)::bigint AS sold \
    FROM order_items oi \
    JOIN sub_orders so ON so.id = oi.sub_order_id \
    JOIN orders o ON o.id = so.order_id \
    JOIN flash_sale_items fsi \
      ON fsi.flash_sale_id = $1 AND fsi.product_id = oi.product_id \
    JOIN flash_sales fs ON fs.id = fsi.flash_sale_id \
    WHERE oi.product_id = ANY($2) AND so.status <> 'cancelled' \
      AND o.created_at >= fs.starts_at AND o.created_at < fs.ends_at \
      AND oi.unit_price_poisha = fsi.sale_price_poisha + COALESCE( \
          (SELECT pv.price_delta_poisha FROM product_variants pv \
            WHERE pv.id = oi.variant_id), 0) \
    GROUP BY oi.product_id";

/// The SQL twin of the flash resolution (Wave 53): a
/// lateral join that computes, for the product aliased `p`,
/// the flash price the Rust resolution
/// ([`resolve_flash_on`]) would produce at the database
/// clock — the live sale picked by the same rule
/// ([`live_sale_on`]: active, half-open window, earliest
/// ending first), the item's sale price, withheld when the
/// item's cap is already consumed by units sold at the sale
/// price (the [`FLASH_UNITS_SOLD_SQL`] aggregate in
/// per-product scalar form: non-cancelled sub-orders,
/// orders placed inside the window, snapshot price equal
/// to the sale price plus the variant delta).
///
/// Discovery queries (search/browse, the product list's
/// price sorts) filter and order by THIS expression, so a
/// facet or a price sort can never disagree with the
/// `flash_price_poisha` the Rust attach fills on the very
/// same rows. The alias is `fx`; the effective price is
/// [`EFFECTIVE_PRICE_SQL`]. The join carries no binds —
/// it reads the database clock, like the `is_promoted`
/// column in the shared product SELECT.
pub(crate) const FLASH_PRICE_JOIN: &str = "LEFT JOIN LATERAL (\
    SELECT fsi.sale_price_poisha AS flash_price_poisha \
    FROM (SELECT id, starts_at, ends_at, created_at FROM flash_sales \
          WHERE is_active AND starts_at <= now() AND now() < ends_at \
          ORDER BY ends_at ASC, created_at ASC LIMIT 1) fs \
    JOIN flash_sale_items fsi \
      ON fsi.flash_sale_id = fs.id AND fsi.product_id = p.id \
    WHERE (fsi.stock_cap IS NULL OR (\
          SELECT COALESCE(SUM(oi.quantity), 0)::bigint \
          FROM order_items oi \
          JOIN sub_orders so ON so.id = oi.sub_order_id \
          JOIN orders o ON o.id = so.order_id \
          WHERE oi.product_id = p.id AND so.status <> 'cancelled' \
            AND o.created_at >= fs.starts_at AND o.created_at < fs.ends_at \
            AND oi.unit_price_poisha = fsi.sale_price_poisha + COALESCE((\
                SELECT pv.price_delta_poisha FROM product_variants pv \
                WHERE pv.id = oi.variant_id), 0)) < fsi.stock_cap) \
    ORDER BY fs.ends_at ASC, fs.created_at ASC LIMIT 1) fx ON true";

/// The price a buyer pays for the product aliased `p`
/// under the [`FLASH_PRICE_JOIN`]: the resolved flash
/// price when one exists, else the base price. Every
/// price filter, price sort and price facet in the
/// discovery queries names this expression — never
/// `p.price_poisha` alone.
pub(crate) const EFFECTIVE_PRICE_SQL: &str = "COALESCE(fx.flash_price_poisha, p.price_poisha)";

/// The sale live at `now`, if any.
async fn live_sale_on(
    conn: &mut PgConnection,
    now: DateTime<Utc>,
) -> Result<Option<LiveSaleRow>, ApiError> {
    sqlx::query_as(
        "SELECT id, ends_at FROM flash_sales \
         WHERE is_active AND starts_at <= $1 AND $1 < ends_at \
         ORDER BY ends_at ASC, created_at ASC LIMIT 1",
    )
    .bind(now)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("find live flash sale", e))
}

/// The items of one sale among the given products.
async fn flash_items_on(
    conn: &mut PgConnection,
    sale_id: Uuid,
    product_ids: &[Uuid],
) -> Result<Vec<FlashItemRow>, ApiError> {
    sqlx::query_as(
        "SELECT product_id, sale_price_poisha, stock_cap FROM flash_sale_items \
         WHERE flash_sale_id = $1 AND product_id = ANY($2)",
    )
    .bind(sale_id)
    .bind(product_ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load flash sale items", e))
}

/// Units sold at the sale price, per product (one aggregate —
/// see [`FLASH_UNITS_SOLD_SQL`]). Products with no matching
/// order items are absent from the map (their count is 0).
pub async fn flash_units_sold_on(
    conn: &mut PgConnection,
    sale_id: Uuid,
    product_ids: &[Uuid],
) -> Result<HashMap<Uuid, i64>, ApiError> {
    if product_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, i64)> = sqlx::query_as(FLASH_UNITS_SOLD_SQL)
        .bind(sale_id)
        .bind(product_ids)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| ApiError::internal("count units sold at sale price", e))?;
    Ok(rows.into_iter().collect())
}

/// The pool form of [`flash_units_sold_on`] — the feed and
/// the admin item views read cap consumption through here.
pub async fn flash_units_sold(
    pool: &PgPool,
    sale_id: Uuid,
    product_ids: &[Uuid],
) -> Result<HashMap<Uuid, i64>, ApiError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| ApiError::internal("acquire connection for flash units", e))?;
    flash_units_sold_on(&mut conn, sale_id, product_ids).await
}

/// Resolve flash prices for a set of products at `now` over
/// one connection: the shared core of every public form
/// below. Only products whose price actually resolves (live
/// sale, item present, cap not exhausted) appear in the map.
async fn resolve_flash_on(
    conn: &mut PgConnection,
    product_ids: &[Uuid],
    now: DateTime<Utc>,
) -> Result<HashMap<Uuid, ResolvedFlashPrice>, ApiError> {
    if product_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let Some(sale) = live_sale_on(conn, now).await? else {
        return Ok(HashMap::new());
    };
    let items = flash_items_on(conn, sale.id, product_ids).await?;
    if items.is_empty() {
        return Ok(HashMap::new());
    }
    // The cap aggregate only has work to do for capped items.
    let capped_ids: Vec<Uuid> = items
        .iter()
        .filter(|item| item.stock_cap.is_some())
        .map(|item| item.product_id)
        .collect();
    let sold = flash_units_sold_on(conn, sale.id, &capped_ids).await?;
    let mut resolved = HashMap::new();
    for item in items {
        let sold_at_sale = sold.get(&item.product_id).copied().unwrap_or(0);
        if let Some(price) = domain::resolve_flash_price(
            item.sale_price_poisha,
            item.stock_cap.map(i64::from),
            sold_at_sale,
        ) {
            resolved.insert(
                item.product_id,
                ResolvedFlashPrice {
                    sale_id: sale.id,
                    sale_price_poisha: price,
                    ends_at: sale.ends_at,
                },
            );
        }
    }
    Ok(resolved)
}

/// THE single-product flash price resolution (pool form):
/// the price a buyer pays for this product right now, when a
/// flash sale governs it — `None` when the product sells at
/// its base price (no live sale, not an item, or the item's
/// cap is exhausted).
pub async fn active_flash_price(
    pool: &PgPool,
    product_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<ResolvedFlashPrice>, ApiError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| ApiError::internal("acquire connection for flash price", e))?;
    let mut resolved = resolve_flash_on(&mut conn, &[product_id], now).await?;
    Ok(resolved.remove(&product_id))
}

/// THE single-product flash price resolution (transaction
/// form): the form checkout calls, inside its own transaction
/// and under its product-row locks, so the cap it reads is
/// the cap as of this order's placement.
pub async fn active_flash_price_tx(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    product_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<ResolvedFlashPrice>, ApiError> {
    let mut resolved = resolve_flash_on(tx, &[product_id], now).await?;
    Ok(resolved.remove(&product_id))
}

/// The batch resolution as a plain map — the form the cart
/// uses (cart lines are not product views): product id →
/// resolved flash price, for every product in the set whose
/// price currently resolves.
pub async fn flash_price_map(
    pool: &PgPool,
    product_ids: &[Uuid],
    now: DateTime<Utc>,
) -> Result<HashMap<Uuid, ResolvedFlashPrice>, ApiError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| ApiError::internal("acquire connection for flash price map", e))?;
    resolve_flash_on(&mut conn, product_ids, now).await
}

/// Fill the flash fields of a batch of product views (the
/// batch form of the resolution): each product in a live
/// sale with cap headroom gains `flash_price_poisha` +
/// `flash_ends_at`; every other product keeps the nulls it
/// loaded with. One connection, three queries, any batch
/// size — never a per-product loop.
pub async fn attach_flash_prices(
    pool: &PgPool,
    products: &mut [Product],
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    let mut refs: Vec<&mut Product> = products.iter_mut().collect();
    attach_flash_prices_refs(pool, &mut refs, now).await
}

/// The reference-slice form of [`attach_flash_prices`] — for
/// views that wrap a `Product` (also-bought items, flash
/// cards) and cannot hand over a contiguous slice.
pub async fn attach_flash_prices_refs(
    pool: &PgPool,
    products: &mut [&mut Product],
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    if products.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = products.iter().map(|product| product.id).collect();
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| ApiError::internal("acquire connection for flash prices", e))?;
    let resolved = resolve_flash_on(&mut conn, &ids, now).await?;
    for product in products.iter_mut() {
        if let Some(flash) = resolved.get(&product.id) {
            product.flash_price_poisha = Some(flash.sale_price_poisha);
            product.flash_ends_at = Some(flash.ends_at);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Home banners (admin)
// ---------------------------------------------------------------------------

/// One home banner as the API serves it: the stored row plus
/// the computed `image_url` (the media module's URL rule over
/// whichever image source the banner carries — an uploaded
/// object under `site/banners/<id>` or an external hotlink).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct HomeBanner {
    pub id: Uuid,
    pub title: String,
    pub subtitle: Option<String>,
    pub link_url: Option<String>,
    /// The uploaded image's base key; never serialized (it
    /// reaches clients as `image_url`).
    #[sqlx(default)]
    #[serde(skip)]
    pub storage_key: Option<String>,
    /// The uploaded image's original width; never serialized.
    #[sqlx(default)]
    #[serde(skip)]
    pub storage_width: Option<i32>,
    /// The hotlinked image URL when the banner hotlinks;
    /// never serialized (it reaches clients as `image_url`).
    #[sqlx(default)]
    #[serde(skip)]
    pub external_image_url: Option<String>,
    /// The display URL: the sized upload URL or the hotlink,
    /// verbatim; null for a text-only banner.
    #[sqlx(default)]
    pub image_url: Option<String>,
    pub position: i32,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const BANNER_COLUMNS: &str = "id, title, subtitle, link_url, storage_key, \
    width AS storage_width, external_image_url, position, starts_at, ends_at, \
    is_active, created_at, updated_at";

/// Fill a banner's `image_url` from its stored image facts:
/// the hotlink verbatim, else the upload's 1200px display URL
/// (hero slots are wide — the largest derived size the media
/// pipeline plans), else null.
fn with_banner_image(mut banner: HomeBanner) -> HomeBanner {
    banner.image_url = match banner.external_image_url.as_deref() {
        Some(url) => Some(url.to_string()),
        None => banner
            .storage_key
            .as_deref()
            .map(|key| media::sized_url(key, banner.storage_width, 1200)),
    };
    banner
}

/// One page of banners plus the total count.
#[derive(Debug, Serialize)]
pub struct BannerList {
    pub items: Vec<HomeBanner>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// Every banner on the platform, in carousel order (admin
/// view: windows and the active flag are shown, not applied).
pub async fn list_banners(
    pool: &PgPool,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<BannerList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM home_banners")
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count banners", e))?;
    let items: Vec<HomeBanner> = sqlx::query_as(&format!(
        "SELECT {BANNER_COLUMNS} FROM home_banners \
         ORDER BY position ASC, created_at ASC LIMIT $1 OFFSET $2"
    ))
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list banners", e))?
    .into_iter()
    .map(with_banner_image)
    .collect();
    Ok(BannerList {
        items,
        total,
        page,
        limit,
    })
}

/// Load one banner by id (404 when absent).
pub async fn get_banner(pool: &PgPool, id: Uuid) -> Result<HomeBanner, ApiError> {
    let banner = sqlx::query_as(&format!(
        "SELECT {BANNER_COLUMNS} FROM home_banners WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load banner", e))?
    .ok_or_else(|| ApiError::not_found("banner not found"))?;
    Ok(with_banner_image(banner))
}

/// The validated, storable form of a banner write (create or
/// merged update) — domain validation happens once, here, so
/// the create and update paths can never disagree.
struct BannerFields {
    title: String,
    subtitle: Option<String>,
    link_url: Option<String>,
    external_image_url: Option<String>,
    position: i32,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    is_active: bool,
}

/// The raw, unvalidated banner field set a write carries
/// (create: the request itself; update: the request merged
/// over the stored row). Bundled so validation takes the set
/// as one argument.
struct BannerFieldInput<'a> {
    title: &'a str,
    subtitle: Option<&'a str>,
    link_url: Option<&'a str>,
    external_image_url: Option<&'a str>,
    position: i32,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
    is_active: bool,
}

fn validate_banner_fields(input: BannerFieldInput<'_>) -> Result<BannerFields, ApiError> {
    let title = domain::validate_merch_title(input.title).map_err(ApiError::bad_request)?;
    let subtitle =
        domain::validate_merch_subtitle(input.subtitle, domain::MAX_BANNER_SUBTITLE_CHARS)
            .map_err(ApiError::bad_request)?;
    let link_url = match input.link_url {
        Some(raw) if !raw.trim().is_empty() => {
            Some(domain::validate_banner_link(raw).map_err(ApiError::bad_request)?)
        }
        _ => None,
    };
    let external_image_url = match input.external_image_url {
        Some(raw) if !raw.trim().is_empty() => Some(
            domain::validate_external_image_url(raw).map_err(ApiError::bad_request)?,
        ),
        _ => None,
    };
    domain::validate_optional_window(
        input.starts_at.map(|t| t.timestamp()),
        input.ends_at.map(|t| t.timestamp()),
    )
    .map_err(ApiError::bad_request)?;
    Ok(BannerFields {
        title,
        subtitle,
        link_url,
        external_image_url,
        position: input.position,
        starts_at: input.starts_at,
        ends_at: input.ends_at,
        is_active: input.is_active,
    })
}

/// Create a banner. A banner created WITH an external image
/// URL starts hotlinked; one created without any image is a
/// text slot until the upload endpoint gives it one (an
/// upload clears the hotlink — one image source, always).
pub async fn create_banner(pool: &PgPool, input: CreateBannerInput) -> Result<HomeBanner, ApiError> {
    let fields = validate_banner_fields(BannerFieldInput {
        title: &input.title,
        subtitle: input.subtitle.as_deref(),
        link_url: input.link_url.as_deref(),
        external_image_url: input.external_image_url.as_deref(),
        position: input.position.unwrap_or(0),
        starts_at: input.starts_at,
        ends_at: input.ends_at,
        is_active: input.is_active.unwrap_or(true),
    })?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO home_banners (title, subtitle, link_url, external_image_url, \
         position, starts_at, ends_at, is_active) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(&fields.title)
    .bind(&fields.subtitle)
    .bind(&fields.link_url)
    .bind(&fields.external_image_url)
    .bind(fields.position)
    .bind(fields.starts_at)
    .bind(fields.ends_at)
    .bind(fields.is_active)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("create banner", e))?;
    get_banner(pool, id).await
}

/// Update a banner: provided fields override, absent fields
/// keep their stored values, and the MERGED row must pass
/// the same validation as a create (a patch that moves only
/// `ends_at` still cannot invert the window). Text fields
/// clear by sending an empty string (validation normalises
/// blank to null); a stored window bound cannot be cleared
/// through the patch — the bound fields carry no
/// present-but-null distinction.
pub async fn update_banner(
    state: &AppState,
    id: Uuid,
    patch: UpdateBannerInput,
) -> Result<HomeBanner, ApiError> {
    let pool = state.require_pool()?;
    let existing = get_banner(pool, id).await?;
    let fields = validate_banner_fields(BannerFieldInput {
        title: patch.title.as_deref().unwrap_or(&existing.title),
        subtitle: patch.subtitle.as_deref().or(existing.subtitle.as_deref()),
        link_url: patch.link_url.as_deref().or(existing.link_url.as_deref()),
        external_image_url: patch
            .external_image_url
            .as_deref()
            .or(existing.external_image_url.as_deref()),
        position: patch.position.unwrap_or(existing.position),
        starts_at: patch.starts_at.or(existing.starts_at),
        ends_at: patch.ends_at.or(existing.ends_at),
        is_active: patch.is_active.unwrap_or(existing.is_active),
    })?;
    // Setting a hotlink replaces an upload as the image
    // source (the CHECK allows only one); the uploaded
    // objects are deleted with the source switch so a
    // replaced upload never lingers unreferenced.
    let switching_to_external = fields.external_image_url.is_some()
        && existing.external_image_url.as_deref() != fields.external_image_url.as_deref();
    sqlx::query(
        "UPDATE home_banners SET title = $2, subtitle = $3, link_url = $4, \
         external_image_url = $5, \
         storage_key = CASE WHEN $6 THEN NULL ELSE storage_key END, \
         width = CASE WHEN $6 THEN NULL ELSE width END, \
         height = CASE WHEN $6 THEN NULL ELSE height END, \
         position = $7, starts_at = $8, ends_at = $9, is_active = $10, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&fields.title)
    .bind(&fields.subtitle)
    .bind(&fields.link_url)
    .bind(&fields.external_image_url)
    .bind(switching_to_external)
    .bind(fields.position)
    .bind(fields.starts_at)
    .bind(fields.ends_at)
    .bind(fields.is_active)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("update banner", e))?;
    if switching_to_external {
        if let Some(key) = &existing.storage_key {
            media::delete_stored_image(state.media_store.as_ref(), key, existing.storage_width)
                .await;
        }
    }
    get_banner(pool, id).await
}

/// Delete a banner and its uploaded objects (a hotlink has
/// nothing to delete — the URL was never ours).
pub async fn delete_banner(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    let pool = state.require_pool()?;
    let existing = get_banner(pool, id).await?;
    sqlx::query("DELETE FROM home_banners WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("delete banner", e))?;
    if let Some(key) = &existing.storage_key {
        media::delete_stored_image(state.media_store.as_ref(), key, existing.storage_width).await;
    }
    Ok(())
}

/// Upload a banner's image through the Wave 51 pipeline
/// (sniff → decode → EXIF → derived sizes), under the
/// deterministic base key `site/banners/<id>` — a re-upload
/// replaces the same objects (the old set is deleted first,
/// so a smaller re-upload cannot leave a stale larger
/// derived size behind). The upload becomes the banner's one
/// image source: any hotlink is cleared.
pub async fn upload_banner_image(
    state: &AppState,
    id: Uuid,
    bytes: &[u8],
    declared_content_type: Option<&str>,
) -> Result<HomeBanner, ApiError> {
    let pool = state.require_pool()?;
    let existing = get_banner(pool, id).await?;
    let base_key = format!("site/banners/{id}");
    let processed = media::process_upload(&base_key, bytes, declared_content_type)?;
    if existing.storage_key.is_some() {
        media::delete_stored_image(state.media_store.as_ref(), &base_key, existing.storage_width)
            .await;
    }
    media::store_processed(state.media_store.as_ref(), &processed).await?;
    let width = i32::try_from(processed.width)
        .map_err(|_| ApiError::internal_any("banner image width", "width out of range"))?;
    let height = i32::try_from(processed.height)
        .map_err(|_| ApiError::internal_any("banner image height", "height out of range"))?;
    sqlx::query(
        "UPDATE home_banners SET storage_key = $2, width = $3, height = $4, \
         external_image_url = NULL, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&base_key)
    .bind(width)
    .bind(height)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("store banner image", e))?;
    get_banner(pool, id).await
}

/// Remove a banner's UPLOADED image (objects + row fields).
/// A hotlinked banner has no upload to remove — clearing a
/// hotlink is an update (PATCH with an empty
/// `external_image_url`, which validation normalises to
/// null), so this is a 400 there, not a silent no-op.
pub async fn delete_banner_image(state: &AppState, id: Uuid) -> Result<HomeBanner, ApiError> {
    let pool = state.require_pool()?;
    let existing = get_banner(pool, id).await?;
    let Some(key) = existing.storage_key.clone() else {
        return Err(ApiError::bad_request(
            "this banner has no uploaded image to remove — \
             এই ব্যানারে আপলোড করা কোনো ছবি নেই",
        ));
    };
    sqlx::query(
        "UPDATE home_banners SET storage_key = NULL, width = NULL, height = NULL, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("clear banner image", e))?;
    media::delete_stored_image(state.media_store.as_ref(), &key, existing.storage_width).await;
    get_banner(pool, id).await
}

// Banner request shapes

#[derive(Deserialize)]
pub struct CreateBannerInput {
    pub title: String,
    pub subtitle: Option<String>,
    pub link_url: Option<String>,
    pub external_image_url: Option<String>,
    pub position: Option<i32>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub is_active: Option<bool>,
}

#[derive(Deserialize)]
pub struct UpdateBannerInput {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub link_url: Option<String>,
    pub external_image_url: Option<String>,
    pub position: Option<i32>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub is_active: Option<bool>,
}

#[derive(Deserialize)]
pub struct MerchListQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

// Banner handlers (thin; every one behind the admin guard)

pub async fn list_banners_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<MerchListQuery>,
) -> Result<Json<BannerList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(list_banners(pool, query.page, query.limit).await?))
}

pub async fn create_banner_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<CreateBannerInput>,
) -> Result<(StatusCode, Json<HomeBanner>), ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let banner = create_banner(pool, input).await?;
    Ok((StatusCode::CREATED, Json(banner)))
}

pub async fn update_banner_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(patch): Json<UpdateBannerInput>,
) -> Result<Json<HomeBanner>, ApiError> {
    require_admin(&state, &user).await?;
    Ok(Json(update_banner(&state, id, patch).await?))
}

pub async fn delete_banner_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state, &user).await?;
    delete_banner(&state, id).await?;
    Ok(Json(serde_json::json!({ "deleted": true, "id": id })))
}

pub async fn upload_banner_image_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    multipart: Multipart,
) -> Result<Json<HomeBanner>, ApiError> {
    require_admin(&state, &user).await?;
    let form = product_images::read_upload_form(multipart).await?;
    let banner = upload_banner_image(&state, id, &form.bytes, form.content_type.as_deref()).await?;
    Ok(Json(banner))
}

pub async fn delete_banner_image_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<HomeBanner>, ApiError> {
    require_admin(&state, &user).await?;
    Ok(Json(delete_banner_image(&state, id).await?))
}

// ---------------------------------------------------------------------------
// Collections (admin + public landing data)
// ---------------------------------------------------------------------------

/// One curated collection (the membership lives in
/// `collection_products`; the views that carry products are
/// [`CollectionDetail`] and the home feed's sections).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Collection {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub is_active: bool,
    pub position: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLLECTION_COLUMNS: &str = "id, slug, title, subtitle, is_active, position, \
    created_at, updated_at";

/// A collection plus its products, in the admin's chosen
/// membership order.
#[derive(Debug, Serialize)]
pub struct CollectionDetail {
    #[serde(flatten)]
    pub collection: Collection,
    pub products: Vec<Product>,
}

/// One page of collections plus the total count.
#[derive(Debug, Serialize)]
pub struct CollectionList {
    pub items: Vec<Collection>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// Is this database error a unique-violation (SQLSTATE
/// 23505)? The slug and membership uniqueness rules surface
/// through it.
fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(|db| db.code())
        .is_some_and(|code| code == "23505")
}

/// Every collection on the platform, in home-page order
/// (admin view: inactive collections included).
pub async fn list_collections(
    pool: &PgPool,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<CollectionList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM collections")
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count collections", e))?;
    let items: Vec<Collection> = sqlx::query_as(&format!(
        "SELECT {COLLECTION_COLUMNS} FROM collections \
         ORDER BY position ASC, created_at ASC LIMIT $1 OFFSET $2"
    ))
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list collections", e))?;
    Ok(CollectionList {
        items,
        total,
        page,
        limit,
    })
}

/// Load one collection by id (404 when absent).
pub async fn get_collection(pool: &PgPool, id: Uuid) -> Result<Collection, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {COLLECTION_COLUMNS} FROM collections WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load collection", e))?
    .ok_or_else(|| ApiError::not_found("collection not found"))
}

/// Load one collection by slug (404 when absent).
pub async fn get_collection_by_slug(pool: &PgPool, slug: &str) -> Result<Collection, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {COLLECTION_COLUMNS} FROM collections WHERE slug = $1"
    ))
    .bind(slug)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load collection by slug", e))?
    .ok_or_else(|| ApiError::not_found("collection not found"))
}

/// A collection's products in membership order. `public_only`
/// applies the storefront visibility rule (active product in
/// an active store); the admin form passes false so the
/// picker shows exactly what was picked, drafts included.
pub async fn load_collection_products(
    pool: &PgPool,
    collection_id: Uuid,
    public_only: bool,
    limit: Option<i64>,
) -> Result<Vec<Product>, ApiError> {
    let visibility = if public_only {
        "AND p.status = 'active' AND s.status = 'active'"
    } else {
        ""
    };
    let limit = limit.unwrap_or(200).clamp(1, 200);
    let mut products: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM collection_products cp \
         JOIN products p ON p.id = cp.product_id \
         JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE cp.collection_id = $1 {visibility} \
         ORDER BY cp.position ASC, cp.product_id ASC LIMIT $2"
    ))
    .bind(collection_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load collection products", e))?
    .into_iter()
    .map(with_stock_status)
    .collect();
    attach_flash_prices(pool, &mut products, Utc::now()).await?;
    Ok(products)
}

/// The full detail view of one collection (admin visibility
/// for the products — see [`load_collection_products`]).
pub async fn collection_detail(
    pool: &PgPool,
    collection: Collection,
) -> Result<CollectionDetail, ApiError> {
    let products = load_collection_products(pool, collection.id, false, None).await?;
    Ok(CollectionDetail {
        collection,
        products,
    })
}

/// The PUBLIC landing view of one collection, by slug: an
/// inactive collection is a 404 (it is not a page the
/// platform currently publishes), and the products are the
/// publicly visible ones in membership order.
pub async fn public_collection_by_slug(
    pool: &PgPool,
    slug: &str,
) -> Result<CollectionDetail, ApiError> {
    let collection = get_collection_by_slug(pool, slug).await?;
    if !collection.is_active {
        return Err(ApiError::not_found("collection not found"));
    }
    let products = load_collection_products(pool, collection.id, true, None).await?;
    Ok(CollectionDetail {
        collection,
        products,
    })
}

/// Create a collection. The slug is unique across the
/// platform (it is a URL namespace) — a collision is a 409.
pub async fn create_collection(
    pool: &PgPool,
    input: CreateCollectionInput,
) -> Result<Collection, ApiError> {
    let slug = domain::validate_collection_slug(input.slug.trim())
        .map_err(ApiError::bad_request)?
        .to_string();
    let title = domain::validate_merch_title(&input.title).map_err(ApiError::bad_request)?;
    let subtitle =
        domain::validate_merch_subtitle(input.subtitle.as_deref(), domain::MAX_COLLECTION_SUBTITLE_CHARS)
            .map_err(ApiError::bad_request)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO collections (slug, title, subtitle, is_active, position) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(&slug)
    .bind(&title)
    .bind(&subtitle)
    .bind(input.is_active.unwrap_or(true))
    .bind(input.position.unwrap_or(0))
    .fetch_one(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict(format!(
                "a collection with the slug '{slug}' already exists — \
                 '{slug}' স্লাগের একটি কালেকশন ইতিমধ্যে আছে"
            ))
        } else {
            ApiError::internal("create collection", e)
        }
    })?;
    get_collection(pool, id).await
}

/// Update a collection: provided fields override, absent
/// fields keep their stored values, the merged row passes
/// create's validation, and a slug change into another
/// collection's slug is the same 409 as a colliding create.
pub async fn update_collection(
    pool: &PgPool,
    id: Uuid,
    patch: UpdateCollectionInput,
) -> Result<Collection, ApiError> {
    let existing = get_collection(pool, id).await?;
    let slug = match patch.slug.as_deref() {
        Some(raw) => domain::validate_collection_slug(raw.trim())
            .map_err(ApiError::bad_request)?
            .to_string(),
        None => existing.slug.clone(),
    };
    let title = match patch.title.as_deref() {
        Some(raw) => domain::validate_merch_title(raw).map_err(ApiError::bad_request)?,
        None => existing.title.clone(),
    };
    let subtitle = match patch.subtitle.as_deref() {
        Some(raw) => domain::validate_merch_subtitle(
            Some(raw),
            domain::MAX_COLLECTION_SUBTITLE_CHARS,
        )
        .map_err(ApiError::bad_request)?,
        None => existing.subtitle.clone(),
    };
    sqlx::query(
        "UPDATE collections SET slug = $2, title = $3, subtitle = $4, \
         is_active = $5, position = $6, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&slug)
    .bind(&title)
    .bind(&subtitle)
    .bind(patch.is_active.unwrap_or(existing.is_active))
    .bind(patch.position.unwrap_or(existing.position))
    .execute(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict(format!(
                "a collection with the slug '{slug}' already exists — \
                 '{slug}' স্লাগের একটি কালেকশন ইতিমধ্যে আছে"
            ))
        } else {
            ApiError::internal("update collection", e)
        }
    })?;
    get_collection(pool, id).await
}

/// Delete a collection (its membership rows cascade).
pub async fn delete_collection(pool: &PgPool, id: Uuid) -> Result<(), ApiError> {
    let existing = get_collection(pool, id).await?;
    sqlx::query("DELETE FROM collections WHERE id = $1")
        .bind(existing.id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("delete collection", e))?;
    Ok(())
}

/// Replace a collection's membership with the given ordered
/// product list, in ONE transaction: the list IS the order
/// (position = index). Every named product must exist (a
/// picker typo must not silently drop a slot), and a product
/// named twice is a 400 — the membership's primary key would
/// refuse it anyway, but the admin deserves the reason, not
/// a constraint name. An empty list clears the collection.
pub async fn set_collection_products(
    pool: &PgPool,
    collection_id: Uuid,
    product_ids: Vec<Uuid>,
) -> Result<CollectionDetail, ApiError> {
    let collection = get_collection(pool, collection_id).await?;
    let mut seen = std::collections::HashSet::new();
    for product_id in &product_ids {
        if !seen.insert(*product_id) {
            return Err(ApiError::bad_request(format!(
                "product {product_id} appears twice in the list — \
                 পণ্যটি তালিকায় দুবার আছে"
            )));
        }
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin collection membership transaction", e))?;
    for product_id in &product_ids {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM products WHERE id = $1)")
                .bind(product_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| ApiError::internal("check collection product exists", e))?;
        if !exists {
            return Err(ApiError::bad_request(format!(
                "product {product_id} does not exist — পণ্যটি পাওয়া যায়নি"
            )));
        }
    }
    sqlx::query("DELETE FROM collection_products WHERE collection_id = $1")
        .bind(collection_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("clear collection membership", e))?;
    for (position, product_id) in product_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO collection_products (collection_id, product_id, position) \
             VALUES ($1, $2, $3)",
        )
        .bind(collection_id)
        .bind(product_id)
        .bind(i32::try_from(position).unwrap_or(i32::MAX))
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("insert collection membership", e))?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit collection membership", e))?;
    collection_detail(pool, collection).await
}

// Collection request shapes

#[derive(Deserialize)]
pub struct CreateCollectionInput {
    pub slug: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub is_active: Option<bool>,
    pub position: Option<i32>,
}

#[derive(Deserialize)]
pub struct UpdateCollectionInput {
    pub slug: Option<String>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub is_active: Option<bool>,
    pub position: Option<i32>,
}

#[derive(Deserialize)]
pub struct SetCollectionProductsInput {
    pub product_ids: Vec<Uuid>,
}

// Collection handlers (thin; admin writes behind the guard)

pub async fn list_collections_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<MerchListQuery>,
) -> Result<Json<CollectionList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(list_collections(pool, query.page, query.limit).await?))
}

pub async fn create_collection_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<CreateCollectionInput>,
) -> Result<(StatusCode, Json<Collection>), ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let collection = create_collection(pool, input).await?;
    Ok((StatusCode::CREATED, Json(collection)))
}

pub async fn get_collection_admin_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<CollectionDetail>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let collection = get_collection(pool, id).await?;
    Ok(Json(collection_detail(pool, collection).await?))
}

pub async fn update_collection_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(patch): Json<UpdateCollectionInput>,
) -> Result<Json<Collection>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(update_collection(pool, id, patch).await?))
}

pub async fn delete_collection_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    delete_collection(pool, id).await?;
    Ok(Json(serde_json::json!({ "deleted": true, "id": id })))
}

pub async fn set_collection_products_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<SetCollectionProductsInput>,
) -> Result<Json<CollectionDetail>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        set_collection_products(pool, id, input.product_ids).await?,
    ))
}

/// The public collection landing data (see
/// [`public_collection_by_slug`]).
pub async fn get_collection_public_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<(HeaderMap, Json<CollectionDetail>), ApiError> {
    let pool = state.require_pool()?;
    let detail = public_collection_by_slug(pool, &slug).await?;
    Ok((public_cache_headers(true), Json(detail)))
}

// ---------------------------------------------------------------------------
// Flash sales (admin)
// ---------------------------------------------------------------------------

/// One flash sale as the admin lists serve it, with its item
/// count folded in.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct FlashSale {
    pub id: Uuid,
    pub title: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub is_active: bool,
    /// How many products the sale carries (0 until items are
    /// set — a sale with no items is a window, not an event).
    #[sqlx(default)]
    pub item_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const FLASH_SALE_COLUMNS: &str = "fs.id, fs.title, fs.starts_at, fs.ends_at, fs.is_active, \
    (SELECT COUNT(*)::bigint FROM flash_sale_items fsi WHERE fsi.flash_sale_id = fs.id) \
        AS item_count, \
    fs.created_at, fs.updated_at";

/// One page of flash sales plus the total count.
#[derive(Debug, Serialize)]
pub struct FlashSaleList {
    pub items: Vec<FlashSale>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// One flash sale item as the admin editor shows it: the
/// product's identity and CURRENT base price next to the
/// sale terms, plus the live cap accounting (units already
/// sold at the sale price, and what remains of the cap).
#[derive(Debug, Serialize)]
pub struct FlashSaleItemView {
    pub product_id: Uuid,
    pub product_title: String,
    pub base_price_poisha: i64,
    pub sale_price_poisha: i64,
    pub stock_cap: Option<i32>,
    pub sold_at_sale: i64,
    /// Units still sellable at the sale price; null when the
    /// item is uncapped.
    pub cap_remaining: Option<i64>,
}

/// A flash sale plus its item views.
#[derive(Debug, Serialize)]
pub struct FlashSaleDetail {
    #[serde(flatten)]
    pub sale: FlashSale,
    pub items: Vec<FlashSaleItemView>,
}

/// Every flash sale on the platform, newest window first.
pub async fn list_flash_sales(
    pool: &PgPool,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<FlashSaleList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM flash_sales")
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count flash sales", e))?;
    let items: Vec<FlashSale> = sqlx::query_as(&format!(
        "SELECT {FLASH_SALE_COLUMNS} FROM flash_sales fs \
         ORDER BY fs.starts_at DESC, fs.created_at DESC LIMIT $1 OFFSET $2"
    ))
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list flash sales", e))?;
    Ok(FlashSaleList {
        items,
        total,
        page,
        limit,
    })
}

/// Load one flash sale by id (404 when absent).
pub async fn get_flash_sale(pool: &PgPool, id: Uuid) -> Result<FlashSale, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {FLASH_SALE_COLUMNS} FROM flash_sales fs WHERE fs.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load flash sale", e))?
    .ok_or_else(|| ApiError::not_found("flash sale not found"))
}

/// A flash sale's full detail: the sale plus every item with
/// its product facts and cap accounting.
pub async fn flash_sale_detail(pool: &PgPool, id: Uuid) -> Result<FlashSaleDetail, ApiError> {
    let sale = get_flash_sale(pool, id).await?;
    let rows: Vec<(Uuid, String, i64, i64, Option<i32>)> = sqlx::query_as(
        "SELECT fsi.product_id, p.title, p.price_poisha, fsi.sale_price_poisha, fsi.stock_cap \
         FROM flash_sale_items fsi JOIN products p ON p.id = fsi.product_id \
         WHERE fsi.flash_sale_id = $1 \
         ORDER BY (p.price_poisha - fsi.sale_price_poisha) DESC, p.title ASC",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load flash sale item views", e))?;
    let product_ids: Vec<Uuid> = rows.iter().map(|row| row.0).collect();
    let sold = flash_units_sold(pool, id, &product_ids).await?;
    let items = rows
        .into_iter()
        .map(|(product_id, product_title, base_price, sale_price, stock_cap)| {
            let sold_at_sale = sold.get(&product_id).copied().unwrap_or(0);
            FlashSaleItemView {
                product_id,
                product_title,
                base_price_poisha: base_price,
                sale_price_poisha: sale_price,
                stock_cap,
                sold_at_sale,
                cap_remaining: stock_cap.map(|cap| (i64::from(cap) - sold_at_sale).max(0)),
            }
        })
        .collect();
    Ok(FlashSaleDetail { sale, items })
}

/// The "at most one live flash sale" gate, run inside the
/// caller's transaction: refuse (409, bilingual) when another
/// ACTIVE sale's window overlaps the candidate window.
/// Windows that merely touch do not overlap (the domain's
/// half-open rule), so back-to-back sales schedule cleanly.
async fn refuse_overlapping_flash_sale(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    exclude_id: Option<Uuid>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
) -> Result<(), ApiError> {
    let overlapping: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM flash_sales \
         WHERE is_active AND ($3::uuid IS NULL OR id <> $3) \
           AND starts_at < $2 AND $1 < ends_at",
    )
    .bind(starts_at)
    .bind(ends_at)
    .bind(exclude_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| ApiError::internal("check flash sale overlap", e))?;
    if overlapping > 0 {
        return Err(ApiError::conflict(
            "another active flash sale already overlaps this window — \
             only one flash sale can run at a time — \
             এই সময়ে আরেকটি সক্রিয় ফ্ল্যাশ সেল চলছে, একসঙ্গে একটির বেশি ফ্ল্যাশ সেল চলতে পারে না",
        ));
    }
    Ok(())
}

/// Create a flash sale. The window is domain-validated, and
/// an ACTIVE sale whose window overlaps another active sale
/// is refused inside the same transaction that inserts it,
/// so the one-live-sale rule holds for the write itself.
pub async fn create_flash_sale(
    pool: &PgPool,
    input: CreateFlashSaleInput,
) -> Result<FlashSale, ApiError> {
    let title = domain::validate_merch_title(&input.title).map_err(ApiError::bad_request)?;
    domain::validate_flash_window(input.starts_at.timestamp(), input.ends_at.timestamp())
        .map_err(ApiError::bad_request)?;
    let is_active = input.is_active.unwrap_or(true);
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin flash sale transaction", e))?;
    if is_active {
        refuse_overlapping_flash_sale(&mut tx, None, input.starts_at, input.ends_at).await?;
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO flash_sales (title, starts_at, ends_at, is_active) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(&title)
    .bind(input.starts_at)
    .bind(input.ends_at)
    .bind(is_active)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("create flash sale", e))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit flash sale", e))?;
    get_flash_sale(pool, id).await
}

/// Update a flash sale: provided fields override, the merged
/// window is re-validated, and re-activating (or moving) a
/// sale into another active sale's window is refused with
/// the sale itself excluded from the overlap check.
pub async fn update_flash_sale(
    pool: &PgPool,
    id: Uuid,
    patch: UpdateFlashSaleInput,
) -> Result<FlashSale, ApiError> {
    let existing = get_flash_sale(pool, id).await?;
    let title = match patch.title.as_deref() {
        Some(raw) => domain::validate_merch_title(raw).map_err(ApiError::bad_request)?,
        None => existing.title.clone(),
    };
    let starts_at = patch.starts_at.unwrap_or(existing.starts_at);
    let ends_at = patch.ends_at.unwrap_or(existing.ends_at);
    let is_active = patch.is_active.unwrap_or(existing.is_active);
    domain::validate_flash_window(starts_at.timestamp(), ends_at.timestamp())
        .map_err(ApiError::bad_request)?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin flash sale update transaction", e))?;
    if is_active {
        refuse_overlapping_flash_sale(&mut tx, Some(id), starts_at, ends_at).await?;
    }
    sqlx::query(
        "UPDATE flash_sales SET title = $2, starts_at = $3, ends_at = $4, \
         is_active = $5, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&title)
    .bind(starts_at)
    .bind(ends_at)
    .bind(is_active)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("update flash sale", e))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit flash sale update", e))?;
    get_flash_sale(pool, id).await
}

/// Delete a flash sale (its items cascade). Deleting the
/// live sale ends it immediately — resolution simply finds
/// no sale from the next request on.
pub async fn delete_flash_sale(pool: &PgPool, id: Uuid) -> Result<(), ApiError> {
    let existing = get_flash_sale(pool, id).await?;
    sqlx::query("DELETE FROM flash_sales WHERE id = $1")
        .bind(existing.id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("delete flash sale", e))?;
    Ok(())
}

/// Replace a flash sale's items, in ONE transaction. Every
/// item is validated against its product's CURRENT base
/// price (the domain rule: a genuine reduction only) and its
/// cap against the domain cap rule; a product named twice,
/// a missing product, or a non-reduction price fails the
/// whole replace with the reason — a half-applied item list
/// would be a price event nobody approved.
pub async fn set_flash_sale_items(
    pool: &PgPool,
    sale_id: Uuid,
    items: Vec<FlashSaleItemInput>,
) -> Result<FlashSaleDetail, ApiError> {
    let sale = get_flash_sale(pool, sale_id).await?;
    let mut seen = std::collections::HashSet::new();
    for item in &items {
        if !seen.insert(item.product_id) {
            return Err(ApiError::bad_request(format!(
                "product {} appears twice in the item list — \
                 পণ্যটি তালিকায় দুবার আছে",
                item.product_id
            )));
        }
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin flash items transaction", e))?;
    // Validate everything before writing anything.
    let mut validated: Vec<(Uuid, i64, Option<i32>)> = Vec::with_capacity(items.len());
    for item in &items {
        let base_price: Option<i64> =
            sqlx::query_scalar("SELECT price_poisha FROM products WHERE id = $1")
                .bind(item.product_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| ApiError::internal("load product for flash item", e))?;
        let Some(base_price) = base_price else {
            return Err(ApiError::bad_request(format!(
                "product {} does not exist — পণ্যটি পাওয়া যায়নি",
                item.product_id
            )));
        };
        let sale_price = domain::validate_flash_sale_price(item.sale_price_poisha, base_price)
            .map_err(|reason| {
                ApiError::bad_request(format!(
                    "{reason} (product {}, base price {base_price} poisha) — \
                     বিক্রয়মূল্য পণ্যের বর্তমান দামের চেয়ে কম হতে হবে",
                    item.product_id
                ))
            })?;
        let cap = domain::validate_flash_stock_cap(item.stock_cap).map_err(ApiError::bad_request)?;
        let cap = match cap {
            Some(value) => Some(
                i32::try_from(value).map_err(|_| {
                    ApiError::bad_request(
                        "the stock cap is too large — স্টক ক্যাপটি অতিরিক্ত বড়",
                    )
                })?,
            ),
            None => None,
        };
        validated.push((item.product_id, sale_price, cap));
    }
    // Wave 54 extension: remember the outgoing item set —
    // the follower alert below fires only for products
    // that are NEW to the sale, so re-saving the same set
    // (or repricing an existing item) never re-alerts.
    let previous_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT product_id FROM flash_sale_items WHERE flash_sale_id = $1")
            .bind(sale_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("load outgoing flash sale items", e))?;
    sqlx::query("DELETE FROM flash_sale_items WHERE flash_sale_id = $1")
        .bind(sale_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("clear flash sale items", e))?;
    for (product_id, sale_price, cap) in &validated {
        sqlx::query(
            "INSERT INTO flash_sale_items (flash_sale_id, product_id, sale_price_poisha, stock_cap) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(sale_id)
        .bind(product_id)
        .bind(sale_price)
        .bind(cap)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("insert flash sale item", e))?;
    }
    // Wave 54 extension: when the sale is LIVE right now,
    // followers of the newly-added products' stores get
    // the `store_flash_sale` notice inside this same
    // transaction. A sale scheduled for later alerts
    // nobody at item-set time — the honest limit,
    // documented in the README.
    let now = chrono::Utc::now();
    if sale.is_active && sale.starts_at <= now && now < sale.ends_at {
        let new_ids: Vec<Uuid> = validated
            .iter()
            .map(|(product_id, _, _)| *product_id)
            .filter(|product_id| !previous_ids.contains(product_id))
            .collect();
        if !new_ids.is_empty() {
            crate::notices::enqueue_store_flash_sale_notices(
                &mut tx,
                sale_id,
                &sale.title,
                &new_ids,
            )
            .await?;
        }
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit flash sale items", e))?;
    flash_sale_detail(pool, sale_id).await
}

// Flash sale request shapes

#[derive(Deserialize)]
pub struct CreateFlashSaleInput {
    pub title: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub is_active: Option<bool>,
}

#[derive(Deserialize)]
pub struct UpdateFlashSaleInput {
    pub title: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub is_active: Option<bool>,
}

#[derive(Deserialize)]
pub struct FlashSaleItemInput {
    pub product_id: Uuid,
    pub sale_price_poisha: i64,
    pub stock_cap: Option<i64>,
}

#[derive(Deserialize)]
pub struct SetFlashSaleItemsInput {
    pub items: Vec<FlashSaleItemInput>,
}

// Flash sale handlers (thin; every one behind the admin guard)

pub async fn list_flash_sales_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<MerchListQuery>,
) -> Result<Json<FlashSaleList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(list_flash_sales(pool, query.page, query.limit).await?))
}

pub async fn get_flash_sale_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<FlashSaleDetail>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(flash_sale_detail(pool, id).await?))
}

pub async fn create_flash_sale_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<CreateFlashSaleInput>,
) -> Result<(StatusCode, Json<FlashSale>), ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let sale = create_flash_sale(pool, input).await?;
    Ok((StatusCode::CREATED, Json(sale)))
}

pub async fn update_flash_sale_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(patch): Json<UpdateFlashSaleInput>,
) -> Result<Json<FlashSale>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(update_flash_sale(pool, id, patch).await?))
}

pub async fn delete_flash_sale_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    delete_flash_sale(pool, id).await?;
    Ok(Json(serde_json::json!({ "deleted": true, "id": id })))
}

pub async fn set_flash_sale_items_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<SetFlashSaleItemsInput>,
) -> Result<Json<FlashSaleDetail>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(set_flash_sale_items(pool, id, input.items).await?))
}

// ---------------------------------------------------------------------------
// The home feed
// ---------------------------------------------------------------------------

/// Section size caps for the home feed — named constants so
/// the feed's shape is a decision in one place, not a scatter
/// of magic numbers across queries.
pub const HOME_BANNER_LIMIT: i64 = 10;
/// See [`HOME_BANNER_LIMIT`].
pub const HOME_CATEGORY_LIMIT: i64 = 12;
/// See [`HOME_BANNER_LIMIT`].
pub const HOME_FLASH_ITEM_LIMIT: i64 = 12;
/// See [`HOME_BANNER_LIMIT`].
pub const HOME_COLLECTION_LIMIT: i64 = 4;
/// See [`HOME_BANNER_LIMIT`].
pub const HOME_COLLECTION_PRODUCT_LIMIT: i64 = 8;
/// See [`HOME_BANNER_LIMIT`].
pub const HOME_RAIL_LIMIT: i64 = 10;
/// See [`HOME_BANNER_LIMIT`].
pub const HOME_FEATURED_STORE_LIMIT: i64 = 8;

/// One category tile: the category label (categories are
/// still free-text product labels — there is no categories
/// table), how many active products carry it, and a cover
/// image: the primary image of the category's best-selling
/// product that has one (the sold-count definition again —
/// the aggregate at the tail of
/// [`crate::products::PRODUCT_COLUMNS`]).
#[derive(Debug, Serialize)]
pub struct HomeCategory {
    pub category: String,
    pub product_count: i64,
    pub image_url: Option<String>,
}

/// The category aggregate's raw row (image facts folded into
/// `image_url` by [`load_categories`]).
#[derive(Debug, FromRow)]
struct HomeCategoryRow {
    category: String,
    product_count: i64,
    storage_key: Option<String>,
    external_url: Option<String>,
    width: Option<i32>,
}

/// One flash sale card in the feed: the shared product view
/// (its `flash_price_poisha` already cap-checked by the
/// attach) plus the sale terms the section renders — the
/// listed sale price, the cap, and the units already sold at
/// the sale price (the "sold X of Y" bar's numerator).
#[derive(Debug, Serialize)]
pub struct HomeFlashItem {
    #[serde(flatten)]
    pub product: Product,
    pub sale_price_poisha: i64,
    pub stock_cap: Option<i32>,
    pub sold_at_sale: i64,
}

/// The feed's flash sale block: the live sale and its cards,
/// biggest discount first. Null in the feed when no sale is
/// live at the feed's instant.
#[derive(Debug, Serialize)]
pub struct HomeFlashSale {
    pub id: Uuid,
    pub title: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub items: Vec<HomeFlashItem>,
}

/// One collection section of the feed: the collection plus
/// its first products in membership order.
#[derive(Debug, Serialize)]
pub struct HomeCollectionSection {
    #[serde(flatten)]
    pub collection: Collection,
    pub products: Vec<Product>,
}

/// One featured store tile: a VERIFIED store (basic or
/// verified tier — an unverified store is never spotlighted)
/// with its Wave 18 branding and its active product count.
#[derive(Debug, Serialize, FromRow)]
pub struct FeaturedStore {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub logo_url: Option<String>,
    pub banner_url: Option<String>,
    pub brand_color: Option<String>,
    pub verified_tier: String,
    pub product_count: i64,
}

/// THE home feed: everything the marketplace home page
/// renders, in one response, assembled from the same shared
/// product materialisation as every other surface. Sections
/// with nothing honest to show are empty (or null, for the
/// flash sale) and the page hides them.
#[derive(Debug, Serialize)]
pub struct HomeFeed {
    pub banners: Vec<HomeBanner>,
    pub categories: Vec<HomeCategory>,
    pub flash_sale: Option<HomeFlashSale>,
    pub collections: Vec<HomeCollectionSection>,
    pub new_arrivals: Vec<Product>,
    pub top_rated: Vec<Product>,
    pub best_sellers: Vec<Product>,
    pub promoted: Vec<Product>,
    pub featured_stores: Vec<FeaturedStore>,
}

/// Load one no-bind rail of public products through the
/// shared materialisation and attach flash prices. The SQL
/// is a complete SELECT over the shared columns; callers
/// supply their ordering (and any WHERE) as part of it.
async fn load_rail(pool: &PgPool, sql: &str, now: DateTime<Utc>) -> Result<Vec<Product>, ApiError> {
    let mut products: Vec<Product> = sqlx::query_as(sql)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("load home rail", e))?
        .into_iter()
        .map(with_stock_status)
        .collect();
    attach_flash_prices(pool, &mut products, now).await?;
    Ok(products)
}

/// The category aggregate (Wave 52, extracted in Wave 53):
/// counts over active products in active stores, each
/// tile's cover from the category's best-selling product
/// that has an image (newest with an image when nothing
/// has sold — the lateral's sold-count order falls through
/// to recency). Sorted by count descending, label
/// ascending, capped at `limit`.
///
/// This is THE category list: the home feed's tiles and
/// the public `GET /v1/categories` both read it, so the
/// two surfaces can never publish different vocabularies
/// or counts.
pub async fn load_categories(pool: &PgPool, limit: i64) -> Result<Vec<HomeCategory>, ApiError> {
    let category_rows: Vec<HomeCategoryRow> = sqlx::query_as(
        "SELECT cat.category, cat.product_count, \
                img.storage_key, img.external_url, img.width \
         FROM ( \
             SELECT p.category AS category, COUNT(*)::bigint AS product_count \
             FROM products p JOIN stores s ON s.id = p.store_id \
             WHERE p.status = 'active' AND s.status = 'active' \
               AND p.category IS NOT NULL AND btrim(p.category) <> '' \
             GROUP BY p.category \
         ) cat \
         LEFT JOIN LATERAL ( \
             SELECT pi.storage_key, pi.external_url, pi.width \
             FROM products p2 \
             JOIN stores s2 ON s2.id = p2.store_id \
             JOIN product_images pi ON pi.product_id = p2.id \
             WHERE p2.category = cat.category \
               AND p2.status = 'active' AND s2.status = 'active' \
             ORDER BY (SELECT COALESCE(SUM(oi.quantity), 0)::bigint \
                       FROM order_items oi \
                       JOIN sub_orders so ON so.id = oi.sub_order_id \
                       WHERE oi.product_id = p2.id AND so.status = 'delivered') DESC, \
                      p2.created_at DESC \
             LIMIT 1 \
         ) img ON true \
         ORDER BY cat.product_count DESC, cat.category ASC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load categories", e))?;
    Ok(category_rows
        .into_iter()
        .map(|row| HomeCategory {
            image_url: media::product_image_url(
                row.storage_key.as_deref(),
                row.external_url.as_deref(),
                row.width,
            ),
            category: row.category,
            product_count: row.product_count,
        })
        .collect())
}

/// Assemble the home feed as of `now`. The instant is a
/// parameter — not read from the clock inside — so the admin
/// preview (`GET /v1/admin/home-preview?at=`) renders exactly
/// what the public feed will show at a scheduled instant,
/// through this same builder: banner visibility, the live
/// sale and the rails' flash prices all resolve against
/// `now`. (Collection sections attach flash prices through
/// the shared loader, which reads the real clock — a preview
/// nuance confined to collection cards' sale badges. Sold
/// counts and ratings are ledger facts, not functions of the
/// instant; the promoted flag inside the shared product
/// columns reads the database clock, as it does on every
/// surface.)
pub async fn build_home_feed(pool: &PgPool, now: DateTime<Utc>) -> Result<HomeFeed, ApiError> {
    // Banners visible at `now`: the domain's
    // `banner_is_visible` rule in SQL form.
    let banners: Vec<HomeBanner> = sqlx::query_as(&format!(
        "SELECT {BANNER_COLUMNS} FROM home_banners \
         WHERE is_active AND (starts_at IS NULL OR starts_at <= $1) \
           AND (ends_at IS NULL OR $1 < ends_at) \
         ORDER BY position ASC, created_at ASC LIMIT $2"
    ))
    .bind(now)
    .bind(HOME_BANNER_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load home banners", e))?
    .into_iter()
    .map(with_banner_image)
    .collect();

    // Category tiles: the shared aggregate (Wave 53
    // extracted it for `GET /v1/categories`).
    let categories = load_categories(pool, HOME_CATEGORY_LIMIT).await?;

    // The flash sale live at `now`, with its cards.
    let flash_sale = load_home_flash_sale(pool, now).await?;

    // Collection sections: active collections in position
    // order, each with its first products (public visibility,
    // membership order).
    let feed_collections: Vec<Collection> = sqlx::query_as(&format!(
        "SELECT {COLLECTION_COLUMNS} FROM collections \
         WHERE is_active ORDER BY position ASC, created_at ASC LIMIT $1"
    ))
    .bind(HOME_COLLECTION_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load home collections", e))?;
    let mut collections = Vec::with_capacity(feed_collections.len());
    for collection in feed_collections {
        let products = load_collection_products(
            pool,
            collection.id,
            true,
            Some(HOME_COLLECTION_PRODUCT_LIMIT),
        )
        .await?;
        collections.push(HomeCollectionSection {
            collection,
            products,
        });
    }

    // The rails — all over the shared columns, all publicly
    // visible products only.
    let new_arrivals = load_rail(
        pool,
        &format!(
            "SELECT {PRODUCT_COLUMNS} FROM products p JOIN stores s ON s.id = p.store_id \
             {PRODUCT_RATING_JOIN} \
             WHERE p.status = 'active' AND s.status = 'active' \
             ORDER BY p.created_at DESC LIMIT {HOME_RAIL_LIMIT}"
        ),
        now,
    )
    .await?;
    let top_rated = load_rail(
        pool,
        &format!(
            "SELECT {PRODUCT_COLUMNS} FROM products p JOIN stores s ON s.id = p.store_id \
             {PRODUCT_RATING_JOIN} \
             WHERE p.status = 'active' AND s.status = 'active' AND rv.rating_count > 0 \
             ORDER BY rv.rating_avg DESC NULLS LAST, rv.rating_count DESC, \
             p.created_at DESC LIMIT {HOME_RAIL_LIMIT}"
        ),
        now,
    )
    .await?;
    let mut best_sellers = load_rail(
        pool,
        &format!(
            "SELECT {PRODUCT_COLUMNS} FROM products p JOIN stores s ON s.id = p.store_id \
             {PRODUCT_RATING_JOIN} \
             WHERE p.status = 'active' AND s.status = 'active' \
             ORDER BY sold_count DESC, p.created_at DESC LIMIT {HOME_RAIL_LIMIT}"
        ),
        now,
    )
    .await?;
    // A best-sellers rail of products nobody has bought is a
    // lie of framing: only products with real delivered sales
    // belong on it.
    best_sellers.retain(|product| product.sold_count > 0);
    let promoted = load_rail(
        pool,
        &format!(
            "SELECT {PRODUCT_COLUMNS} FROM products p JOIN stores s ON s.id = p.store_id \
             {PRODUCT_RATING_JOIN} \
             WHERE p.status = 'active' AND s.status = 'active' \
               AND EXISTS (SELECT 1 FROM promotions pr \
                           WHERE pr.product_id = p.id AND pr.status = 'active' \
                             AND pr.starts_at <= now() AND pr.ends_at > now()) \
             ORDER BY p.created_at DESC LIMIT {HOME_RAIL_LIMIT}"
        ),
        now,
    )
    .await?;

    // Featured stores: verified tiers only, most-stocked
    // first.
    let featured_stores: Vec<FeaturedStore> = sqlx::query_as(
        "SELECT s.id, s.slug, s.name, s.logo_url, s.banner_url, s.brand_color, \
                s.verified_tier, COUNT(p.id)::bigint AS product_count \
         FROM stores s \
         LEFT JOIN products p ON p.store_id = s.id AND p.status = 'active' \
         WHERE s.status = 'active' AND s.verified_tier <> 'unverified' \
         GROUP BY s.id \
         ORDER BY product_count DESC, s.created_at ASC LIMIT $1",
    )
    .bind(HOME_FEATURED_STORE_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load featured stores", e))?;

    Ok(HomeFeed {
        banners,
        categories,
        flash_sale,
        collections,
        new_arrivals,
        top_rated,
        best_sellers,
        promoted,
        featured_stores,
    })
}

/// The feed's flash sale block: the sale live at `now` plus
/// its publicly visible items as cards (biggest discount
/// first), each card's flash fields cap-checked by the
/// shared attach and its `sold_at_sale` read from the one
/// cap aggregate.
async fn load_home_flash_sale(
    pool: &PgPool,
    now: DateTime<Utc>,
) -> Result<Option<HomeFlashSale>, ApiError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| ApiError::internal("acquire connection for home flash sale", e))?;
    let Some(sale) = live_sale_on(&mut conn, now).await? else {
        return Ok(None);
    };
    let sale_row: (String, DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
        "SELECT title, starts_at, ends_at FROM flash_sales WHERE id = $1",
    )
    .bind(sale.id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("load home flash sale", e))?;

    #[derive(FromRow)]
    struct FlashCardRow {
        #[sqlx(flatten)]
        product: Product,
        sale_price_poisha: i64,
        stock_cap: Option<i32>,
    }
    let rows: Vec<FlashCardRow> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS}, fsi.sale_price_poisha, fsi.stock_cap \
         FROM flash_sale_items fsi \
         JOIN products p ON p.id = fsi.product_id \
         JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE fsi.flash_sale_id = $1 AND p.status = 'active' AND s.status = 'active' \
         ORDER BY (p.price_poisha - fsi.sale_price_poisha) DESC, p.created_at DESC \
         LIMIT $2"
    ))
    .bind(sale.id)
    .bind(HOME_FLASH_ITEM_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load home flash items", e))?;
    let product_ids: Vec<Uuid> = rows.iter().map(|row| row.product.id).collect();
    let sold = flash_units_sold(pool, sale.id, &product_ids).await?;
    let mut items: Vec<HomeFlashItem> = rows
        .into_iter()
        .map(|row| HomeFlashItem {
            sold_at_sale: sold.get(&row.product.id).copied().unwrap_or(0),
            product: with_stock_status(row.product),
            sale_price_poisha: row.sale_price_poisha,
            stock_cap: row.stock_cap,
        })
        .collect();
    let mut refs: Vec<&mut Product> = items.iter_mut().map(|item| &mut item.product).collect();
    attach_flash_prices_refs(pool, &mut refs, now).await?;
    Ok(Some(HomeFlashSale {
        id: sale.id,
        title: sale_row.0,
        starts_at: sale_row.1,
        ends_at: sale_row.2,
        items,
    }))
}

// Home feed handlers

/// `GET /v1/home` — the public feed, assembled at the
/// request instant. Anonymous by construction (nothing in
/// the feed varies by viewer), so it carries the catalogue
/// cache header like the other public catalogue reads.
pub async fn home_feed_handler(
    State(state): State<AppState>,
) -> Result<(HeaderMap, Json<HomeFeed>), ApiError> {
    let pool = state.require_pool()?;
    let feed = build_home_feed(pool, Utc::now()).await?;
    Ok((public_cache_headers(true), Json(feed)))
}

/// How many categories `GET /v1/categories` serves at
/// most (Wave 53). The home feed caps its tiles at
/// [`HOME_CATEGORY_LIMIT`]; the standalone list is the
/// full vocabulary page, so its cap is a safety bound,
/// not a display choice.
pub const CATEGORIES_LIST_LIMIT: i64 = 200;

/// The public category list payload (Wave 53): the same
/// tile shape the home feed carries, under its own
/// envelope so the endpoint can grow without the feed
/// changing.
#[derive(Debug, Serialize)]
pub struct CategoryList {
    pub categories: Vec<HomeCategory>,
}

/// `GET /v1/categories` — the platform's category
/// vocabulary with product counts and cover images, from
/// the same aggregate as the home feed's tiles
/// ([`load_categories`]). Public and anonymous, so it
/// carries the catalogue cache header.
pub async fn list_categories_handler(
    State(state): State<AppState>,
) -> Result<(HeaderMap, Json<CategoryList>), ApiError> {
    let pool = state.require_pool()?;
    let categories = load_categories(pool, CATEGORIES_LIST_LIMIT).await?;
    Ok((public_cache_headers(true), Json(CategoryList { categories })))
}

/// The admin preview query: `at` is an RFC 3339 timestamp.
#[derive(Deserialize)]
pub struct HomePreviewQuery {
    pub at: Option<String>,
}

/// `GET /v1/admin/home-preview?at=` — the feed as of a
/// scheduled instant, through the SAME builder the public
/// endpoint uses, so an admin can verify a campaign (banner
/// windows, a flash sale's start) before it goes live. A
/// malformed `at` is a 400 with the expected format named.
pub async fn admin_home_preview_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<HomePreviewQuery>,
) -> Result<Json<HomeFeed>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let now = match query.at.as_deref() {
        None => Utc::now(),
        Some(raw) => DateTime::parse_from_rfc3339(raw.trim())
            .map(|parsed| parsed.with_timezone(&Utc))
            .map_err(|_| {
                ApiError::bad_request(
                    "at must be an RFC 3339 timestamp, e.g. 2026-12-01T10:00:00Z — \
                     at হতে হবে RFC 3339 ফরম্যাটের সময়, যেমন 2026-12-01T10:00:00Z",
                )
            })?,
    };
    Ok(Json(build_home_feed(pool, now).await?))
}

// ---------------------------------------------------------------------------
// Seller flash participation
// ---------------------------------------------------------------------------

/// One of the seller's own products inside the live flash
/// sale, with the sale terms and the live cap accounting —
/// the seller's answer to "my product is being sold at what,
/// and how much of the cap is gone?". A seller whose price is
/// being discounted by a platform event must never have to
/// guess at either number.
#[derive(Debug, Serialize)]
pub struct SellerFlashItem {
    pub product_id: Uuid,
    pub product_title: String,
    pub store_slug: String,
    pub base_price_poisha: i64,
    pub sale_price_poisha: i64,
    pub stock_cap: Option<i32>,
    pub sold_at_sale: i64,
    /// Units still sellable at the sale price; null when the
    /// item is uncapped.
    pub cap_remaining: Option<i64>,
}

/// The live flash sale from one seller's seat: the sale
/// itself plus the seller's items in it (empty when the sale
/// carries none of their products).
#[derive(Debug, Serialize)]
pub struct SellerFlashSale {
    pub sale_id: Uuid,
    pub title: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub items: Vec<SellerFlashItem>,
}

/// The seller's view of the flash sale live right now:
/// `None` when no sale is live at all (the page renders its
/// honest "no sale running" state), otherwise the sale with
/// the caller's items in it — resolved through the same live
/// sale lookup and the same cap aggregate as every other
/// surface, so the seller sees the numbers the price
/// resolution itself is using.
pub async fn seller_flash_sale(
    pool: &PgPool,
    owner_user_id: Uuid,
) -> Result<Option<SellerFlashSale>, ApiError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| ApiError::internal("acquire connection for seller flash sale", e))?;
    let Some(sale) = live_sale_on(&mut conn, Utc::now()).await? else {
        return Ok(None);
    };
    let sale_row: (String, DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
        "SELECT title, starts_at, ends_at FROM flash_sales WHERE id = $1",
    )
    .bind(sale.id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("load seller flash sale", e))?;
    let rows: Vec<(Uuid, String, String, i64, i64, Option<i32>)> = sqlx::query_as(
        "SELECT p.id, p.title, s.slug, p.price_poisha, fsi.sale_price_poisha, fsi.stock_cap \
         FROM flash_sale_items fsi \
         JOIN products p ON p.id = fsi.product_id \
         JOIN stores s ON s.id = p.store_id \
         WHERE fsi.flash_sale_id = $1 AND s.owner_user_id = $2 \
         ORDER BY (p.price_poisha - fsi.sale_price_poisha) DESC, p.title ASC",
    )
    .bind(sale.id)
    .bind(owner_user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load seller flash items", e))?;
    let product_ids: Vec<Uuid> = rows.iter().map(|row| row.0).collect();
    let sold = flash_units_sold(pool, sale.id, &product_ids).await?;
    let items = rows
        .into_iter()
        .map(
            |(product_id, product_title, store_slug, base_price, sale_price, stock_cap)| {
                let sold_at_sale = sold.get(&product_id).copied().unwrap_or(0);
                SellerFlashItem {
                    product_id,
                    product_title,
                    store_slug,
                    base_price_poisha: base_price,
                    sale_price_poisha: sale_price,
                    stock_cap,
                    sold_at_sale,
                    cap_remaining: stock_cap.map(|cap| (i64::from(cap) - sold_at_sale).max(0)),
                }
            },
        )
        .collect();
    Ok(Some(SellerFlashSale {
        sale_id: sale.id,
        title: sale_row.0,
        starts_at: sale_row.1,
        ends_at: sale_row.2,
        items,
    }))
}

/// `GET /v1/seller/flash-sale` — the caller's participation
/// in the live flash sale (see [`seller_flash_sale`]). Any
/// signed-in user may ask; a user with no store simply has
/// no items. Answers `null` when no sale is live.
pub async fn seller_flash_sale_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Option<SellerFlashSale>>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(seller_flash_sale(pool, user.user_id).await?))
}

// ---------------------------------------------------------------------------
// Merchandising events + stats
// ---------------------------------------------------------------------------

/// The two countable merchandising interactions.
pub const MERCH_EVENT_BANNER_CLICK: &str = "banner_click";
/// See [`MERCH_EVENT_BANNER_CLICK`].
pub const MERCH_EVENT_COLLECTION_VIEW: &str = "collection_view";

/// Record one merchandising event (the public beacon): a
/// banner click or a collection landing view. Append-only —
/// the row is the count's raw material and nothing else is
/// stored (no user, no session: see migration 0047).
pub async fn record_merch_event(pool: &PgPool, kind: &str, target_id: Uuid) -> Result<(), ApiError> {
    if kind != MERCH_EVENT_BANNER_CLICK && kind != MERCH_EVENT_COLLECTION_VIEW {
        return Err(ApiError::bad_request(format!(
            "kind must be '{MERCH_EVENT_BANNER_CLICK}' or '{MERCH_EVENT_COLLECTION_VIEW}' — \
             kind হতে হবে '{MERCH_EVENT_BANNER_CLICK}' অথবা '{MERCH_EVENT_COLLECTION_VIEW}'"
        )));
    }
    sqlx::query("INSERT INTO merch_events (kind, target_id) VALUES ($1, $2)")
        .bind(kind)
        .bind(target_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("record merchandising event", e))?;
    Ok(())
}

#[derive(Deserialize)]
pub struct RecordMerchEventInput {
    pub kind: String,
    pub target_id: Uuid,
}

/// `POST /v1/merch/events` — the fire-and-forget beacon the
/// home page and collection pages call. Public by design
/// (clicks mostly come from anonymous visitors); the only
/// payload is a kind and a target id.
pub async fn record_merch_event_handler(
    State(state): State<AppState>,
    Json(input): Json<RecordMerchEventInput>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let pool = state.require_pool()?;
    record_merch_event(pool, &input.kind, input.target_id).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "recorded": true, "kind": input.kind })),
    ))
}

/// The admin merchandising stats block (the metrics
/// endpoint folds this in): event counts per kind, all-time
/// and for the current Asia/Dhaka day (the Wave 27/43
/// day-boundary convention the rest of the metrics use).
#[derive(Debug, Serialize)]
pub struct MerchandisingStats {
    pub banner_clicks_total: i64,
    pub collection_views_total: i64,
    pub banner_clicks_today: i64,
    pub collection_views_today: i64,
}

/// One recorded merchandising event, as the admin overview's
/// recent-activity list serves it. `target_title` resolves
/// the event's target to the banner's or collection's
/// CURRENT title (null when the target has been deleted —
/// the event outlives its target by design, since the log is
/// append-only).
#[derive(Debug, Serialize, FromRow)]
pub struct MerchEventView {
    pub id: i64,
    pub kind: String,
    pub target_id: Uuid,
    pub target_title: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// The most recent merchandising events, newest first — the
/// overview's "is the beacon alive, and what is being
/// clicked" check. One query: the title resolution is a pair
/// of LEFT JOINs over the two possible target tables.
pub async fn list_recent_merch_events(
    pool: &PgPool,
    limit: Option<i64>,
) -> Result<Vec<MerchEventView>, ApiError> {
    let limit = limit.unwrap_or(20).clamp(1, 100);
    sqlx::query_as(
        "SELECT e.id, e.kind, e.target_id, \
                COALESCE(b.title, c.title) AS target_title, e.created_at \
         FROM merch_events e \
         LEFT JOIN home_banners b ON b.id = e.target_id AND e.kind = 'banner_click' \
         LEFT JOIN collections c ON c.id = e.target_id AND e.kind = 'collection_view' \
         ORDER BY e.id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list merchandising events", e))
}

/// `GET /v1/admin/merch-events` — the recent-events list
/// (admin only; see [`list_recent_merch_events`]).
pub async fn list_merch_events_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<MerchListQuery>,
) -> Result<Json<Vec<MerchEventView>>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(list_recent_merch_events(pool, query.limit).await?))
}

/// Read the merchandising stats in ONE aggregate.
pub async fn merchandising_stats(pool: &PgPool) -> Result<MerchandisingStats, ApiError> {
    let row: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
           COUNT(*) FILTER (WHERE kind = 'banner_click'), \
           COUNT(*) FILTER (WHERE kind = 'collection_view'), \
           COUNT(*) FILTER (WHERE kind = 'banner_click' \
               AND (created_at AT TIME ZONE 'Asia/Dhaka')::date \
                   = (now() AT TIME ZONE 'Asia/Dhaka')::date), \
           COUNT(*) FILTER (WHERE kind = 'collection_view' \
               AND (created_at AT TIME ZONE 'Asia/Dhaka')::date \
                   = (now() AT TIME ZONE 'Asia/Dhaka')::date) \
         FROM merch_events",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("merchandising stats", e))?;
    Ok(MerchandisingStats {
        banner_clicks_total: row.0,
        collection_views_total: row.1,
        banner_clicks_today: row.2,
        collection_views_today: row.3,
    })
}
