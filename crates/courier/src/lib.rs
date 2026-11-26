//! Unified courier adapter for Bangladeshi couriers.
//!
//! One trait, one method per operation — every courier is called
//! through exactly the same individually callable functions, and
//! every adapter reports shipment status in the ONE unified status
//! set owned by the domain crate (`domain::is_valid_shipment_status`
//! and friends), so callers never learn courier-specific strings.
//!
//! Wave 6 state: Steadfast has a real adapter ([`SteadfastAdapter`])
//! built from merchant credential config — without complete
//! credentials every operation returns `NotConfigured`, and it never
//! fakes success.
//!
//! Wave 7 state: Pathao ([`PathaoAdapter`]) and RedX
//! ([`RedxAdapter`]) are real adapters behind the same trait, coded
//! to their documented API shapes with fixture-tested response
//! parsers. They CANNOT be verified against the live services until
//! platform merchant credentials exist: without complete credentials
//! every network operation returns `NotConfigured`, exactly like
//! Steadfast, and nothing fakes success.
//!
//! Wave 11 state: eCourier ([`EcourierAdapter`]) and Paperfly
//! ([`PaperflyAdapter`]) are real adapters too, behind the same
//! credential gate and with the same fixture-tested defensive
//! parsers — all five API couriers (Tier 1 Steadfast / Pathao /
//! RedX, Tier 1b eCourier / Paperfly) now have adapters. The manual
//! Phase 2 couriers (Sundarban, SA Paribahan, Karatoa, Carrybee)
//! have no APIs to adapt to; since Wave 19 they run as a
//! seller-driven manual workflow in the API's shipments module —
//! deliberately NOT as adapters here, since there is no API for
//! an adapter to call.
//! [`DevCourierAdapter`] is a clearly-labelled SIMULATOR for local
//! and CI use only, reachable only through [`courier_adapter_for`]
//! when `CouriersConfig::dev_mode` is true. Platform master accounts
//! are the model throughout: sellers need no courier account of
//! their own.

use async_trait::async_trait;
use domain::DeliveryZone;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The couriers this platform knows by name, in display order.
pub const KNOWN_COURIERS: [&str; 5] = ["steadfast", "pathao", "redx", "ecourier", "paperfly"];

/// Is this a courier name the platform recognises?
pub fn is_known_courier(name: &str) -> bool {
    KNOWN_COURIERS.contains(&name)
}

/// A parcel to be delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parcel {
    pub weight_kg: u32,
    /// Cash-on-delivery amount to collect, in poisha (0 = prepaid).
    pub cod_amount_poisha: i64,
    pub zone: DeliveryZone,
}

/// Everything needed to book one shipment with any courier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShipmentRequest {
    /// Platform sub-order id — also the idempotency key.
    pub order_id: String,
    pub store_id: String,
    /// Merchant-facing invoice number for the consignment (Steadfast
    /// requires one; the API passes a short form of the sub-order id).
    pub invoice: String,
    pub recipient_name: String,
    pub recipient_phone: String,
    pub recipient_address: String,
    pub parcel: Parcel,
    /// Optional delivery note for the courier.
    pub note: Option<String>,
    /// Recipient area id, for couriers whose API addresses areas by
    /// numeric id (RedX requires one to book a parcel). Most
    /// couriers ignore it; the API passes it from the booking
    /// request when the seller picked such a courier.
    pub area_id: Option<i64>,
}

/// A booked shipment, as returned by an adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shipment {
    pub courier: String,
    /// Courier-side consignment / tracking id.
    pub consignment_id: String,
    /// Courier-side tracking code, when the courier issues a
    /// separate one (Steadfast does; the dev simulator does not).
    pub tracking_code: Option<String>,
    /// Unified shipment status (domain crate's set), e.g. `created`.
    pub status: String,
}

/// The result of one tracking poll: the unified status, plus the
/// courier's raw status string when the courier provided one (kept
/// for the shipment event audit trail; `None` for the dev simulator,
/// which has no raw status of its own).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackingUpdate {
    pub status: String,
    pub raw_status: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CourierError {
    /// The adapter exists but has no merchant credentials configured yet.
    #[error("courier '{0}' is not configured (no merchant credentials)")]
    NotConfigured(String),
    /// This courier does not support the requested operation.
    #[error("courier '{courier}' does not support operation '{operation}'")]
    Unsupported { courier: String, operation: String },
    /// The courier's API was reached (or attempted) and failed: a
    /// transport error, an HTTP error, or an error payload from the
    /// courier itself. The message carries the courier's own words
    /// when it sent any.
    #[error("courier gateway error: {0}")]
    Gateway(String),
    /// The factory was asked for a courier name it does not know.
    #[error("unknown courier '{0}'")]
    UnknownCourier(String),
}

/// Merchant credentials and API base URL for one courier, as plain
/// config values. The API crate fills these from environment
/// variables; this crate never reads the environment itself.
#[derive(Debug, Clone)]
pub struct CourierConfig {
    pub api_key: Option<String>,
    pub secret_key: Option<String>,
    pub base_url: String,
}

/// Steadfast's production API base URL (their portal API, v1).
pub const DEFAULT_STEADFAST_BASE_URL: &str = "https://portal.packzy.com/api/v1";

impl Default for CourierConfig {
    fn default() -> Self {
        CourierConfig {
            api_key: None,
            secret_key: None,
            base_url: DEFAULT_STEADFAST_BASE_URL.to_string(),
        }
    }
}

fn present(value: &Option<String>) -> bool {
    value.as_ref().is_some_and(|v| !v.trim().is_empty())
}

/// Are the credentials a courier API needs all present? Steadfast
/// needs an API key and a secret key.
pub fn courier_credentials_complete(config: &CourierConfig) -> bool {
    present(&config.api_key) && present(&config.secret_key)
}

/// Pathao merchant credentials and API base URL, as plain config
/// values (the API crate fills them from environment variables; this
/// crate never reads the environment itself). Pathao's merchant API
/// authenticates with an OAuth-style client-credentials token: the
/// adapter exchanges `client_id` + `client_secret` for a bearer
/// token, and every order names the merchant's numeric `store_id`.
#[derive(Debug, Clone)]
pub struct PathaoConfig {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    /// The merchant's Pathao store id (sent as a JSON number).
    pub store_id: Option<String>,
    pub base_url: String,
}

/// Pathao's production API base URL (the "Hermes" merchant API).
pub const DEFAULT_PATHAO_BASE_URL: &str = "https://api-hermes.pathao.com";

impl Default for PathaoConfig {
    fn default() -> Self {
        PathaoConfig {
            client_id: None,
            client_secret: None,
            store_id: None,
            base_url: DEFAULT_PATHAO_BASE_URL.to_string(),
        }
    }
}

/// Are Pathao's credentials all present? It needs a client id, a
/// client secret AND a store id — a token alone cannot book an
/// order, because every order names its store.
pub fn pathao_credentials_complete(config: &PathaoConfig) -> bool {
    present(&config.client_id) && present(&config.client_secret) && present(&config.store_id)
}

/// RedX merchant credentials and API base URL, as plain config
/// values (the API crate fills them from environment variables; this
/// crate never reads the environment itself).
#[derive(Debug, Clone)]
pub struct RedxConfig {
    pub api_key: Option<String>,
    pub base_url: String,
}

/// RedX's production open-API base URL (v1).
pub const DEFAULT_REDX_BASE_URL: &str = "https://openapi.redx.com.bd/v1";

impl Default for RedxConfig {
    fn default() -> Self {
        RedxConfig {
            api_key: None,
            base_url: DEFAULT_REDX_BASE_URL.to_string(),
        }
    }
}

/// Are RedX's credentials all present? A single API access token is
/// the whole credential set.
pub fn redx_credentials_complete(config: &RedxConfig) -> bool {
    present(&config.api_key)
}

/// eCourier merchant credentials and API base URL, as plain config
/// values (the API crate fills them from environment variables; this
/// crate never reads the environment itself). eCourier's documented
/// auth is TWO headers: an API auth token and the merchant's user
/// id. The exact header NAMES vary between eCourier doc versions —
/// the adapter sends `API-AUTH-TOKEN` and `USER-ID`, and both names
/// are flagged in the README as merchant-onboarding confirm items,
/// the same honesty treatment as RedX's auth header.
#[derive(Debug, Clone)]
pub struct EcourierConfig {
    pub api_key: Option<String>,
    pub user_id: Option<String>,
    pub base_url: String,
}

/// eCourier's production API base URL (their backoffice API).
pub const DEFAULT_ECOURIER_BASE_URL: &str = "https://backoffice.ecourier.com.bd/api";

impl Default for EcourierConfig {
    fn default() -> Self {
        EcourierConfig {
            api_key: None,
            user_id: None,
            base_url: DEFAULT_ECOURIER_BASE_URL.to_string(),
        }
    }
}

/// Are eCourier's credentials all present? The API auth token AND
/// the merchant user id — eCourier identifies the caller by the
/// pair, so one without the other cannot book or track.
pub fn ecourier_credentials_complete(config: &EcourierConfig) -> bool {
    present(&config.api_key) && present(&config.user_id)
}

/// Paperfly merchant credentials, pickup details and API base URL,
/// as plain config values (the API crate fills them from environment
/// variables; this crate never reads the environment itself).
/// Paperfly's documented auth is HTTP Basic (merchant username +
/// password) PLUS a merchant key sent in a `paperflykey` header —
/// the header name is flagged in the README as a merchant-
/// onboarding confirm item. The pickup fields are the platform's
/// own pickup point (a platform master account collects from one
/// place): Paperfly requires them on every order placement, so they
/// live in config rather than in each shipment request.
#[derive(Debug, Clone)]
pub struct PaperflyConfig {
    pub username: Option<String>,
    pub password: Option<String>,
    /// The merchant key sent in the `paperflykey` header.
    pub merchant_id: Option<String>,
    /// Pickup contact name at the platform's pickup point.
    pub pickup_name: Option<String>,
    /// Pickup contact phone at the platform's pickup point.
    pub pickup_phone: Option<String>,
    /// Pickup address of the platform's pickup point.
    pub pickup_address: Option<String>,
    pub base_url: String,
}

/// Paperfly's production API base URL.
pub const DEFAULT_PAPERFLY_BASE_URL: &str = "https://api.paperfly.com.bd";

impl Default for PaperflyConfig {
    fn default() -> Self {
        PaperflyConfig {
            username: None,
            password: None,
            merchant_id: None,
            pickup_name: None,
            pickup_phone: None,
            pickup_address: None,
            base_url: DEFAULT_PAPERFLY_BASE_URL.to_string(),
        }
    }
}

/// Are Paperfly's credentials all present? Username, password AND
/// the merchant key — Basic auth alone cannot place an order,
/// because every call also carries the key header. The pickup
/// fields are NOT credentials: they gate order placement (with a
/// gateway error naming what is missing), not this completeness
/// check, because a merchant can be fully credentialed before its
/// pickup point is agreed.
pub fn paperfly_credentials_complete(config: &PaperflyConfig) -> bool {
    present(&config.username) && present(&config.password) && present(&config.merchant_id)
}

/// Courier configuration for the whole platform: the dev simulator
/// switch, each real courier's credentials, and the default courier
/// sellers get when they do not pick one.
#[derive(Debug, Clone)]
pub struct CouriersConfig {
    /// Enables the DEV courier simulator. Local/CI only — never true
    /// in a real deployment.
    pub dev_mode: bool,
    pub steadfast: CourierConfig,
    pub pathao: PathaoConfig,
    pub redx: RedxConfig,
    pub ecourier: EcourierConfig,
    pub paperfly: PaperflyConfig,
    /// Courier name used when a booking does not name one.
    pub default_courier: String,
}

impl Default for CouriersConfig {
    fn default() -> Self {
        CouriersConfig {
            dev_mode: false,
            steadfast: CourierConfig::default(),
            pathao: PathaoConfig::default(),
            redx: RedxConfig::default(),
            ecourier: EcourierConfig::default(),
            paperfly: PaperflyConfig::default(),
            default_courier: "steadfast".to_string(),
        }
    }
}

