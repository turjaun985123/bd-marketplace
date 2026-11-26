//! Payments: method availability, gateway webhook callbacks, retry,
//! and the dev simulator helper (Wave 5).
//!
//! Honesty rules for this module: a payment only becomes `paid` when
//! a signature-verified gateway callback says so with the exact
//! amount. Real gateway signature verification is NOT implemented —
//! it cannot be, without each gateway's merchant credentials and
//! integration documentation — so callbacks for non-dev payments are
//! logged and rejected (401) rather than trusted. The DEV simulator's
//! callbacks are verified with the HMAC scheme in the `payments`
//! crate, the single implementation shared by signer and verifier.
//! COD orders have no payment row at all: their
//! `orders.payment_status` stays `pay_on_delivery`.

use axum::{
    extract::{Path, State},
    Json,
};
use chrono::{DateTime, Utc};
use domain::{can_transition_payment, validate_callback_amount};
use payments::{
    is_dev_reference, parse_payment_method, sign_dev_callback, verify_dev_callback,
    PaymentMethod, PaymentsConfig,
};
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One payment attempt against an order (a row of `payments`).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Payment {
    pub id: Uuid,
    pub order_id: Uuid,
    pub method: String,
    pub amount_poisha: i64,
    pub status: String,
    pub gateway_ref: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub paid_at: Option<DateTime<Utc>>,
}

/// The latest payment attempt, embedded in the order view.
#[derive(Debug, Clone, Serialize)]
pub struct PaymentSummary {
    pub id: Uuid,
    pub method: String,
    pub status: String,
    pub gateway_ref: Option<String>,
    /// Local dev-simulator redirect path, present only for payments
    /// the DEV simulator created (their refs start with `DEV-`).
    pub dev_redirect_url: Option<String>,
}

/// Availability of one payment method, as the checkout page renders
/// it. The API is the single source of truth for availability — the
/// web app never decides this itself.
#[derive(Debug, Serialize)]
pub struct MethodAvailability {
    pub method: String,
    pub available: bool,
    /// `cod` for cash on delivery, `live` when merchant credentials
    /// are present, `dev` when the DEV simulator would serve it.
    pub mode: Option<String>,
    /// Machine-readable reason when unavailable (`not_configured`).
    pub reason: Option<String>,
}

const PAYMENT_COLUMNS: &str = "id, order_id, method, amount_poisha, status, gateway_ref, \
    created_at, updated_at, paid_at";

// ---------------------------------------------------------------------------
// Loaders + writers (shared by checkout, webhook, retry and simulate)
// ---------------------------------------------------------------------------

/// Load one payment attempt by id.
pub async fn load_payment(pool: &PgPool, payment_id: Uuid) -> Result<Payment, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM payments WHERE id = $1"
    ))
    .bind(payment_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load payment", e))?
    .ok_or_else(|| ApiError::not_found("payment not found"))
}

/// The most recent payment attempt for an order, as a summary for the
/// order view (with the dev redirect derived from a `DEV-` ref).
pub async fn load_latest_payment_summary(
    pool: &PgPool,
    order_id: Uuid,
) -> Result<Option<PaymentSummary>, ApiError> {
    let row: Option<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, method, status, gateway_ref FROM payments \
         WHERE order_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load latest payment", e))?;
    Ok(row.map(|(id, method, status, gateway_ref)| {
        let dev_redirect_url = gateway_ref
            .as_deref()
            .filter(|reference| is_dev_reference(reference))
            .map(|reference| format!("/dev-pay/{reference}"));
        PaymentSummary {
            id,
            method,
            status,
            gateway_ref,
            dev_redirect_url,
        }
    }))
}

/// Insert a `pending` payment attempt inside the caller's transaction
/// (checkout). The order row is updated separately by checkout's
/// insert, which already stores `payment_status = 'pending'`.
pub async fn insert_pending_payment(
    conn: &mut PgConnection,
    order_id: Uuid,
    method: &str,
    amount_poisha: i64,
    gateway_ref: &str,
) -> Result<Uuid, ApiError> {
    sqlx::query_scalar(
        "INSERT INTO payments (order_id, method, amount_poisha, status, gateway_ref) \
         VALUES ($1, $2, $3, 'pending', $4) RETURNING id",
    )
    .bind(order_id)
    .bind(method)
    .bind(amount_poisha)
    .bind(gateway_ref)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("insert payment", e))
}

