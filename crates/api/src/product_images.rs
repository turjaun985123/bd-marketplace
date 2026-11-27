//! Product images (Wave 51): upload, attach-by-URL, gallery
//! management, the seller media library, store branding
//! uploads, and the local media serve route.
//!
//! The shape of the feature:
//!
//! - A product carries up to `MAX_IMAGES_PER_PRODUCT`
//!   images (migration 0040). Each is either an UPLOAD
//!   (bytes in the media store under a deterministic base
//!   key — see the `media` module) or an EXTERNAL hotlink
//!   (Facebook import photos, URL-based sellers). Exactly
//!   one is primary whenever any exist; the domain's
//!   `normalize_image_order` is the one rule that keeps
//!   positions dense and the primary unique, and every
//!   write path here (first upload, batch reorder, delete
//!   promotion) runs through it.
//! - Seller writes sit behind the store access helpers
//!   with the `manage_catalog` action, exactly like the
//!   variants endpoints (the closest sibling): a product
//!   that is missing OR belongs to a store the caller has
//!   no relationship with is the same 404, and a member
//!   whose role lacks the action gets the matrix's 403.
//! - Image failures never hide: undecodable bytes are a
//!   bilingual 400, the per-product cap is a bilingual 400,
//!   and an unconfigured S3 store answers its typed
//!   `media_not_configured` 503 from the store layer.
//!
//! The public side needs no endpoints of its own: product
//! views carry `image_url` (see `products::PRODUCT_COLUMNS`)
//! and the product detail carries the full `images` gallery
//! built by [`load_product_gallery`].

use std::collections::HashMap;

use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    normalize_image_order, validate_alt_text, validate_external_image_url, validate_media_key,
    ImageOrderItem, CARD_IMAGE_WIDTH, GALLERY_IMAGE_WIDTH, MAX_IMAGES_PER_PRODUCT,
    ZOOM_IMAGE_WIDTH,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::media::{self, MediaStore, ProcessedImage};
use crate::products::{get_product_by_id, Product};
use crate::stores::{get_store_by_id, Store};
use crate::{ApiError, AppState};

/// The multipart route's body limit (Wave 51): the 10 MiB
/// file cap plus room for the form fields and multipart
/// framing. Layered on the upload routes ONLY — the global
/// 1 MiB limit (Wave 28) still guards everything else.
pub const IMAGE_UPLOAD_ROUTE_LIMIT_BYTES: usize = 12 * 1024 * 1024;

/// How many image ids one media bulk-delete may carry.
pub const MAX_BULK_DELETE_IDS: usize = 50;

// ---------------------------------------------------------------------------
// Rows and views
// ---------------------------------------------------------------------------

/// One `product_images` row, as stored.
#[derive(Debug, Clone, FromRow)]
pub struct ProductImageRow {
    pub id: Uuid,
    pub product_id: Uuid,
    pub variant_id: Option<Uuid>,
    pub storage_key: Option<String>,
    pub external_url: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub size_bytes: Option<i64>,
    pub position: i32,
    pub is_primary: bool,
    pub alt_text: Option<String>,
    /// SHA-256 of the uploaded bytes (Wave 51 extension,
    /// migration 0042); null for external images.
    pub content_hash: Option<String>,
    /// The name the uploaded file arrived with (Wave 51
    /// extension, migration 0043); null for external
    /// images and pre-migration rows.
    pub original_filename: Option<String>,
    pub created_at: DateTime<Utc>,
}

const IMAGE_COLUMNS: &str = "id, product_id, variant_id, storage_key, external_url, \
    width, height, size_bytes, position, is_primary, alt_text, content_hash, \
    original_filename, created_at";

/// The four display URLs of one image: card (300-class),
/// gallery (600-class), zoom (1200-class) and the original.
/// For an external image all four are the external URL —
/// the platform holds no bytes to derive from.
#[derive(Debug, Clone, Serialize)]
pub struct ImageUrls {
    pub card: String,
    pub gallery: String,
    pub zoom: String,
    pub original: String,
}

/// Build the display URLs for one image row — THE url
/// rule for seller views, the public gallery and the media
/// library alike (via the `media` module's sized URLs).
pub fn image_urls(row: &ProductImageRow) -> ImageUrls {
    if let Some(url) = &row.external_url {
        return ImageUrls {
            card: url.clone(),
            gallery: url.clone(),
            zoom: url.clone(),
            original: url.clone(),
        };
    }
    let key = row.storage_key.as_deref().unwrap_or_default();
    ImageUrls {
        card: media::sized_url(key, row.width, CARD_IMAGE_WIDTH),
        gallery: media::sized_url(key, row.width, GALLERY_IMAGE_WIDTH),
        zoom: media::sized_url(key, row.width, ZOOM_IMAGE_WIDTH),
        original: media::public_url_for_key(&media::original_key(key)),
    }
}

/// The full seller-facing image shape: the row plus its
/// four display URLs and its source (`upload` / `external`).
#[derive(Debug, Clone, Serialize)]
pub struct ProductImageView {
    pub id: Uuid,
    pub product_id: Uuid,
    pub variant_id: Option<Uuid>,
    pub source: String,
    pub url_card: String,
    pub url_gallery: String,
    pub url_zoom: String,
    pub url_original: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub size_bytes: Option<i64>,
    pub position: i32,
    pub is_primary: bool,
    pub alt_text: Option<String>,
    /// The uploaded file's original name, when known.
    pub original_filename: Option<String>,
    /// Quality warning codes for stored images (Wave 51
    /// extension; see the domain rule) — empty for
    /// external images and healthy photos.
    pub warnings: Vec<&'static str>,
    pub created_at: DateTime<Utc>,
}

impl From<&ProductImageRow> for ProductImageView {
    fn from(row: &ProductImageRow) -> Self {
        let urls = image_urls(row);
        ProductImageView {
            id: row.id,
            product_id: row.product_id,
            variant_id: row.variant_id,
            source: if row.external_url.is_some() {
                "external".to_string()
            } else {
                "upload".to_string()
            },
            url_card: urls.card,
            url_gallery: urls.gallery,
            url_zoom: urls.zoom,
            url_original: urls.original,
            width: row.width,
            height: row.height,
            size_bytes: row.size_bytes,
            position: row.position,
            is_primary: row.is_primary,
            alt_text: row.alt_text.clone(),
            original_filename: row.original_filename.clone(),
            warnings: if row.external_url.is_some() {
                Vec::new()
            } else {
                domain::image_quality_warnings(row.width, row.height)
            },
            created_at: row.created_at,
        }
    }
}

/// One image in the PUBLIC product gallery (the product
/// detail's `images` array): display URLs and placement
/// facts only — no storage internals, no byte counts.
#[derive(Debug, Clone, Serialize)]
pub struct GalleryImageView {
    pub id: Uuid,
    pub url_card: String,
    pub url_gallery: String,
    pub url_zoom: String,
    pub alt_text: Option<String>,
    pub variant_id: Option<Uuid>,
    pub is_primary: bool,
}

impl From<&ProductImageRow> for GalleryImageView {
    fn from(row: &ProductImageRow) -> Self {
        let urls = image_urls(row);
        GalleryImageView {
            id: row.id,
            url_card: urls.card,
            url_gallery: urls.gallery,
            url_zoom: urls.zoom,
            alt_text: row.alt_text.clone(),
            variant_id: row.variant_id,
            is_primary: row.is_primary,
        }
    }
}

/// One image in the seller media library (extra surface of
/// Wave 51): the full image view plus which product and
/// store it belongs to, so the library can group and link.
#[derive(Debug, Serialize)]
pub struct SellerMediaItem {
    #[serde(flatten)]
    pub image: ProductImageView,
    pub product_title: String,
    pub store_id: Uuid,
}

/// One page of the seller media library.
#[derive(Debug, Serialize)]
pub struct SellerMediaList {
    pub items: Vec<SellerMediaItem>,
    pub total: i64,
    /// Uploaded bytes across the caller's accessible
    /// stores (Wave 51 extension) — the usage figure the
    /// library page shows against the per-store quota.
    pub storage_bytes: i64,
    /// The per-store media quota (domain constant).
    pub storage_quota_bytes: i64,
}

// ---------------------------------------------------------------------------
// Access + loaders
// ---------------------------------------------------------------------------

