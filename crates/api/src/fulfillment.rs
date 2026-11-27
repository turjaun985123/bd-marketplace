//! Seller fulfillment center + SLA dashboard (Wave 40).
//!
//! Until this wave a seller worked their orders one by one on
//! the orders page. This module gives them ONE work queue over
//! the same sub-orders: what needs confirming, what needs a
//! courier booking, what is moving, what came back — each item
//! classified by the domain crate's pure `fulfillment_bucket`,
//! aged against the platform's two fulfillment SLAs, with bulk
//! shipment booking and printable courier labels on top.
//!
//! The rules this module lives by:
//!
//! - **No new statuses.** The queue is a view: buckets come
//!   from `domain::fulfillment_bucket` over the existing
//!   sub-order and shipment statuses, lateness from
//!   `domain::sla_breached`. The counts, the item list and the
//!   dashboard card all classify through the same two
//!   functions, so they can never disagree.
//! - **Age is measured from placement.** `sub_orders` carries
//!   `created_at` and no `updated_at`, so an item's age is
//!   `now − created_at` in EVERY bucket. That makes the ship
//!   SLA the stricter reading — a seller who confirms late has
//!   less of the 48 hours left to book — and it is the only
//!   clock the schema honestly offers; the alternative
//!   (pretending the confirm time is known) would be fiction.
//! - **COD figures come from the booking computation.** A
//!   booked item shows its shipment row's stored
//!   `cod_amount_poisha`; an unbooked one shows
//!   `shipments::preview_cod_amount_for_sub_order` — the same
//!   shared computation a booking will run, never a second
//!   formula in this module.
//! - **Bulk booking is per-item honest.** Every id runs the
//!   single-booking path (`shipments::book_shipment_for_seller`)
//!   with its own guards and its own transaction; the response
//!   reports each item's real outcome and one failure never
//!   rolls back the others. There is no batch transaction and
//!   no fake "all booked".
//! - **Labels are data.** The labels endpoint returns what a
//!   label must say; rendering is the web print view's job.
//!   Courier-native label PDFs need per-courier label APIs
//!   that are merchant-onboarding items, not platform code.
//! - **The buyer's order note rides along (Wave 55).** The
//!   note checkout stores on the order surfaces on both the
//!   queue rows and the label data — a packing note the
//!   seller only sees AFTER the parcel ships would be
//!   worthless, so it is on the two surfaces the seller
//!   works from before booking.
//!
//! Scope is the Wave 34 store scope at `manage_orders`: the
//! owner, or a member whose role carries it (every member role
//! does). Error conventions follow the shipments/orders family
//! this queue belongs to: a caller with NO relationship to the
//! named store gets a 403, a missing store a 404, and no
//! `store_id` means the caller's own earliest store (404 when
//! they have none — the seller-dashboard convention).

use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    fulfillment_bucket, sla_breached, validate_fulfillment_sla_hours,
    validate_manual_courier, validate_manual_tracking_id, FulfillmentBucket,
    FULFILLMENT_CONFIRM_SLA_HOURS_KEY, FULFILLMENT_SHIP_SLA_HOURS_KEY,
    STORE_ACTION_MANAGE_ORDERS,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::access::{require_role, store_role, StoreScopeQuery};
use crate::admin::platform_setting_i64;
use crate::auth::AuthUser;
use crate::shipments::{
    book_shipment_for_seller, preview_cod_amount_for_sub_order, CreateShipmentInput, Shipment,
};
use crate::stores::{get_store_by_id, get_store_by_owner, Store};
use crate::{ApiError, AppState};

/// Most sub-orders one bulk booking accepts. A batch is a
/// seller standing at a courier counter with a pile of
/// parcels; twenty at a time keeps each request's adapter
/// calls bounded and its failure list readable.
pub const MAX_BULK_BOOK_IDS: usize = 20;

/// Most labels one labels request returns — a print run, not
/// an export. Bounded like every other list in the API.
pub const MAX_LABEL_IDS: usize = 100;

