//! Seller bulk edit (Wave 57).
//!
//! One request, up to [`domain::MAX_BULK_EDIT_ITEMS`]
//! products, any combination of four actions: a price
//! change (set / percent / fixed delta), a stock change
//! (set / adjust), a status flip, a category assignment.
//! The module is deliberately a thin composer over the
//! audited single-product paths — it invents no write
//! path of its own:
//!
//! - Price, status and category go through
//!   [`crate::products::update_product_for_owner`], so
//!   the merged validation, the variant-stranding price
//!   guard and the draft→active follower-notice rule all
//!   apply exactly as they do to the single edit form.
//! - Stock goes through
//!   [`crate::inventory::adjust_stock_for_seller`] — the
//!   Wave 23 audited path. A bulk "set stock to N" is
//!   computed as a delta against the stored stock and
//!   lands as a `stock_adjustments` row in the adjust
//!   service's own transaction. **There is no second
//!   stock path**: this module never writes
//!   `products.stock`.
//! - Price math is the domain's checked math
//!   ([`domain::bulk_price_after_percent`] /
//!   [`domain::bulk_price_after_delta`]): percent changes
//!   round half up to the poisha and floor at 1 poisha,
//!   so a bulk cut can never silently make a product
//!   free.
//!
//! Scoping: every product id is judged on its own — an
//! id the caller cannot manage (another store's product,
//! or one that does not exist) is a per-item error in the
//! report, never silently skipped and never aborting the
//! rest of the batch. Request-shape problems (no ids, no
//! action, an out-of-range percent, an unknown category
//! slug) fail the whole request with a 400 before any
//! product is touched.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::products::{get_product_by_id, update_product_for_owner, UpdateProductInput};
use crate::{access, ApiError, AppState};

/// A bulk price action. `mode` discriminates the three
/// shapes, mirroring how the seller thinks: name the new
/// price, move it by a percent, or move it by a fixed
/// poisha amount.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BulkPriceAction {
    Set { price_poisha: i64 },
    Percent { percent: i64 },
    Adjust { delta_poisha: i64 },
}

/// A bulk stock action: set the stored stock to an exact
/// count (a stocktake), or adjust it by a signed delta
/// (a delivery arrived, units were damaged).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BulkStockAction {
    Set { stock: i64 },
    Adjust { delta: i64 },
}

/// A bulk category assignment: exactly one of the three
/// keys names the target — a taxonomy id, a taxonomy
/// slug, or a free-text label (which the update path's
/// resolver links on the Wave 56 exact-match rule, or
/// stores as a plain label when it matches nothing).
#[derive(Debug, Clone, Deserialize)]
pub struct BulkCategoryAction {
    pub category_id: Option<Uuid>,
    pub category_slug: Option<String>,
    pub category_label: Option<String>,
}

/// POST /v1/seller/products/bulk-edit request body.
#[derive(Debug, Deserialize)]
pub struct BulkEditRequest {
    pub product_ids: Vec<Uuid>,
    pub price: Option<BulkPriceAction>,
    pub stock: Option<BulkStockAction>,
    pub status: Option<String>,
    pub category: Option<BulkCategoryAction>,
}

/// One product's outcome in a bulk-edit report.
#[derive(Debug, Serialize)]
pub struct BulkEditOutcome {
    pub product_id: Uuid,
    pub ok: bool,
    pub error: Option<String>,
    /// The actions that actually changed something, in
    /// application order (`price`, `stock`, `status`,
    /// `category`) — an action whose value already matched
    /// is not listed.
    pub changed: Vec<String>,
}

/// The bulk-edit report.
#[derive(Debug, Serialize)]
pub struct BulkEditReport {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub outcomes: Vec<BulkEditOutcome>,
}

