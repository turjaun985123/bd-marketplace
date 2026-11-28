//! Storefront section layouts (Wave 58): the section
//! editor — Shopify's signature storefront customisation,
//! built as a real sections system rather than a settings
//! blob.
//!
//! A seller composes their storefront HOME from ordered,
//! configurable sections (domain catalogue:
//! `domain::STOREFRONT_SECTION_TYPES` — hero, rich text,
//! product rails, collection shelves, category grids, the
//! catalogue grid, image banners, store info, FAQ). The
//! layout is one JSONB document per store (migration
//! 0065), validated and normalised by the domain crate on
//! every write; the theme (Wave 18: classic/minimal/bold
//! + brand colour) still frames the page — sections are
//! layout, theme is skin.
//!
//! Surfaces:
//!
//! - `GET /v1/stores/:slug/layout` — PUBLIC. The RESOLVED
//!   render model: sections in order, each carrying its
//!   data already loaded server-side (product rails carry
//!   real product cards through the shared product view
//!   machinery, flash prices attached; shelves carry
//!   their collections / products; grids carry counts;
//!   the hero falls back to the store's branding). The
//!   web renders this model and never re-fetches per
//!   section. Disabled sections, sections outside their
//!   scheduling window, unknown (future) section types,
//!   and sections whose data resolves empty (an on-sale
//!   rail with nothing on sale, a shelf whose collection
//!   was deleted, a store-info block with neither text
//!   written) are DROPPED here, server-side — the one
//!   exception is the catalogue grid, which renders even
//!   for an empty catalogue (it carries the store's
//!   product total, and an empty storefront must still
//!   show its empty state where the seller placed it).
//! - `GET|PUT /v1/seller/store/layout`,
//!   `POST /v1/seller/store/layout/reset` — the seller's
//!   own document, under EXACTLY the branding access rule
//!   (Wave 34 `manage_marketing`: the owner, or a manager
//!   acting via `?store_id=`; staff are refused, strangers
//!   get the scoped 404). PUT is a full replace: the whole
//!   document is domain-validated first (errors are
//!   bilingual and name the offending section's position),
//!   then its references are checked against the store
//!   (a manual rail may only name the store's own
//!   products; a shelf may only name the store's own
//!   collections) — a failed PUT changes nothing. There
//!   is deliberately NO version-conflict guard: the editor
//!   is one seller surface, last write wins, and the
//!   stored `updated_at`/`updated_by` keep the provenance.
//! - `GET /v1/seller/store/layout/catalogue` — the editor
//!   metadata (types, labels, descriptions, caps, layout
//!   presets) served from the domain catalogue, so the
//!   picker and the validator can never drift apart.
//! - `POST /v1/stores/:slug/section-events` +
//!   `GET /v1/seller/store/layout/stats` — analytics-lite
//!   (migration 0066): aggregate-only section view/click
//!   counts in the Wave 52 merch-events pattern. No user,
//!   no session, no IP is ever recorded.
//!
//! DEFAULT LAYOUT: a store with no row renders
//! `domain::default_storefront_layout()` — the composition
//! equivalent to the pre-Wave-58 storefront (hero from
//! branding, the three rails in their historical order
//! with their historical titles, the collection shelf,
//! both category grids, the catalogue grid, the info
//! blocks). The fallback is CODE, never a seeded row, so
//! reset = DELETE the row, and a store that never opens
//! the editor renders exactly what it always rendered.
//!
//! Scheduling (the Wave 52 banner rule, per section): a
//! section may carry `starts_at`/`ends_at`; the public
//! read keeps it only while `now` is inside the half-open
//! window [starts_at, ends_at) and it is enabled — the
//! domain's one visibility rule
//! (`storefront_section_is_live`, the banner rule itself).
//! The seller's own GET always returns the full stored
//! document, windows and disabled sections included.
//!
//! Preview: there is deliberately no preview endpoint —
//! the editor previews the DRAFT client-side through the
//! same section components, resolving rails/shelves from
//! the seller's own product/collection reads. The
//! fidelity limit is documented in the README: a preview
//! can differ from this endpoint's resolution (flash
//! prices mid-sale, a collection deleted in another tab)
//! — saving and reading the public layout is the truth.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    storefront_layout_preset, storefront_section_catalogue, storefront_section_is_live,
    validate_storefront_event_kind, validate_storefront_layout, StorefrontLayout,
    StorefrontSection, STOREFRONT_LAYOUT_PRESETS, STOREFRONT_LAYOUT_VERSION,
    STOREFRONT_STATS_WINDOW_DAYS, STORE_ACTION_MANAGE_MARKETING,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::access::{resolve_scoped_store, StoreScopeQuery};
use crate::auth::AuthUser;
use crate::merch::{attach_flash_prices, FLASH_PRICE_JOIN};
use crate::products::{with_stock_status, Product, PRODUCT_COLUMNS, PRODUCT_RATING_JOIN};
use crate::store_collections::StoreCollection;
use crate::stores::{get_store_by_slug, Store};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

