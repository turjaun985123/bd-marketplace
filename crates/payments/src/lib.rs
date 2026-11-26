//! Unified payment adapter for Bangladesh.
//!
//! One trait, one method per operation. COD has a trivial real
//! implementation that only records payment intent (cash is collected
//! by the courier at delivery — this code moves no money). The real
//! gateway adapters (bKash, Nagad, SSLCommerz, ShurjoPay) are built
//! from merchant credential config: with incomplete credentials every
//! operation returns `NotConfigured`, and even with credentials the
//! live gateway calls are not implemented yet (`NotImplemented`) —
//! they never fake success and never make network calls. No Stripe:
//! Bangladesh-first, BDT only.
//!
//! Wave 5 adds [`DevGatewayAdapter`]: a clearly-labelled SIMULATOR for
//! local and CI use only. It exists so the checkout, webhook and retry
//! flows can be exercised end to end before any merchant account
//! exists. It is reachable only through [`payment_adapter_for`] when
//! `PaymentsConfig::dev_mode` is true, its references are prefixed
//! `DEV-`, and production deployments must never enable dev mode.

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Payment methods supported by the platform (BDT only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaymentMethod {
    Cod,
    Bkash,
    Nagad,
    SslCommerz,
    ShurjoPay,
}

impl PaymentMethod {
    /// The snake_case string stored in the database and used by the
    /// checkout API payload (`cod`, `bkash`, `nagad`, `sslcommerz`,
    /// `shurjopay`).
    pub fn as_str(&self) -> &'static str {
        match self {
            PaymentMethod::Cod => "cod",
            PaymentMethod::Bkash => "bkash",
            PaymentMethod::Nagad => "nagad",
            PaymentMethod::SslCommerz => "sslcommerz",
            PaymentMethod::ShurjoPay => "shurjopay",
        }
    }

    /// Is this an online gateway method (everything except COD)?
    pub fn is_online(&self) -> bool {
        !matches!(self, PaymentMethod::Cod)
    }
}

/// Parse a payment method from its snake_case string (the inverse of
/// [`PaymentMethod::as_str`]). Returns `None` for anything else. This
/// is the single string-to-enum mapping for payment methods.
pub fn parse_payment_method(value: &str) -> Option<PaymentMethod> {
    match value {
        "cod" => Some(PaymentMethod::Cod),
        "bkash" => Some(PaymentMethod::Bkash),
        "nagad" => Some(PaymentMethod::Nagad),
        "sslcommerz" => Some(PaymentMethod::SslCommerz),
        "shurjopay" => Some(PaymentMethod::ShurjoPay),
        _ => None,
    }
}

/// Lifecycle of a payment, in platform terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaymentStatus {
    /// Intent recorded; nothing collected yet (e.g. COD before delivery).
    Recorded,
    Pending,
    Paid,
    Failed,
    Refunded,
}

/// A recorded payment attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentIntent {
    pub method: PaymentMethod,
    /// Platform order id this payment belongs to.
    pub order_id: String,
    /// Amount in poisha.
    pub amount_poisha: i64,
    /// Provider reference, or a platform reference for COD.
    pub reference: String,
    pub status: PaymentStatus,
    /// Where the buyer would be sent to pay. Only the DEV simulator
    /// produces one in Wave 5 (a local `/dev-pay/<ref>` path); real
    /// gateways return `None` until their live integration lands.
    pub redirect_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    #[error("payment method '{0}' is not configured (no merchant credentials)")]
    NotConfigured(String),
    #[error("payment method '{0}' live gateway integration is not implemented yet")]
    NotImplemented(String),
    #[error("payment method '{method}' does not support operation '{operation}'")]
    Unsupported { method: String, operation: String },
}

/// Merchant credentials for one gateway, as plain config values. The
/// API crate fills these from environment variables; this crate never
/// reads the environment itself. Which fields a gateway needs is
/// defined by [`gateway_credentials_complete`].
#[derive(Debug, Clone, Default)]
pub struct GatewayConfig {
    pub merchant_id: Option<String>,
    pub api_key: Option<String>,
    pub secret: Option<String>,
}

fn present(value: &Option<String>) -> bool {
    value.as_ref().is_some_and(|v| !v.trim().is_empty())
}