/// Resolve a product the caller may work for the image
/// endpoints (the variants module's rule, shared verbatim
/// in spirit): the product must exist and the caller must
/// hold a store role carrying `manage_catalog`; no
/// relationship is the same 404 as a missing product.
async fn catalog_product(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
) -> Result<Product, ApiError> {
    let product = get_product_by_id(pool, product_id).await?;
    match crate::access::store_role(pool, user.user_id, product.store_id).await? {
        Some(role) => {
            crate::access::require_role(&role, domain::STORE_ACTION_MANAGE_CATALOG)?;
        }
        None => return Err(ApiError::not_found("product not found")),
    }
    Ok(product)
}

/// Every image row of one product, in gallery order
/// (position, then creation — positions are kept dense by
/// the write paths, the second key only orders legacy ties).
pub async fn load_image_rows(
    pool: &PgPool,
    product_id: Uuid,
) -> Result<Vec<ProductImageRow>, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {IMAGE_COLUMNS} FROM product_images \
         WHERE product_id = $1 ORDER BY position ASC, created_at ASC"
    ))
    .bind(product_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load product images", e))
}

/// The public gallery of one product — the product
/// detail's `images` array, in display order.
pub async fn load_product_gallery(
    pool: &PgPool,
    product_id: Uuid,
) -> Result<Vec<GalleryImageView>, ApiError> {
    let rows = load_image_rows(pool, product_id).await?;
    Ok(rows.iter().map(GalleryImageView::from).collect())
}

/// The display URL of the first image assigned to one
/// variant (primary first), or `None` when the variant has
/// no image of its own — the variant views' `image_url`.
pub async fn image_url_for_variant(
    pool: &PgPool,
    variant_id: Uuid,
) -> Result<Option<String>, ApiError> {
    let row: Option<ProductImageRow> = sqlx::query_as(&format!(
        "SELECT {IMAGE_COLUMNS} FROM product_images \
         WHERE variant_id = $1 ORDER BY is_primary DESC, position ASC, created_at ASC LIMIT 1"
    ))
    .bind(variant_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load variant image", e))?;
    Ok(row.as_ref().map(|row| image_urls(row).gallery))
}

/// Every variant-assigned image of one product, as a
/// variant-id → display-URL map (the first image per
/// variant, primary first) — the batch form
/// [`image_url_for_variant`]'s rule, for list views that
/// render many variants at once.
pub async fn image_urls_by_variant(
    pool: &PgPool,
    product_id: Uuid,
) -> Result<HashMap<Uuid, String>, ApiError> {
    let rows: Vec<ProductImageRow> = sqlx::query_as(&format!(
        "SELECT {IMAGE_COLUMNS} FROM product_images \
         WHERE product_id = $1 AND variant_id IS NOT NULL \
         ORDER BY is_primary DESC, position ASC, created_at ASC"
    ))
    .bind(product_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load variant images", e))?;
    let mut urls: HashMap<Uuid, String> = HashMap::new();
    for row in &rows {
        if let Some(variant_id) = row.variant_id {
            urls.entry(variant_id)
                .or_insert_with(|| image_urls(row).gallery);
        }
    }
    Ok(urls)
}

/// The primary-image URL of each of many products in ONE
/// query (Wave 51 extension) — the batch fill behind the
/// order / invoice line thumbnails, where a view holds
/// items from many products and a per-item lookup would
/// be an N+1. Products without images are absent from the
/// map (their lines render the placeholder).
pub async fn primary_image_urls(
    pool: &PgPool,
    product_ids: &[Uuid],
) -> Result<HashMap<Uuid, String>, ApiError> {
    if product_ids.is_empty() {
        return Ok(HashMap::new());
    }
    #[derive(FromRow)]
    struct PrimaryRow {
        product_id: Uuid,
        storage_key: Option<String>,
        external_url: Option<String>,
        width: Option<i32>,
    }
    let rows: Vec<PrimaryRow> = sqlx::query_as(
        "SELECT DISTINCT ON (product_id) product_id, storage_key, external_url, width \
         FROM product_images WHERE product_id = ANY($1) \
         ORDER BY product_id, is_primary DESC, position ASC, created_at ASC",
    )
    .bind(product_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load primary image urls", e))?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            media::product_image_url(
                row.storage_key.as_deref(),
                row.external_url.as_deref(),
                row.width,
            )
            .map(|url| (row.product_id, url))
        })
        .collect())
}

/// Check an optional variant assignment: a named variant
/// must belong to THIS product (a variant id from another
/// product is a 400, never a cross-product link).
async fn require_variant_of_product(
    pool: &PgPool,
    product_id: Uuid,
    variant_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let Some(variant_id) = variant_id else {
        return Ok(());
    };
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM product_variants WHERE id = $1 AND product_id = $2)",
    )
    .bind(variant_id)
    .bind(product_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("check image variant", e))?;
    if !exists {
        return Err(ApiError::bad_request(
            "the chosen variant does not belong to this product — \
             নির্বাচিত ভ্যারিয়েন্টটি এই পণ্যের নয়",
        ));
    }
    Ok(())
}

/// The platform setting key for the per-product image cap.
pub const MEDIA_MAX_IMAGES_SETTING_KEY: &str = "media_max_images_per_product";
/// The platform setting key for the per-store media quota (MB).
pub const MEDIA_STORE_QUOTA_MB_SETTING_KEY: &str = "media_store_quota_mb";

/// The media limits in force (Wave 51 extension): the
/// per-product image cap and the per-store quota, read
/// from the platform settings when an admin has set them
/// (validated by the domain rules — a stored value outside
/// the legal range is ignored), else the domain defaults.
/// The `admin::platform_setting_i64` + `effective_*`
/// pattern the SLA and commission limits already use.
#[derive(Debug, Clone, Copy)]
pub struct MediaLimits {
    pub max_images_per_product: i64,
    pub store_quota_bytes: i64,
}

pub async fn effective_media_limits(pool: &PgPool) -> MediaLimits {
    let mut limits = MediaLimits {
        max_images_per_product: MAX_IMAGES_PER_PRODUCT,
        store_quota_bytes: domain::MAX_STORE_MEDIA_BYTES,
    };
    if let Some(value) =
        crate::admin::platform_setting_i64(pool, MEDIA_MAX_IMAGES_SETTING_KEY).await
    {
        if domain::validate_media_max_images(value) {
            limits.max_images_per_product = value;
        }
    }
    if let Some(value) =
        crate::admin::platform_setting_i64(pool, MEDIA_STORE_QUOTA_MB_SETTING_KEY).await
    {
        if domain::validate_media_store_quota_mb(value) {
            limits.store_quota_bytes = value * 1024 * 1024;
        }
    }
    limits
}

/// The per-product cap gate shared by the upload and the
/// URL-attach paths: the current row count must leave room
/// for one more under the effective cap.
async fn require_image_capacity(
    pool: &PgPool,
    product_id: Uuid,
    max_images: i64,
) -> Result<i64, ApiError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM product_images WHERE product_id = $1")
        .bind(product_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count product images", e))?;
    if count >= max_images {
        return Err(ApiError::bad_request(format!(
            "a product may carry at most {max_images} images — \
             একটি পণ্যে সর্বোচ্চ {max_images}টি ছবি রাখা যাবে"
        )));
    }
    Ok(count)
}

/// Rewrite a product's image layout from the domain
/// normalisation of its CURRENT rows (the delete path's
/// promotion rule): positions dense from 0, exactly one
/// primary. Runs inside the caller's transaction.
async fn renormalize_product_images(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    product_id: Uuid,
) -> Result<(), ApiError> {
    let rows: Vec<ProductImageRow> = sqlx::query_as(&format!(
        "SELECT {IMAGE_COLUMNS} FROM product_images \
         WHERE product_id = $1 ORDER BY position ASC, created_at ASC FOR UPDATE"
    ))
    .bind(product_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| ApiError::internal("load images for renormalise", e))?;
    let items: Vec<ImageOrderItem<Uuid>> = rows
        .iter()
        .map(|row| ImageOrderItem {
            key: row.id,
            position: row.position,
            is_primary: row.is_primary,
        })
        .collect();
    for item in normalize_image_order(&items) {
        sqlx::query(
            "UPDATE product_images SET position = $2, is_primary = $3 \
             WHERE id = $1 AND product_id = $4",
        )
        .bind(item.key)
        .bind(item.position)
        .bind(item.is_primary)
        .bind(product_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| ApiError::internal("renormalise product images", e))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Service functions — seller image management
// ---------------------------------------------------------------------------

/// List one product's images for its seller (the
/// management view behind the uploader component).
pub async fn list_product_images(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
) -> Result<Vec<ProductImageView>, ApiError> {
    catalog_product(pool, user, product_id).await?;
    let rows = load_image_rows(pool, product_id).await?;
    Ok(rows.iter().map(ProductImageView::from).collect())
}

/// One store's current media usage (Wave 51 extension):
/// the summed `size_bytes` of its products' UPLOADED
/// images (external hotlinks hold no bytes and never
/// count). THE figure the quota gate and the media
/// library's usage display both read.
pub async fn store_media_usage(pool: &PgPool, store_id: Uuid) -> Result<i64, ApiError> {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(pi.size_bytes), 0)::bigint \
         FROM product_images pi JOIN products p ON p.id = pi.product_id \
         WHERE p.store_id = $1 AND pi.storage_key IS NOT NULL",
    )
    .bind(store_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("store media usage", e))
}

