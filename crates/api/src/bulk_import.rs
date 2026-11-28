//! Seller bulk catalogue import (Wave 57).
//!
//! A Daraz/Shopify-scale seller does not type products one
//! form at a time: they keep a spreadsheet. This module is
//! the spreadsheet door — a CSV sheet in the domain's
//! [`domain::BULK_IMPORT_COLUMNS`] format becomes products
//! through the SAME rules the single-create path enforces:
//!
//! - **The plan-cap gate is shared.** The batch consults
//!   [`crate::subscriptions::product_cap_for_store`] — the
//!   one cap function — and spends its remaining budget on
//!   the batch's net-new rows, in file order. Updates never
//!   consume budget; a row past the budget fails with its
//!   own per-row error and the rest of the batch still
//!   applies (the documented partial-apply rule).
//! - **The follower-notice funnel is shared.** A row that
//!   is born `active` AND newly created enqueues the
//!   store's follower notices through
//!   [`crate::notices::enqueue_store_new_product_notices`]
//!   inside the row's own transaction — the single-create
//!   funnel's exact semantics. Updates never notify: a
//!   re-import is a catalogue sync, not a publish.
//! - **The single stock path is shared.** A re-import that
//!   changes a product's stock writes the new value AND
//!   its `stock_adjustments` audit row in the same
//!   transaction through
//!   [`crate::inventory::record_stock_adjustment`] — the
//!   Wave 23 writer. No bulk path writes `products.stock`
//!   bare.
//!
//! Idempotency mirrors the Wave 12 Facebook import: the
//! seller's `sku` column is stored as the product's
//! `source_ref` with `source = 'bulk'`, and the
//! `(store_id, source_ref)` partial UNIQUE index makes a
//! re-import an update, never a duplicate. A sku already
//! claimed by ANOTHER source (a manual product the seller
//! later stamped, a Facebook import) is a per-row error —
//! the bulk path never hijacks a product it did not
//! create.
//!
//! Every completed run (dry runs included) is recorded in
//! `bulk_imports` with its full report, so the seller can
//! re-open it from the history endpoints below. A fatally
//! malformed file (bad header, over the row cap) writes
//! nothing and records nothing — there is no report to
//! re-open.

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use domain::{BulkFieldError, BulkProductRow};

use crate::auth::AuthUser;
use crate::inventory::{record_stock_adjustment, StockMovement};
use crate::products::{insert_product, NewProduct};
use crate::{access, ApiError, AppState};

/// The bulk import's request-body ceiling: the sheet is
/// raw CSV text, and 4 MiB carries the 500-row cap many
/// times over even with long descriptions. The route
/// raises the app's global 1 MiB body limit to this for
/// the import path only (the Wave 51 route-limit shape).
pub const BULK_IMPORT_BODY_LIMIT_BYTES: usize = 4 * 1_024 * 1_024;

/// The source tag bulk-created products carry (the
/// idempotency pair is `(store_id, source_ref)` with the
/// seller's sku in `source_ref`).
pub const BULK_SOURCE: &str = "bulk";

/// One row's outcome in an import report.
#[derive(Debug, Clone, Serialize)]
pub struct BulkRowOutcome {
    pub row_number: usize,
    pub sku: Option<String>,
    /// `created` | `updated` | `error` for a real run;
    /// `would_create` | `would_update` | `error` for a
    /// dry run.
    pub action: String,
    pub product_id: Option<Uuid>,
    pub errors: Vec<BulkFieldError>,
    /// Non-fatal facts the seller should know (a stock
    /// change skipped because variants manage it, a
    /// category label that matched no taxonomy node).
    pub notes: Vec<String>,
    /// The image attach failed (bad URL, unreachable
    /// shape) — the product write stands; only the image
    /// is missing. Never fails the row.
    pub image_error: Option<String>,
}

/// The import report: counts plus one outcome per data
/// row, in file order. Serialised verbatim into
/// `bulk_imports.report` for the history endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct BulkImportReport {
    pub import_id: Option<Uuid>,
    pub store_id: Uuid,
    pub dry_run: bool,
    pub total_rows: usize,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
    pub outcomes: Vec<BulkRowOutcome>,
}

