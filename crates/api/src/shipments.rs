//! Shipments: courier booking for seller sub-orders, tracking
//! refresh, and courier availability (Wave 6). Wave 7 adds the
//! Pathao and RedX adapters behind the same factory, per-courier
//! delivery-charge estimates (`GET /v1/couriers/estimate`), and
//! smart routing: booking with courier `auto` resolves to the
//! cheapest available courier through the domain `choose_courier`
//! function, and the shipment stores the CHOSEN courier's name.
//! Wave 11 adds the eCourier and Paperfly adapters behind the same
//! factory; availability and estimates needed no new logic here —
//! they iterate the known couriers and read each one's credential
//! rule from the courier crate, so the new couriers appear in both
//! endpoints (and in smart routing) by construction.
//!
//! Wave 19 adds the manual couriers (Sundarban, SA Paribahan,
//! Karatoa, Carrybee). They have NO public APIs — nothing in this
//! wave pretends otherwise: there is no manual adapter and no
//! network call. A manual booking is a shipment row the seller
//! creates directly (`courier = 'manual'`, the display name and
//! the counter receipt number recorded on the row), and every
//! later status move is seller-entered through the manual-status
//! endpoint, checked against the same domain transition table
//! and synced to the sub-order by the same shared sync function
//! the refresh path uses. The receipt number is the verifiable
//! reference: it is what the buyer sees on the public tracking
//! view and what support can check against the paper receipt.
//!
//! Honesty rules for this module, mirroring the payments module: a
//! shipment exists only when a courier adapter actually booked it —
//! a real adapter with merchant credentials, or the DEV courier
//! simulator behind its explicit dev flag (its consignment ids are
//! prefixed `DEV-`). Without credentials and without dev mode,
//! booking is a 503 and nothing is written. Statuses live in the
//! domain crate's unified shipment set; the legal moves are the
//! domain transition table's, never this module's invention.
//! Estimates are the adapters' local domain-tariff estimates, never
//! live courier quotes, and the responses say so by construction:
//! an unavailable courier has no price at all.
//!
//! One shipment per sub-order: the database enforces it (UNIQUE on
//! `shipments.sub_order_id`) and this module checks first so the
//! seller gets a clean 409 instead of a constraint error.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use courier::{
    courier_adapter_for, courier_credentials_complete, ecourier_credentials_complete,
    paperfly_credentials_complete, pathao_credentials_complete, redx_credentials_complete,
    CourierError, CouriersConfig, Parcel, ShipmentRequest, KNOWN_COURIERS,
};
use domain::{
    can_transition_shipment, can_transition_sub_order, choose_courier, is_valid_shipment_status,
    manual_courier_display_name, parse_delivery_zone, validate_manual_courier,
    validate_manual_tracking_id, validate_weight_grams, weight_kg_from_grams, CourierQuote,
    DeliveryZone, MANUAL_COURIERS,
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
    /// sub-order's goods + delivery share for COD orders, net of
    /// the advance share when the order is `advance_paid`; 0 when
    /// the order was paid online).
    pub cod_amount_poisha: i64,
    pub note: Option<String>,
    /// Wave 19: the manual courier's display name (e.g.
    /// `Sundarban Courier`) when `courier` is `manual`; `None`
    /// for API-courier shipments.
    pub manual_courier_name: Option<String>,
    /// Wave 19: the counter receipt number the seller recorded
    /// for a manual shipment; `None` until they do (and always
    /// `None` for API-courier shipments).
    pub manual_tracking_id: Option<String>,
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
    /// Wave 19: manual courier display name / receipt number,
    /// mirroring [`Shipment`]; both `None` for API couriers.
    pub manual_courier_name: Option<String>,
    pub manual_tracking_id: Option<String>,
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
    /// Human-facing name when the entry needs one: the manual
    /// couriers (Wave 19) carry their display name here (e.g.
    /// `Sundarban Courier`); API couriers leave it `None` and
    /// the UI falls back to the courier key. A manual entry's
    /// `courier` is the manual KEY (`sundarban`, …): booking one
    /// posts `courier = 'manual'` with `manual_courier = <key>`
    /// (see the booking handler), never the key as the courier.
    pub display_name: Option<String>,
}

const SHIPMENT_COLUMNS: &str = "id, sub_order_id, courier, consignment_id, tracking_code, \
    status, cod_amount_poisha, note, manual_courier_name, manual_tracking_id, \
    created_at, updated_at";

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