/// THE tail of every upload-shaped write (Wave 51 +
/// extension): the duplicate guard, the store quota gate,
/// the per-product cap, the object writes, then the row —
/// in that order, so a refusal leaves nothing behind but
/// (on a store-write failure part-way) inert orphaned
/// objects no row references, which the orphan reaper
/// collects. The product's FIRST image becomes its
/// primary; later uploads append. Shared by the single
/// upload, the bulk upload and the from-web fetch so the
/// three can never drift apart.
async fn insert_uploaded_image(
    state: &AppState,
    product: &Product,
    image_id: Uuid,
    processed: &ProcessedImage,
    alt_text: Option<String>,
    variant_id: Option<Uuid>,
    original_filename: Option<String>,
) -> Result<ProductImageView, ApiError> {
    let pool = state.require_pool()?;
    let product_id = product.id;

    // The duplicate guard: the same bytes already on THIS
    // product are a seller mistake, refused bilingually —
    // the gallery would show the photo twice and the
    // store would pay storage for nothing.
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM product_images \
         WHERE product_id = $1 AND content_hash = $2)",
    )
    .bind(product_id)
    .bind(&processed.content_hash)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("check duplicate image", e))?;
    if duplicate {
        return Err(ApiError::bad_request(
            "this exact image is already on the product — \
             এই একই ছবি পণ্যে আগেই আছে",
        ));
    }

    // The store quota gate (domain rule over live usage,
    // against the effective — possibly admin-set — cap).
    let limits = effective_media_limits(pool).await;
    let usage = store_media_usage(pool, product.store_id).await?;
    if !domain::within_media_quota(usage, processed.size_bytes, limits.store_quota_bytes) {
        return Err(ApiError::bad_request(format!(
            "the store's media quota is full ({} MB) — delete some images first — \
             স্টোরের ছবির কোটা পূর্ণ ({} MB), আগে কিছু ছবি মুছুন",
            limits.store_quota_bytes / (1024 * 1024),
            limits.store_quota_bytes / (1024 * 1024)
        )));
    }

    let count = require_image_capacity(pool, product_id, limits.max_images_per_product).await?;
    let base_key = format!("products/{product_id}/{image_id}");
    media::store_processed(state.media_store.as_ref(), processed).await?;

    // A blank alt falls back to the filename suggestion
    // (Wave 51 extension): the seller can always edit it
    // afterwards, and an image with SOME alt beats one
    // with none for screen readers and search.
    let alt_text = alt_text.or_else(|| {
        original_filename
            .as_deref()
            .and_then(domain::suggest_alt_text)
    });
    let row: ProductImageRow = sqlx::query_as(&format!(
        "INSERT INTO product_images \
         (id, product_id, variant_id, storage_key, width, height, size_bytes, \
          position, is_primary, alt_text, content_hash, original_filename) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING {IMAGE_COLUMNS}"
    ))
    .bind(image_id)
    .bind(product_id)
    .bind(variant_id)
    .bind(&base_key)
    .bind(processed.width as i32)
    .bind(processed.height as i32)
    .bind(processed.size_bytes)
    .bind(count as i32)
    .bind(count == 0)
    .bind(&alt_text)
    .bind(&processed.content_hash)
    .bind(&original_filename)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("insert product image", e))?;
    Ok(ProductImageView::from(&row))
}

/// The inputs of one image upload (Wave 51): the bytes
/// plus everything the form (or the fetch) said about
/// them — one struct so the single, bulk and from-web
/// paths all call the upload with the same shape.
pub struct ImageUploadInput<'a> {
    pub bytes: &'a [u8],
    pub declared_content_type: Option<&'a str>,
    pub alt_text: Option<&'a str>,
    pub variant_id: Option<Uuid>,
    pub original_filename: Option<&'a str>,
}

/// Upload one image to a product (Wave 51): access,
/// variant, alt and filename validation, the shared
/// pipeline under the new image's real base key, then the
/// shared insert tail ([`insert_uploaded_image`]).
pub async fn upload_product_image(
    state: &AppState,
    user: &AuthUser,
    product_id: Uuid,
    input: ImageUploadInput<'_>,
) -> Result<ProductImageView, ApiError> {
    let pool = state.require_pool()?;
    let product = catalog_product(pool, user, product_id).await?;
    require_variant_of_product(pool, product_id, input.variant_id).await?;
    let alt_text = validate_alt_text(input.alt_text).map_err(ApiError::bad_request)?;
    let original_filename =
        domain::validate_original_filename(input.original_filename).map_err(ApiError::bad_request)?;
    let image_id = Uuid::new_v4();
    let base_key = format!("products/{product_id}/{image_id}");
    let processed = media::process_upload(&base_key, input.bytes, input.declared_content_type)?;
    insert_uploaded_image(
        state,
        &product,
        image_id,
        &processed,
        alt_text,
        input.variant_id,
        original_filename,
    )
    .await
}

/// Attach an external image URL to a product WITHOUT any
/// access check — the internal form the Facebook import
/// uses after its own authorisation (the import acts on
/// the seller's own store by construction). Validation and
/// the cap still apply; callers that cannot surface an
/// error to a user (the import) log the failure and move
/// on, per the import's never-fail-the-batch rule.
pub async fn attach_external_image(
    pool: &PgPool,
    product_id: Uuid,
    url: &str,
    alt_text: Option<&str>,
    variant_id: Option<Uuid>,
) -> Result<ProductImageView, ApiError> {
    let url = validate_external_image_url(url).map_err(ApiError::bad_request)?;
    require_variant_of_product(pool, product_id, variant_id).await?;
    let alt_text = validate_alt_text(alt_text).map_err(ApiError::bad_request)?;
    let limits = effective_media_limits(pool).await;
    let count = require_image_capacity(pool, product_id, limits.max_images_per_product).await?;
    let row: ProductImageRow = sqlx::query_as(&format!(
        "INSERT INTO product_images \
         (product_id, variant_id, external_url, position, is_primary, alt_text) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {IMAGE_COLUMNS}"
    ))
    .bind(product_id)
    .bind(variant_id)
    .bind(&url)
    .bind(count as i32)
    .bind(count == 0)
    .bind(&alt_text)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("attach external product image", e))?;
    Ok(ProductImageView::from(&row))
}

/// Attach an external image URL to a product for its
/// seller (the from-url endpoint): the access-checked form
/// of [`attach_external_image`].
pub async fn attach_product_image_url(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    url: &str,
    alt_text: Option<&str>,
    variant_id: Option<Uuid>,
) -> Result<ProductImageView, ApiError> {
    catalog_product(pool, user, product_id).await?;
    attach_external_image(pool, product_id, url, alt_text, variant_id).await
}

/// One entry of the batch layout update: the image, its
/// requested position and primary flag, and the optional
/// field edits. `alt_text` / `variant_id` are
/// double-optional on purpose: absent leaves the stored
/// value, explicit null clears it, a value sets it.
#[derive(Debug, Deserialize)]
pub struct ImagePlacementInput {
    pub id: Uuid,
    pub position: i32,
    #[serde(default)]
    pub is_primary: bool,
    #[serde(default)]
    pub alt_text: Option<Option<String>>,
    #[serde(default)]
    pub variant_id: Option<Option<Uuid>>,
}