/// How many queue rows the fulfillment list returns at most,
/// oldest first (the queue is FIFO by design: the oldest
/// parcel has waited longest). The COUNTS are computed over
/// the full actionable set, not this page — see
/// [`fulfillment_counts`].
pub const FULFILLMENT_LIST_LIMIT: i64 = 200;

/// The SQL fragment excluding finished work from the queue:
/// a sub-order whose bucket is Done (delivered, with no
/// return-path shipment state — the domain mapping's
/// precedence) AND older than seven days leaves the queue.
/// The fragment must mirror `fulfillment_bucket`'s Done case
/// exactly; the counts query and the items query share it so
/// the two can never drift.
const DONE_RETENTION_FILTER: &str = "NOT (so.status = 'delivered' \
    AND so.created_at < now() - interval '7 days' \
    AND COALESCE(sh.status, '') NOT IN ('returned', 'cancelled', 'return_in_transit'))";

// ---------------------------------------------------------------------------
// SLA settings (DB platform_settings > startup env > domain const)
// ---------------------------------------------------------------------------

/// The platform's two fulfillment SLAs, in hours: how long a
/// placed sub-order may wait for confirmation, and how long a
/// confirmed one may wait for its courier booking.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct FulfillmentSla {
    pub confirm_hours: i64,
    pub ship_hours: i64,
}

/// The SLA pair in force right now: a stored platform setting
/// wins over the startup value (`FULFILLMENT_CONFIRM_SLA_HOURS`
/// / `FULFILLMENT_SHIP_SLA_HOURS`, themselves defaulting to
/// the domain's 24 / 48), and a stored value outside the
/// domain's legal range is ignored — the
/// `admin::effective_*` pattern, one helper pair for the two
/// halves of the policy.
pub async fn effective_fulfillment_sla(state: &AppState) -> FulfillmentSla {
    let mut sla = FulfillmentSla {
        confirm_hours: state.fulfillment_confirm_sla_hours,
        ship_hours: state.fulfillment_ship_sla_hours,
    };
    if let Some(pool) = &state.pool {
        if let Some(value) = platform_setting_i64(pool, FULFILLMENT_CONFIRM_SLA_HOURS_KEY).await {
            if validate_fulfillment_sla_hours(value) {
                sla.confirm_hours = value;
            }
        }
        if let Some(value) = platform_setting_i64(pool, FULFILLMENT_SHIP_SLA_HOURS_KEY).await {
            if validate_fulfillment_sla_hours(value) {
                sla.ship_hours = value;
            }
        }
    }
    sla
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// Resolve the store a fulfillment request acts on, gated at
/// `manage_orders`: an explicit `store_id` must exist (404)
/// and the caller must own it or hold a member role in it —
/// no relationship at all is a 403, this module's family
/// convention (shipments/orders), and a role that lacked the
/// action would be `require_role`'s bilingual 403. No
/// `store_id` resolves the caller's own earliest store (404
/// when they have none).
async fn resolve_fulfillment_store(
    pool: &PgPool,
    user: &AuthUser,
    store_id: Option<Uuid>,
) -> Result<Store, ApiError> {
    match store_id {
        Some(store_id) => {
            let store = get_store_by_id(pool, store_id).await?;
            match store_role(pool, user.user_id, store_id).await? {
                Some(role) => require_role(&role, STORE_ACTION_MANAGE_ORDERS)?,
                None => {
                    return Err(ApiError::forbidden(
                        "you do not own this store",
                    ))
                }
            }
            Ok(store)
        }
        None => get_store_by_owner(pool, user.user_id).await,
    }
}

// ---------------------------------------------------------------------------
// Counts (the queue's tallies — shared by the list endpoint
// and the seller dashboard card)
// ---------------------------------------------------------------------------

/// Per-bucket tallies over a store scope's actionable
/// sub-orders, plus how many of the actionable items have
/// breached their SLA. `done` counts only the Done items
/// still inside the queue's 7-day retention.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct FulfillmentCounts {
    pub to_confirm: i64,
    pub to_ship: i64,
    pub in_transit: i64,
    pub done: i64,
    pub attention: i64,
    /// Items in `to_confirm` / `to_ship` whose age has reached
    /// their bucket's SLA.
    pub breached: i64,
}