/// Query for the import endpoint: which store, and whether
/// this is a dry run. Scalar fields only, so the plain
/// `Query` extractor suffices.
#[derive(Debug, Deserialize)]
pub struct BulkImportQuery {
    pub store_id: Uuid,
    pub dry_run: Option<bool>,
}

#[derive(FromRow)]
struct ExistingBulkRow {
    id: Uuid,
    store_id: Uuid,
    source_ref: String,
    source: String,
    stock: i32,
    /// Whether the platform has taken this product down
    /// (admin products remake). A hidden row refuses the
    /// import outright — see `apply_bulk_row`.
    admin_hidden: bool,
}

/// Resolve one row's category against the taxonomy flats
/// loaded once for the batch. The slug wins over the
/// label (the same precedence as the single-create path's
/// category_id over its label). Returns the link plus the
/// label to store, or the row's field error.
fn resolve_bulk_category(
    flats: &[domain::CategoryFlat],
    row: &BulkProductRow,
    notes: &mut Vec<String>,
) -> Result<(Option<Uuid>, Option<String>), BulkFieldError> {
    if let Some(slug) = &row.category_slug {
        return match domain::category_by_slug(flats, slug) {
            None => Err(BulkFieldError {
                field: "category_slug".to_string(),
                message: format!(
                    "category slug '{slug}' does not exist — '{slug}' নামে কোনো ক্যাটাগরি নেই"
                ),
            }),
            Some(node) if !node.is_active => Err(BulkFieldError {
                field: "category_slug".to_string(),
                message: format!(
                    "category '{slug}' is retired — '{slug}' ক্যাটাগরিটি বন্ধ আছে"
                ),
            }),
            Some(node) => {
                let id = Uuid::parse_str(&node.id).map_err(|_| BulkFieldError {
                    field: "category_slug".to_string(),
                    message: "category id is malformed".to_string(),
                })?;
                Ok((Some(id), Some(node.name_en.clone())))
            }
        };
    }
    if let Some(label) = &row.category_label {
        return match domain::resolve_category_link(label, flats) {
            Some(node_id) => {
                let id = Uuid::parse_str(&node_id).map_err(|_| BulkFieldError {
                    field: "category_label".to_string(),
                    message: "category id is malformed".to_string(),
                })?;
                Ok((Some(id), Some(label.clone())))
            }
            None => {
                notes.push(format!(
                    "category label '{label}' matched no taxonomy node; stored as a free-text label"
                ));
                Ok((None, Some(label.clone())))
            }
        };
    }
    Ok((None, None))
}