/// Apply a batch layout update to a product's images in
/// ONE transaction (Wave 51): the batch names the images
/// it reorders / re-flags / edits; images it does not name
/// keep their stored placement and join the same
/// normalisation (so the primary rule holds over the whole
/// gallery, not just the batch). Primary flagging is
/// opt-in: a batch that flags an item makes the batch's
/// flags authoritative, while a batch that flags nothing
/// (a pure reorder / alt-text edit) preserves the stored
/// primary. Unknown image ids are a 400 — a layout that
/// names another product's image must never half-apply.
/// Returns the full updated list.
pub async fn update_product_images(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    items: &[ImagePlacementInput],
) -> Result<Vec<ProductImageView>, ApiError> {
    catalog_product(pool, user, product_id).await?;
    if items.is_empty() {
        return Err(ApiError::bad_request("the update names no images"));
    }
    // Validate every edit BEFORE the transaction opens:
    // alt texts through the domain rule, variant ids
    // against this product.
    for item in items {
        if let Some(alt) = &item.alt_text {
            validate_alt_text(alt.as_deref()).map_err(ApiError::bad_request)?;
        }
        if let Some(variant_id) = &item.variant_id {
            require_variant_of_product(pool, product_id, *variant_id).await?;
        }
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin image update", e))?;
    let rows: Vec<ProductImageRow> = sqlx::query_as(&format!(
        "SELECT {IMAGE_COLUMNS} FROM product_images \
         WHERE product_id = $1 ORDER BY position ASC, created_at ASC FOR UPDATE"
    ))
    .bind(product_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("load images for update", e))?;

    let known: std::collections::HashSet<Uuid> = rows.iter().map(|row| row.id).collect();
    for item in items {
        if !known.contains(&item.id) {
            return Err(ApiError::bad_request(format!(
                "image {} does not belong to this product",
                item.id
            )));
        }
    }
    let requested: HashMap<Uuid, &ImagePlacementInput> =
        items.iter().map(|item| (item.id, item)).collect();
    let stored: HashMap<Uuid, &ProductImageRow> =
        rows.iter().map(|row| (row.id, row)).collect();

    // The primary rule for a batch: flagging is OPT-IN. If
    // any batch item carries `is_primary`, the batch's
    // flags rule (an unflagged batch item gives up a stored
    // primary). If NO batch item is flagged, every image
    // keeps its stored flag — an alt-text-only edit must
    // never silently dethrone the primary.
    let any_flagged = items.iter().any(|item| item.is_primary);

    // The normalisation input: batch items first (in the
    // caller's order, with their requested placement), then
    // the unnamed images in their stored order — the stable
    // sort keeps that priority on position ties.
    let mut order_items: Vec<ImageOrderItem<Uuid>> = Vec::with_capacity(rows.len());
    for item in items {
        order_items.push(ImageOrderItem {
            key: item.id,
            position: item.position,
            is_primary: if any_flagged {
                item.is_primary
            } else {
                stored[&item.id].is_primary
            },
        });
    }
    for row in &rows {
        if !requested.contains_key(&row.id) {
            order_items.push(ImageOrderItem {
                key: row.id,
                position: row.position,
                is_primary: row.is_primary,
            });
        }
    }
    let normalized = normalize_image_order(&order_items);
    let placement: HashMap<Uuid, (i32, bool)> = normalized
        .iter()
        .map(|item| (item.key, (item.position, item.is_primary)))
        .collect();

    for row in &rows {
        let (position, is_primary) = placement[&row.id];
        let edit = requested.get(&row.id);
        let alt_text = match edit.and_then(|item| item.alt_text.as_ref()) {
            Some(value) => validate_alt_text(value.as_deref())
                .map_err(ApiError::bad_request)?,
            None => row.alt_text.clone(),
        };
        let variant_id = match edit.and_then(|item| item.variant_id.as_ref()) {
            Some(value) => *value,
            None => row.variant_id,
        };
        sqlx::query(
            "UPDATE product_images SET position = $2, is_primary = $3, \
             alt_text = $4, variant_id = $5 WHERE id = $1",
        )
        .bind(row.id)
        .bind(position)
        .bind(is_primary)
        .bind(&alt_text)
        .bind(variant_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("update product image", e))?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit image update", e))?;

    let rows = load_image_rows(pool, product_id).await?;
    Ok(rows.iter().map(ProductImageView::from).collect())
}

/// Delete one image of a product (Wave 51): the row goes
/// in a transaction that renormalises the survivors (so
/// deleting the primary promotes the next by position —
/// the domain rule), and the stored objects are removed
/// after the commit (the store is not transactional; an
/// object whose row is gone is inert, while a row whose
/// objects are gone would be a broken storefront).
/// Returns the full remaining list.
pub async fn delete_product_image(
    state: &AppState,
    user: &AuthUser,
    product_id: Uuid,
    image_id: Uuid,
) -> Result<Vec<ProductImageView>, ApiError> {
    let pool = state.require_pool()?;
    catalog_product(pool, user, product_id).await?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin image delete", e))?;
    let row: Option<ProductImageRow> = sqlx::query_as(&format!(
        "SELECT {IMAGE_COLUMNS} FROM product_images \
         WHERE id = $1 AND product_id = $2 FOR UPDATE"
    ))
    .bind(image_id)
    .bind(product_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("load image for delete", e))?;
    let row = row.ok_or_else(|| ApiError::not_found("image not found"))?;
    sqlx::query("DELETE FROM product_images WHERE id = $1")
        .bind(image_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("delete product image", e))?;
    renormalize_product_images(&mut tx, product_id).await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit image delete", e))?;

    if let Some(key) = &row.storage_key {
        media::delete_stored_image(state.media_store.as_ref(), key, row.width).await;
    }
    let rows = load_image_rows(pool, product_id).await?;
    Ok(rows.iter().map(ProductImageView::from).collect())
}

// ---------------------------------------------------------------------------
// Service functions — seller media library
// ---------------------------------------------------------------------------

/// The seller media library (Wave 51): every image across
/// every store the caller may work the catalogue of,
/// newest first, optionally narrowed to one product. The
/// scope is `accessible_store_ids` under `manage_catalog`
/// — the same scope the cross-store seller lists use.
pub async fn list_seller_media(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Option<Uuid>,
    source: Option<&str>,
    sort: Option<&str>,
) -> Result<SellerMediaList, ApiError> {
    // The source filter's two legal values (Wave 51
    // extension); anything else is a 400, never a silent
    // empty list.
    let source_upload: Option<bool> = match source {
        None => None,
        Some("upload") => Some(true),
        Some("external") => Some(false),
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "unknown media source filter '{other}' (upload | external)"
            )))
        }
    };
    // The sort orders the LIBRARY grid only (never a
    // product's gallery order): newest first by default,
    // oldest, or largest stored file first — the cleanup
    // view. Unknown values are a 400, like the filter.
    let order_by = match sort {
        None | Some("newest") => "pi.created_at DESC",
        Some("oldest") => "pi.created_at ASC",
        Some("largest") => "pi.size_bytes DESC NULLS LAST, pi.created_at DESC",
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "unknown media sort '{other}' (newest | oldest | largest)"
            )))
        }
    };
    let store_ids = crate::access::accessible_store_ids(
        pool,
        user.user_id,
        domain::STORE_ACTION_MANAGE_CATALOG,
    )
    .await?;
    #[derive(FromRow)]
    struct MediaRow {
        #[sqlx(flatten)]
        image: ProductImageRow,
        product_title: String,
        store_id: Uuid,
    }
    let rows: Vec<MediaRow> = sqlx::query_as(&format!(
        "SELECT pi.id, pi.product_id, pi.variant_id, pi.storage_key, pi.external_url, \
         pi.width, pi.height, pi.size_bytes, pi.position, pi.is_primary, pi.alt_text, \
         pi.content_hash, pi.original_filename, pi.created_at, p.title AS product_title, p.store_id \
         FROM product_images pi JOIN products p ON p.id = pi.product_id \
         WHERE p.store_id = ANY($1) AND ($2::uuid IS NULL OR pi.product_id = $2) \
         AND ($3::boolean IS NULL OR (pi.storage_key IS NOT NULL) = $3) \
         ORDER BY {order_by} LIMIT 500"
    ))
    .bind(&store_ids)
    .bind(product_id)
    .bind(source_upload)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list seller media", e))?;
    let total = rows.len() as i64;
    let storage_bytes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(pi.size_bytes), 0)::bigint \
         FROM product_images pi JOIN products p ON p.id = pi.product_id \
         WHERE p.store_id = ANY($1) AND pi.storage_key IS NOT NULL",
    )
    .bind(&store_ids)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("seller media usage", e))?;
    Ok(SellerMediaList {
        items: rows
            .into_iter()
            .map(|row| SellerMediaItem {
                image: ProductImageView::from(&row.image),
                product_title: row.product_title,
                store_id: row.store_id,
            })
            .collect(),
        total,
        storage_bytes,
        storage_quota_bytes: effective_media_limits(pool).await.store_quota_bytes,
    })
}