/// Availability of every payment method under the current config:
/// COD always; an online method only when its merchant credentials
/// are present (mode `live`) or the DEV simulator is on (mode `dev`).
pub fn payment_methods(config: &PaymentsConfig) -> Vec<MethodAvailability> {
    let mut methods = vec![MethodAvailability {
        method: PaymentMethod::Cod.as_str().to_string(),
        available: true,
        mode: Some("cod".to_string()),
        reason: None,
    }];
    for method in [
        PaymentMethod::Bkash,
        PaymentMethod::Nagad,
        PaymentMethod::SslCommerz,
        PaymentMethod::ShurjoPay,
    ] {
        let availability = if config.credentials_present(method) {
            MethodAvailability {
                method: method.as_str().to_string(),
                available: true,
                mode: Some("live".to_string()),
                reason: None,
            }
        } else if config.dev_mode {
            MethodAvailability {
                method: method.as_str().to_string(),
                available: true,
                mode: Some("dev".to_string()),
                reason: None,
            }
        } else {
            MethodAvailability {
                method: method.as_str().to_string(),
                available: false,
                mode: None,
                reason: Some("not_configured".to_string()),
            }
        };
        methods.push(availability);
    }
    methods
}

// ---------------------------------------------------------------------------
// Webhook callback application — THE one code path (webhook + dev
// simulate both end here; there is no second apply implementation)
// ---------------------------------------------------------------------------

/// Apply one gateway callback to the payment it references.
///
/// The order of checks is deliberate and every received callback is
/// written to `payment_events` (with its raw body and signature
/// verdict) before any rejection, so rejected callbacks are auditable
/// too:
///
/// A callback for an unknown reference is a 404 (there is no payment
/// to attach an event to). A callback whose method segment disagrees
/// with the payment row is logged and rejected 400. A bad signature
/// is logged and rejected 401 with no state change. An amount that
/// differs from the payment amount is logged and rejected 400 with
/// no state change. A duplicate delivery of an already-applied
/// result returns the payment unchanged (200, idempotent). Any other
/// illegal move is rejected 409 by the domain transition table.
/// On success the payment and its order's `payment_status` move
/// together.
#[allow(clippy::too_many_arguments)]
pub async fn apply_payment_callback(
    pool: &PgPool,
    config: &PaymentsConfig,
    method: PaymentMethod,
    payment_ref: &str,
    amount_poisha: i64,
    callback_status: &str,
    signature: &str,
    raw_body: &str,
) -> Result<Payment, ApiError> {
    let payment: Payment = sqlx::query_as(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM payments WHERE gateway_ref = $1 \
         ORDER BY created_at DESC LIMIT 1"
    ))
    .bind(payment_ref)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("find payment by gateway ref", e))?
    .ok_or_else(|| ApiError::not_found("payment not found for gateway reference"))?;

    // Signature verdict. Only DEV simulator references can be verified
    // today (HMAC scheme in the payments crate). Real gateway
    // verification is a deliberate TODO: it needs each gateway's
    // merchant credentials and documented signature scheme, neither of
    // which exists yet, so non-dev callbacks are treated as
    // unverifiable and rejected below instead of being trusted.
    let signature_valid = match payment.gateway_ref.as_deref() {
        Some(reference) if is_dev_reference(reference) => verify_dev_callback(
            &config.dev_secret,
            reference,
            amount_poisha,
            callback_status,
            signature,
        ),
        _ => false,
    };

    let method_matches = payment.method == method.as_str();
    let event_type = if method_matches {
        format!("webhook_{callback_status}")
    } else {
        "webhook_method_mismatch".to_string()
    };
    sqlx::query(
        "INSERT INTO payment_events (payment_id, event_type, raw_body, signature_valid) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(payment.id)
    .bind(&event_type)
    .bind(raw_body)
    .bind(signature_valid)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("log payment event", e))?;

    if !method_matches {
        return Err(ApiError::bad_request(
            "callback method does not match the payment's method",
        ));
    }
    if !signature_valid {
        return Err(ApiError::unauthorized("invalid callback signature"));
    }
    let target = match callback_status {
        "success" => "paid",
        "failed" => "failed",
        _ => {
            return Err(ApiError::bad_request(
                "callback status must be 'success' or 'failed'",
            ))
        }
    };
    if !validate_callback_amount(payment.amount_poisha, amount_poisha) {
        return Err(ApiError::bad_request(
            "callback amount does not match the payment amount",
        ));
    }
    if payment.status == target {
        // Duplicate delivery of an already-applied result: success,
        // no double effects.
        return Ok(payment);
    }
    if !can_transition_payment(&payment.status, target) {
        return Err(ApiError::conflict(format!(
            "payment cannot move from '{}' to '{target}'",
            payment.status
        )));
    }

    let paid_at: Option<DateTime<Utc>> = if target == "paid" {
        Some(Utc::now())
    } else {
        None
    };
    sqlx::query(
        "UPDATE payments SET status = $2, paid_at = COALESCE($3, paid_at), \
         updated_at = now() WHERE id = $1",
    )
    .bind(payment.id)
    .bind(target)
    .bind(paid_at)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("update payment status", e))?;
    sqlx::query("UPDATE orders SET payment_status = $2 WHERE id = $1")
        .bind(payment.order_id)
        .bind(target)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("update order payment status", e))?;

    load_payment(pool, payment.id).await
}