/// One section as the seller's editor sends and receives
/// it: the domain section with its scheduling window in
/// RFC 3339 (the API's one timestamp convention) instead
/// of the domain's unix seconds. `config` passes through
/// untouched — its shape is the type's own business,
/// validated by the domain crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutSectionWire {
    pub id: String,
    #[serde(rename = "type")]
    pub section_type: String,
    #[serde(default = "default_wire_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<DateTime<Utc>>,
}

fn default_wire_enabled() -> bool {
    true
}

/// The PUT body: the whole layout document. `version` may
/// be omitted (the current version is assumed); a stated
/// version other than the current one is refused by the
/// domain validator rather than silently rewritten.
#[derive(Debug, Deserialize)]
pub struct LayoutInput {
    pub version: Option<i64>,
    #[serde(default)]
    pub sections: Vec<LayoutSectionWire>,
}

/// The seller's layout view: the effective document (the
/// stored one, or the platform default when the store has
/// never customised), whether it is custom, and when the
/// stored row was last written.
#[derive(Debug, Serialize)]
pub struct SellerLayoutView {
    pub store_id: Uuid,
    pub store_slug: String,
    pub version: i64,
    pub is_custom: bool,
    pub sections: Vec<LayoutSectionWire>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// One preset in the catalogue: a named starting layout
/// the editor can load into the draft in one click.
#[derive(Debug, Serialize)]
pub struct LayoutPresetWire {
    pub key: String,
    pub label_en: String,
    pub label_bn: String,
    pub sections: Vec<LayoutSectionWire>,
}

/// The editor catalogue: everything the add-section
/// picker and the config forms need, served from the
/// domain so the UI and the validator share one source.
#[derive(Debug, Serialize)]
pub struct LayoutCatalogueView {
    pub version: i64,
    pub max_sections: usize,
    pub types: Vec<domain::SectionTypeMeta>,
    pub presets: Vec<LayoutPresetWire>,
    /// The rail sources the config form offers (the
    /// domain's closed list — the form never hard-codes
    /// a vocabulary the validator would refuse).
    pub rail_sources: Vec<String>,
    /// The category-grid modes, same reasoning.
    pub category_grid_modes: Vec<String>,
}

/// The hero's resolved data: the section's own fields
/// where the seller set them, the store's branding where
/// they did not (headline ← store name, subtext ←
/// description, image ← banner). `announcement` always
/// rides from the store — it is the announcement BAR's
/// text, a branding field with no per-section override.
#[derive(Debug, Serialize)]
pub struct HeroData {
    pub headline: Option<String>,
    pub subtext: Option<String>,
    pub image_url: Option<String>,
    pub announcement: Option<String>,
    pub link_url: Option<String>,
    pub link_label: Option<String>,
    /// Whether the seller set ANY hero field of their own
    /// (headline / subtext / image / link). A pure
    /// branding echo is not customised: the web renders
    /// the announcement + banner pieces (the old frame)
    /// and skips the headline block, which would only
    /// repeat the store header's name and description.
    pub is_customised: bool,
}

/// One category tile of a category grid: the taxonomy
/// shape (slug + names + path) or the label shape
/// (`category`), with the live product count.
#[derive(Debug, Serialize)]
pub struct CategoryTile {
    pub name: String,
    pub name_en: Option<String>,
    pub name_bn: Option<String>,
    pub slug: Option<String>,
    pub category: Option<String>,
    pub icon_key: Option<String>,
    pub product_count: i64,
    pub path: Vec<crate::categories::CategoryPathEntry>,
}

/// The store info section's resolved data: the profile
/// texts from the store row (edited on the branding
/// surface, never duplicated into the layout) plus the
/// store's trust numbers — the same live aggregates the
/// public store view serves (Wave 54).
#[derive(Debug, Serialize)]
pub struct StoreInfoData {
    pub about: Option<String>,
    pub return_policy: Option<String>,
    pub rating_avg: Option<f64>,
    pub rating_count: i64,
    pub total_sold: i64,
    pub product_count: i64,
    pub follower_count: i64,
}

/// One RESOLVED section of the public render model: the
/// section's identity and config, plus exactly one
/// payload family filled by its type (the rest stay
/// absent from the JSON). `products` serves rails and
/// single-collection shelves; `collections` the
/// all-collections shelf; `total_products` the catalogue
/// grid (whose cards the web pages through the store's
/// product list, as it always has).
#[derive(Debug, Serialize)]
pub struct ResolvedSection {
    pub id: String,
    #[serde(rename = "type")]
    pub section_type: String,
    pub config: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hero: Option<HeroData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub products: Option<Vec<Product>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_products: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collections: Option<Vec<StoreCollection>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<StoreCollection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<CategoryTile>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_info: Option<StoreInfoData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faq_items: Option<Vec<domain::FaqItem>>,
}

/// The public layout response: the store's effective
/// composition, resolved. `is_custom` tells the web (and
/// curious sellers) whether this store composed its own
/// layout or renders the platform default.
#[derive(Debug, Serialize)]
pub struct PublicLayoutView {
    pub store_slug: String,
    pub version: i64,
    pub is_custom: bool,
    pub sections: Vec<ResolvedSection>,
}

/// The section-event beacon body (public, anonymous).
#[derive(Debug, Deserialize)]
pub struct SectionEventInput {
    pub section_id: String,
    pub kind: String,
}

/// One section's analytics row in the seller stats view.
#[derive(Debug, Serialize)]
pub struct SectionStats {
    pub section_id: String,
    #[serde(rename = "type")]
    pub section_type: String,
    pub views_total: i64,
    pub clicks_total: i64,
    pub views_today: i64,
    pub clicks_today: i64,
    pub views_window: i64,
    pub clicks_window: i64,
}

/// The seller stats response: one row per section of the
/// store's CURRENT effective layout (zeros included — a
/// section nobody has seen yet is information too), over
/// the trailing window the domain names.
#[derive(Debug, Serialize)]
pub struct LayoutStatsView {
    pub store_id: Uuid,
    pub window_days: i64,
    pub sections: Vec<SectionStats>,
}

// ---------------------------------------------------------------------------
// Conversions between the wire and domain shapes
// ---------------------------------------------------------------------------

fn wire_to_domain_section(wire: &LayoutSectionWire) -> StorefrontSection {
    StorefrontSection {
        id: wire.id.clone(),
        section_type: wire.section_type.clone(),
        enabled: wire.enabled,
        config: wire.config.clone(),
        starts_at: wire.starts_at.map(|moment| moment.timestamp()),
        ends_at: wire.ends_at.map(|moment| moment.timestamp()),
    }
}

fn domain_to_wire_section(section: &StorefrontSection) -> LayoutSectionWire {
    LayoutSectionWire {
        id: section.id.clone(),
        section_type: section.section_type.clone(),
        enabled: section.enabled,
        config: section.config.clone(),
        starts_at: section
            .starts_at
            .and_then(|unix| DateTime::from_timestamp(unix, 0)),
        ends_at: section
            .ends_at
            .and_then(|unix| DateTime::from_timestamp(unix, 0)),
    }
}

fn layout_view(
    store: &Store,
    layout: &StorefrontLayout,
    is_custom: bool,
    updated_at: Option<DateTime<Utc>>,
) -> SellerLayoutView {
    SellerLayoutView {
        store_id: store.id,
        store_slug: store.slug.clone(),
        version: layout.version,
        is_custom,
        sections: layout.sections.iter().map(domain_to_wire_section).collect(),
        updated_at,
    }
}

/// Read a string out of a resolved config (configs that
/// reach resolution were normalised on write, so a key is
/// either absent or the right type; the default-layout
/// document is built to the same shape).
fn config_str<'a>(config: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    config.get(key).and_then(|value| value.as_str())
}

