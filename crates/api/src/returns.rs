//! Returns (Wave 16): buyer return requests for DELIVERED
//! sub-orders, the seller's approve / reject / received decisions,
//! and the admin refund queue.
//!
//! The flow, with every rule in the pure domain functions: a buyer
//! may request a return only for a sub-order that was `delivered`
//! (`can_request_return`) — one request per sub-order, enforced by
//! the UNIQUE constraint in migration 0018 and checked here first
//! so the buyer gets a clean 409. The seller who owns the store
//! approves or rejects (`can_transition_return`); when the goods
//! physically come back, the seller marks the request `received`,
//! which in ONE transaction snapshots the refund
//! (`compute_refund_poisha`: goods minus the sub-order's coupon
//! discount — the delivery charge is never refunded, the courier
//! trip was consumed), moves the refund to `pending`, moves the
//! sub-order to `returned` and restocks its items through the one
//! shared restock helper.
//!
//! The refund itself is an OFF-PLATFORM obligation: no payment
//! gateway refund API is wired (the Wave 5 TODO), so `pending`
//! means "the platform owes the buyer this amount" and an admin
//! marks it `done` after refunding by bKash / cash outside the
//! system. The `done` flag is a record of that fact, never a
//! claim that a gateway moved money.
//!
//! Courier-driven returns (RTO) are NOT return requests: they
//! restock through the shipment refresh path (see `shipments`)
//! and never create rows here or refunds anywhere.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    can_request_return, can_transition_return, compute_refund_poisha, mask_phone,
    validate_return_reason,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::admin::require_admin;
use crate::auth::AuthUser;
use crate::orders::restock_sub_order_items;
use crate::products::clamp_pagination;
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One returned line, as summarised on the return views.
#[derive(Debug, Clone, Serialize)]
pub struct ReturnItemSummary {
    pub title_snapshot: String,
    pub quantity: i32,
    pub line_total_poisha: i64,
}

/// A return request as the BUYER sees it: their own request, its
/// status, and the refund snapshot once the goods were received.
#[derive(Debug, Clone, Serialize)]
pub struct ReturnRequest {
    pub id: Uuid,
    pub sub_order_id: Uuid,
    pub order_id: Uuid,
    pub buyer_id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub reason: String,
    /// requested | approved | rejected | received
    pub status: String,
    /// Goods minus the sub-order's coupon discount, snapshotted
    /// when the seller received the goods back; `None` before.
    pub refund_poisha: Option<i64>,
    /// none | pending | done — the refund's own lifecycle.
    pub refund_status: String,
    pub seller_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub items: Vec<ReturnItemSummary>,
}

/// A return request as the SELLER (and the admin refund queue)
/// sees it: the buyer view plus the buyer's MASKED phone — enough
/// to contact the buyer about the return, never the full number.
#[derive(Debug, Clone, Serialize)]
pub struct SellerReturnRequest {
    pub id: Uuid,
    pub sub_order_id: Uuid,
    pub order_id: Uuid,
    pub buyer_id: Uuid,
    pub buyer_phone_masked: String,
    pub store_id: Uuid,
    pub store_slug: String,
    pub reason: String,
    /// requested | approved | rejected | received
    pub status: String,
    pub refund_poisha: Option<i64>,
    /// none | pending | done — the refund's own lifecycle.
    pub refund_status: String,
    pub seller_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub items: Vec<ReturnItemSummary>,
}

