//! Wave 12: Facebook Page linking and post-to-product import.
//!
//! Bangladeshi sellers sell heavily through Facebook Page posts,
//! so the platform meets them where their catalogue already
//! lives: a seller links their Page (identity only — page id and
//! display name) and pastes post texts; the pure domain parser
//! (`draft_from_fb_post`) turns each post into an editable product
//! draft, and the import creates the approved drafts as products
//! through the ONE product insert path (`products::insert_product`).
//!
//! Honesty rule, same as the payment gateways and couriers:
//! fetching posts directly from a seller's Page needs the Meta
//! Graph API — a registered Meta app plus a page access token
//! from an OAuth flow. Neither exists yet, so the paste pipeline
//! is the real import source in this wave and the Graph source
//! reports its state plainly: 503 while the app credentials are
//! absent, 501 while the page-token OAuth flow is still missing.
//! Nothing here pretends to have fetched a post it did not fetch.
//!
//! Import idempotency: every imported product records
//! `source = 'facebook'` and `source_ref = <the post id the
//! seller supplied>`, and a draft whose `source_ref` the store
//! already has is skipped as "already imported" — re-running an
//! import is safe. A partial UNIQUE index (migration 0014)
//! backstops the rule in the database.

use std::collections::HashSet;

use axum::{extract::State, http::StatusCode, Json};
use domain::{draft_from_fb_post, validate_product_input, ProductInput, DEFAULT_WEIGHT_GRAMS};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::products::{get_product_by_id, insert_product, NewProduct, Product};
use crate::stores::{get_store_by_id, get_store_by_owner, Store};
use crate::{ApiError, AppState};

/// How many posts (or drafts) one preview / import call may carry.
pub const MAX_IMPORT_ITEMS: usize = 50;

/// How long one pasted post text may be, in characters.
pub const MAX_POST_TEXT_CHARS: usize = 5_000;

/// The status imported products are created with: importing is an
/// explicit publish action (the seller reviewed and edited every
/// draft in the preview first), so imports go live as `active`.
const IMPORTED_PRODUCT_STATUS: &str = "active";

// ---------------------------------------------------------------------------
// Page linking
// ---------------------------------------------------------------------------

/// Validate and store the caller's Facebook Page link on their
/// store. The page id is Meta's numeric page id (letters are also
/// accepted — some pages carry a username-style id); the name is
/// display text the seller recognises. Both are seller-supplied
/// claims: with no Graph API token yet, the platform cannot
/// verify ownership, and the status endpoint says the link is
/// unverified by reporting `graph_api` separately.
pub async fn link_facebook_page_for_owner(
    pool: &PgPool,
    user: &AuthUser,
    page_id: &str,
    page_name: &str,
) -> Result<Store, ApiError> {
    let store = get_store_by_owner(pool, user.user_id).await?;
    let page_id = page_id.trim();
    if page_id.is_empty()
        || page_id.chars().count() > 64
        || !page_id.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(ApiError::bad_request(
            "page_id must be 1-64 letters or digits",
        ));
    }
    let page_name = page_name.trim();
    if page_name.is_empty() || page_name.chars().count() > 120 {
        return Err(ApiError::bad_request(
            "page_name must be 1-120 characters",
        ));
    }
    sqlx::query(
        "UPDATE stores SET facebook_page_id = $2, facebook_page_name = $3 WHERE id = $1",
    )
    .bind(store.id)
    .bind(page_id)
    .bind(page_name)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("link facebook page", e))?;
    get_store_by_id(pool, store.id).await
}

/// Remove the caller's Facebook Page link (both fields nulled).
pub async fn unlink_facebook_page_for_owner(
    pool: &PgPool,
    user: &AuthUser,
) -> Result<Store, ApiError> {
    let store = get_store_by_owner(pool, user.user_id).await?;
    sqlx::query(
        "UPDATE stores SET facebook_page_id = NULL, facebook_page_name = NULL WHERE id = $1",
    )
    .bind(store.id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("unlink facebook page", e))?;
    get_store_by_id(pool, store.id).await
}

/// The seller-facing Facebook status: whether a page is linked
/// (and which), plus whether the platform's Meta app credentials
/// exist (`graph_api`) — the two facts are independent: a seller
/// can link a page long before the Graph integration can use it.
#[derive(Debug, Serialize)]
pub struct FacebookStatus {
    pub linked: bool,
    pub page_id: Option<String>,
    pub page_name: Option<String>,
    /// `configured` when both FACEBOOK_APP_ID and
    /// FACEBOOK_APP_SECRET are present in the API's environment,
    /// else `not_configured`.
    pub graph_api: String,
}