/// Run one bulk import. The caller (the handler, the demo
/// seed) has already resolved the store's identity; this
/// function performs the Wave 34 access check itself so
/// every caller gets the same gate.
pub async fn import_bulk_csv(
    state: &AppState,
    user: &AuthUser,
    store_id: Uuid,
    csv_text: &str,
    dry_run: bool,
) -> Result<BulkImportReport, ApiError> {
    let pool = state.require_pool()?;
    access::require_store_action(
        pool,
        user.user_id,
        store_id,
        domain::STORE_ACTION_MANAGE_CATALOG,
    )
    .await?;

    let parsed = domain::parse_bulk_product_csv(csv_text).map_err(ApiError::bad_request)?;
    let total_rows = parsed.rows.len() + parsed.row_errors.len();

    // The taxonomy loads once for the whole batch (the
    // per-row resolution is then pure).
    let taxonomy_rows = crate::categories::load_taxonomy_rows(pool).await?;
    let flats = crate::categories::rows_to_flat(&taxonomy_rows);

    // The store's existing sku claims, in one query.
    let skus: Vec<String> = parsed.rows.iter().map(|row| row.sku.clone()).collect();
    let existing: Vec<ExistingBulkRow> = sqlx::query_as(
        "SELECT id, store_id, source_ref, source, stock, \
         (admin_hidden_at IS NOT NULL) AS admin_hidden FROM products \
         WHERE store_id = $1 AND source_ref = ANY($2)",
    )
    .bind(store_id)
    .bind(&skus)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load existing bulk skus", e))?;

    // The plan-cap budget for net-new rows: the shared cap
    // minus what the store already carries. Updates are
    // free; creates spend one each, in file order.
    let cap = crate::subscriptions::product_cap_for_store(state, store_id).await?;
    let current_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE store_id = $1")
        .bind(store_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count store products", e))?;
    let mut cap_budget = (cap - current_count).max(0);

    let mut outcomes: Vec<BulkRowOutcome> = Vec::new();
    // The domain parser's row errors join the outcomes
    // first; the final sort by row number interleaves them
    // with the applied rows.
    for row_error in &parsed.row_errors {
        outcomes.push(BulkRowOutcome {
            row_number: row_error.row_number,
            sku: row_error.sku.clone(),
            action: "error".to_string(),
            product_id: None,
            errors: row_error.errors.clone(),
            notes: Vec::new(),
            image_error: None,
        });
    }

    for row in &parsed.rows {
        let outcome = apply_bulk_row(
            pool,
            user,
            store_id,
            &flats,
            &existing,
            row,
            dry_run,
            &mut cap_budget,
        )
        .await?;
        outcomes.push(outcome);
    }
    outcomes.sort_by_key(|outcome| outcome.row_number);

    let created = outcomes
        .iter()
        .filter(|o| o.action == "created" || o.action == "would_create")
        .count();
    let updated = outcomes
        .iter()
        .filter(|o| o.action == "updated" || o.action == "would_update")
        .count();
    let skipped = outcomes.iter().filter(|o| o.action == "error").count();

    let import_id = Uuid::new_v4();
    let report = BulkImportReport {
        import_id: Some(import_id),
        store_id,
        dry_run,
        total_rows,
        created,
        updated,
        skipped,
        outcomes,
    };

    // Record the run (dry runs too) so the seller can
    // re-open this exact report. The history write is part
    // of the run's contract, so its failure fails the
    // request rather than silently losing the record.
    let report_json = serde_json::to_value(&report)
        .map_err(|e| ApiError::internal_any("serialise import report", e))?;
    sqlx::query(
        "INSERT INTO bulk_imports (id, store_id, actor_user_id, dry_run, \
         total_rows, created_count, updated_count, error_count, report) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(import_id)
    .bind(store_id)
    .bind(user.user_id)
    .bind(dry_run)
    .bind(i32::try_from(total_rows).unwrap_or(i32::MAX))
    .bind(i32::try_from(created).unwrap_or(i32::MAX))
    .bind(i32::try_from(updated).unwrap_or(i32::MAX))
    .bind(i32::try_from(skipped).unwrap_or(i32::MAX))
    .bind(&report_json)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("record bulk import", e))?;
    Ok(report)
}

/// Apply one validated row: resolve its category, match
/// it against the store's sku claims, then create or
/// update — or explain, in the outcome, why neither
/// happened. Per-row failures NEVER abort the batch.
#[allow(clippy::too_many_arguments)]
async fn apply_bulk_row(
    pool: &PgPool,
    user: &AuthUser,
    store_id: Uuid,
    flats: &[domain::CategoryFlat],
    existing: &[ExistingBulkRow],
    row: &BulkProductRow,
    dry_run: bool,
    cap_budget: &mut i64,
) -> Result<BulkRowOutcome, ApiError> {
    let base = BulkRowOutcome {
        row_number: row.row_number,
        sku: Some(row.sku.clone()),
        action: "error".to_string(),
        product_id: None,
        errors: Vec::new(),
        notes: Vec::new(),
        image_error: None,
    };
    let mut notes: Vec<String> = Vec::new();
    let (category_id, category_label) = match resolve_bulk_category(flats, row, &mut notes) {
        Ok(resolved) => resolved,
        Err(error) => {
            return Ok(BulkRowOutcome {
                errors: vec![error],
                notes,
                ..base
            });
        }
    };

    let claim = existing.iter().find(|entry| entry.source_ref == row.sku);
    match claim {
        Some(entry) if entry.source != BULK_SOURCE => Ok(BulkRowOutcome {
            errors: vec![BulkFieldError {
                field: "sku".to_string(),
                message: format!(
                    "sku '{}' is already used by a product created outside bulk import; \
                     rename the sku or edit that product directly — \
                     '{}' SKU-টি অন্য উপায়ে তৈরি পণ্যে ব্যবহৃত হচ্ছে",
                    row.sku, row.sku
                ),
            }],
            notes,
            ..base
        }),
        Some(entry) => {
            // Admin products remake: a product the
            // PLATFORM has taken down refuses the row.
            // The bulk update path writes title/price/
            // stock/status directly in SQL, bypassing the
            // single-product editor — without this guard
            // the documented export → edit → re-import
            // round-trip would silently rewrite a removed
            // listing (and the demo seed, which imports
            // through this same path, would too). The row
            // errors in dry runs as well: "would_update"
            // for a row the real run refuses would be a
            // lie. Nothing is written, and the flag itself
            // is never touched by an import.
            if entry.admin_hidden {
                return Ok(BulkRowOutcome {
                    errors: vec![BulkFieldError {
                        field: "sku".to_string(),
                        message: format!(
                            "product with sku '{}' has been removed by the platform and cannot be edited by import — \
                             '{}' SKU-এর পণ্যটি প্ল্যাটফর্ম থেকে সরিয়ে নেওয়া হয়েছে; আমদানি দিয়ে সম্পাদনা করা যাবে না",
                            row.sku, row.sku
                        ),
                    }],
                    notes,
                    ..base
                });
            }
            let product_id = entry.id;
            if dry_run {
                return Ok(BulkRowOutcome {
                    action: "would_update".to_string(),
                    product_id: Some(product_id),
                    notes,
                    ..base
                });
            }
            update_bulk_product(pool, user, entry, row, category_id, category_label, &mut notes)
                .await?;
            let image_error = attach_row_image(pool, product_id, row).await;
            Ok(BulkRowOutcome {
                action: "updated".to_string(),
                product_id: Some(product_id),
                notes,
                image_error,
                ..base
            })
        }
        None => {
            if *cap_budget <= 0 {
                return Ok(BulkRowOutcome {
                    errors: vec![BulkFieldError {
                        field: "sku".to_string(),
                        message:
                            "your store's plan product limit is reached — এই স্টোরের প্ল্যানের \
                             পণ্যের সীমা শেষ; upgrade the plan or remove products first"
                                .to_string(),
                    }],
                    notes,
                    ..base
                });
            }
            *cap_budget -= 1;
            if dry_run {
                return Ok(BulkRowOutcome {
                    action: "would_create".to_string(),
                    notes,
                    ..base
                });
            }
            let product_id =
                create_bulk_product(pool, store_id, row, category_id, category_label).await?;
            let image_error = attach_row_image(pool, product_id, row).await;
            Ok(BulkRowOutcome {
                action: "created".to_string(),
                product_id: Some(product_id),
                notes,
                image_error,
                ..base
            })
        }
    }
}

/// Create one bulk product: the shared insert path, plus
/// the follower-notice funnel when the row is born active
/// — both in the row's own transaction, exactly like the
/// single-create path composes them.
async fn create_bulk_product(
    pool: &PgPool,
    store_id: Uuid,
    row: &BulkProductRow,
    category_id: Option<Uuid>,
    category_label: Option<String>,
) -> Result<Uuid, ApiError> {
    let input = NewProduct {
        store_id,
        title: row.title.clone(),
        title_bn: row.title_bn.clone(),
        description: row.description.clone(),
        category: category_label,
        category_id,
        price_poisha: row.price_poisha,
        compare_at_poisha: row.compare_at_poisha,
        stock: row.stock,
        status: row.status.clone(),
        weight_grams: i32::try_from(row.weight_grams).unwrap_or(i32::MAX),
        source: BULK_SOURCE.to_string(),
        source_ref: Some(row.sku.clone()),
    };
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin bulk create", e))?;
    let product_id = insert_product(&mut *tx, &input).await?;
    if row.status == "active" {
        crate::notices::enqueue_store_new_product_notices(&mut tx, product_id).await?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit bulk create", e))?;
    Ok(product_id)
}

/// Update one bulk product in its own transaction. The
/// sheet's data columns overwrite; its BLANK optional
/// cells (title_bn, description, compare_at) leave the
/// stored values standing — a re-import of a price-only
/// sheet must not erase copy the seller wrote in the form.
/// Stock moves only through the Wave 23 writer, in this
/// same transaction; a product whose stock lives on its
/// variants keeps its variant stock and gains a note.
async fn update_bulk_product(
    pool: &PgPool,
    user: &AuthUser,
    existing: &ExistingBulkRow,
    row: &BulkProductRow,
    category_id: Option<Uuid>,
    category_label: Option<String>,
    notes: &mut Vec<String>,
) -> Result<(), ApiError> {
    let product_id = existing.id;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin bulk update", e))?;

    let has_active_variants: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM product_variants \
         WHERE product_id = $1 AND is_active)",
    )
    .bind(product_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("check product variants", e))?;

    sqlx::query(
        "UPDATE products SET title = $2, \
         title_bn = COALESCE($3, title_bn), \
         description = COALESCE($4, description), \
         price_poisha = $5, \
         compare_at_poisha = COALESCE($6, compare_at_poisha), \
         weight_grams = $7, category = $8, category_id = $9, status = $10, \
         updated_at = now() WHERE id = $1",
    )
    .bind(product_id)
    .bind(&row.title)
    .bind(&row.title_bn)
    .bind(&row.description)
    .bind(row.price_poisha)
    .bind(row.compare_at_poisha)
    .bind(i32::try_from(row.weight_grams).unwrap_or(i32::MAX))
    .bind(&category_label)
    .bind(category_id)
    .bind(&row.status)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("update bulk product", e))?;

    if row.stock != existing.stock {
        if has_active_variants {
            notes.push(
                "stock change skipped: this product's stock is managed by its variants"
                    .to_string(),
            );
        } else {
            let delta = row.stock - existing.stock;
            sqlx::query("UPDATE products SET stock = $2 WHERE id = $1")
                .bind(product_id)
                .bind(row.stock)
                .execute(&mut *tx)
                .await
                .map_err(|e| ApiError::internal("update bulk stock", e))?;
            record_stock_adjustment(
                &mut tx,
                &StockMovement {
                    product_id,
                    variant_id: None,
                    store_id: existing.store_id,
                    actor: "seller",
                    actor_user_id: Some(user.user_id),
                    delta,
                    new_stock: row.stock,
                    reason: "count_fix",
                    note: Some("bulk import (wave 57)"),
                },
            )
            .await?;
        }
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit bulk update", e))?;
    Ok(())
}

