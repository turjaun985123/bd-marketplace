//! Seller KYC & verification tiers (Wave 21; admin
//! verifications remake).
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
//! NID handling is the sensitive part of this module, so the
//! rule is absolute: the FULL NID number is stored once (in
//! `store_verifications.nid_number`) and returned by exactly
//! ONE endpoint — the admin full view below, which exists
//! because a reviewer cannot check a number against a document
//! image without seeing it. Since the admin verifications
//! remake that endpoint is a POST carrying a mandatory,
//! domain-validated `reason`, and it writes its
//! `verification.view_full_nid` audit row (reason + last4 +
//! application status — never the number) in a transaction
//! over the locked row BEFORE the response is built: no
//! committed audit row, no number. Every other surface
//! carries only the masked form, and the masked read paths
//! (admin queue, seller view, payout summary) no longer even
//! SELECT the full number — they read the stored `nid_last4`
//! plus the number's length and rebuild the mask with
//! `domain::mask_nid_parts` (migration 0074 enforces
//! `nid_last4 = right(nid_number, 4)` at the database).
//!
//! Decisions are transactional in the platform's one lock
//! order (store row first, then the verification row, both
//! `FOR UPDATE` — the payouts remake's `transition_payout`
//! shape): the transition is re-checked under the lock, an
//! approval on a suspended store is a 409, and the caller
//! must present the `expected_version` they reviewed, so a
//! decision can never land on a payload that changed after
//! the review. Submissions take the same locks, stamp the
//! submitting owner, and append a row to
//! `store_verification_events` (submissions only — decisions
//! and full-NID views live in the admin audit log, the one
//! canonical decision record). Every decision also enqueues
//! the seller's outcome notice inside the same transaction.
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
    http::{header, HeaderMap, HeaderValue},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    can_transition_verification, mask_nid_parts, mask_phone, normalize_bd_phone,
    search_like_pattern, validate_applicant_name, validate_branding_url, validate_nid_number,
    validate_reviewer_note, validate_tier, validate_trade_license, validate_view_reason,
    NOTIFICATION_VERIFICATION_APPROVED, NOTIFICATION_VERIFICATION_REJECTED,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::admin::require_admin;
use crate::audit::record_admin_action;
use crate::auth::AuthUser;
use crate::notices::enqueue_verification_notice;
use crate::stores::Store;
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
    /// The applicant name as on the NID, when supplied
    /// (admin verifications remake; older applications and
    /// sellers who have not resubmitted carry `None`).
    pub applicant_name: Option<String>,
    /// The application's optimistic-concurrency version
    /// (admin verifications remake); `None` when the store
    /// has never applied.
    pub version: Option<i64>,
}

/// One row of the admin review queue: enough to triage and to
/// open the document, with the owner's phone and the NID both
/// masked. The full NID is deliberately absent — it is behind
/// the separate full-view endpoint — and so is the trade
/// license NUMBER (only its presence is a queue signal; the
/// number itself rides the full view).
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
    /// The application's version — the value a decision must
    /// present back as `expected_version`.
    pub version: i64,
    /// The name as on the NID, when the seller supplied one.
    pub applicant_name: Option<String>,
    /// active | suspended — a suspended store's approval is
    /// blocked, and the queue must say so before the click.
    pub store_status: String,
    pub store_created_at: DateTime<Utc>,
    /// Decision context (admin verifications remake): the
    /// store's live footprint, computed with the platform's
    /// canonical definitions — active products only, delivered
    /// sub-orders, GMV as goods − discount + delivery over
    /// delivered sub-orders, received returns, and payouts
    /// still in `requested` (the canonical pending bucket).
    pub product_count: i64,
    pub delivered_orders: i64,
    pub delivered_gmv_poisha: i64,
    pub returns_received: i64,
    pub pending_payout_count: i64,
    pub pending_payout_poisha: i64,
    /// Other stores — owned by a DIFFERENT owner — whose
    /// current application carries the same NID / trade
    /// license number. Counts only, never identities: the
    /// same proprietor reusing their own NID across their own
    /// stores is legitimate and is NOT counted; a stranger's
    /// store carrying it is the fraud signal.
    pub duplicate_nid_stores: i64,
    pub duplicate_license_stores: i64,
    /// Lifetime recorded rejections of this store's
    /// applications (admin audit log count — decisions made
    /// before the Wave 28 audit log existed are not counted;
    /// the README says so).
    pub prior_rejections: i64,
}

/// Per-status counts over the WHOLE queue (the payouts
/// remake's summary rule: counts never inherit the list's
/// status/search filters, so the filter chips and the
/// filtered `total` can never contradict each other). Only
/// applications are counted — a store that never applied has
/// no row and no honest bucket here.
#[derive(Debug, Serialize)]
pub struct AdminVerificationSummary {
    pub pending: i64,
    pub approved: i64,
    pub rejected: i64,
}

/// The admin review queue page (admin verifications remake):
/// the payouts list envelope — `items`, the FILTERED `total`,
/// the echoed `page`/`limit`, and the unfiltered `summary`.
#[derive(Debug, Serialize)]
pub struct AdminVerificationList {
    pub items: Vec<AdminVerificationItem>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
    pub summary: AdminVerificationSummary,
}

/// One entry of a store's verification history on the admin
/// detail view: a submission event (from
/// `store_verification_events`) or a decision/view (from the
/// admin audit log), normalised to one shape. Actor phones
/// are masked — this is a read surface like any other.
#[derive(Debug, Serialize)]
pub struct VerificationHistoryEntry {
    pub at: DateTime<Utc>,
    /// submitted | resubmitted | approved | rejected |
    /// viewed_full_nid.
    pub kind: String,
    pub actor_phone_masked: Option<String>,
    /// The rejection note, for `rejected` entries.
    pub note: Option<String>,
    /// The granted tier, for `approved` entries.
    pub tier: Option<String>,
    /// The stated reason, for `viewed_full_nid` entries.
    pub reason: Option<String>,
}

