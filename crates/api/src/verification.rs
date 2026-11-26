//! Seller KYC & verification tiers (Wave 21).
//!
//! A store's `verified_tier` is trust metadata the platform
//! grants after a human reviews the seller's documents:
//!
//! - `unverified` — every store at birth; nothing submitted or
//!   nothing approved yet.
//! - `basic` — the seller's NID was submitted and approved.
//! - `verified` — NID plus a trade license were approved.
//!
//! The flow is deliberately manual: the seller submits an
//! application (NID number, optional trade license number,
//! optional hotlinked document image), an admin approves it at
//! a tier or rejects it with a note the seller can act on, and
//! a rejected seller fixes the problem and resubmits. The
//! domain crate owns the rules (NID shape, masking, the
//! transition table); this module owns the rows.
//!
//! NID handling is the sensitive part of this wave, so the
//! rule is absolute: the FULL NID number is stored once (in
//! `store_verifications.nid_number`) and returned by exactly
//! ONE endpoint — the admin full view below, which exists
//! because a reviewer cannot check a number against a document
//! image without seeing it. Every other surface carries only
//! the masked form (`domain::mask_nid` over the number, backed
//! by the stored `nid_last4`): the seller's own view, the
//! admin queue list, and of course the public store view,
//! which carries the tier and nothing about the NID at all.
//!
//! The tier also feeds the payout gate (Wave 21, in the
//! `payouts` module): when the `kyc_payout_gate` platform
//! setting is `on`, an `unverified` store cannot request a
//! payout. The gate defaults to off so existing sellers are
//! never surprised by a rule that did not exist when they
//! joined; the platform turns it on when it is ready to
//! enforce KYC for money movement.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    can_transition_verification, mask_nid, mask_phone, validate_branding_url, validate_nid_number,
    validate_reviewer_note, validate_tier, validate_trade_license,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::admin::require_admin;
use crate::audit::record_admin_action;
use crate::auth::AuthUser;
use crate::stores::{get_store_by_id, get_store_by_owner};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Views — none of these can even hold a full NID, by construction
// ---------------------------------------------------------------------------

/// The seller's own verification view: where their application
/// stands and what the store's tier is. `nid_masked` is the
/// ONLY form of the NID here — the seller typed the number,
/// but the API never echoes an identity document back in full.
#[derive(Debug, Serialize)]
pub struct SellerVerificationView {
    /// none | pending | approved | rejected (`none` = never applied).
    pub status: String,
    /// The store's current tier (unverified | basic | verified).
    pub tier: String,
    pub nid_masked: Option<String>,
    pub trade_license_number: Option<String>,
    pub document_url: Option<String>,
    /// The reviewer's rejection reason, when rejected.
    pub reviewer_note: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub reviewed_at: Option<DateTime<Utc>>,
}

/// One row of the admin review queue: enough to triage and to
/// open the document, with the owner's phone and the NID both
/// masked. The full NID is deliberately absent — it is behind
/// the separate full-view endpoint.
#[derive(Debug, Serialize)]
pub struct AdminVerificationItem {
    pub store_id: Uuid,
    pub store_slug: String,
    pub store_name: String,
    /// The store owner's phone, masked via `domain::mask_phone`.
    pub owner_phone_masked: String,
    /// pending | approved | rejected.
    pub status: String,
    /// The store's current tier.
    pub tier: String,
    pub nid_masked: String,
    pub has_trade_license: bool,
    pub document_url: Option<String>,
    pub reviewer_note: Option<String>,
    pub submitted_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
}

/// The admin full view: the ONE response in the whole API that
/// carries a complete NID number. It exists so a reviewer can
/// compare the number against the submitted document image;
/// every other endpoint is structurally unable to leak it.
#[derive(Debug, Serialize)]
pub struct AdminVerificationFull {
    pub store_id: Uuid,
    pub nid_number: String,
}

/// A `store_verifications` row, as stored (internal — never
/// serialized directly; the views above are built from it).
#[derive(Debug, FromRow)]
struct VerificationRow {
    store_id: Uuid,
    status: String,
    nid_number: String,
    trade_license_number: Option<String>,
    document_url: Option<String>,
    reviewer_note: Option<String>,
    submitted_at: DateTime<Utc>,
    reviewed_at: Option<DateTime<Utc>>,
}