/// Convert integer poisha to a taka amount for a courier API
/// boundary. Steadfast's API takes `cod_amount` as a JSON number in
/// TAKA, so this is the one place a float touches money: the value
/// is derived from exact integer poisha with two-decimal precision
/// (4,690 poisha becomes 46.90) and is never stored or compared —
/// money everywhere else in the platform stays integer poisha.
pub fn poisha_to_taka(poisha: i64) -> f64 {
    poisha as f64 / 100.0
}

/// Convert integer poisha to WHOLE taka for a courier API boundary
/// that takes integer taka amounts (Pathao's `amount_to_collect`).
/// Rounding is half-up: 49 poisha becomes 0 taka, 50 poisha becomes
/// 1 taka, 150 poisha becomes 2 taka. Amounts here are COD totals
/// (never negative in practice); a negative input is clamped to 0
/// rather than rounded into a negative collection.
pub fn poisha_to_whole_taka(poisha: i64) -> i64 {
    if poisha <= 0 {
        return 0;
    }
    (poisha + 50) / 100
}

/// One adapter per courier, one method per operation.
#[async_trait]
pub trait CourierAdapter: Send + Sync {
    /// Human/machine name of the courier, e.g. "steadfast".
    fn name(&self) -> &str;

    async fn create_shipment(&self, req: &ShipmentRequest) -> Result<Shipment, CourierError>;

    async fn track(&self, consignment_id: &str) -> Result<TrackingUpdate, CourierError>;

    async fn cancel(&self, consignment_id: &str) -> Result<(), CourierError>;

    /// Delivery charge estimate in poisha for a parcel.
    async fn price_estimate(&self, parcel: &Parcel) -> Result<i64, CourierError>;
}

// ---------------------------------------------------------------------------
// Steadfast (real adapter, credential-gated)
// ---------------------------------------------------------------------------

/// Steadfast (portal.packzy.com) adapter. Talks to the real API when
/// constructed with complete credentials; without them every network
/// operation returns `NotConfigured` and nothing is sent anywhere.
/// `price_estimate` is the exception: it is a purely local estimate
/// from the domain delivery-charge function, so it works with or
/// without credentials and is labelled an estimate, never a quote
/// from Steadfast itself.
#[derive(Debug)]
pub struct SteadfastAdapter {
    config: CourierConfig,
    base_url: String,
    client: reqwest::Client,
}

impl SteadfastAdapter {
    pub fn new(config: CourierConfig) -> Self {
        let base_url = config.base_url.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("reqwest client builds with the rustls TLS backend");
        SteadfastAdapter {
            config,
            base_url,
            client,
        }
    }

    fn check_configured(&self) -> Result<(), CourierError> {
        if courier_credentials_complete(&self.config) {
            Ok(())
        } else {
            Err(CourierError::NotConfigured("steadfast".to_string()))
        }
    }

    fn api_key(&self) -> &str {
        self.config.api_key.as_deref().unwrap_or_default()
    }

    fn secret_key(&self) -> &str {
        self.config.secret_key.as_deref().unwrap_or_default()
    }
}

/// Extract a human-readable message from a courier payload: their
/// `message` field when present, else their `errors` object rendered
/// compactly, else a generic line naming the HTTP status. Shared by
/// every real adapter — courier error payloads all carry a
/// `message`-shaped field in practice.
fn gateway_message(
    courier: &str,
    payload: &Value,
    http_status: reqwest::StatusCode,
) -> String {
    if let Some(message) = payload.get("message").and_then(Value::as_str) {
        if !message.trim().is_empty() {
            return message.to_string();
        }
    }
    if let Some(errors) = payload.get("errors") {
        return format!("{courier} rejected the request: {errors}");
    }
    format!("{courier} request failed with HTTP {http_status}")
}

/// The recipient phone in the 11-digit local form (`01XXXXXXXXX`)
/// that Pathao and RedX require, converted from the platform's
/// canonical `+880…` form by the single domain conversion. If the
/// stored number cannot be converted (checkout stores what the
/// buyer typed, so it should not happen), the raw value is passed
/// through and the courier's own validation will reject it if it is
/// wrong — we never invent a number.
fn local_phone_or_raw(phone: &str) -> String {
    domain::to_local_phone(phone).unwrap_or_else(|| phone.to_string())
}

/// Find a courier-side id in a response payload: each field name is
/// tried at the top level first, then under a `data` object, and a
/// value counts whether the courier sends it as a string or as a
/// number. Shared by the eCourier and Paperfly create parsers —
/// their exact id field names are among the least certain parts of
/// their documented shapes, so the parsers accept every known name
/// rather than betting on one.
fn find_response_id(payload: &Value, fields: &[&str]) -> Option<String> {
    let data = payload.get("data");
    for field in fields {
        for scope in [Some(payload), data].into_iter().flatten() {
            match scope.get(field) {
                Some(Value::String(s)) if !s.trim().is_empty() => {
                    return Some(s.clone());
                }
                Some(Value::Number(n)) => return Some(n.to_string()),
                _ => {}
            }
        }
    }
    None
}

/// Find a raw status string in a tracking response payload: the
/// known field names are tried at the top level first, then under a
/// `data` object. Shared by the RedX, eCourier and Paperfly track
/// parsers — all three couriers' tracking payloads are `message`-
/// plus-status shaped in their docs, but none of the three names
/// its status field consistently across doc versions.
fn find_response_status(payload: &Value, fields: &[&str]) -> Option<String> {
    let data = payload.get("data");
    for scope in [Some(payload), data].into_iter().flatten() {
        for field in fields {
            if let Some(raw) = scope
                .get(field)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
            {
                return Some(raw.to_string());
            }
        }
    }
    None
}

/// Parse a Steadfast `create_order` response body into a [`Shipment`].
/// Pure, so the response shapes are unit-testable without a network.
/// Success is the body's own verdict: `status` field 200 AND a
/// `consignment` object carrying a `consignment_id` (Steadfast sends
/// it as a number; a string is accepted too). Anything else is a
/// gateway error carrying Steadfast's message when it sent one.
pub fn parse_create_order_response(courier: &str, payload: &Value) -> Result<Shipment, CourierError> {
    let body_status = payload.get("status").and_then(Value::as_i64);
    if body_status != Some(200) {
        let message = payload
            .get("message")
            .and_then(Value::as_str)
            .filter(|m| !m.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "steadfast create_order failed (response status: {})",
                    body_status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "absent".to_string())
                )
            });
        return Err(CourierError::Gateway(message));
    }
    let consignment = payload
        .get("consignment")
        .ok_or_else(|| {
            CourierError::Gateway(
                "steadfast create_order response had no consignment object".to_string(),
            )
        })?;
    let consignment_id = match consignment.get("consignment_id") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            return Err(CourierError::Gateway(
                "steadfast create_order response had no consignment_id".to_string(),
            ))
        }
    };
    let tracking_code = consignment
        .get("tracking_code")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(Shipment {
        courier: courier.to_string(),
        consignment_id,
        tracking_code,
        status: "created".to_string(),
    })
}

/// Parse a Steadfast `status_by_cid` response body into the RAW
/// `delivery_status` string. Pure, so it is unit-testable without a
/// network. Mapping the raw value onto the unified set is NOT done
/// here — callers use `domain::map_steadfast_status`, the single
/// mapping implementation.
pub fn parse_status_response(payload: &Value) -> Result<String, CourierError> {
    payload
        .get("delivery_status")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            CourierError::Gateway(
                "steadfast status response had no delivery_status".to_string(),
            )
        })
}

#[async_trait]
impl CourierAdapter for SteadfastAdapter {
    fn name(&self) -> &str {
        "steadfast"
    }

    async fn create_shipment(&self, req: &ShipmentRequest) -> Result<Shipment, CourierError> {
        self.check_configured()?;
        let mut body = serde_json::Map::new();
        body.insert("invoice".to_string(), json!(req.invoice));
        body.insert("recipient_name".to_string(), json!(req.recipient_name));
        body.insert("recipient_phone".to_string(), json!(req.recipient_phone));
        body.insert(
            "recipient_address".to_string(),
            json!(req.recipient_address),
        );
        body.insert(
            "cod_amount".to_string(),
            json!(poisha_to_taka(req.parcel.cod_amount_poisha)),
        );
        if let Some(note) = &req.note {
            body.insert("note".to_string(), json!(note));
        }
        let url = format!("{}/create_order", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("Api-Key", self.api_key())
            .header("Secret-Key", self.secret_key())
            .header("Content-Type", "application/json")
            .json(&Value::Object(body))
            .send()
            .await
            .map_err(|e| {
                CourierError::Gateway(format!("steadfast create_order request failed: {e}"))
            })?;
        let http_status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            CourierError::Gateway(format!(
                "steadfast create_order returned an unreadable response: {e}"
            ))
        })?;
        if !http_status.is_success() {
            return Err(CourierError::Gateway(gateway_message(
                self.name(),
                &payload,
                http_status,
            )));
        }
        parse_create_order_response(self.name(), &payload)
    }

    async fn track(&self, consignment_id: &str) -> Result<TrackingUpdate, CourierError> {
        self.check_configured()?;
        let url = format!("{}/status_by_cid/{consignment_id}", self.base_url);
        let response = self
            .client
            .get(&url)
            .header("Api-Key", self.api_key())
            .header("Secret-Key", self.secret_key())
            .send()
            .await
            .map_err(|e| {
                CourierError::Gateway(format!("steadfast status request failed: {e}"))
            })?;
        let http_status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            CourierError::Gateway(format!(
                "steadfast status returned an unreadable response: {e}"
            ))
        })?;
        if !http_status.is_success() {
            return Err(CourierError::Gateway(gateway_message(
                self.name(),
                &payload,
                http_status,
            )));
        }
        let raw_status = parse_status_response(&payload)?;
        Ok(TrackingUpdate {
            status: domain::map_steadfast_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
        })
    }

    async fn cancel(&self, _consignment_id: &str) -> Result<(), CourierError> {
        self.check_configured()?;
        // Honest limit: Steadfast's basic portal API has no endpoint
        // to cancel a created consignment — cancellations are handled
        // in their merchant panel. We report that instead of
        // pretending a cancel happened.
        Err(CourierError::Unsupported {
            courier: "steadfast".to_string(),
            operation: "cancel".to_string(),
        })
    }

    async fn price_estimate(&self, parcel: &Parcel) -> Result<i64, CourierError> {
        // A purely local estimate from the domain delivery-charge
        // function (zone base + per-started-kg). No network call and
        // no credentials are involved, and the result is an estimate
        // under OUR tariff — Steadfast's own charge is confirmed on
        // the consignment after booking.
        Ok(domain::calculate_delivery_charge(
            parcel.zone,
            parcel.weight_kg,
            parcel.cod_amount_poisha,
        ))
    }
}

// ---------------------------------------------------------------------------
// Pathao (real adapter, credential-gated)
// ---------------------------------------------------------------------------

/// Pathao (api-hermes.pathao.com, the "Hermes" merchant API)
/// adapter. Coded to Pathao's documented API shapes — an OAuth-style
/// client-credentials token exchange, then bearer-authenticated
/// order creation and lookup — with every response parsed by a pure,
/// fixture-tested parser. It CANNOT be verified against the live
/// service until platform merchant credentials exist: without
/// complete credentials every network operation returns
/// `NotConfigured` and nothing is sent anywhere. `price_estimate`
/// is the exception, as with Steadfast: a purely local domain
/// estimate (Pathao's own price-plan endpoint is an authenticated
/// POST, which would make smart routing network-dependent and slow;
/// routing stays offline and fast, and the estimate is labelled an
/// estimate under our tariff, never a Pathao quote).
#[derive(Debug)]
pub struct PathaoAdapter {
    config: PathaoConfig,
    base_url: String,
    client: reqwest::Client,
    /// The cached bearer token and the instant it stops being safe
    /// to use. Pathao's docs describe long-lived tokens (~30 days),
    /// but the adapter trusts the token response's own `expires_in`
    /// when it sends one (falling back to one hour) and refreshes a
    /// minute early, rather than hard-coding a lifetime.
    token_cache: Mutex<Option<(String, std::time::Instant)>>,
}