/// The narrow row the counts classify: just what the domain
/// bucket function and the age clock need.
#[derive(FromRow)]
struct BucketRow {
    status: String,
    shipment_status: Option<String>,
    has_shipment: bool,
    created_at: DateTime<Utc>,
}

/// Count a store scope's queue by bucket. THE counts rule —
/// the fulfillment list and the dashboard card both read it,
/// over the same retention filter and the same domain
/// classification, so the two surfaces always agree. Runs
/// over every actionable row (a narrow four-column
/// projection), never the 200-row display page.
pub async fn fulfillment_counts(
    pool: &PgPool,
    store_ids: &[Uuid],
    sla: &FulfillmentSla,
) -> Result<FulfillmentCounts, ApiError> {
    let rows: Vec<BucketRow> = sqlx::query_as(&format!(
        "SELECT so.status, sh.status AS shipment_status, \
         (sh.id IS NOT NULL) AS has_shipment, so.created_at \
         FROM sub_orders so \
         LEFT JOIN shipments sh ON sh.sub_order_id = so.id \
         WHERE so.store_id = ANY($1) AND {DONE_RETENTION_FILTER}"
    ))
    .bind(store_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("count fulfillment queue", e))?;

    let now = Utc::now();
    let mut counts = FulfillmentCounts::default();
    for row in rows {
        let bucket = fulfillment_bucket(&row.status, row.has_shipment, row.shipment_status.as_deref());
        match bucket {
            FulfillmentBucket::ToConfirm => counts.to_confirm += 1,
            FulfillmentBucket::ToShip => counts.to_ship += 1,
            FulfillmentBucket::InTransit => counts.in_transit += 1,
            FulfillmentBucket::Done => counts.done += 1,
            FulfillmentBucket::Attention => counts.attention += 1,
        }
        let age_hours = (now - row.created_at).num_hours().max(0);
        if sla_breached(bucket, age_hours, sla.confirm_hours, sla.ship_hours) {
            counts.breached += 1;
        }
    }
    Ok(counts)
}

// ---------------------------------------------------------------------------
// The queue list
// ---------------------------------------------------------------------------

/// The shipment on a queued sub-order, as the queue and the
/// labels link need it (the order views' `ShipmentSummary`
/// plus the stored collection amount).
#[derive(Debug, Clone, Serialize)]
pub struct FulfillmentShipment {
    pub id: Uuid,
    pub courier: String,
    pub consignment_id: String,
    /// Unified shipment status (domain crate's set).
    pub status: String,
    /// Cash the courier must collect, in poisha — the amount
    /// the booking computation stored on the row.
    pub cod_amount_poisha: i64,
    pub manual_courier_name: Option<String>,
    pub manual_tracking_id: Option<String>,
}