/// Attach the row's image URL through the Wave 51
/// from-url service, after the product write has
/// committed. Best-effort by design (the Wave 12 import
/// precedent): a failure is reported on the outcome and
/// never fails the row.
async fn attach_row_image(
    pool: &PgPool,
    product_id: Uuid,
    row: &BulkProductRow,
) -> Option<String> {
    let url = row.image_url.as_ref()?;
    match crate::product_images::attach_external_image(pool, product_id, url, None, None).await {
        Ok(_) => None,
        Err(error) => Some(error.message),
    }
}

// --- Handlers ---------------------------------------------------------------

/// POST /v1/seller/products/bulk-import?store_id=&dry_run=
/// with the raw CSV sheet as the request body
/// (`Content-Type: text/csv`, or any type — the body IS
/// the sheet).
pub async fn bulk_import_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<BulkImportQuery>,
    body: String,
) -> Result<Json<BulkImportReport>, ApiError> {
    let report = import_bulk_csv(
        &state,
        &user,
        query.store_id,
        &body,
        query.dry_run.unwrap_or(false),
    )
    .await?;
    Ok(Json(report))
}

/// GET /v1/seller/products/import-template.csv — the
/// example sheet, generated from the domain column spec
/// so it can never drift from the parser's contract.
pub async fn bulk_import_template_handler(user: AuthUser) -> Response {
    // Any signed-in seller may fetch the template; the
    // extractor's presence is the authentication.
    let _ = &user;
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"bulk-import-template.csv\"",
            ),
        ],
        domain::bulk_template_csv(),
    )
        .into_response()
}

