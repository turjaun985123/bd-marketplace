//! Notification enqueue helpers (Wave 29): the API-side half
//! of the outbox. Each helper loads the recipient and the
//! template variables for one event INSIDE the caller's
//! transaction and queues the row through the ONE writer,
//! `notify::enqueue` — so the notification commits or rolls
//! back together with the state change it announces.
//!
//! The funnel → kind map (each event is enqueued at exactly
//! one place; see the README Wave 29 section):
//!
//! - `order_placed` — `orders::place_order`, once per order.
//! - `sub_order_shipped` — whichever single path performs a
//!   sub-order's confirmed → shipped move: the seller status
//!   PATCH (`orders::update_sub_order_status`) or a shipment
//!   booking (`shipments::create_shipment_for_seller` /
//!   `create_manual_shipment_for_seller`). The moves are
//!   mutually exclusive — a shipped sub-order cannot be
//!   booked, a booked one cannot be PATCHed to shipped — so
//!   exactly one fires per sub-order.
//! - `sub_order_delivered` — the seller PATCH, or the shared
//!   shipment sync (`shipments::sync_sub_order_with_shipment_status`)
//!   when a courier/manual status delivers the parcel. A
//!   sub-order reaches `delivered` exactly once (the domain
//!   transition table has no move out of it), so the two
//!   paths can never both fire.
//! - `sub_order_cancelled` — the seller PATCH (the only
//!   seller-driven cancel of a single sub-order).
//! - `sub_order_returned` — the shared shipment sync (RTO:
//!   courier-reported or seller-entered return).
//! - `payout_paid` — `admin::transition_payout` on the move
//!   to `paid`, to the seller.
//! - `return_update` — `returns::decide_return_request`
//!   (approved / rejected) and `returns::mark_return_received`
//!   (received), to the buyer.

use serde_json::json;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use crate::ApiError;

/// Queue the notice for one sub-order event: the recipient
/// is the order's buyer, reached at the order's DELIVERY
/// phone (the number the buyer gave for this purchase —
/// not necessarily their account phone).
pub async fn enqueue_sub_order_notice(
    conn: &mut PgConnection,
    sub_order_id: Uuid,
    kind: &str,
) -> Result<(), ApiError> {
    #[derive(FromRow)]
    struct NoticeRow {
        buyer_user_id: Uuid,
        delivery_phone: String,
        order_id: Uuid,
        store_name: String,
    }
    let row: NoticeRow = sqlx::query_as(
        "SELECT o.buyer_user_id, o.delivery_phone, o.id AS order_id, \
         s.name AS store_name \
         FROM sub_orders so \
         JOIN orders o ON o.id = so.order_id \
         JOIN stores s ON s.id = so.store_id \
         WHERE so.id = $1",
    )
    .bind(sub_order_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load sub-order notice recipient", e))?;
    let payload = json!({
        "order_id": row.order_id.to_string(),
        "store_name": row.store_name,
        "sub_order_id": sub_order_id.to_string(),
    });
    notify::enqueue(
        &mut *conn,
        row.buyer_user_id,
        &row.delivery_phone,
        kind,
        &payload,
    )
    .await
    .map_err(|e| ApiError::internal("enqueue sub-order notification", e))?;
    Ok(())
}

/// Queue the `return_update` notice for a return request,
/// reading its CURRENT state — call this AFTER the decision
/// or receipt update, inside the same transaction, so the
/// body reports the status that actually committed. The
/// recipient is the buyer at their account phone (a return
/// is arranged with the account holder, and the returns
/// module already treats that number as the contact).
pub async fn enqueue_return_notice(
    conn: &mut PgConnection,
    return_id: Uuid,
) -> Result<(), ApiError> {
    #[derive(FromRow)]
    struct ReturnNoticeRow {
        buyer_id: Uuid,
        buyer_phone: String,
        order_id: Uuid,
        store_name: String,
        status: String,
        refund_poisha: Option<i64>,
    }
    let row: ReturnNoticeRow = sqlx::query_as(
        "SELECT rr.buyer_id, u.phone AS buyer_phone, rr.order_id, \
         s.name AS store_name, rr.status, rr.refund_poisha \
         FROM return_requests rr \
         JOIN users u ON u.id = rr.buyer_id \
         JOIN stores s ON s.id = rr.store_id \
         WHERE rr.id = $1",
    )
    .bind(return_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load return notice recipient", e))?;
    let payload = json!({
        "order_id": row.order_id.to_string(),
        "store_name": row.store_name,
        "status": row.status,
        "amount_poisha": row.refund_poisha,
    });
    notify::enqueue(
        &mut *conn,
        row.buyer_id,
        &row.buyer_phone,
        domain::NOTIFICATION_RETURN_UPDATE,
        &payload,
    )
    .await
    .map_err(|e| ApiError::internal("enqueue return notification", e))?;
    Ok(())
}

/// Queue the `payout_paid` notice for a store's owner: the
/// recipient is the seller at their ACCOUNT phone (a payout
/// belongs to the account, unlike an order event, which
/// belongs to the delivery details).
pub async fn enqueue_payout_paid_notice(
    conn: &mut PgConnection,
    store_id: Uuid,
    amount_poisha: i64,
) -> Result<(), ApiError> {
    let (owner_user_id, phone): (Uuid, String) = sqlx::query_as(
        "SELECT s.owner_user_id, u.phone FROM stores s \
         JOIN users u ON u.id = s.owner_user_id WHERE s.id = $1",
    )
    .bind(store_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load payout notice recipient", e))?;
    let payload = json!({ "amount_poisha": amount_poisha });
    notify::enqueue(
        &mut *conn,
        owner_user_id,
        &phone,
        domain::NOTIFICATION_PAYOUT_PAID,
        &payload,
    )
    .await
    .map_err(|e| ApiError::internal("enqueue payout notification", e))?;
    Ok(())
}