impl PathaoAdapter {
    pub fn new(config: PathaoConfig) -> Self {
        let base_url = config.base_url.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("reqwest client builds with the rustls TLS backend");
        PathaoAdapter {
            config,
            base_url,
            client,
            token_cache: Mutex::new(None),
        }
    }

    fn check_configured(&self) -> Result<(), CourierError> {
        if pathao_credentials_complete(&self.config) {
            Ok(())
        } else {
            Err(CourierError::NotConfigured("pathao".to_string()))
        }
    }

    /// The merchant's numeric store id, parsed from config. The
    /// store id is a string in config because environment variables
    /// are strings; Pathao's API wants a JSON number.
    fn store_id(&self) -> Result<i64, CourierError> {
        self.config
            .store_id
            .as_deref()
            .unwrap_or_default()
            .trim()
            .parse::<i64>()
            .map_err(|_| {
                CourierError::Gateway(
                    "pathao store id is not a number — check PATHAO_STORE_ID".to_string(),
                )
            })
    }

    /// A valid bearer token: the cached one while it is fresh, else
    /// a newly issued one from Pathao's token endpoint.
    async fn access_token(&self) -> Result<String, CourierError> {
        self.check_configured()?;
        {
            let cache = self
                .token_cache
                .lock()
                .expect("pathao token cache lock is not poisoned");
            if let Some((token, valid_until)) = cache.as_ref() {
                if std::time::Instant::now() < *valid_until {
                    return Ok(token.clone());
                }
            }
        }
        let url = format!("{}/aladdin/api/v1/issue-token", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&json!({
                "client_id": self.config.client_id.as_deref().unwrap_or_default(),
                "client_secret": self.config.client_secret.as_deref().unwrap_or_default(),
                "grant_type": "client_credentials",
            }))
            .send()
            .await
            .map_err(|e| {
                CourierError::Gateway(format!("pathao token request failed: {e}"))
            })?;
        let http_status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            CourierError::Gateway(format!(
                "pathao token endpoint returned an unreadable response: {e}"
            ))
        })?;
        if !http_status.is_success() {
            return Err(CourierError::Gateway(gateway_message(
                self.name(),
                &payload,
                http_status,
            )));
        }
        let (token, expires_in) = parse_pathao_token_response(&payload)?;
        // Refresh a minute early rather than racing the expiry.
        let ttl = expires_in.saturating_sub(60).max(60);
        let valid_until = std::time::Instant::now() + std::time::Duration::from_secs(ttl);
        *self
            .token_cache
            .lock()
            .expect("pathao token cache lock is not poisoned") = Some((token.clone(), valid_until));
        Ok(token)
    }
}

/// Parse a Pathao `issue-token` response body into the access token
/// and its lifetime in seconds. Pure, so it is unit-testable without
/// a network. The token is accepted wherever Pathao puts it
/// (`data.access_token`, or a top-level `access_token`); a missing
/// `expires_in` falls back to 3,600 seconds. A payload with no token
/// at all is a gateway error carrying Pathao's `message` when it
/// sent one.
pub fn parse_pathao_token_response(payload: &Value) -> Result<(String, u64), CourierError> {
    let data = payload.get("data");
    let token = data
        .and_then(|d| d.get("access_token"))
        .or_else(|| payload.get("access_token"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());
    let token = match token {
        Some(token) => token.to_string(),
        None => {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .filter(|m| !m.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    "pathao token response had no access_token".to_string()
                });
            return Err(CourierError::Gateway(message));
        }
    };
    let expires_in = data
        .and_then(|d| d.get("expires_in"))
        .or_else(|| payload.get("expires_in"))
        .and_then(Value::as_u64)
        .unwrap_or(3_600);
    Ok((token, expires_in))
}

/// Parse a Pathao create-order response body into a [`Shipment`].
/// Pure, so it is unit-testable without a network. Success is the
/// body's own verdict: `code` field 200 AND a `data` object carrying
/// a `consignment_id` (a number; a string is accepted too). The
/// shipment's status comes from `data.order_status` through the
/// single domain mapping; a fresh order whose reported status is
/// missing or unrecognised is recorded as `created` — it WAS
/// created, and the next tracking poll will move it.
pub fn parse_pathao_create_response(
    courier: &str,
    payload: &Value,
) -> Result<Shipment, CourierError> {
    let code = payload.get("code").and_then(Value::as_i64);
    if code != Some(200) {
        let message = payload
            .get("message")
            .and_then(Value::as_str)
            .filter(|m| !m.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "pathao create order failed (response code: {})",
                    code.map(|c| c.to_string())
                        .unwrap_or_else(|| "absent".to_string())
                )
            });
        return Err(CourierError::Gateway(message));
    }
    let data = payload.get("data").ok_or_else(|| {
        CourierError::Gateway("pathao create order response had no data object".to_string())
    })?;
    let consignment_id = match data.get("consignment_id") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            return Err(CourierError::Gateway(
                "pathao create order response had no consignment_id".to_string(),
            ))
        }
    };
    let status = data
        .get("order_status")
        .and_then(Value::as_str)
        .map(|raw| domain::map_pathao_status(raw))
        .filter(|mapped| *mapped != "unknown")
        .unwrap_or("created");
    Ok(Shipment {
        courier: courier.to_string(),
        consignment_id,
        // Pathao's consignment id IS its tracking id; there is no
        // separate tracking code.
        tracking_code: None,
        status: status.to_string(),
    })
}

/// Parse a Pathao order-lookup response body into the RAW
/// `order_status` string. Pure, so it is unit-testable without a
/// network. Mapping the raw value onto the unified set is NOT done
/// here — callers use `domain::map_pathao_status`, the single
/// mapping implementation.
pub fn parse_pathao_track_response(payload: &Value) -> Result<String, CourierError> {
    if let Some(code) = payload.get("code").and_then(Value::as_i64) {
        if code != 200 {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .filter(|m| !m.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("pathao order lookup failed (response code: {code})"));
            return Err(CourierError::Gateway(message));
        }
    }
    payload
        .get("data")
        .and_then(|d| d.get("order_status"))
        .or_else(|| payload.get("order_status"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            CourierError::Gateway(
                "pathao order lookup response had no order_status".to_string(),
            )
        })
}

#[async_trait]
impl CourierAdapter for PathaoAdapter {
    fn name(&self) -> &str {
        "pathao"
    }

    async fn create_shipment(&self, req: &ShipmentRequest) -> Result<Shipment, CourierError> {
        self.check_configured()?;
        let store_id = self.store_id()?;
        let token = self.access_token().await?;
        let mut body = serde_json::Map::new();
        body.insert("store_id".to_string(), json!(store_id));
        body.insert("merchant_order_id".to_string(), json!(req.invoice));
        body.insert("recipient_name".to_string(), json!(req.recipient_name));
        // Pathao requires the recipient phone in the 11-digit local
        // form (01XXXXXXXXX); the platform stores +880 canonical.
        body.insert(
            "recipient_phone".to_string(),
            json!(local_phone_or_raw(&req.recipient_phone)),
        );
        body.insert(
            "recipient_address".to_string(),
            json!(req.recipient_address),
        );
        body.insert("item_quantity".to_string(), json!(1));
        body.insert(
            "item_weight".to_string(),
            json!(f64::from(req.parcel.weight_kg)),
        );
        // Pathao collects WHOLE taka; poisha are rounded half-up at
        // this boundary only (poisha_to_whole_taka).
        body.insert(
            "amount_to_collect".to_string(),
            json!(poisha_to_whole_taka(req.parcel.cod_amount_poisha)),
        );
        // 48 = Pathao's standard (normal) delivery type.
        body.insert("delivery_type".to_string(), json!(48));
        if let Some(note) = &req.note {
            body.insert("special_instruction".to_string(), json!(note));
        }
        let url = format!("{}/aladdin/api/v1/orders", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(&Value::Object(body))
            .send()
            .await
            .map_err(|e| {
                CourierError::Gateway(format!("pathao create order request failed: {e}"))
            })?;
        let http_status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            CourierError::Gateway(format!(
                "pathao create order returned an unreadable response: {e}"
            ))
        })?;
        if !http_status.is_success() {
            return Err(CourierError::Gateway(gateway_message(
                self.name(),
                &payload,
                http_status,
            )));
        }
        parse_pathao_create_response(self.name(), &payload)
    }

    async fn track(&self, consignment_id: &str) -> Result<TrackingUpdate, CourierError> {
        self.check_configured()?;
        let token = self.access_token().await?;
        let url = format!("{}/aladdin/api/v1/orders/{consignment_id}", self.base_url);
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| {
                CourierError::Gateway(format!("pathao order lookup request failed: {e}"))
            })?;
        let http_status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            CourierError::Gateway(format!(
                "pathao order lookup returned an unreadable response: {e}"
            ))
        })?;
        if !http_status.is_success() {
            return Err(CourierError::Gateway(gateway_message(
                self.name(),
                &payload,
                http_status,
            )));
        }
        let raw_status = parse_pathao_track_response(&payload)?;
        Ok(TrackingUpdate {
            status: domain::map_pathao_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
        })
    }

    async fn cancel(&self, _consignment_id: &str) -> Result<(), CourierError> {
        self.check_configured()?;
        // Honest limit: Pathao's merchant API has no simple cancel
        // endpoint for a created order — cancellations go through
        // Pathao support / the merchant panel. We report that
        // instead of pretending a cancel happened.
        Err(CourierError::Unsupported {
            courier: "pathao".to_string(),
            operation: "cancel".to_string(),
        })
    }

    async fn price_estimate(&self, parcel: &Parcel) -> Result<i64, CourierError> {
        // A purely local estimate from the domain delivery-charge
        // function, exactly like Steadfast's. Pathao's own price
        // plan endpoint is an authenticated POST; calling it per
        // routing decision would make routing network-dependent and
        // slow, so routing stays offline. The result is an estimate
        // under OUR tariff — Pathao's own charge is confirmed on the
        // consignment after booking.
        Ok(domain::calculate_delivery_charge(
            parcel.zone,
            parcel.weight_kg,
            parcel.cod_amount_poisha,
        ))
    }
}

// ---------------------------------------------------------------------------
// RedX (real adapter, credential-gated)
// ---------------------------------------------------------------------------

/// RedX (openapi.redx.com.bd) adapter. Coded to RedX's documented
/// open-API shapes with defensive, fixture-tested parsers. It
/// CANNOT be verified against the live service until platform
/// merchant credentials exist: without an API key every network
/// operation returns `NotConfigured` and nothing is sent anywhere.
/// Two documented uncertainties must be confirmed at merchant
/// onboarding (see the README's Wave 7 section): the exact
/// `API-ACCESS-TOKEN` header format (this adapter sends the raw key
/// as the header value, per RedX's docs style), and the tracking
/// endpoint path (this adapter uses `GET /tracking/{tracking_id}`
/// and parses the status defensively from several known field
/// names). `price_estimate` is a purely local domain estimate, like
/// the other adapters — labelled an estimate, never a RedX quote.
#[derive(Debug)]
pub struct RedxAdapter {
    config: RedxConfig,
    base_url: String,
    client: reqwest::Client,
}

impl RedxAdapter {
    pub fn new(config: RedxConfig) -> Self {
        let base_url = config.base_url.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("reqwest client builds with the rustls TLS backend");
        RedxAdapter {
            config,
            base_url,
            client,
        }
    }

    fn check_configured(&self) -> Result<(), CourierError> {
        if redx_credentials_complete(&self.config) {
            Ok(())
        } else {
            Err(CourierError::NotConfigured("redx".to_string()))
        }
    }

    fn api_key(&self) -> &str {
        self.config.api_key.as_deref().unwrap_or_default()
    }
}