/// One row of the fulfillment queue: the sub-order's work
/// state (bucket, age, SLA), its money summary, and its
/// shipment when one is booked. Buyer details are the
/// DELIVERY fields a seller needs to work the parcel — the
/// same fields the seller sub-order list already shows.
#[derive(Debug, Serialize)]
pub struct FulfillmentItem {
    pub sub_order_id: Uuid,
    pub order_id: Uuid,
    pub store_id: Uuid,
    /// The sub-order's own fulfilment status.
    pub status: String,
    /// The domain bucket: `to_confirm`, `to_ship`,
    /// `in_transit`, `done` or `attention`.
    pub bucket: String,
    /// Whole hours since the sub-order was placed (see the
    /// module doc for the one-clock rule).
    pub age_hours: i64,
    pub sla_breached: bool,
    pub created_at: DateTime<Utc>,
    pub items_count: i64,
    pub goods_subtotal_poisha: i64,
    /// Coupon discount allocated to this sub-order (Wave 14).
    pub discount_poisha: i64,
    /// Loyalty discount allocated to this sub-order (Wave 35).
    pub loyalty_discount_poisha: i64,
    pub delivery_charge_poisha: i64,
    /// Cash due on delivery, in poisha: the booked shipment's
    /// stored collection amount, or the booking computation's
    /// preview while unbooked — the same number either way.
    pub cod_due_poisha: i64,
    pub delivery_name: String,
    pub delivery_phone: String,
    pub delivery_zone: String,
    /// The order's delivery district (Wave 55), when the
    /// order recorded one — sellers plan courier runs by
    /// district; null for zone-only orders.
    pub delivery_district: Option<String>,
    /// The buyer's order note (Wave 55), from the parent
    /// order — packing instructions live where the
    /// packer works.
    pub order_note: Option<String>,
    /// The parent order's payment status (the Wave 17 advance
    /// states surface here exactly as on the seller list).
    pub order_payment_status: String,
    pub shipment: Option<FulfillmentShipment>,
}

/// The queue response: the store it describes, the SLA pair
/// the ages were judged against, the exact counts, and the
/// oldest-first page of items.
#[derive(Debug, Serialize)]
pub struct FulfillmentQueue {
    pub store_id: Uuid,
    pub store_name: String,
    pub sla: FulfillmentSla,
    pub counts: FulfillmentCounts,
    pub items: Vec<FulfillmentItem>,
}

#[derive(FromRow)]
struct FulfillmentRow {
    sub_order_id: Uuid,
    order_id: Uuid,
    store_id: Uuid,
    status: String,
    created_at: DateTime<Utc>,
    goods_subtotal_poisha: i64,
    discount_poisha: i64,
    loyalty_discount_poisha: i64,
    delivery_charge_poisha: i64,
    items_count: i64,
    delivery_name: String,
    delivery_phone: String,
    delivery_zone: String,
    delivery_district: Option<String>,
    order_note: Option<String>,
    order_payment_status: String,
    shipment_id: Option<Uuid>,
    shipment_courier: Option<String>,
    shipment_consignment_id: Option<String>,
    shipment_status: Option<String>,
    shipment_cod_amount_poisha: Option<i64>,
    manual_courier_name: Option<String>,
    manual_tracking_id: Option<String>,
}