/// Read an integer out of a resolved config, with the
/// caller's fallback for a missing key.
fn config_i64(config: &serde_json::Value, key: &str, fallback: i64) -> i64 {
    config
        .get(key)
        .and_then(|value| value.as_i64())
        .unwrap_or(fallback)
}

// ---------------------------------------------------------------------------
// Storage: the layout document + the effective-layout read
// ---------------------------------------------------------------------------

/// The stored half of a layout row.
#[derive(Debug, FromRow)]
struct StoredLayoutRow {
    layout: serde_json::Value,
    updated_at: DateTime<Utc>,
}

/// A store's EFFECTIVE layout: its stored document when
/// it has one, else the platform default from the domain
/// (code, not a row). Returns (layout, is_custom,
/// updated_at of the stored row).
async fn effective_layout(
    pool: &PgPool,
    store_id: Uuid,
) -> Result<(StorefrontLayout, bool, Option<DateTime<Utc>>), ApiError> {
    let row: Option<StoredLayoutRow> = sqlx::query_as(
        "SELECT layout, updated_at FROM store_layouts WHERE store_id = $1",
    )
    .bind(store_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load store layout", e))?;
    match row {
        Some(row) => {
            let layout: StorefrontLayout = serde_json::from_value(row.layout).map_err(|e| {
                ApiError::internal_any("parse stored store layout", e)
            })?;
            Ok((layout, true, Some(row.updated_at)))
        }
        None => Ok((domain::default_storefront_layout(), false, None)),
    }
}

/// Cross-check a validated layout's references against
/// the store that owns it: a manual rail may only name
/// the store's OWN products, and a shelf may only name
/// the store's OWN collection. (Domain validation already
/// proved the ids are well-formed; this proves they are
/// the seller's to show — a foreign id is a 400 naming
/// the section, never a silent drop, because on WRITE the
/// seller can fix it. At RESOLUTION time the same
/// situation — a product drafted, a collection deleted —
/// drops quietly instead, because the buyer cannot.)
async fn validate_layout_references(
    pool: &PgPool,
    store_id: Uuid,
    layout: &StorefrontLayout,
) -> Result<(), ApiError> {
    for (index, section) in layout.sections.iter().enumerate() {
        let position = index + 1;
        if section.section_type == domain::SECTION_TYPE_PRODUCT_RAIL
            && config_str(&section.config, "source") == Some(domain::RAIL_SOURCE_MANUAL)
        {
            let raw_ids = section
                .config
                .get("product_ids")
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default();
            let mut ids: Vec<Uuid> = Vec::with_capacity(raw_ids.len());
            for raw in &raw_ids {
                let Some(text) = raw.as_str() else {
                    continue;
                };
                let parsed = Uuid::parse_str(text).map_err(|_| {
                    ApiError::bad_request(format!(
                        "section {position} (product_rail): product id '{text}' is not a valid id — \
                         '{text}' সঠিক পণ্য আইডি নয়"
                    ))
                })?;
                ids.push(parsed);
            }
            let owned: Vec<Uuid> = sqlx::query_scalar(
                "SELECT id FROM products WHERE store_id = $1 AND id = ANY($2)",
            )
            .bind(store_id)
            .bind(&ids)
            .fetch_all(pool)
            .await
            .map_err(|e| ApiError::internal("check manual rail products", e))?;
            for id in &ids {
                if !owned.contains(id) {
                    return Err(ApiError::bad_request(format!(
                        "section {position} (product_rail): product {id} does not belong to this store — \
                         {id} পণ্যটি এই স্টোরের নয়"
                    )));
                }
            }
        }
        if section.section_type == domain::SECTION_TYPE_COLLECTION_SHELF {
            if let Some(raw) = config_str(&section.config, "collection_id") {
                let collection_id = Uuid::parse_str(raw).map_err(|_| {
                    ApiError::bad_request(format!(
                        "section {position} (collection_shelf): collection id '{raw}' is not a valid id — \
                         '{raw}' সঠিক কালেকশন আইডি নয়"
                    ))
                })?;
                let exists: Option<Uuid> = sqlx::query_scalar(
                    "SELECT id FROM store_collections WHERE store_id = $1 AND id = $2",
                )
                .bind(store_id)
                .bind(collection_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| ApiError::internal("check shelf collection", e))?;
                if exists.is_none() {
                    return Err(ApiError::bad_request(format!(
                        "section {position} (collection_shelf): collection {collection_id} does not belong to this store — \
                         {collection_id} কালেকশনটি এই স্টোরের নয়"
                    )));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Resolution: turning a stored section into render data
// ---------------------------------------------------------------------------

/// The delivered-units aggregate the best-sellers rail
/// orders by — the platform's one "sold" (delivered
/// sub-orders only), as a joinable subquery.
const SOLD_UNITS_JOIN: &str = "LEFT JOIN ( \
     SELECT oi.product_id AS sold_pid, SUM(oi.quantity)::bigint AS sold_units \
     FROM order_items oi \
     JOIN sub_orders so ON so.id = oi.sub_order_id \
     WHERE so.status = 'delivered' \
     GROUP BY oi.product_id \
 ) sold ON sold.sold_pid = p.id";

/// Load one automatic rail's products: the shared product
/// columns + rating join over the store's ACTIVE products,
/// with the source's own filter and order. Flash prices
/// attach afterwards like every product surface.
async fn load_rail_products(
    pool: &PgPool,
    store_id: Uuid,
    source: &str,
    limit: i64,
    now: DateTime<Utc>,
) -> Result<Vec<Product>, ApiError> {
    let sql = match source {
        domain::RAIL_SOURCE_BEST_SELLERS => format!(
            "SELECT {PRODUCT_COLUMNS} FROM products p \
             JOIN stores s ON s.id = p.store_id {PRODUCT_RATING_JOIN} {SOLD_UNITS_JOIN} \
             WHERE p.store_id = $1 AND p.status = 'active' \
             AND COALESCE(sold.sold_units, 0) > 0 \
             ORDER BY sold.sold_units DESC, p.created_at DESC LIMIT $2"
        ),
        domain::RAIL_SOURCE_ON_SALE => format!(
            "SELECT {PRODUCT_COLUMNS} FROM products p \
             JOIN stores s ON s.id = p.store_id {PRODUCT_RATING_JOIN} {FLASH_PRICE_JOIN} \
             WHERE p.store_id = $1 AND p.status = 'active' \
             AND (fx.flash_price_poisha IS NOT NULL \
                  OR (p.compare_at_poisha IS NOT NULL \
                      AND p.compare_at_poisha > p.price_poisha)) \
             ORDER BY p.created_at DESC LIMIT $2"
        ),
        // newest — and the fallback for a source string a
        // future build wrote (resolution never fails a
        // storefront over a value it does not know).
        _ => format!(
            "SELECT {PRODUCT_COLUMNS} FROM products p \
             JOIN stores s ON s.id = p.store_id {PRODUCT_RATING_JOIN} \
             WHERE p.store_id = $1 AND p.status = 'active' \
             ORDER BY p.created_at DESC, p.id DESC LIMIT $2"
        ),
    };
    let mut products: Vec<Product> = sqlx::query_as(&sql)
        .bind(store_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("load rail products", e))?
        .into_iter()
        .map(with_stock_status)
        .collect();
    attach_flash_prices(pool, &mut products, now).await?;
    Ok(products)
}

/// Load a manual rail's products: the seller's ids, the
/// store's ACTIVE products only (a product drafted or
/// deleted after the layout was saved drops out quietly),
/// re-ordered in Rust into the seller's saved order —
/// SQL's ANY has no order of its own.
async fn load_manual_rail_products(
    pool: &PgPool,
    store_id: Uuid,
    config: &serde_json::Value,
    now: DateTime<Utc>,
) -> Result<Vec<Product>, ApiError> {
    let raw_ids = config
        .get("product_ids")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let mut ids: Vec<Uuid> = Vec::with_capacity(raw_ids.len());
    for raw in &raw_ids {
        if let Some(text) = raw.as_str() {
            if let Ok(parsed) = Uuid::parse_str(text) {
                ids.push(parsed);
            }
        }
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let fetched: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products p \
         JOIN stores s ON s.id = p.store_id {PRODUCT_RATING_JOIN} \
         WHERE p.store_id = $1 AND p.status = 'active' AND p.id = ANY($2)"
    ))
    .bind(store_id)
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load manual rail products", e))?;
    let mut by_id: std::collections::HashMap<Uuid, Product> = fetched
        .into_iter()
        .map(|product| (product.id, with_stock_status(product)))
        .collect();
    let mut ordered: Vec<Product> = Vec::with_capacity(ids.len());
    for id in &ids {
        if let Some(product) = by_id.remove(id) {
            ordered.push(product);
        }
    }
    attach_flash_prices(pool, &mut ordered, now).await?;
    Ok(ordered)
}

/// Load one collection's member products for a shelf, in
/// the seller's position order — the public collection
/// page's own read, capped by the shelf's limit.
async fn load_shelf_products(
    pool: &PgPool,
    collection_id: Uuid,
    limit: i64,
    now: DateTime<Utc>,
) -> Result<Vec<Product>, ApiError> {
    let mut products: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM store_collection_products cp \
         JOIN products p ON p.id = cp.product_id \
         JOIN stores s ON s.id = p.store_id {PRODUCT_RATING_JOIN} \
         WHERE cp.collection_id = $1 AND p.status = 'active' \
         ORDER BY cp.position ASC, p.id ASC LIMIT $2"
    ))
    .bind(collection_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load shelf products", e))?
    .into_iter()
    .map(with_stock_status)
    .collect();
    attach_flash_prices(pool, &mut products, now).await?;
    Ok(products)
}

/// The label-mode category tiles: the faceted browse's
/// category facet scoped to this store (Wave 54's
/// StoreCategories read) — same grouping, same order, so
/// the grid and the old component can never disagree.
async fn load_label_category_tiles(
    pool: &PgPool,
    store_id: Uuid,
    limit: i64,
) -> Result<Vec<CategoryTile>, ApiError> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT p.category AS category, COUNT(*)::bigint AS count \
         FROM products p \
         WHERE p.store_id = $1 AND p.status = 'active' \
         AND p.category IS NOT NULL AND btrim(p.category) <> '' \
         GROUP BY p.category ORDER BY count DESC, p.category ASC LIMIT $2",
    )
    .bind(store_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load store category labels", e))?;
    Ok(rows
        .into_iter()
        .map(|(category, count)| CategoryTile {
            name: category.clone(),
            name_en: None,
            name_bn: None,
            slug: None,
            category: Some(category),
            icon_key: None,
            product_count: count,
            path: Vec::new(),
        })
        .collect())
}

/// Resolve ONE section into its render payload, or `None`
/// when the section must not render: disabled, outside
/// its scheduling window, an unknown (future) type, or a
/// known type whose data resolves empty. The catalogue
/// grid is the one exception — it resolves even for an
/// empty catalogue, carrying the total.
async fn resolve_section(
    pool: &PgPool,
    store: &Store,
    section: &StorefrontSection,
    now: DateTime<Utc>,
) -> Result<Option<ResolvedSection>, ApiError> {
    if !storefront_section_is_live(section, now.timestamp()) {
        return Ok(None);
    }
    let mut resolved = ResolvedSection {
        id: section.id.clone(),
        section_type: section.section_type.clone(),
        config: section.config.clone(),
        hero: None,
        products: None,
        total_products: None,
        collections: None,
        collection: None,
        categories: None,
        store_info: None,
        faq_items: None,
    };
    match section.section_type.as_str() {
        domain::SECTION_TYPE_HERO => {
            let is_customised = [
                "headline",
                "subtext",
                "image_url",
                "link_url",
                "link_label",
            ]
            .iter()
            .any(|key| config_str(&section.config, key).is_some());
            resolved.hero = Some(HeroData {
                headline: config_str(&section.config, "headline")
                    .map(str::to_string)
                    .or_else(|| Some(store.name.clone())),
                subtext: config_str(&section.config, "subtext")
                    .map(str::to_string)
                    .or_else(|| store.description.clone()),
                image_url: config_str(&section.config, "image_url")
                    .map(str::to_string)
                    .or_else(|| store.banner_url.clone()),
                announcement: store.announcement.clone(),
                link_url: config_str(&section.config, "link_url").map(str::to_string),
                link_label: config_str(&section.config, "link_label").map(str::to_string),
                is_customised,
            });
        }
        domain::SECTION_TYPE_RICH_TEXT => {
            // The payload IS the config (heading + body,
            // validated on write); nothing to load.
        }
        domain::SECTION_TYPE_PRODUCT_RAIL => {
            let source = config_str(&section.config, "source")
                .unwrap_or(domain::RAIL_SOURCE_NEWEST);
            let limit = config_i64(
                &section.config,
                "limit",
                domain::DEFAULT_RAIL_LIMIT,
            );
            let products = if source == domain::RAIL_SOURCE_MANUAL {
                load_manual_rail_products(pool, store.id, &section.config, now).await?
            } else {
                load_rail_products(pool, store.id, source, limit, now).await?
            };
            if products.is_empty() {
                return Ok(None);
            }
            resolved.products = Some(products);
        }
        domain::SECTION_TYPE_COLLECTION_SHELF => {
            let limit = config_i64(
                &section.config,
                "limit",
                domain::DEFAULT_SHELF_LIMIT,
            );
            match config_str(&section.config, "collection_id") {
                Some(raw) => {
                    let Ok(collection_id) = Uuid::parse_str(raw) else {
                        return Ok(None);
                    };
                    // The same read the collections module
                    // serves (active members counted); a
                    // collection deleted after the layout
                    // was saved drops the section quietly.
                    let collection = match crate::store_collections::load_collection(
                        pool,
                        store.id,
                        collection_id,
                        true,
                    )
                    .await
                    {
                        Ok(collection) => collection,
                        Err(error) if error.status == StatusCode::NOT_FOUND => {
                            return Ok(None)
                        }
                        Err(error) => return Err(error),
                    };
                    let products =
                        load_shelf_products(pool, collection_id, limit, now).await?;
                    if products.is_empty() {
                        return Ok(None);
                    }
                    resolved.collection = Some(collection);
                    resolved.products = Some(products);
                }
                None => {
                    let tiles: Vec<StoreCollection> =
                        crate::store_collections::list_collections(pool, store.id, true)
                            .await?
                            .into_iter()
                            .filter(|collection| collection.product_count > 0)
                            .collect();
                    if tiles.is_empty() {
                        return Ok(None);
                    }
                    resolved.collections = Some(tiles);
                }
            }
        }
        domain::SECTION_TYPE_CATEGORY_GRID => {
            let limit = config_i64(
                &section.config,
                "limit",
                domain::DEFAULT_CATEGORY_GRID_LIMIT,
            );
            let tiles = if config_str(&section.config, "mode")
                == Some(domain::CATEGORY_GRID_MODE_LABELS)
            {
                load_label_category_tiles(pool, store.id, limit).await?
            } else {
                let specialties =
                    crate::categories::store_category_specialties(pool, &store.slug).await?;
                specialties
                    .into_iter()
                    .take(limit.max(0) as usize)
                    .map(|specialty| CategoryTile {
                        name: specialty
                            .name_bn
                            .clone()
                            .unwrap_or_else(|| specialty.name_en.clone()),
                        name_en: Some(specialty.name_en.clone()),
                        name_bn: specialty.name_bn.clone(),
                        slug: Some(specialty.slug.clone()),
                        category: None,
                        icon_key: Some(specialty.icon_key.clone()),
                        product_count: specialty.product_count,
                        path: specialty.path.clone(),
                    })
                    .collect()
            };
            if tiles.is_empty() {
                return Ok(None);
            }
            resolved.categories = Some(tiles);
        }
        domain::SECTION_TYPE_PRODUCT_GRID => {
            let total: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM products WHERE store_id = $1 AND status = 'active'",
            )
            .bind(store.id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("count store products", e))?;
            resolved.total_products = Some(total);
        }
        domain::SECTION_TYPE_IMAGE_BANNER => {
            // The payload IS the config (image + link +
            // caption, validated on write).
        }
        domain::SECTION_TYPE_STORE_INFO => {
            if store.about.is_none() && store.return_policy.is_none() {
                return Ok(None);
            }
            let public = crate::stores::get_public_store(pool, &store.slug, None).await?;
            resolved.store_info = Some(StoreInfoData {
                about: store.about.clone(),
                return_policy: store.return_policy.clone(),
                rating_avg: public.rating_avg,
                rating_count: public.rating_count,
                total_sold: public.total_sold,
                product_count: public.product_count,
                follower_count: public.follower_count,
            });
        }
        domain::SECTION_TYPE_FAQ => {
            let items: Vec<domain::FaqItem> = section
                .config
                .get("items")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default();
            if items.is_empty() {
                return Ok(None);
            }
            resolved.faq_items = Some(items);
        }
        _ => {
            // An unknown (future) section type: preserved
            // in the document, skipped at render — the
            // domain's forward-tolerance rule.
            return Ok(None);
        }
    }
    Ok(Some(resolved))
}

// ---------------------------------------------------------------------------
// Public read
// ---------------------------------------------------------------------------

/// Load a store's public render model: the store must
/// exist AND be active (a suspended store's storefront is
/// hidden with the store — the collections precedent),
/// then every section of its effective layout resolves in
/// order, with the unrenderable ones dropped.
pub async fn public_layout(pool: &PgPool, slug: &str) -> Result<PublicLayoutView, ApiError> {
    let store = get_store_by_slug(pool, slug).await?;
    if store.status != "active" {
        return Err(ApiError::not_found("store not found"));
    }
    let (layout, is_custom, _) = effective_layout(pool, store.id).await?;
    let now = Utc::now();
    let mut sections = Vec::with_capacity(layout.sections.len());
    for section in &layout.sections {
        if let Some(resolved) = resolve_section(pool, &store, section, now).await? {
            sections.push(resolved);
        }
    }
    Ok(PublicLayoutView {
        store_slug: store.slug.clone(),
        version: layout.version,
        is_custom,
        sections,
    })
}

pub async fn public_layout_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<PublicLayoutView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(public_layout(pool, &slug).await?))
}

