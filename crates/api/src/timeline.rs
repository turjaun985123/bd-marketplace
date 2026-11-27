//! Order timelines (Wave 55): the ordered event list a
//! buyer watches — placed, confirmed, courier booked,
//! each courier status, delivered, returns — assembled
//! from the rows that ALREADY record those facts (the
//! order/sub-order rows, the shipment + its Wave 16
//! event log, the payments, the return request).
//!
//! Honesty rule: an event carries a timestamp ONLY when
//! a real column recorded the moment. The order's
//! placement, the shipment's booking and every courier
//! event have one; a seller's confirm (or a manual
//! delivered/cancel flip) has none — sub-orders carry no
//! status-change timestamp — so those events ship with
//! `at: null` and sit in their logical position in the
//! sequence. Nothing here invents a time, and the
//! granularity limit is documented in the README's Wave
//! 55 section rather than papered over.
//!
//! One builder per level, shared by every surface: the
//! buyer order view (`orders::SubOrder.timeline`), the
//! public tracking view (order-level + per-sub-order
//! timelines), and the receipt all read these exact
//! events — no surface assembles its own history.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::shipments::load_shipment_for_sub_order;
use crate::ApiError;

/// One event on an order timeline. `kind` is the stable
/// vocabulary the web steppers render from; `status`
/// carries the sub-state for the kinds that have one
/// (the courier's unified status on `shipment_status`,
/// the request's status on the return events); `note`
/// carries the human detail (courier + consignment on
/// booking, the courier's raw status text on its
/// events, the payment method on payment events).
#[derive(Debug, Clone, Serialize)]
pub struct TimelineEvent {
    pub kind: String,
    /// When it happened — null when the platform records
    /// no timestamp for the moment (see module docs).
    pub at: Option<DateTime<Utc>>,
    pub status: Option<String>,
    pub note: Option<String>,
}

fn event(
    kind: &str,
    at: Option<DateTime<Utc>>,
    status: Option<String>,
    note: Option<String>,
) -> TimelineEvent {
    TimelineEvent {
        kind: kind.to_string(),
        at,
        status,
        note,
    }
}

/// The return-request facts the timeline reads (one row
/// per sub-order at most — the table's UNIQUE).
#[derive(sqlx::FromRow)]
struct ReturnFacts {
    status: String,
    created_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
}

/// The per-sub-order timeline (Wave 55): placement, the
/// seller's confirm, the courier leg with its full
/// event log, delivery, and the return path when one
/// exists. Events are in logical order; the courier
/// events among themselves are in recorded order.
pub async fn sub_order_timeline(
    pool: &PgPool,
    sub_order_id: Uuid,
    status: &str,
    created_at: DateTime<Utc>,
) -> Result<Vec<TimelineEvent>, ApiError> {
    let mut events = vec![event("placed", Some(created_at), None, None)];

    // The confirm happened for every status past placed;
    // its moment is not recorded (module docs).
    if matches!(status, "confirmed" | "shipped" | "delivered" | "returned") {
        events.push(event("confirmed", None, None, None));
    }

    let shipment = load_shipment_for_sub_order(pool, sub_order_id).await?;
    let mut delivered_at: Option<DateTime<Utc>> = None;
    if let Some(shipment) = &shipment {
        let booked_note = if let Some(name) = &shipment.manual_courier_name {
            format!("{name} · {}", shipment.consignment_id)
        } else {
            format!("{} · {}", shipment.courier, shipment.consignment_id)
        };
        events.push(event(
            "shipment_booked",
            Some(shipment.created_at),
            None,
            Some(booked_note),
        ));
        let rows: Vec<(String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
            "SELECT status, raw_status, created_at FROM shipment_events \
             WHERE shipment_id = $1 ORDER BY created_at, id",
        )
        .bind(shipment.id)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("load shipment events for timeline", e))?;
        for (event_status, raw, at) in rows {
            if event_status == "delivered" {
                delivered_at = Some(at);
            }
            events.push(event(
                "shipment_status",
                Some(at),
                Some(event_status),
                raw,
            ));
        }
    }

    if matches!(status, "delivered" | "returned") {
        // The courier's delivered event timestamps the
        // delivery when the courier reported it; a
        // seller-marked delivery has no recorded moment.
        events.push(event("delivered", delivered_at, None, None));
    }
    if status == "returned" {
        events.push(event("returned", None, None, None));
    }
    if status == "cancelled" {
        events.push(event("cancelled", None, None, None));
    }

    let return_facts: Option<ReturnFacts> = sqlx::query_as(
        "SELECT status, created_at, decided_at FROM return_requests \
         WHERE sub_order_id = $1",
    )
    .bind(sub_order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load return for timeline", e))?;
    if let Some(facts) = return_facts {
        events.push(event(
            "return_requested",
            Some(facts.created_at),
            Some(facts.status.clone()),
            None,
        ));
        if let Some(decided_at) = facts.decided_at {
            events.push(event(
                "return_decided",
                Some(decided_at),
                Some(facts.status),
                None,
            ));
        }
    }

    Ok(events)
}

