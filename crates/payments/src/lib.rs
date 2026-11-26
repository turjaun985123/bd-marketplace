//! Unified payment adapter for Bangladesh.
//!
//! One trait, one method per operation. Wave 1: COD has a trivial real
//! implementation that only records payment intent (cash is collected by
//! the courier at delivery — this code moves no money). bKash, Nagad,
//! SSLCommerz and ShurjoPay are typed stubs returning `NotConfigured`;
//! they never fake success, never make network calls and hold no
//! credentials. No Stripe: Bangladesh-first, BDT only.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Payment methods supported by the platform (BDT only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaymentMethod {
    Cod,
    Bkash,
    Nagad,
    SslCommerz,
    ShurjoPay,
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
}

#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    #[error("payment method '{0}' is not configured (no merchant credentials)")]
    NotConfigured(String),
    #[error("payment method '{method}' does not support operation '{operation}'")]
    Unsupported { method: String, operation: String },
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

/// Cash on Delivery: the only Wave 1 "real" adapter, and it only records
/// intent. The courier collects the cash; reconciliation happens in a
/// later wave via the courier crate.
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

/// Defines a stub adapter whose every operation returns
/// `PaymentError::NotConfigured`. No network, no credentials, no fake success.
macro_rules! stub_adapter {
    ($struct_name:ident, $method_variant:expr, $method_name:literal) => {
        #[derive(Debug, Default)]
        pub struct $struct_name;

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
                Err(PaymentError::NotConfigured($method_name.to_string()))
            }

            async fn verify(&self, _reference: &str) -> Result<PaymentStatus, PaymentError> {
                Err(PaymentError::NotConfigured($method_name.to_string()))
            }

            async fn refund(
                &self,
                _reference: &str,
                _amount_poisha: i64,
            ) -> Result<(), PaymentError> {
                Err(PaymentError::NotConfigured($method_name.to_string()))
            }

            async fn payout_status(
                &self,
                _payout_id: &str,
            ) -> Result<PaymentStatus, PaymentError> {
                Err(PaymentError::NotConfigured($method_name.to_string()))
            }
        }
    };
}

stub_adapter!(BkashAdapter, PaymentMethod::Bkash, "bkash");
stub_adapter!(NagadAdapter, PaymentMethod::Nagad, "nagad");
stub_adapter!(SslCommerzAdapter, PaymentMethod::SslCommerz, "sslcommerz");
stub_adapter!(ShurjoPayAdapter, PaymentMethod::ShurjoPay, "shurjopay");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_adapters_report_their_method() {
        assert_eq!(BkashAdapter.method(), PaymentMethod::Bkash);
        assert_eq!(NagadAdapter.method(), PaymentMethod::Nagad);
        assert_eq!(SslCommerzAdapter.method(), PaymentMethod::SslCommerz);
        assert_eq!(ShurjoPayAdapter.method(), PaymentMethod::ShurjoPay);
        assert_eq!(CodAdapter.method(), PaymentMethod::Cod);
    }
}