/// One shipment-summary row as stored: (id, courier, consignment
/// id, status, manual courier name, manual tracking id).
type ShipmentSummaryRow = (Uuid, String, String, String, Option<String>, Option<String>);

/// The shipment on a sub-order, as a summary for the order views
/// (buyer order detail and the seller sub-order list share it).
pub async fn load_shipment_summary_for_sub_order(
    pool: &PgPool,
    sub_order_id: Uuid,
) -> Result<Option<ShipmentSummary>, ApiError> {
    let row: Option<ShipmentSummaryRow> =
        sqlx::query_as(
            "SELECT id, courier, consignment_id, status, manual_courier_name, \
             manual_tracking_id FROM shipments WHERE sub_order_id = $1",
        )
        .bind(sub_order_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load shipment summary", e))?;
    Ok(row.map(
        |(id, courier, consignment_id, status, manual_courier_name, manual_tracking_id)| {
            ShipmentSummary {
                id,
                courier,
                consignment_id,
                status,
                manual_courier_name,
                manual_tracking_id,
            }
        },
    ))
}

/// Does this courier have complete merchant credentials in the
/// config? Every known courier now has a real adapter with its own
/// completeness rule in the courier crate (Steadfast Wave 6, Pathao
/// and RedX Wave 7, eCourier and Paperfly Wave 11) — this match is
/// the one place the API maps a courier name onto its rule.
fn courier_has_live_credentials(name: &str, config: &CouriersConfig) -> bool {
    match name {
        "steadfast" => courier_credentials_complete(&config.steadfast),
        "pathao" => pathao_credentials_complete(&config.pathao),
        "redx" => redx_credentials_complete(&config.redx),
        "ecourier" => ecourier_credentials_complete(&config.ecourier),
        "paperfly" => paperfly_credentials_complete(&config.paperfly),
        _ => false,
    }
}

/// Availability of ONE courier under the current config: `live`
/// when its merchant credentials are complete; available in `dev`
/// mode when the simulator is on (the simulator serves every known
/// courier); otherwise honestly `not_configured`. The single
/// availability rule — the availability list, the estimate endpoint
/// and smart routing all read it from here.
pub fn courier_availability_entry(name: &str, config: &CouriersConfig) -> CourierAvailability {
    let (available, mode, reason) = if courier_has_live_credentials(name, config) {
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
        is_default: name == config.default_courier,
        display_name: None,
    }
}

/// Availability of the four manual couriers (Wave 19). A manual
/// courier needs no credentials and no configuration — the
/// seller books at the counter — so it is always `available`
/// with mode `manual`. The entry's `courier` is the manual KEY
/// and `display_name` its human name; the booking call itself
/// posts `courier = 'manual'` plus `manual_courier = <key>`.
pub fn manual_courier_availability() -> Vec<CourierAvailability> {
    MANUAL_COURIERS
        .iter()
        .map(|(key, display)| CourierAvailability {
            courier: key.to_string(),
            available: true,
            mode: Some("manual".to_string()),
            reason: None,
            is_default: false,
            display_name: Some(display.to_string()),
        })
        .collect()
}

/// Availability of every known courier under the current config.
pub fn courier_availability(config: &CouriersConfig) -> Vec<CourierAvailability> {
    KNOWN_COURIERS
        .iter()
        .map(|name| courier_availability_entry(name, config))
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
// Estimates + smart routing (Wave 7)
// ---------------------------------------------------------------------------

/// One courier's delivery-charge estimate for a parcel. The price
/// is present only for an available courier, and it is the
/// adapter's local estimate under the platform tariff — never a
/// live courier quote (see the module docs).
#[derive(Debug, Serialize)]
pub struct CourierEstimate {
    pub courier: String,
    pub available: bool,
    /// `live` or `dev` when available (same rule as availability).
    pub mode: Option<String>,
    /// Machine-readable reason when unavailable (`not_configured`).
    pub reason: Option<String>,
    pub price_poisha: Option<i64>,
    /// Human-readable note when there is something the price
    /// cannot say: the manual couriers (Wave 19) carry one
    /// explaining they are booked at the counter and priced
    /// there, which is why their price is always `None`.
    pub note: Option<String>,
}

/// The estimate endpoint's answer: every known courier's estimate
/// for one zone and weight, plus the smart-routing recommendation —
/// the domain `choose_courier` verdict over these same numbers
/// (`None` when no courier is available).
#[derive(Debug, Serialize)]
pub struct CourierEstimates {
    pub zone: String,
    pub weight_grams: i64,
    pub estimates: Vec<CourierEstimate>,
    pub recommended: Option<String>,
}

/// The parcel the estimates and routing price: billed per started
/// kilogram exactly like checkout, and with no COD amount — a
/// delivery-charge estimate never includes a COD fee (the Wave 1
/// rule: no COD percentage is baked in anywhere).
fn estimate_parcel(zone: DeliveryZone, weight_grams: i64) -> Parcel {
    let weight_kg = weight_kg_from_grams(weight_grams).ceil().max(1.0) as u32;
    Parcel {
        weight_kg,
        cod_amount_poisha: 0,
        zone,
    }
}

/// Price every known courier for one parcel: availability from the
/// one shared rule, and for each available courier the adapter's
/// local estimate. Returns the public estimates AND the routing
/// quotes built from the same numbers, so the estimate endpoint and
/// booking's `auto` resolution can never disagree. An available
/// courier whose estimate call fails gets no quote — routing never
/// picks a courier it could not price.
async fn courier_quotes(
    config: &CouriersConfig,
    parcel: &Parcel,
) -> (Vec<CourierEstimate>, Vec<CourierQuote<'static>>) {
    let mut estimates = Vec::new();
    let mut quotes = Vec::new();
    for name in KNOWN_COURIERS {
        let availability = courier_availability_entry(name, config);
        // Only an available courier is ever priced: an unavailable
        // courier's price must not exist, let alone be routed on.
        let price_poisha = if availability.available {
            match courier_adapter_for(name, config) {
                Ok(adapter) => adapter.price_estimate(parcel).await.ok(),
                Err(_) => None,
            }
        } else {
            None
        };
        if let Some(price) = price_poisha {
            quotes.push(CourierQuote {
                courier: name,
                available: true,
                price_poisha: price,
            });
        }
        estimates.push(CourierEstimate {
            courier: availability.courier,
            available: availability.available,
            mode: availability.mode,
            reason: availability.reason,
            price_poisha,
            note: None,
        });
    }
    (estimates, quotes)
}

/// Estimate every courier for a zone and weight, and recommend one.
/// The estimate endpoint's one service function.
pub async fn estimate_couriers(
    state: &AppState,
    zone: DeliveryZone,
    weight_grams: i64,
) -> CourierEstimates {
    let parcel = estimate_parcel(zone, weight_grams);
    let (mut estimates, quotes) = courier_quotes(&state.couriers_config, &parcel).await;
    // Wave 19: the manual couriers are listed so the seller sees
    // the full menu in one response, but they are NEVER priced
    // (the counter sets the charge, not the platform tariff) and
    // NEVER quoted — `choose_courier` only ever sees the adapter
    // quotes built above, so `auto` routing cannot pick one.
    for (key, _display) in MANUAL_COURIERS {
        estimates.push(CourierEstimate {
            courier: key.to_string(),
            available: true,
            mode: Some("manual".to_string()),
            reason: None,
            price_poisha: None,
            note: Some(
                "manual courier — booked at the counter; the charge is set there, \
                 not estimated"
                    .to_string(),
            ),
        });
    }
    CourierEstimates {
        zone: zone.as_str().to_string(),
        weight_grams,
        estimates,
        recommended: choose_courier(&quotes).map(str::to_string),
    }
}

/// Resolve which courier serves a booking. THE one resolution
/// function — the booking handler calls it and nothing else
/// decides. No name means the configured default courier; an
/// explicit name is used as given (the factory and the adapter then
/// produce the honest 400 for an unknown name or 503 for an
/// unconfigured one, exactly as before Wave 7); `auto` means smart
/// routing — the cheapest available courier for this parcel by the
/// domain rule, or a 503 when no courier can book at all. The
/// resolved name is what gets stored on the shipment — never the
/// literal string `auto`.
pub async fn resolve_booking_courier(
    state: &AppState,
    requested: Option<&str>,
    zone: DeliveryZone,
    weight_grams: i64,
) -> Result<String, ApiError> {
    let config = &state.couriers_config;
    match requested {
        None => Ok(config.default_courier.clone()),
        Some("auto") => {
            let parcel = estimate_parcel(zone, weight_grams);
            let (_estimates, quotes) = courier_quotes(config, &parcel).await;
            choose_courier(&quotes).map(str::to_string).ok_or_else(|| {
                ApiError::service_unavailable(
                    "no courier is available for this booking (none configured)",
                )
            })
        }
        Some(name) => Ok(name.to_string()),
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
    order_id: Uuid,
    goods_subtotal_poisha: i64,
    delivery_charge_poisha: i64,
    payment_status: String,
    /// Wave 17: the parent order's advance snapshot — the
    /// booking guard and the collection netting read these.
    advance_required_poisha: i64,
    advance_paid_poisha: i64,
    delivery_name: String,
    delivery_phone: String,
    delivery_address: String,
    delivery_zone: String,
}

/// Load the sub-order row a booking needs and apply EVERY
/// pre-booking guard, in the established order: the sub-order
/// exists (404), the caller owns its store (403), it is
/// `confirmed` (409), its order's advance is not still pending
/// (409, Wave 17) and it has no shipment yet (409). THE one
/// guard path — API-courier booking and manual booking (Wave
/// 19) both start here, so a manual parcel can never slip past
/// a check an API parcel would face.
async fn load_booking_row(
    pool: &PgPool,
    user: &AuthUser,
    sub_order_id: Uuid,
) -> Result<SubOrderBookingRow, ApiError> {
    let row: Option<SubOrderBookingRow> = sqlx::query_as(
        "SELECT so.status, so.store_id, s.owner_user_id, o.id AS order_id, \
         so.goods_subtotal_poisha, so.delivery_charge_poisha, \
         o.payment_status, o.advance_required_poisha, o.advance_paid_poisha, \
         o.delivery_name, o.delivery_phone, \
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
    // Wave 17 guard: an order whose advance is still unpaid must
    // not ship — the whole point of collecting the advance is
    // that the parcel only moves once the buyer has money in.
    // Confirming the sub-order stays allowed (that is how the
    // seller signals readiness); only the courier booking waits.
    // A flagged order (advance required but no gateway could
    // collect it, still `pay_on_delivery`) is NOT blocked: the
    // platform accepted that risk at checkout, visibly.
    if row.advance_required_poisha > 0 && row.payment_status == "advance_pending" {
        return Err(ApiError::conflict(
            "advance payment pending for this order — the buyer must pay the advance before the parcel can ship",
        ));
    }
    if load_shipment_for_sub_order(pool, sub_order_id)
        .await?
        .is_some()
    {
        return Err(ApiError::conflict(
            "this sub-order already has a shipment",
        ));
    }
    Ok(row)
}

/// The cash a booked parcel must collect on delivery, from the
/// booking row. THE one computation — API bookings and manual
/// bookings collect identically. The courier collects the
/// sub-order's goods + delivery share only when the order
/// itself is pay-on-delivery; an online-paid order's parcel
/// collects nothing. Wave 17: an `advance_paid` order's parcel
/// collects the same share NET of this sub-order's slice of
/// the advance the buyer already paid online — the advance is
/// allocated across the order's sub-orders in proportion to
/// their goods + delivery shares (integer division; a
/// sub-order never collects below zero).
async fn compute_booking_cod_amount(
    pool: &PgPool,
    row: &SubOrderBookingRow,
) -> Result<i64, ApiError> {
    if row.payment_status == "pay_on_delivery" {
        Ok(row.goods_subtotal_poisha + row.delivery_charge_poisha)
    } else if row.payment_status == "advance_paid" {
        let sub_share = row.goods_subtotal_poisha + row.delivery_charge_poisha;
        let order_collect_total: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(goods_subtotal_poisha + delivery_charge_poisha), 0)::bigint \
             FROM sub_orders WHERE order_id = $1",
        )
        .bind(row.order_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("sum order collection shares", e))?;
        let advance_share = if order_collect_total > 0 {
            row.advance_paid_poisha * sub_share / order_collect_total
        } else {
            0
        };
        Ok((sub_share - advance_share).max(0))
    } else {
        Ok(0)
    }
}

/// Book a courier shipment for one of the seller's CONFIRMED
/// sub-orders. The courier is resolved by
/// [`resolve_booking_courier`] (explicit name, configured default,
/// or `auto` smart routing) once the parcel's zone and weight are
/// known. `area_id` is passed through for couriers whose API needs
/// a numeric recipient area id (RedX); the rest ignore it. The
/// adapter call happens first (it may be a network call, so no
/// transaction is held across it); the shipment row, its `create`
/// event and the sub-order's move to `shipped` then commit in ONE
/// transaction. A duplicate booking is a 409 — checked up front,
/// and the UNIQUE constraint is the backstop if two bookings race.
pub async fn create_shipment_for_seller(
    state: &AppState,
    user: &AuthUser,
    sub_order_id: Uuid,
    courier_name: Option<&str>,
    area_id: Option<i64>,
) -> Result<Shipment, ApiError> {
    let pool = state.require_pool()?;
    let config = &state.couriers_config;

    let row = load_booking_row(pool, user, sub_order_id).await?;

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

    // Which courier serves this parcel: the seller's pick, the
    // configured default, or `auto` smart routing over the
    // estimates for this zone and weight.
    let courier_name =
        resolve_booking_courier(state, courier_name, zone, total_grams).await?;
    let adapter = courier_adapter_for(&courier_name, config).map_err(courier_error_to_api)?;

    let cod_amount_poisha = compute_booking_cod_amount(pool, &row).await?;

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
        area_id,
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

    // Wave 29: booking IS the sub-order's shipped move, so
    // the shipped notification is queued here, in the same
    // transaction — one of the mutually exclusive shipped
    // funnels (see the `notices` module doc).
    crate::notices::enqueue_sub_order_notice(
        &mut tx,
        sub_order_id,
        domain::NOTIFICATION_SUB_ORDER_SHIPPED,
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit booking", e))?;

    load_shipment(pool, shipment_id).await
}

/// Book a MANUAL courier shipment (Wave 19) for one of the
/// seller's CONFIRMED sub-orders. There is no adapter and no
/// network call — the seller books at the courier's counter
/// under the platform's master account — so the shipment row is
/// written directly, in ONE transaction with its `create` event
/// (raw status `manual`: the seller is the source) and the
/// sub-order's move to `shipped`. Every pre-booking guard is
/// the shared [`load_booking_row`] path and the collection
/// amount is the shared [`compute_booking_cod_amount`], so a
/// manual parcel is guarded and priced exactly like an API
/// parcel. `manual_courier_key` and `manual_tracking_id` arrive
/// already validated by the domain functions.
///
/// `consignment_id` is NOT NULL in the schema: when the seller
/// records the counter receipt number it IS the consignment id;
/// until then the row carries a generated `MANUAL-…`
/// placeholder derived from the shipment id, so "no receipt
/// number yet" is visible in every view instead of being
/// confused with a real one. Recording the number later (the
/// manual-tracking endpoint) replaces the placeholder.
pub async fn create_manual_shipment_for_seller(
    state: &AppState,
    user: &AuthUser,
    sub_order_id: Uuid,
    manual_courier_key: &str,
    manual_tracking_id: Option<String>,
) -> Result<Shipment, ApiError> {
    let pool = state.require_pool()?;
    let row = load_booking_row(pool, user, sub_order_id).await?;
    let cod_amount_poisha = compute_booking_cod_amount(pool, &row).await?;
    let display_name = manual_courier_display_name(manual_courier_key)
        .ok_or_else(|| ApiError::bad_request("unknown manual courier"))?;

    let shipment_id = Uuid::new_v4();
    let consignment_id = match &manual_tracking_id {
        Some(tracking_id) => tracking_id.clone(),
        None => format!("MANUAL-{}", &shipment_id.simple().to_string()[..8]),
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin manual booking transaction", e))?;

    sqlx::query(
        "INSERT INTO shipments (id, sub_order_id, courier, consignment_id, \
         tracking_code, status, cod_amount_poisha, note, \
         manual_courier_name, manual_tracking_id) \
         VALUES ($1, $2, 'manual', $3, NULL, 'created', $4, NULL, $5, $6)",
    )
    .bind(shipment_id)
    .bind(sub_order_id)
    .bind(&consignment_id)
    .bind(cod_amount_poisha)
    .bind(display_name)
    .bind(&manual_tracking_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            ApiError::conflict("this sub-order already has a shipment")
        } else {
            ApiError::internal("insert manual shipment", e)
        }
    })?;

    sqlx::query(
        "INSERT INTO shipment_events (shipment_id, status, raw_status, source) \
         VALUES ($1, 'created', 'manual', 'create')",
    )
    .bind(shipment_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("insert manual shipment create event", e))?;

    // The same confirmed → shipped move an API booking makes.
    sqlx::query("UPDATE sub_orders SET status = 'shipped' WHERE id = $1")
        .bind(sub_order_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("mark sub-order shipped", e))?;

    // Wave 29: the shipped notification, in the same
    // transaction — the manual booking is one of the mutually
    // exclusive shipped funnels (see the `notices` module
    // doc).
    crate::notices::enqueue_sub_order_notice(
        &mut tx,
        sub_order_id,
        domain::NOTIFICATION_SUB_ORDER_SHIPPED,
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit manual booking", e))?;

    load_shipment(pool, shipment_id).await
}

/// Sync a sub-order with a shipment status that was just
/// applied, inside the caller's transaction. THE one sync rule —
/// the courier refresh path and the manual-status endpoint both
/// call it, so a parcel's arrival or return moves the sub-order
/// identically however the status was learned. Only `delivered`
/// and `returned` overlap between the two status sets; each
/// move is still checked against the sub-order transition
/// table. A sync to `returned` also restocks the sub-order's
/// items in the same transaction (Wave 16) — the goods
/// physically came back — via the one shared restock helper. A
/// courier-driven or seller-entered return creates NO return
/// request and NO refund: whether money goes back is a separate
/// platform decision (a COD parcel was never paid for; a
/// prepaid one is refunded manually), never an automatic side
/// effect of a shipment status. Wave 29: an applied sync also
/// queues the buyer's `sub_order_delivered` /
/// `sub_order_returned` notification in the same transaction
/// (see the `notices` module).
async fn sync_sub_order_with_shipment_status(
    conn: &mut sqlx::PgConnection,
    sub_order_id: Uuid,
    sub_status: &str,
    shipment_status: &str,
) -> Result<(), ApiError> {
    let target = match shipment_status {
        "delivered" => "delivered",
        "returned" => "returned",
        _ => return Ok(()),
    };
    if !can_transition_sub_order(sub_status, target) {
        return Ok(());
    }
    sqlx::query("UPDATE sub_orders SET status = $2 WHERE id = $1")
        .bind(sub_order_id)
        .bind(target)
        .execute(&mut *conn)
        .await
        .map_err(|e| ApiError::internal("sync sub-order status", e))?;
    if target == "returned" {
        crate::orders::restock_sub_order_items(conn, sub_order_id, "rto_restock").await?;
    }
    // Wave 29: the sync is THE funnel for courier-driven
    // delivered / returned moves (refresh and manual status
    // both land here), so the buyer's notification is queued
    // here, inside the caller's transaction. Reaching this
    // line means the transition really applied — the guards
    // above return early otherwise — so a repeat poll can
    // never queue a duplicate.
    let kind = if target == "delivered" {
        domain::NOTIFICATION_SUB_ORDER_DELIVERED
    } else {
        domain::NOTIFICATION_SUB_ORDER_RETURNED
    };
    crate::notices::enqueue_sub_order_notice(conn, sub_order_id, kind).await?;
    Ok(())
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
/// sub-order transition table). A sync to `returned` also restocks
/// the sub-order's items in the same transaction (Wave 16) — the
/// goods physically came back — but it creates no return request
/// and no refund (see the note at the sync site).
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

    // Wave 19: a manual shipment has no API to poll — its
    // seller-entered status IS the current truth — so a refresh
    // is an honest no-op returning the stored shipment, never
    // an "unknown courier" error and never an invented update.
    if shipment.courier == "manual" {
        return Ok(shipment);
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

    // The sub-order sync (delivered / returned + restock) is
    // the ONE shared rule — the manual-status endpoint applies
    // the same function to seller-entered statuses.
    sync_sub_order_with_shipment_status(
        &mut tx,
        shipment.sub_order_id,
        &sub_status,
        &update.status,
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit refresh", e))?;

    load_shipment(pool, shipment_id).await
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn list_couriers(State(state): State<AppState>) -> Json<Vec<CourierAvailability>> {
    let mut entries = courier_availability(&state.couriers_config);
    entries.extend(manual_courier_availability());
    Json(entries)
}

#[derive(Deserialize)]
pub struct CourierEstimateQuery {
    /// Delivery zone (`inside_dhaka`, `dhaka_suburb`,
    /// `outside_dhaka`); defaults to `inside_dhaka`.
    pub zone: Option<String>,
    /// Parcel weight in grams; defaults to 1,000 (1 kg) and is
    /// validated by the same domain rule as product weights.
    pub weight_grams: Option<i64>,
}

pub async fn courier_estimates(
    State(state): State<AppState>,
    Query(query): Query<CourierEstimateQuery>,
) -> Result<Json<CourierEstimates>, ApiError> {
    let zone = match query.zone.as_deref() {
        None => DeliveryZone::InsideDhaka,
        Some(raw) => parse_delivery_zone(raw)
            .ok_or_else(|| ApiError::bad_request(format!("unknown delivery zone '{raw}'")))?,
    };
    let weight_grams = query.weight_grams.unwrap_or(1_000);
    validate_weight_grams(weight_grams)
        .map_err(|message| ApiError::bad_request(format!("weight_grams: {message}")))?;
    Ok(Json(estimate_couriers(&state, zone, weight_grams).await))
}

#[derive(Deserialize)]
pub struct CreateShipmentInput {
    /// Courier name; omitted means the configured default courier,
    /// and `auto` means smart routing (the cheapest available
    /// courier for this parcel — the shipment stores the chosen
    /// courier's name, never the literal `auto`). `manual`
    /// (Wave 19) books a manual courier instead: no adapter is
    /// called and `manual_courier` names which one.
    pub courier: Option<String>,
    /// Recipient area id for couriers whose API addresses areas by
    /// numeric id (RedX requires one); ignored by the others.
    pub area_id: Option<i64>,
    /// The manual courier's key (`sundarban`, `sa_paribahan`,
    /// `karatoa`, `carrybee`) — required when `courier` is
    /// `manual`, validated by `validate_manual_courier`.
    pub manual_courier: Option<String>,
    /// The counter receipt number for a manual booking, when
    /// the seller already has it; it can also be recorded later
    /// through the manual-tracking endpoint. Validated by
    /// `validate_manual_tracking_id`.
    pub manual_tracking_id: Option<String>,
}

pub async fn create_shipment(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<CreateShipmentInput>>,
) -> Result<(StatusCode, Json<Shipment>), ApiError> {
    let input = body.map(|Json(input)| input);
    let courier = input.as_ref().and_then(|input| input.courier.as_deref());
    if courier == Some("manual") {
        // Wave 19: a manual booking names its courier by key
        // and may carry the receipt number; both are validated
        // by the pure domain functions before anything is read
        // or written, so an invalid manual booking is a 400
        // that changes nothing.
        let input = input.expect("body present when courier is set");
        let raw_courier = input.manual_courier.as_deref().ok_or_else(|| {
            ApiError::bad_request(
                "manual_courier is required when courier is 'manual' \
                 (one of: sundarban, sa_paribahan, karatoa, carrybee)",
            )
        })?;
        let manual_courier_key = validate_manual_courier(raw_courier).map_err(|message| {
            ApiError::bad_request(format!("manual_courier: {message}"))
        })?;
        let manual_tracking_id =
            validate_manual_tracking_id(input.manual_tracking_id.as_deref()).map_err(
                |message| ApiError::bad_request(format!("manual_tracking_id: {message}")),
            )?;
        let shipment = create_manual_shipment_for_seller(
            &state,
            &user,
            id,
            manual_courier_key,
            manual_tracking_id,
        )
        .await?;
        return Ok((StatusCode::CREATED, Json(shipment)));
    }
    let area_id = input.as_ref().and_then(|input| input.area_id);
    let shipment =
        create_shipment_for_seller(&state, &user, id, courier, area_id).await?;
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

// ---------------------------------------------------------------------------
// Manual courier operations (Wave 19, seller)
// ---------------------------------------------------------------------------

/// Load one shipment for a seller-only operation, with its
/// sub-order's current status: the shipment exists (404) and
/// the caller owns its store (403 — the same convention as
/// booking in this module).
async fn load_shipment_for_seller(
    pool: &PgPool,
    user: &AuthUser,
    shipment_id: Uuid,
) -> Result<(Shipment, String), ApiError> {
    let shipment = load_shipment(pool, shipment_id).await?;
    let (owner_user_id, sub_status): (Uuid, String) = sqlx::query_as(
        "SELECT s.owner_user_id, so.status \
         FROM sub_orders so \
         JOIN stores s ON s.id = so.store_id \
         WHERE so.id = $1",
    )
    .bind(shipment.sub_order_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("load shipment owner", e))?;
    if owner_user_id != user.user_id {
        return Err(ApiError::forbidden(
            "you do not own this shipment's store",
        ));
    }
    Ok((shipment, sub_status))
}

/// The manual-only guard shared by the two manual operations:
/// seller-entered statuses and receipt numbers exist only for
/// manual shipments — an API courier's shipment takes its
/// statuses from the courier (via refresh), so asking for a
/// manual move on one is a 409, never a silent write.
fn require_manual_shipment(shipment: &Shipment) -> Result<(), ApiError> {
    if shipment.courier != "manual" {
        return Err(ApiError::conflict(format!(
            "shipment is with courier '{}', not a manual courier — \
             its status comes from the courier, not manual entry",
            shipment.courier
        )));
    }
    Ok(())
}

/// Record one seller-entered status for a manual shipment. The
/// target must be a unified status (400 when it is not one at
/// all) and a legal move from the current one under the domain
/// transition table (409 otherwise — a seller cannot skip the
/// parcel's journey any more than a courier poll can). The
/// move, its `manual` event and the sub-order sync commit in
/// ONE transaction, and the sync is the shared
/// [`sync_sub_order_with_shipment_status`], so a manual
/// delivery or return lands exactly like a courier-reported
/// one (including the Wave 16 restock on return).
pub async fn set_manual_shipment_status(
    state: &AppState,
    user: &AuthUser,
    shipment_id: Uuid,
    target_status: &str,
) -> Result<Shipment, ApiError> {
    let pool = state.require_pool()?;
    let (shipment, sub_status) = load_shipment_for_seller(pool, user, shipment_id).await?;
    require_manual_shipment(&shipment)?;
    if !is_valid_shipment_status(target_status) {
        return Err(ApiError::bad_request(format!(
            "unknown shipment status '{target_status}'"
        )));
    }
    if !can_transition_shipment(&shipment.status, target_status) {
        return Err(ApiError::conflict(format!(
            "shipment cannot move from '{}' to '{}'",
            shipment.status, target_status
        )));
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin manual status transaction", e))?;

    sqlx::query("UPDATE shipments SET status = $2, updated_at = now() WHERE id = $1")
        .bind(shipment_id)
        .bind(target_status)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("update manual shipment status", e))?;

    sqlx::query(
        "INSERT INTO shipment_events (shipment_id, status, raw_status, source) \
         VALUES ($1, $2, 'manual', 'manual')",
    )
    .bind(shipment_id)
    .bind(target_status)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("insert manual shipment event", e))?;

    sync_sub_order_with_shipment_status(
        &mut tx,
        shipment.sub_order_id,
        &sub_status,
        target_status,
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit manual status", e))?;

    load_shipment(pool, shipment_id).await
}

/// Record (or correct) the counter receipt number of a manual
/// shipment after booking. The number is the booking's
/// verifiable reference, so it is stored both as the manual
/// tracking id and as the consignment id (replacing the
/// generated `MANUAL-…` placeholder when there was one).
/// `tracking_id` arrives already validated by
/// `validate_manual_tracking_id`.
pub async fn set_manual_tracking_id(
    state: &AppState,
    user: &AuthUser,
    shipment_id: Uuid,
    tracking_id: &str,
) -> Result<Shipment, ApiError> {
    let pool = state.require_pool()?;
    let (shipment, _sub_status) = load_shipment_for_seller(pool, user, shipment_id).await?;
    require_manual_shipment(&shipment)?;
    sqlx::query(
        "UPDATE shipments SET manual_tracking_id = $2, consignment_id = $2, \
         updated_at = now() WHERE id = $1",
    )
    .bind(shipment_id)
    .bind(tracking_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("update manual tracking id", e))?;
    load_shipment(pool, shipment_id).await
}

#[derive(Deserialize)]
pub struct ManualStatusInput {
    /// The unified shipment status the seller is recording.
    pub status: String,
}

pub async fn set_manual_status(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<ManualStatusInput>,
) -> Result<Json<Shipment>, ApiError> {
    Ok(Json(
        set_manual_shipment_status(&state, &user, id, &input.status).await?,
    ))
}

#[derive(Deserialize)]
pub struct ManualTrackingInput {
    /// The counter receipt number; required (a blank value is
    /// a 400 — clearing a recorded number is not supported,
    /// because the receipt is the booking's audit reference).
    pub tracking_id: Option<String>,
}

pub async fn set_manual_tracking(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<ManualTrackingInput>,
) -> Result<Json<Shipment>, ApiError> {
    let tracking_id = validate_manual_tracking_id(input.tracking_id.as_deref())
        .map_err(|message| ApiError::bad_request(format!("tracking_id: {message}")))?
        .ok_or_else(|| ApiError::bad_request("tracking_id is required"))?;
    Ok(Json(
        set_manual_tracking_id(&state, &user, id, &tracking_id).await?,
    ))
}