/// The fulfillment queue for one store the caller may work:
/// scope and SLA resolved once, counts over the full
/// actionable set, items oldest-first (FIFO — the oldest
/// parcel has waited longest), capped at
/// [`FULFILLMENT_LIST_LIMIT`].
pub async fn list_fulfillment(
    state: &AppState,
    user: &AuthUser,
    store_id: Option<Uuid>,
) -> Result<FulfillmentQueue, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_fulfillment_store(pool, user, store_id).await?;
    let sla = effective_fulfillment_sla(state).await;
    let counts = fulfillment_counts(pool, &[store.id], &sla).await?;

    let rows: Vec<FulfillmentRow> = sqlx::query_as(&format!(
        "SELECT so.id AS sub_order_id, so.order_id, so.store_id, so.status, \
         so.created_at, so.goods_subtotal_poisha, so.discount_poisha, \
         so.loyalty_discount_poisha, so.delivery_charge_poisha, \
         (SELECT COUNT(*) FROM order_items oi WHERE oi.sub_order_id = so.id) AS items_count, \
         o.delivery_name, o.delivery_phone, o.delivery_zone, o.delivery_district, \
         o.order_note, \
         o.payment_status AS order_payment_status, \
         sh.id AS shipment_id, sh.courier AS shipment_courier, \
         sh.consignment_id AS shipment_consignment_id, \
         sh.status AS shipment_status, \
         sh.cod_amount_poisha AS shipment_cod_amount_poisha, \
         sh.manual_courier_name, sh.manual_tracking_id \
         FROM sub_orders so \
         JOIN orders o ON o.id = so.order_id \
         LEFT JOIN shipments sh ON sh.sub_order_id = so.id \
         WHERE so.store_id = $1 AND {DONE_RETENTION_FILTER} \
         ORDER BY so.created_at ASC \
         LIMIT $2"
    ))
    .bind(store.id)
    .bind(FULFILLMENT_LIST_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list fulfillment queue", e))?;

    let now = Utc::now();
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let bucket = fulfillment_bucket(
            &row.status,
            row.shipment_id.is_some(),
            row.shipment_status.as_deref(),
        );
        let age_hours = (now - row.created_at).num_hours().max(0);
        // The COD figure: the booked shipment's stored amount
        // when there is one; the booking computation's preview
        // (the same shared function) while unbooked.
        let (cod_due_poisha, shipment) = match row.shipment_id {
            Some(shipment_id) => (
                row.shipment_cod_amount_poisha.unwrap_or(0),
                Some(FulfillmentShipment {
                    id: shipment_id,
                    courier: row.shipment_courier.clone().unwrap_or_default(),
                    consignment_id: row.shipment_consignment_id.clone().unwrap_or_default(),
                    status: row.shipment_status.clone().unwrap_or_default(),
                    cod_amount_poisha: row.shipment_cod_amount_poisha.unwrap_or(0),
                    manual_courier_name: row.manual_courier_name.clone(),
                    manual_tracking_id: row.manual_tracking_id.clone(),
                }),
            ),
            None => (preview_cod_amount_for_sub_order(pool, row.sub_order_id).await?, None),
        };
        items.push(FulfillmentItem {
            sub_order_id: row.sub_order_id,
            order_id: row.order_id,
            store_id: row.store_id,
            status: row.status,
            bucket: bucket.as_str().to_string(),
            age_hours,
            sla_breached: sla_breached(bucket, age_hours, sla.confirm_hours, sla.ship_hours),
            created_at: row.created_at,
            items_count: row.items_count,
            goods_subtotal_poisha: row.goods_subtotal_poisha,
            discount_poisha: row.discount_poisha,
            loyalty_discount_poisha: row.loyalty_discount_poisha,
            delivery_charge_poisha: row.delivery_charge_poisha,
            cod_due_poisha,
            delivery_name: row.delivery_name,
            delivery_phone: row.delivery_phone,
            delivery_zone: row.delivery_zone,
            delivery_district: row.delivery_district,
            order_note: row.order_note,
            order_payment_status: row.order_payment_status,
            shipment,
        });
    }

    Ok(FulfillmentQueue {
        store_id: store.id,
        store_name: store.name.clone(),
        sla,
        counts,
        items,
    })
}

// ---------------------------------------------------------------------------
// Bulk booking
// ---------------------------------------------------------------------------

/// A bulk booking request: the store the batch belongs to and
/// up to [`MAX_BULK_BOOK_IDS`] sub-order ids, plus exactly the
/// courier fields the single-booking endpoint accepts
/// (`courier` — a name, `auto`, or `manual` with its fields;
/// `area_id` for couriers that address areas numerically).
#[derive(Debug, Deserialize)]
pub struct BulkBookInput {
    pub store_id: Option<Uuid>,
    pub sub_order_ids: Vec<Uuid>,
    pub courier: Option<String>,
    pub area_id: Option<i64>,
    pub manual_courier: Option<String>,
    pub manual_tracking_id: Option<String>,
}

/// One id's real outcome: the created shipment on success, or
/// the booking path's own error message on failure. Nothing
/// here is summarised into success that did not happen.
#[derive(Debug, Serialize)]
pub struct BulkBookItemResult {
    pub sub_order_id: Uuid,
    pub ok: bool,
    pub shipment: Option<Shipment>,
    pub error: Option<String>,
}

