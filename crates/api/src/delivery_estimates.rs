//! Delivery-time estimates (Wave 54): the "when will it
//! arrive" half of the product page's delivery block. The
//! charge half is the Wave 1 domain rule
//! (`calculate_delivery_charge`, the same one checkout
//! bills by); the time half is a per-zone day range the
//! platform publishes — built-in defaults from the domain
//! (`default_delivery_day_range`: Dhaka 1–2 days, suburb
//! 2–3, outside 3–5), overridable per zone by an admin
//! platform setting (`delivery_days_<zone>` = `"<min>-<max>"`,
//! resolved by the domain's one resolution rule,
//! `resolve_delivery_day_range`).
//!
//! There is deliberately NO env/startup layer for these
//! ranges (the Wave 21 KYC gate's precedent: a runtime
//! platform decision, not a deployment parameter) — the
//! precedence is DB setting > domain default, and the
//! admin settings view reads through the same resolver,
//! so the range a buyer is promised is the range the
//! admin sees.
//!
//! The estimate is for ONE parcel of the given weight —
//! on the product page, one unit of the product. It is a
//! promise about the courier leg (seller dispatch time
//! is the seller's, and the fulfillment SLA is a separate
//! Wave 40 surface); the UI copy says exactly that.

use axum::{
    extract::{Query, State},
    Json,
};
use domain::{
    calculate_delivery_charge, parse_delivery_zone, resolve_delivery_day_range,
    validate_weight_grams, DeliveryZone,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::{ApiError, AppState};

/// One zone's effective estimate range, resolved through
/// the domain's rule: the stored setting when it parses
/// and validates, else the zone default. A missing pool
/// (the DB-less demo endpoints) resolves to the defaults.
pub async fn effective_delivery_day_range(
    pool: Option<&PgPool>,
    zone: DeliveryZone,
) -> (i64, i64) {
    let stored = match pool {
        Some(pool) => {
            crate::admin::platform_setting_text(pool, domain::delivery_days_setting_key(zone))
                .await
        }
        None => None,
    };
    resolve_delivery_day_range(zone, stored.as_deref())
}

/// All three zones' effective ranges, in zone order —
/// the admin settings view's read (and the one the
/// settings write validates against).
pub async fn effective_delivery_day_ranges(
    pool: Option<&PgPool>,
) -> [(DeliveryZone, (i64, i64)); 3] {
    [
        (
            DeliveryZone::InsideDhaka,
            effective_delivery_day_range(pool, DeliveryZone::InsideDhaka).await,
        ),
        (
            DeliveryZone::DhakaSuburb,
            effective_delivery_day_range(pool, DeliveryZone::DhakaSuburb).await,
        ),
        (
            DeliveryZone::OutsideDhaka,
            effective_delivery_day_range(pool, DeliveryZone::OutsideDhaka).await,
        ),
    ]
}

/// One delivery estimate: the charge for the parcel and
/// the day range it should arrive in.
#[derive(Debug, Serialize)]
pub struct DeliveryEstimate {
    /// The zone's stored string form (`inside_dhaka`,
    /// `dhaka_suburb`, `outside_dhaka`).
    pub zone: String,
    pub weight_grams: i64,
    pub delivery_charge_poisha: i64,
    pub min_days: i64,
    pub max_days: i64,
}

/// Compute one estimate. The charge bills per STARTED
/// kilogram, exactly like checkout (`build_order_totals`
/// rounds the parcel weight up the same way): the grams
/// are validated by the domain weight rule, converted to
/// started kilograms, and priced by the Wave 1 rule with
/// a zero COD amount (the estimate prices the trip, not
/// the collection).
pub async fn estimate_delivery(
    pool: Option<&PgPool>,
    zone: DeliveryZone,
    weight_grams: i64,
) -> Result<DeliveryEstimate, ApiError> {
    let weight_grams = validate_weight_grams(weight_grams).map_err(ApiError::bad_request)?;
    let started_kg = ((weight_grams + 999) / 1_000).max(1);
    let started_kg_u32 = u32::try_from(started_kg)
        .map_err(|_| ApiError::bad_request("weight_grams out of range"))?;
    let delivery_charge_poisha = calculate_delivery_charge(zone, started_kg_u32, 0)
        .map_err(ApiError::bad_request)?;
    let (min_days, max_days) = effective_delivery_day_range(pool, zone).await;
    Ok(DeliveryEstimate {
        zone: zone.as_str().to_string(),
        weight_grams,
        delivery_charge_poisha,
        min_days,
        max_days,
    })
}

// ---------------------------------------------------------------------------
// Handler (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DeliveryEstimateQuery {
    pub zone: Option<String>,
    pub weight_grams: Option<i64>,
}

/// GET /v1/delivery-estimate?zone=&weight_grams= — public:
/// the numbers are the platform's published tariff and
/// promises, the same for every visitor. A missing zone
/// is a 400 (the block must never guess a buyer's zone);
/// a missing weight defaults to one standard kilogram
/// parcel (`domain::DEFAULT_WEIGHT_GRAMS`).
pub async fn delivery_estimate_handler(
    State(state): State<AppState>,
    Query(query): Query<DeliveryEstimateQuery>,
) -> Result<Json<DeliveryEstimate>, ApiError> {
    let zone_raw = query.zone.as_deref().unwrap_or("");
    let zone = parse_delivery_zone(zone_raw).ok_or_else(|| {
        ApiError::bad_request(
            "zone must be one of: inside_dhaka, dhaka_suburb, outside_dhaka — \
             এলাকা সঠিক নয়",
        )
    })?;
    let weight_grams = query.weight_grams.unwrap_or(domain::DEFAULT_WEIGHT_GRAMS);
    Ok(Json(
        estimate_delivery(state.pool.as_ref(), zone, weight_grams).await?,
    ))
}