/// Are the credentials a gateway needs all present? bKash needs an app
/// key and secret; Nagad needs a merchant id and API key; SSLCommerz
/// needs a store id (merchant id) and store password (secret);
/// ShurjoPay needs a username (merchant id) and password (secret).
/// COD needs nothing and is always complete.
pub fn gateway_credentials_complete(method: PaymentMethod, config: &GatewayConfig) -> bool {
    match method {
        PaymentMethod::Cod => true,
        PaymentMethod::Bkash => present(&config.api_key) && present(&config.secret),
        PaymentMethod::Nagad => present(&config.merchant_id) && present(&config.api_key),
        PaymentMethod::SslCommerz => present(&config.merchant_id) && present(&config.secret),
        PaymentMethod::ShurjoPay => present(&config.merchant_id) && present(&config.secret),
    }
}

/// Payment configuration for the whole platform: the dev simulator
/// switch and secret, plus one credential set per gateway.
#[derive(Debug, Clone, Default)]
pub struct PaymentsConfig {
    /// Enables the DEV gateway simulator. Local/CI only — never true
    /// in a real deployment.
    pub dev_mode: bool,
    /// HMAC secret for dev callback signatures.
    pub dev_secret: String,
    pub bkash: GatewayConfig,
    pub nagad: GatewayConfig,
    pub sslcommerz: GatewayConfig,
    pub shurjopay: GatewayConfig,
}

impl PaymentsConfig {
    /// The credential set for an online method (`None` for COD, which
    /// has no gateway credentials).
    pub fn gateway_config(&self, method: PaymentMethod) -> Option<&GatewayConfig> {
        match method {
            PaymentMethod::Cod => None,
            PaymentMethod::Bkash => Some(&self.bkash),
            PaymentMethod::Nagad => Some(&self.nagad),
            PaymentMethod::SslCommerz => Some(&self.sslcommerz),
            PaymentMethod::ShurjoPay => Some(&self.shurjopay),
        }
    }

    /// Are this method's merchant credentials all present?
    pub fn credentials_present(&self, method: PaymentMethod) -> bool {
        match self.gateway_config(method) {
            Some(config) => gateway_credentials_complete(method, config),
            None => true,
        }
    }
}

/// One adapter per payment method, one method per operation.
#[async_trait]
pub trait PaymentAdapter: Send + Sync {
    fn method(&self) -> PaymentMethod;

    async fn initiate(
        &self,
        order_id: &str,
        amount_poisha: i64,
    ) -> Result<PaymentIntent, PaymentError>;

    async fn verify(&self, reference: &str) -> Result<PaymentStatus, PaymentError>;

    async fn refund(&self, reference: &str, amount_poisha: i64) -> Result<(), PaymentError>;

    async fn payout_status(&self, payout_id: &str) -> Result<PaymentStatus, PaymentError>;
}

/// Cash on Delivery: the only "real" adapter since Wave 1, and it only
/// records intent. The courier collects the cash; reconciliation
/// happens via the courier crate in a later wave.
#[derive(Debug, Default)]
pub struct CodAdapter;

#[async_trait]
impl PaymentAdapter for CodAdapter {
    fn method(&self) -> PaymentMethod {
        PaymentMethod::Cod
    }

    async fn initiate(
        &self,
        order_id: &str,
        amount_poisha: i64,
    ) -> Result<PaymentIntent, PaymentError> {
        Ok(PaymentIntent {
            method: PaymentMethod::Cod,
            order_id: order_id.to_string(),
            amount_poisha,
            reference: format!("COD-{order_id}"),
            status: PaymentStatus::Recorded,
            redirect_url: None,
        })
    }

    async fn verify(&self, _reference: &str) -> Result<PaymentStatus, PaymentError> {
        // COD is verified by courier cash reconciliation, not here.
        Ok(PaymentStatus::Recorded)
    }

    async fn refund(&self, _reference: &str, _amount_poisha: i64) -> Result<(), PaymentError> {
        Err(PaymentError::Unsupported {
            method: "cod".to_string(),
            operation: "refund".to_string(),
        })
    }

    async fn payout_status(&self, _payout_id: &str) -> Result<PaymentStatus, PaymentError> {
        Err(PaymentError::Unsupported {
            method: "cod".to_string(),
            operation: "payout_status".to_string(),
        })
    }
}