/// The bulk booking response: per-item outcomes in request
/// order, plus the tallies the UI reports.
#[derive(Debug, Serialize)]
pub struct BulkBookResult {
    pub store_id: Uuid,
    pub results: Vec<BulkBookItemResult>,
    pub booked: usize,
    pub failed: usize,
}

/// Book a batch of one store's confirmed sub-orders. Scope
/// and request-shape errors (no relationship with the store,
/// an empty or oversized id list, invalid manual fields) fail
/// the whole request BEFORE anything is booked — they are the
/// caller's errors, like the single endpoint's 400s. Every id
/// after that is independent: it must belong to the scoped
/// store (a foreign or unknown id is a per-item error, never
/// a batch failure), then it runs the single-booking path
/// with that path's own guards and transaction.
pub async fn bulk_book_shipments(
    state: &AppState,
    user: &AuthUser,
    input: &BulkBookInput,
) -> Result<BulkBookResult, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_fulfillment_store(pool, user, input.store_id).await?;
    if input.sub_order_ids.is_empty() {
        return Err(ApiError::bad_request(
            "sub_order_ids must name at least one sub-order",
        ));
    }
    if input.sub_order_ids.len() > MAX_BULK_BOOK_IDS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_BULK_BOOK_IDS} sub-orders can be booked at once"
        )));
    }
    // A manual batch validates its manual fields ONCE, up
    // front — the single endpoint validates them before any
    // row is read, so a bulk request with a bad manual courier
    // is the same 400 that changes nothing, not twenty
    // identical per-item errors.
    if input.courier.as_deref() == Some("manual") {
        let raw_courier = input.manual_courier.as_deref().ok_or_else(|| {
            ApiError::bad_request(
                "manual_courier is required when courier is 'manual' \
                 (one of: sundarban, sa_paribahan, karatoa, carrybee)",
            )
        })?;
        validate_manual_courier(raw_courier).map_err(|message| {
            ApiError::bad_request(format!("manual_courier: {message}"))
        })?;
        validate_manual_tracking_id(input.manual_tracking_id.as_deref()).map_err(|message| {
            ApiError::bad_request(format!("manual_tracking_id: {message}"))
        })?;
    }

    let booking_input = CreateShipmentInput {
        courier: input.courier.clone(),
        area_id: input.area_id,
        manual_courier: input.manual_courier.clone(),
        manual_tracking_id: input.manual_tracking_id.clone(),
    };

    let mut results = Vec::with_capacity(input.sub_order_ids.len());
    let mut booked = 0usize;
    for sub_order_id in &input.sub_order_ids {
        let outcome = book_one_in_store(state, user, &store, *sub_order_id, &booking_input).await;
        match outcome {
            Ok(shipment) => {
                booked += 1;
                results.push(BulkBookItemResult {
                    sub_order_id: *sub_order_id,
                    ok: true,
                    shipment: Some(shipment),
                    error: None,
                });
            }
            Err(error) => {
                results.push(BulkBookItemResult {
                    sub_order_id: *sub_order_id,
                    ok: false,
                    shipment: None,
                    error: Some(error.message),
                });
            }
        }
    }
    let failed = results.len() - booked;
    Ok(BulkBookResult {
        store_id: store.id,
        results,
        booked,
        failed,
    })
}

/// One bulk item: the id must name a sub-order OF THE SCOPED
/// STORE (an unknown id and another store's id are per-item
/// errors — the batch is for this store's queue), then the
/// single-booking path runs it with its own guards.
async fn book_one_in_store(
    state: &AppState,
    user: &AuthUser,
    store: &Store,
    sub_order_id: Uuid,
    booking_input: &CreateShipmentInput,
) -> Result<Shipment, ApiError> {
    let pool = state.require_pool()?;
    let owner_store: Option<Uuid> =
        sqlx::query_scalar("SELECT store_id FROM sub_orders WHERE id = $1")
            .bind(sub_order_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("load sub-order store for bulk booking", e))?;
    match owner_store {
        None => Err(ApiError::not_found("sub-order not found")),
        Some(found) if found != store.id => Err(ApiError::forbidden(
            "sub-order belongs to a different store",
        )),
        _ => book_shipment_for_seller(state, user, sub_order_id, booking_input).await,
    }
}

