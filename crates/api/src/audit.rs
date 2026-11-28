//! Admin audit log (Wave 28): one row per consequential admin
//! action, written by the ONE writer in this module
//! ([`record_admin_action`]) inside the SAME transaction as the
//! action itself — so the log can never claim an action that
//! rolled back, and never misses one that committed.
//!
//! The action vocabulary (string literals at the call sites,
//! listed here so the vocabulary lives in exactly one place):
//!
//! - `payout.approve`, `payout.reject`, `payout.mark_paid` —
//!   target `payout`, detail `{from, to}` (plus `note` on a
//!   rejection — the reason, required since the admin
//!   payouts remake — and `payment_reference` on a
//!   mark-paid: non-secret proof-of-payment context. Account
//!   numbers and phones are NEVER written here).
//! - `store.suspend`, `store.reactivate` — target `store`,
//!   detail `{status}`.
//! - `settings.update` — target `platform_settings`, detail
//!   `{changed_keys}`: the NAMES of the settings the patch
//!   changed, never their values.
//! - `verification.approve` (detail `{tier}`),
//!   `verification.reject` (detail `{note}` — the reviewer note
//!   the seller themselves will read) — target `store`.
//! - `qna.hide`, `qna.unhide` — target `question`, no detail.
//!   Only the ADMIN moderation endpoints are audited; a seller
//!   hiding a question on their own product is routine
//!   moderation, not a platform action.
//! - `return.refund_done` — target `return_request`, no detail.
//! - `category.create` (detail `{slug}`), `category.update`
//!   (detail `{changed_keys}` — key NAMES only, the settings
//!   pattern), `category.delete` (detail `{slug}`),
//!   `category.reorder` (detail `{count}`, no target),
//!   `category.merge` (target = the deleted source, detail
//!   `{target_slug, products_moved, children_moved}`),
//!   `category.import` (detail `{created, updated}`, no
//!   target) — target `category` (Wave 56).
//!
//! What is NEVER logged: secrets of any kind (passwords do not
//! exist here, but JWT secrets, gateway credentials, OTP codes
//! and full NID numbers are equally excluded) — detail payloads
//! carry identifiers and non-secret context only. The read side
//! shows the acting admin as a MASKED phone (`domain::mask_phone`),
//! matching how buyers are displayed in reviews and Q&A.

use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use domain::mask_phone;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::admin::require_admin;
use crate::auth::AuthUser;
use crate::products::clamp_pagination;
use crate::{ApiError, AppState};

/// Record one admin action in the audit log. Takes any sqlx
/// executor so callers pass their TRANSACTION (`&mut *tx`) and
/// the row commits or rolls back together with the action it
/// describes — that same-transaction property is the point of
/// the log. `detail` is serialised to JSON text and cast to
/// jsonb in SQL (the workspace's sqlx build has no `json`
/// feature; the cast keeps the column a real jsonb).
pub async fn record_admin_action(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    admin_user_id: Uuid,
    action: &str,
    target_type: &str,
    target_id: Option<&str>,
    detail: Option<serde_json::Value>,
) -> Result<(), ApiError> {
    let detail_text = detail.map(|value| value.to_string());
    sqlx::query(
        "INSERT INTO admin_audit_log (admin_user_id, action, target_type, target_id, detail) \
         VALUES ($1, $2, $3, $4, $5::jsonb)",
    )
    .bind(admin_user_id)
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(detail_text)
    .execute(executor)
    .await
    .map_err(|e| ApiError::internal("record admin audit action", e))?;
    Ok(())
}

/// One audit row as stored, plus the acting admin's phone
/// (joined from `users`); the phone is masked before the row
/// leaves this module.
#[derive(FromRow)]
struct AuditRow {
    id: Uuid,
    admin_user_id: Uuid,
    admin_phone: String,
    action: String,
    target_type: String,
    target_id: Option<String>,
    detail: Option<String>,
    created_at: DateTime<Utc>,
}