/// Parse a RedX create-parcel response body into a [`Shipment`].
/// Pure, so it is unit-testable without a network. Success is a
/// non-empty `tracking_id` (top-level, or nested under `data`) —
/// that id is both the consignment id and the customer-facing
/// tracking code for RedX. Anything else is a gateway error carrying
/// RedX's `message` when it sent one.
pub fn parse_redx_create_response(
    courier: &str,
    payload: &Value,
) -> Result<Shipment, CourierError> {
    let tracking_id = payload
        .get("tracking_id")
        .or_else(|| payload.get("data").and_then(|d| d.get("tracking_id")))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());
    match tracking_id {
        Some(id) => Ok(Shipment {
            courier: courier.to_string(),
            consignment_id: id.to_string(),
            tracking_code: Some(id.to_string()),
            status: "created".to_string(),
        }),
        None => {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .filter(|m| !m.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    "redx create parcel response had no tracking_id".to_string()
                });
            Err(CourierError::Gateway(message))
        }
    }
}

/// The status field names a RedX tracking response might use, in
/// the order they are tried. RedX's tracking payload shape is the
/// least certain of the three couriers', so the parser is defensive
/// on purpose: each name is checked at the top level first, then
/// under a `data` object.
const REDX_STATUS_FIELDS: [&str; 3] = ["status", "parcel_status", "current_status"];

/// Parse a RedX tracking response body into the RAW status string.
/// Pure, so it is unit-testable without a network. Mapping the raw
/// value onto the unified set is NOT done here — callers use
/// `domain::map_redx_status`, the single mapping implementation.
pub fn parse_redx_track_response(payload: &Value) -> Result<String, CourierError> {
    find_response_status(payload, &REDX_STATUS_FIELDS).ok_or_else(|| {
        CourierError::Gateway(
            "redx tracking response had no recognisable status field".to_string(),
        )
    })
}

#[async_trait]
impl CourierAdapter for RedxAdapter {
    fn name(&self) -> &str {
        "redx"
    }

    async fn create_shipment(&self, req: &ShipmentRequest) -> Result<Shipment, CourierError> {
        self.check_configured()?;
        // RedX books parcels against numeric AREA ids — theirs, not
        // ours — and there is no honest default: without the
        // recipient's RedX area id the booking cannot be made, so it
        // fails loudly instead of guessing an area.
        let area_id = req.area_id.ok_or_else(|| {
            CourierError::Gateway(
                "redx booking requires the recipient area_id (RedX area ids are numeric and courier-specific)"
                    .to_string(),
            )
        })?;
        let mut body = serde_json::Map::new();
        body.insert("customer_name".to_string(), json!(req.recipient_name));
        // RedX requires the customer phone in the 11-digit local
        // form (01XXXXXXXXX); the platform stores +880 canonical.
        body.insert(
            "customer_phone".to_string(),
            json!(local_phone_or_raw(&req.recipient_phone)),
        );
        body.insert(
            "customer_address".to_string(),
            json!(req.recipient_address),
        );
        body.insert("area_id".to_string(), json!(area_id));
        body.insert(
            "cash_collection_amount".to_string(),
            json!(poisha_to_taka(req.parcel.cod_amount_poisha)),
        );
        // RedX takes the parcel weight in GRAMS.
        body.insert(
            "parcel_weight".to_string(),
            json!(i64::from(req.parcel.weight_kg) * 1_000),
        );
        body.insert("merchant_invoice_id".to_string(), json!(req.invoice));
        if let Some(note) = &req.note {
            body.insert("note".to_string(), json!(note));
        }
        let url = format!("{}/parcel", self.base_url);
        let response = self
            .client
            .post(&url)
            // RedX's documented auth header carries the raw API key
            // as its value. UNCERTAINTY: whether the live API expects
            // the raw key or a "Bearer <key>" form must be confirmed
            // at merchant onboarding (see README, Wave 7).
            .header("API-ACCESS-TOKEN", self.api_key())
            .header("Content-Type", "application/json")
            .json(&Value::Object(body))
            .send()
            .await
            .map_err(|e| {
                CourierError::Gateway(format!("redx create parcel request failed: {e}"))
            })?;
        let http_status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            CourierError::Gateway(format!(
                "redx create parcel returned an unreadable response: {e}"
            ))
        })?;
        if !http_status.is_success() {
            return Err(CourierError::Gateway(gateway_message(
                self.name(),
                &payload,
                http_status,
            )));
        }
        parse_redx_create_response(self.name(), &payload)
    }

    async fn track(&self, consignment_id: &str) -> Result<TrackingUpdate, CourierError> {
        self.check_configured()?;
        // UNCERTAINTY: the exact tracking path must be confirmed at
        // merchant onboarding; the parser is defensive about the
        // response shape either way (see README, Wave 7).
        let url = format!("{}/tracking/{consignment_id}", self.base_url);
        let response = self
            .client
            .get(&url)
            .header("API-ACCESS-TOKEN", self.api_key())
            .send()
            .await
            .map_err(|e| {
                CourierError::Gateway(format!("redx tracking request failed: {e}"))
            })?;
        let http_status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            CourierError::Gateway(format!(
                "redx tracking returned an unreadable response: {e}"
            ))
        })?;
        if !http_status.is_success() {
            return Err(CourierError::Gateway(gateway_message(
                self.name(),
                &payload,
                http_status,
            )));
        }
        let raw_status = parse_redx_track_response(&payload)?;
        Ok(TrackingUpdate {
            status: domain::map_redx_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
        })
    }

    async fn cancel(&self, _consignment_id: &str) -> Result<(), CourierError> {
        self.check_configured()?;
        // Honest limit: RedX's open API documents no cancel call for
        // a created parcel — cancellations go through RedX support /
        // the merchant panel. We report that instead of pretending.
        Err(CourierError::Unsupported {
            courier: "redx".to_string(),
            operation: "cancel".to_string(),
        })
    }

    async fn price_estimate(&self, parcel: &Parcel) -> Result<i64, CourierError> {
        // A purely local estimate from the domain delivery-charge
        // function, exactly like the other adapters — no network
        // call, and an estimate under OUR tariff, not a RedX quote.
        Ok(domain::calculate_delivery_charge(
            parcel.zone,
            parcel.weight_kg,
            parcel.cod_amount_poisha,
        ))
    }
}

// ---------------------------------------------------------------------------
// eCourier (real adapter, credential-gated)
// ---------------------------------------------------------------------------

/// eCourier (backoffice.ecourier.com.bd) adapter. Coded to
/// eCourier's documented API shapes with defensive, fixture-tested
/// parsers. It CANNOT be verified against the live service until
/// platform merchant credentials exist: without complete credentials
/// every network operation returns `NotConfigured` and nothing is
/// sent anywhere. Documented uncertainties to confirm at merchant
/// onboarding (see the README's Wave 11 section): the exact auth
/// header NAMES (this adapter sends the API key as `API-AUTH-TOKEN`
/// and the merchant user id as `USER-ID`; eCourier doc versions
/// differ on the names), the exact create-response id field (the
/// parser accepts `order_id`, `tracking`, `consignment_id` and
/// `e_id`, top level or under `data`), and the tracking status
/// field name (the parser accepts `status`, `order_status` and
/// `current_status`). `price_estimate` is a purely local domain
/// estimate, like the other adapters — labelled an estimate, never
/// an eCourier quote.
#[derive(Debug)]
pub struct EcourierAdapter {
    config: EcourierConfig,
    base_url: String,
    client: reqwest::Client,
}

impl EcourierAdapter {
    pub fn new(config: EcourierConfig) -> Self {
        let base_url = config.base_url.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("reqwest client builds with the rustls TLS backend");
        EcourierAdapter {
            config,
            base_url,
            client,
        }
    }

    fn check_configured(&self) -> Result<(), CourierError> {
        if ecourier_credentials_complete(&self.config) {
            Ok(())
        } else {
            Err(CourierError::NotConfigured("ecourier".to_string()))
        }
    }

    fn api_key(&self) -> &str {
        self.config.api_key.as_deref().unwrap_or_default()
    }

    fn user_id(&self) -> &str {
        self.config.user_id.as_deref().unwrap_or_default()
    }
}

/// The id field names an eCourier create-order response might use,
/// in the order they are tried. eCourier doc versions name the
/// booked order differently (`order_id`, a `tracking` code, a
/// `consignment_id`, or their short `e_id`), so the parser accepts
/// every known name rather than betting on one.
const ECOURIER_ID_FIELDS: [&str; 4] = ["order_id", "tracking", "consignment_id", "e_id"];

/// The status field names an eCourier tracking response might use,
/// in the order they are tried.
const ECOURIER_STATUS_FIELDS: [&str; 3] = ["status", "order_status", "current_status"];

/// Parse an eCourier create-order response body into a [`Shipment`].
/// Pure, so it is unit-testable without a network. Success is a
/// recognisable order id (see [`ECOURIER_ID_FIELDS`], top level or
/// nested under `data`, string or number). The `tracking` field,
/// when the response carries one distinct from the id, is kept as
/// the customer-facing tracking code. Anything else is a gateway
/// error carrying eCourier's `message` when it sent one.
pub fn parse_ecourier_create_response(
    courier: &str,
    payload: &Value,
) -> Result<Shipment, CourierError> {
    let consignment_id = find_response_id(payload, &ECOURIER_ID_FIELDS);
    match consignment_id {
        Some(id) => {
            let tracking_code = find_response_id(payload, &["tracking"])
                .filter(|tracking| *tracking != id);
            Ok(Shipment {
                courier: courier.to_string(),
                consignment_id: id,
                tracking_code,
                status: "created".to_string(),
            })
        }
        None => {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .filter(|m| !m.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    "ecourier create order response had no recognisable order id".to_string()
                });
            Err(CourierError::Gateway(message))
        }
    }
}

/// Parse an eCourier tracking response body into the RAW status
/// string. Pure, so it is unit-testable without a network. Mapping
/// the raw value onto the unified set is NOT done here — callers use
/// `domain::map_ecourier_status`, the single mapping implementation.
pub fn parse_ecourier_track_response(payload: &Value) -> Result<String, CourierError> {
    find_response_status(payload, &ECOURIER_STATUS_FIELDS).ok_or_else(|| {
        CourierError::Gateway(
            "ecourier tracking response had no recognisable status field".to_string(),
        )
    })
}

#[async_trait]
impl CourierAdapter for EcourierAdapter {
    fn name(&self) -> &str {
        "ecourier"
    }