/// GET /v1/seller/products/export.csv?store_id= — the
/// store's whole catalogue in the import's own column
/// format: export → edit → re-import is the round-trip.
/// Base product rows only — variants are managed in their
/// own module and are deliberately NOT flattened into the
/// sheet (documented in the README). The sku column is
/// the product's source_ref: products created outside an
/// import carry none and export with a blank sku — assign
/// one in the sheet before re-importing those rows (the
/// import requires a sku and will say so per row, never
/// silently duplicate).
pub async fn bulk_export_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<BulkExportQuery>,
) -> Result<Response, ApiError> {
    let pool = state.require_pool()?;
    access::require_store_action(
        pool,
        user.user_id,
        query.store_id,
        domain::STORE_ACTION_MANAGE_CATALOG,
    )
    .await?;
    let body = export_catalogue_csv(pool, query.store_id).await?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"catalogue-export.csv\"",
            ),
        ],
        body,
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub struct BulkExportQuery {
    pub store_id: Uuid,
}

#[derive(FromRow)]
struct ExportRow {
    source_ref: Option<String>,
    title: String,
    title_bn: Option<String>,
    description: Option<String>,
    price_poisha: i64,
    compare_at_poisha: Option<i64>,
    stock: i32,
    weight_grams: i32,
    status: String,
    category_slug: Option<String>,
    category: Option<String>,
    image_url: Option<String>,
}

