//! The demo catalogue seed run (Wave 57).
//!
//! [`run_demo_seed`] builds the demo catalogue from
//! [`crate::demo_catalogue`]'s data tables THROUGH the
//! platform's own service functions — never raw product
//! SQL:
//!
//! - each store's seller account is ensured, its store is
//!   created via [`crate::stores::create_store_for_user`]
//!   (or reused when a previous run made it), and its
//!   branding is set through the branding service;
//! - each store's products are serialised into a CSV
//!   sheet in the bulk-import format and imported via
//!   [`crate::bulk_import::import_bulk_csv`] — so the
//!   plan cap, the taxonomy resolver, the follower-notice
//!   funnel and the sku idempotency all apply to the seed
//!   exactly as they apply to a seller's own sheet.
//!   Re-running the seed therefore UPDATES the catalogue
//!   in place instead of duplicating it;
//! - each newly created product receives ONE generated
//!   placeholder image through the real media pipeline
//!   ([`crate::product_images::upload_product_image`]):
//!   a deterministic two-colour gradient rendered from
//!   the product's sku by the `image` crate, in-process,
//!   with no network. The image is honestly a placeholder
//!   — its alt text says so — and it exists so the demo
//!   storefront shows the Wave 51 media system working,
//!   not to pass generated art off as product photography.
//!
//! The only raw SQL in the module is the seller-account
//! upsert: user accounts have no service-level "create"
//! function (accounts are born from OTP login), and a
//! seed that logged in through OTP would need the SMS
//! dev path. The upsert writes the same row an OTP login
//! would create, nothing more.
//!
//! The worker binary exposes this as its `seed-demo`
//! subcommand and refuses to run it unless `DEMO_SEED=1`
//! is set — the guard lives in the binary; this function
//! itself is the testable unit (the integration suite
//! calls it twice against the test database and asserts
//! the second run creates nothing).

use serde::Serialize;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::bulk_import::{import_bulk_csv, BulkImportReport};
use crate::demo_catalogue::{DemoProductDef, DemoStoreCatalogue, DEMO_CATALOGUE};
use crate::product_images::{upload_product_image, ImageUploadInput};
use crate::stores::{create_store_for_user, get_store_by_slug, update_store_branding_for_store, Store, UpdateBrandingRequest};
use crate::{ApiError, AppState};

/// One store's slice of the seed report.
#[derive(Debug, Serialize)]
pub struct DemoStoreSeedOutcome {
    pub store_slug: String,
    pub store_id: Option<Uuid>,
    pub store_created: bool,
    pub products_created: usize,
    pub products_updated: usize,
    pub products_errored: usize,
    pub images_attached: usize,
    pub error: Option<String>,
}

/// The seed run's report (the worker prints it; the
/// integration test asserts on it).
#[derive(Debug, Serialize)]
pub struct DemoSeedReport {
    pub stores_created: usize,
    pub stores_reused: usize,
    pub products_created: usize,
    pub products_updated: usize,
    pub products_errored: usize,
    pub images_attached: usize,
    pub stores: Vec<DemoStoreSeedOutcome>,
}

/// Ensure the demo seller's account row and return the
/// AuthUser the services act as. An existing account with
/// the demo phone is reused as-is (its role is upgraded
/// to seller by the store creation, as for any user).
async fn ensure_demo_seller(state: &AppState, entry: &DemoStoreCatalogue) -> Result<AuthUser, ApiError> {
    let pool = state.require_pool()?;
    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (phone, name, role) VALUES ($1, $2, 'seller') \
         ON CONFLICT (phone) DO UPDATE SET name = EXCLUDED.name \
         RETURNING id",
    )
    .bind(entry.store.seller_phone)
    .bind(entry.store.seller_name)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("ensure demo seller", e))?;
    Ok(AuthUser {
        user_id,
        phone: entry.store.seller_phone.to_string(),
        role: "seller".to_string(),
        api_key: None,
    })
}