/// The bulk-delete answer: how many of the named images
/// were deleted, and how many were skipped (unknown ids,
/// or images in stores the caller may not work — skipped
/// silently by design, so the response never confirms
/// another seller's image exists).
#[derive(Debug, Serialize)]
pub struct BulkDeleteResult {
    pub deleted: i64,
    pub skipped: i64,
}

/// Delete a batch of images across the caller's stores
/// (Wave 51 media library): each id is resolved on its
/// own — access-checked against its product's store,
/// deleted with the same renormalise-then-remove-objects
/// discipline as the single delete. One bad id never
/// vetoes the batch.
pub async fn bulk_delete_seller_media(
    state: &AppState,
    user: &AuthUser,
    image_ids: &[Uuid],
) -> Result<BulkDeleteResult, ApiError> {
    let pool = state.require_pool()?;
    if image_ids.len() > MAX_BULK_DELETE_IDS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_BULK_DELETE_IDS} images per bulk delete"
        )));
    }
    let mut deleted: i64 = 0;
    let mut skipped: i64 = 0;
    for image_id in image_ids {
        let product_id: Option<Uuid> =
            sqlx::query_scalar("SELECT product_id FROM product_images WHERE id = $1")
                .bind(image_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| ApiError::internal("resolve media image", e))?;
        let Some(product_id) = product_id else {
            skipped += 1;
            continue;
        };
        // The single-delete path re-checks access; its 404
        // (no relationship) is this batch's "skip".
        match delete_product_image(state, user, product_id, *image_id).await {
            Ok(_) => deleted += 1,
            Err(err) if err.status == StatusCode::NOT_FOUND => skipped += 1,
            Err(err) => return Err(err),
        }
    }
    Ok(BulkDeleteResult { deleted, skipped })
}

// ---------------------------------------------------------------------------
// Service functions — replace, bulk, fetch-from-web (Wave 51 extension)
// ---------------------------------------------------------------------------

/// Replace one uploaded image's BYTES in place (Wave 51
/// extension): the row keeps its id, position, primary
/// flag, alt text and variant assignment — only the photo
/// changes (the retake-the-photo workflow). External
/// images have no stored bytes and refuse (a 400 — change
/// a hotlink by deleting and re-attaching). The old
/// objects are deleted BEFORE the new ones are written
/// (same keys), and the duplicate guard + quota apply
/// with the replaced image's own bytes netted out.
pub async fn replace_product_image(
    state: &AppState,
    user: &AuthUser,
    product_id: Uuid,
    image_id: Uuid,
    bytes: &[u8],
    declared_content_type: Option<&str>,
    original_filename: Option<&str>,
) -> Result<ProductImageView, ApiError> {
    let pool = state.require_pool()?;
    let product = catalog_product(pool, user, product_id).await?;
    let existing: Option<ProductImageRow> = sqlx::query_as(&format!(
        "SELECT {IMAGE_COLUMNS} FROM product_images WHERE id = $1 AND product_id = $2"
    ))
    .bind(image_id)
    .bind(product_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load image for replace", e))?;
    let existing = existing.ok_or_else(|| ApiError::not_found("image not found"))?;
    let Some(base_key) = existing.storage_key.clone() else {
        return Err(ApiError::bad_request(
            "an external (URL) image has no stored photo to replace — \
             delete it and attach the new URL instead — \
             বাইরের URL ছবি বদলানো যায় না, মুছে নতুন URL যোগ করুন",
        ));
    };
    let processed = media::process_upload(&base_key, bytes, declared_content_type)?;

    // Duplicate guard, excluding the image being replaced.
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM product_images \
         WHERE product_id = $1 AND content_hash = $2 AND id <> $3)",
    )
    .bind(product_id)
    .bind(&processed.content_hash)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("check duplicate image", e))?;
    if duplicate {
        return Err(ApiError::bad_request(
            "this exact image is already on the product — \
             এই একই ছবি পণ্যে আগেই আছে",
        ));
    }
    // Quota with the old photo's bytes netted out.
    let limits = effective_media_limits(pool).await;
    let usage = store_media_usage(pool, product.store_id).await?;
    let net_current = usage - existing.size_bytes.unwrap_or(0);
    if !domain::within_media_quota(net_current, processed.size_bytes, limits.store_quota_bytes) {
        return Err(ApiError::bad_request(
            "the store's media quota is full — delete some images first — \
             স্টোরের ছবির কোটা পূর্ণ, আগে কিছু ছবি মুছুন",
        ));
    }

    media::delete_stored_image(state.media_store.as_ref(), &base_key, existing.width).await;
    media::store_processed(state.media_store.as_ref(), &processed).await?;
    // The replacement file's name replaces the old one
    // when it sent a usable name; otherwise the stored
    // name stays (the field is metadata, not identity).
    let original_filename =
        domain::validate_original_filename(original_filename).map_err(ApiError::bad_request)?;
    let row: ProductImageRow = sqlx::query_as(&format!(
        "UPDATE product_images SET width = $2, height = $3, size_bytes = $4, \
         content_hash = $5, original_filename = COALESCE($6, original_filename) \
         WHERE id = $1 RETURNING {IMAGE_COLUMNS}"
    ))
    .bind(image_id)
    .bind(processed.width as i32)
    .bind(processed.height as i32)
    .bind(processed.size_bytes)
    .bind(&processed.content_hash)
    .bind(&original_filename)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("update replaced image", e))?;
    Ok(ProductImageView::from(&row))
}