/// One of the application owner's OTHER stores, on the admin
/// detail view: enough to spot a proprietor's legitimate
/// multi-store footprint (and to tell it apart from a
/// stranger reusing the same documents).
#[derive(Debug, Serialize)]
pub struct OwnerStoreSummary {
    pub store_id: Uuid,
    pub store_slug: String,
    pub store_name: String,
    pub tier: String,
    /// The store's application status, or `none`.
    pub verification_status: String,
}

/// The admin detail view for one store's application: the
/// queue item, the merged submission/decision history, and
/// the owner's other stores. Masked by construction — the
/// full NID and the trade license number are NOT here; they
/// ride the audited full view only.
#[derive(Debug, Serialize)]
pub struct AdminVerificationDetail {
    #[serde(flatten)]
    pub item: AdminVerificationItem,
    pub history: Vec<VerificationHistoryEntry>,
    pub owner_stores: Vec<OwnerStoreSummary>,
}

/// The admin full view: the ONE response in the whole API
/// that carries a complete NID number (plus the trade license
/// number the reviewer must check for the `verified` tier).
/// It exists so a reviewer can compare the numbers against
/// the submitted document image; every other endpoint is
/// structurally unable to leak them. Served only by the
/// audited POST (see the module docs).
#[derive(Debug, Serialize)]
pub struct AdminVerificationFull {
    pub store_id: Uuid,
    pub nid_number: String,
    pub trade_license_number: Option<String>,
    pub applicant_name: Option<String>,
    /// The application version this snapshot was taken at —
    /// the value a following decision presents as
    /// `expected_version`.
    pub version: i64,
    pub status: String,
}

/// A `store_verifications` row in its MASKED projection
/// (internal — never serialized directly; the views above are
/// built from it). The full NID is not a field: masked paths
/// hold `nid_last4` + the number's length, which is all
/// `mask_nid_parts` needs.
#[derive(Debug, FromRow)]
struct VerificationRow {
    store_id: Uuid,
    status: String,
    nid_last4: String,
    nid_length: i64,
    trade_license_number: Option<String>,
    document_url: Option<String>,
    reviewer_note: Option<String>,
    submitted_at: DateTime<Utc>,
    reviewed_at: Option<DateTime<Utc>>,
    version: i64,
    applicant_name: Option<String>,
    submitted_by_user_id: Option<Uuid>,
    reviewed_by_user_id: Option<Uuid>,
}

const VERIFICATION_COLUMNS: &str = "store_id, status, nid_last4, \
    char_length(nid_number)::bigint AS nid_length, trade_license_number, \
    document_url, reviewer_note, submitted_at, reviewed_at, version, \
    applicant_name, submitted_by_user_id, reviewed_by_user_id";

/// Load a store's application row (masked projection), if it
/// has ever applied.
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

/// The same masked projection, locked `FOR UPDATE` inside a
/// decision/submission transaction.
async fn load_verification_for_update(
    conn: &mut PgConnection,
    store_id: Uuid,
) -> Result<Option<VerificationRow>, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {VERIFICATION_COLUMNS} FROM store_verifications \
         WHERE store_id = $1 FOR UPDATE"
    ))
    .bind(store_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load store verification for update", e))
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
            applicant_name: None,
            version: None,
        },
        Some(row) => SellerVerificationView {
            status: row.status,
            tier: tier.to_string(),
            nid_masked: Some(mask_nid_parts(row.nid_length as usize, &row.nid_last4)),
            trade_license_number: row.trade_license_number,
            document_url: row.document_url,
            reviewer_note: row.reviewer_note,
            submitted_at: Some(row.submitted_at),
            reviewed_at: row.reviewed_at,
            applicant_name: row.applicant_name,
            version: Some(row.version),
        },
    }
}

// ---------------------------------------------------------------------------
// Seller side
// ---------------------------------------------------------------------------

/// One store's verification state, for its owner: the store
/// (resolved by the handler — the caller's own by default,
/// or one named by `?store_id=` under `manage_verification`,
/// owner-only since the admin verifications remake) plus its
/// application row.
pub async fn get_seller_verification(
    pool: &PgPool,
    store: &Store,
) -> Result<SellerVerificationView, ApiError> {
    let row = load_verification(pool, store.id).await?;
    Ok(seller_view(&store.verified_tier, row))
}

/// The verification summary for an admin surface that holds
/// only a store id + tier (admin payouts remake: the payout
/// detail shows the deciding admin whether the store is
/// verified and where an application stands). Returns the
/// seller view verbatim — masked by construction, and a
/// store that never applied reads as status `none`, never
/// an error: "no application" is a normal state, not a
/// missing resource.
pub async fn admin_verification_summary(
    pool: &PgPool,
    store_id: Uuid,
    verified_tier: &str,
) -> Result<SellerVerificationView, ApiError> {
    let row = load_verification(pool, store_id).await?;
    Ok(seller_view(verified_tier, row))
}

/// What a seller submits to apply (or re-apply) for verification.
#[derive(Debug, Deserialize)]
pub struct SubmitVerificationInput {
    pub nid_number: String,
    pub trade_license_number: Option<String>,
    pub document_url: Option<String>,
    /// The name as it appears on the NID (admin verifications
    /// remake). Optional: the seller page predates the field,
    /// and a missing name must not reject an otherwise valid
    /// application — the reviewer still has the document.
    pub applicant_name: Option<String>,
}