/// A page of buyer return requests plus the total count.
#[derive(Debug, Serialize)]
pub struct ReturnRequestList {
    pub items: Vec<ReturnRequest>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// A page of seller/admin return requests plus the total count.
#[derive(Debug, Serialize)]
pub struct SellerReturnRequestList {
    pub items: Vec<SellerReturnRequest>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

// ---------------------------------------------------------------------------
// Row type + loaders (one loader per shape)
// ---------------------------------------------------------------------------

#[derive(FromRow)]
struct ReturnRequestRow {
    id: Uuid,
    sub_order_id: Uuid,
    order_id: Uuid,
    buyer_id: Uuid,
    buyer_phone: String,
    store_id: Uuid,
    store_slug: String,
    reason: String,
    status: String,
    refund_poisha: Option<i64>,
    refund_status: String,
    seller_note: Option<String>,
    created_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
}

const RETURN_COLUMNS: &str = "rr.id, rr.sub_order_id, rr.order_id, rr.buyer_id, \
    u.phone AS buyer_phone, rr.store_id, s.slug AS store_slug, rr.reason, rr.status, \
    rr.refund_poisha, rr.refund_status, rr.seller_note, rr.created_at, rr.decided_at";

const RETURN_FROM: &str = "FROM return_requests rr \
    JOIN stores s ON s.id = rr.store_id \
    JOIN users u ON u.id = rr.buyer_id";

async fn load_return_items(
    pool: &PgPool,
    sub_order_id: Uuid,
) -> Result<Vec<ReturnItemSummary>, ApiError> {
    let rows: Vec<(String, i32, i64)> = sqlx::query_as(
        "SELECT title_snapshot, quantity, line_total_poisha FROM order_items \
         WHERE sub_order_id = $1 ORDER BY id",
    )
    .bind(sub_order_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load return items", e))?;
    Ok(rows
        .into_iter()
        .map(|(title_snapshot, quantity, line_total_poisha)| ReturnItemSummary {
            title_snapshot,
            quantity,
            line_total_poisha,
        })
        .collect())
}

async fn buyer_view(pool: &PgPool, row: ReturnRequestRow) -> Result<ReturnRequest, ApiError> {
    let items = load_return_items(pool, row.sub_order_id).await?;
    Ok(ReturnRequest {
        id: row.id,
        sub_order_id: row.sub_order_id,
        order_id: row.order_id,
        buyer_id: row.buyer_id,
        store_id: row.store_id,
        store_slug: row.store_slug,
        reason: row.reason,
        status: row.status,
        refund_poisha: row.refund_poisha,
        refund_status: row.refund_status,
        seller_note: row.seller_note,
        created_at: row.created_at,
        decided_at: row.decided_at,
        items,
    })
}

async fn seller_view(
    pool: &PgPool,
    row: ReturnRequestRow,
) -> Result<SellerReturnRequest, ApiError> {
    let items = load_return_items(pool, row.sub_order_id).await?;
    Ok(SellerReturnRequest {
        id: row.id,
        sub_order_id: row.sub_order_id,
        order_id: row.order_id,
        buyer_id: row.buyer_id,
        buyer_phone_masked: mask_phone(&row.buyer_phone),
        store_id: row.store_id,
        store_slug: row.store_slug,
        reason: row.reason,
        status: row.status,
        refund_poisha: row.refund_poisha,
        refund_status: row.refund_status,
        seller_note: row.seller_note,
        created_at: row.created_at,
        decided_at: row.decided_at,
        items,
    })
}

/// Load one return request (buyer view) by id.
pub async fn load_return_request(
    pool: &PgPool,
    return_id: Uuid,
) -> Result<ReturnRequest, ApiError> {
    let row: ReturnRequestRow = sqlx::query_as(&format!(
        "SELECT {RETURN_COLUMNS} {RETURN_FROM} WHERE rr.id = $1"
    ))
    .bind(return_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load return request", e))?
    .ok_or_else(|| ApiError::not_found("return request not found"))?;
    buyer_view(pool, row).await
}

/// Load one return request (seller view) by id.
pub async fn load_seller_return_request(
    pool: &PgPool,
    return_id: Uuid,
) -> Result<SellerReturnRequest, ApiError> {
    let row: ReturnRequestRow = sqlx::query_as(&format!(
        "SELECT {RETURN_COLUMNS} {RETURN_FROM} WHERE rr.id = $1"
    ))
    .bind(return_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load return request", e))?
    .ok_or_else(|| ApiError::not_found("return request not found"))?;
    seller_view(pool, row).await
}

/// Is this SQLx error a Postgres unique-constraint violation (23505)?
fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err.as_database_error() {
        Some(db) => db.code().is_some_and(|code| code.as_ref() == "23505"),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Buyer: create + list
// ---------------------------------------------------------------------------

/// Open a return request for one of the caller's DELIVERED
/// sub-orders. Ownership is checked through the parent order and
/// a sub-order that is not the buyer's is a 404 (the same no-leak
/// rule as the buyer order endpoints); a sub-order that has not
/// been delivered is a 409, and so is a second request for the
/// same sub-order (one return per sub-order — the UNIQUE
/// constraint is the backstop).
pub async fn create_return_request(
    pool: &PgPool,
    user: &AuthUser,
    sub_order_id: Uuid,
    reason: &str,
) -> Result<ReturnRequest, ApiError> {
    validate_return_reason(reason).map_err(ApiError::bad_request)?;
    let reason = reason.trim();

    let row: Option<(Uuid, Uuid, String, Uuid)> = sqlx::query_as(
        "SELECT so.order_id, so.store_id, so.status, o.buyer_user_id \
         FROM sub_orders so JOIN orders o ON o.id = so.order_id WHERE so.id = $1",
    )
    .bind(sub_order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load sub-order for return", e))?;
    let (order_id, store_id, sub_status, buyer_user_id) =
        row.ok_or_else(|| ApiError::not_found("sub-order not found"))?;
    if buyer_user_id != user.user_id {
        return Err(ApiError::not_found("sub-order not found"));
    }
    if !can_request_return(&sub_status) {
        return Err(ApiError::conflict(format!(
            "only a delivered sub-order can be returned (currently '{sub_status}')"
        )));
    }

    let return_id: Uuid = sqlx::query_scalar(
        "INSERT INTO return_requests (sub_order_id, order_id, buyer_id, store_id, reason) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(sub_order_id)
    .bind(order_id)
    .bind(user.user_id)
    .bind(store_id)
    .bind(reason)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict("a return has already been requested for this sub-order")
        } else {
            ApiError::internal("insert return request", e)
        }
    })?;

    load_return_request(pool, return_id).await
}

/// The caller's own return requests, newest first, paginated.
pub async fn list_return_requests_for_buyer(
    pool: &PgPool,
    user: &AuthUser,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<ReturnRequestList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM return_requests WHERE buyer_id = $1")
            .bind(user.user_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("count buyer return requests", e))?;

    let rows: Vec<ReturnRequestRow> = sqlx::query_as(&format!(
        "SELECT {RETURN_COLUMNS} {RETURN_FROM} WHERE rr.buyer_id = $1 \
         ORDER BY rr.created_at DESC, rr.id LIMIT $2 OFFSET $3"
    ))
    .bind(user.user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list buyer return requests", e))?;

    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(buyer_view(pool, row).await?);
    }
    Ok(ReturnRequestList {
        items,
        total,
        page,
        limit,
    })
}

// ---------------------------------------------------------------------------
// Seller: list + decisions
// ---------------------------------------------------------------------------

/// Return requests for stores the caller owns, newest first.
pub async fn list_return_requests_for_seller(
    pool: &PgPool,
    user: &AuthUser,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<SellerReturnRequestList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM return_requests rr \
         JOIN stores s ON s.id = rr.store_id WHERE s.owner_user_id = $1",
    )
    .bind(user.user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count seller return requests", e))?;

    let rows: Vec<ReturnRequestRow> = sqlx::query_as(&format!(
        "SELECT {RETURN_COLUMNS} {RETURN_FROM} WHERE s.owner_user_id = $1 \
         ORDER BY rr.created_at DESC, rr.id LIMIT $2 OFFSET $3"
    ))
    .bind(user.user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list seller return requests", e))?;

    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(seller_view(pool, row).await?);
    }
    Ok(SellerReturnRequestList {
        items,
        total,
        page,
        limit,
    })
}

/// Decide one of the caller's return requests: `approved` or
/// `rejected`, the only moves out of `requested`. Ownership first
/// (404 unknown id, 403 someone else's store — the seller
/// sub-order rule), then the domain transition table (409 on an
/// illegal move). An optional note is stored for the buyer; a
/// decision without a note keeps any note already there.
pub async fn decide_return_request(
    pool: &PgPool,
    user: &AuthUser,
    return_id: Uuid,
    target: &str,
    note: Option<&str>,
) -> Result<SellerReturnRequest, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin return decision transaction", e))?;

    let current: Option<(String, Uuid)> = sqlx::query_as(
        "SELECT rr.status, s.owner_user_id FROM return_requests rr \
         JOIN stores s ON s.id = rr.store_id WHERE rr.id = $1 FOR UPDATE OF rr",
    )
    .bind(return_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("load return request for decision", e))?;
    let (current_status, owner_user_id) =
        current.ok_or_else(|| ApiError::not_found("return request not found"))?;
    if owner_user_id != user.user_id {
        return Err(ApiError::forbidden(
            "you do not own this return request's store",
        ));
    }
    if !can_transition_return(&current_status, target) {
        return Err(ApiError::conflict(format!(
            "cannot move return request from '{current_status}' to '{target}'"
        )));
    }

    let note = note.map(str::trim).filter(|note| !note.is_empty());
    sqlx::query(
        "UPDATE return_requests SET status = $2, \
         seller_note = COALESCE($3, seller_note), decided_at = now() WHERE id = $1",
    )
    .bind(return_id)
    .bind(target)
    .bind(note)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("decide return request", e))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit return decision", e))?;

    load_seller_return_request(pool, return_id).await
}

/// Mark an APPROVED return as received back: the goods are in the
/// seller's hands again. ONE transaction snapshots the refund
/// (goods minus the sub-order's coupon discount, via the domain
/// function), opens the refund as `pending`, moves the sub-order
/// to `returned` and restocks its items through the shared
/// restock helper — the goods, the money obligation and the stock
/// can never disagree. Any other starting status is a 409.
pub async fn mark_return_received(
    pool: &PgPool,
    user: &AuthUser,
    return_id: Uuid,
) -> Result<SellerReturnRequest, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin return receipt transaction", e))?;

    let current: Option<(String, Uuid, Uuid, i64, i64)> = sqlx::query_as(
        "SELECT rr.status, s.owner_user_id, rr.sub_order_id, \
         so.goods_subtotal_poisha, so.discount_poisha \
         FROM return_requests rr \
         JOIN stores s ON s.id = rr.store_id \
         JOIN sub_orders so ON so.id = rr.sub_order_id \
         WHERE rr.id = $1 FOR UPDATE OF rr",
    )
    .bind(return_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("load return request for receipt", e))?;
    let (current_status, owner_user_id, sub_order_id, goods_poisha, discount_poisha) =
        current.ok_or_else(|| ApiError::not_found("return request not found"))?;
    if owner_user_id != user.user_id {
        return Err(ApiError::forbidden(
            "you do not own this return request's store",
        ));
    }
    if !can_transition_return(&current_status, "received") {
        return Err(ApiError::conflict(format!(
            "only an approved return can be marked received (currently '{current_status}')"
        )));
    }

    let refund_poisha = compute_refund_poisha(goods_poisha, discount_poisha);
    sqlx::query(
        "UPDATE return_requests SET status = 'received', refund_poisha = $2, \
         refund_status = 'pending' WHERE id = $1",
    )
    .bind(return_id)
    .bind(refund_poisha)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("mark return received", e))?;