/// One file of a bulk upload, as read from the form.
pub struct BulkUploadFile {
    pub filename: String,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

/// One failed file of a bulk upload: its name and the
/// refusal it met (the batch continues past it).
#[derive(Debug, Serialize)]
pub struct BulkUploadFailure {
    pub filename: String,
    pub error: String,
}

/// The bulk upload's answer: what landed and what did not.
#[derive(Debug, Serialize)]
pub struct BulkUploadResult {
    pub created: Vec<ProductImageView>,
    pub failed: Vec<BulkUploadFailure>,
}

/// Upload MANY images to one product in one request
/// (Wave 51 extension): access is checked once; each file
/// then runs the full single-upload path (pipeline, dup
/// guard, quota, cap) and lands or fails on its own —
/// the answer names every failure with its reason, so a
/// seller fixing one bad file never re-uploads the good
/// ones. A file count above the product's image cap is
/// not pre-refused: files land until the cap, the rest
/// fail with the cap's message.
pub async fn upload_product_images_bulk(
    state: &AppState,
    user: &AuthUser,
    product_id: Uuid,
    files: &[BulkUploadFile],
) -> Result<BulkUploadResult, ApiError> {
    if files.is_empty() {
        return Err(ApiError::bad_request(
            "the bulk upload has no files — কোনো ফাইল পাওয়া যায়নি",
        ));
    }
    if files.len() > MAX_IMAGES_PER_PRODUCT as usize {
        return Err(ApiError::bad_request(format!(
            "a bulk upload carries at most {MAX_IMAGES_PER_PRODUCT} files — \
             একবারে সর্বোচ্চ {MAX_IMAGES_PER_PRODUCT}টি ফাইল"
        )));
    }
    let pool = state.require_pool()?;
    catalog_product(pool, user, product_id).await?;
    let mut created = Vec::new();
    let mut failed = Vec::new();
    for file in files {
        match upload_product_image(
            state,
            user,
            product_id,
            ImageUploadInput {
                bytes: &file.bytes,
                declared_content_type: file.content_type.as_deref(),
                alt_text: None,
                variant_id: None,
                original_filename: Some(&file.filename),
            },
        )
        .await
        {
            Ok(view) => created.push(view),
            Err(err) => failed.push(BulkUploadFailure {
                filename: file.filename.clone(),
                error: err.message,
            }),
        }
    }
    Ok(BulkUploadResult { created, failed })
}

/// Whether server-side image fetches may target private
/// addresses (Wave 51 extension): an env escape hatch for
/// development and tests (`MEDIA_FETCH_ALLOW_PRIVATE_HOSTS
/// =true`) — NEVER production, where it would reopen the
/// SSRF hole the domain rules close. Default closed.
fn fetch_allows_private_hosts() -> bool {
    std::env::var("MEDIA_FETCH_ALLOW_PRIVATE_HOSTS")
        .is_ok_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

/// Fetch one image's bytes from a public URL (Wave 51
/// extension — the from-web attach's download): the
/// domain URL rule, then DNS resolution with EVERY
/// resolved address passed through the blocked-IP rule
/// (one private address among public ones refuses the
/// fetch — a rebinding host must not smuggle the request
/// inward), then a capped, time-boxed GET. Known limit,
/// stated plainly: reqwest re-resolves the host when it
/// connects, so a narrow DNS-rebinding window remains
/// between the check and the connect; the check still
/// stops the honest-mistake and the static-target cases,
/// which are the realistic ones for a seller-facing
/// fetch. The body is read in chunks under the image
/// size cap — an endless or oversized stream is cut off,
/// never buffered whole.
async fn fetch_image_bytes(url: &str) -> Result<(Vec<u8>, Option<String>), ApiError> {
    let url = domain::validate_fetch_url(url).map_err(ApiError::bad_request)?;
    // Host (and port) from the validated URL's authority.
    let authority = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_default()
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_string();
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => {
            (host.trim_matches(['[', ']']).to_string(), port.parse::<u16>().unwrap_or(0))
        }
        _ => (
            authority.trim_matches(['[', ']']).to_string(),
            if url.starts_with("https://") { 443 } else { 80 },
        ),
    };
    if !fetch_allows_private_hosts() {
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if domain::is_blocked_fetch_ip(&ip.to_string()) {
                return Err(ApiError::bad_request(
                    "the image URL points at a non-public address — \
                     ছবির URL পাবলিক ঠিকানা হতে হবে",
                ));
            }
        } else {
            let resolved = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|_| {
                    ApiError::bad_request(
                        "the image host could not be resolved — \
                         ছবির হোস্ট খুঁজে পাওয়া যায়নি",
                    )
                })?;
            for address in resolved {
                if domain::is_blocked_fetch_ip(&address.ip().to_string()) {
                    return Err(ApiError::bad_request(
                        "the image URL resolves to a non-public address — \
                         ছবির URL পাবলিক ঠিকানায় যেতে হবে",
                    ));
                }
            }
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent("BDMarketplace-ImageFetch/1.0")
        .build()
        .map_err(|e| ApiError::internal("build fetch client", e))?;
    let mut response = client.get(&url).send().await.map_err(|e| {
        ApiError::bad_request(format!(
            "the image could not be fetched: {e} — ছবিটি আনা যায়নি"
        ))
    })?;
    if !response.status().is_success() {
        return Err(ApiError::bad_request(format!(
            "the image host answered {} — ছবির হোস্ট সাড়া দেয়নি",
            response.status()
        )));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or(value).trim().to_string());
    let cap = domain::MAX_IMAGE_BYTES as usize;
    if let Some(length) = response.content_length() {
        if length > cap as u64 {
            return Err(ApiError::bad_request(
                "the fetched image is larger than the 10 MB cap — \
                 আনা ছবিটি ১০ MB-এর বেশি বড়",
            ));
        }
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| {
        ApiError::bad_request(format!(
            "the image download was interrupted: {e} — ডাউনলোড মাঝপথে থেমেছে"
        ))
    })? {
        if bytes.len() + chunk.len() > cap {
            return Err(ApiError::bad_request(
                "the fetched image is larger than the 10 MB cap — \
                 আনা ছবিটি ১০ MB-এর বেশি বড়",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((bytes, content_type))
}

/// Attach an image to a product by FETCHING it (Wave 51
/// extension): the server downloads the URL (SSRF-guarded,
/// capped) and stores the bytes through the same pipeline
/// as an upload — the durable form of the hotlink attach,
/// for sellers whose source photos live on hosts that may
/// not keep them (a Facebook CDN link rots; a stored copy
/// does not).
pub async fn attach_product_image_from_web(
    state: &AppState,
    user: &AuthUser,
    product_id: Uuid,
    url: &str,
    alt_text: Option<&str>,
    variant_id: Option<Uuid>,
) -> Result<ProductImageView, ApiError> {
    let pool = state.require_pool()?;
    let product = catalog_product(pool, user, product_id).await?;
    require_variant_of_product(pool, product_id, variant_id).await?;
    let alt_text = validate_alt_text(alt_text).map_err(ApiError::bad_request)?;
    let (bytes, content_type) = fetch_image_bytes(url).await?;
    // The stored row remembers the source file's name —
    // the URL's last path segment, when it has one.
    let original_filename = domain::validate_original_filename(
        url.rsplit('/')
            .next()
            .map(|tail| tail.split(['?', '#']).next().unwrap_or(tail)),
    )
    .ok()
    .flatten();
    let image_id = Uuid::new_v4();
    let base_key = format!("products/{product_id}/{image_id}");
    let processed = media::process_upload(&base_key, &bytes, content_type.as_deref())?;
    insert_uploaded_image(
        state,
        &product,
        image_id,
        &processed,
        alt_text,
        variant_id,
        original_filename,
    )
    .await
}

// ---------------------------------------------------------------------------
// Service functions — orphan reaper + admin media (Wave 51 extension)
// ---------------------------------------------------------------------------

/// The orphan reaper's report (Wave 51 extension): how
/// many objects the store holds, how many the database
/// accounts for, and how many orphans were deleted.
#[derive(Debug, Clone, Serialize)]
pub struct MediaReapReport {
    pub scanned: i64,
    pub known: i64,
    pub deleted: i64,
}

/// Collect every object key the DATABASE accounts for:
/// product images (rows carry their base key + width),
/// review photos (deterministic per-review base), and
/// branding objects whose store URL points at this
/// deployment's own media base (an S3 deployment's
/// branding URLs name the endpoint — those bases are
/// added too when they parse back to a `stores/` key).
async fn known_media_keys(pool: &PgPool) -> Result<std::collections::HashSet<String>, ApiError> {
    let mut known = std::collections::HashSet::new();
    let images: Vec<(String, Option<i32>)> = sqlx::query_as(
        "SELECT storage_key, width FROM product_images WHERE storage_key IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list image keys for reaper", e))?;
    for (base_key, width) in &images {
        known.extend(media::all_object_keys(base_key, *width));
    }
    let reviews: Vec<(Uuid, Option<i32>)> = sqlx::query_as(
        "SELECT id, image_width FROM reviews WHERE image_storage_key IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list review keys for reaper", e))?;
    for (review_id, width) in &reviews {
        known.extend(media::all_object_keys(&format!("reviews/{review_id}/photo"), *width));
    }
    let stores: Vec<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT logo_url, banner_url FROM stores")
            .fetch_all(pool)
            .await
            .map_err(|e| ApiError::internal("list branding urls for reaper", e))?;
    for (logo_url, banner_url) in &stores {
        for url in [logo_url, banner_url] {
            let Some(url) = url else { continue };
            // A branding URL names its original object;
            // strip the public base and the -orig suffix
            // to recover the base key.
            let Some(key) = url.strip_prefix(media::public_base()) else {
                continue;
            };
            let Some(base) = key.strip_suffix("-orig") else {
                continue;
            };
            if base.starts_with("stores/") {
                known.extend(media::all_object_keys(base, None));
            }
        }
    }
    Ok(known)
}

/// Delete the media objects no database row accounts for
/// (Wave 51 extension — the worker's media job): uploads
/// are written object-first, so a crash or a refused
/// insert between the store writes and the row insert
/// leaves inert orphans; left alone they would fill the
/// disk with bytes nothing references. The rule is
/// conservative — an object is deleted ONLY when the
/// database positively does not name its key — and the
/// pass is idempotent, so the worker may run it every
/// cycle.
pub async fn reap_orphan_media(state: &AppState) -> Result<MediaReapReport, ApiError> {
    let pool = state.require_pool()?;
    let known = known_media_keys(pool).await?;
    let listed = state.media_store.list("").await?;
    let mut deleted: i64 = 0;
    for key in &listed {
        if !known.contains(key) {
            state.media_store.delete(key).await?;
            deleted += 1;
        }
    }
    Ok(MediaReapReport {
        scanned: listed.len() as i64,
        known: known.len() as i64,
        deleted,
    })
}

/// One image in the ADMIN media list (Wave 51 extension):
/// the seller media item plus the store's slug, so a
/// moderator can find the shop responsible for a photo.
#[derive(Debug, Serialize)]
pub struct AdminMediaItem {
    #[serde(flatten)]
    pub image: ProductImageView,
    pub product_title: String,
    pub store_id: Uuid,
    pub store_slug: String,
}

/// The admin media list (Wave 51 extension): the newest
/// product images across ALL stores — the moderation
/// surface for imagery (a platform admin removes a
/// policy-breaking photo here; the delete is audited).
pub async fn list_admin_media(
    pool: &PgPool,
    store_slug: Option<&str>,
) -> Result<Vec<AdminMediaItem>, ApiError> {
    #[derive(FromRow)]
    struct AdminMediaRow {
        #[sqlx(flatten)]
        image: ProductImageRow,
        product_title: String,
        store_id: Uuid,
        store_slug: String,
    }
    let rows: Vec<AdminMediaRow> = sqlx::query_as(&format!(
        "SELECT pi.id, pi.product_id, pi.variant_id, pi.storage_key, pi.external_url, \
         pi.width, pi.height, pi.size_bytes, pi.position, pi.is_primary, pi.alt_text, \
         pi.content_hash, pi.original_filename, pi.created_at, p.title AS product_title, p.store_id, \
         s.slug AS store_slug \
         FROM product_images pi \
         JOIN products p ON p.id = pi.product_id \
         JOIN stores s ON s.id = p.store_id \
         WHERE ($1::text IS NULL OR s.slug = $1) \
         ORDER BY pi.created_at DESC LIMIT 200"
    ))
    .bind(store_slug)
    .await
    .map_err(|e| ApiError::internal("list admin media", e))?;
    Ok(rows
        .into_iter()
        .map(|row| AdminMediaItem {
            image: ProductImageView::from(&row.image),
            product_title: row.product_title,
            store_id: row.store_id,
            store_slug: row.store_slug,
        })
        .collect())
}

/// Delete one product image as a platform ADMIN (Wave 51
/// extension — moderation): bypasses the seller access
/// rule (the caller is the platform, already through
/// `require_admin` in the handler), keeps the survivors'
/// renormalise discipline, removes the objects, and
/// writes the audit row in the same transaction as the
/// delete — a moderation removal is always attributable.
pub async fn delete_admin_media(
    state: &AppState,
    admin: &AuthUser,
    image_id: Uuid,
) -> Result<(), ApiError> {
    let pool = state.require_pool()?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin admin media delete", e))?;
    let row: Option<ProductImageRow> = sqlx::query_as(&format!(
        "SELECT {IMAGE_COLUMNS} FROM product_images WHERE id = $1 FOR UPDATE"
    ))
    .bind(image_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("load image for admin delete", e))?;
    let row = row.ok_or_else(|| ApiError::not_found("image not found"))?;
    sqlx::query("DELETE FROM product_images WHERE id = $1")
        .bind(image_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("delete image (admin)", e))?;
    renormalize_product_images(&mut tx, row.product_id).await?;
    crate::audit::record_admin_action(
        &mut *tx,
        admin.user_id,
        "media_delete",
        "product_image",
        Some(&image_id.to_string()),
        Some(serde_json::json!({ "product_id": row.product_id.to_string() })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit admin media delete", e))?;
    if let Some(key) = &row.storage_key {
        media::delete_stored_image(state.media_store.as_ref(), key, row.width).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Service functions — store branding images
// ---------------------------------------------------------------------------

/// Upload a store branding image (Wave 51) — the logo or
/// the banner of the caller's OWN store (the branding
/// surface is owner-scoped, like the Wave 18 branding
/// write it extends). The bytes go through the same
/// pipeline as product images, under the deterministic key
/// `stores/<store_id>/<kind>` — a re-upload replaces the
/// same objects — and the store's `logo_url` / `banner_url`
/// is set to the served original URL, so every existing
/// branding surface renders the upload with no further
/// wiring. The old objects are cleared first (a smaller
/// re-upload must not leave a stale larger derived size
/// behind under the same key).
pub async fn upload_store_branding_image(
    state: &AppState,
    user: &AuthUser,
    kind: &str,
    bytes: &[u8],
    declared_content_type: Option<&str>,
) -> Result<Store, ApiError> {
    if kind != "logo" && kind != "banner" {
        return Err(ApiError::bad_request(
            "branding image kind must be 'logo' or 'banner'",
        ));
    }
    let pool = state.require_pool()?;
    let store = crate::stores::get_store_by_owner(pool, user.user_id).await?;
    let base_key = format!("stores/{}/{kind}", store.id);
    let processed: ProcessedImage =
        media::process_upload(&base_key, bytes, declared_content_type)?;
    media::delete_stored_image(state.media_store.as_ref(), &base_key, None).await;
    media::store_processed(state.media_store.as_ref(), &processed).await?;
    let url = media::public_url_for_key(&media::original_key(&base_key));
    if kind == "logo" {
        sqlx::query("UPDATE stores SET logo_url = $2 WHERE id = $1")
            .bind(store.id)
            .bind(&url)
            .execute(pool)
            .await
            .map_err(|e| ApiError::internal("set store logo", e))?;
    } else {
        sqlx::query("UPDATE stores SET banner_url = $2 WHERE id = $1")
            .bind(store.id)
            .bind(&url)
            .execute(pool)
            .await
            .map_err(|e| ApiError::internal("set store banner", e))?;
    }
    get_store_by_id(pool, store.id).await
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AttachImageUrlInput {
    pub url: String,
    pub alt_text: Option<String>,
    pub variant_id: Option<Uuid>,
}

/// The outcome of a bulk URL attach (Wave 51 extension):
/// the images created and the per-URL failures — one bad
/// link never sinks the batch (the import rule, applied
/// to the seller's own paste-a-list flow).
#[derive(Serialize)]
pub struct BulkAttachOutcome {
    pub created: Vec<ProductImageView>,
    pub failed: Vec<BulkUploadFailure>,
}

/// Attach MANY external images in one call (Wave 51
/// extension): each URL goes through the single attach
/// (validation, cap) independently. More URLs than the
/// per-product cap is a 400 up front — the batch could
/// never fully land.
pub async fn attach_product_images_from_urls(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    urls: Vec<String>,
) -> Result<BulkAttachOutcome, ApiError> {
    if urls.len() as i64 > domain::MAX_IMAGES_PER_PRODUCT {
        return Err(ApiError::bad_request(format!(
            "too many URLs in one batch (max {})",
            domain::MAX_IMAGES_PER_PRODUCT
        )));
    }
    let mut outcome = BulkAttachOutcome {
        created: Vec::new(),
        failed: Vec::new(),
    };
    for url in urls {
        match attach_product_image_url(pool, user, product_id, &url, None, None).await {
            Ok(view) => outcome.created.push(view),
            Err(err) => outcome.failed.push(BulkUploadFailure {
                filename: url,
                error: err.message,
            }),
        }
    }
    Ok(outcome)
}

#[derive(Deserialize)]
pub struct AttachUrlsInput {
    pub urls: Vec<String>,
}

#[derive(Deserialize)]
pub struct UpdateImagesInput {
    pub items: Vec<ImagePlacementInput>,
}

#[derive(Deserialize)]
pub struct BulkDeleteInput {
    pub image_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
pub struct MediaListQuery {
    pub product_id: Option<Uuid>,
    /// `upload` | `external` (Wave 51 extension).
    pub source: Option<String>,
    /// `newest` | `oldest` | `largest` (Wave 51 extension).
    pub sort: Option<String>,
}

#[derive(Deserialize)]
pub struct BrandingImageQuery {
    pub kind: String,
}

// ---------------------------------------------------------------------------
// Multipart reading (shared by the upload handlers)
// ---------------------------------------------------------------------------

/// The parts of an image upload form: the file's bytes and
/// declared content type, plus the optional text fields.
pub struct UploadForm {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub alt_text: Option<String>,
    pub variant_id: Option<Uuid>,
    /// The file field's name for the file, when it sent one.
    pub filename: Option<String>,
}

/// Read an image upload's multipart body (Wave 51): the
/// `file` field is required; `alt_text` and `variant_id`
/// are optional text fields; unknown fields are drained
/// and ignored (a form may carry UI bookkeeping). A field
/// that cannot be read, a variant id that is not a UUID,
/// and a missing file are all bilingual 400s — the seller
/// can fix every one of them.
pub async fn read_upload_form(mut multipart: Multipart) -> Result<UploadForm, ApiError> {
    let mut form = UploadForm {
        bytes: Vec::new(),
        content_type: None,
        alt_text: None,
        variant_id: None,
        filename: None,
    };
    let mut saw_file = false;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("the upload form could not be read: {e}")))?
    {
        match field.name() {
            Some("file") => {
                form.filename = field.file_name().map(str::to_string);
                form.content_type = field.content_type().map(str::to_string);
                let bytes = field.bytes().await.map_err(|e| {
                    ApiError::bad_request(format!("the uploaded file could not be read: {e}"))
                })?;
                form.bytes = bytes.to_vec();
                saw_file = true;
            }
            Some("alt_text") => {
                form.alt_text = Some(field.text().await.map_err(|e| {
                    ApiError::bad_request(format!("the alt text field could not be read: {e}"))
                })?);
            }
            Some("variant_id") => {
                let raw = field.text().await.map_err(|e| {
                    ApiError::bad_request(format!("the variant field could not be read: {e}"))
                })?;
                let raw = raw.trim();
                if !raw.is_empty() {
                    form.variant_id = Some(Uuid::parse_str(raw).map_err(|_| {
                        ApiError::bad_request("variant_id must be a UUID")
                    })?);
                }
            }
            _ => {
                // Drain unknown fields so the body is fully
                // consumed; their content is not our business.
                let _ = field.bytes().await;
            }
        }
    }
    if !saw_file {
        return Err(ApiError::bad_request(
            "the upload has no 'file' field — ছবির ফাইল পাওয়া যায়নি",
        ));
    }
    Ok(form)
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn upload_image_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    multipart: Multipart,
) -> Result<(StatusCode, Json<ProductImageView>), ApiError> {
    let form = read_upload_form(multipart).await?;
    let image = upload_product_image(
        &state,
        &user,
        product_id,
        ImageUploadInput {
            bytes: &form.bytes,
            declared_content_type: form.content_type.as_deref(),
            alt_text: form.alt_text.as_deref(),
            variant_id: form.variant_id,
            original_filename: form.filename.as_deref(),
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(image)))
}

pub async fn attach_images_from_urls_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(input): Json<AttachUrlsInput>,
) -> Result<Json<BulkAttachOutcome>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        attach_product_images_from_urls(pool, &user, product_id, input.urls).await?,
    ))
}

pub async fn attach_image_url_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(input): Json<AttachImageUrlInput>,
) -> Result<(StatusCode, Json<ProductImageView>), ApiError> {
    let pool = state.require_pool()?;
    let image = attach_product_image_url(
        pool,
        &user,
        product_id,
        &input.url,
        input.alt_text.as_deref(),
        input.variant_id,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(image)))
}

pub async fn list_images_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
) -> Result<Json<Vec<ProductImageView>>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(list_product_images(pool, &user, product_id).await?))
}