/// Append one submission event inside the caller's
/// transaction. Events carry the last4 only — never the full
/// NID, and never a hash of it (a hash of a 10/13/17-digit
/// number is a brute-forceable copy; see migration 0074).
#[allow(clippy::too_many_arguments)]
async fn record_verification_event(
    conn: &mut PgConnection,
    store_id: Uuid,
    actor_user_id: Uuid,
    kind: &str,
    nid_last4: &str,
    has_trade_license: bool,
    document_url: Option<&str>,
    applicant_name: Option<&str>,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO store_verification_events \
           (store_id, actor_user_id, kind, nid_last4, has_trade_license, \
            document_url, applicant_name) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(store_id)
    .bind(actor_user_id)
    .bind(kind)
    .bind(nid_last4)
    .bind(has_trade_license)
    .bind(document_url)
    .bind(applicant_name)
    .execute(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("record verification event", e))?;
    Ok(())
}

/// Submit (or resubmit) the caller's verification application.
///
/// The application's status after a successful submit is
/// always `pending`. From `none` and from `rejected` that is
/// the domain transition table's move; a still-`pending`
/// application is REPLACED in place instead — the seller is
/// correcting the same application, not filing a second one —
/// and any previous reviewer note is cleared either way. An
/// `approved` store cannot re-apply (409): its tier is
/// settled, and changing it is a fresh admin action, out of
/// scope for applications. Every value is domain-validated
/// before anything is written, so an invalid submit changes
/// nothing.
///
/// Remake discipline: the write runs in ONE transaction that
/// locks the store row first and the application row second
/// (the platform's lock order — the same order decisions
/// take, so a submit and a decision serialise instead of
/// racing). A payload-CHANGING edit of a pending application
/// bumps `version` and appends a `resubmitted` event but
/// keeps the original `submitted_at` (the application's
/// queue seniority belongs to the application, not to its
/// latest typo fix); an IDENTICAL resubmit of a pending
/// application is a pure no-op — no version bump, no event,
/// no timestamp movement. The submitting owner is stamped on
/// the row and the event.
pub async fn submit_verification(
    pool: &PgPool,
    store: &Store,
    submitter_user_id: Uuid,
    input: SubmitVerificationInput,
) -> Result<SellerVerificationView, ApiError> {
    let nid_number = validate_nid_number(&input.nid_number).map_err(ApiError::bad_request)?;
    let trade_license =
        validate_trade_license(input.trade_license_number.as_deref()).map_err(ApiError::bad_request)?;
    let document_url = validate_branding_url(input.document_url.as_deref()).map_err(|e| {
        ApiError::bad_request(format!(
            "ডকুমেন্টের URL গ্রহণযোগ্য নয় (document URL rejected): {e}"
        ))
    })?;
    let applicant_name =
        validate_applicant_name(input.applicant_name.as_deref()).map_err(ApiError::bad_request)?;
    let nid_last4: String = {
        let chars: Vec<char> = nid_number.chars().collect();
        chars[chars.len() - 4..].iter().collect()
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin submit verification", e))?;
    // Lock order, platform-wide: the store row first, then
    // the application row — identical to the decision path,
    // so the two can never deadlock or interleave.
    sqlx::query("SELECT id FROM stores WHERE id = $1 FOR UPDATE")
        .bind(store.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("lock store for verification submit", e))?;
    let existing = load_verification_for_update(&mut tx, store.id).await?;
    let current_status = existing
        .as_ref()
        .map(|row| row.status.as_str())
        .unwrap_or("none");
    if current_status == "approved" {
        return Err(ApiError::conflict(
            "এই স্টোরটি ইতিমধ্যে যাচাইকৃত — নতুন আবেদনের প্রয়োজন নেই (this store is already verified; a new application is not needed)",
        ));
    }

    if current_status == "pending" {
        // Identical-payload check: the NID comparison happens
        // INSIDE the database (a boolean out, the number never
        // leaves), the rest compares on the locked row.
        let row = existing.as_ref().expect("pending implies a row");
        let same_nid: bool = sqlx::query_scalar(
            "SELECT nid_number = $2 FROM store_verifications WHERE store_id = $1",
        )
        .bind(store.id)
        .bind(&nid_number)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("compare submitted NID", e))?;
        let identical = same_nid
            && row.trade_license_number == trade_license
            && row.document_url == document_url
            && row.applicant_name == applicant_name;
        if identical {
            let view = seller_view(&store.verified_tier, existing);
            tx.commit()
                .await
                .map_err(|e| ApiError::internal("commit verification no-op submit", e))?;
            return Ok(view);
        }
        sqlx::query(
            "UPDATE store_verifications SET \
               nid_number = $2, nid_last4 = $3, trade_license_number = $4, \
               document_url = $5, applicant_name = $6, \
               submitted_by_user_id = $7, version = version + 1 \
             WHERE store_id = $1",
        )
        .bind(store.id)
        .bind(&nid_number)
        .bind(&nid_last4)
        .bind(&trade_license)
        .bind(&document_url)
        .bind(&applicant_name)
        .bind(submitter_user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("edit pending verification application", e))?;
        record_verification_event(
            &mut tx,
            store.id,
            submitter_user_id,
            "resubmitted",
            &nid_last4,
            trade_license.is_some(),
            document_url.as_deref(),
            applicant_name.as_deref(),
        )
        .await?;
    } else {
        if !can_transition_verification(current_status, "pending") {
            return Err(ApiError::conflict(format!(
                "এই অবস্থা থেকে আবেদন জমা দেওয়া যাবে না: '{current_status}' (cannot submit a verification application from '{current_status}')"
            )));
        }
        sqlx::query(
            "INSERT INTO store_verifications \
               (store_id, status, nid_number, nid_last4, trade_license_number, \
                document_url, reviewer_note, submitted_at, reviewed_at, \
                applicant_name, submitted_by_user_id, reviewed_by_user_id) \
             VALUES ($1, 'pending', $2, $3, $4, $5, NULL, now(), NULL, $6, $7, NULL) \
             ON CONFLICT (store_id) DO UPDATE SET \
               status = 'pending', \
               nid_number = EXCLUDED.nid_number, \
               nid_last4 = EXCLUDED.nid_last4, \
               trade_license_number = EXCLUDED.trade_license_number, \
               document_url = EXCLUDED.document_url, \
               reviewer_note = NULL, \
               submitted_at = now(), \
               reviewed_at = NULL, \
               version = store_verifications.version + 1, \
               applicant_name = EXCLUDED.applicant_name, \
               submitted_by_user_id = EXCLUDED.submitted_by_user_id, \
               reviewed_by_user_id = NULL",
        )
        .bind(store.id)
        .bind(&nid_number)
        .bind(&nid_last4)
        .bind(&trade_license)
        .bind(&document_url)
        .bind(&applicant_name)
        .bind(submitter_user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("submit verification application", e))?;
        record_verification_event(
            &mut tx,
            store.id,
            submitter_user_id,
            "submitted",
            &nid_last4,
            trade_license.is_some(),
            document_url.as_deref(),
            applicant_name.as_deref(),
        )
        .await?;
    }

    let row = load_verification_for_update(&mut tx, store.id).await?;
    let view = seller_view(&store.verified_tier, row);
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit verification submit", e))?;
    Ok(view)
}