const VERIFICATION_COLUMNS: &str = "store_id, status, nid_number, trade_license_number, \
    document_url, reviewer_note, submitted_at, reviewed_at";

/// Load a store's application row, if it has ever applied.
async fn load_verification(
    pool: &PgPool,
    store_id: Uuid,
) -> Result<Option<VerificationRow>, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {VERIFICATION_COLUMNS} FROM store_verifications WHERE store_id = $1"
    ))
    .bind(store_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load store verification", e))
}

/// Build the seller view from the store's tier and its
/// (optional) application row.
fn seller_view(tier: &str, row: Option<VerificationRow>) -> SellerVerificationView {
    match row {
        None => SellerVerificationView {
            status: "none".to_string(),
            tier: tier.to_string(),
            nid_masked: None,
            trade_license_number: None,
            document_url: None,
            reviewer_note: None,
            submitted_at: None,
            reviewed_at: None,
        },
        Some(row) => SellerVerificationView {
            status: row.status,
            tier: tier.to_string(),
            nid_masked: Some(mask_nid(&row.nid_number)),
            trade_license_number: row.trade_license_number,
            document_url: row.document_url,
            reviewer_note: row.reviewer_note,
            submitted_at: Some(row.submitted_at),
            reviewed_at: row.reviewed_at,
        },
    }
}

// ---------------------------------------------------------------------------
// Seller side
// ---------------------------------------------------------------------------

/// The caller's own verification state: their store (404 when
/// they have none) plus its application row.
pub async fn get_seller_verification(
    pool: &PgPool,
    user: &AuthUser,
) -> Result<SellerVerificationView, ApiError> {
    let store = get_store_by_owner(pool, user.user_id).await?;
    let row = load_verification(pool, store.id).await?;
    Ok(seller_view(&store.verified_tier, row))
}

/// What a seller submits to apply (or re-apply) for verification.
#[derive(Debug, Deserialize)]
pub struct SubmitVerificationInput {
    pub nid_number: String,
    pub trade_license_number: Option<String>,
    pub document_url: Option<String>,
}

/// Submit (or resubmit) the caller's verification application.
///
/// The application's status after a successful submit is always
/// `pending`. From `none` and from `rejected` that is the
/// domain transition table's move; a still-`pending`
/// application is REPLACED in place instead — the seller is
/// correcting the same application, not filing a second one —
/// and any previous reviewer note is cleared either way. An
/// `approved` store cannot re-apply (409): its tier is
/// settled, and changing it is a fresh admin action, out of
/// scope for applications. Every value is domain-validated
/// before anything is written, so an invalid submit changes
/// nothing.
pub async fn submit_verification(
    pool: &PgPool,
    user: &AuthUser,
    input: SubmitVerificationInput,
) -> Result<SellerVerificationView, ApiError> {
    let store = get_store_by_owner(pool, user.user_id).await?;

    let nid_number = validate_nid_number(&input.nid_number).map_err(ApiError::bad_request)?;
    let trade_license =
        validate_trade_license(input.trade_license_number.as_deref()).map_err(ApiError::bad_request)?;
    let document_url =
        validate_branding_url(input.document_url.as_deref()).map_err(ApiError::bad_request)?;
    let nid_last4: String = nid_number.chars().skip(nid_number.len() - 4).collect();

    let existing = load_verification(pool, store.id).await?;
    let current_status = existing
        .as_ref()
        .map(|row| row.status.as_str())
        .unwrap_or("none");
    if current_status == "approved" {
        return Err(ApiError::conflict(
            "this store is already verified; a new application is not needed",
        ));
    }
    if current_status != "pending"
        && !can_transition_verification(current_status, "pending")
    {
        return Err(ApiError::conflict(format!(
            "cannot submit a verification application from '{current_status}'"
        )));
    }

    sqlx::query(
        "INSERT INTO store_verifications \
           (store_id, status, nid_number, nid_last4, trade_license_number, \
            document_url, reviewer_note, submitted_at, reviewed_at) \
         VALUES ($1, 'pending', $2, $3, $4, $5, NULL, now(), NULL) \
         ON CONFLICT (store_id) DO UPDATE SET \
           status = 'pending', \
           nid_number = EXCLUDED.nid_number, \
           nid_last4 = EXCLUDED.nid_last4, \
           trade_license_number = EXCLUDED.trade_license_number, \
           document_url = EXCLUDED.document_url, \
           reviewer_note = NULL, \
           submitted_at = now(), \
           reviewed_at = NULL",
    )
    .bind(store.id)
    .bind(&nid_number)
    .bind(&nid_last4)
    .bind(&trade_license)
    .bind(&document_url)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("submit verification application", e))?;

    let row = load_verification(pool, store.id).await?;
    Ok(seller_view(&store.verified_tier, row))
}