pub async fn update_images_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(input): Json<UpdateImagesInput>,
) -> Result<Json<Vec<ProductImageView>>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        update_product_images(pool, &user, product_id, &input.items).await?,
    ))
}

pub async fn delete_image_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path((product_id, image_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<ProductImageView>>, ApiError> {
    Ok(Json(
        delete_product_image(&state, &user, product_id, image_id).await?,
    ))
}

pub async fn list_seller_media_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<MediaListQuery>,
) -> Result<Json<SellerMediaList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_seller_media(
            pool,
            &user,
            query.product_id,
            query.source.as_deref(),
            query.sort.as_deref(),
        )
        .await?,
    ))
}

pub async fn bulk_delete_media_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<BulkDeleteInput>,
) -> Result<Json<BulkDeleteResult>, ApiError> {
    Ok(Json(
        bulk_delete_seller_media(&state, &user, &input.image_ids).await?,
    ))
}

/// Read a BULK upload's multipart body (Wave 51
/// extension): every `file` field becomes one entry (with
/// its filename for the per-file report); other fields are
/// drained and ignored.
pub async fn read_bulk_upload_form(
    mut multipart: Multipart,
) -> Result<Vec<BulkUploadFile>, ApiError> {
    let mut files = Vec::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("the upload form could not be read: {e}")))?
    {
        if field.name() == Some("file") {
            let filename = field
                .file_name()
                .map(str::to_string)
                .unwrap_or_else(|| "file".to_string());
            let content_type = field.content_type().map(str::to_string);
            let bytes = field.bytes().await.map_err(|e| {
                ApiError::bad_request(format!("an uploaded file could not be read: {e}"))
            })?;
            files.push(BulkUploadFile {
                filename,
                content_type,
                bytes: bytes.to_vec(),
            });
        } else {
            let _ = field.bytes().await;
        }
    }
    Ok(files)
}