/// One step of the tracking stepper as served (Wave 55):
/// the domain's `TrackingStep` with its wire strings —
/// `kind` (`placed`/`confirmed`/`shipped`/`delivered`/
/// `returned`/`cancelled`) and `state` (`done`/`current`/
/// `upcoming`/`alert`).
#[derive(Debug, Clone, Serialize)]
pub struct TimelineStep {
    pub kind: String,
    pub state: String,
}

/// The stepper for one sub-order, computed HERE from the
/// domain's one derivation (`sub_order_tracking_steps`)
/// so every client renders the same steps — the web's
/// local mirror exists only as a fallback for views that
/// predate the field. `shipment_status` is the courier
/// leg's unified status when a shipment exists (a
/// courier-cancelled leg forces the cancelled branch).
pub fn sub_order_steps(status: &str, shipment_status: Option<&str>) -> Vec<TimelineStep> {
    domain::sub_order_tracking_steps(status, shipment_status)
        .into_iter()
        .map(|step| TimelineStep {
            kind: step.kind.to_string(),
            state: step.state.as_str().to_string(),
        })
        .collect()
}

/// One payment row's timeline facts.
#[derive(sqlx::FromRow)]
struct PaymentFacts {
    method: String,
    created_at: DateTime<Utc>,
    paid_at: Option<DateTime<Utc>>,
}

/// The order-level timeline (Wave 55): the placement,
/// then the money — every payment attempt's initiation
/// and settlement. A payment row on a COD order is by
/// construction the Wave 17 advance, so its events are
/// named for the advance; on an online order they are
/// the payment itself. COD orders with no advance have
/// no payment rows and so no money events — cash on
/// delivery is not an event the platform records, and
/// none is invented.
pub async fn order_timeline(
    pool: &PgPool,
    order_id: Uuid,
    placed_at: DateTime<Utc>,
    payment_method: &str,
) -> Result<Vec<TimelineEvent>, ApiError> {
    let mut events = vec![event("order_placed", Some(placed_at), None, None)];
    let payments: Vec<PaymentFacts> = sqlx::query_as(
        "SELECT method, created_at, paid_at FROM payments \
         WHERE order_id = $1 ORDER BY created_at, id",
    )
    .bind(order_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load payments for timeline", e))?;
    let is_advance = payment_method == "cod";
    for payment in payments {
        let (initiated, settled) = if is_advance {
            ("advance_initiated", "advance_paid")
        } else {
            ("payment_initiated", "payment_paid")
        };
        events.push(event(
            initiated,
            Some(payment.created_at),
            None,
            Some(payment.method.clone()),
        ));
        if let Some(paid_at) = payment.paid_at {
            events.push(event(settled, Some(paid_at), None, Some(payment.method)));
        }
    }
    Ok(events)
}
