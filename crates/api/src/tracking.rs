//! Public order tracking (Wave 16): the shareable tracking view
//! behind `GET /v1/track?order_id=<uuid>&phone=<raw>`.
//!
//! There is deliberately NO login on this endpoint: a buyer
//! shares the link with family, or opens it from an SMS, without
//! an account session. The credential is the delivery phone
//! number given at checkout — the order id alone is a bearer
//! identifier anyone could guess or scrape, the phone is the
//! knowledge factor. The comparison is the pure domain rule
//! `phones_match_for_tracking` (both sides normalized, so the
//! local and international forms match).
//!
//! No-leak rule: an unknown order id, a malformed id, a missing
//! phone and a wrong phone ALL answer the same 404 with the same
//! body. The endpoint must never confirm that an order exists —
//! otherwise it becomes an oracle for enumerating order ids.
//!
//! The response is a purpose-built view, not the buyer `Order`:
//! it carries the timeline a recipient needs (per-store status,
//! items, the courier shipment with its full event history) and
//! none of the money split (no commission, no payout, no address).
//!
//! Wave 55: the view gains TIMELINES — the order-level
//! event list (placement + payment/advance events) and a
//! per-sub-order event list, both built by the shared
//! `timeline` module, the same events the buyer's order
//! page renders. The shipment's own event array stays
//! (it is the courier leg in full); the timeline is the
//! whole story around it.

use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use domain::phones_match_for_tracking;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::shipments::load_shipment_for_sub_order;
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One step in a shipment's history, oldest first. `note` is the
/// courier's own raw status string when the courier provided one
/// (the `refresh` events carry it; the `create` event has none).
#[derive(Debug, Serialize)]
pub struct TrackedShipmentEvent {
    pub status: String,
    pub note: Option<String>,
    pub at: DateTime<Utc>,
}

/// The courier shipment on a tracked sub-order, with its events.
#[derive(Debug, Serialize)]
pub struct TrackedShipment {
    pub courier: String,
    pub consignment_id: String,
    /// Unified shipment status (domain crate's set).
    pub status: String,
    /// Wave 19: for a manual shipment, the courier's display
    /// name (e.g. `Sundarban Courier`) and the counter receipt
    /// number the seller recorded — the reference the recipient
    /// can quote at the counter. Both `None` for API couriers;
    /// the receipt number is also `None` until the seller
    /// records it (the consignment id then still carries the
    /// generated `MANUAL-…` placeholder).
    pub manual_courier_name: Option<String>,
    pub manual_tracking_id: Option<String>,
    pub events: Vec<TrackedShipmentEvent>,
}

/// One purchased line on the tracking view.
#[derive(Debug, Serialize)]
pub struct TrackedItem {
    pub title_snapshot: String,
    pub quantity: i32,
}

/// One seller's slice of the tracked order.
#[derive(Debug, Serialize)]
pub struct TrackedSubOrder {
    pub store_slug: String,
    /// placed | confirmed | shipped | delivered | returned | cancelled
    pub status: String,
    pub items: Vec<TrackedItem>,
    /// The courier shipment, or `None` until the seller books one.
    pub shipment: Option<TrackedShipment>,
    /// The sub-order's event timeline (Wave 55), from the
    /// shared `timeline` module — placement, confirm,
    /// booking, courier events, delivery, returns.
    pub timeline: Vec<crate::timeline::TimelineEvent>,
    /// The tracking stepper (Wave 55), from the domain's
    /// one derivation.
    pub steps: Vec<crate::timeline::TimelineStep>,
}

/// The public tracking view of one order.
#[derive(Debug, Serialize)]
pub struct TrackedOrder {
    pub order_id: Uuid,
    pub placed_at: DateTime<Utc>,
    /// pay_on_delivery | pending | paid | failed | refunded
    pub payment_status: String,
    pub payment_method: String,
    pub grand_total_poisha: i64,
    /// The order-level timeline (Wave 55): placement and
    /// the payment/advance events. The per-store story
    /// rides each sub-order's own timeline.
    pub timeline: Vec<crate::timeline::TimelineEvent>,
    /// Sub-orders ordered by store slug.
    pub sub_orders: Vec<TrackedSubOrder>,
}

#[derive(FromRow)]
struct TrackedSubOrderRow {
    id: Uuid,
    store_slug: String,
    status: String,
    /// The sub-order's placement instant — the timeline's
    /// first event (Wave 55).
    created_at: DateTime<Utc>,
}

/// The one 404 every tracking failure shares — see the module
/// docs for why the cases must stay indistinguishable.
fn tracking_not_found() -> ApiError {
    ApiError::not_found("order not found")
}