// ---------------------------------------------------------------------------
// Labels (data only — the web print view renders them)
// ---------------------------------------------------------------------------

/// Everything one courier label must say, for one BOOKED
/// sub-order. Sender block: the store's name/slug and the
/// owner's phone — the schema carries no store street
/// address, so none is invented (see the README's Wave 40
/// limits).
#[derive(Debug, Serialize)]
pub struct FulfillmentLabel {
    pub sub_order_id: Uuid,
    pub order_id: Uuid,
    /// When the sub-order was placed.
    pub placed_at: DateTime<Utc>,
    pub store_name: String,
    pub store_slug: String,
    /// The store owner's phone — the label's sender contact.
    pub seller_phone: String,
    pub delivery_name: String,
    pub delivery_phone: String,
    pub delivery_address: String,
    pub delivery_zone: String,
    /// The buyer's order note (Wave 55), printed with
    /// the label so it travels with the parcel.
    pub order_note: Option<String>,
    /// The order's district (Wave 55); null for
    /// zone-only orders.
    pub delivery_district: Option<String>,
    /// The courier's key as stored (`steadfast`, `manual`, …).
    pub courier: String,
    /// The human courier name: the manual courier's display
    /// name for manual shipments, else the key.
    pub courier_display_name: String,
    pub consignment_id: String,
    pub manual_tracking_id: Option<String>,
    /// Cash the courier must collect, in poisha — the booked
    /// shipment's stored amount.
    pub cod_amount_poisha: i64,
    pub items_count: i64,
}

/// One requested id that produced no label, with the reason —
/// labels are never silently dropped.
#[derive(Debug, Serialize)]
pub struct SkippedLabel {
    /// The id as requested, when it parsed as one.
    pub sub_order_id: Option<Uuid>,
    pub reason: String,
}

/// The labels response: one label per booked, in-scope id,
/// and a named reason for every id that produced none.
#[derive(Debug, Serialize)]
pub struct FulfillmentLabels {
    pub store_id: Uuid,
    pub labels: Vec<FulfillmentLabel>,
    pub skipped: Vec<SkippedLabel>,
}

#[derive(FromRow)]
struct LabelRow {
    sub_order_id: Uuid,
    order_id: Uuid,
    store_id: Uuid,
    placed_at: DateTime<Utc>,
    store_name: String,
    store_slug: String,
    seller_phone: String,
    delivery_name: String,
    delivery_phone: String,
    delivery_address: String,
    delivery_zone: String,
    order_note: Option<String>,
    delivery_district: Option<String>,
    courier: Option<String>,
    consignment_id: Option<String>,
    manual_courier_name: Option<String>,
    manual_tracking_id: Option<String>,
    cod_amount_poisha: Option<i64>,
    items_count: i64,
}

