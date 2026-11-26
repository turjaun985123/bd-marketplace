//! Shipments: courier booking for seller sub-orders, tracking
//! refresh, and courier availability (Wave 6).
//!
//! Honesty rules for this module, mirroring the payments module: a
//! shipment exists only when a courier adapter actually booked it —
//! the real Steadfast adapter with merchant credentials, or the DEV
//! courier simulator behind its explicit dev flag (its consignment
//! ids are prefixed `DEV-`). Without credentials and without dev
//! mode, booking is a 503 and nothing is written. Statuses live in
//! the domain crate's unified shipment set; the legal moves are the
//! domain transition table's, never this module's invention.
//!
//! One shipment per sub-order: the database enforces it (UNIQUE on
//! `shipments.sub_order_id`) and this module checks first so the
//! seller gets a clean 409 instead of a constraint error.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use courier::{
    courier_adapter_for, courier_credentials_complete, CourierError, CouriersConfig, Parcel,
    ShipmentRequest, KNOWN_COURIERS,
};
use domain::{
    can_transition_shipment, can_transition_sub_order, parse_delivery_zone, weight_kg_from_grams,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One courier booking for a sub-order (a row of `shipments`).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Shipment {
    pub id: Uuid,
    pub sub_order_id: Uuid,
    pub courier: String,
    pub consignment_id: String,
    pub tracking_code: Option<String>,
    /// Unified shipment status (domain crate's set).
    pub status: String,
    /// Cash the courier must collect on delivery, in poisha (the
    /// sub-order's goods + delivery share for COD orders; 0 when the
    /// order was paid online).
    pub cod_amount_poisha: i64,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The shipment on a sub-order, embedded in the order views.
#[derive(Debug, Clone, Serialize)]
pub struct ShipmentSummary {
    pub id: Uuid,
    pub courier: String,
    pub consignment_id: String,
    pub status: String,
}

/// Availability of one courier, as the seller page renders it. The
/// API is the single source of truth — the web app never decides
/// this itself (the same rule as payment methods in Wave 5).
#[derive(Debug, Serialize)]
pub struct CourierAvailability {
    pub courier: String,
    pub available: bool,
    /// `live` when merchant credentials are present, `dev` when the
    /// DEV simulator would serve this courier.
    pub mode: Option<String>,
    /// Machine-readable reason when unavailable (`not_configured`).
    pub reason: Option<String>,
    /// True for the platform's configured default courier.
    pub is_default: bool,
}

const SHIPMENT_COLUMNS: &str = "id, sub_order_id, courier, consignment_id, tracking_code, \
    status, cod_amount_poisha, note, created_at, updated_at";

// ---------------------------------------------------------------------------
// Loaders (shared by booking, refresh and the order views)
// ---------------------------------------------------------------------------

/// Load one shipment by id.
pub async fn load_shipment(pool: &PgPool, shipment_id: Uuid) -> Result<Shipment, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {SHIPMENT_COLUMNS} FROM shipments WHERE id = $1"
    ))
    .bind(shipment_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load shipment", e))?
    .ok_or_else(|| ApiError::not_found("shipment not found"))
}

/// Load the shipment booked for one sub-order, if any.
pub async fn load_shipment_for_sub_order(
    pool: &PgPool,
    sub_order_id: Uuid,
) -> Result<Option<Shipment>, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {SHIPMENT_COLUMNS} FROM shipments WHERE sub_order_id = $1"
    ))
    .bind(sub_order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load shipment for sub-order", e))
}

/// The shipment on a sub-order, as a summary for the order views
/// (buyer order detail and the seller sub-order list share it).
pub async fn load_shipment_summary_for_sub_order(
    pool: &PgPool,
    sub_order_id: Uuid,
) -> Result<Option<ShipmentSummary>, ApiError> {
    let row: Option<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT id, courier, consignment_id, status FROM shipments WHERE sub_order_id = $1",
    )
    .bind(sub_order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load shipment summary", e))?;
    Ok(row.map(|(id, courier, consignment_id, status)| ShipmentSummary {
        id,
        courier,
        consignment_id,
        status,
    }))
}

/// Availability of every known courier under the current config:
/// Steadfast is `live` when its merchant credentials are complete;
/// any known courier is available in `dev` mode (the simulator
/// serves it); everything else is honestly `not_configured`.
pub fn courier_availability(config: &CouriersConfig) -> Vec<CourierAvailability> {
    KNOWN_COURIERS
        .iter()
        .map(|name| {
            let (available, mode, reason) =
                if *name == "steadfast" && courier_credentials_complete(&config.steadfast) {
                    (true, Some("live".to_string()), None)
                } else if config.dev_mode {
                    (true, Some("dev".to_string()), None)
                } else {
                    (false, None, Some("not_configured".to_string()))
                };
            CourierAvailability {
                courier: name.to_string(),
                available,
                mode,
                reason,
                is_default: *name == config.default_courier,
            }
        })
        .collect()
}