/// Resolve the request's category action ONCE, before any
/// product is touched: a slug that names no live taxonomy
/// node is a request-shape error (400), because the
/// seller made one pick for the whole batch — unlike the
/// import, where the slug is per-row data. Returns the
/// (category_id, category label) pair the update path
/// understands: the id form passes `category_id` only (the
/// update path syncs the label from the node); the label
/// form passes the label only (the update path resolves).
async fn resolve_bulk_category(
    pool: &PgPool,
    action: &BulkCategoryAction,
) -> Result<(Option<Uuid>, Option<String>), ApiError> {
    let named = [
        action.category_id.is_some(),
        action.category_slug.is_some(),
        action.category_label.is_some(),
    ]
    .iter()
    .filter(|flag| **flag)
    .count();
    if named != 1 {
        return Err(ApiError::bad_request(
            "category needs exactly one of category_id, category_slug, category_label — \
             ক্যাটাগরির জন্য ঠিক একটি উপায় দিতে হবে",
        ));
    }
    if let Some(id) = action.category_id {
        // Existence and liveness are validated per item
        // by the update path's category rules, exactly as
        // the single edit validates them.
        return Ok((Some(id), None));
    }
    if let Some(slug) = &action.category_slug {
        let slug = domain::validate_category_slug(slug).map_err(ApiError::bad_request)?;
        let rows = crate::categories::load_taxonomy_rows(pool).await?;
        let flats = crate::categories::rows_to_flat(&rows);
        let node = domain::category_by_slug(&flats, &slug).ok_or_else(|| {
            ApiError::bad_request(format!(
                "category slug '{slug}' does not exist — '{slug}' নামে কোনো ক্যাটাগরি নেই"
            ))
        })?;
        if !node.is_active {
            return Err(ApiError::bad_request(format!(
                "category '{slug}' is retired — '{slug}' ক্যাটাগরিটি বন্ধ আছে"
            )));
        }
        let id = Uuid::parse_str(&node.id)
            .map_err(|_| ApiError::bad_request("category id is malformed"))?;
        return Ok((Some(id), None));
    }
    let label = action
        .category_label
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    if label.is_empty() {
        return Err(ApiError::bad_request(
            "category_label must not be empty — ক্যাটাগরির লেবেল খালি হতে পারবে না",
        ));
    }
    domain::validate_category(Some(&label)).map_err(ApiError::bad_request)?;
    Ok((None, Some(label)))
}

/// Validate the request's shape before any product is
/// touched. Whole-request failures are 400s: the seller
/// fixes the request, not one row of a report.
fn validate_bulk_edit_request(request: &BulkEditRequest) -> Result<Vec<Uuid>, ApiError> {
    if request.product_ids.is_empty() {
        return Err(ApiError::bad_request(
            "product_ids must name at least one product — অন্তত একটি পণ্য বেছে নিন",
        ));
    }
    if request.product_ids.len() > domain::MAX_BULK_EDIT_ITEMS {
        return Err(ApiError::bad_request(format!(
            "at most {} products per bulk edit — একবারে সর্বোচ্চ {}টি পণ্য",
            domain::MAX_BULK_EDIT_ITEMS,
            domain::MAX_BULK_EDIT_ITEMS
        )));
    }
    // Deduplicate, keeping first-occurrence order: naming
    // a product twice must not apply a percent twice.
    let mut ids: Vec<Uuid> = Vec::with_capacity(request.product_ids.len());
    for id in &request.product_ids {
        if !ids.contains(id) {
            ids.push(*id);
        }
    }
    if request.price.is_none()
        && request.stock.is_none()
        && request.status.is_none()
        && request.category.is_none()
    {
        return Err(ApiError::bad_request(
            "choose at least one action: price, stock, status or category — \
             অন্তত একটি কাজ বেছে নিন: মূল্য, স্টক, অবস্থা বা ক্যাটাগরি",
        ));
    }
    if let Some(BulkPriceAction::Percent { percent }) = &request.price {
        domain::validate_bulk_percent(*percent).map_err(|message| {
            ApiError::bad_request(format!(
                "{message} — শতাংশ -৯৯ থেকে ৫০০-এর মধ্যে হতে হবে"
            ))
        })?;
    }
    if let Some(BulkPriceAction::Set { price_poisha }) = &request.price {
        if *price_poisha < 1 {
            return Err(ApiError::bad_request(
                "price_poisha must be at least 1 — মূল্য কমপক্ষে ১ পয়সা হতে হবে",
            ));
        }
    }
    if let Some(BulkStockAction::Set { stock }) = &request.stock {
        domain::validate_bulk_stock_target(*stock).map_err(|message| {
            ApiError::bad_request(format!("{message} — স্টকের পরিমাণ সঠিক নয়"))
        })?;
    }
    if let Some(BulkStockAction::Adjust { delta }) = &request.stock {
        if *delta == 0 {
            return Err(ApiError::bad_request(
                "stock delta must not be zero — স্টকের পরিবর্তন শূন্য হতে পারবে না",
            ));
        }
    }
    if let Some(status) = &request.status {
        if !domain::is_valid_product_status(status) {
            return Err(ApiError::bad_request(
                "status must be one of: draft, active, archived — \
                 অবস্থা draft, active বা archived হতে হবে",
            ));
        }
    }
    Ok(ids)
}