pub async fn facebook_status_for_owner(
    state: &AppState,
    pool: &PgPool,
    user: &AuthUser,
) -> Result<FacebookStatus, ApiError> {
    let store = get_store_by_owner(pool, user.user_id).await?;
    Ok(FacebookStatus {
        linked: store.facebook_page_id.is_some(),
        page_id: store.facebook_page_id.clone(),
        page_name: store.facebook_page_name.clone(),
        graph_api: if state.facebook_config.graph_api_configured() {
            "configured".to_string()
        } else {
            "not_configured".to_string()
        },
    })
}

// ---------------------------------------------------------------------------
// Import preview
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ImportPostInput {
    pub source_ref: String,
    pub text: String,
}

#[derive(Deserialize)]
pub struct ImportPreviewRequest {
    pub posts: Vec<ImportPostInput>,
}

/// One parsed, editable product draft. `valid` says whether the
/// draft could be imported exactly as shown; `errors` names every
/// problem in seller-facing words (the UI highlights the row and
/// prints them).
#[derive(Debug, Serialize)]
pub struct ImportDraftView {
    pub source_ref: String,
    pub title: String,
    pub title_bn: Option<String>,
    pub description: Option<String>,
    pub price_poisha: Option<i64>,
    pub weight_grams: i64,
    pub stock: i32,
    pub valid: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportPreview {
    pub drafts: Vec<ImportDraftView>,
}

/// Every reason a draft (parsed or seller-edited) cannot be
/// imported, in one place so the preview and the import apply the
/// SAME rules: the shared domain product validator, plus the
/// import-only rule that a sellable product needs a real price —
/// a missing price, or a price of zero, means the post did not
/// name one the seller has confirmed.
fn draft_errors(title: &str, price_poisha: Option<i64>, stock: i32, weight_grams: i64) -> Vec<String> {
    let mut errors = Vec::new();
    match price_poisha {
        None => errors.push("no price found in the post text".to_string()),
        Some(price) if price <= 0 => {
            errors.push("price must be greater than zero".to_string())
        }
        _ => {}
    }
    let input = ProductInput {
        title: title.to_string(),
        price_poisha: price_poisha.unwrap_or(0),
        stock,
        status: IMPORTED_PRODUCT_STATUS.to_string(),
        weight_grams,
    };
    if let Err(message) = validate_product_input(&input) {
        errors.push(message);
    }
    errors
}

/// Parse pasted posts into drafts. Pure read: NO database writes —
/// the seller reviews and edits the drafts, and only the import
/// call creates products.
pub fn preview_import(posts: &[ImportPostInput]) -> Result<ImportPreview, ApiError> {
    if posts.len() > MAX_IMPORT_ITEMS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_IMPORT_ITEMS} posts per preview"
        )));
    }
    let mut drafts = Vec::with_capacity(posts.len());
    for post in posts {
        if post.source_ref.trim().is_empty() {
            return Err(ApiError::bad_request("every post needs a source_ref"));
        }
        if post.text.chars().count() > MAX_POST_TEXT_CHARS {
            return Err(ApiError::bad_request(format!(
                "post '{}' is longer than {MAX_POST_TEXT_CHARS} characters",
                post.source_ref
            )));
        }
        let parsed = draft_from_fb_post(&post.text);
        let errors = draft_errors(&parsed.title, parsed.price_poisha, 0, DEFAULT_WEIGHT_GRAMS);
        drafts.push(ImportDraftView {
            source_ref: post.source_ref.clone(),
            title: parsed.title,
            title_bn: None,
            description: if parsed.description.is_empty() {
                None
            } else {
                Some(parsed.description)
            },
            price_poisha: parsed.price_poisha,
            weight_grams: DEFAULT_WEIGHT_GRAMS,
            stock: 0,
            valid: errors.is_empty(),
            errors,
        });
    }
    Ok(ImportPreview { drafts })
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ImportDraftInput {
    pub source_ref: String,
    pub title: String,
    pub title_bn: Option<String>,
    pub description: Option<String>,
    pub price_poisha: i64,
    pub weight_grams: Option<i64>,
    pub stock: i32,
}

#[derive(Deserialize)]
pub struct ImportRequest {
    pub drafts: Vec<ImportDraftInput>,
}

/// One draft the import did not create, with the reason.
#[derive(Debug, Serialize)]
pub struct SkippedImport {
    pub source_ref: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub created: Vec<Product>,
    pub skipped: Vec<SkippedImport>,
}

