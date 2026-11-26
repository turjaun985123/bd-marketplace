//! Unified courier adapter for Bangladeshi couriers.
//!
//! One trait, one method per operation — every courier is called through
//! exactly the same individually callable functions. Wave 1 ships typed
//! stubs for the Tier 1 couriers (Steadfast, Pathao, RedX): they return a
//! clear `NotConfigured` error. They never fake success, never make network
//! calls and hold no credentials. Tier 1b (eCourier, Paperfly) and the
//! manual Phase 2 couriers (Sundarban, SA Paribahan, Karatoa, Carrybee)
//! are added behind this same trait in later waves.

use async_trait::async_trait;
use domain::DeliveryZone;
use serde::{Deserialize, Serialize};

/// Common shipment status set. Every courier's raw statuses are mapped
/// onto this enum by its adapter, so callers never learn courier-specific
/// status strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShipmentStatus {
    PendingPickup,
    PickedUp,
    InTransit,
    Hub,
    OutForDelivery,
    Delivered,
    Returned,
    Cancelled,
    Failed,
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
    /// Platform order / sub-order id — also the idempotency key.
    pub order_id: String,
    pub store_id: String,
    pub recipient_name: String,
    pub recipient_phone: String,
    pub recipient_address: String,
    pub parcel: Parcel,
}

/// A booked shipment, as returned by an adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shipment {
    pub courier: String,
    /// Courier-side consignment / tracking id.
    pub consignment_id: String,
    pub status: ShipmentStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum CourierError {
    /// The adapter exists but has no merchant credentials configured yet.
    #[error("courier '{0}' is not configured (no merchant credentials)")]
    NotConfigured(String),
    /// This courier does not support the requested operation.
    #[error("courier '{courier}' does not support operation '{operation}'")]
    Unsupported { courier: String, operation: String },
}

/// One adapter per courier, one method per operation.
#[async_trait]
pub trait CourierAdapter: Send + Sync {
    /// Human/machine name of the courier, e.g. "steadfast".
    fn name(&self) -> &str;

    async fn create_shipment(&self, req: &ShipmentRequest) -> Result<Shipment, CourierError>;

    async fn track(&self, consignment_id: &str) -> Result<ShipmentStatus, CourierError>;

    async fn cancel(&self, consignment_id: &str) -> Result<(), CourierError>;

    /// Delivery charge estimate in poisha for a parcel.
    async fn price_estimate(&self, parcel: &Parcel) -> Result<i64, CourierError>;
}

/// Defines a stub adapter: a named struct whose every operation returns
/// `CourierError::NotConfigured`. No network, no credentials, no fake success.
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
            ) -> Result<ShipmentStatus, CourierError> {
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

stub_adapter!(SteadfastAdapter, "steadfast");
stub_adapter!(PathaoAdapter, "pathao");
stub_adapter!(RedxAdapter, "redx");

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> ShipmentRequest {
        ShipmentRequest {
            order_id: "sub-1".to_string(),
            store_id: "store-a".to_string(),
            recipient_name: "Test Buyer".to_string(),
            recipient_phone: "01XXXXXXXXX".to_string(),
            recipient_address: "Dhaka".to_string(),
            parcel: Parcel {
                weight_kg: 1,
                cod_amount_poisha: 0,
                zone: DeliveryZone::InsideDhaka,
            },
        }
    }

    #[test]
    fn stub_adapters_have_expected_names() {
        assert_eq!(SteadfastAdapter.name(), "steadfast");
        assert_eq!(PathaoAdapter.name(), "pathao");
        assert_eq!(RedxAdapter.name(), "redx");
        let _ = sample_request();
        let err = CourierError::NotConfigured("steadfast".to_string());
        assert!(err.to_string().contains("not configured"));
    }
}