// ---------------------------------------------------------------------------
// Admin side
// ---------------------------------------------------------------------------

/// One queue row as selected from the database (internal).
/// `trade_license_number` is selected only to derive
/// `has_trade_license` — it is never serialised from here.
#[derive(Debug, FromRow)]
struct AdminVerificationRow {
    store_id: Uuid,
    store_slug: String,
    store_name: String,
    owner_phone: String,
    status: String,
    tier: String,
    nid_last4: String,
    nid_length: i64,
    trade_license_number: Option<String>,
    document_url: Option<String>,
    reviewer_note: Option<String>,
    submitted_at: DateTime<Utc>,
    reviewed_at: Option<DateTime<Utc>>,
    version: i64,
    applicant_name: Option<String>,
    store_status: String,
    store_created_at: DateTime<Utc>,
    product_count: i64,
    delivered_orders: i64,
    delivered_gmv_poisha: i64,
    returns_received: i64,
    pending_payout_count: i64,
    pending_payout_poisha: i64,
    duplicate_nid_stores: i64,
    duplicate_license_stores: i64,
    prior_rejections: i64,
}

/// The queue/detail item projection: base columns plus the
/// decision-context aggregates as scalar subqueries over the
/// SAME row set (one round trip, no per-row helper calls —
/// the payouts remake's batching rule). SUM over bigint
/// columns is cast back to bigint explicitly.
const ADMIN_ITEM_COLUMNS: &str = "v.store_id, s.slug AS store_slug, \
    s.name AS store_name, u.phone AS owner_phone, v.status, \
    s.verified_tier AS tier, v.nid_last4, \
    char_length(v.nid_number)::bigint AS nid_length, \
    v.trade_license_number, v.document_url, v.reviewer_note, \
    v.submitted_at, v.reviewed_at, v.version, v.applicant_name, \
    s.status AS store_status, s.created_at AS store_created_at, \
    (SELECT COUNT(*) FROM products p \
      WHERE p.store_id = s.id AND p.status = 'active') AS product_count, \
    (SELECT COUNT(*) FROM sub_orders so \
      WHERE so.store_id = s.id AND so.status = 'delivered') AS delivered_orders, \
    (SELECT COALESCE(SUM(so.goods_subtotal_poisha - so.discount_poisha \
      + so.delivery_charge_poisha), 0)::bigint FROM sub_orders so \
      WHERE so.store_id = s.id AND so.status = 'delivered') AS delivered_gmv_poisha, \
    (SELECT COUNT(*) FROM sub_orders so \
      WHERE so.store_id = s.id AND so.status = 'returned') AS returns_received, \
    (SELECT COUNT(*) FROM payouts po \
      WHERE po.store_id = s.id AND po.status = 'requested') AS pending_payout_count, \
    (SELECT COALESCE(SUM(po.amount_poisha), 0)::bigint FROM payouts po \
      WHERE po.store_id = s.id AND po.status = 'requested') AS pending_payout_poisha, \
    (SELECT COUNT(*) FROM store_verifications v2 \
      JOIN stores s2 ON s2.id = v2.store_id \
      WHERE v2.nid_number = v.nid_number AND v2.store_id <> v.store_id \
        AND s2.owner_user_id <> s.owner_user_id) AS duplicate_nid_stores, \
    (SELECT COUNT(*) FROM store_verifications v3 \
      JOIN stores s3 ON s3.id = v3.store_id \
      WHERE v.trade_license_number IS NOT NULL \
        AND v3.trade_license_number = v.trade_license_number \
        AND v3.store_id <> v.store_id \
        AND s3.owner_user_id <> s.owner_user_id) AS duplicate_license_stores, \
    (SELECT COUNT(*) FROM admin_audit_log a \
      WHERE a.target_type = 'store' AND a.target_id = v.store_id::text \
        AND a.action = 'verification.reject') AS prior_rejections";

const ADMIN_ITEM_FROM: &str = "FROM store_verifications v \
    JOIN stores s ON s.id = v.store_id \
    JOIN users u ON u.id = s.owner_user_id";