    sqlx::query("UPDATE sub_orders SET status = 'returned' WHERE id = $1")
        .bind(sub_order_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("mark sub-order returned", e))?;

    restock_sub_order_items(&mut tx, sub_order_id).await?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit return receipt", e))?;

    load_seller_return_request(pool, return_id).await
}

// ---------------------------------------------------------------------------
// Admin: the refund queue
// ---------------------------------------------------------------------------

/// Every return request on the platform, newest first, with an
/// optional refund-status filter — the admin refund queue reads
/// it with `refund_status=pending`.
pub async fn list_return_requests_for_admin(
    pool: &PgPool,
    refund_status: Option<&str>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<SellerReturnRequestList, ApiError> {
    if let Some(status) = refund_status {
        if !matches!(status, "none" | "pending" | "done") {
            return Err(ApiError::bad_request(format!(
                "unknown refund_status '{status}'"
            )));
        }
    }
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM return_requests rr \
         WHERE ($1::text IS NULL OR rr.refund_status = $1)",
    )
    .bind(refund_status)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin return requests", e))?;

    let rows: Vec<ReturnRequestRow> = sqlx::query_as(&format!(
        "SELECT {RETURN_COLUMNS} {RETURN_FROM} \
         WHERE ($1::text IS NULL OR rr.refund_status = $1) \
         ORDER BY rr.created_at DESC, rr.id LIMIT $2 OFFSET $3"
    ))
    .bind(refund_status)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin return requests", e))?;

    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(seller_view(pool, row).await?);
    }
    Ok(SellerReturnRequestList {
        items,
        total,
        page,
        limit,
    })
}