/// Shared body of the real gateway adapters: without complete
/// credentials every operation is `NotConfigured`; with credentials
/// the live gateway calls are still `NotImplemented` (they land once
/// merchant accounts exist and the gateways' integration docs are
/// followed — until then nothing here pretends to move money).
macro_rules! gateway_adapter {
    ($struct_name:ident, $method_variant:expr, $method_name:literal) => {
        #[derive(Debug, Default)]
        pub struct $struct_name {
            pub config: GatewayConfig,
        }

        impl $struct_name {
            pub fn new(config: GatewayConfig) -> Self {
                $struct_name { config }
            }

            fn check(&self) -> Result<(), PaymentError> {
                if gateway_credentials_complete($method_variant, &self.config) {
                    Err(PaymentError::NotImplemented($method_name.to_string()))
                } else {
                    Err(PaymentError::NotConfigured($method_name.to_string()))
                }
            }
        }

        #[async_trait]
        impl PaymentAdapter for $struct_name {
            fn method(&self) -> PaymentMethod {
                $method_variant
            }

            async fn initiate(
                &self,
                _order_id: &str,
                _amount_poisha: i64,
            ) -> Result<PaymentIntent, PaymentError> {
                self.check()?;
                unreachable!("check() always returns an error")
            }

            async fn verify(&self, _reference: &str) -> Result<PaymentStatus, PaymentError> {
                self.check()?;
                unreachable!("check() always returns an error")
            }

            async fn refund(
                &self,
                _reference: &str,
                _amount_poisha: i64,
            ) -> Result<(), PaymentError> {
                self.check()?;
                Ok(())
            }

            async fn payout_status(
                &self,
                _payout_id: &str,
            ) -> Result<PaymentStatus, PaymentError> {
                self.check()?;
                unreachable!("check() always returns an error")
            }
        }
    };
}

gateway_adapter!(BkashAdapter, PaymentMethod::Bkash, "bkash");
gateway_adapter!(NagadAdapter, PaymentMethod::Nagad, "nagad");
gateway_adapter!(SslCommerzAdapter, PaymentMethod::SslCommerz, "sslcommerz");
gateway_adapter!(ShurjoPayAdapter, PaymentMethod::ShurjoPay, "shurjopay");

/// DEV-ONLY gateway simulator. This adapter does NOT talk to any real
/// gateway and never moves money: `initiate` mints a deterministic
/// `DEV-<method>-<order_id>` reference and a local `/dev-pay/<ref>`
/// redirect path, so the checkout, webhook and retry flows can be
/// exercised in development and CI. It must only ever be constructed
/// through [`payment_adapter_for`] when dev mode is explicitly on.
#[derive(Debug, Clone)]
pub struct DevGatewayAdapter {
    method: PaymentMethod,
    secret: String,
}

impl DevGatewayAdapter {
    pub fn new(method: PaymentMethod, secret: String) -> Self {
        DevGatewayAdapter { method, secret }
    }

    /// The dev secret this adapter signs and verifies callbacks with.
    pub fn secret(&self) -> &str {
        &self.secret
    }
}

#[async_trait]
impl PaymentAdapter for DevGatewayAdapter {
    fn method(&self) -> PaymentMethod {
        self.method
    }

    async fn initiate(
        &self,
        order_id: &str,
        amount_poisha: i64,
    ) -> Result<PaymentIntent, PaymentError> {
        let reference = format!("DEV-{}-{}", self.method.as_str(), order_id);
        Ok(PaymentIntent {
            method: self.method,
            order_id: order_id.to_string(),
            amount_poisha,
            redirect_url: Some(format!("/dev-pay/{reference}")),
            reference,
            status: PaymentStatus::Pending,
        })
    }

    async fn verify(&self, reference: &str) -> Result<PaymentStatus, PaymentError> {
        // A dev reference is all the simulator tracks; settlement only
        // happens through a signed callback (see the API webhook),
        // never through this call.
        let prefix = format!("DEV-{}-", self.method.as_str());
        if reference.starts_with(&prefix) {
            Ok(PaymentStatus::Pending)
        } else {
            Err(PaymentError::Unsupported {
                method: self.method.as_str().to_string(),
                operation: "verify (unknown dev reference)".to_string(),
            })
        }
    }

    async fn refund(&self, _reference: &str, _amount_poisha: i64) -> Result<(), PaymentError> {
        Err(PaymentError::Unsupported {
            method: self.method.as_str().to_string(),
            operation: "refund (dev simulator)".to_string(),
        })
    }

    async fn payout_status(&self, _payout_id: &str) -> Result<PaymentStatus, PaymentError> {
        Err(PaymentError::Unsupported {
            method: self.method.as_str().to_string(),
            operation: "payout_status (dev simulator)".to_string(),
        })
    }
}