// ---------------------------------------------------------------------------
// Admin side
// ---------------------------------------------------------------------------

/// One queue row as selected from the database (internal).
#[derive(Debug, FromRow)]
struct AdminVerificationRow {
    store_id: Uuid,
    store_slug: String,
    store_name: String,
    owner_phone: String,
    status: String,
    tier: String,
    nid_number: String,
    trade_license_number: Option<String>,
    document_url: Option<String>,
    reviewer_note: Option<String>,
    submitted_at: DateTime<Utc>,
    reviewed_at: Option<DateTime<Utc>>,
}

/// The admin review queue. `status` filters by application
/// status: `pending` (the default — the work queue), `approved`,
/// `rejected`, or `all`. An unknown filter is a 400, never a
/// silently-empty list.
pub async fn list_admin_verifications(
    pool: &PgPool,
    status: Option<&str>,
) -> Result<Vec<AdminVerificationItem>, ApiError> {
    let status = status.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("pending");
    if status != "all" && !matches!(status, "pending" | "approved" | "rejected") {
        return Err(ApiError::bad_request(
            "status must be pending, approved, rejected or all",
        ));
    }
    let filter: Option<&str> = if status == "all" { None } else { Some(status) };

    let rows: Vec<AdminVerificationRow> = sqlx::query_as(
        "SELECT v.store_id, s.slug AS store_slug, s.name AS store_name, \
         u.phone AS owner_phone, v.status, s.verified_tier AS tier, \
         v.nid_number, v.trade_license_number, v.document_url, \
         v.reviewer_note, v.submitted_at, v.reviewed_at \
         FROM store_verifications v \
         JOIN stores s ON s.id = v.store_id \
         JOIN users u ON u.id = s.owner_user_id \
         WHERE ($1::text IS NULL OR v.status = $1) \
         ORDER BY v.submitted_at ASC",
    )
    .bind(filter)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list verifications", e))?;

    Ok(rows
        .into_iter()
        .map(|row| AdminVerificationItem {
            store_id: row.store_id,
            store_slug: row.store_slug,
            store_name: row.store_name,
            owner_phone_masked: mask_phone(&row.owner_phone),
            status: row.status,
            tier: row.tier,
            nid_masked: mask_nid(&row.nid_number),
            has_trade_license: row.trade_license_number.is_some(),
            document_url: row.document_url,
            reviewer_note: row.reviewer_note,
            submitted_at: row.submitted_at,
            reviewed_at: row.reviewed_at,
        })
        .collect())
}

/// The full NID for one store's application — THE single place
/// the number leaves the database (see the module docs). 404
/// when the store has never applied.
pub async fn get_verification_full_nid(
    pool: &PgPool,
    store_id: Uuid,
) -> Result<AdminVerificationFull, ApiError> {
    let row = load_verification(pool, store_id)
        .await?
        .ok_or_else(|| ApiError::not_found("this store has no verification application"))?;
    Ok(AdminVerificationFull {
        store_id: row.store_id,
        nid_number: row.nid_number,
    })
}