/// Record that the platform HAS refunded the buyer off-platform
/// (bKash / cash — no gateway refund API is wired). Only a
/// `pending` refund can be marked done; anything else is a 409,
/// so a refund can never be recorded twice.
pub async fn mark_refund_done(
    pool: &PgPool,
    return_id: Uuid,
) -> Result<SellerReturnRequest, ApiError> {
    let refund_status: Option<String> =
        sqlx::query_scalar("SELECT refund_status FROM return_requests WHERE id = $1")
            .bind(return_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("load return refund status", e))?;
    let refund_status =
        refund_status.ok_or_else(|| ApiError::not_found("return request not found"))?;
    if refund_status != "pending" {
        return Err(ApiError::conflict(format!(
            "refund is not pending (currently '{refund_status}')"
        )));
    }

    sqlx::query("UPDATE return_requests SET refund_status = 'done' WHERE id = $1")
        .bind(return_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("mark refund done", e))?;

    load_seller_return_request(pool, return_id).await
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateReturnRequest {
    pub reason: String,
}

#[derive(Deserialize)]
pub struct DecideReturnRequest {
    /// Optional note for the buyer (e.g. the rejection reason).
    pub note: Option<String>,
}

#[derive(Deserialize)]
pub struct ListReturnsQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
    /// Admin list only: filter by the refund lifecycle.
    pub refund_status: Option<String>,
}

