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

// ---------------------------------------------------------------------------
// The user's own inbox (Wave 54 extension)
// ---------------------------------------------------------------------------
//
// The admin view above watches the QUEUE; this is the
// other side of the same table — the signed-in user's
// own notifications as an in-app inbox (the follow
// feature's new-product notices land here, alongside
// order and payout notices). A row is the user's copy:
// they see its rendered body in full (it was written for
// them) and mark it read; `read_at` (migration 0054) is
// their state and never touches delivery.

/// One notification in the owner's inbox: the rendered
/// body, its delivery status, and the owner's read state.
#[derive(Debug, Serialize, FromRow)]
pub struct UserNotification {
    pub id: Uuid,
    pub kind: String,
    pub body: String,
    /// pending | sending | sent | failed (delivery, not read state)
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
    pub read_at: Option<DateTime<Utc>>,
}

/// One page of the owner's inbox plus their unread total
/// (over ALL their rows, not just this page — the badge
/// number).
#[derive(Debug, Serialize)]
pub struct UserNotificationList {
    pub items: Vec<UserNotification>,
    pub total: i64,
    pub unread_count: i64,
    pub page: i64,
    pub limit: i64,
}

/// The owner's inbox, newest first. Ownership is the
/// WHERE clause: a user can only ever read their own
/// rows, so there is no per-row check to forget.
pub async fn list_user_notifications(
    pool: &PgPool,
    user: &AuthUser,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<UserNotificationList, ApiError> {
    let (page, limit) = crate::products::clamp_pagination(page, limit);
    let offset = (page - 1) * limit;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1")
        .bind(user.user_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count user notifications", e))?;
    let unread_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND read_at IS NULL",
    )
    .bind(user.user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count unread notifications", e))?;
    let items: Vec<UserNotification> = sqlx::query_as(
        "SELECT id, kind, body, status, created_at, sent_at, read_at \
         FROM notifications WHERE user_id = $1 \
         ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3",
    )
    .bind(user.user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list user notifications", e))?;
    Ok(UserNotificationList {
        items,
        total,
        unread_count,
        page,
        limit,
    })
}

/// Mark ONE of the owner's notifications read. Idempotent:
/// the first read time stands (COALESCE), and marking a
/// row that is not the owner's (or does not exist) is a
/// 404 — the row is simply not in their inbox.
pub async fn mark_notification_read(
    pool: &PgPool,
    user: &AuthUser,
    notification_id: Uuid,
) -> Result<UserNotification, ApiError> {
    sqlx::query_as::<_, UserNotification>(
        "UPDATE notifications SET read_at = COALESCE(read_at, now()) \
         WHERE id = $1 AND user_id = $2 \
         RETURNING id, kind, body, status, created_at, sent_at, read_at",
    )
    .bind(notification_id)
    .bind(user.user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("mark notification read", e))?
    .ok_or_else(|| ApiError::not_found("notification not found"))
}

/// Mark every unread notification of the owner's read;
/// returns how many rows changed.
pub async fn mark_all_notifications_read(
    pool: &PgPool,
    user: &AuthUser,
) -> Result<i64, ApiError> {
    let result = sqlx::query(
        "UPDATE notifications SET read_at = now() \
         WHERE user_id = $1 AND read_at IS NULL",
    )
    .bind(user.user_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("mark all notifications read", e))?;
    Ok(result.rows_affected() as i64)
}

#[derive(Deserialize)]
pub struct UserNotificationsQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn list_user_notifications_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<UserNotificationsQuery>,
) -> Result<Json<UserNotificationList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_user_notifications(pool, &user, query.page, query.limit).await?,
    ))
}

pub async fn mark_notification_read_handler(
    State(state): State<AppState>,
    user: AuthUser,
    axum::extract::Path(notification_id): axum::extract::Path<Uuid>,
) -> Result<Json<UserNotification>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        mark_notification_read(pool, &user, notification_id).await?,
    ))
}

/// The mark-all response: how many rows were newly read.
#[derive(Debug, Serialize)]
pub struct MarkAllReadResult {
    pub marked_read: i64,
}

pub async fn mark_all_notifications_read_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<MarkAllReadResult>, ApiError> {
    let pool = state.require_pool()?;
    let marked_read = mark_all_notifications_read(pool, &user).await?;
    Ok(Json(MarkAllReadResult { marked_read }))
}
