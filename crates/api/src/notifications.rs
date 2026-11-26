//! Admin notifications view (Wave 29): the read side of the
//! outbox. The counts show the queue's health at a glance —
//! above all `failed`, the rows that exhausted their send
//! attempts (in a deployment without an SMS gateway, every
//! row lands there with `sms_not_configured` on it: visible,
//! counted, never silently dropped). Phone numbers are
//! MASKED (`domain::mask_phone`): an operator diagnosing a
//! failed delivery needs the kind, the error and the shape
//! of the number, not the number itself.

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
use crate::{ApiError, AppState};

/// How many queued notifications sit in each status.
#[derive(Debug, Serialize, FromRow)]
pub struct NotificationCounts {
    pub pending: i64,
    pub sending: i64,
    pub sent: i64,
    pub failed: i64,
}

/// One outbox row as the admin sees it (phone masked).
#[derive(Debug, Serialize)]
pub struct AdminNotification {
    pub id: Uuid,
    pub kind: String,
    pub phone_masked: String,
    /// pending | sending | sent | failed
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
}

/// The admin notifications response: queue counts plus the
/// most recent rows (optionally one status only).
#[derive(Debug, Serialize)]
pub struct AdminNotificationList {
    pub counts: NotificationCounts,
    pub items: Vec<AdminNotification>,
}

#[derive(FromRow)]
struct NotificationRow {
    id: Uuid,
    kind: String,
    phone: String,
    status: String,
    attempts: i32,
    last_error: Option<String>,
    created_at: DateTime<Utc>,
    sent_at: Option<DateTime<Utc>>,
}

/// Read the outbox for the admin view: counts over ALL rows,
/// then the newest `limit` rows (optionally filtered to one
/// status). The limit is clamped to 1..=100 so the view
/// stays a glance, not an export.
pub async fn list_notifications(
    pool: &PgPool,
    status: Option<&str>,
    limit: Option<i64>,
) -> Result<AdminNotificationList, ApiError> {
    let limit = limit.unwrap_or(50).clamp(1, 100);
    let status = status.map(str::trim).filter(|s| !s.is_empty());

    let counts: NotificationCounts = sqlx::query_as(
        "SELECT \
         (COUNT(*) FILTER (WHERE status = 'pending'))::bigint AS pending, \
         (COUNT(*) FILTER (WHERE status = 'sending'))::bigint AS sending, \
         (COUNT(*) FILTER (WHERE status = 'sent'))::bigint AS sent, \
         (COUNT(*) FILTER (WHERE status = 'failed'))::bigint AS failed \
         FROM notifications",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count notifications", e))?;

    let rows: Vec<NotificationRow> = sqlx::query_as(
        "SELECT id, kind, phone, status, attempts, last_error, created_at, sent_at \
         FROM notifications \
         WHERE ($1::text IS NULL OR status = $1) \
         ORDER BY created_at DESC, id DESC LIMIT $2",
    )
    .bind(status)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list notifications", e))?;

    let items = rows
        .into_iter()
        .map(|row| AdminNotification {
            id: row.id,
            kind: row.kind,
            phone_masked: mask_phone(&row.phone),
            status: row.status,
            attempts: row.attempts,
            last_error: row.last_error,
            created_at: row.created_at,
            sent_at: row.sent_at,
        })
        .collect();

    Ok(AdminNotificationList { counts, items })
}

// ---------------------------------------------------------------------------
// Handlers (thin: guard -> call one service function -> respond)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AdminNotificationsQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
}

pub async fn list_notifications_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminNotificationsQuery>,
) -> Result<Json<AdminNotificationList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_notifications(pool, query.status.as_deref(), query.limit).await?,
    ))
}
