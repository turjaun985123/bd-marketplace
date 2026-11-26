//! Buyer risk: the COD fraud shield's data side (Wave 17).
//!
//! The policy itself — what counts as risky — is the pure domain
//! function `assess_buyer_risk`. This module owns the ONE
//! aggregate query that feeds it: a buyer's order and sub-order
//! history counted into `BuyerRiskInputs`. Checkout (which
//! decides whether an advance is forced for this buyer), the
//! seller buyer-risk endpoint and the seller sub-order list all
//! read through [`buyer_risk_for_user`], so the three surfaces
//! can never disagree about a buyer.
//!
//! What counts, and why:
//!
//! `delivered` and `returned_rto` count SUB-ORDERS: fulfilment
//! happens per seller slice, so risk accrues per slice too.
//! `returned_rto` counts every sub-order in the `returned`
//! state — courier RTO and received buyer returns alike; both
//! mean the parcel came back instead of being accepted.
//! `buyer_cancelled` counts sub-orders of orders the BUYER
//! cancelled wholesale (`orders.status = 'cancelled'` is set
//! only by the buyer cancel path); a seller cancelling one
//! sub-order of a live order is the seller's decision and never
//! counts against the buyer. `total_orders` counts orders.
//!
//! Privacy: the endpoint below answers aggregate counts for a
//! phone number the seller already has (it is on their parcels),
//! to callers who own a store. It never returns the buyer's
//! name, address or order details.

use axum::{
    extract::{Query, State},
    Json,
};
use domain::{assess_buyer_risk, normalize_bd_phone, BuyerRisk, BuyerRiskInputs};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::{ApiError, AppState};

/// A buyer's raw counts plus the shield's verdict — the shared
/// result of [`buyer_risk_for_user`].
#[derive(Debug, Clone, Copy)]
pub struct BuyerRiskReport {
    pub inputs: BuyerRiskInputs,
    pub risk: BuyerRisk,
}

/// THE buyer-risk aggregate: one query counting the buyer's
/// history into the domain input shape, then the pure domain
/// verdict. Used by checkout (the force flag), the seller
/// buyer-risk endpoint and the seller sub-order list (the risk
/// level shown next to each parcel).
pub async fn buyer_risk_for_user(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<BuyerRiskReport, ApiError> {
    #[derive(FromRow)]
    struct RiskCounts {
        total_orders: i64,
        delivered: i64,
        returned_rto: i64,
        buyer_cancelled: i64,
    }

    let counts: RiskCounts = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM orders WHERE buyer_user_id = $1) \
             AS total_orders, \
           (SELECT COUNT(*) FROM sub_orders so \
              JOIN orders o ON o.id = so.order_id \
              WHERE o.buyer_user_id = $1 AND so.status = 'delivered') \
             AS delivered, \
           (SELECT COUNT(*) FROM sub_orders so \
              JOIN orders o ON o.id = so.order_id \
              WHERE o.buyer_user_id = $1 AND so.status = 'returned') \
             AS returned_rto, \
           (SELECT COUNT(*) FROM sub_orders so \
              JOIN orders o ON o.id = so.order_id \
              WHERE o.buyer_user_id = $1 AND o.status = 'cancelled' \
                AND so.status = 'cancelled') \
             AS buyer_cancelled",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("aggregate buyer risk", e))?;

    let inputs = BuyerRiskInputs {
        total_orders: counts.total_orders,
        delivered: counts.delivered,
        returned_rto: counts.returned_rto,
        buyer_cancelled: counts.buyer_cancelled,
    };
    Ok(BuyerRiskReport {
        inputs,
        risk: assess_buyer_risk(inputs),
    })
}

// ---------------------------------------------------------------------------
// Seller buyer-risk lookup
// ---------------------------------------------------------------------------

/// The lookup's answer. For an unknown phone `found` is false and
/// every count is zero with a `low` verdict — the response never
/// confirms anything beyond the aggregates for a known buyer.
#[derive(Debug, Serialize)]
pub struct BuyerRiskView {
    pub found: bool,
    pub total_orders: i64,
    pub delivered: i64,
    pub returned_rto: i64,
    pub buyer_cancelled: i64,
    /// `low`, `medium` or `high` (the domain level's string).
    pub risk_level: String,
    pub requires_advance: bool,
}

/// Look up one phone number's risk report for a seller. The
/// caller must own a store (403 otherwise — this is a seller
/// tool, not a public oracle). The phone is normalized through
/// the same domain rule as everywhere else; an invalid number
/// is a 400.
pub async fn buyer_risk_by_phone(
    pool: &PgPool,
    caller: &AuthUser,
    raw_phone: &str,
) -> Result<BuyerRiskView, ApiError> {
    let store_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM stores WHERE owner_user_id = $1")
            .bind(caller.user_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::internal("check seller stores", e))?;
    if store_count == 0 {
        return Err(ApiError::forbidden(
            "buyer risk lookup is available to sellers (store owners) only",
        ));
    }

    let phone = normalize_bd_phone(raw_phone)
        .ok_or_else(|| ApiError::bad_request("invalid Bangladeshi phone number"))?;
    let buyer_id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE phone = $1")
        .bind(&phone)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("find buyer by phone", e))?;

    let Some(buyer_id) = buyer_id else {
        return Ok(BuyerRiskView {
            found: false,
            total_orders: 0,
            delivered: 0,
            returned_rto: 0,
            buyer_cancelled: 0,
            risk_level: assess_buyer_risk(BuyerRiskInputs::default())
                .level
                .as_str()
                .to_string(),
            requires_advance: false,
        });
    };

    let report = buyer_risk_for_user(pool, buyer_id).await?;
    Ok(BuyerRiskView {
        found: true,
        total_orders: report.inputs.total_orders,
        delivered: report.inputs.delivered,
        returned_rto: report.inputs.returned_rto,
        buyer_cancelled: report.inputs.buyer_cancelled,
        risk_level: report.risk.level.as_str().to_string(),
        requires_advance: report.risk.requires_advance,
    })
}

// ---------------------------------------------------------------------------
// Handler (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct BuyerRiskQuery {
    pub phone: Option<String>,
}

pub async fn buyer_risk_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<BuyerRiskQuery>,
) -> Result<Json<BuyerRiskView>, ApiError> {
    let pool = state.require_pool()?;
    let phone = query
        .phone
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("a phone query parameter is required"))?;
    Ok(Json(buyer_risk_by_phone(pool, &user, phone).await?))
}