/// Label data for the requested sub-orders of one store.
/// Only BOOKED sub-orders get a label (a label without a
/// consignment id is a lie); every other id is reported in
/// `skipped` with its reason — unknown, another store's, or
/// not booked yet.
pub async fn list_fulfillment_labels(
    state: &AppState,
    user: &AuthUser,
    store_id: Option<Uuid>,
    raw_ids: &str,
) -> Result<FulfillmentLabels, ApiError> {
    let pool = state.require_pool()?;
    let store = resolve_fulfillment_store(pool, user, store_id).await?;

    let mut ids: Vec<Uuid> = Vec::new();
    let mut skipped: Vec<SkippedLabel> = Vec::new();
    for token in raw_ids.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        match token.parse::<Uuid>() {
            Ok(id) => ids.push(id),
            Err(_) => skipped.push(SkippedLabel {
                sub_order_id: None,
                reason: format!("'{token}' is not a valid sub-order id"),
            }),
        }
    }
    if ids.is_empty() && skipped.is_empty() {
        return Err(ApiError::bad_request(
            "ids must name at least one sub-order",
        ));
    }
    if ids.len() > MAX_LABEL_IDS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_LABEL_IDS} labels can be printed at once"
        )));
    }

    let mut labels = Vec::with_capacity(ids.len());
    for id in ids {
        let row: Option<LabelRow> = sqlx::query_as(
            "SELECT so.id AS sub_order_id, so.order_id, so.store_id, \
             so.created_at AS placed_at, \
             st.name AS store_name, st.slug AS store_slug, \
             u.phone AS seller_phone, \
             o.delivery_name, o.delivery_phone, o.delivery_address, o.delivery_zone, \
             o.order_note, o.delivery_district, \
             sh.courier, sh.consignment_id, sh.manual_courier_name, \
             sh.manual_tracking_id, sh.cod_amount_poisha, \
             (SELECT COUNT(*) FROM order_items oi WHERE oi.sub_order_id = so.id) AS items_count \
             FROM sub_orders so \
             JOIN stores st ON st.id = so.store_id \
             JOIN users u ON u.id = st.owner_user_id \
             JOIN orders o ON o.id = so.order_id \
             LEFT JOIN shipments sh ON sh.sub_order_id = so.id \
             WHERE so.id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load label row", e))?;
        let Some(row) = row else {
            skipped.push(SkippedLabel {
                sub_order_id: Some(id),
                reason: "sub-order not found".to_string(),
            });
            continue;
        };
        if row.store_id != store.id {
            skipped.push(SkippedLabel {
                sub_order_id: Some(id),
                reason: "sub-order belongs to a different store".to_string(),
            });
            continue;
        }
        let (Some(courier), Some(consignment_id)) = (row.courier, row.consignment_id) else {
            skipped.push(SkippedLabel {
                sub_order_id: Some(id),
                reason: "no shipment booked for this sub-order yet".to_string(),
            });
            continue;
        };
        labels.push(FulfillmentLabel {
            sub_order_id: row.sub_order_id,
            order_id: row.order_id,
            placed_at: row.placed_at,
            store_name: row.store_name,
            store_slug: row.store_slug,
            seller_phone: row.seller_phone,
            delivery_name: row.delivery_name,
            delivery_phone: row.delivery_phone,
            delivery_address: row.delivery_address,
            delivery_zone: row.delivery_zone,
            order_note: row.order_note,
            delivery_district: row.delivery_district,
            courier_display_name: row
                .manual_courier_name
                .clone()
                .unwrap_or_else(|| courier.clone()),
            courier,
            consignment_id,
            manual_tracking_id: row.manual_tracking_id,
            cod_amount_poisha: row.cod_amount_poisha.unwrap_or(0),
            items_count: row.items_count,
        });
    }

    Ok(FulfillmentLabels {
        store_id: store.id,
        labels,
        skipped,
    })
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn list_fulfillment_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<StoreScopeQuery>,
) -> Result<Json<FulfillmentQueue>, ApiError> {
    Ok(Json(
        list_fulfillment(&state, &user, scope.store_id).await?,
    ))
}

pub async fn bulk_book_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<BulkBookInput>,
) -> Result<Json<BulkBookResult>, ApiError> {
    Ok(Json(bulk_book_shipments(&state, &user, &input).await?))
}

#[derive(Deserialize)]
pub struct LabelsQuery {
    pub store_id: Option<Uuid>,
    /// Comma-separated sub-order ids (a query string cannot
    /// repeat portably across the web client's URL builder).
    pub ids: Option<String>,
}

pub async fn labels_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<LabelsQuery>,
) -> Result<Json<FulfillmentLabels>, ApiError> {
    let raw_ids = query.ids.as_deref().unwrap_or("");
    Ok(Json(
        list_fulfillment_labels(&state, &user, query.store_id, raw_ids).await?,
    ))
}