// ---------------------------------------------------------------------------
// Seller reads + writes (the branding access rule)
// ---------------------------------------------------------------------------

/// Resolve the store a layout request acts on — the
/// branding rule verbatim (Wave 34 `manage_marketing` via
/// the shared scoped resolver): the caller's own store,
/// or a `?store_id=` they manage marketing for.
async fn resolve_layout_store(
    pool: &PgPool,
    user: &AuthUser,
    scope: &StoreScopeQuery,
) -> Result<Store, ApiError> {
    let (store, _role) = resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        STORE_ACTION_MANAGE_MARKETING,
    )
    .await?;
    Ok(store)
}

pub async fn seller_get_layout_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<SellerLayoutView>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_layout_store(pool, &user, &scope).await?;
    let (layout, is_custom, updated_at) = effective_layout(pool, store.id).await?;
    Ok(Json(layout_view(&store, &layout, is_custom, updated_at)))
}

/// PUT the whole layout: domain validation first (a
/// failed PUT changes nothing), reference checks second,
/// then one upsert. Last write wins — no conflict guard,
/// by design (see the module doc).
pub async fn seller_put_layout_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
    Json(input): Json<LayoutInput>,
) -> Result<Json<SellerLayoutView>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_layout_store(pool, &user, &scope).await?;
    let draft = StorefrontLayout {
        version: input.version.unwrap_or(STOREFRONT_LAYOUT_VERSION),
        sections: input.sections.iter().map(wire_to_domain_section).collect(),
    };
    let validated = validate_storefront_layout(&draft).map_err(ApiError::bad_request)?;
    validate_layout_references(pool, store.id, &validated).await?;
    let document = serde_json::to_value(&validated)
        .map_err(|e| ApiError::internal_any("serialise store layout", e))?;
    let updated_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO store_layouts (store_id, layout, updated_by, created_at, updated_at) \
         VALUES ($1, $2, $3, now(), now()) \
         ON CONFLICT (store_id) DO UPDATE SET \
             layout = EXCLUDED.layout, \
             updated_by = EXCLUDED.updated_by, \
             updated_at = now() \
         RETURNING updated_at",
    )
    .bind(store.id)
    .bind(document)
    .bind(user.user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("save store layout", e))?;
    Ok(Json(layout_view(&store, &validated, true, Some(updated_at))))
}