/// The stored (raw) stock of a product the caller may
/// manage, judged with the inventory module's own
/// convention: no relationship reads as 404, a role
/// without manage_catalog reads as 403. The bulk stock
/// SET needs the current count to compute its delta; the
/// read mirrors `adjust_stock_for_seller`'s gate so the
/// two can never disagree about who may move stock.
async fn manageable_raw_stock(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
) -> Result<i64, ApiError> {
    let row: Option<(Uuid, i32)> =
        sqlx::query_as("SELECT store_id, stock FROM products WHERE id = $1")
            .bind(product_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("load product for bulk stock", e))?;
    let (store_id, stock) = row.ok_or_else(|| ApiError::not_found("product not found"))?;
    match access::store_role(pool, user.user_id, store_id).await? {
        None => Err(ApiError::not_found("product not found")),
        Some(role) => {
            access::require_role(&role, domain::STORE_ACTION_MANAGE_CATALOG)?;
            Ok(i64::from(stock))
        }
    }
}

/// Apply the request to one product. Every write goes
/// through the single-product service paths; this
/// function only sequences them and translates their
/// outcomes into the report's shape.
async fn apply_bulk_edit_item(
    pool: &PgPool,
    user: &AuthUser,
    product_id: Uuid,
    request: &BulkEditRequest,
    category_pick: Option<&(Option<Uuid>, Option<String>)>,
) -> BulkEditOutcome {
    let mut changed: Vec<String> = Vec::new();
    let fail = |error: ApiError, changed: Vec<String>| BulkEditOutcome {
        product_id,
        ok: false,
        error: Some(error.message),
        changed,
    };

    // Price / status / category ride ONE update call so
    // the product is validated and written once. The
    // current product is read first so `changed` stays
    // honest: an action whose value already matches is
    // neither written nor reported as a change.
    if request.price.is_some() || request.status.is_some() || category_pick.is_some() {
        let current = match get_product_by_id(pool, product_id).await {
            Ok(product) => product,
            Err(error) => return fail(error, changed),
        };
        let mut patch = UpdateProductInput {
            title: None,
            title_bn: None,
            description: None,
            category: None,
            category_id: None,
            price_poisha: None,
            compare_at_poisha: None,
            status: None,
            weight_grams: None,
        };
        if let Some(price) = &request.price {
            let new_price = match price {
                BulkPriceAction::Set { price_poisha } => *price_poisha,
                BulkPriceAction::Percent { percent } => {
                    match domain::bulk_price_after_percent(current.price_poisha, *percent) {
                        Some(value) => value,
                        None => {
                            return fail(
                                ApiError::bad_request(
                                    "the percent change would not leave a valid price — \
                                     শতাংশ পরিবর্তনে বৈধ মূল্য থাকছে না",
                                ),
                                changed,
                            )
                        }
                    }
                }
                BulkPriceAction::Adjust { delta_poisha } => {
                    match domain::bulk_price_after_delta(current.price_poisha, *delta_poisha) {
                        Some(value) => value,
                        None => {
                            return fail(
                                ApiError::bad_request(
                                    "the price change would take the price below 1 poisha — \
                                     মূল্য ১ পয়সার নিচে নামানো যাবে না",
                                ),
                                changed,
                            )
                        }
                    }
                }
            };
            if new_price != current.price_poisha {
                patch.price_poisha = Some(new_price);
                changed.push("price".to_string());
            }
        }
        if let Some(status) = &request.status {
            if *status != current.status {
                patch.status = Some(status.clone());
                changed.push("status".to_string());
            }
        }
        if let Some((category_id, category_label)) = category_pick {
            let category_changed = match category_id {
                Some(id) => current.category_id != Some(*id),
                None => current.category.as_deref() != category_label.as_deref(),
            };
            if category_changed {
                patch.category_id = *category_id;
                patch.category = category_label.clone();
                changed.push("category".to_string());
            }
        }
        // The update call ALWAYS runs when any of the
        // three actions is present — even when every value
        // already matches and the patch is empty — because
        // it is also the access verdict: a product the
        // caller cannot manage must error here, never pass
        // silently as "nothing to change".
        if let Err(error) = update_product_for_owner(pool, user, product_id, patch).await {
            return fail(error, changed);
        }
    }

    // Stock rides the audited adjust path — its own
    // transaction, its own stock_adjustments row.
    if let Some(stock) = &request.stock {
        let (delta, reason) = match stock {
            BulkStockAction::Adjust { delta } => (*delta, "correction"),
            BulkStockAction::Set { stock: target } => {
                match manageable_raw_stock(pool, user, product_id).await {
                    Ok(current) => (*target - current, "count_fix"),
                    Err(error) => return fail(error, changed),
                }
            }
        };
        if delta != 0 {
            match crate::inventory::adjust_stock_for_seller(
                pool,
                user,
                product_id,
                delta,
                reason,
                Some("bulk edit (wave 57)"),
            )
            .await
            {
                Ok(_) => changed.push("stock".to_string()),
                Err(error) => return fail(error, changed),
            }
        }
    }

    BulkEditOutcome {
        product_id,
        ok: true,
        error: None,
        changed,
    }
}