// ---------------------------------------------------------------------------
// Retry + dev simulate (buyer-facing service functions)
// ---------------------------------------------------------------------------

/// Check the caller owns the payment's order. Another buyer's payment
/// is a 404, the same no-leak rule as the order endpoints.
async fn load_payment_for_owner(
    pool: &PgPool,
    user: &AuthUser,
    payment_id: Uuid,
) -> Result<Payment, ApiError> {
    let payment = load_payment(pool, payment_id).await?;
    let owner: Uuid = sqlx::query_scalar("SELECT buyer_user_id FROM orders WHERE id = $1")
        .bind(payment.order_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("load payment order owner", e))?;
    if owner != user.user_id {
        return Err(ApiError::not_found("payment not found"));
    }
    Ok(payment)
}

/// Retry a failed payment: `failed` returns to `pending` (the domain
/// transition table's only way back), reusing the same gateway
/// reference, and the order's `payment_status` follows.
pub async fn retry_payment_for_buyer(
    pool: &PgPool,
    user: &AuthUser,
    payment_id: Uuid,
) -> Result<Payment, ApiError> {
    let payment = load_payment_for_owner(pool, user, payment_id).await?;
    if payment.status != "failed" || !can_transition_payment(&payment.status, "pending") {
        return Err(ApiError::conflict(
            "only a failed payment can be retried",
        ));
    }
    sqlx::query("UPDATE payments SET status = 'pending', updated_at = now() WHERE id = $1")
        .bind(payment.id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("retry payment", e))?;
    sqlx::query("UPDATE orders SET payment_status = 'pending' WHERE id = $1")
        .bind(payment.order_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::internal("retry order payment status", e))?;

    load_payment(pool, payment.id).await
}

/// DEV ONLY: simulate a successful gateway callback for one of the
/// caller's dev payments. Builds and signs the callback exactly as
/// the dev gateway would, then applies it through the SAME
/// [`apply_payment_callback`] the webhook endpoint uses. Returns 404
/// whenever dev mode is off, so in a non-dev deployment the route
/// behaves as if it did not exist.
pub async fn dev_simulate_success(
    pool: &PgPool,
    config: &PaymentsConfig,
    user: &AuthUser,
    payment_id: Uuid,
) -> Result<Payment, ApiError> {
    if !config.dev_mode {
        return Err(ApiError::not_found("payment not found"));
    }
    let payment = load_payment_for_owner(pool, user, payment_id).await?;
    let reference = payment
        .gateway_ref
        .clone()
        .filter(|r| is_dev_reference(r))
        .ok_or_else(|| ApiError::bad_request("payment was not created by the dev simulator"))?;
    let method = parse_payment_method(&payment.method)
        .ok_or_else(|| ApiError::bad_request("unknown payment method on payment"))?;
    let signature = sign_dev_callback(
        &config.dev_secret,
        &reference,
        payment.amount_poisha,
        "success",
    );
    let raw_body = serde_json::json!({
        "payment_ref": reference,
        "amount_poisha": payment.amount_poisha,
        "status": "success",
        "simulated": true,
    })
    .to_string();
    apply_payment_callback(
        pool,
        config,
        method,
        &reference,
        payment.amount_poisha,
        "success",
        &signature,
        &raw_body,
    )
    .await
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn list_methods(State(state): State<AppState>) -> Json<Vec<MethodAvailability>> {
    Json(payment_methods(&state.payments_config))
}

#[derive(serde::Deserialize)]
pub struct WebhookBody {
    pub payment_ref: String,
    pub amount_poisha: i64,
    pub status: String,
    pub signature: String,
}

pub async fn webhook(
    State(state): State<AppState>,
    Path(method_segment): Path<String>,
    Json(body): Json<WebhookBody>,
) -> Result<Json<Payment>, ApiError> {
    let pool = state.require_pool()?;
    let method = parse_payment_method(&method_segment).ok_or_else(|| {
        ApiError::bad_request(format!("unknown payment method '{method_segment}'"))
    })?;
    let raw_body = serde_json::json!({
        "payment_ref": body.payment_ref,
        "amount_poisha": body.amount_poisha,
        "status": body.status,
        "signature": body.signature,
    })
    .to_string();
    let payment = apply_payment_callback(
        pool,
        &state.payments_config,
        method,
        &body.payment_ref,
        body.amount_poisha,
        &body.status,
        &body.signature,
        &raw_body,
    )
    .await?;
    Ok(Json(payment))
}

pub async fn retry(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Payment>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(retry_payment_for_buyer(pool, &user, id).await?))
}

pub async fn dev_simulate(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Payment>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        dev_simulate_success(pool, &state.payments_config, &user, id).await?,
    ))
}