pub async fn create_return_request_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<CreateReturnRequest>,
) -> Result<(StatusCode, Json<ReturnRequest>), ApiError> {
    let pool = state.require_pool()?;
    let created = create_return_request(pool, &user, id, &input.reason).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

pub async fn list_buyer_return_requests_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListReturnsQuery>,
) -> Result<Json<ReturnRequestList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_return_requests_for_buyer(pool, &user, query.page, query.limit).await?,
    ))
}

pub async fn list_seller_return_requests_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListReturnsQuery>,
) -> Result<Json<SellerReturnRequestList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_return_requests_for_seller(pool, &user, query.page, query.limit).await?,
    ))
}

pub async fn approve_return_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<DecideReturnRequest>>,
) -> Result<Json<SellerReturnRequest>, ApiError> {
    let pool = state.require_pool()?;
    let note = body.and_then(|Json(input)| input.note);
    Ok(Json(
        decide_return_request(pool, &user, id, "approved", note.as_deref()).await?,
    ))
}

pub async fn reject_return_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<DecideReturnRequest>>,
) -> Result<Json<SellerReturnRequest>, ApiError> {
    let pool = state.require_pool()?;
    let note = body.and_then(|Json(input)| input.note);
    Ok(Json(
        decide_return_request(pool, &user, id, "rejected", note.as_deref()).await?,
    ))
}

pub async fn received_return_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SellerReturnRequest>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(mark_return_received(pool, &user, id).await?))
}

pub async fn list_admin_return_requests_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListReturnsQuery>,
) -> Result<Json<SellerReturnRequestList>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        list_return_requests_for_admin(
            pool,
            query.refund_status.as_deref(),
            query.page,
            query.limit,
        )
        .await?,
    ))
}

pub async fn refund_done_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SellerReturnRequest>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(mark_refund_done(pool, id).await?))
}