/// Run one bulk edit: validate the shape, resolve the
/// category pick once, then apply per product. The
/// per-item services own their transactions — the batch
/// as a whole is deliberately NOT one transaction (the
/// Wave 40 bulk-book precedent): a 200-product batch that
/// rolled back as a unit on product 199 would be
/// unusable, and the report names exactly what landed.
pub async fn bulk_edit_products(
    state: &AppState,
    user: &AuthUser,
    request: &BulkEditRequest,
) -> Result<BulkEditReport, ApiError> {
    let pool = state.require_pool()?;
    let ids = validate_bulk_edit_request(request)?;
    let category_pick = match &request.category {
        Some(action) => Some(resolve_bulk_category(pool, action).await?),
        None => None,
    };
    let mut outcomes: Vec<BulkEditOutcome> = Vec::with_capacity(ids.len());
    for product_id in &ids {
        outcomes.push(
            apply_bulk_edit_item(pool, user, *product_id, request, category_pick.as_ref()).await,
        );
    }
    let succeeded = outcomes.iter().filter(|outcome| outcome.ok).count();
    Ok(BulkEditReport {
        total: outcomes.len(),
        succeeded,
        failed: outcomes.len() - succeeded,
        outcomes,
    })
}

/// POST /v1/seller/products/bulk-edit
pub async fn bulk_edit_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(request): Json<BulkEditRequest>,
) -> Result<Json<BulkEditReport>, ApiError> {
    let report = bulk_edit_products(&state, &user, &request).await?;
    Ok(Json(report))
}