    async fn create_shipment(&self, req: &ShipmentRequest) -> Result<Shipment, CourierError> {
        self.check_configured()?;
        // eCourier books orders against numeric AREA ids — theirs,
        // not ours — and there is no honest default: without the
        // recipient's eCourier area id the booking cannot be made,
        // so it fails loudly instead of guessing an area (the same
        // rule as RedX).
        let area_id = req.area_id.ok_or_else(|| {
            CourierError::Gateway(
                "ecourier booking requires the recipient area_id (eCourier area ids are numeric and courier-specific)"
                    .to_string(),
            )
        })?;
        let mut body = serde_json::Map::new();
        body.insert("recipient_name".to_string(), json!(req.recipient_name));
        // eCourier requires the recipient mobile in the 11-digit
        // local form (01XXXXXXXXX); the platform stores +880
        // canonical.
        body.insert(
            "recipient_mobile".to_string(),
            json!(local_phone_or_raw(&req.recipient_phone)),
        );
        body.insert(
            "recipient_address".to_string(),
            json!(req.recipient_address),
        );
        body.insert("area_id".to_string(), json!(area_id));
        // eCourier's `product_price` is the amount to collect, in
        // WHOLE taka; poisha are rounded half-up at this boundary
        // only (poisha_to_whole_taka).
        body.insert(
            "product_price".to_string(),
            json!(poisha_to_whole_taka(req.parcel.cod_amount_poisha)),
        );
        body.insert("number_of_item".to_string(), json!(1));
        body.insert(
            "weight".to_string(),
            json!(f64::from(req.parcel.weight_kg)),
        );
        body.insert("merchant_order_id".to_string(), json!(req.invoice));
        if let Some(note) = &req.note {
            body.insert("comments".to_string(), json!(note));
        }
        let url = format!("{}/order", self.base_url);
        let response = self
            .client
            .post(&url)
            // UNCERTAINTY: eCourier doc versions differ on the auth
            // header names. This adapter sends the API key as
            // `API-AUTH-TOKEN` and the merchant user id as
            // `USER-ID`; both names must be confirmed at merchant
            // onboarding (see README, Wave 11).
            .header("API-AUTH-TOKEN", self.api_key())
            .header("USER-ID", self.user_id())
            .header("Content-Type", "application/json")
            .json(&Value::Object(body))
            .send()
            .await
            .map_err(|e| {
                CourierError::Gateway(format!("ecourier create order request failed: {e}"))
            })?;
        let http_status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            CourierError::Gateway(format!(
                "ecourier create order returned an unreadable response: {e}"
            ))
        })?;
        if !http_status.is_success() {
            return Err(CourierError::Gateway(gateway_message(
                self.name(),
                &payload,
                http_status,
            )));
        }
        parse_ecourier_create_response(self.name(), &payload)
    }

    async fn track(&self, consignment_id: &str) -> Result<TrackingUpdate, CourierError> {
        self.check_configured()?;
        let url = format!("{}/track", self.base_url);
        let response = self
            .client
            .get(&url)
            .header("API-AUTH-TOKEN", self.api_key())
            .header("USER-ID", self.user_id())
            .query(&[("tracking", consignment_id)])
            .send()
            .await
            .map_err(|e| {
                CourierError::Gateway(format!("ecourier tracking request failed: {e}"))
            })?;
        let http_status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            CourierError::Gateway(format!(
                "ecourier tracking returned an unreadable response: {e}"
            ))
        })?;
        if !http_status.is_success() {
            return Err(CourierError::Gateway(gateway_message(
                self.name(),
                &payload,
                http_status,
            )));
        }
        let raw_status = parse_ecourier_track_response(&payload)?;
        Ok(TrackingUpdate {
            status: domain::map_ecourier_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
        })
    }

    async fn cancel(&self, _consignment_id: &str) -> Result<(), CourierError> {
        self.check_configured()?;
        // Honest limit: eCourier's basic API documents no cancel
        // call for a created order — cancellations go through
        // eCourier support / the merchant panel. We report that
        // instead of pretending a cancel happened.
        Err(CourierError::Unsupported {
            courier: "ecourier".to_string(),
            operation: "cancel".to_string(),
        })
    }

    async fn price_estimate(&self, parcel: &Parcel) -> Result<i64, CourierError> {
        // A purely local estimate from the domain delivery-charge
        // function, exactly like the other adapters — no network
        // call, and an estimate under OUR tariff, not an eCourier
        // quote.
        Ok(domain::calculate_delivery_charge(
            parcel.zone,
            parcel.weight_kg,
            parcel.cod_amount_poisha,
        ))
    }
}

// ---------------------------------------------------------------------------
// Paperfly (real adapter, credential-gated)
// ---------------------------------------------------------------------------

/// Paperfly (api.paperfly.com.bd) adapter. Coded to Paperfly's
/// documented API shapes with defensive, fixture-tested parsers. It
/// CANNOT be verified against the live service until platform
/// merchant credentials exist: without complete credentials every
/// network operation returns `NotConfigured` and nothing is sent
/// anywhere. Documented uncertainties to confirm at merchant
/// onboarding (see the README's Wave 11 section): the merchant-key
/// header name (this adapter sends the key as `paperflykey`
/// alongside HTTP Basic auth), the exact create-response tracking
/// field (the parser accepts `trackingId`, `tracking_id` and
/// `refNo`, top level or under `data`), and the tracking call's
/// verb and path (this adapter POSTs JSON to
/// `/API-Order-Tracking`). `price_estimate` is a purely local
/// domain estimate, like the other adapters — labelled an estimate,
/// never a Paperfly quote.
#[derive(Debug)]
pub struct PaperflyAdapter {
    config: PaperflyConfig,
    base_url: String,
    client: reqwest::Client,
}

impl PaperflyAdapter {
    pub fn new(config: PaperflyConfig) -> Self {
        let base_url = config.base_url.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("reqwest client builds with the rustls TLS backend");
        PaperflyAdapter {
            config,
            base_url,
            client,
        }
    }

    fn check_configured(&self) -> Result<(), CourierError> {
        if paperfly_credentials_complete(&self.config) {
            Ok(())
        } else {
            Err(CourierError::NotConfigured("paperfly".to_string()))
        }
    }

    /// The pickup config fields that are missing, by their
    /// environment-variable names. Paperfly requires pickup details
    /// on every order placement; they are platform config, not
    /// credentials, so a fully credentialed merchant can still be
    /// missing them — and the booking must say exactly what is
    /// missing instead of sending a half-empty order.
    fn missing_pickup_fields(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !present(&self.config.pickup_name) {
            missing.push("PAPERFLY_PICKUP_NAME");
        }
        if !present(&self.config.pickup_phone) {
            missing.push("PAPERFLY_PICKUP_PHONE");
        }
        if !present(&self.config.pickup_address) {
            missing.push("PAPERFLY_PICKUP_ADDRESS");
        }
        missing
    }
}

/// The tracking-id field names a Paperfly order-placement response
/// might use, in the order they are tried.
const PAPERFLY_ID_FIELDS: [&str; 3] = ["trackingId", "tracking_id", "refNo"];

/// The status field names a Paperfly tracking response might use,
/// in the order they are tried.
const PAPERFLY_STATUS_FIELDS: [&str; 3] = ["status", "order_status", "current_status"];

/// Parse a Paperfly order-placement response body into a
/// [`Shipment`]. Pure, so it is unit-testable without a network.
/// Success is a recognisable tracking id (see
/// [`PAPERFLY_ID_FIELDS`], top level or nested under `data`, string
/// or number) — that id is both the consignment id and the
/// customer-facing tracking code for Paperfly. Anything else is a
/// gateway error carrying Paperfly's `message` when it sent one.
pub fn parse_paperfly_create_response(
    courier: &str,
    payload: &Value,
) -> Result<Shipment, CourierError> {
    let tracking_id = find_response_id(payload, &PAPERFLY_ID_FIELDS);
    match tracking_id {
        Some(id) => Ok(Shipment {
            courier: courier.to_string(),
            consignment_id: id.clone(),
            tracking_code: Some(id),
            status: "created".to_string(),
        }),
        None => {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .filter(|m| !m.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    "paperfly order placement response had no recognisable tracking id"
                        .to_string()
                });
            Err(CourierError::Gateway(message))
        }
    }
}

/// Parse a Paperfly tracking response body into the RAW status
/// string. Pure, so it is unit-testable without a network. Mapping
/// the raw value onto the unified set is NOT done here — callers use
/// `domain::map_paperfly_status`, the single mapping implementation.
pub fn parse_paperfly_track_response(payload: &Value) -> Result<String, CourierError> {
    find_response_status(payload, &PAPERFLY_STATUS_FIELDS).ok_or_else(|| {
        CourierError::Gateway(
            "paperfly tracking response had no recognisable status field".to_string(),
        )
    })
}

#[async_trait]
impl CourierAdapter for PaperflyAdapter {
    fn name(&self) -> &str {
        "paperfly"
    }

    async fn create_shipment(&self, req: &ShipmentRequest) -> Result<Shipment, CourierError> {
        self.check_configured()?;
        // Paperfly requires the pickup point on every placement —
        // and it is platform config (a master account collects from
        // one place), never something to invent per order. Missing
        // pickup config fails loudly, naming what is missing,
        // before any network call.
        let missing = self.missing_pickup_fields();
        if !missing.is_empty() {
            return Err(CourierError::Gateway(format!(
                "paperfly booking requires pickup configuration: missing {}",
                missing.join(", ")
            )));
        }
        let mut body = serde_json::Map::new();
        body.insert("merInvoiceNo".to_string(), json!(req.invoice));
        body.insert(
            "pickMerchantName".to_string(),
            json!(self.config.pickup_name.as_deref().unwrap_or_default()),
        );
        body.insert(
            "pickMerchantPhone".to_string(),
            json!(local_phone_or_raw(
                self.config.pickup_phone.as_deref().unwrap_or_default()
            )),
        );
        body.insert(
            "pickMerchantAddress".to_string(),
            json!(self.config.pickup_address.as_deref().unwrap_or_default()),
        );
        body.insert("customerName".to_string(), json!(req.recipient_name));
        // Paperfly requires the customer phone in the 11-digit local
        // form (01XXXXXXXXX); the platform stores +880 canonical.
        body.insert(
            "customerPhone".to_string(),
            json!(local_phone_or_raw(&req.recipient_phone)),
        );
        body.insert(
            "customerAddress".to_string(),
            json!(req.recipient_address),
        );
        // Paperfly collects WHOLE taka; poisha are rounded half-up
        // at this boundary only (poisha_to_whole_taka).
        body.insert(
            "cashCollectionAmount".to_string(),
            json!(poisha_to_whole_taka(req.parcel.cod_amount_poisha)),
        );
        // ShipmentRequest carries no product title (a sub-order can
        // hold several products), so the product name is the honest
        // generic form naming the order it belongs to.
        body.insert(
            "productName".to_string(),
            json!(format!("Parcel for order {}", req.invoice)),
        );
        body.insert("productQuantity".to_string(), json!(1));
        // Paperfly takes the product weight as a kg STRING.
        body.insert(
            "productWeight".to_string(),
            json!(req.parcel.weight_kg.to_string()),
        );
        if let Some(note) = &req.note {
            body.insert("orderNote".to_string(), json!(note));
        }
        let url = format!("{}/OrderPlacement", self.base_url);
        let response = self
            .client
            .post(&url)
            // Paperfly's documented auth: HTTP Basic (merchant
            // username + password) PLUS the merchant key in a
            // header. UNCERTAINTY: the exact key header name (this
            // adapter sends `paperflykey`) must be confirmed at
            // merchant onboarding (see README, Wave 11).
            .basic_auth(
                self.config.username.as_deref().unwrap_or_default(),
                Some(self.config.password.as_deref().unwrap_or_default()),
            )
            .header(
                "paperflykey",
                self.config.merchant_id.as_deref().unwrap_or_default(),
            )
            .header("Content-Type", "application/json")
            .json(&Value::Object(body))
            .send()
            .await
            .map_err(|e| {
                CourierError::Gateway(format!("paperfly order placement request failed: {e}"))
            })?;
        let http_status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            CourierError::Gateway(format!(
                "paperfly order placement returned an unreadable response: {e}"
            ))
        })?;
        if !http_status.is_success() {
            return Err(CourierError::Gateway(gateway_message(
                self.name(),
                &payload,
                http_status,
            )));
        }
        parse_paperfly_create_response(self.name(), &payload)
    }

    async fn track(&self, consignment_id: &str) -> Result<TrackingUpdate, CourierError> {
        self.check_configured()?;
        // UNCERTAINTY: the exact tracking verb and path must be
        // confirmed at merchant onboarding; this adapter POSTs the
        // tracking id as JSON to /API-Order-Tracking, and the parser
        // is defensive about the response shape either way (see
        // README, Wave 11).
        let url = format!("{}/API-Order-Tracking", self.base_url);
        let response = self
            .client
            .post(&url)
            .basic_auth(
                self.config.username.as_deref().unwrap_or_default(),
                Some(self.config.password.as_deref().unwrap_or_default()),
            )
            .header(
                "paperflykey",
                self.config.merchant_id.as_deref().unwrap_or_default(),
            )
            .header("Content-Type", "application/json")
            .json(&json!({ "trackingId": consignment_id }))
            .send()
            .await
            .map_err(|e| {
                CourierError::Gateway(format!("paperfly tracking request failed: {e}"))
            })?;
        let http_status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            CourierError::Gateway(format!(
                "paperfly tracking returned an unreadable response: {e}"
            ))
        })?;
        if !http_status.is_success() {
            return Err(CourierError::Gateway(gateway_message(
                self.name(),
                &payload,
                http_status,
            )));
        }
        let raw_status = parse_paperfly_track_response(&payload)?;
        Ok(TrackingUpdate {
            status: domain::map_paperfly_status(&raw_status).to_string(),
            raw_status: Some(raw_status),
        })
    }

    async fn cancel(&self, _consignment_id: &str) -> Result<(), CourierError> {
        self.check_configured()?;
        // Honest limit: Paperfly's API documents no cancel call for
        // a placed order — cancellations go through Paperfly
        // support / the merchant panel. We report that instead of
        // pretending a cancel happened.
        Err(CourierError::Unsupported {
            courier: "paperfly".to_string(),
            operation: "cancel".to_string(),
        })
    }

    async fn price_estimate(&self, parcel: &Parcel) -> Result<i64, CourierError> {
        // A purely local estimate from the domain delivery-charge
        // function, exactly like the other adapters — no network
        // call, and an estimate under OUR tariff, not a Paperfly
        // quote.
        Ok(domain::calculate_delivery_charge(
            parcel.zone,
            parcel.weight_kg,
            parcel.cod_amount_poisha,
        ))
    }
}

