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
//! fakes success. Pathao, RedX, eCourier and Paperfly remain typed
//! `NotConfigured` stubs behind the same trait (Pathao and RedX are
//! Wave 7; the manual Phase 2 couriers Sundarban, SA Paribahan,
//! Karatoa and Carrybee join later). [`DevCourierAdapter`] is a
//! clearly-labelled SIMULATOR for local and CI use only, reachable
//! only through [`courier_adapter_for`] when
//! `CouriersConfig::dev_mode` is true. Platform master accounts are
//! the model throughout: sellers need no courier account of their
//! own.

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
/// (the only configured courier in Wave 6) needs an API key and a
/// secret key.
pub fn courier_credentials_complete(config: &CourierConfig) -> bool {
    present(&config.api_key) && present(&config.secret_key)
}

/// Courier configuration for the whole platform: the dev simulator
/// switch, Steadfast's credentials, and the default courier sellers
/// get when they do not pick one.
#[derive(Debug, Clone)]
pub struct CouriersConfig {
    /// Enables the DEV courier simulator. Local/CI only — never true
    /// in a real deployment.
    pub dev_mode: bool,
    pub steadfast: CourierConfig,
    /// Courier name used when a booking does not name one.
    pub default_courier: String,
}

impl Default for CouriersConfig {
    fn default() -> Self {
        CouriersConfig {
            dev_mode: false,
            steadfast: CourierConfig::default(),
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

/// Extract a human-readable message from a Steadfast payload: their
/// `message` field when present, else their `errors` object rendered
/// compactly, else a generic line naming the HTTP status.
fn gateway_message(payload: &Value, http_status: reqwest::StatusCode) -> String {
    if let Some(message) = payload.get("message").and_then(Value::as_str) {
        if !message.trim().is_empty() {
            return message.to_string();
        }
    }
    if let Some(errors) = payload.get("errors") {
        return format!("steadfast rejected the request: {errors}");
    }
    format!("steadfast request failed with HTTP {http_status}")
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
// DEV courier simulator (local/CI only)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Stub adapters (typed NotConfigured — never fake success)
// ---------------------------------------------------------------------------

/// Defines a stub adapter: a named struct whose every operation
/// returns `CourierError::NotConfigured`. No network, no credentials,
/// no fake success. Real adapters replace these one courier at a
/// time (Steadfast in Wave 6, Pathao and RedX in Wave 7).
macro_rules! stub_adapter {
    ($struct_name:ident, $courier_name:literal) => {
        #[derive(Debug, Default)]
        pub struct $struct_name;

        #[async_trait]
        impl CourierAdapter for $struct_name {
            fn name(&self) -> &str {
                $courier_name
            }

            async fn create_shipment(
                &self,
                _req: &ShipmentRequest,
            ) -> Result<Shipment, CourierError> {
                Err(CourierError::NotConfigured($courier_name.to_string()))
            }

            async fn track(
                &self,
                _consignment_id: &str,
            ) -> Result<TrackingUpdate, CourierError> {
                Err(CourierError::NotConfigured($courier_name.to_string()))
            }

            async fn cancel(&self, _consignment_id: &str) -> Result<(), CourierError> {
                Err(CourierError::NotConfigured($courier_name.to_string()))
            }

            async fn price_estimate(&self, _parcel: &Parcel) -> Result<i64, CourierError> {
                Err(CourierError::NotConfigured($courier_name.to_string()))
            }
        }
    };
}

stub_adapter!(PathaoAdapter, "pathao");
stub_adapter!(RedxAdapter, "redx");
stub_adapter!(EcourierAdapter, "ecourier");
stub_adapter!(PaperflyAdapter, "paperfly");

/// THE factory: the one callable place that decides which adapter
/// serves a courier name. An unknown name is an error. In dev mode
/// every known courier is served by the DEV simulator. Otherwise
/// Steadfast gets its real (credential-gated) adapter and the rest
/// get their `NotConfigured` stubs — whose operations honestly say
/// the courier is not available yet.
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
        "pathao" => Ok(Box::new(PathaoAdapter)),
        "redx" => Ok(Box::new(RedxAdapter)),
        "ecourier" => Ok(Box::new(EcourierAdapter)),
        _ => Ok(Box::new(PaperflyAdapter)),
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
    fn stub_adapters_have_expected_names() {
        assert_eq!(PathaoAdapter.name(), "pathao");
        assert_eq!(RedxAdapter.name(), "redx");
        assert_eq!(EcourierAdapter.name(), "ecourier");
        assert_eq!(PaperflyAdapter.name(), "paperfly");
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
    async fn factory_selects_dev_real_stub_and_rejects_unknown() {
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

        // No dev, no credentials: steadfast is its real adapter,
        // honestly unconfigured; pathao is still a stub.
        let steadfast = courier_adapter_for("steadfast", &config).expect("steadfast adapter");
        assert_eq!(steadfast.name(), "steadfast");
        assert!(matches!(
            steadfast.create_shipment(&sample_request()).await,
            Err(CourierError::NotConfigured(_))
        ));
        let pathao = courier_adapter_for("pathao", &config).expect("pathao stub");
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
}