/// Load the tracking view of one order, gated on the delivery
/// phone. The order row is loaded first and the phone compared
/// in Rust (via the domain rule), so a wrong phone and an
/// unknown id return through the exact same error.
pub async fn track_order(
    pool: &PgPool,
    order_id: Uuid,
    supplied_phone: &str,
) -> Result<TrackedOrder, ApiError> {
    let row: Option<(String, String, String, i64, DateTime<Utc>)> = sqlx::query_as(
        "SELECT delivery_phone, payment_status, payment_method, \
         grand_total_poisha, created_at FROM orders WHERE id = $1",
    )
    .bind(order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load order for tracking", e))?;
    let (delivery_phone, payment_status, payment_method, grand_total_poisha, placed_at) =
        row.ok_or_else(tracking_not_found)?;
    if !phones_match_for_tracking(&delivery_phone, supplied_phone) {
        return Err(tracking_not_found());
    }

    let sub_rows: Vec<TrackedSubOrderRow> = sqlx::query_as(
        "SELECT so.id, s.slug AS store_slug, so.status, so.created_at FROM sub_orders so \
         JOIN stores s ON s.id = so.store_id \
         WHERE so.order_id = $1 ORDER BY s.slug, so.id",
    )
    .bind(order_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load sub-orders for tracking", e))?;

    let mut sub_orders = Vec::with_capacity(sub_rows.len());
    for sub in sub_rows {
        let item_rows: Vec<(String, i32)> = sqlx::query_as(
            "SELECT title_snapshot, quantity FROM order_items \
             WHERE sub_order_id = $1 ORDER BY id",
        )
        .bind(sub.id)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("load items for tracking", e))?;
        let items = item_rows
            .into_iter()
            .map(|(title_snapshot, quantity)| TrackedItem {
                title_snapshot,
                quantity,
            })
            .collect();

        let shipment = match load_shipment_for_sub_order(pool, sub.id).await? {
            None => None,
            Some(shipment) => {
                let event_rows: Vec<(String, Option<String>, DateTime<Utc>)> =
                    sqlx::query_as(
                        "SELECT status, raw_status, created_at FROM shipment_events \
                         WHERE shipment_id = $1 ORDER BY created_at, id",
                    )
                    .bind(shipment.id)
                    .fetch_all(pool)
                    .await
                    .map_err(|e| ApiError::internal("load shipment events", e))?;
                Some(TrackedShipment {
                    courier: shipment.courier,
                    consignment_id: shipment.consignment_id,
                    status: shipment.status,
                    manual_courier_name: shipment.manual_courier_name,
                    manual_tracking_id: shipment.manual_tracking_id,
                    events: event_rows
                        .into_iter()
                        .map(|(status, note, at)| TrackedShipmentEvent { status, note, at })
                        .collect(),
                })
            }
        };

        // Wave 55: the per-sub-order timeline, from the
        // one shared builder.
        let timeline =
            crate::timeline::sub_order_timeline(pool, sub.id, &sub.status, sub.created_at)
                .await?;
        // Wave 55: the stepper over the same facts the
        // timeline was built from (the shipment view's
        // unified status is the courier leg).
        let steps = crate::timeline::sub_order_steps(
            &sub.status,
            shipment.as_ref().map(|view| view.status.as_str()),
        );

        sub_orders.push(TrackedSubOrder {
            store_slug: sub.store_slug,
            status: sub.status,
            items,
            shipment,
            timeline,
            steps,
        });
    }

    // Wave 55: the order-level timeline (placement +
    // payment/advance events), same builder family.
    let timeline =
        crate::timeline::order_timeline(pool, order_id, placed_at, &payment_method).await?;

    Ok(TrackedOrder {
        order_id,
        placed_at,
        payment_status,
        payment_method,
        grand_total_poisha,
        timeline,
        sub_orders,
    })
}

// ---------------------------------------------------------------------------
// Handler (thin, and deliberately unauthenticated — see module docs)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct TrackQuery {
    pub order_id: Option<String>,
    pub phone: Option<String>,
}

pub async fn track_handler(
    State(state): State<AppState>,
    Query(query): Query<TrackQuery>,
) -> Result<Json<TrackedOrder>, ApiError> {
    let pool = state.require_pool()?;
    // A malformed id is the same 404 as an unknown one: parsing
    // must never become a distinguishable failure mode.
    let order_id = query
        .order_id
        .as_deref()
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .ok_or_else(tracking_not_found)?;
    let phone = query.phone.as_deref().unwrap_or("");
    Ok(Json(track_order(pool, order_id, phone).await?))
}