/// Reset = DELETE the row: the store falls back to the
/// platform default composition (code, not data), and the
/// response is that default document, `is_custom: false`.
pub async fn seller_reset_layout_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<SellerLayoutView>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_layout_store(pool, &user, &scope).await?;
    sqlx::query("DELETE FROM store_layouts WHERE store_id = $1")
        .bind(store.id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("reset store layout", e))?;
    Ok(Json(layout_view(
        &store,
        &domain::default_storefront_layout(),
        false,
        None,
    )))
}

/// The editor catalogue: the domain's type metadata, the
/// caps, and the presets in wire form. Any signed-in user
/// may read it (it is platform metadata, not store data);
/// the editor page is seller-gated by its layout reads.
pub async fn seller_layout_catalogue_handler(
    _user: AuthUser,
) -> Result<Json<LayoutCatalogueView>, ApiError> {
    let presets = STOREFRONT_LAYOUT_PRESETS
        .iter()
        .filter_map(|key| {
            let layout = storefront_layout_preset(key)?;
            let (label_en, label_bn) = match *key {
                domain::LAYOUT_PRESET_MINIMAL => {
                    ("Minimal", "মিনিমাল — শুধু প্রয়োজনীয় অংশ")
                }
                domain::LAYOUT_PRESET_MERCHANDISING => {
                    ("Merchandising", "মার্চেন্ডাইজিং — বিক্রি সামনে")
                }
                _ => ("Platform default", "প্ল্যাটফর্ম ডিফল্ট"),
            };
            Some(LayoutPresetWire {
                key: (*key).to_string(),
                label_en: label_en.to_string(),
                label_bn: label_bn.to_string(),
                sections: layout.sections.iter().map(domain_to_wire_section).collect(),
            })
        })
        .collect();
    Ok(Json(LayoutCatalogueView {
        version: STOREFRONT_LAYOUT_VERSION,
        max_sections: domain::MAX_STOREFRONT_SECTIONS,
        types: storefront_section_catalogue(),
        presets,
        rail_sources: domain::RAIL_SOURCES
            .iter()
            .map(|source| (*source).to_string())
            .collect(),
        category_grid_modes: domain::CATEGORY_GRID_MODES
            .iter()
            .map(|mode| (*mode).to_string())
            .collect(),
    }))
}