/// Ensure the store itself: reuse the store already
/// carrying the demo slug when it belongs to this seller;
/// refuse (per-store error) when the slug belongs to
/// someone else — the seed never takes over a store.
async fn ensure_demo_store(
    state: &AppState,
    user: &AuthUser,
    entry: &DemoStoreCatalogue,
) -> Result<(Store, bool), ApiError> {
    let pool = state.require_pool()?;
    match get_store_by_slug(pool, entry.store.slug).await {
        Ok(existing) => {
            if existing.owner_user_id != user.user_id {
                return Err(ApiError::conflict(format!(
                    "demo store slug '{}' already belongs to another seller",
                    entry.store.slug
                )));
            }
            return Ok((existing, false));
        }
        Err(error) if error.status == axum::http::StatusCode::NOT_FOUND => {}
        Err(error) => return Err(error),
    }
    let store = create_store_for_user(
        pool,
        user,
        entry.store.name,
        entry.store.slug,
        Some(entry.store.description),
    )
    .await?;
    Ok((store, true))
}

/// Set the store's demo branding through the branding
/// service (theme, accent colour, announcement) — the
/// same write the seller's branding page performs.
async fn apply_demo_branding(state: &AppState, store: &Store, entry: &DemoStoreCatalogue) -> Result<(), ApiError> {
    let pool = state.require_pool()?;
    update_store_branding_for_store(
        pool,
        store,
        UpdateBrandingRequest {
            theme_key: Some(entry.store.theme_key.to_string()),
            brand_color: Some(entry.store.brand_color.to_string()),
            logo_url: None,
            banner_url: None,
            announcement: Some(entry.store.announcement.to_string()),
            description: None,
            about: None,
            return_policy: None,
        },
    )
    .await?;
    Ok(())
}

/// Serialise one store's product table into the
/// bulk-import sheet format. Column order follows
/// [`domain::BULK_IMPORT_COLUMNS`]; every product is born
/// `active` (a demo catalogue nobody can see demonstrates
/// nothing) and carries no image URL — its placeholder
/// image is generated locally by the seed instead of
/// hotlinking an invented address.
fn demo_store_csv(entry: &DemoStoreCatalogue) -> String {
    let mut grid: Vec<Vec<String>> = Vec::with_capacity(entry.products.len() + 1);
    grid.push(
        domain::BULK_IMPORT_COLUMNS
            .iter()
            .map(|name| name.to_string())
            .collect(),
    );
    for product in entry.products {
        grid.push(vec![
            product.sku.to_string(),
            product.title.to_string(),
            product.title_bn.to_string(),
            product.description.unwrap_or_default().to_string(),
            product.price_poisha.to_string(),
            product
                .compare_at_poisha
                .map(|value| value.to_string())
                .unwrap_or_default(),
            product.stock.to_string(),
            product.weight_grams.to_string(),
            "active".to_string(),
            product.category_slug.to_string(),
            String::new(),
            String::new(),
        ]);
    }
    domain::write_csv(&grid)
}

/// A tiny deterministic hash of the sku — the
/// placeholder's colours derive from it, so a product's
/// placeholder is stable across runs and machines (no
/// RNG state, no clock).
fn sku_seed(sku: &str) -> u32 {
    let mut hash: u32 = 2_166_136_261;
    for byte in sku.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    hash
}

/// Render one product's placeholder image: a soft
/// two-colour diagonal gradient with a lighter disc —
/// deliberately abstract (it must never be mistaken for
/// a photograph of the product), coloured from the sku
/// hash over a palette of catalogue-friendly hues.
fn placeholder_png(product: &DemoProductDef) -> Vec<u8> {
    const PALETTE: [(u8, u8, u8); 8] = [
        (27, 94, 32),
        (142, 36, 77),
        (13, 71, 161),
        (46, 125, 50),
        (109, 76, 65),
        (173, 20, 87),
        (51, 105, 30),
        (74, 20, 140),
    ];
    let seed = sku_seed(product.sku);
    let base = PALETTE[(seed as usize) % PALETTE.len()];
    let accent = PALETTE[((seed >> 3) as usize + 3) % PALETTE.len()];
    let size: u32 = 600;
    let image = image::RgbImage::from_fn(size, size, |x, y| {
        let t = f32::from((x + y) as u16) / f32::from((2 * (size - 1)) as u16);
        let blend = |a: u8, b: u8| -> u8 {
            let value = f32::from(a) + (f32::from(b) - f32::from(a)) * t;
            value.round().clamp(0.0, 255.0) as u8
        };
        let dx = i64::from(x) - i64::from(size) / 2;
        let dy = i64::from(y) - i64::from(size) / 2;
        let in_disc = dx * dx + dy * dy < i64::from(size / 4) * i64::from(size / 4);
        if in_disc {
            image::Rgb([
                blend(accent.0, 255),
                blend(accent.1, 255),
                blend(accent.2, 255),
            ])
        } else {
            image::Rgb([blend(base.0, accent.0), blend(base.1, accent.1), blend(base.2, accent.2)])
        }
    });
    let mut bytes: Vec<u8> = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut bytes);
    let dynamic = image::DynamicImage::ImageRgb8(image);
    if dynamic
        .write_to(&mut cursor, image::ImageFormat::Png)
        .is_ok()
    {
        bytes
    } else {
        Vec::new()
    }
}