/// Poll counts for the DEV simulator, shared process-wide: adapters
/// are constructed per call by the factory, so the progression state
/// cannot live inside one adapter instance. This is in-memory state
/// for a simulator — it resets when the process restarts, which is
/// exactly right for development and tests, and one more reason the
/// simulator must never run in production.
static DEV_POLL_COUNTS: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();

fn dev_poll_counts() -> &'static Mutex<HashMap<String, u32>> {
    DEV_POLL_COUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// DEV-ONLY courier simulator. This adapter does NOT talk to any
/// real courier and never books a real parcel: `create_shipment`
/// mints a deterministic `DEV-<courier>-<sub_order_id>` consignment
/// id, and each `track` poll advances that consignment one step
/// (picked_up, then in_transit, then out_for_delivery, then
/// delivered) so seller booking and buyer tracking flows can be
/// exercised end to end before any courier merchant account exists.
/// It must only ever be constructed through [`courier_adapter_for`]
/// when dev mode is explicitly on.
#[derive(Debug, Clone)]
pub struct DevCourierAdapter {
    courier_name: String,
}

impl DevCourierAdapter {
    pub fn new(courier_name: &str) -> Self {
        DevCourierAdapter {
            courier_name: courier_name.to_string(),
        }
    }
}

#[async_trait]
impl CourierAdapter for DevCourierAdapter {
    fn name(&self) -> &str {
        &self.courier_name
    }

    async fn create_shipment(&self, req: &ShipmentRequest) -> Result<Shipment, CourierError> {
        Ok(Shipment {
            courier: self.courier_name.clone(),
            consignment_id: format!("DEV-{}-{}", self.courier_name, req.order_id),
            tracking_code: None,
            status: "created".to_string(),
        })
    }

    async fn track(&self, consignment_id: &str) -> Result<TrackingUpdate, CourierError> {
        let poll = {
            let mut counts = dev_poll_counts()
                .lock()
                .expect("dev poll counter lock is not poisoned");
            let entry = counts.entry(consignment_id.to_string()).or_insert(0);
            *entry += 1;
            *entry
        };
        let status = match poll {
            1 => "picked_up",
            2 => "in_transit",
            3 => "out_for_delivery",
            _ => "delivered",
        };
        Ok(TrackingUpdate {
            status: status.to_string(),
            raw_status: None,
        })
    }

    async fn cancel(&self, _consignment_id: &str) -> Result<(), CourierError> {
        Ok(())
    }

    async fn price_estimate(&self, parcel: &Parcel) -> Result<i64, CourierError> {
        // Same local domain estimate as the real adapters.
        Ok(domain::calculate_delivery_charge(
            parcel.zone,
            parcel.weight_kg,
            parcel.cod_amount_poisha,
        ))
    }
}