// ---------------------------------------------------------------------------
// Section analytics (aggregate-only, the merch pattern)
// ---------------------------------------------------------------------------

/// Record one section event. The store must be active and
/// the section must exist in the store's CURRENT effective
/// layout (any state — a click on a just-disabled section
/// in an open tab is still a real event for a real
/// section; a made-up id is a 404, so the log can never be
/// padded with sections the storefront cannot show).
pub async fn record_section_event(
    pool: &PgPool,
    slug: &str,
    input: &SectionEventInput,
) -> Result<(), ApiError> {
    let store = get_store_by_slug(pool, slug).await?;
    if store.status != "active" {
        return Err(ApiError::not_found("store not found"));
    }
    let kind = validate_storefront_event_kind(&input.kind).map_err(ApiError::bad_request)?;
    domain::validate_section_id(&input.section_id).map_err(ApiError::bad_request)?;
    let (layout, _, _) = effective_layout(pool, store.id).await?;
    if !layout.sections.iter().any(|section| section.id == input.section_id) {
        return Err(ApiError::not_found("section not found"));
    }
    sqlx::query(
        "INSERT INTO storefront_events (store_id, section_id, kind) VALUES ($1, $2, $3)",
    )
    .bind(store.id)
    .bind(&input.section_id)
    .bind(kind)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("record storefront event", e))?;
    Ok(())
}