/// Attach placeholders to the products a store's import
/// just CREATED (updates keep whatever images they
/// already have — re-rendering them every run would churn
/// the media library for nothing). A failure skips that
/// product's image and is visible in the count, never in
/// a failed run: the catalogue matters more than its
/// placeholders.
async fn attach_seed_images(
    state: &AppState,
    user: &AuthUser,
    entry: &DemoStoreCatalogue,
    report: &BulkImportReport,
) -> usize {
    let mut attached = 0;
    for outcome in &report.outcomes {
        if outcome.action != "created" {
            continue;
        }
        let (Some(product_id), Some(sku)) = (outcome.product_id, outcome.sku.as_ref()) else {
            continue;
        };
        let Some(product) = entry.products.iter().find(|def| def.sku == sku) else {
            continue;
        };
        let bytes = placeholder_png(product);
        if bytes.is_empty() {
            continue;
        }
        let alt = format!("Demo placeholder image for {}", product.title);
        let upload = upload_product_image(
            state,
            user,
            product_id,
            ImageUploadInput {
                bytes: &bytes,
                declared_content_type: Some("image/png"),
                alt_text: Some(&alt),
                variant_id: None,
                original_filename: Some("demo-placeholder.png"),
            },
        )
        .await;
        if upload.is_ok() {
            attached += 1;
        }
    }
    attached
}

/// Seed one store end to end, folding its import report
/// into the outcome shape.
async fn seed_one_store(state: &AppState, entry: &DemoStoreCatalogue) -> DemoStoreSeedOutcome {
    let mut outcome = DemoStoreSeedOutcome {
        store_slug: entry.store.slug.to_string(),
        store_id: None,
        store_created: false,
        products_created: 0,
        products_updated: 0,
        products_errored: 0,
        images_attached: 0,
        error: None,
    };
    let run = async {
        let user = ensure_demo_seller(state, entry).await?;
        let (store, created) = ensure_demo_store(state, &user, entry).await?;
        apply_demo_branding(state, &store, entry).await?;
        let csv = demo_store_csv(entry);
        let report = import_bulk_csv(state, &user, store.id, &csv, false).await?;
        let images = attach_seed_images(state, &user, entry, &report).await;
        Ok::<(Store, bool, BulkImportReport, usize), ApiError>((store, created, report, images))
    };
    match run.await {
        Ok((store, created, report, images)) => {
            outcome.store_id = Some(store.id);
            outcome.store_created = created;
            outcome.products_created = report.created;
            outcome.products_updated = report.updated;
            outcome.products_errored = report.skipped;
            outcome.images_attached = images;
            outcome
        }
        Err(error) => {
            outcome.error = Some(error.message);
            outcome
        }
    }
}

/// Run the whole demo seed: every store in the
/// catalogue, in order. One store's failure never stops
/// the others — the report names it and the run moves on
/// (the worker's exit code reflects the aggregate).
pub async fn run_demo_seed(state: &AppState) -> Result<DemoSeedReport, ApiError> {
    let mut report = DemoSeedReport {
        stores_created: 0,
        stores_reused: 0,
        products_created: 0,
        products_updated: 0,
        products_errored: 0,
        images_attached: 0,
        stores: Vec::new(),
    };
    for entry in DEMO_CATALOGUE {
        let outcome = seed_one_store(state, entry).await;
        if outcome.error.is_none() {
            if outcome.store_created {
                report.stores_created += 1;
            } else {
                report.stores_reused += 1;
            }
        }
        report.products_created += outcome.products_created;
        report.products_updated += outcome.products_updated;
        report.products_errored += outcome.products_errored;
        report.images_attached += outcome.images_attached;
        report.stores.push(outcome);
    }
    Ok(report)
}