/// THE factory: the one callable place that decides which adapter
/// serves a courier name. An unknown name is an error. In dev mode
/// every known courier is served by the DEV simulator. Otherwise
/// every known courier gets its real (credential-gated) adapter —
/// Steadfast, Pathao and RedX since Waves 6/7, eCourier and
/// Paperfly since Wave 11 — and with incomplete credentials their
/// network operations return `NotConfigured`, never fake success.
pub fn courier_adapter_for(
    name: &str,
    config: &CouriersConfig,
) -> Result<Box<dyn CourierAdapter>, CourierError> {
    if !is_known_courier(name) {
        return Err(CourierError::UnknownCourier(name.to_string()));
    }
    if config.dev_mode {
        return Ok(Box::new(DevCourierAdapter::new(name)));
    }
    match name {
        "steadfast" => Ok(Box::new(SteadfastAdapter::new(config.steadfast.clone()))),
        "pathao" => Ok(Box::new(PathaoAdapter::new(config.pathao.clone()))),
        "redx" => Ok(Box::new(RedxAdapter::new(config.redx.clone()))),
        "ecourier" => Ok(Box::new(EcourierAdapter::new(config.ecourier.clone()))),
        _ => Ok(Box::new(PaperflyAdapter::new(config.paperfly.clone()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> ShipmentRequest {
        ShipmentRequest {
            order_id: "sub-1".to_string(),
            store_id: "store-a".to_string(),
            invoice: "SUB-sub1".to_string(),
            recipient_name: "Test Buyer".to_string(),
            recipient_phone: "01712345678".to_string(),
            recipient_address: "House 1, Road 2, Dhaka".to_string(),
            parcel: Parcel {
                weight_kg: 1,
                cod_amount_poisha: 469_000,
                zone: DeliveryZone::InsideDhaka,
            },
            note: None,
            area_id: None,
        }
    }

    fn complete_steadfast() -> CourierConfig {
        CourierConfig {
            api_key: Some("api-key".to_string()),
            secret_key: Some("secret-key".to_string()),
            ..CourierConfig::default()
        }
    }

    #[test]
    fn adapter_names_match_courier_names() {
        assert_eq!(
            EcourierAdapter::new(EcourierConfig::default()).name(),
            "ecourier"
        );
        assert_eq!(
            PaperflyAdapter::new(PaperflyConfig::default()).name(),
            "paperfly"
        );
        let err = CourierError::NotConfigured("pathao".to_string());
        assert!(err.to_string().contains("not configured"));
    }

    #[test]
    fn poisha_converts_to_taka_at_the_boundary() {
        assert!((poisha_to_taka(4_690) - 46.90).abs() < 1e-9);
        assert!((poisha_to_taka(469_000) - 4_690.0).abs() < 1e-9);
        assert_eq!(poisha_to_taka(0), 0.0);
        assert!((poisha_to_taka(1) - 0.01).abs() < 1e-9);
        assert!((poisha_to_taka(10_050) - 100.50).abs() < 1e-9);
    }

    #[test]
    fn credential_completeness_needs_key_and_secret() {
        assert!(!courier_credentials_complete(&CourierConfig::default()));
        let key_only = CourierConfig {
            api_key: Some("k".to_string()),
            ..CourierConfig::default()
        };
        assert!(!courier_credentials_complete(&key_only));
        assert!(courier_credentials_complete(&complete_steadfast()));
        let blank = CourierConfig {
            api_key: Some("  ".to_string()),
            secret_key: Some("s".to_string()),
            ..CourierConfig::default()
        };
        assert!(!courier_credentials_complete(&blank));
    }

    #[test]
    fn steadfast_create_response_success_fixture() {
        // The documented success shape: body status 200 and a
        // consignment object whose id is a NUMBER.
        let payload = json!({
            "status": 200,
            "message": "Consignment has been created successfully.",
            "consignment": {
                "consignment_id": 12345,
                "invoice": "SUB-sub1",
                "tracking_code": "SF12345BD"
            }
        });
        let shipment = parse_create_order_response("steadfast", &payload)
            .expect("success payload parses");
        assert_eq!(shipment.courier, "steadfast");
        assert_eq!(shipment.consignment_id, "12345");
        assert_eq!(shipment.tracking_code.as_deref(), Some("SF12345BD"));
        assert_eq!(shipment.status, "created");

        // A string consignment id is accepted too.
        let payload = json!({
            "status": 200,
            "consignment": { "consignment_id": "67890" }
        });
        let shipment = parse_create_order_response("steadfast", &payload)
            .expect("string id payload parses");
        assert_eq!(shipment.consignment_id, "67890");
        assert_eq!(shipment.tracking_code, None);
    }

    #[test]
    fn steadfast_create_response_error_fixtures() {
        // Validation failure: body status is not 200 and Steadfast's
        // own message is carried into the error.
        let payload = json!({
            "status": 400,
            "message": "The recipient phone field is required."
        });
        let err = parse_create_order_response("steadfast", &payload)
            .expect_err("error payload is rejected");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
        assert!(err.to_string().contains("recipient phone"), "{err}");

        // Success status but no consignment: a broken response, not
        // a booking.
        let payload = json!({ "status": 200, "message": "ok" });
        let err = parse_create_order_response("steadfast", &payload)
            .expect_err("missing consignment is rejected");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");

        // Success status and consignment but no id inside it.
        let payload = json!({ "status": 200, "consignment": { "invoice": "x" } });
        let err = parse_create_order_response("steadfast", &payload)
            .expect_err("missing consignment id is rejected");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
    }

    #[test]
    fn steadfast_status_response_fixtures() {
        let payload = json!({ "status": 200, "delivery_status": "delivered" });
        assert_eq!(parse_status_response(&payload).expect("raw status"), "delivered");
        let payload = json!({ "status": 200, "delivery_status": "hold" });
        let raw = parse_status_response(&payload).expect("raw status");
        assert_eq!(domain::map_steadfast_status(&raw), "in_transit");
        let payload = json!({ "status": 200 });
        assert!(matches!(
            parse_status_response(&payload),
            Err(CourierError::Gateway(_))
        ));
    }

    #[tokio::test]
    async fn unconfigured_steadfast_never_fakes_success() {
        let adapter = SteadfastAdapter::new(CourierConfig::default());
        let err = adapter
            .create_shipment(&sample_request())
            .await
            .expect_err("no credentials, no booking");
        assert!(matches!(err, CourierError::NotConfigured(_)), "{err}");
        let err = adapter
            .track("12345")
            .await
            .expect_err("no credentials, no tracking");
        assert!(matches!(err, CourierError::NotConfigured(_)), "{err}");
        // The local price estimate still works: it is our own tariff,
        // computed by the domain function (inside Dhaka, 1 kg).
        let estimate = adapter
            .price_estimate(&sample_request().parcel)
            .await
            .expect("local estimate needs no credentials");
        assert_eq!(estimate, 6_000);
    }

    #[tokio::test]
    async fn configured_steadfast_cancel_is_honestly_unsupported() {
        let adapter = SteadfastAdapter::new(complete_steadfast());
        let err = adapter
            .cancel("12345")
            .await
            .expect_err("steadfast has no cancel endpoint");
        assert!(
            matches!(err, CourierError::Unsupported { .. }),
            "{err}"
        );
    }

    #[tokio::test]
    async fn dev_adapter_progresses_through_statuses() {
        let adapter = DevCourierAdapter::new("steadfast");
        let shipment = adapter
            .create_shipment(&sample_request())
            .await
            .expect("dev booking succeeds");
        assert_eq!(shipment.consignment_id, "DEV-steadfast-sub-1");
        assert_eq!(shipment.status, "created");
        assert_eq!(shipment.tracking_code, None);
        // A separately constructed adapter shares the progression —
        // the factory builds a fresh adapter per call in the API.
        let other = DevCourierAdapter::new("steadfast");
        assert_eq!(other.track(&shipment.consignment_id).await.expect("poll 1").status, "picked_up");
        assert_eq!(adapter.track(&shipment.consignment_id).await.expect("poll 2").status, "in_transit");
        assert_eq!(adapter.track(&shipment.consignment_id).await.expect("poll 3").status, "out_for_delivery");
        assert_eq!(adapter.track(&shipment.consignment_id).await.expect("poll 4").status, "delivered");
        assert_eq!(adapter.track(&shipment.consignment_id).await.expect("poll 5").status, "delivered");
        assert!(adapter.cancel(&shipment.consignment_id).await.is_ok());
    }

    #[tokio::test]
    async fn factory_selects_dev_real_and_rejects_unknown() {
        // Unknown names are an error in every mode.
        let config = CouriersConfig::default();
        assert!(matches!(
            courier_adapter_for("rocket", &config),
            Err(CourierError::UnknownCourier(_))
        ));

        // Dev mode: any known courier is served by the simulator.
        let dev_config = CouriersConfig {
            dev_mode: true,
            ..CouriersConfig::default()
        };
        let dev = courier_adapter_for("pathao", &dev_config).expect("dev adapter");
        assert_eq!(dev.name(), "pathao");
        let shipment = dev
            .create_shipment(&sample_request())
            .await
            .expect("dev booking");
        assert_eq!(shipment.consignment_id, "DEV-pathao-sub-1");

        // No dev, no credentials: the real adapters are selected
        // and honestly report NotConfigured on network operations.
        let steadfast = courier_adapter_for("steadfast", &config).expect("steadfast adapter");
        assert_eq!(steadfast.name(), "steadfast");
        assert!(matches!(
            steadfast.create_shipment(&sample_request()).await,
            Err(CourierError::NotConfigured(_))
        ));
        let pathao = courier_adapter_for("pathao", &config).expect("pathao adapter");
        assert!(matches!(
            pathao.create_shipment(&sample_request()).await,
            Err(CourierError::NotConfigured(_))
        ));

        // Complete credentials: the real Steadfast adapter is chosen
        // (its local price estimate proves the selection without any
        // network call).
        let configured = CouriersConfig {
            steadfast: complete_steadfast(),
            ..CouriersConfig::default()
        };
        let real = courier_adapter_for("steadfast", &configured).expect("real adapter");
        assert_eq!(real.name(), "steadfast");
        let estimate = real
            .price_estimate(&sample_request().parcel)
            .await
            .expect("real adapter estimates locally");
        assert_eq!(estimate, 6_000);
    }

    // -----------------------------------------------------------------
    // Wave 7: Pathao + RedX adapters
    // -----------------------------------------------------------------

    fn complete_pathao() -> PathaoConfig {
        PathaoConfig {
            client_id: Some("client-id".to_string()),
            client_secret: Some("client-secret".to_string()),
            store_id: Some("1234".to_string()),
            ..PathaoConfig::default()
        }
    }

    fn complete_redx() -> RedxConfig {
        RedxConfig {
            api_key: Some("redx-key".to_string()),
            ..RedxConfig::default()
        }
    }

    #[test]
    fn whole_taka_rounding_is_half_up() {
        assert_eq!(poisha_to_whole_taka(469_000), 4_690);
        assert_eq!(poisha_to_whole_taka(150), 2);
        assert_eq!(poisha_to_whole_taka(149), 1);
        assert_eq!(poisha_to_whole_taka(50), 1);
        assert_eq!(poisha_to_whole_taka(49), 0);
        assert_eq!(poisha_to_whole_taka(0), 0);
        assert_eq!(poisha_to_whole_taka(-500), 0);
    }

    #[test]
    fn pathao_and_redx_credential_completeness() {
        // Pathao needs all three: client id, client secret, store id.
        assert!(!pathao_credentials_complete(&PathaoConfig::default()));
        let no_store = PathaoConfig {
            client_id: Some("id".to_string()),
            client_secret: Some("secret".to_string()),
            ..PathaoConfig::default()
        };
        assert!(!pathao_credentials_complete(&no_store));
        assert!(pathao_credentials_complete(&complete_pathao()));
        let blank_store = PathaoConfig {
            store_id: Some("  ".to_string()),
            ..complete_pathao()
        };
        assert!(!pathao_credentials_complete(&blank_store));
        // RedX needs only its API key.
        assert!(!redx_credentials_complete(&RedxConfig::default()));
        assert!(redx_credentials_complete(&complete_redx()));
    }

    #[test]
    fn pathao_token_response_fixtures() {
        // The documented success shape: the token and its lifetime
        // nested under `data`.
        let payload = json!({
            "message": "Token generated successfully.",
            "type": "success",
            "code": 200,
            "data": {
                "token_type": "Bearer",
                "expires_in": 2_592_000,
                "access_token": "token-abc"
            }
        });
        let (token, expires_in) = parse_pathao_token_response(&payload).expect("token parses");
        assert_eq!(token, "token-abc");
        assert_eq!(expires_in, 2_592_000);

        // No expires_in: the one-hour fallback applies.
        let payload = json!({ "data": { "access_token": "token-xyz" } });
        let (token, expires_in) = parse_pathao_token_response(&payload).expect("token parses");
        assert_eq!(token, "token-xyz");
        assert_eq!(expires_in, 3_600);

        // An error payload carries Pathao's message into the error.
        let payload = json!({ "code": 401, "message": "Invalid client credentials." });
        let err = parse_pathao_token_response(&payload).expect_err("error payload is rejected");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
        assert!(err.to_string().contains("Invalid client credentials"), "{err}");
    }

    #[test]
    fn pathao_create_response_fixtures() {
        // Success: code 200, numeric consignment id, and the order
        // status mapped through the domain table (Pending → created).
        let payload = json!({
            "message": "Order created successfully.",
            "type": "success",
            "code": 200,
            "data": {
                "consignment_id": 98765,
                "merchant_order_id": "SUB-sub1",
                "order_status": "Pending",
                "delivery_fee": 60
            }
        });
        let shipment = parse_pathao_create_response("pathao", &payload).expect("create parses");
        assert_eq!(shipment.courier, "pathao");
        assert_eq!(shipment.consignment_id, "98765");
        assert_eq!(shipment.tracking_code, None);
        assert_eq!(shipment.status, "created");

        // Error: a non-200 code carries Pathao's own message.
        let payload = json!({
            "code": 422,
            "message": "The recipient phone field is required."
        });
        let err = parse_pathao_create_response("pathao", &payload)
            .expect_err("error payload is rejected");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
        assert!(err.to_string().contains("recipient phone"), "{err}");

        // Success code but no consignment id: a broken response,
        // not a booking.
        let payload = json!({ "code": 200, "data": { "order_status": "Pending" } });
        let err = parse_pathao_create_response("pathao", &payload)
            .expect_err("missing consignment id is rejected");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
    }

    #[test]
    fn pathao_track_response_fixtures() {
        let payload = json!({
            "code": 200,
            "data": { "consignment_id": 98765, "order_status": "InTransit" }
        });
        let raw = parse_pathao_track_response(&payload).expect("raw status");
        assert_eq!(raw, "InTransit");
        assert_eq!(domain::map_pathao_status(&raw), "in_transit");

        let payload = json!({ "code": 404, "message": "Order not found." });
        let err = parse_pathao_track_response(&payload).expect_err("error payload is rejected");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");

        let payload = json!({ "code": 200, "data": {} });
        assert!(matches!(
            parse_pathao_track_response(&payload),
            Err(CourierError::Gateway(_))
        ));
    }

    #[test]
    fn redx_create_response_fixtures() {
        // Success: a tracking id, which is both the consignment id
        // and the customer-facing tracking code.
        let payload = json!({ "tracking_id": "REDX98765", "message": "Parcel created" });
        let shipment = parse_redx_create_response("redx", &payload).expect("create parses");
        assert_eq!(shipment.courier, "redx");
        assert_eq!(shipment.consignment_id, "REDX98765");
        assert_eq!(shipment.tracking_code.as_deref(), Some("REDX98765"));
        assert_eq!(shipment.status, "created");

        // The id nested under `data` is accepted too.
        let payload = json!({ "data": { "tracking_id": "REDX111" } });
        let shipment = parse_redx_create_response("redx", &payload).expect("nested id parses");
        assert_eq!(shipment.consignment_id, "REDX111");

        // No tracking id: a gateway error carrying RedX's message.
        let payload = json!({ "message": "Invalid area id" });
        let err = parse_redx_create_response("redx", &payload)
            .expect_err("error payload is rejected");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
        assert!(err.to_string().contains("Invalid area id"), "{err}");
    }

    #[test]
    fn redx_track_response_fixtures() {
        // The known field names are tried in order, top level first.
        let payload = json!({ "status": "delivered" });
        let raw = parse_redx_track_response(&payload).expect("raw status");
        assert_eq!(domain::map_redx_status(&raw), "delivered");
        let payload = json!({ "parcel_status": "out-for-delivery" });
        let raw = parse_redx_track_response(&payload).expect("raw status");
        assert_eq!(domain::map_redx_status(&raw), "out_for_delivery");
        let payload = json!({ "data": { "current_status": "picked-up" } });
        let raw = parse_redx_track_response(&payload).expect("raw status");
        assert_eq!(domain::map_redx_status(&raw), "picked_up");
        // Top level wins over the nested object.
        let payload = json!({
            "status": "in-transit",
            "data": { "status": "delivered" }
        });
        assert_eq!(parse_redx_track_response(&payload).expect("raw status"), "in-transit");
        // Nothing recognisable: an honest gateway error.
        let payload = json!({ "something": "else" });
        assert!(matches!(
            parse_redx_track_response(&payload),
            Err(CourierError::Gateway(_))
        ));
    }

    #[tokio::test]
    async fn unconfigured_pathao_and_redx_never_fake_success() {
        let pathao = PathaoAdapter::new(PathaoConfig::default());
        assert!(matches!(
            pathao.create_shipment(&sample_request()).await,
            Err(CourierError::NotConfigured(_))
        ));
        assert!(matches!(
            pathao.track("98765").await,
            Err(CourierError::NotConfigured(_))
        ));
        // The local price estimate still works: it is our own tariff
        // (inside Dhaka, 1 kg), computed by the domain function.
        let estimate = pathao
            .price_estimate(&sample_request().parcel)
            .await
            .expect("local estimate needs no credentials");
        assert_eq!(estimate, 6_000);

        let redx = RedxAdapter::new(RedxConfig::default());
        let mut with_area = sample_request();
        with_area.area_id = Some(42);
        assert!(matches!(
            redx.create_shipment(&with_area).await,
            Err(CourierError::NotConfigured(_))
        ));
        assert!(matches!(
            redx.track("REDX1").await,
            Err(CourierError::NotConfigured(_))
        ));
    }

    #[tokio::test]
    async fn configured_redx_requires_an_area_id() {
        // Credentials complete but no area id in the request: the
        // booking fails loudly BEFORE any network call — RedX area
        // ids are numeric and courier-specific, so there is no
        // honest default.
        let redx = RedxAdapter::new(complete_redx());
        let err = redx
            .create_shipment(&sample_request())
            .await
            .expect_err("no area id, no booking");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
        assert!(err.to_string().contains("area_id"), "{err}");
    }

    #[tokio::test]
    async fn configured_pathao_and_redx_cancel_is_honestly_unsupported() {
        let pathao = PathaoAdapter::new(complete_pathao());
        assert!(matches!(
            pathao.cancel("98765").await,
            Err(CourierError::Unsupported { .. })
        ));
        let redx = RedxAdapter::new(complete_redx());
        assert!(matches!(
            redx.cancel("REDX1").await,
            Err(CourierError::Unsupported { .. })
        ));
    }

    #[tokio::test]
    async fn factory_selects_pathao_and_redx_by_credentials() {
        // Configured: the real adapters are selected (their local
        // price estimates prove the selection without a network
        // call, and an unconfigured create stays NotConfigured).
        let configured = CouriersConfig {
            pathao: complete_pathao(),
            redx: complete_redx(),
            ..CouriersConfig::default()
        };
        let pathao = courier_adapter_for("pathao", &configured).expect("pathao adapter");
        assert_eq!(pathao.name(), "pathao");
        let estimate = pathao
            .price_estimate(&sample_request().parcel)
            .await
            .expect("configured pathao estimates locally");
        assert_eq!(estimate, 6_000);
        let redx = courier_adapter_for("redx", &configured).expect("redx adapter");
        assert_eq!(redx.name(), "redx");
        let estimate = redx
            .price_estimate(&sample_request().parcel)
            .await
            .expect("configured redx estimates locally");
        assert_eq!(estimate, 6_000);

        // Unconfigured: same real adapters, honest NotConfigured.
        let plain = CouriersConfig::default();
        let redx = courier_adapter_for("redx", &plain).expect("redx adapter");
        let mut with_area = sample_request();
        with_area.area_id = Some(42);
        assert!(matches!(
            redx.create_shipment(&with_area).await,
            Err(CourierError::NotConfigured(_))
        ));
    }

    // -----------------------------------------------------------------
    // Wave 11: eCourier + Paperfly adapters
    // -----------------------------------------------------------------

    fn complete_ecourier() -> EcourierConfig {
        EcourierConfig {
            api_key: Some("ecourier-token".to_string()),
            user_id: Some("merchant-42".to_string()),
            ..EcourierConfig::default()
        }
    }

    fn complete_paperfly() -> PaperflyConfig {
        PaperflyConfig {
            username: Some("merchant".to_string()),
            password: Some("paperfly-password".to_string()),
            merchant_id: Some("paperfly-key".to_string()),
            ..PaperflyConfig::default()
        }
    }

    fn complete_paperfly_with_pickup() -> PaperflyConfig {
        PaperflyConfig {
            pickup_name: Some("Platform Warehouse".to_string()),
            pickup_phone: Some("01712345678".to_string()),
            pickup_address: Some("Warehouse 1, Tejgaon, Dhaka".to_string()),
            ..complete_paperfly()
        }
    }

    #[test]
    fn ecourier_and_paperfly_credential_completeness() {
        // eCourier needs BOTH the API token and the merchant user id.
        assert!(!ecourier_credentials_complete(&EcourierConfig::default()));
        let key_only = EcourierConfig {
            api_key: Some("token".to_string()),
            ..EcourierConfig::default()
        };
        assert!(!ecourier_credentials_complete(&key_only));
        assert!(ecourier_credentials_complete(&complete_ecourier()));
        let blank_user = EcourierConfig {
            user_id: Some("  ".to_string()),
            ..complete_ecourier()
        };
        assert!(!ecourier_credentials_complete(&blank_user));
        // Paperfly needs username, password AND the merchant key —
        // the pickup fields are placement config, not credentials,
        // so a pickup-less but credentialed config is complete.
        assert!(!paperfly_credentials_complete(&PaperflyConfig::default()));
        let no_key = PaperflyConfig {
            username: Some("merchant".to_string()),
            password: Some("pw".to_string()),
            ..PaperflyConfig::default()
        };
        assert!(!paperfly_credentials_complete(&no_key));
        assert!(paperfly_credentials_complete(&complete_paperfly()));
    }

    #[test]
    fn ecourier_create_response_fixtures() {
        // Success with the order id as a NUMBER under `order_id`.
        let payload = json!({ "order_id": 555001, "message": "Order created" });
        let shipment =
            parse_ecourier_create_response("ecourier", &payload).expect("create parses");
        assert_eq!(shipment.courier, "ecourier");
        assert_eq!(shipment.consignment_id, "555001");
        assert_eq!(shipment.tracking_code, None);
        assert_eq!(shipment.status, "created");

        // Success with a distinct tracking code: it is kept as the
        // customer-facing tracking code.
        let payload = json!({ "order_id": "EC-9", "tracking": "TRK-9" });
        let shipment =
            parse_ecourier_create_response("ecourier", &payload).expect("create parses");
        assert_eq!(shipment.consignment_id, "EC-9");
        assert_eq!(shipment.tracking_code.as_deref(), Some("TRK-9"));

        // The id nested under `data` (here the short `e_id`) is
        // accepted too.
        let payload = json!({ "data": { "e_id": "E77" } });
        let shipment =
            parse_ecourier_create_response("ecourier", &payload).expect("nested id parses");
        assert_eq!(shipment.consignment_id, "E77");

        // No recognisable id: a gateway error carrying eCourier's
        // message.
        let payload = json!({ "message": "Invalid area id" });
        let err = parse_ecourier_create_response("ecourier", &payload)
            .expect_err("error payload is rejected");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
        assert!(err.to_string().contains("Invalid area id"), "{err}");

        // No id and no message: still an honest gateway error.
        let payload = json!({ "success": false });
        assert!(matches!(
            parse_ecourier_create_response("ecourier", &payload),
            Err(CourierError::Gateway(_))
        ));
    }

    #[test]
    fn ecourier_track_response_fixtures() {
        // The known field names are tried in order, top level first.
        let payload = json!({ "status": "Delivered" });
        let raw = parse_ecourier_track_response(&payload).expect("raw status");
        assert_eq!(domain::map_ecourier_status(&raw), "delivered");
        let payload = json!({ "order_status": "Shipped" });
        let raw = parse_ecourier_track_response(&payload).expect("raw status");
        assert_eq!(domain::map_ecourier_status(&raw), "in_transit");
        let payload = json!({ "data": { "current_status": "Picked" } });
        let raw = parse_ecourier_track_response(&payload).expect("raw status");
        assert_eq!(domain::map_ecourier_status(&raw), "picked_up");
        // Nothing recognisable: an honest gateway error.
        let payload = json!({ "something": "else" });
        assert!(matches!(
            parse_ecourier_track_response(&payload),
            Err(CourierError::Gateway(_))
        ));
    }

    #[test]
    fn paperfly_create_response_fixtures() {
        // Success: a tracking id, which is both the consignment id
        // and the customer-facing tracking code.
        let payload = json!({ "trackingId": "PF98765", "message": "Order placed" });
        let shipment =
            parse_paperfly_create_response("paperfly", &payload).expect("create parses");
        assert_eq!(shipment.courier, "paperfly");
        assert_eq!(shipment.consignment_id, "PF98765");
        assert_eq!(shipment.tracking_code.as_deref(), Some("PF98765"));
        assert_eq!(shipment.status, "created");

        // The snake_case name and the ref number are accepted too,
        // including nested under `data`.
        let payload = json!({ "tracking_id": "PF111" });
        let shipment =
            parse_paperfly_create_response("paperfly", &payload).expect("snake id parses");
        assert_eq!(shipment.consignment_id, "PF111");
        let payload = json!({ "data": { "refNo": "PF222" } });
        let shipment =
            parse_paperfly_create_response("paperfly", &payload).expect("refNo parses");
        assert_eq!(shipment.consignment_id, "PF222");

        // No tracking id: a gateway error carrying Paperfly's
        // message.
        let payload = json!({ "message": "Invalid merchant key" });
        let err = parse_paperfly_create_response("paperfly", &payload)
            .expect_err("error payload is rejected");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
        assert!(err.to_string().contains("Invalid merchant key"), "{err}");
    }

    #[test]
    fn paperfly_track_response_fixtures() {
        let payload = json!({ "status": "Out for Delivery" });
        let raw = parse_paperfly_track_response(&payload).expect("raw status");
        assert_eq!(domain::map_paperfly_status(&raw), "out_for_delivery");
        let payload = json!({ "data": { "order_status": "Order Received" } });
        let raw = parse_paperfly_track_response(&payload).expect("raw status");
        assert_eq!(domain::map_paperfly_status(&raw), "created");
        // Nothing recognisable: an honest gateway error.
        let payload = json!({ "something": "else" });
        assert!(matches!(
            parse_paperfly_track_response(&payload),
            Err(CourierError::Gateway(_))
        ));
    }

    #[tokio::test]
    async fn unconfigured_ecourier_and_paperfly_never_fake_success() {
        let ecourier = EcourierAdapter::new(EcourierConfig::default());
        let mut with_area = sample_request();
        with_area.area_id = Some(42);
        assert!(matches!(
            ecourier.create_shipment(&with_area).await,
            Err(CourierError::NotConfigured(_))
        ));
        assert!(matches!(
            ecourier.track("555001").await,
            Err(CourierError::NotConfigured(_))
        ));
        // The local price estimate still works: it is our own tariff
        // (inside Dhaka, 1 kg), computed by the domain function.
        let estimate = ecourier
            .price_estimate(&sample_request().parcel)
            .await
            .expect("local estimate needs no credentials");
        assert_eq!(estimate, 6_000);

        let paperfly = PaperflyAdapter::new(PaperflyConfig::default());
        assert!(matches!(
            paperfly.create_shipment(&sample_request()).await,
            Err(CourierError::NotConfigured(_))
        ));
        assert!(matches!(
            paperfly.track("PF1").await,
            Err(CourierError::NotConfigured(_))
        ));
        let estimate = paperfly
            .price_estimate(&sample_request().parcel)
            .await
            .expect("local estimate needs no credentials");
        assert_eq!(estimate, 6_000);
    }

    #[tokio::test]
    async fn configured_ecourier_requires_an_area_id() {
        // Credentials complete but no area id in the request: the
        // booking fails loudly BEFORE any network call — eCourier
        // area ids are numeric and courier-specific, so there is no
        // honest default (the same rule as RedX).
        let ecourier = EcourierAdapter::new(complete_ecourier());
        let err = ecourier
            .create_shipment(&sample_request())
            .await
            .expect_err("no area id, no booking");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
        assert!(err.to_string().contains("area_id"), "{err}");
    }

    #[tokio::test]
    async fn configured_paperfly_requires_pickup_config() {
        // Credentials complete but no pickup config: the booking
        // fails loudly BEFORE any network call, naming every missing
        // pickup field — pickup details are platform config and are
        // never invented per order.
        let paperfly = PaperflyAdapter::new(complete_paperfly());
        let err = paperfly
            .create_shipment(&sample_request())
            .await
            .expect_err("no pickup config, no booking");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
        assert!(err.to_string().contains("PAPERFLY_PICKUP_NAME"), "{err}");
        assert!(err.to_string().contains("PAPERFLY_PICKUP_PHONE"), "{err}");
        assert!(err.to_string().contains("PAPERFLY_PICKUP_ADDRESS"), "{err}");

        // A partially filled pickup config names only what is
        // actually missing.
        let partial = PaperflyConfig {
            pickup_name: Some("Platform Warehouse".to_string()),
            ..complete_paperfly()
        };
        let paperfly = PaperflyAdapter::new(partial);
        let err = paperfly
            .create_shipment(&sample_request())
            .await
            .expect_err("partial pickup config, no booking");
        assert!(matches!(err, CourierError::Gateway(_)), "{err}");
        assert!(!err.to_string().contains("PAPERFLY_PICKUP_NAME"), "{err}");
        assert!(err.to_string().contains("PAPERFLY_PICKUP_PHONE"), "{err}");
    }

    #[tokio::test]
    async fn configured_ecourier_and_paperfly_cancel_is_honestly_unsupported() {
        let ecourier = EcourierAdapter::new(complete_ecourier());
        assert!(matches!(
            ecourier.cancel("555001").await,
            Err(CourierError::Unsupported { .. })
        ));
        let paperfly = PaperflyAdapter::new(complete_paperfly_with_pickup());
        assert!(matches!(
            paperfly.cancel("PF1").await,
            Err(CourierError::Unsupported { .. })
        ));
    }

    #[tokio::test]
    async fn factory_selects_ecourier_and_paperfly_by_credentials() {
        // Configured: the real adapters are selected (their local
        // price estimates prove the selection without a network
        // call).
        let configured = CouriersConfig {
            ecourier: complete_ecourier(),
            paperfly: complete_paperfly(),
            ..CouriersConfig::default()
        };
        let ecourier = courier_adapter_for("ecourier", &configured).expect("ecourier adapter");
        assert_eq!(ecourier.name(), "ecourier");
        let estimate = ecourier
            .price_estimate(&sample_request().parcel)
            .await
            .expect("configured ecourier estimates locally");
        assert_eq!(estimate, 6_000);
        let paperfly = courier_adapter_for("paperfly", &configured).expect("paperfly adapter");
        assert_eq!(paperfly.name(), "paperfly");
        let estimate = paperfly
            .price_estimate(&sample_request().parcel)
            .await
            .expect("configured paperfly estimates locally");
        assert_eq!(estimate, 6_000);

        // Unconfigured: same real adapters, honest NotConfigured.
        let plain = CouriersConfig::default();
        let ecourier = courier_adapter_for("ecourier", &plain).expect("ecourier adapter");
        let mut with_area = sample_request();
        with_area.area_id = Some(42);
        assert!(matches!(
            ecourier.create_shipment(&with_area).await,
            Err(CourierError::NotConfigured(_))
        ));
        let paperfly = courier_adapter_for("paperfly", &plain).expect("paperfly adapter");
        assert!(matches!(
            paperfly.create_shipment(&sample_request()).await,
            Err(CourierError::NotConfigured(_))
        ));
    }
}