/// Map a courier-crate error onto the API's error type: an
/// unconfigured or unsupported courier is a 503 (the capability is
/// not available yet), an unknown courier name is the caller's 400,
/// and a courier-side failure is a 502 carrying the courier's
/// message.
fn courier_error_to_api(err: CourierError) -> ApiError {
    match err {
        CourierError::NotConfigured(name) => ApiError::service_unavailable(format!(
            "courier '{name}' is not available yet (no merchant credentials configured)"
        )),
        CourierError::UnknownCourier(name) => {
            ApiError::bad_request(format!("unknown courier '{name}'"))
        }
        CourierError::Unsupported { courier, operation } => ApiError::service_unavailable(
            format!("courier '{courier}' does not support '{operation}'"),
        ),
        CourierError::Gateway(message) => {
            ApiError::new(StatusCode::BAD_GATEWAY, message)
        }
    }
}

/// Is this SQLx error a Postgres unique-constraint violation (23505)?
fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err.as_database_error() {
        Some(db) => db.code().is_some_and(|code| code.as_ref() == "23505"),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Booking (seller)
// ---------------------------------------------------------------------------

/// Everything booking needs from the sub-order, its store and its
/// parent order, loaded in one query.
#[derive(FromRow)]
struct SubOrderBookingRow {
    status: String,
    store_id: Uuid,
    owner_user_id: Uuid,
    goods_subtotal_poisha: i64,
    delivery_charge_poisha: i64,
    payment_status: String,
    delivery_name: String,
    delivery_phone: String,
    delivery_address: String,
    delivery_zone: String,
}

/// Book a courier shipment for one of the seller's CONFIRMED
/// sub-orders. The adapter call happens first (it may be a network
/// call for Steadfast, so no transaction is held across it); the
/// shipment row, its `create` event and the sub-order's move to
/// `shipped` then commit in ONE transaction. A duplicate booking is
/// a 409 — checked up front, and the UNIQUE constraint is the
/// backstop if two bookings race.
pub async fn create_shipment_for_seller(
    state: &AppState,
    user: &AuthUser,
    sub_order_id: Uuid,
    courier_name: Option<&str>,
) -> Result<Shipment, ApiError> {
    let pool = state.require_pool()?;
    let config = &state.couriers_config;

    let row: Option<SubOrderBookingRow> = sqlx::query_as(
        "SELECT so.status, so.store_id, s.owner_user_id, \
         so.goods_subtotal_poisha, so.delivery_charge_poisha, \
         o.payment_status, o.delivery_name, o.delivery_phone, \
         o.delivery_address, o.delivery_zone \
         FROM sub_orders so \
         JOIN stores s ON s.id = so.store_id \
         JOIN orders o ON o.id = so.order_id \
         WHERE so.id = $1",
    )
    .bind(sub_order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load sub-order for booking", e))?;
    let row = row.ok_or_else(|| ApiError::not_found("sub-order not found"))?;
    if row.owner_user_id != user.user_id {
        return Err(ApiError::forbidden(
            "you do not own this sub-order's store",
        ));
    }
    if row.status != "confirmed" {
        return Err(ApiError::conflict(format!(
            "sub-order must be 'confirmed' before booking a courier (currently '{}')",
            row.status
        )));
    }
    if load_shipment_for_sub_order(pool, sub_order_id)
        .await?
        .is_some()
    {
        return Err(ApiError::conflict(
            "this sub-order already has a shipment",
        ));
    }

    let courier_name = courier_name
        .map(str::to_string)
        .unwrap_or_else(|| config.default_courier.clone());
    let adapter = courier_adapter_for(&courier_name, config).map_err(courier_error_to_api)?;

    // Parcel weight: the sub-order's items at their products'
    // current weights, billed per started kilogram exactly like
    // checkout (Wave 4). Product weights can be edited after
    // checkout; the booked weight is the products' weight now.
    let total_grams: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(p.weight_grams * oi.quantity), 0)::bigint \
         FROM order_items oi JOIN products p ON p.id = oi.product_id \
         WHERE oi.sub_order_id = $1",
    )
    .bind(sub_order_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("sum sub-order parcel weight", e))?;
    let weight_kg = weight_kg_from_grams(total_grams).ceil().max(1.0) as u32;
    let zone = parse_delivery_zone(&row.delivery_zone).ok_or_else(|| {
        ApiError::internal("parse delivery zone", sqlx::Error::RowNotFound)
    })?;

    // The courier collects the sub-order's goods + delivery share
    // only when the order itself is pay-on-delivery; an online-paid
    // order's parcel collects nothing.
    let cod_amount_poisha = if row.payment_status == "pay_on_delivery" {
        row.goods_subtotal_poisha + row.delivery_charge_poisha
    } else {
        0
    };

    let request = ShipmentRequest {
        order_id: sub_order_id.to_string(),
        store_id: row.store_id.to_string(),
        invoice: format!("SUB-{}", &sub_order_id.simple().to_string()[..8]),
        recipient_name: row.delivery_name.clone(),
        recipient_phone: row.delivery_phone.clone(),
        recipient_address: row.delivery_address.clone(),
        parcel: Parcel {
            weight_kg,
            cod_amount_poisha,
            zone,
        },
        note: None,
    };

    let booked = adapter
        .create_shipment(&request)
        .await
        .map_err(courier_error_to_api)?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin booking transaction", e))?;

    let shipment_id: Uuid = sqlx::query_scalar(
        "INSERT INTO shipments (sub_order_id, courier, consignment_id, \
         tracking_code, status, cod_amount_poisha, note) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(sub_order_id)
    .bind(&booked.courier)
    .bind(&booked.consignment_id)
    .bind(&booked.tracking_code)
    .bind(&booked.status)
    .bind(cod_amount_poisha)
    .bind(&request.note)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict("this sub-order already has a shipment")
        } else {
            ApiError::internal("insert shipment", e)
        }
    })?;

    sqlx::query(
        "INSERT INTO shipment_events (shipment_id, status, raw_status, source) \
         VALUES ($1, $2, NULL, 'create')",
    )
    .bind(shipment_id)
    .bind(&booked.status)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("insert shipment create event", e))?;

    // The domain sub-order table's confirmed → shipped move; the
    // status was verified above, inside this same request.
    sqlx::query("UPDATE sub_orders SET status = 'shipped' WHERE id = $1")
        .bind(sub_order_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("mark sub-order shipped", e))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit booking", e))?;

    load_shipment(pool, shipment_id).await
}