/// Create products from seller-approved drafts. Every draft is
/// validated BEFORE anything is written (same rules as the
/// preview), drafts the store already imported are skipped, and
/// the surviving drafts are inserted in ONE transaction through
/// the shared product insert path, stamped
/// `source = 'facebook'` with their `source_ref`.
pub async fn import_products_for_owner(
    pool: &PgPool,
    user: &AuthUser,
    drafts: &[ImportDraftInput],
) -> Result<ImportResult, ApiError> {
    if drafts.len() > MAX_IMPORT_ITEMS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_IMPORT_ITEMS} drafts per import"
        )));
    }
    let store = get_store_by_owner(pool, user.user_id).await?;

    // Which of these refs does the store already carry? One query
    // for the whole batch — the idempotency check.
    let refs: Vec<String> = drafts.iter().map(|d| d.source_ref.trim().to_string()).collect();
    let existing: Vec<String> = sqlx::query_scalar(
        "SELECT source_ref FROM products WHERE store_id = $1 AND source_ref = ANY($2)",
    )
    .bind(store.id)
    .bind(&refs)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("check imported refs", e))?;
    let existing: HashSet<String> = existing.into_iter().collect();

    let mut claimed: HashSet<String> = HashSet::new();
    let mut ready: Vec<NewProduct> = Vec::new();
    let mut skipped: Vec<SkippedImport> = Vec::new();
    for draft in drafts {
        let source_ref = draft.source_ref.trim().to_string();
        if source_ref.is_empty() {
            skipped.push(SkippedImport {
                source_ref,
                reason: "source_ref must not be empty".to_string(),
            });
            continue;
        }
        if existing.contains(&source_ref) || claimed.contains(&source_ref) {
            skipped.push(SkippedImport {
                source_ref,
                reason: "already imported".to_string(),
            });
            continue;
        }
        let weight_grams = draft.weight_grams.unwrap_or(DEFAULT_WEIGHT_GRAMS);
        let errors = draft_errors(
            &draft.title,
            Some(draft.price_poisha),
            draft.stock,
            weight_grams,
        );
        if !errors.is_empty() {
            skipped.push(SkippedImport {
                source_ref,
                reason: errors.join("; "),
            });
            continue;
        }
        // The domain validator has already proven 1..=50,000.
        let weight_grams = i32::try_from(weight_grams)
            .map_err(|_| ApiError::bad_request("weight_grams out of range"))?;
        claimed.insert(source_ref.clone());
        ready.push(NewProduct {
            store_id: store.id,
            title: draft.title.trim().to_string(),
            title_bn: draft.title_bn.clone(),
            description: draft.description.clone(),
            price_poisha: draft.price_poisha,
            compare_at_poisha: None,
            stock: draft.stock,
            status: IMPORTED_PRODUCT_STATUS.to_string(),
            weight_grams,
            source: "facebook".to_string(),
            source_ref: Some(source_ref),
        });
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin import", e))?;
    let mut created_ids: Vec<Uuid> = Vec::with_capacity(ready.len());
    for product in &ready {
        created_ids.push(insert_product(&mut *tx, product).await?);
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit import", e))?;

    let mut created = Vec::with_capacity(created_ids.len());
    for id in created_ids {
        created.push(get_product_by_id(pool, id).await?);
    }
    Ok(ImportResult { created, skipped })
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LinkFacebookPageRequest {
    pub page_id: String,
    pub page_name: String,
}

pub async fn link_facebook_page(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<LinkFacebookPageRequest>,
) -> Result<Json<Store>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        link_facebook_page_for_owner(pool, &user, &req.page_id, &req.page_name).await?,
    ))
}

pub async fn unlink_facebook_page(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Store>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(unlink_facebook_page_for_owner(pool, &user).await?))
}

pub async fn facebook_status(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<FacebookStatus>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(facebook_status_for_owner(&state, pool, &user).await?))
}

pub async fn import_preview_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<ImportPreviewRequest>,
) -> Result<Json<ImportPreview>, ApiError> {
    let pool = state.require_pool()?;
    // Previewing is a seller operation on their own store; a user
    // without a store has nothing to import into.
    get_store_by_owner(pool, user.user_id).await?;
    Ok(Json(preview_import(&req.posts)?))
}

pub async fn import_products_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<ImportRequest>,
) -> Result<Json<ImportResult>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(import_products_for_owner(pool, &user, &req.drafts).await?))
}

/// The Graph API import source. Honest stubs only: without the
/// platform's Meta app credentials this is a 503; with them it is
/// still a 501, because fetching a seller's posts needs a page
/// access token and the OAuth flow that produces one lands in a
/// later wave. It NEVER returns posts it did not fetch.
pub async fn import_from_page(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !state.facebook_config.graph_api_configured() {
        return Err(ApiError::service_unavailable(
            "Facebook Graph API is not configured",
        ));
    }
    let pool = state.require_pool()?;
    get_store_by_owner(pool, user.user_id).await?;
    Err(ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "Facebook Graph API import is not available yet: fetching page posts \
         needs a page access token, and the OAuth flow lands in a later wave",
    ))
}