fn item_from_row(row: AdminVerificationRow) -> AdminVerificationItem {
    AdminVerificationItem {
        store_id: row.store_id,
        store_slug: row.store_slug,
        store_name: row.store_name,
        owner_phone_masked: mask_phone(&row.owner_phone),
        status: row.status,
        tier: row.tier,
        nid_masked: mask_nid_parts(row.nid_length as usize, &row.nid_last4),
        has_trade_license: row.trade_license_number.is_some(),
        document_url: row.document_url,
        reviewer_note: row.reviewer_note,
        submitted_at: row.submitted_at,
        reviewed_at: row.reviewed_at,
        version: row.version,
        applicant_name: row.applicant_name,
        store_status: row.store_status,
        store_created_at: row.store_created_at,
        product_count: row.product_count,
        delivered_orders: row.delivered_orders,
        delivered_gmv_poisha: row.delivered_gmv_poisha,
        returns_received: row.returns_received,
        pending_payout_count: row.pending_payout_count,
        pending_payout_poisha: row.pending_payout_poisha,
        duplicate_nid_stores: row.duplicate_nid_stores,
        duplicate_license_stores: row.duplicate_license_stores,
        prior_rejections: row.prior_rejections,
    }
}

/// The admin review queue (admin verifications remake).
/// `status` filters by application status: `pending` (the
/// default — the work queue), `approved`, `rejected`, or
/// `all`. An unknown filter is a 400, never a silently-empty
/// list. `q` searches store name/slug, applicant name and the
/// owner's phone (local or canonical form) — it NEVER matches
/// the NID or trade license numbers: a search that could
/// confirm guessed fragments of an identity number would be
/// a probing oracle around the full view's audit gate.
/// Ordering is oldest-first with a store_id tiebreak, so
/// page boundaries are deterministic.
pub async fn list_admin_verifications(
    pool: &PgPool,
    status: Option<&str>,
    q: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminVerificationList, ApiError> {
    let status = status.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("pending");
    if status != "all" && !matches!(status, "pending" | "approved" | "rejected") {
        return Err(ApiError::bad_request(
            "status অবশ্যই pending, approved, rejected বা all হতে হবে (status must be pending, approved, rejected or all)",
        ));
    }
    let filter: Option<&str> = if status == "all" { None } else { Some(status) };
    let page = page.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * limit;

    let query_text = q.map(str::trim).filter(|s| !s.is_empty());
    let pattern = query_text.map(search_like_pattern);
    let canonical_phone = query_text.and_then(normalize_bd_phone);

    // Count and page share their predicates verbatim (the
    // payouts remake's rule — a drift here makes `total` lie
    // about the pages).
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) {ADMIN_ITEM_FROM} \
         WHERE ($1::text IS NULL OR v.status = $1) \
           AND ($2::text IS NULL OR s.name ILIKE $2 OR s.slug ILIKE $2 \
                OR v.applicant_name ILIKE $2 OR u.phone ILIKE $2 \
                OR ($3::text IS NOT NULL AND u.phone = $3))"
    ))
    .bind(filter)
    .bind(pattern.as_deref())
    .bind(canonical_phone.as_deref())
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count verifications", e))?;

    let rows: Vec<AdminVerificationRow> = sqlx::query_as(&format!(
        "SELECT {ADMIN_ITEM_COLUMNS} {ADMIN_ITEM_FROM} \
         WHERE ($1::text IS NULL OR v.status = $1) \
           AND ($2::text IS NULL OR s.name ILIKE $2 OR s.slug ILIKE $2 \
                OR v.applicant_name ILIKE $2 OR u.phone ILIKE $2 \
                OR ($3::text IS NOT NULL AND u.phone = $3)) \
         ORDER BY v.submitted_at ASC, v.store_id ASC \
         LIMIT $4 OFFSET $5"
    ))
    .bind(filter)
    .bind(pattern.as_deref())
    .bind(canonical_phone.as_deref())
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list verifications", e))?;

    // Whole-queue summary: no status/search inheritance.
    let summary_rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*) FROM store_verifications GROUP BY status",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("summarise verifications", e))?;
    let mut summary = AdminVerificationSummary {
        pending: 0,
        approved: 0,
        rejected: 0,
    };
    for (status_name, count) in summary_rows {
        match status_name.as_str() {
            "pending" => summary.pending = count,
            "approved" => summary.approved = count,
            "rejected" => summary.rejected = count,
            _ => {}
        }
    }

    Ok(AdminVerificationList {
        items: rows.into_iter().map(item_from_row).collect(),
        total,
        page,
        limit,
        summary,
    })
}