/// Approve a store's pending application at a tier (`basic` or
/// `verified`). The `verified` tier REQUIRES a trade license
/// on the application — approving a license-less seller as a
/// verified business would print a badge the documents do not
/// support, so it is a 400. Approval stamps the review, sets
/// the application's status and raises the store's tier in one
/// transaction. A store with no pending application is a 409.
pub async fn approve_verification(
    pool: &PgPool,
    admin_id: Uuid,
    store_id: Uuid,
    tier: &str,
) -> Result<SellerVerificationView, ApiError> {
    let tier = validate_tier(tier).map_err(ApiError::bad_request)?;
    if tier == "unverified" {
        return Err(ApiError::bad_request(
            "approval tier must be 'basic' or 'verified'",
        ));
    }
    let row = load_verification(pool, store_id)
        .await?
        .ok_or_else(|| ApiError::not_found("this store has no verification application"))?;
    if !can_transition_verification(&row.status, "approved") {
        return Err(ApiError::conflict(format!(
            "cannot approve a verification application in status '{}'",
            row.status
        )));
    }
    if tier == "verified" && row.trade_license_number.is_none() {
        return Err(ApiError::bad_request(
            "the 'verified' tier requires a trade license on the application; \
             approve at 'basic' or ask the seller to add one",
        ));
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin approve verification", e))?;
    sqlx::query(
        "UPDATE store_verifications SET status = 'approved', reviewed_at = now() \
         WHERE store_id = $1",
    )
    .bind(store_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("approve verification", e))?;
    sqlx::query("UPDATE stores SET verified_tier = $2 WHERE id = $1")
        .bind(store_id)
        .bind(tier)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("set store verified tier", e))?;
    // Wave 28: the decision joins the admin audit log inside
    // the same transaction. The detail carries the granted
    // tier only — never the NID or license numbers.
    record_admin_action(
        &mut *tx,
        admin_id,
        "verification.approve",
        "store",
        Some(&store_id.to_string()),
        Some(serde_json::json!({ "tier": tier })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit approve verification", e))?;

    let store = get_store_by_id(pool, store_id).await?;
    let row = load_verification(pool, store_id).await?;
    Ok(seller_view(&store.verified_tier, row))
}

/// Reject a store's pending application with a note the seller
/// can act on (the note is required and domain-validated). The
/// store's tier does not move — a rejected application grants
/// nothing. A store with no pending application is a 409.
pub async fn reject_verification(
    pool: &PgPool,
    admin_id: Uuid,
    store_id: Uuid,
    note: &str,
) -> Result<SellerVerificationView, ApiError> {
    let note = validate_reviewer_note(note).map_err(ApiError::bad_request)?;
    let row = load_verification(pool, store_id)
        .await?
        .ok_or_else(|| ApiError::not_found("this store has no verification application"))?;
    if !can_transition_verification(&row.status, "rejected") {
        return Err(ApiError::conflict(format!(
            "cannot reject a verification application in status '{}'",
            row.status
        )));
    }

    // Wave 28: the update and its audit row commit together.
    // The audit detail carries the reviewer note — the same
    // text the seller reads on their own view; no document
    // numbers are ever written to the log.
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin reject verification", e))?;
    sqlx::query(
        "UPDATE store_verifications \
         SET status = 'rejected', reviewer_note = $2, reviewed_at = now() \
         WHERE store_id = $1",
    )
    .bind(store_id)
    .bind(&note)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("reject verification", e))?;
    record_admin_action(
        &mut *tx,
        admin_id,
        "verification.reject",
        "store",
        Some(&store_id.to_string()),
        Some(serde_json::json!({ "note": note })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit reject verification", e))?;

    let store = get_store_by_id(pool, store_id).await?;
    let row = load_verification(pool, store_id).await?;
    Ok(seller_view(&store.verified_tier, row))
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AdminVerificationsQuery {
    pub status: Option<String>,
}

#[derive(Deserialize)]
pub struct ApproveVerificationInput {
    pub tier: String,
}

#[derive(Deserialize)]
pub struct RejectVerificationInput {
    pub note: String,
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn get_seller_verification_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<SellerVerificationView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_seller_verification(pool, &user).await?))
}

pub async fn submit_seller_verification_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<SubmitVerificationInput>,
) -> Result<Json<SellerVerificationView>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(submit_verification(pool, &user, input).await?))
}

pub async fn list_admin_verifications_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminVerificationsQuery>,
) -> Result<Json<Vec<AdminVerificationItem>>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_admin_verifications(pool, query.status.as_deref()).await?,
    ))
}

pub async fn get_admin_verification_full_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(store_id): Path<Uuid>,
) -> Result<Json<AdminVerificationFull>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(get_verification_full_nid(pool, store_id).await?))
}

pub async fn approve_verification_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(store_id): Path<Uuid>,
    Json(input): Json<ApproveVerificationInput>,
) -> Result<Json<SellerVerificationView>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        approve_verification(pool, admin.id, store_id, &input.tier).await?,
    ))
}

pub async fn reject_verification_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(store_id): Path<Uuid>,
    Json(input): Json<RejectVerificationInput>,
) -> Result<Json<SellerVerificationView>, ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        reject_verification(pool, admin.id, store_id, &input.note).await?,
    ))
}