/// One audit entry as the admin UI sees it: the acting admin
/// identified by masked phone only.
#[derive(Debug, Serialize)]
pub struct AdminAuditEntry {
    pub id: Uuid,
    pub admin_user_id: Uuid,
    pub admin_phone_masked: String,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub detail: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

/// A page of audit entries plus the total count.
#[derive(Debug, Serialize)]
pub struct AdminAuditList {
    pub items: Vec<AdminAuditEntry>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// Read the audit log, newest first, optionally filtered to
/// one action. The detail column comes back as text and is
/// parsed here (see [`record_admin_action`] for why).
pub async fn list_audit_log(
    pool: &PgPool,
    action: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<AdminAuditList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let action = action.map(str::trim).filter(|s| !s.is_empty());

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM admin_audit_log \
         WHERE ($1::text IS NULL OR action = $1)",
    )
    .bind(action)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin audit log", e))?;

    let rows: Vec<AuditRow> = sqlx::query_as(
        "SELECT a.id, a.admin_user_id, u.phone AS admin_phone, a.action, \
         a.target_type, a.target_id, a.detail::text AS detail, a.created_at \
         FROM admin_audit_log a \
         JOIN users u ON u.id = a.admin_user_id \
         WHERE ($1::text IS NULL OR a.action = $1) \
         ORDER BY a.created_at DESC, a.id DESC LIMIT $2 OFFSET $3",
    )
    .bind(action)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin audit log", e))?;

    let items = rows
        .into_iter()
        .map(|row| AdminAuditEntry {
            id: row.id,
            admin_user_id: row.admin_user_id,
            admin_phone_masked: mask_phone(&row.admin_phone),
            action: row.action,
            target_type: row.target_type,
            target_id: row.target_id,
            detail: row
                .detail
                .and_then(|text| serde_json::from_str(&text).ok()),
            created_at: row.created_at,
        })
        .collect();

    Ok(AdminAuditList {
        items,
        total,
        page,
        limit,
    })
}

/// The audit history of ONE target (admin payouts remake:
/// the payout detail shows every recorded decision on that
/// payout). Newest first, at most `limit` entries. The
/// masking and detail parsing are the shared read path's —
/// this reader exists so no other module re-implements them.
pub async fn list_audit_for_target(
    pool: &PgPool,
    target_type: &str,
    target_id: &str,
    limit: i64,
) -> Result<Vec<AdminAuditEntry>, ApiError> {
    let rows: Vec<AuditRow> = sqlx::query_as(
        "SELECT a.id, a.admin_user_id, u.phone AS admin_phone, a.action, \
         a.target_type, a.target_id, a.detail::text AS detail, a.created_at \
         FROM admin_audit_log a \
         JOIN users u ON u.id = a.admin_user_id \
         WHERE a.target_type = $1 AND a.target_id = $2 \
         ORDER BY a.created_at DESC, a.id DESC LIMIT $3",
    )
    .bind(target_type)
    .bind(target_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin audit log for target", e))?;

    Ok(rows
        .into_iter()
        .map(|row| AdminAuditEntry {
            id: row.id,
            admin_user_id: row.admin_user_id,
            admin_phone_masked: mask_phone(&row.admin_phone),
            action: row.action,
            target_type: row.target_type,
            target_id: row.target_id,
            detail: row
                .detail
                .and_then(|text| serde_json::from_str(&text).ok()),
            created_at: row.created_at,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Handlers (thin: guard -> call one service function -> respond)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AdminAuditQuery {
    pub page: Option<i64>,
    /// Page size; `per_page` is accepted as an alias of the
    /// codebase-wide `limit` parameter.
    pub limit: Option<i64>,
    pub per_page: Option<i64>,
    pub action: Option<String>,
}

pub async fn list_audit_log_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminAuditQuery>,
) -> Result<Json<AdminAuditList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let limit = query.per_page.or(query.limit);
    Ok(Json(
        list_audit_log(pool, query.action.as_deref(), query.page, limit).await?,
    ))
}