pub async fn replace_image_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path((product_id, image_id)): Path<(Uuid, Uuid)>,
    multipart: Multipart,
) -> Result<Json<ProductImageView>, ApiError> {
    let form = read_upload_form(multipart).await?;
    let image = replace_product_image(
        &state,
        &user,
        product_id,
        image_id,
        &form.bytes,
        form.content_type.as_deref(),
        form.filename.as_deref(),
    )
    .await?;
    Ok(Json(image))
}

pub async fn bulk_upload_images_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    multipart: Multipart,
) -> Result<Json<BulkUploadResult>, ApiError> {
    let files = read_bulk_upload_form(multipart).await?;
    Ok(Json(
        upload_product_images_bulk(&state, &user, product_id, &files).await?,
    ))
}

#[derive(Deserialize)]
pub struct AttachFromWebInput {
    pub url: String,
    pub alt_text: Option<String>,
    pub variant_id: Option<Uuid>,
}

pub async fn attach_image_from_web_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(product_id): Path<Uuid>,
    Json(input): Json<AttachFromWebInput>,
) -> Result<(StatusCode, Json<ProductImageView>), ApiError> {
    let image = attach_product_image_from_web(
        &state,
        &user,
        product_id,
        &input.url,
        input.alt_text.as_deref(),
        input.variant_id,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(image)))
}

#[derive(Deserialize)]
pub struct AdminMediaQuery {
    /// Restrict the moderation list to one store, by slug.
    pub store: Option<String>,
}

pub async fn list_admin_media_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminMediaQuery>,
) -> Result<Json<Vec<AdminMediaItem>>, ApiError> {
    crate::admin::require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(list_admin_media(pool, query.store.as_deref()).await?))
}

pub async fn delete_admin_media_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(image_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    crate::admin::require_admin(&state, &user).await?;
    delete_admin_media(&state, &user, image_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn upload_branding_image_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<BrandingImageQuery>,
    multipart: Multipart,
) -> Result<Json<Store>, ApiError> {
    let form = read_upload_form(multipart).await?;
    let store = upload_store_branding_image(
        &state,
        &user,
        &query.kind,
        &form.bytes,
        form.content_type.as_deref(),
    )
    .await?;
    Ok(Json(store))
}

/// GET /media/*key (Wave 51) — serve one stored object of
/// the LOCAL driver. The key is validated by the domain
/// rule before it ever touches the store (no traversal is
/// representable), the content type is sniffed from the
/// bytes themselves (the same magic-byte rule uploads are
/// gated by — the served type can never disagree with the
/// content), and the cache header is immutable: object
/// keys are content-addressed by image id + size, so a URL
/// never changes its bytes. For the S3 driver this route
/// answers 404 — views carry the endpoint's own URLs then,
/// and a local-style URL is simply wrong.
#[derive(Deserialize)]
pub struct ServeMediaQuery {
    /// `?download=1` asks for an attachment disposition
    /// (the seller's "save the original" link) instead of
    /// an inline render.
    pub download: Option<String>,
}

pub async fn serve_media_handler(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Query(query): Query<ServeMediaQuery>,
) -> Result<(HeaderMap, Vec<u8>), ApiError> {
    if !state.media_store.serves_locally() {
        return Err(ApiError::not_found("media not found"));
    }
    if validate_media_key(&key).is_err() {
        return Err(ApiError::not_found("media not found"));
    }
    let bytes = state
        .media_store
        .get(&key)
        .await?
        .ok_or_else(|| ApiError::not_found("media not found"))?;
    let content_type = domain::sniff_image_kind(&bytes)
        .map(|kind| kind.content_type())
        .unwrap_or("application/octet-stream");
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(content_type),
    );
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    if query.download.as_deref() == Some("1") {
        // The attachment name is the key's last segment —
        // already key-validated above (no separators or
        // quotes can reach the header value) — plus the
        // sniffed format's extension when the segment
        // lacks one (stored keys are extensionless by
        // design; a downloaded file should not be).
        let segment = key.rsplit('/').next().unwrap_or("image");
        let filename = match domain::sniff_image_kind(&bytes) {
            Some(kind) if !segment.contains('.') => {
                format!("{segment}.{}", kind.extension())
            }
            _ => segment.to_string(),
        };
        if let Ok(value) = header::HeaderValue::from_str(&format!(
            "attachment; filename=\"{filename}\""
        )) {
            headers.insert(header::CONTENT_DISPOSITION, value);
        }
    }
    Ok((headers, bytes))
}