pub async fn record_section_event_handler(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Json(input): Json<SectionEventInput>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let pool = state.require_pool()?;
    record_section_event(pool, &slug, &input).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "recorded": true })),
    ))
}

/// The aggregate counts for one store's events, grouped
/// by section: totals, the current Dhaka day, and the
/// trailing window — the analytics module's day-boundary
/// SQL (Asia/Dhaka calendar days), so "today" means the
/// seller's today.
#[derive(Debug, FromRow)]
struct SectionEventCounts {
    section_id: String,
    views_total: i64,
    clicks_total: i64,
    views_today: i64,
    clicks_today: i64,
    views_window: i64,
    clicks_window: i64,
}

async fn load_section_event_counts(
    pool: &PgPool,
    store_id: Uuid,
) -> Result<Vec<SectionEventCounts>, ApiError> {
    sqlx::query_as(
        "SELECT section_id, \
         COUNT(*) FILTER (WHERE kind = 'section_view')::bigint AS views_total, \
         COUNT(*) FILTER (WHERE kind = 'section_click')::bigint AS clicks_total, \
         COUNT(*) FILTER (WHERE kind = 'section_view' AND created_at >= \
             (((now() AT TIME ZONE 'Asia/Dhaka')::date)::timestamp \
              AT TIME ZONE 'Asia/Dhaka'))::bigint AS views_today, \
         COUNT(*) FILTER (WHERE kind = 'section_click' AND created_at >= \
             (((now() AT TIME ZONE 'Asia/Dhaka')::date)::timestamp \
              AT TIME ZONE 'Asia/Dhaka'))::bigint AS clicks_today, \
         COUNT(*) FILTER (WHERE kind = 'section_view' AND created_at >= \
             (((now() AT TIME ZONE 'Asia/Dhaka')::date - (($2 - 1)::int))::timestamp \
              AT TIME ZONE 'Asia/Dhaka'))::bigint AS views_window, \
         COUNT(*) FILTER (WHERE kind = 'section_click' AND created_at >= \
             (((now() AT TIME ZONE 'Asia/Dhaka')::date - (($2 - 1)::int))::timestamp \
              AT TIME ZONE 'Asia/Dhaka'))::bigint AS clicks_window \
         FROM storefront_events WHERE store_id = $1 GROUP BY section_id",
    )
    .bind(store_id)
    .bind(STOREFRONT_STATS_WINDOW_DAYS)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load storefront event counts", e))
}

pub async fn seller_layout_stats_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<LayoutStatsView>, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_layout_store(pool, &user, &scope).await?;
    let (layout, _, _) = effective_layout(pool, store.id).await?;
    let counts = load_section_event_counts(pool, store.id).await?;
    let sections = layout
        .sections
        .iter()
        .map(|section| {
            let found = counts.iter().find(|row| row.section_id == section.id);
            SectionStats {
                section_id: section.id.clone(),
                section_type: section.section_type.clone(),
                views_total: found.map(|row| row.views_total).unwrap_or(0),
                clicks_total: found.map(|row| row.clicks_total).unwrap_or(0),
                views_today: found.map(|row| row.views_today).unwrap_or(0),
                clicks_today: found.map(|row| row.clicks_today).unwrap_or(0),
                views_window: found.map(|row| row.views_window).unwrap_or(0),
                clicks_window: found.map(|row| row.clicks_window).unwrap_or(0),
            }
        })
        .collect();
    Ok(Json(LayoutStatsView {
        store_id: store.id,
        window_days: STOREFRONT_STATS_WINDOW_DAYS,
        sections,
    }))
}