/// Sign a DEV gateway callback: HMAC-SHA256 (hex) over
/// `"<payment_ref>:<amount_poisha>:<status>"`. The API webhook and the
/// dev-simulate helper share this ONE implementation with the tests,
/// so the scheme can never drift between signer and verifier.
pub fn sign_dev_callback(
    secret: &str,
    payment_ref: &str,
    amount_poisha: i64,
    status: &str,
) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(format!("{payment_ref}:{amount_poisha}:{status}").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Verify a DEV gateway callback signature produced by
/// [`sign_dev_callback`]. Malformed hex or a wrong signature both
/// return false; the MAC comparison itself is constant-time.
pub fn verify_dev_callback(
    secret: &str,
    payment_ref: &str,
    amount_poisha: i64,
    status: &str,
    signature_hex: &str,
) -> bool {
    let Ok(signature) = hex::decode(signature_hex) else {
        return false;
    };
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(format!("{payment_ref}:{amount_poisha}:{status}").as_bytes());
    mac.verify_slice(&signature).is_ok()
}

/// Is this reference one the DEV simulator minted (`DEV-` prefix)?
pub fn is_dev_reference(reference: &str) -> bool {
    reference.starts_with("DEV-")
}

/// Build the real (non-dev) adapter for an online method from its
/// credential config.
fn real_adapter(method: PaymentMethod, config: &GatewayConfig) -> Box<dyn PaymentAdapter> {
    match method {
        PaymentMethod::Bkash => Box::new(BkashAdapter::new(config.clone())),
        PaymentMethod::Nagad => Box::new(NagadAdapter::new(config.clone())),
        PaymentMethod::SslCommerz => Box::new(SslCommerzAdapter::new(config.clone())),
        PaymentMethod::ShurjoPay => Box::new(ShurjoPayAdapter::new(config.clone())),
        PaymentMethod::Cod => Box::new(CodAdapter),
    }
}

/// THE factory: the one callable place that decides which adapter
/// serves a payment method. COD always gets [`CodAdapter`]. An online
/// method with complete merchant credentials gets its real adapter;
/// without credentials it gets the DEV simulator when dev mode is on,
/// and its real (unconfigured) adapter otherwise — whose operations
/// honestly return `NotConfigured`.
pub fn payment_adapter_for(
    method: PaymentMethod,
    config: &PaymentsConfig,
) -> Box<dyn PaymentAdapter> {
    if !method.is_online() {
        return Box::new(CodAdapter);
    }
    let gateway_config = config
        .gateway_config(method)
        .expect("online methods have a gateway config");
    if gateway_credentials_complete(method, gateway_config) {
        real_adapter(method, gateway_config)
    } else if config.dev_mode {
        Box::new(DevGatewayAdapter::new(
            method,
            config.dev_secret.clone(),
        ))
    } else {
        real_adapter(method, gateway_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_bkash() -> GatewayConfig {
        GatewayConfig {
            merchant_id: None,
            api_key: Some("app-key".to_string()),
            secret: Some("app-secret".to_string()),
        }
    }

    #[test]
    fn method_string_round_trip() {
        for method in [
            PaymentMethod::Cod,
            PaymentMethod::Bkash,
            PaymentMethod::Nagad,
            PaymentMethod::SslCommerz,
            PaymentMethod::ShurjoPay,
        ] {
            assert_eq!(parse_payment_method(method.as_str()), Some(method));
        }
        assert_eq!(parse_payment_method("stripe"), None);
        assert_eq!(parse_payment_method("COD"), None);
        assert_eq!(parse_payment_method(""), None);
    }

    #[test]
    fn adapters_report_their_method() {
        assert_eq!(BkashAdapter::default().method(), PaymentMethod::Bkash);
        assert_eq!(NagadAdapter::default().method(), PaymentMethod::Nagad);
        assert_eq!(
            SslCommerzAdapter::default().method(),
            PaymentMethod::SslCommerz
        );
        assert_eq!(
            ShurjoPayAdapter::default().method(),
            PaymentMethod::ShurjoPay
        );
        assert_eq!(CodAdapter.method(), PaymentMethod::Cod);
    }

    #[test]
    fn credential_completeness_per_gateway() {
        assert!(!gateway_credentials_complete(
            PaymentMethod::Bkash,
            &GatewayConfig::default()
        ));
        assert!(gateway_credentials_complete(
            PaymentMethod::Bkash,
            &complete_bkash()
        ));
        // bKash ignores merchant_id; Nagad requires it.
        let nagad = GatewayConfig {
            merchant_id: Some("M-1".to_string()),
            api_key: Some("k".to_string()),
            secret: None,
        };
        assert!(gateway_credentials_complete(PaymentMethod::Nagad, &nagad));
        assert!(!gateway_credentials_complete(
            PaymentMethod::SslCommerz,
            &nagad
        ));
        // Empty strings do not count as present.
        let blank = GatewayConfig {
            merchant_id: Some("  ".to_string()),
            api_key: Some(String::new()),
            secret: Some("s".to_string()),
        };
        assert!(!gateway_credentials_complete(
            PaymentMethod::ShurjoPay,
            &blank
        ));
    }

    #[tokio::test]
    async fn unconfigured_adapter_never_fakes_success() {
        let adapter = BkashAdapter::default();
        let err = adapter
            .initiate("order-1", 100_00)
            .await
            .expect_err("no credentials, no success");
        assert!(matches!(err, PaymentError::NotConfigured(_)), "{err}");
    }

    #[tokio::test]
    async fn configured_adapter_reports_not_implemented() {
        let adapter = BkashAdapter::new(complete_bkash());
        let err = adapter
            .initiate("order-1", 100_00)
            .await
            .expect_err("live calls are not implemented yet");
        assert!(matches!(err, PaymentError::NotImplemented(_)), "{err}");
    }

    #[tokio::test]
    async fn dev_adapter_mints_dev_reference_and_redirect() {
        let adapter = DevGatewayAdapter::new(PaymentMethod::Bkash, "s3cret".to_string());
        let intent = adapter
            .initiate("order-42", 469_000)
            .await
            .expect("dev initiate succeeds");
        assert_eq!(intent.reference, "DEV-bkash-order-42");
        assert_eq!(intent.status, PaymentStatus::Pending);
        assert_eq!(
            intent.redirect_url.as_deref(),
            Some("/dev-pay/DEV-bkash-order-42")
        );
        assert!(is_dev_reference(&intent.reference));
        assert!(!is_dev_reference("COD-order-42"));
    }

    #[test]
    fn dev_callback_signature_round_trip() {
        let signature = sign_dev_callback("s3cret", "DEV-bkash-order-42", 469_000, "success");
        assert!(verify_dev_callback(
            "s3cret",
            "DEV-bkash-order-42",
            469_000,
            "success",
            &signature
        ));
        // Any change to secret, ref, amount, status or signature fails.
        assert!(!verify_dev_callback(
            "wrong",
            "DEV-bkash-order-42",
            469_000,
            "success",
            &signature
        ));
        assert!(!verify_dev_callback(
            "s3cret",
            "DEV-bkash-order-43",
            469_000,
            "success",
            &signature
        ));
        assert!(!verify_dev_callback(
            "s3cret",
            "DEV-bkash-order-42",
            469_001,
            "success",
            &signature
        ));
        assert!(!verify_dev_callback(
            "s3cret",
            "DEV-bkash-order-42",
            469_000,
            "failed",
            &signature
        ));
        assert!(!verify_dev_callback(
            "s3cret",
            "DEV-bkash-order-42",
            469_000,
            "success",
            "deadbeef"
        ));
        assert!(!verify_dev_callback(
            "s3cret",
            "DEV-bkash-order-42",
            469_000,
            "success",
            "not-hex!"
        ));
    }

    #[tokio::test]
    async fn factory_picks_cod_dev_and_real_adapters() {
        let mut config = PaymentsConfig::default();
        // COD is always the COD adapter.
        let cod = payment_adapter_for(PaymentMethod::Cod, &config);
        assert_eq!(cod.method(), PaymentMethod::Cod);
        assert!(cod.initiate("o1", 100).await.is_ok());

        // No credentials, dev off: real adapter, honestly unconfigured.
        let real = payment_adapter_for(PaymentMethod::Nagad, &config);
        assert!(matches!(
            real.initiate("o1", 100).await,
            Err(PaymentError::NotConfigured(_))
        ));

        // Dev mode on: the simulator serves unconfigured methods.
        config.dev_mode = true;
        config.dev_secret = "s3cret".to_string();
        let dev = payment_adapter_for(PaymentMethod::Nagad, &config);
        let intent = dev.initiate("o1", 100).await.expect("dev initiate");
        assert_eq!(intent.reference, "DEV-nagad-o1");

        // Complete credentials beat dev mode: the real adapter is used.
        config.nagad = GatewayConfig {
            merchant_id: Some("M-1".to_string()),
            api_key: Some("k".to_string()),
            secret: None,
        };
        let configured = payment_adapter_for(PaymentMethod::Nagad, &config);
        assert!(matches!(
            configured.initiate("o1", 100).await,
            Err(PaymentError::NotImplemented(_))
        ));
    }
}