/// The admin detail for one store's application. 404 —
/// identically — for an unknown store and for a store that
/// has never applied: the detail is about an APPLICATION,
/// and "no application" is not a view (contrast
/// `admin_verification_summary`, the payouts surface, where
/// `none` is the correct answer).
pub async fn get_admin_verification_detail(
    pool: &PgPool,
    store_id: Uuid,
) -> Result<AdminVerificationDetail, ApiError> {
    let row: AdminVerificationRow = sqlx::query_as(&format!(
        "SELECT {ADMIN_ITEM_COLUMNS} {ADMIN_ITEM_FROM} WHERE v.store_id = $1"
    ))
    .bind(store_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load verification detail", e))?
    .ok_or_else(|| {
        ApiError::not_found(
            "এই স্টোরের কোনো যাচাইকরণ আবেদন নেই (this store has no verification application)",
        )
    })?;
    let item = item_from_row(row);

    // History: submission events + this store's verification
    // audit rows, merged newest-first. The audit slice is
    // filtered IN SQL (`verification.%`) — filtering a capped
    // all-actions list in Rust would silently drop decisions
    // for stores with many unrelated audit rows.
    let mut history: Vec<VerificationHistoryEntry> = Vec::new();
    let events: Vec<(String, DateTime<Utc>, Option<String>)> = sqlx::query_as(
        "SELECT e.kind, e.created_at, u.phone \
         FROM store_verification_events e \
         LEFT JOIN users u ON u.id = e.actor_user_id \
         WHERE e.store_id = $1 ORDER BY e.created_at DESC, e.id DESC LIMIT 50",
    )
    .bind(store_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load verification events", e))?;
    for (kind, at, phone) in events {
        history.push(VerificationHistoryEntry {
            at,
            kind,
            actor_phone_masked: phone.as_deref().map(mask_phone),
            note: None,
            tier: None,
            reason: None,
        });
    }
    let audit_rows: Vec<(String, DateTime<Utc>, Option<String>, Option<serde_json::Value>)> =
        sqlx::query_as(
            "SELECT a.action, a.created_at, u.phone, a.detail \
             FROM admin_audit_log a \
             LEFT JOIN users u ON u.id = a.admin_user_id \
             WHERE a.target_type = 'store' AND a.target_id = $1 \
               AND a.action LIKE 'verification.%' \
             ORDER BY a.created_at DESC LIMIT 50",
        )
        .bind(store_id.to_string())
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("load verification audit history", e))?;
    for (action, at, phone, detail) in audit_rows {
        let (kind, note, tier, reason) = match action.as_str() {
            "verification.approve" => (
                "approved",
                None,
                detail
                    .as_ref()
                    .and_then(|d| d.get("tier"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                None,
            ),
            "verification.reject" => (
                "rejected",
                detail
                    .as_ref()
                    .and_then(|d| d.get("note"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                None,
                None,
            ),
            "verification.view_full_nid" => (
                "viewed_full_nid",
                None,
                None,
                detail
                    .as_ref()
                    .and_then(|d| d.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            ),
            _ => continue,
        };
        history.push(VerificationHistoryEntry {
            at,
            kind: kind.to_string(),
            actor_phone_masked: phone.as_deref().map(mask_phone),
            note,
            tier,
            reason,
        });
    }
    history.sort_by(|a, b| b.at.cmp(&a.at));
    history.truncate(50);

    let owner_stores: Vec<OwnerStoreSummary> = sqlx::query_as::<_, (Uuid, String, String, String, Option<String>)>(
        "SELECT s.id, s.slug, s.name, s.verified_tier, v.status \
         FROM stores s LEFT JOIN store_verifications v ON v.store_id = s.id \
         WHERE s.owner_user_id = (SELECT owner_user_id FROM stores WHERE id = $1) \
         ORDER BY s.created_at ASC",
    )
    .bind(store_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load owner stores", e))?
    .into_iter()
    .map(|(id, slug, name, tier, status)| OwnerStoreSummary {
        store_id: id,
        store_slug: slug,
        store_name: name,
        tier,
        verification_status: status.unwrap_or_else(|| "none".to_string()),
    })
    .collect();

    Ok(AdminVerificationDetail {
        item,
        history,
        owner_stores,
    })
}

/// The full row for the audited full view (internal — the
/// ONLY projection in the module that holds the number).
#[derive(Debug, FromRow)]
struct FullVerificationRow {
    store_id: Uuid,
    status: String,
    nid_number: String,
    trade_license_number: Option<String>,
    applicant_name: Option<String>,
    version: i64,
}

/// The full NID for one store's application — THE single
/// place the number leaves the database (see the module
/// docs). The caller must state a reason
/// (`domain::validate_view_reason`: meaningful length, and
/// never containing a long digit run — a pasted NID in the
/// audit detail would defeat the whole design). The row is
/// locked, the `verification.view_full_nid` audit row is
/// written, and the transaction COMMITS before the response
/// is built from that locked snapshot: no committed audit
/// row, no number; and the audited snapshot is exactly the
/// number returned, even if the seller resubmits a moment
/// later. 404 when the store has never applied.
pub async fn get_verification_full_nid(
    pool: &PgPool,
    admin_id: Uuid,
    store_id: Uuid,
    reason: &str,
) -> Result<AdminVerificationFull, ApiError> {
    let reason = validate_view_reason(reason).map_err(ApiError::bad_request)?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin full NID view", e))?;
    let row: FullVerificationRow = sqlx::query_as(
        "SELECT store_id, status, nid_number, trade_license_number, \
         applicant_name, version \
         FROM store_verifications WHERE store_id = $1 FOR UPDATE",
    )
    .bind(store_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("load verification for full view", e))?
    .ok_or_else(|| {
        ApiError::not_found(
            "এই স্টোরের কোনো যাচাইকরণ আবেদন নেই (this store has no verification application)",
        )
    })?;
    let nid_last4: String = {
        let chars: Vec<char> = row.nid_number.chars().collect();
        chars[chars.len() - 4..].iter().collect()
    };
    record_admin_action(
        &mut *tx,
        admin_id,
        "verification.view_full_nid",
        "store",
        Some(&store_id.to_string()),
        Some(serde_json::json!({
            "reason": reason,
            "nid_last4": nid_last4,
            "status": row.status,
        })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit full NID view audit", e))?;

    Ok(AdminVerificationFull {
        store_id: row.store_id,
        nid_number: row.nid_number,
        trade_license_number: row.trade_license_number,
        applicant_name: row.applicant_name,
        version: row.version,
        status: row.status,
    })
}

/// Lock the store row (returning its status + tier) and then
/// the application row, in the platform's one lock order —
/// the shared opening of both decision paths.
async fn lock_store_and_application(
    conn: &mut PgConnection,
    store_id: Uuid,
) -> Result<(String, String, Option<VerificationRow>), ApiError> {
    let store_row: Option<(String, String)> =
        sqlx::query_as("SELECT status, verified_tier FROM stores WHERE id = $1 FOR UPDATE")
            .bind(store_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| ApiError::internal("lock store for verification decision", e))?;
    let (store_status, store_tier) = store_row.ok_or_else(|| {
        ApiError::not_found(
            "এই স্টোরের কোনো যাচাইকরণ আবেদন নেই (this store has no verification application)",
        )
    })?;
    let row = load_verification_for_update(conn, store_id).await?;
    Ok((store_status, store_tier, row))
}

/// The version gate shared by both decisions: the caller must
/// present the version they reviewed, and it must still be
/// the application's version under the lock. A mismatch means
/// the payload changed after the review — the decision would
/// land on documents nobody saw — so it is a 409 that asks
/// for a reload, never a silent proceed.
fn require_expected_version(
    expected: Option<i64>,
    current: i64,
) -> Result<(), ApiError> {
    let expected = expected.ok_or_else(|| {
        ApiError::bad_request(
            "expected_version আবশ্যক — পেজটি রিলোড করে আবার চেষ্টা করুন (expected_version is required; reload the application and try again)",
        )
    })?;
    if expected != current {
        return Err(ApiError::conflict(
            "আবেদনটি আপনার দেখার পর বদলে গেছে — রিলোড করে নতুন সংস্করণটি আবার দেখুন (the application changed after you loaded it; reload and review the new version before deciding)",
        ));
    }
    Ok(())
}

/// Approve a store's pending application at a tier (`basic`
/// or `verified`). The `verified` tier REQUIRES a trade
/// license on the application — approving a license-less
/// seller as a verified business would print a badge the
/// documents do not support, so it is a 400. Approval also
/// requires the submitted document image: the platform's one
/// authenticity claim is that a human compared the number
/// against the document, so a document-less approval is a
/// 400 unless the reviewer explicitly overrides (the
/// override is recorded in the audit detail). Approval on a
/// suspended store is a 409 (the payouts freeze rule).
/// Approval stamps the review, sets the application's status
/// and raises the store's tier in one transaction, and
/// enqueues the seller's approval notice in the same
/// transaction. A store with no application is a 404; a
/// decided one is a 409.
pub async fn approve_verification(
    pool: &PgPool,
    admin_id: Uuid,
    store_id: Uuid,
    tier: &str,
    expected_version: Option<i64>,
    no_document_override: bool,
) -> Result<SellerVerificationView, ApiError> {
    let tier = validate_tier(tier).map_err(ApiError::bad_request)?;
    if tier == "unverified" {
        return Err(ApiError::bad_request(
            "অনুমোদনের স্তর 'basic' বা 'verified' হতে হবে (approval tier must be 'basic' or 'verified')",
        ));
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin approve verification", e))?;
    let (store_status, _store_tier, row) =
        lock_store_and_application(&mut tx, store_id).await?;
    let row = row.ok_or_else(|| {
        ApiError::not_found(
            "এই স্টোরের কোনো যাচাইকরণ আবেদন নেই (this store has no verification application)",
        )
    })?;
    if !can_transition_verification(&row.status, "approved") {
        return Err(ApiError::conflict(format!(
            "এই অবস্থার আবেদন অনুমোদন করা যাবে না: '{}' (cannot approve a verification application in status '{}')",
            row.status, row.status
        )));
    }
    require_expected_version(expected_version, row.version)?;
    if store_status == "suspended" {
        return Err(ApiError::conflict(
            "স্টোরটি সাসপেন্ডেড আছে — যাচাইকরণের সিদ্ধান্ত স্থগিত; আগে স্টোরটি সক্রিয় করুন (this store is suspended — verification decisions are frozen while suspended; reactivate the store first)",
        ));
    }
    if tier == "verified" && row.trade_license_number.is_none() {
        return Err(ApiError::bad_request(
            "'verified' স্তরের জন্য আবেদনে ট্রেড লাইসেন্স থাকা আবশ্যক; 'basic' স্তরে অনুমোদন করুন অথবা বিক্রেতাকে লাইসেন্স যোগ করতে বলুন (the 'verified' tier requires a trade license on the application; approve at 'basic' or ask the seller to add one)",
        ));
    }
    let document_present = row.document_url.is_some();
    if !document_present && !no_document_override {
        return Err(ApiError::bad_request(
            "এই আবেদনে কোনো ডকুমেন্টের ছবি নেই — ডকুমেন্ট ছাড়া অনুমোদন করতে হলে ওভাররাইড স্পষ্টভাবে নিশ্চিত করুন (this application has no document image; approving without one requires the explicit no-document override)",
        ));
    }

    sqlx::query(
        "UPDATE store_verifications SET status = 'approved', reviewed_at = now(), \
         reviewed_by_user_id = $2, version = version + 1 \
         WHERE store_id = $1",
    )
    .bind(store_id)
    .bind(admin_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("approve verification", e))?;
    sqlx::query("UPDATE stores SET verified_tier = $2 WHERE id = $1")
        .bind(store_id)
        .bind(tier)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("set store verified tier", e))?;
    // The decision joins the admin audit log inside the same
    // transaction. The detail carries the granted tier, the
    // document facts and the version decided at — never the
    // NID or license numbers.
    record_admin_action(
        &mut *tx,
        admin_id,
        "verification.approve",
        "store",
        Some(&store_id.to_string()),
        Some(serde_json::json!({
            "tier": tier,
            "document_present": document_present,
            "no_document_override": !document_present && no_document_override,
            "version": row.version,
        })),
    )
    .await?;
    enqueue_verification_notice(
        &mut tx,
        store_id,
        NOTIFICATION_VERIFICATION_APPROVED,
        Some(tier),
        None,
    )
    .await?;
    let updated = load_verification_for_update(&mut tx, store_id).await?;
    let view = seller_view(tier, updated);
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit approve verification", e))?;
    Ok(view)
}

/// Reject a store's pending application with a note the
/// seller can act on (the note is required and
/// domain-validated). The store's tier does not move — a
/// rejected application grants nothing. Rejection stays
/// available on a suspended store (the payouts precedent:
/// rejection releases the seller; freezing it would trap
/// them). The update, its audit row and the seller's
/// rejection notice commit together. A store with no
/// application is a 404; a decided one is a 409.
pub async fn reject_verification(
    pool: &PgPool,
    admin_id: Uuid,
    store_id: Uuid,
    note: &str,
    expected_version: Option<i64>,
) -> Result<SellerVerificationView, ApiError> {
    let note = validate_reviewer_note(note).map_err(ApiError::bad_request)?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin reject verification", e))?;
    let (_store_status, store_tier, row) =
        lock_store_and_application(&mut tx, store_id).await?;
    let row = row.ok_or_else(|| {
        ApiError::not_found(
            "এই স্টোরের কোনো যাচাইকরণ আবেদন নেই (this store has no verification application)",
        )
    })?;
    if !can_transition_verification(&row.status, "rejected") {
        return Err(ApiError::conflict(format!(
            "এই অবস্থার আবেদন প্রত্যাখ্যান করা যাবে না: '{}' (cannot reject a verification application in status '{}')",
            row.status, row.status
        )));
    }
    require_expected_version(expected_version, row.version)?;

    sqlx::query(
        "UPDATE store_verifications \
         SET status = 'rejected', reviewer_note = $2, reviewed_at = now(), \
             reviewed_by_user_id = $3, version = version + 1 \
         WHERE store_id = $1",
    )
    .bind(store_id)
    .bind(&note)
    .bind(admin_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("reject verification", e))?;
    // The audit detail carries the reviewer note — the same
    // text the seller reads on their own view; no document
    // numbers are ever written to the log.
    record_admin_action(
        &mut *tx,
        admin_id,
        "verification.reject",
        "store",
        Some(&store_id.to_string()),
        Some(serde_json::json!({ "note": note, "version": row.version })),
    )
    .await?;
    enqueue_verification_notice(
        &mut tx,
        store_id,
        NOTIFICATION_VERIFICATION_REJECTED,
        None,
        Some(&note),
    )
    .await?;
    let updated = load_verification_for_update(&mut tx, store_id).await?;
    let view = seller_view(&store_tier, updated);
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit reject verification", e))?;
    Ok(view)
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AdminVerificationsQuery {
    pub status: Option<String>,
    pub q: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct FullVerificationInput {
    pub reason: String,
}

#[derive(Deserialize)]
pub struct ApproveVerificationInput {
    pub tier: String,
    pub expected_version: Option<i64>,
    pub no_document_override: Option<bool>,
}

#[derive(Deserialize)]
pub struct RejectVerificationInput {
    pub note: String,
    pub expected_version: Option<i64>,
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn get_seller_verification_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<crate::access::StoreScopeQuery>,
) -> Result<Json<SellerVerificationView>, ApiError> {
    let pool = state.require_pool()?;
    // Admin verifications remake: verification is the OWNER's
    // identity file — reading or submitting it resolves under
    // `manage_verification`, which the domain matrix grants
    // to owners only (Wave 34's shared store-settings action
    // also reaches managers, who must not see the owner's
    // document or license). Without `?store_id=`, the
    // caller's own store.
    let (store, _role) = crate::access::resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        domain::STORE_ACTION_MANAGE_VERIFICATION,
    )
    .await?;
    Ok(Json(get_seller_verification(pool, &store).await?))
}

pub async fn submit_seller_verification_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<crate::access::StoreScopeQuery>,
    Json(input): Json<SubmitVerificationInput>,
) -> Result<Json<SellerVerificationView>, ApiError> {
    let pool = state.require_pool()?;
    let (store, _role) = crate::access::resolve_scoped_store(
        pool,
        user.user_id,
        scope.store_id,
        domain::STORE_ACTION_MANAGE_VERIFICATION,
    )
    .await?;
    Ok(Json(
        submit_verification(pool, &store, user.user_id, input).await?,
    ))
}

pub async fn list_admin_verifications_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminVerificationsQuery>,
) -> Result<Json<AdminVerificationList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_admin_verifications(
            pool,
            query.status.as_deref(),
            query.q.as_deref(),
            query.page,
            query.limit,
        )
        .await?,
    ))
}

pub async fn get_admin_verification_detail_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(store_id): Path<Uuid>,
) -> Result<Json<AdminVerificationDetail>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(get_admin_verification_detail(pool, store_id).await?))
}

pub async fn get_admin_verification_full_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(store_id): Path<Uuid>,
    Json(input): Json<FullVerificationInput>,
) -> Result<(HeaderMap, Json<AdminVerificationFull>), ApiError> {
    let admin = require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let full = get_verification_full_nid(pool, admin.id, store_id, &input.reason).await?;
    // The number must never be cached — by browsers, shared
    // caches, or a service worker (the global security
    // headers set no Cache-Control, so this response sets
    // its own).
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    Ok((headers, Json(full)))
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
        approve_verification(
            pool,
            admin.id,
            store_id,
            &input.tier,
            input.expected_version,
            input.no_document_override.unwrap_or(false),
        )
        .await?,
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
        reject_verification(pool, admin.id, store_id, &input.note, input.expected_version)
            .await?,
    ))
}