// ---------------------------------------------------------------------------
// Tracking refresh (seller or buyer)
// ---------------------------------------------------------------------------

/// Poll the courier for one shipment's current status and apply it.
/// Only the sub-order's seller and the order's buyer may refresh —
/// anyone else gets a 404, the same no-leak rule as the buyer order
/// endpoints. An unchanged poll, a status the courier reported that
/// maps to `unknown`, or a move the domain shipment table forbids
/// (a courier reporting a backwards state) all leave the stored
/// shipment untouched and write no event. A legal change updates the
/// shipment, writes a `refresh` event with the courier's raw status,
/// and syncs the sub-order where the two status sets overlap:
/// `delivered` and `returned` (each still checked against the
/// sub-order transition table).
pub async fn refresh_shipment_for_participant(
    state: &AppState,
    user: &AuthUser,
    shipment_id: Uuid,
) -> Result<Shipment, ApiError> {
    let pool = state.require_pool()?;
    let shipment = load_shipment(pool, shipment_id).await?;

    let (owner_user_id, buyer_user_id, sub_status): (Uuid, Uuid, String) = sqlx::query_as(
        "SELECT s.owner_user_id, o.buyer_user_id, so.status \
         FROM sub_orders so \
         JOIN stores s ON s.id = so.store_id \
         JOIN orders o ON o.id = so.order_id \
         WHERE so.id = $1",
    )
    .bind(shipment.sub_order_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("load shipment participants", e))?;
    if user.user_id != owner_user_id && user.user_id != buyer_user_id {
        return Err(ApiError::not_found("shipment not found"));
    }

    let adapter =
        courier_adapter_for(&shipment.courier, &state.couriers_config).map_err(courier_error_to_api)?;
    let update = adapter
        .track(&shipment.consignment_id)
        .await
        .map_err(courier_error_to_api)?;

    if update.status == shipment.status
        || update.status == "unknown"
        || !can_transition_shipment(&shipment.status, &update.status)
    {
        return Ok(shipment);
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin refresh transaction", e))?;

    sqlx::query("UPDATE shipments SET status = $2, updated_at = now() WHERE id = $1")
        .bind(shipment_id)
        .bind(&update.status)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("update shipment status", e))?;

    sqlx::query(
        "INSERT INTO shipment_events (shipment_id, status, raw_status, source) \
         VALUES ($1, $2, $3, 'refresh')",
    )
    .bind(shipment_id)
    .bind(&update.status)
    .bind(&update.raw_status)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("insert shipment refresh event", e))?;

    let sub_target = match update.status.as_str() {
        "delivered" => Some("delivered"),
        "returned" => Some("returned"),
        _ => None,
    };
    if let Some(target) = sub_target {
        if can_transition_sub_order(&sub_status, target) {
            sqlx::query("UPDATE sub_orders SET status = $2 WHERE id = $1")
                .bind(shipment.sub_order_id)
                .bind(target)
                .execute(&mut *tx)
                .await
                .map_err(|e| ApiError::internal("sync sub-order status", e))?;
        }
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit refresh", e))?;

    load_shipment(pool, shipment_id).await
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn list_couriers(State(state): State<AppState>) -> Json<Vec<CourierAvailability>> {
    Json(courier_availability(&state.couriers_config))
}

#[derive(Deserialize)]
pub struct CreateShipmentInput {
    /// Courier name; omitted means the configured default courier.
    pub courier: Option<String>,
}

pub async fn create_shipment(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<CreateShipmentInput>>,
) -> Result<(StatusCode, Json<Shipment>), ApiError> {
    let courier = body.and_then(|Json(input)| input.courier);
    let shipment = create_shipment_for_seller(&state, &user, id, courier.as_deref()).await?;
    Ok((StatusCode::CREATED, Json(shipment)))
}

pub async fn refresh(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Shipment>, ApiError> {
    Ok(Json(
        refresh_shipment_for_participant(&state, &user, id).await?,
    ))
}