/// Build the export sheet for one store. The stock
/// column is the product's stored stock pool; for a
/// product with active variants the live stock is the
/// variants' sum (the Wave 33 rule) and a re-import will
/// report the stock change as variant-managed — the
/// sheet is honest about which pool it carries.
pub async fn export_catalogue_csv(pool: &PgPool, store_id: Uuid) -> Result<String, ApiError> {
    let rows: Vec<ExportRow> = sqlx::query_as(
        "SELECT p.source_ref, p.title, p.title_bn, p.description, \
         p.price_poisha, p.compare_at_poisha, p.stock, p.weight_grams, \
         p.status, c.slug AS category_slug, p.category, \
         (SELECT pi.external_url FROM product_images pi \
          WHERE pi.product_id = p.id AND pi.is_primary \
          ORDER BY pi.position, pi.created_at LIMIT 1) AS image_url \
         FROM products p \
         LEFT JOIN categories c ON c.id = p.category_id \
         WHERE p.store_id = $1 \
         ORDER BY p.created_at, p.title",
    )
    .bind(store_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export catalogue", e))?;
    let mut grid: Vec<Vec<String>> = Vec::with_capacity(rows.len() + 1);
    grid.push(
        domain::BULK_IMPORT_COLUMNS
            .iter()
            .map(|name| name.to_string())
            .collect(),
    );
    for row in rows {
        grid.push(vec![
            row.source_ref.unwrap_or_default(),
            row.title,
            row.title_bn.unwrap_or_default(),
            row.description.unwrap_or_default(),
            row.price_poisha.to_string(),
            row.compare_at_poisha
                .map(|value| value.to_string())
                .unwrap_or_default(),
            row.stock.to_string(),
            row.weight_grams.to_string(),
            row.status,
            row.category_slug.unwrap_or_default(),
            row.category.unwrap_or_default(),
            row.image_url.unwrap_or_default(),
        ]);
    }
    Ok(domain::write_csv(&grid))
}

// --- Import history ---------------------------------------------------------

/// One history-list entry: the run's counts without the
/// full outcome list (the detail endpoint serves that).
#[derive(Debug, Serialize, FromRow)]
pub struct BulkImportSummary {
    pub id: Uuid,
    pub store_id: Uuid,
    pub dry_run: bool,
    pub total_rows: i32,
    pub created_count: i32,
    pub updated_count: i32,
    pub error_count: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct BulkImportHistoryQuery {
    pub store_id: Uuid,
    pub limit: Option<i64>,
    /// Optional filter: only dry runs (`true`) or only
    /// real runs (`false`); absent lists both.
    pub dry_run: Option<bool>,
}

/// GET /v1/seller/products/import-history?store_id=&limit=&dry_run=
pub async fn bulk_import_history_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<BulkImportHistoryQuery>,
) -> Result<Json<Vec<BulkImportSummary>>, ApiError> {
    let pool = state.require_pool()?;
    access::require_store_action(
        pool,
        user.user_id,
        query.store_id,
        domain::STORE_ACTION_MANAGE_CATALOG,
    )
    .await?;
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let rows: Vec<BulkImportSummary> = sqlx::query_as(
        "SELECT id, store_id, dry_run, total_rows, created_count, \
         updated_count, error_count, created_at FROM bulk_imports \
         WHERE store_id = $1 AND ($3::boolean IS NULL OR dry_run = $3) \
         ORDER BY created_at DESC LIMIT $2",
    )
    .bind(query.store_id)
    .bind(limit)
    .bind(query.dry_run)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list bulk import history", e))?;
    Ok(Json(rows))
}

/// One row of the admin import-activity view: a run plus
/// the store it targeted and the actor's MASKED phone
/// (the audit views' convention — full numbers never
/// leave through an admin list).
#[derive(Debug, Serialize)]
pub struct AdminBulkImportEntry {
    pub id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub store_name: String,
    pub actor_phone_masked: String,
    pub dry_run: bool,
    pub total_rows: i32,
    pub created_count: i32,
    pub updated_count: i32,
    pub error_count: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct AdminBulkImportsQuery {
    pub limit: Option<i64>,
}

/// GET /v1/admin/bulk-imports?limit= — the operator's
/// cross-store view of bulk catalogue activity (support
/// and abuse watch: a seller bulk-creating thousands of
/// rows, or a store whose sheets keep failing, is visible
/// here without opening the store's own history). Admin
/// only, like every /v1/admin route (JWT-only under the
/// Wave 37 key exclusions).
pub async fn admin_bulk_imports_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminBulkImportsQuery>,
) -> Result<Json<Vec<AdminBulkImportEntry>>, ApiError> {
    crate::admin::require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    #[derive(FromRow)]
    struct AdminBulkImportRow {
        id: Uuid,
        store_id: Uuid,
        store_slug: String,
        store_name: String,
        actor_phone: String,
        dry_run: bool,
        total_rows: i32,
        created_count: i32,
        updated_count: i32,
        error_count: i32,
        created_at: chrono::DateTime<chrono::Utc>,
    }
    let rows: Vec<AdminBulkImportRow> = sqlx::query_as(
        "SELECT b.id, b.store_id, s.slug AS store_slug, s.name AS store_name, \
         u.phone AS actor_phone, b.dry_run, b.total_rows, b.created_count, \
         b.updated_count, b.error_count, b.created_at \
         FROM bulk_imports b \
         JOIN stores s ON s.id = b.store_id \
         JOIN users u ON u.id = b.actor_user_id \
         ORDER BY b.created_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin bulk imports", e))?;
    Ok(Json(
        rows.into_iter()
            .map(|row| AdminBulkImportEntry {
                id: row.id,
                store_id: row.store_id,
                store_slug: row.store_slug,
                store_name: row.store_name,
                actor_phone_masked: domain::mask_phone(&row.actor_phone),
                dry_run: row.dry_run,
                total_rows: row.total_rows,
                created_count: row.created_count,
                updated_count: row.updated_count,
                error_count: row.error_count,
                created_at: row.created_at,
            })
            .collect(),
    ))
}

/// GET /v1/seller/products/import-history/:id — the full
/// stored report of one run. Access is judged by the
/// run's OWN store (the id names the run, not a store).
pub async fn bulk_import_detail_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(import_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pool = state.require_pool()?;
    let row: Option<(Uuid, serde_json::Value)> =
        sqlx::query_as("SELECT store_id, report FROM bulk_imports WHERE id = $1")
            .bind(import_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("load bulk import", e))?;
    let (store_id, report) = row.ok_or_else(|| ApiError::not_found("import not found"))?;
    access::require_store_action(
        pool,
        user.user_id,
        store_id,
        domain::STORE_ACTION_MANAGE_CATALOG,
    )
    .await?;
    Ok(Json(report))
}
