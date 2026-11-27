//! Pure domain functions for the Bangladesh-first marketplace.
//!
//! Money choice: all money is integer **poisha** (`i64`), the minor unit of
//! BDT (1 BDT = 100 poisha). No floats are ever used for money, so pricing,
//! commission and payout arithmetic is exact. Product weights are stored in
//! grams (`weight_grams`); delivery billing converts a parcel's total grams
//! to whole kilograms with [`weight_kg_from_grams`] because couriers bill
//! per started kilogram.
//!
//! Governing principle — Atomic, Individually Callable Functions: every
//! capability here is a single-responsibility public function with typed
//! inputs/outputs and no DB/network side effects. API handlers, the worker,
//! CLI tools and tests all call these same functions; logic is never
//! re-implemented per caller.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Delivery zones used by Bangladeshi couriers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeliveryZone {
    InsideDhaka,
    DhakaSuburb,
    OutsideDhaka,
}

/// One line in a buyer's cart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CartItem {
    pub product_id: String,
    pub store_id: String,
    pub quantity: u32,
    /// Unit price in poisha.
    pub unit_price_poisha: i64,
    /// Weight of ONE unit in grams (the product's `weight_grams`).
    /// Defaults to [`DEFAULT_WEIGHT_GRAMS`] when a payload omits it, so
    /// older clients keep working; checkout copies the real product
    /// weight here and order totals sum it per seller group.
    #[serde(default = "default_unit_weight_grams")]
    pub unit_weight_grams: i64,
    /// The product variant this line buys (Wave 33), as the
    /// variant's id string; `None` for a variant-less line.
    /// Defaults to `None` when a payload omits it, so older
    /// clients keep working. The variant is part of the line's
    /// identity: two lines for the same product with different
    /// variants (or different final prices) stay separate lines
    /// through the split and the totals.
    #[serde(default)]
    pub variant_id: Option<String>,
}

fn default_unit_weight_grams() -> i64 {
    DEFAULT_WEIGHT_GRAMS
}

impl CartItem {
    /// Line total in poisha.
    ///
    /// Wave 39 (security audit CR-12): the multiplication is
    /// CHECKED — a unit price near `i64::MAX` times a quantity
    /// returns an error instead of wrapping into a small or
    /// negative total (in release builds the wrap was silent).
    pub fn line_total_poisha(&self) -> Result<i64, String> {
        self.unit_price_poisha
            .checked_mul(i64::from(self.quantity))
            .ok_or_else(|| {
                "লাইন টোটাল হিসাব সীমা ছাড়িয়ে গেছে — line total overflows the money range"
                    .to_string()
            })
    }
}

/// All items belonging to one seller, split out of a unified cart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SellerSubOrder {
    pub store_id: String,
    pub items: Vec<CartItem>,
    /// Goods subtotal for this seller, in poisha (delivery not included).
    pub subtotal_poisha: i64,
}

/// Result of a commission calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommissionQuote {
    /// Platform commission in poisha.
    pub commission_poisha: i64,
    /// Amount the seller receives in poisha (subtotal - commission).
    pub seller_payout_poisha: i64,
}

/// Base delivery charge in poisha for the first kilogram, per zone.
fn base_charge_poisha(zone: DeliveryZone) -> i64 {
    match zone {
        DeliveryZone::InsideDhaka => 6_000,   // 60 BDT
        DeliveryZone::DhakaSuburb => 10_000,  // 100 BDT
        DeliveryZone::OutsideDhaka => 12_000, // 120 BDT
    }
}

/// Extra charge in poisha for each started kilogram beyond the first.
fn extra_kg_charge_poisha(zone: DeliveryZone) -> i64 {
    match zone {
        DeliveryZone::InsideDhaka => 2_000,  // 20 BDT
        DeliveryZone::DhakaSuburb => 3_000,  // 30 BDT
        DeliveryZone::OutsideDhaka => 4_000, // 40 BDT
    }
}

/// Calculate the delivery charge (in poisha) for one parcel.
///
/// Wave 1 rule: zone base charge for the first kg + a per-kg surcharge for
/// each additional kg. `cod_amount_poisha` is accepted (and validated) now
/// because courier COD fees depend on it, but no COD percentage is baked in:
/// the verified COD rate differs per courier and must be confirmed in
/// writing at merchant onboarding (see the MVP plan), so Wave 1 charges
/// no COD fee rather than inventing one.
pub fn calculate_delivery_charge(
    zone: DeliveryZone,
    weight_kg: u32,
    cod_amount_poisha: i64,
) -> Result<i64, String> {
    // Wave 39 (security audit CR-11): a negative COD amount is a
    // caller error returned as a value — the previous `assert!`
    // made it a panic (a denial-of-service on any path that could
    // feed it attacker-influenced input).
    if cod_amount_poisha < 0 {
        return Err(
            "COD টাকা ঋণাত্মক হতে পারে না — COD amount cannot be negative".to_string(),
        );
    }
    let billable_weight = weight_kg.max(1);
    let extra_kg = i64::from(billable_weight - 1);
    let extra_charge = extra_kg
        .checked_mul(extra_kg_charge_poisha(zone))
        .ok_or_else(|| {
            "ডেলিভারি চার্জ হিসাব সীমা ছাড়িয়ে গেছে — delivery charge overflows the money range"
                .to_string()
        })?;
    base_charge_poisha(zone).checked_add(extra_charge).ok_or_else(|| {
        "ডেলিভারি চার্জ হিসাব সীমা ছাড়িয়ে গেছে — delivery charge overflows the money range"
            .to_string()
    })
}

/// Split a unified cart into one sub-order per seller.
///
/// First-appearance order of sellers is preserved. This is the single
/// function used by checkout, order splitting and manual seller orders.
///
/// Wave 39 (security audit CR-12): subtotals accumulate with
/// CHECKED arithmetic — an overflowing line total or subtotal is
/// an error, never a wrapped figure.
pub fn split_cart_by_seller(items: &[CartItem]) -> Result<Vec<SellerSubOrder>, String> {
    let mut index_by_store: HashMap<String, usize> = HashMap::new();
    let mut sub_orders: Vec<SellerSubOrder> = Vec::new();

    for item in items {
        let idx = match index_by_store.get(&item.store_id) {
            Some(&i) => i,
            None => {
                let i = sub_orders.len();
                index_by_store.insert(item.store_id.clone(), i);
                sub_orders.push(SellerSubOrder {
                    store_id: item.store_id.clone(),
                    items: Vec::new(),
                    subtotal_poisha: 0,
                });
                i
            }
        };
        let line_total = item.line_total_poisha()?;
        sub_orders[idx].subtotal_poisha = sub_orders[idx]
            .subtotal_poisha
            .checked_add(line_total)
            .ok_or_else(|| {
                "কার্টের উপমোট হিসাব সীমা ছাড়িয়ে গেছে — cart subtotal overflows the money range"
                    .to_string()
            })?;
        sub_orders[idx].items.push(item.clone());
    }

    Ok(sub_orders)
}

/// Compute platform commission and seller payout for a goods subtotal.
///
/// `rate_bps` is the commission rate in basis points (800 bps = 8%).
/// Commission is applied to the goods subtotal only — never to delivery
/// charges. Integer division truncates towards zero, keeping the
/// result exact in poisha.
///
/// Wave 39 (security audit CR-12 + HI-19): the multiplication runs
/// in i128 (the pattern `compute_advance_poisha` already used), so
/// a subtotal near `i64::MAX` can no longer wrap into a negative
/// commission; a negative subtotal and a rate above 100% (10,000
/// bps) are rejected as errors instead of producing a payout
/// larger than the subtotal.
pub fn compute_commission(
    subtotal_poisha: i64,
    rate_bps: u32,
) -> Result<CommissionQuote, String> {
    if subtotal_poisha < 0 {
        return Err(
            "কমিশনের ভিত্তি ঋণাত্মক হতে পারে না — commission subtotal cannot be negative"
                .to_string(),
        );
    }
    if rate_bps > 10_000 {
        return Err(format!(
            "কমিশনের হার ১০০% এর বেশি হতে পারে না — commission rate {rate_bps} bps exceeds 100%"
        ));
    }
    // i128 intermediate: subtotal (≤ i64::MAX) × rate (≤ 10,000)
    // always fits, and the quotient never exceeds the subtotal
    // itself, so both casts back to i64 are exact.
    let commission_poisha =
        (i128::from(subtotal_poisha) * i128::from(rate_bps) / 10_000) as i64;
    Ok(CommissionQuote {
        commission_poisha,
        seller_payout_poisha: subtotal_poisha - commission_poisha,
    })
}

impl DeliveryZone {
    /// The snake_case string stored in the database (`orders.delivery_zone`)
    /// and used by the checkout API payload.
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryZone::InsideDhaka => "inside_dhaka",
            DeliveryZone::DhakaSuburb => "dhaka_suburb",
            DeliveryZone::OutsideDhaka => "outside_dhaka",
        }
    }
}

/// Parse a delivery zone from its snake_case database/API string
/// (`inside_dhaka`, `dhaka_suburb`, `outside_dhaka`). Returns `None`
/// for anything else. Inverse of [`DeliveryZone::as_str`].
pub fn parse_delivery_zone(value: &str) -> Option<DeliveryZone> {
    match value {
        "inside_dhaka" => Some(DeliveryZone::InsideDhaka),
        "dhaka_suburb" => Some(DeliveryZone::DhakaSuburb),
        "outside_dhaka" => Some(DeliveryZone::OutsideDhaka),
        _ => None,
    }
}

/// Maximum quantity of a single product in a cart.
pub const MAX_CART_QUANTITY: u32 = 99;

/// Validate a cart quantity: a single line may hold 1..=99 units.
/// Takes `i64` so raw JSON numbers can be handed over unchecked;
/// returns the quantity as `u32` on success.
pub fn validate_cart_quantity(quantity: i64) -> Result<u32, String> {
    if (1..=i64::from(MAX_CART_QUANTITY)).contains(&quantity) {
        Ok(quantity as u32)
    } else {
        Err(format!(
            "quantity must be between 1 and {MAX_CART_QUANTITY}"
        ))
    }
}

/// Smallest accepted product weight in grams.
pub const MIN_WEIGHT_GRAMS: i64 = 1;
/// Largest accepted product weight in grams (50 kg per single product).
pub const MAX_WEIGHT_GRAMS: i64 = 50_000;
/// Weight assumed for a product that does not specify one (1 kg).
pub const DEFAULT_WEIGHT_GRAMS: i64 = 1_000;

/// Validate a product weight in grams: 1..=50,000. Takes and returns
/// `i64` so raw JSON numbers can be handed over unchecked, mirroring
/// [`validate_cart_quantity`].
pub fn validate_weight_grams(weight_grams: i64) -> Result<i64, String> {
    if (MIN_WEIGHT_GRAMS..=MAX_WEIGHT_GRAMS).contains(&weight_grams) {
        Ok(weight_grams)
    } else {
        Err(format!(
            "weight_grams must be between {MIN_WEIGHT_GRAMS} and {MAX_WEIGHT_GRAMS}"
        ))
    }
}

/// Convert a total weight in grams to kilograms (exact, fractional).
/// Non-positive totals convert to 0.0. Callers that bill per started
/// kilogram round the result up — [`build_order_totals`] does exactly
/// that before calling [`calculate_delivery_charge`].
pub fn weight_kg_from_grams(total_grams: i64) -> f64 {
    if total_grams <= 0 {
        0.0
    } else {
        total_grams as f64 / 1_000.0
    }
}

/// One seller's group of cart items, ready for order-total computation.
/// The group's parcel weight is derived from its items: each item
/// carries its product's `unit_weight_grams`, and the group total is
/// the sum of unit weight times quantity (see [`build_order_totals`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderGroupInput {
    pub store_id: String,
    pub items: Vec<CartItem>,
    /// Coupon discount allocated to this group (Wave 14), in poisha;
    /// zero when the order uses no coupon. Allocated by
    /// [`allocate_discount`] over the in-scope groups, so the group
    /// discounts sum exactly to the order's discount.
    pub discount_poisha: i64,
    /// Who bears this group's discount (Wave 14). True for a STORE
    /// coupon: the seller bears it, so commission is computed on
    /// goods minus discount. False for a PLATFORM coupon: the
    /// platform bears it, so commission stays on the full goods.
    /// The API decides this from the coupon's scope; the totals
    /// function only applies the arithmetic.
    pub commission_on_discounted: bool,
    /// This group's own commission rate in basis points (Wave 36),
    /// when it differs from the order-wide rate handed to
    /// [`build_order_totals`]: a store on a paid subscription plan
    /// earns a plan discount off the platform rate, and each
    /// seller group in a split order may sit on a different plan.
    /// `None` means "the order-wide rate" — every pre-Wave-36
    /// caller and test constructs groups this way and is
    /// unaffected.
    pub commission_rate_bps: Option<u32>,
}

/// Money breakdown for one seller's sub-order, all in poisha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubOrderTotals {
    pub goods_subtotal_poisha: i64,
    /// Coupon discount allocated to this sub-order (Wave 14).
    pub discount_poisha: i64,
    pub delivery_charge_poisha: i64,
    pub commission_poisha: i64,
    pub seller_payout_poisha: i64,
}

/// Full money breakdown of a split order: one entry per seller group,
/// in the same order as the input groups, plus the order-level totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderTotals {
    pub sub_orders: Vec<SubOrderTotals>,
    pub goods_total_poisha: i64,
    /// Total coupon discount across all sub-orders (Wave 14).
    pub discount_total_poisha: i64,
    pub delivery_total_poisha: i64,
    /// What the buyer pays: goods − discount + delivery.
    pub grand_total_poisha: i64,
}

/// Compute every money figure for a split order from per-seller groups.
///
/// This is a composition of the existing atomic functions — it does NOT
/// re-implement their maths: the delivery charge comes from
/// [`calculate_delivery_charge`] (the COD amount passed is the group's
/// goods subtotal; the Wave 1 charge rule validates the COD amount but
/// does not price on it, so the charge is exact) and the commission
/// figure from [`compute_commission`] applied to the group's
/// commission base (see the coupon paragraph below).
///
/// Each group's parcel weight is the sum of its items' unit weights
/// times quantities, converted with [`weight_kg_from_grams`] and
/// rounded UP to the next whole kilogram (couriers bill per started
/// kilogram), so a 3.5 kg group is billed as 4 kg.
///
/// Coupon discounts (Wave 14) are applied per group, from the
/// group's `discount_poisha` (clamped to its goods subtotal — a
/// group can never be discounted below zero). The seller payout is
/// always goods − discount − commission. The commission base is
/// the discounted goods when the group bears its own discount
/// (`commission_on_discounted`, a store coupon) and the full goods
/// when the platform bears it (a platform coupon). The buyer pays
/// goods − discount + delivery: the discount reduces the grand
/// total no matter who bears it.
///
/// Commission rates (Wave 36): `commission_rate_bps` is the
/// order-wide rate, and a group carrying its own
/// `commission_rate_bps` (a subscription plan discount resolved
/// per store by the API) pays THAT rate instead — in a split
/// order each store's slice is charged at its own plan's rate,
/// snapshotted into its sub-order exactly like the single-rate
/// case.
///
/// Wave 39 (security audit CR-11/CR-12): the function returns a
/// `Result` — negative prices/weights and any arithmetic
/// overflow are errors, never panics or wrapped figures.
pub fn build_order_totals(
    groups: &[OrderGroupInput],
    zone: DeliveryZone,
    commission_rate_bps: u32,
) -> Result<OrderTotals, String> {
    let mut sub_orders = Vec::with_capacity(groups.len());
    let mut goods_total_poisha = 0i64;
    let mut discount_total_poisha = 0i64;
    let mut delivery_total_poisha = 0i64;

    for group in groups {
        // Wave 39 (security audit CR-11): the totals boundary
        // REJECTS impossible money inputs instead of panicking on
        // them downstream — a negative unit price, a negative
        // unit weight, or a group whose goods subtotal lands
        // negative. (The audit showed a negative price previously
        // reached `calculate_delivery_charge`'s assert and crashed
        // the request.) Product prices are also validated when a
        // product is created; this is the boundary that must hold
        // no matter what a caller hands in.
        let mut goods_subtotal_poisha = 0i64;
        let mut total_weight_grams = 0i64;
        for item in &group.items {
            if item.unit_price_poisha < 0 {
                return Err(
                    "পণ্যের দাম ঋণাত্মক হতে পারে না — unit price cannot be negative"
                        .to_string(),
                );
            }
            if item.unit_weight_grams < 0 {
                return Err(
                    "পণ্যের ওজন ঋণাত্মক হতে পারে না — unit weight cannot be negative"
                        .to_string(),
                );
            }
            goods_subtotal_poisha = goods_subtotal_poisha
                .checked_add(item.line_total_poisha()?)
                .ok_or_else(|| {
                    "পণ্যের মোট হিসাব সীমা ছাড়িয়ে গেছে — goods subtotal overflows the money range"
                        .to_string()
                })?;
            let line_weight = item
                .unit_weight_grams
                .checked_mul(i64::from(item.quantity))
                .ok_or_else(|| {
                    "ওজনের হিসাব সীমা ছাড়িয়ে গেছে — parcel weight overflows the range"
                        .to_string()
                })?;
            total_weight_grams = total_weight_grams
                .checked_add(line_weight)
                .ok_or_else(|| {
                    "ওজনের হিসাব সীমা ছাড়িয়ে গেছে — parcel weight overflows the range"
                        .to_string()
                })?;
        }
        if goods_subtotal_poisha < 0 {
            return Err(
                "পণ্যের উপমোট ঋণাত্মক হতে পারে না — goods subtotal cannot be negative"
                    .to_string(),
            );
        }
        let billable_weight_kg = weight_kg_from_grams(total_weight_grams).ceil() as u32;
        let delivery_charge_poisha = calculate_delivery_charge(
            zone,
            billable_weight_kg,
            goods_subtotal_poisha,
        )?;
        let discount_poisha = group.discount_poisha.clamp(0, goods_subtotal_poisha);
        let commission_base_poisha = if group.commission_on_discounted {
            goods_subtotal_poisha - discount_poisha
        } else {
            goods_subtotal_poisha
        };
        // Wave 36: a group may carry its own rate (a subscription
        // plan discount resolved per store by the API); without
        // one it pays the order-wide rate, exactly as before.
        let group_rate_bps = group.commission_rate_bps.unwrap_or(commission_rate_bps);
        let commission_poisha =
            compute_commission(commission_base_poisha, group_rate_bps)?.commission_poisha;
        goods_total_poisha = goods_total_poisha
            .checked_add(goods_subtotal_poisha)
            .ok_or_else(|| {
                "অর্ডারের মোট হিসাব সীমা ছাড়িয়ে গেছে — order goods total overflows the money range"
                    .to_string()
            })?;
        discount_total_poisha = discount_total_poisha
            .checked_add(discount_poisha)
            .ok_or_else(|| {
                "অর্ডারের মোট হিসাব সীমা ছাড়িয়ে গেছে — order discount total overflows the money range"
                    .to_string()
            })?;
        delivery_total_poisha = delivery_total_poisha
            .checked_add(delivery_charge_poisha)
            .ok_or_else(|| {
                "অর্ডারের মোট হিসাব সীমা ছাড়িয়ে গেছে — order delivery total overflows the money range"
                    .to_string()
            })?;
        sub_orders.push(SubOrderTotals {
            goods_subtotal_poisha,
            discount_poisha,
            delivery_charge_poisha,
            commission_poisha,
            seller_payout_poisha: goods_subtotal_poisha - discount_poisha - commission_poisha,
        });
    }

    let grand_total_poisha = goods_total_poisha
        .checked_sub(discount_total_poisha)
        .and_then(|net| net.checked_add(delivery_total_poisha))
        .ok_or_else(|| {
            "অর্ডারের মোট হিসাব সীমা ছাড়িয়ে গেছে — order grand total overflows the money range"
                .to_string()
        })?;

    Ok(OrderTotals {
        sub_orders,
        goods_total_poisha,
        discount_total_poisha,
        delivery_total_poisha,
        grand_total_poisha,
    })
}

/// Sub-order (per-seller fulfilment) statuses — mirrors the DB CHECK
/// constraint on `sub_orders.status`.
pub fn is_valid_sub_order_status(status: &str) -> bool {
    matches!(
        status,
        "placed" | "confirmed" | "shipped" | "delivered" | "returned" | "cancelled"
    )
}

/// May a sub-order move from `from` to `to`? The single transition table
/// for seller fulfilment: forward flow placed → confirmed → shipped →
/// delivered; a not-yet-shipped sub-order may be cancelled; a shipped
/// one may be returned. Everything else (skips, backwards moves,
/// same-state, moves out of a terminal state) is rejected.
pub fn can_transition_sub_order(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("placed", "confirmed")
            | ("confirmed", "shipped")
            | ("shipped", "delivered")
            | ("placed", "cancelled")
            | ("confirmed", "cancelled")
            | ("shipped", "returned")
    )
}

/// May the BUYER cancel a whole order? Only while every sub-order is
/// still `placed` — once any seller has confirmed (or beyond), the
/// parcels are in motion and the buyer can no longer cancel. An order
/// with no sub-orders is not cancellable. This is the order-level rule;
/// the per-sub-order seller moves live in [`can_transition_sub_order`].
pub fn can_buyer_cancel(sub_statuses: &[&str]) -> bool {
    !sub_statuses.is_empty() && sub_statuses.iter().all(|status| *status == "placed")
}

/// Payment lifecycle statuses for one online payment attempt (mirrors
/// the DB CHECK constraint on `payments.status`). The order-level
/// `orders.payment_status` additionally allows `pay_on_delivery` for
/// COD orders, which have no payment row at all.
pub fn is_valid_payment_status(status: &str) -> bool {
    matches!(status, "pending" | "paid" | "failed" | "refunded")
}

/// May a payment move from `from` to `to`? The single payment
/// transition table (Wave 5): a pending payment settles as paid or
/// fails; a paid payment can be refunded; a failed payment can be
/// retried back to pending. Same-state moves, backwards moves and
/// moves out of `refunded` are all rejected — duplicate webhook
/// deliveries are handled by callers as idempotent no-ops BEFORE
/// consulting this table.
pub fn can_transition_payment(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("pending", "paid") | ("pending", "failed") | ("paid", "refunded") | ("failed", "pending")
    )
}

/// Does a gateway callback report the amount we actually expected?
/// Exact integer-poisha equality — a callback for any other amount
/// (even one poisha more or less) must never settle a payment.
pub fn validate_callback_amount(expected_poisha: i64, reported_poisha: i64) -> bool {
    expected_poisha == reported_poisha
}

/// Shipment (courier parcel) statuses — the unified set every
/// courier's raw statuses are mapped onto, so buyers, sellers and the
/// worker never learn courier-specific strings. Mirrors the DB CHECK
/// constraint on `shipments.status` (migration 0010).
pub fn is_valid_shipment_status(status: &str) -> bool {
    matches!(
        status,
        "created"
            | "picked_up"
            | "in_transit"
            | "out_for_delivery"
            | "delivered"
            | "partial_delivered"
            | "return_in_transit"
            | "returned"
            | "cancelled"
            | "unknown"
    )
}

/// May a shipment move from `from` to `to`? The single shipment
/// transition table (Wave 6): the forward flow created → picked_up →
/// in_transit → out_for_delivery → delivered; a partial delivery
/// (`out_for_delivery → partial_delivered`) may still complete
/// (`→ delivered`) or come back (`→ returned`); a delivered parcel
/// can only move again on the return path (`→ return_in_transit →
/// returned`); any parcel that has not reached a final state may be
/// cancelled, but `delivered` is NOT cancellable (the goods arrived —
/// the return path is the only way back) and `returned` / `cancelled`
/// are final. `unknown` never transitions automatically, in either
/// direction: it is the "courier said something we do not recognise"
/// marker, and callers treat it as no change. Same-state moves are
/// rejected here too — callers treat an unchanged poll result as a
/// no-op BEFORE consulting this table.
pub fn can_transition_shipment(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("created", "picked_up")
            | ("picked_up", "in_transit")
            | ("in_transit", "out_for_delivery")
            | ("out_for_delivery", "delivered")
            | ("out_for_delivery", "partial_delivered")
            | ("partial_delivered", "delivered")
            | ("partial_delivered", "returned")
            | ("delivered", "return_in_transit")
            | ("return_in_transit", "returned")
            | ("created", "cancelled")
            | ("picked_up", "cancelled")
            | ("in_transit", "cancelled")
            | ("out_for_delivery", "cancelled")
            | ("partial_delivered", "cancelled")
            | ("return_in_transit", "cancelled")
    )
}

/// The seller fulfillment center's work buckets (Wave 40): the
/// five piles a seller's sub-orders sort into on the fulfillment
/// queue. The queue is a WORK view over the existing sub-order
/// and shipment statuses — it introduces no new status anywhere;
/// [`fulfillment_bucket`] is the one classification rule and
/// [`sla_breached`] the one lateness rule, so the queue, its
/// counts and the dashboard card can never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FulfillmentBucket {
    /// Ordered, waiting for the seller to confirm.
    ToConfirm,
    /// Confirmed, waiting for a courier booking.
    ToShip,
    /// Booked and moving (or booked and awaiting pickup).
    InTransit,
    /// Delivered — the work is finished.
    Done,
    /// Something came back or was cancelled: a human must look.
    Attention,
}

impl FulfillmentBucket {
    /// The snake_case string the API returns and the web app
    /// keys its tabs on.
    pub fn as_str(&self) -> &'static str {
        match self {
            FulfillmentBucket::ToConfirm => "to_confirm",
            FulfillmentBucket::ToShip => "to_ship",
            FulfillmentBucket::InTransit => "in_transit",
            FulfillmentBucket::Done => "done",
            FulfillmentBucket::Attention => "attention",
        }
    }
}

/// Which bucket one sub-order belongs to (Wave 40), from its own
/// status, whether a shipment exists for it, and the shipment's
/// unified status when it does. The mapping, in precedence
/// order (first match wins):
///
/// 1. Shipment `returned`, `cancelled` or `return_in_transit`
///    → **Attention**. The parcel is coming back or its booking
///    died — whatever the sub-order says, a human must look.
///    (The unified shipment vocabulary has no `failed` status:
///    `cancelled` is its failure-terminal state, and a courier
///    failure that maps to `unknown` leaves the sub-order's own
///    status in charge below.)
/// 2. Sub-order `returned` or `cancelled` → **Attention**.
/// 3. Sub-order `delivered` → **Done**.
/// 4. Sub-order `placed` → **ToConfirm**.
/// 5. Sub-order `confirmed` → **ToShip** when it has no
///    shipment yet; **InTransit** when one exists (booking
///    normally moves the sub-order to `shipped` in the same
///    transaction, so this is a defensive reading of a raced
///    row: the parcel is in the pipeline, not awaiting booking).
/// 6. Sub-order `shipped` → **InTransit** (covers shipment
///    `created` awaiting pickup and every moving state —
///    `picked_up`, `in_transit`, `out_for_delivery`,
///    `partial_delivered` — plus `unknown`).
/// 7. Any other sub-order status → **Attention**: an
///    unrecognised state must fail VISIBLE in the queue, never
///    hide in Done.
pub fn fulfillment_bucket(
    sub_order_status: &str,
    has_shipment: bool,
    shipment_status: Option<&str>,
) -> FulfillmentBucket {
    if matches!(
        shipment_status,
        Some("returned") | Some("cancelled") | Some("return_in_transit")
    ) {
        return FulfillmentBucket::Attention;
    }
    match sub_order_status {
        "returned" | "cancelled" => FulfillmentBucket::Attention,
        "delivered" => FulfillmentBucket::Done,
        "placed" => FulfillmentBucket::ToConfirm,
        "confirmed" => {
            if has_shipment {
                FulfillmentBucket::InTransit
            } else {
                FulfillmentBucket::ToShip
            }
        }
        "shipped" => FulfillmentBucket::InTransit,
        _ => FulfillmentBucket::Attention,
    }
}

/// Has a queued sub-order breached its SLA (Wave 40)? Only the
/// two actionable buckets carry an SLA: a `ToConfirm` item
/// breaches once its age REACHES `confirm_sla_hours` (the limit
/// is the deadline, not a grace period — at exactly the limit
/// the promise is already broken), and a `ToShip` item once its
/// age reaches `ship_sla_hours`. InTransit, Done and Attention
/// items never "breach" here: transit time is the courier's,
/// done is done, and attention items are already flagged.
pub fn sla_breached(
    bucket: FulfillmentBucket,
    age_hours: i64,
    confirm_sla_hours: i64,
    ship_sla_hours: i64,
) -> bool {
    match bucket {
        FulfillmentBucket::ToConfirm => age_hours >= confirm_sla_hours,
        FulfillmentBucket::ToShip => age_hours >= ship_sla_hours,
        _ => false,
    }
}

/// The manual couriers (Wave 19): Sundarban, SA Paribahan,
/// Karatoa and Carrybee. These couriers have NO public merchant
/// API — a seller books at the counter of the platform's master
/// account and carries the parcel there physically — so they can
/// never be adapter-backed like the five API couriers. Instead
/// they join the same shipment system as a seller-driven manual
/// workflow: the seller records the booking, the counter receipt
/// number and every status move by hand. Each entry is the
/// canonical key the API stores and accepts, paired with the
/// display name shown to buyers and sellers.
pub const MANUAL_COURIERS: [(&str, &str); 4] = [
    ("sundarban", "Sundarban Courier"),
    ("sa_paribahan", "SA Paribahan"),
    ("karatoa", "Karatoa Courier"),
    ("carrybee", "Carrybee"),
];

/// The display name of one manual courier key, or `None` when
/// the key is not a manual courier.
pub fn manual_courier_display_name(key: &str) -> Option<&'static str> {
    MANUAL_COURIERS
        .iter()
        .find(|(known, _)| *known == key)
        .map(|(_, display)| *display)
}

/// Validate a seller-supplied manual courier name and return its
/// canonical key. Matching is on the KEY, trimmed and
/// case-insensitive (`Sundarban`, `SUNdarban` and `sundarban`
/// are the same courier); display names are NOT accepted — the
/// key is the one identifier, so what the seller picks in the UI
/// is exactly what gets stored.
pub fn validate_manual_courier(input: &str) -> Result<&'static str, &'static str> {
    let normalized = input.trim().to_ascii_lowercase();
    MANUAL_COURIERS
        .iter()
        .map(|(key, _)| *key)
        .find(|key| *key == normalized)
        .ok_or("unknown manual courier")
}

/// Validate a manual courier's counter receipt / tracking id.
/// The receipt number is the ONLY verifiable reference a manual
/// booking has — the paper receipt from the courier counter —
/// so it is kept verbatim (trimmed) and held to a shape a real
/// receipt number can take: 4 to 64 characters with no
/// whitespace inside. `None` or a blank string means the seller
/// has not recorded one yet and validates to `None`.
pub fn validate_manual_tracking_id(
    input: Option<&str>,
) -> Result<Option<String>, &'static str> {
    let trimmed = match input {
        None => return Ok(None),
        Some(raw) => raw.trim(),
    };
    if trimmed.is_empty() {
        return Ok(None);
    }
    let char_count = trimmed.chars().count();
    if !(4..=64).contains(&char_count) {
        return Err("tracking id must be 4 to 64 characters");
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("tracking id must not contain whitespace");
    }
    Ok(Some(trimmed.to_string()))
}

/// Normalize a courier's raw status string for mapping: trim
/// surrounding whitespace, lowercase, fold spaces, hyphens and
/// underscores into single underscores, and split camelCase word
/// boundaries — so `Out for Delivery`, `out-for-delivery`,
/// `OUT_FOR_DELIVERY` and `OutForDelivery` all normalize to
/// `out_for_delivery`, and Pathao's `InTransit` normalizes to
/// `in_transit`. Every courier status mapping below matches on this
/// normalized form — couriers disagree wildly on casing and
/// separators, and the normalization rule should exist in exactly
/// one place.
pub fn normalize_courier_status(raw: &str) -> String {
    let mut out = String::new();
    // The previous SIGNIFICANT character as written (before
    // lowercasing), or '_' after a separator run. Camel boundaries
    // are detected against the original casing so an all-caps word
    // like `PENDING` stays one word.
    let mut prev: Option<char> = None;
    for c in raw.trim().chars() {
        if c == ' ' || c == '-' || c == '_' {
            if prev != Some('_') && !out.is_empty() {
                out.push('_');
                prev = Some('_');
            }
            continue;
        }
        if c.is_ascii_uppercase()
            && prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
        {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
        prev = Some(c);
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// Map one Steadfast `delivery_status` value onto the unified
/// shipment status set (see [`is_valid_shipment_status`]). This is
/// the single Steadfast mapping: the courier crate's adapter calls
/// it, so the mapping exists in exactly one place.
///
/// Steadfast's documented values and the choices made here:
/// `pending` (consignment created, awaiting pickup) maps to
/// `created`; `delivered` maps to `delivered`; `partial_delivered`
/// keeps its own unified status — only part of the parcel was handed
/// over, so calling it `delivered` would overstate the truth;
/// `cancelled` maps to `cancelled`; `returned` maps to `returned`;
/// `hold` (parcel held at a hub) maps to `in_transit` — the parcel
/// is still inside the delivery pipeline, just paused. Anything else
/// (values Steadfast adds later, typos, empty strings) maps to
/// `unknown`, which the transition table treats as no change.
/// Matching is on the normalized form (see
/// [`normalize_courier_status`]), so whitespace, case, spaces and
/// hyphens never change the answer.
pub fn map_steadfast_status(raw: &str) -> &'static str {
    match normalize_courier_status(raw).as_str() {
        "pending" => "created",
        "delivered" => "delivered",
        "partial_delivered" => "partial_delivered",
        "cancelled" => "cancelled",
        "returned" => "returned",
        "hold" => "in_transit",
        _ => "unknown",
    }
}

/// Map one Pathao `order_status` value onto the unified shipment
/// status set. This is the single Pathao mapping: the courier
/// crate's adapter calls it, so the mapping exists in exactly one
/// place.
///
/// Pathao's documented values and the choices made here: `Pending`
/// and `PickupRequested` (order placed, pickup not yet done) map to
/// `created`, as does `PickupFailed` — a failed pickup attempt means
/// the parcel is still with the merchant, so the shipment has not
/// started moving; `Assigned` (a rider is assigned) and `Picked`
/// map to `picked_up`; `InTransit` maps to `in_transit`, and so
/// does `OnHold` — the same choice as Steadfast's `hold`: the
/// parcel is paused inside the pipeline, not back with the sender;
/// `Delivered` maps to `delivered`; `PartialDelivered` keeps its
/// own unified status; `FailedDelivery` (the rider attempted
/// delivery and failed; Pathao retries) maps to `out_for_delivery`
/// — the parcel is out with the rider on a delivery run, which is
/// exactly what that unified status means, and mapping it backwards
/// to `in_transit` would fight the transition table; `Returned`
/// maps to `returned`; `ReturnRequested` (a return has been asked
/// for but the parcel is still travelling back) maps to
/// `return_in_transit`; `Cancelled` maps to `cancelled`. Anything
/// else maps to `unknown`, which the transition table treats as no
/// change. Matching is on the normalized form, so `InTransit`,
/// `in_transit` and `in transit` are the same value.
pub fn map_pathao_status(raw: &str) -> &'static str {
    match normalize_courier_status(raw).as_str() {
        "pending" => "created",
        "pickup_requested" => "created",
        "pickup_failed" => "created",
        "assigned" => "picked_up",
        "picked" => "picked_up",
        "in_transit" => "in_transit",
        "on_hold" => "in_transit",
        "failed_delivery" => "out_for_delivery",
        "delivered" => "delivered",
        "partial_delivered" => "partial_delivered",
        "partially_delivered" => "partial_delivered",
        "return_requested" => "return_in_transit",
        "returned" => "returned",
        "cancelled" => "cancelled",
        _ => "unknown",
    }
}

/// Map one RedX tracking status onto the unified shipment status
/// set. This is the single RedX mapping: the courier crate's adapter
/// calls it, so the mapping exists in exactly one place.
///
/// RedX reports human-readable status strings (`picked-up`,
/// `out-for-delivery`, `on-hold`, …) rather than machine codes, so
/// the table is deliberately conservative — only the strings we are
/// confident about are mapped, and everything else is `unknown`
/// (which the transition table treats as no change) rather than a
/// guess. The choices: `created` and `package is ready for
/// delivery` (RedX's earliest tracking state: the parcel is booked
/// and waiting) map to `created`; the picked variants (`picked-up`,
/// `picked`) map to `picked_up`; transit and hub strings
/// (`in-transit`, `at-hub`, `received-at-hub`) map to `in_transit`,
/// and so does `on-hold` — the same paused-in-the-pipeline choice as
/// the other couriers; `out-for-delivery` maps to
/// `out_for_delivery`, and so does `failed-delivery` (a failed
/// attempt on a delivery run; RedX retries — the same choice as
/// Pathao's `FailedDelivery`); `delivered` maps to `delivered`;
/// `partial-delivered` / `partially-delivered` keep their own
/// unified status; on the return path, `return-in-transit` and
/// `returning` map to `return_in_transit`, while `returned` and
/// `delivered-returned` (a parcel that came back after a delivery
/// attempt) map to `returned`; `cancelled` (and the US spelling
/// `canceled`) maps to `cancelled`. Matching is on the normalized
/// form, so hyphens, spaces, underscores and case never change the
/// answer.
pub fn map_redx_status(raw: &str) -> &'static str {
    match normalize_courier_status(raw).as_str() {
        "created" => "created",
        "package_is_ready_for_delivery" => "created",
        "picked_up" => "picked_up",
        "picked" => "picked_up",
        "in_transit" => "in_transit",
        "at_hub" => "in_transit",
        "in_hub" => "in_transit",
        "received_at_hub" => "in_transit",
        "on_hold" => "in_transit",
        "out_for_delivery" => "out_for_delivery",
        "failed_delivery" => "out_for_delivery",
        "delivered" => "delivered",
        "partial_delivered" => "partial_delivered",
        "partially_delivered" => "partial_delivered",
        "return_in_transit" => "return_in_transit",
        "returning" => "return_in_transit",
        "returned" => "returned",
        "delivered_returned" => "returned",
        "cancelled" => "cancelled",
        "canceled" => "cancelled",
        _ => "unknown",
    }
}

/// Map one eCourier tracking status onto the unified shipment
/// status set. This is the single eCourier mapping: the courier
/// crate's adapter calls it, so the mapping exists in exactly one
/// place.
///
/// eCourier reports title-case status strings (`Processing`,
/// `Picked`, `Shipped`, `Delivered`, …), so the table is
/// deliberately conservative, like RedX's — only confidently known
/// strings are mapped, and everything else is `unknown` (which the
/// transition table treats as no change) rather than a guess. The
/// choices: `Processing` (order accepted, being prepared for
/// pickup) and `pending` / `created` map to `created`; `picked` /
/// `picked_up` map to `picked_up`; `shipped` and `in_transit` map
/// to `in_transit`, and so do `hold` / `on_hold` — the same
/// paused-in-the-pipeline choice as the other couriers;
/// `out_for_delivery` maps to `out_for_delivery`, and so does
/// `failed_delivery` (a failed attempt on a delivery run, which is
/// retried — the same choice as Pathao and RedX); `delivered` maps
/// to `delivered`; `partial_delivered` / `partially_delivered` keep
/// their own unified status; on the return path, `return_in_transit`
/// and `returning` map to `return_in_transit`, while `returned` and
/// `delivered_returned` map to `returned`; `cancelled` (and the US
/// spelling `canceled`) maps to `cancelled`. Matching is on the
/// normalized form, so case, spaces and hyphens never change the
/// answer.
pub fn map_ecourier_status(raw: &str) -> &'static str {
    match normalize_courier_status(raw).as_str() {
        "processing" => "created",
        "pending" => "created",
        "created" => "created",
        "picked" => "picked_up",
        "picked_up" => "picked_up",
        "shipped" => "in_transit",
        "in_transit" => "in_transit",
        "hold" => "in_transit",
        "on_hold" => "in_transit",
        "out_for_delivery" => "out_for_delivery",
        "failed_delivery" => "out_for_delivery",
        "delivered" => "delivered",
        "partial_delivered" => "partial_delivered",
        "partially_delivered" => "partial_delivered",
        "return_in_transit" => "return_in_transit",
        "returning" => "return_in_transit",
        "returned" => "returned",
        "delivered_returned" => "returned",
        "cancelled" => "cancelled",
        "canceled" => "cancelled",
        _ => "unknown",
    }
}

/// Map one Paperfly tracking status onto the unified shipment
/// status set. This is the single Paperfly mapping: the courier
/// crate's adapter calls it, so the mapping exists in exactly one
/// place.
///
/// Paperfly reports sentence-style status strings (`Order
/// Received`, `Pickup Requested`, `Out for Delivery`, …), so the
/// table is deliberately conservative, like RedX's and eCourier's —
/// only confidently known strings are mapped, and everything else
/// is `unknown`. The choices: `Order Received` and `Pickup
/// Requested` (the order exists; the parcel has not moved yet) and
/// `pending` / `created` map to `created`; `picked` / `picked_up`
/// map to `picked_up`; `in_transit` maps to `in_transit`, and so do
/// `hold` / `on_hold` — the same paused-in-the-pipeline choice as
/// the other couriers; `out_for_delivery` maps to
/// `out_for_delivery`, and so does `failed_delivery` (a failed
/// attempt on a delivery run, which is retried — the same choice
/// as the other couriers); `delivered` maps to `delivered`;
/// `partially_delivered` / `partial_delivered` keep their own
/// unified status; on the return path, `return_in_transit` and
/// `returning` map to `return_in_transit`, while `returned` and
/// `delivered_returned` map to `returned`; `cancelled` (and the US
/// spelling `canceled`) maps to `cancelled`. Matching is on the
/// normalized form, so `Partially Delivered` and
/// `partial_delivered` are the same value.
pub fn map_paperfly_status(raw: &str) -> &'static str {
    match normalize_courier_status(raw).as_str() {
        "order_received" => "created",
        "pickup_requested" => "created",
        "pending" => "created",
        "created" => "created",
        "picked" => "picked_up",
        "picked_up" => "picked_up",
        "in_transit" => "in_transit",
        "hold" => "in_transit",
        "on_hold" => "in_transit",
        "out_for_delivery" => "out_for_delivery",
        "failed_delivery" => "out_for_delivery",
        "delivered" => "delivered",
        "partial_delivered" => "partial_delivered",
        "partially_delivered" => "partial_delivered",
        "return_in_transit" => "return_in_transit",
        "returning" => "return_in_transit",
        "returned" => "returned",
        "delivered_returned" => "returned",
        "cancelled" => "cancelled",
        "canceled" => "cancelled",
        _ => "unknown",
    }
}

/// One courier's candidacy for smart routing: its name, whether it
/// can actually book right now (credentials configured, or the DEV
/// simulator is on — the API layer decides this from config), and
/// its delivery-charge estimate in poisha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CourierQuote<'a> {
    pub courier: &'a str,
    pub available: bool,
    pub price_poisha: i64,
}

/// Platform priority for breaking routing ties, best first:
/// Steadfast, then Pathao, then RedX, then eCourier, then Paperfly.
/// An unrecognised courier name ranks after every known courier.
pub fn courier_priority(courier: &str) -> usize {
    match courier {
        "steadfast" => 0,
        "pathao" => 1,
        "redx" => 2,
        "ecourier" => 3,
        "paperfly" => 4,
        _ => usize::MAX,
    }
}

/// Smart routing: pick the courier for a parcel from the candidates.
/// The rule is deliberately simple and explainable — among the
/// AVAILABLE candidates the lowest estimated price wins, and ties
/// break by platform priority ([`courier_priority`]). Unavailable
/// candidates are skipped entirely (their price is never trusted),
/// and an empty or all-unavailable list yields `None`, which the
/// caller turns into an honest "no courier available" error.
pub fn choose_courier<'a>(candidates: &[CourierQuote<'a>]) -> Option<&'a str> {
    let mut best: Option<&CourierQuote<'a>> = None;
    for candidate in candidates.iter().filter(|c| c.available) {
        let wins = match best {
            None => true,
            Some(current) => {
                candidate.price_poisha < current.price_poisha
                    || (candidate.price_poisha == current.price_poisha
                        && courier_priority(candidate.courier)
                            < courier_priority(current.courier))
            }
        };
        if wins {
            best = Some(candidate);
        }
    }
    best.map(|c| c.courier)
}

/// Validate a Bangladeshi mobile number in local form: exactly 11 ASCII
/// digits, starting with `01`, third digit in 3..=9 (operator prefixes
/// 013–019). Pure helper — the single source of truth for phone rules.
fn is_valid_local_bd_phone(local: &str) -> bool {
    let bytes = local.as_bytes();
    bytes.len() == 11
        && local.bytes().all(|b| b.is_ascii_digit())
        && bytes[0] == b'0'
        && bytes[1] == b'1'
        && (b'3'..=b'9').contains(&bytes[2])
}

/// Is this a valid Bangladeshi mobile number? Accepts the local form
/// `01XXXXXXXXX` and the international form `+8801XXXXXXXXX`.
pub fn is_valid_bd_phone(phone: &str) -> bool {
    if let Some(rest) = phone.strip_prefix("+880") {
        is_valid_local_bd_phone(&format!("0{rest}"))
    } else {
        is_valid_local_bd_phone(phone)
    }
}

/// Normalize a Bangladeshi mobile number to the canonical international
/// form `+8801XXXXXXXXX`. Accepts `01XXXXXXXXX`, `8801XXXXXXXXX` and
/// `+8801XXXXXXXXX` (whitespace ignored). Returns `None` if invalid.
/// Auth stores and looks up only this canonical form.
pub fn normalize_bd_phone(phone: &str) -> Option<String> {
    let cleaned: String = phone.chars().filter(|c| !c.is_whitespace()).collect();
    let local = if let Some(rest) = cleaned.strip_prefix("+880") {
        format!("0{rest}")
    } else if let Some(rest) = cleaned.strip_prefix("880") {
        format!("0{rest}")
    } else {
        cleaned
    };
    if is_valid_local_bd_phone(&local) {
        Some(format!("+880{}", &local[1..]))
    } else {
        None
    }
}

/// Convert a Bangladeshi mobile number to the LOCAL form
/// `01XXXXXXXXX`. Accepts the same inputs as [`normalize_bd_phone`]
/// (local, `880…` and `+880…` forms, whitespace ignored) and returns
/// `None` for anything that is not a valid BD mobile number. This is
/// the form some courier APIs (Pathao, RedX) require for recipient
/// phone numbers, while the platform stores the canonical `+880…`
/// form everywhere else — this function is the single conversion.
pub fn to_local_phone(phone: &str) -> Option<String> {
    let canonical = normalize_bd_phone(phone)?;
    // The canonical form is "+880" followed by the local number
    // without its leading 0; re-attach the 0.
    Some(format!("0{}", &canonical[4..]))
}

/// Is this a valid store slug? Slugs are the storefront subdomain segment:
/// 3–40 chars, lowercase ASCII letters, digits and hyphens, and they must
/// start and end with a letter or digit.
pub fn is_valid_slug(slug: &str) -> bool {
    let len = slug.chars().count();
    if !(3..=40).contains(&len) {
        return false;
    }
    let valid_chars = slug
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !valid_chars {
        return false;
    }
    let bytes = slug.as_bytes();
    bytes[0].is_ascii_alphanumeric() && bytes[len - 1].is_ascii_alphanumeric()
}

/// Product lifecycle statuses (mirrors the DB CHECK constraint).
pub fn is_valid_product_status(status: &str) -> bool {
    matches!(status, "draft" | "active" | "archived")
}

/// Typed input for product validation — mirrors the create/update payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductInput {
    pub title: String,
    /// Price in poisha.
    pub price_poisha: i64,
    pub stock: i32,
    pub status: String,
    /// Weight of one unit in grams (see [`validate_weight_grams`]).
    pub weight_grams: i64,
}

/// Validate product input. Pure and shared by the create and update
/// service functions so the rules can never drift apart. Returns a
/// human-readable error message on failure.
pub fn validate_product_input(input: &ProductInput) -> Result<(), String> {
    if input.title.trim().is_empty() {
        return Err("title must not be empty".to_string());
    }
    if input.price_poisha < 0 {
        return Err("price_poisha must be >= 0".to_string());
    }
    if input.stock < 0 {
        return Err("stock must be >= 0".to_string());
    }
    if !is_valid_product_status(&input.status) {
        return Err("status must be one of: draft, active, archived".to_string());
    }
    validate_weight_grams(input.weight_grams)?;
    Ok(())
}

/// Maximum length of a review body, in characters.
pub const MAX_REVIEW_BODY_CHARS: usize = 1_000;
/// Maximum length of a seller reply body, in characters.
pub const MAX_REPLY_BODY_CHARS: usize = 500;

/// Is this a valid review rating? Ratings are whole stars, 1 to 5.
pub fn validate_rating(rating: i64) -> bool {
    (1..=5).contains(&rating)
}

/// Validate a review body. The body is OPTIONAL: an empty (or
/// whitespace-only) body is allowed and means "rating only". A present
/// body may be at most [`MAX_REVIEW_BODY_CHARS`] characters (counted
/// after trimming, in Unicode characters, not bytes).
pub fn validate_review_body(body: &str) -> Result<(), &'static str> {
    if body.trim().chars().count() > MAX_REVIEW_BODY_CHARS {
        return Err("review body must be at most 1000 characters");
    }
    Ok(())
}

/// Validate a seller reply body. Unlike a review body, a reply is
/// meaningless when empty: it must be 1 to [`MAX_REPLY_BODY_CHARS`]
/// characters after trimming.
pub fn validate_reply_body(body: &str) -> Result<(), &'static str> {
    let len = body.trim().chars().count();
    if len == 0 {
        return Err("reply body must not be empty");
    }
    if len > MAX_REPLY_BODY_CHARS {
        return Err("reply body must be at most 500 characters");
    }
    Ok(())
}

/// Minimum length of a product question body, in characters.
pub const MIN_QUESTION_BODY_CHARS: usize = 3;
/// Maximum length of a product question body, in characters.
pub const MAX_QUESTION_BODY_CHARS: usize = 500;
/// Maximum length of a seller answer body, in characters.
pub const MAX_ANSWER_BODY_CHARS: usize = 1_000;

/// Validate a product question body (Wave 25). A question must say
/// something: after trimming it is [`MIN_QUESTION_BODY_CHARS`] to
/// [`MAX_QUESTION_BODY_CHARS`] characters (Unicode characters, not
/// bytes, so Bangla text counts fairly). Returns the trimmed body
/// — the value callers store.
pub fn validate_question_body(body: &str) -> Result<String, &'static str> {
    let trimmed = body.trim();
    let len = trimmed.chars().count();
    if len < MIN_QUESTION_BODY_CHARS {
        return Err("question body must be at least 3 characters");
    }
    if len > MAX_QUESTION_BODY_CHARS {
        return Err("question body must be at most 500 characters");
    }
    Ok(trimmed.to_string())
}

/// Validate a seller's answer to a product question (Wave 25). An
/// answer is meaningless when empty: after trimming it is 1 to
/// [`MAX_ANSWER_BODY_CHARS`] characters. Returns the trimmed body
/// — the value callers store.
pub fn validate_answer_body(body: &str) -> Result<String, &'static str> {
    let trimmed = body.trim();
    let len = trimmed.chars().count();
    if len == 0 {
        return Err("answer body must not be empty");
    }
    if len > MAX_ANSWER_BODY_CHARS {
        return Err("answer body must be at most 1000 characters");
    }
    Ok(trimmed.to_string())
}

/// Mask a phone number for public display: keep the first three and
/// last three digits of the local form, hide the middle. Canonical
/// numbers are converted to local form first (`+8801712345678`
/// becomes `017****678`). Inputs too short to mask safely are fully
/// hidden. Review lists use this so a buyer's full number is never
/// exposed publicly.
pub fn mask_phone(phone: &str) -> String {
    let local = to_local_phone(phone).unwrap_or_else(|| phone.to_string());
    let chars: Vec<char> = local.chars().collect();
    if chars.len() < 7 {
        return "****".to_string();
    }
    let head: String = chars.iter().take(3).collect();
    let tail: String = chars.iter().skip(chars.len() - 3).collect();
    format!("{head}****{tail}")
}

/// Minimum payout a seller may request, in poisha (500 taka). A
/// platform rule, not a per-seller setting: small payouts cost more
/// to process (mobile-banking fees, manual review) than they are
/// worth, so earnings accumulate until they cross this floor.
pub const MIN_PAYOUT_POISHA: i64 = 50_000;

/// The actions a store role may be asked to perform (Wave 34).
/// Every seller surface names exactly one of these; the matrix
/// in [`store_role_can`] decides who may do it.
pub const STORE_ACTION_MANAGE_MEMBERS: &str = "manage_members";
/// Payout requests and the payout account — where money leaves.
pub const STORE_ACTION_MANAGE_PAYOUTS: &str = "manage_payouts";
/// Balance, analytics, payout list and the seller export.
pub const STORE_ACTION_VIEW_FINANCES: &str = "view_finances";
/// Products, variants and inventory.
pub const STORE_ACTION_MANAGE_CATALOG: &str = "manage_catalog";
/// Sub-orders, shipments and return decisions.
pub const STORE_ACTION_MANAGE_ORDERS: &str = "manage_orders";
/// Coupons, promotions and store branding.
pub const STORE_ACTION_MANAGE_MARKETING: &str = "manage_marketing";
/// Answering product questions (and review replies stay
/// owner-only this wave — see the API's access module).
pub const STORE_ACTION_ANSWER_QUESTIONS: &str = "answer_questions";
/// Verification submission and the SIDRA auto-reply toggle.
pub const STORE_ACTION_MANAGE_STORE_SETTINGS: &str = "manage_store_settings";

/// Every store action, in the order the README's matrix table
/// lists them. Tests iterate this so a new action can never
/// silently miss a matrix row.
pub const STORE_ACTIONS: [&str; 8] = [
    STORE_ACTION_MANAGE_MEMBERS,
    STORE_ACTION_MANAGE_PAYOUTS,
    STORE_ACTION_VIEW_FINANCES,
    STORE_ACTION_MANAGE_CATALOG,
    STORE_ACTION_MANAGE_ORDERS,
    STORE_ACTION_MANAGE_MARKETING,
    STORE_ACTION_ANSWER_QUESTIONS,
    STORE_ACTION_MANAGE_STORE_SETTINGS,
];

/// The store role permission matrix (Wave 34) — THE one place
/// that decides what a role may do inside a store:
///
/// - `owner` (the store's `owner_user_id`; never a member row)
///   may do everything.
/// - `manager` may do everything EXCEPT `manage_members` and
///   `manage_payouts` — a manager runs the shop day to day but
///   cannot add people or move money out.
/// - `staff` may only work the floor: `manage_catalog`,
///   `manage_orders` and `answer_questions`.
///
/// Unknown roles and unknown actions are denied — the matrix
/// fails closed, so a typo'd role string grants nothing.
pub fn store_role_can(role: &str, action: &str) -> bool {
    match role {
        "owner" => STORE_ACTIONS.contains(&action),
        "manager" => matches!(
            action,
            STORE_ACTION_VIEW_FINANCES
                | STORE_ACTION_MANAGE_CATALOG
                | STORE_ACTION_MANAGE_ORDERS
                | STORE_ACTION_MANAGE_MARKETING
                | STORE_ACTION_ANSWER_QUESTIONS
                | STORE_ACTION_MANAGE_STORE_SETTINGS
        ),
        "staff" => matches!(
            action,
            STORE_ACTION_MANAGE_CATALOG
                | STORE_ACTION_MANAGE_ORDERS
                | STORE_ACTION_ANSWER_QUESTIONS
        ),
        _ => false,
    }
}

/// Is this a role a store MEMBER row may carry? Only `manager`
/// and `staff` — `owner` is never a member row (ownership lives
/// on the store itself), so accepting it here would let a
/// member write mint a second owner.
pub fn validate_store_role(role: &str) -> Result<&str, &'static str> {
    match role.trim() {
        "manager" => Ok("manager"),
        "staff" => Ok("staff"),
        _ => Err("role must be 'manager' or 'staff'"),
    }
}

/// A seller's available balance: what they have earned, minus what
/// has already been paid or committed out, minus what is sitting in
/// `requested` payouts waiting for approval. This is the raw
/// arithmetic and is deliberately NOT clamped: with consistent
/// inputs it can never go negative (a payout may only be requested
/// against the available balance), so a negative result means the
/// inputs disagree — a bug the caller should see, not have hidden.
/// The API boundary clamps via its `available_balance` helper and
/// reports and gates payout requests on the clamped value; with
/// consistent inputs the raw and clamped values are identical.
pub fn compute_available_balance(
    earned_poisha: i64,
    paid_out_poisha: i64,
    pending_poisha: i64,
) -> i64 {
    earned_poisha - paid_out_poisha - pending_poisha
}

/// Payout lifecycle statuses — mirrors the DB CHECK constraint on
/// `payouts.status` (migration 0012).
pub fn is_valid_payout_status(status: &str) -> bool {
    matches!(status, "requested" | "approved" | "paid" | "rejected")
}

/// May a payout move from `from` to `to`? The single payout
/// transition table (Wave 9): a request is approved or rejected by
/// the platform; an approved payout is then paid out. `paid` and
/// `rejected` are final. Same-state moves and every other pair are
/// rejected. (The endpoints that drive these moves land with the
/// admin wave; the table lives here now so the rule exists in
/// exactly one place from the start.)
pub fn can_transition_payout(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("requested", "approved") | ("requested", "rejected") | ("approved", "paid")
    )
}

/// Validate a seller's payout account — where payout money goes.
/// - `bkash` / `nagad`: the account number must be a valid
///   Bangladeshi mobile number (the wallet account IS the phone
///   number); local and international forms both accepted.
/// - `bank`: the account name must be non-empty (a bank transfer
///   needs the account holder's name) and the account number must
///   be 6 to 24 ASCII letters or digits (bank account numbers carry
///   no checksum we can verify; shape is all we can check).
///
/// Any other method is rejected. Error strings are user-facing.
pub fn validate_payout_account(
    method: &str,
    account_name: &str,
    account_number: &str,
) -> Result<(), &'static str> {
    match method {
        "bkash" | "nagad" => {
            if normalize_bd_phone(account_number).is_some() {
                Ok(())
            } else {
                Err("wallet account number must be a valid Bangladeshi mobile number")
            }
        }
        "bank" => {
            if account_name.trim().is_empty() {
                return Err("bank payout requires the account holder name");
            }
            let number = account_number.trim();
            let len = number.chars().count();
            if !(6..=24).contains(&len)
                || !number.chars().all(|c| c.is_ascii_alphanumeric())
            {
                return Err("bank account number must be 6-24 letters or digits");
            }
            Ok(())
        }
        _ => Err("payout method must be bkash, nagad or bank"),
    }
}

/// May a seller request a payout of `amount_poisha` against an
/// available balance of `available_poisha`, when the platform
/// minimum payout is `min_payout_poisha`? Two rules: the amount
/// must reach the minimum and must not exceed what is available.
/// Error strings are user-facing and name the failed rule. The
/// minimum is a parameter so an admin-set platform setting can
/// override the built-in [`MIN_PAYOUT_POISHA`] without a second
/// copy of the rules.
pub fn validate_payout_amount_with_min(
    amount_poisha: i64,
    available_poisha: i64,
    min_payout_poisha: i64,
) -> Result<(), &'static str> {
    if amount_poisha < min_payout_poisha {
        return Err("payout amount is below the minimum payout");
    }
    if amount_poisha > available_poisha {
        return Err("payout amount exceeds the available balance");
    }
    Ok(())
}

/// [`validate_payout_amount_with_min`] with the built-in platform
/// minimum ([`MIN_PAYOUT_POISHA`]).
pub fn validate_payout_amount(
    amount_poisha: i64,
    available_poisha: i64,
) -> Result<(), &'static str> {
    validate_payout_amount_with_min(amount_poisha, available_poisha, MIN_PAYOUT_POISHA)
}

// ---------------------------------------------------------------------------
// Sponsored promotions (Wave 26)
// ---------------------------------------------------------------------------

/// The longest single promotion a seller may buy, in days. A cap
/// exists so a seller cannot lock the sponsored slot of their
/// catalogue years ahead at today's price: after 30 days the
/// promotion simply expires and can be bought again at the
/// then-current price.
pub const PROMOTION_MAX_DAYS: i64 = 30;

/// The built-in per-day promotion price, in poisha (100 taka a
/// day), used when neither a platform setting nor the startup
/// `PROMOTION_PRICE_PER_DAY_POISHA` provides one. The effective
/// price resolution (setting, then startup, then this constant)
/// lives in the API's `admin` module, mirroring the commission
/// and advance settings.
pub const DEFAULT_PROMOTION_PRICE_PER_DAY_POISHA: i64 = 10_000;

/// The `platform_settings` key for the per-day promotion price
/// override (Wave 26). When a row with this key holds a valid
/// value, it beats the startup `PROMOTION_PRICE_PER_DAY_POISHA`
/// for NEW promotions; promotions already bought keep the cost
/// snapshot they were charged.
pub const PROMOTION_PRICE_PER_DAY_KEY: &str = "promotion_price_per_day_poisha";

/// Is `price` a legal per-day promotion price, in poisha? The
/// price must be positive — a free promotion is not a product,
/// and a zero or negative stored price would silently make the
/// promotion endpoint hand out sponsored placement for nothing
/// (or worse, charge a negative amount). The cap is 100,000
/// taka a day, far above any plausible rate; anything past it
/// is a data-entry error. The same rule gates the admin
/// settings endpoint, the startup value and the read-back of a
/// stored setting, so an out-of-range row is ignored rather
/// than applied.
pub fn validate_promotion_price_per_day(price_poisha: i64) -> bool {
    (1..=10_000_000).contains(&price_poisha)
}

/// Validate a requested promotion length: a whole number of
/// days between 1 and [`PROMOTION_MAX_DAYS`]. Returns the day
/// count unchanged on success. Error strings are user-facing.
pub fn validate_promotion_days(days: i64) -> Result<i64, &'static str> {
    if days < 1 {
        return Err("a promotion must run for at least 1 day");
    }
    if days > PROMOTION_MAX_DAYS {
        return Err("a promotion may run for at most 30 days");
    }
    Ok(days)
}

/// What a promotion costs: `days` at `price_per_day_poisha`,
/// or `None` when the inputs cannot describe a real charge —
/// a non-positive price (free placement is never sold through
/// this path) or a non-positive day count, or a product that
/// overflows i64. Callers validate first, so `None` here means
/// "refuse the sale", never "charge zero".
pub fn promotion_cost_poisha(days: i64, price_per_day_poisha: i64) -> Option<i64> {
    if days <= 0 || price_per_day_poisha <= 0 {
        return None;
    }
    days.checked_mul(price_per_day_poisha)
}

/// Is a promotion in its paid window at `now`? The one activity
/// rule: the stored status must still be `active` (a cancelled
/// promotion is never active, even inside its window) and `now`
/// must lie in the half-open window [starts_at, ends_at) — a
/// promotion is live from its first second and stops exactly
/// at `ends_at`. Times are unix seconds so the rule stays pure
/// and testable; the API's SQL `is_promoted` flag and the
/// seller-facing display status both implement this same rule
/// over the stored timestamps.
pub fn promotion_is_active(
    starts_at_unix: i64,
    ends_at_unix: i64,
    status: &str,
    now_unix: i64,
) -> bool {
    status == "active" && starts_at_unix <= now_unix && now_unix < ends_at_unix
}

/// Seller subscription plans (Wave 36): three tiers a store can
/// sit on. `free` is the default every store starts on (and
/// falls back to when a paid plan lapses); `pro` and `business`
/// are prepaid from the seller's earnings balance for a
/// 30-day period — the same balance promotions draw on, through
/// the same `seller_charges` ledger, so a plan can never be
/// spent twice. A plan buys two things: a discount off the
/// platform commission rate for that store's sub-orders, and a
/// higher product cap for the store's catalogue.
pub const PLAN_FREE: &str = "free";
/// See [`PLAN_FREE`].
pub const PLAN_PRO: &str = "pro";
/// See [`PLAN_FREE`].
pub const PLAN_BUSINESS: &str = "business";

/// Every subscription plan key, cheapest first. The plans
/// catalogue endpoint iterates this; tests iterate it so a new
/// plan can never silently miss a price, discount or cap row.
pub const SUBSCRIPTION_PLANS: [&str; 3] = [PLAN_FREE, PLAN_PRO, PLAN_BUSINESS];

/// One subscription period, in days: a purchase (or a renewal)
/// covers the store for this many days from its start (or, for
/// a renewal, from the previous period's end — no paid day is
/// ever lost to a late sweep).
pub const SUBSCRIPTION_PERIOD_DAYS: i64 = 30;

/// Is this a subscription plan key the platform sells? Returns
/// the key unchanged on success. `free` is a legal target for
/// the subscribe endpoint: choosing it cancels the paid plan
/// (effective immediately, no refund — see the API module).
pub fn validate_plan_key(plan: &str) -> Result<&str, &'static str> {
    match plan.trim() {
        PLAN_FREE => Ok(PLAN_FREE),
        PLAN_PRO => Ok(PLAN_PRO),
        PLAN_BUSINESS => Ok(PLAN_BUSINESS),
        _ => Err("plan must be 'free', 'pro' or 'business'"),
    }
}

/// The commission discount a plan earns, in basis points off
/// the platform's effective rate: pro takes 100 bps (one
/// percentage point) off, business 200. Unknown keys earn no
/// discount — the function fails closed, like the role matrix,
/// so a garbled stored plan can never mint a discount.
pub fn plan_commission_discount_bps(plan: &str) -> i64 {
    match plan {
        PLAN_PRO => 100,
        PLAN_BUSINESS => 200,
        _ => 0,
    }
}

/// The built-in product cap of the free plan, used when
/// neither a platform setting nor the startup
/// `PLAN_FREE_PRODUCT_CAP` provides one. The free cap is the
/// one tunable cap (see [`PLAN_FREE_PRODUCT_CAP_KEY`]): it is
/// the platform's main free-tier lever, so ops may need to
/// move it without a redeploy.
pub const DEFAULT_FREE_PLAN_PRODUCT_CAP: i64 = 50;

/// How many products a store on `plan` may hold. The count
/// rule itself lives with the API (every product row counts —
/// products are never hard-deleted, so drafts and archived
/// products hold their slot; see the API's products module).
/// Business is unlimited, expressed as `i64::MAX` so the
/// comparison `count >= cap` simply never fires. Unknown keys
/// get the free cap — failing closed again.
pub fn plan_product_cap(plan: &str) -> i64 {
    match plan {
        PLAN_PRO => 500,
        PLAN_BUSINESS => i64::MAX,
        _ => DEFAULT_FREE_PLAN_PRODUCT_CAP,
    }
}

/// The built-in monthly price of the pro plan, in poisha
/// (499 taka), used when neither a platform setting nor the
/// startup `SUBSCRIPTION_PRICE_PRO_POISHA` provides one. The
/// effective price resolution (setting, then startup, then
/// this constant) lives in the API's `admin` module, mirroring
/// the promotion price.
pub const DEFAULT_SUBSCRIPTION_PRICE_PRO_POISHA: i64 = 49_900;

/// The built-in monthly price of the business plan, in poisha
/// (1,499 taka). See [`DEFAULT_SUBSCRIPTION_PRICE_PRO_POISHA`].
pub const DEFAULT_SUBSCRIPTION_PRICE_BUSINESS_POISHA: i64 = 149_900;

/// The `platform_settings` key for the pro plan's price
/// override (Wave 36). When a row with this key holds a valid
/// value, it beats the startup `SUBSCRIPTION_PRICE_PRO_POISHA`
/// for NEW subscriptions and renewals; a period already bought
/// keeps the price it was charged.
pub const SUBSCRIPTION_PRICE_PRO_KEY: &str = "subscription_price_pro_poisha";

/// The `platform_settings` key for the business plan's price
/// override (Wave 36). See [`SUBSCRIPTION_PRICE_PRO_KEY`].
pub const SUBSCRIPTION_PRICE_BUSINESS_KEY: &str = "subscription_price_business_poisha";

/// The `platform_settings` key for the free plan's product cap
/// override (Wave 36). When a row with this key holds a valid
/// value, it beats the startup `PLAN_FREE_PRODUCT_CAP` (and
/// the built-in [`DEFAULT_FREE_PLAN_PRODUCT_CAP`]) for stores
/// on the free plan.
pub const PLAN_FREE_PRODUCT_CAP_KEY: &str = "plan_free_product_cap";

/// Is `price` a legal subscription price, in poisha? Positive
/// — a paid plan at zero would hand out commission discounts
/// for nothing — and at most 100,000 taka a month, far above
/// any plausible tier; anything past it is a data-entry error.
/// The same rule gates the admin settings endpoint, the
/// startup values and the read-back of a stored setting, so an
/// out-of-range row is ignored rather than applied.
pub fn validate_subscription_price(price_poisha: i64) -> bool {
    (1..=10_000_000).contains(&price_poisha)
}

/// Is `cap` a legal free-plan product cap? Zero is allowed (a
/// deliberate freeze on new free-plan products); the ceiling
/// is a million products, far beyond any real catalogue, so
/// anything past it is a data-entry error. Same gating
/// discipline as [`validate_subscription_price`].
pub fn validate_plan_free_product_cap(cap: i64) -> bool {
    (0..=1_000_000).contains(&cap)
}

/// The built-in fulfillment SLA for confirming an order, in
/// hours (Wave 40): a placed sub-order should be confirmed
/// within a day. Used when neither a platform setting nor the
/// startup `FULFILLMENT_CONFIRM_SLA_HOURS` provides one; the
/// effective resolution (setting, then startup, then this
/// constant) lives in the API's `fulfillment` module.
pub const DEFAULT_FULFILLMENT_CONFIRM_SLA_HOURS: i64 = 24;

/// The built-in fulfillment SLA for booking a courier once an
/// order is confirmed, in hours (Wave 40): two days. See
/// [`DEFAULT_FULFILLMENT_CONFIRM_SLA_HOURS`].
pub const DEFAULT_FULFILLMENT_SHIP_SLA_HOURS: i64 = 48;

/// The `platform_settings` key for the confirm-SLA override
/// (Wave 40). When a row with this key holds a valid value, it
/// beats the startup `FULFILLMENT_CONFIRM_SLA_HOURS`.
pub const FULFILLMENT_CONFIRM_SLA_HOURS_KEY: &str = "fulfillment_confirm_sla_hours";

/// The `platform_settings` key for the ship-SLA override
/// (Wave 40). See [`FULFILLMENT_CONFIRM_SLA_HOURS_KEY`].
pub const FULFILLMENT_SHIP_SLA_HOURS_KEY: &str = "fulfillment_ship_sla_hours";

/// Is `hours` a legal fulfillment SLA length? At least one
/// hour (a zero or negative SLA would flag every item the
/// moment it is placed) and at most thirty days (720 hours —
/// beyond that the "SLA" is not a promise anyone works to).
/// The same rule gates the startup values and the read-back
/// of a stored setting, so an out-of-range row is ignored
/// rather than applied.
pub fn validate_fulfillment_sla_hours(hours: i64) -> bool {
    (1..=720).contains(&hours)
}

/// The `platform_settings` key for the commission rate override
/// (Wave 10). When a row with this key holds a valid value, it
/// beats the startup `COMMISSION_RATE_BPS` at checkout.
pub const COMMISSION_BPS_KEY: &str = "commission_bps";

/// The `platform_settings` key for the minimum payout override
/// (Wave 10). When a row with this key holds a valid value, it
/// beats [`MIN_PAYOUT_POISHA`] for new payout requests.
pub const MIN_PAYOUT_KEY: &str = "min_payout_poisha";

/// Is `bps` a legal platform commission rate, in basis points?
/// Zero is allowed (a commission-free promotion is a legitimate
/// platform decision); the cap is 3,000 bps = 30% — above that a
/// "commission" is a data-entry error, not a business model. The
/// same rule gates the admin settings endpoint and the read-back
/// of a stored setting, so an out-of-range row is ignored rather
/// than applied.
pub fn validate_commission_bps(bps: i64) -> bool {
    (0..=3_000).contains(&bps)
}

/// Parse a stored platform setting into an integer: surrounding
/// whitespace is ignored, anything else that is not a plain
/// base-10 integer (empty, decimal point, letters, overflow) is
/// `None`. Settings are stored as text so future non-numeric
/// settings share the table; readers treat `None` as "no override"
/// and fall back to the startup value.
pub fn parse_setting_i64(raw: &str) -> Option<i64> {
    raw.trim().parse::<i64>().ok()
}

/// The `platform_settings` key for the COD advance percentage
/// override (Wave 17). When a row with this key holds a valid
/// value, it beats the startup `ADVANCE_PERCENT_BPS` at checkout.
pub const ADVANCE_PERCENT_BPS_KEY: &str = "advance_percent_bps";

/// The `platform_settings` key for the COD advance threshold
/// override (Wave 17). When a row with this key holds a valid
/// value, it beats the startup `ADVANCE_THRESHOLD_POISHA` at
/// checkout. A threshold of 0 disables threshold-based advances
/// (risk-forced advances still apply).
pub const ADVANCE_THRESHOLD_KEY: &str = "advance_threshold_poisha";

/// Is `bps` a legal COD advance rate, in basis points? The full
/// range is legal: 0 means "no advance is ever taken" (a
/// legitimate platform decision), and 10,000 means the buyer
/// prepays the whole order (at which point it is barely COD any
/// more, but it is still the platform's call). Anything outside
/// 0..=10,000 is a data-entry error. The same rule gates the
/// admin settings endpoint and the read-back of a stored
/// setting, so an out-of-range row is ignored rather than
/// applied.
pub fn validate_advance_bps(bps: i64) -> bool {
    (0..=10_000).contains(&bps)
}

/// Is `threshold` a legal COD advance threshold, in poisha? Any
/// non-negative amount is legal — including 0, which disables
/// threshold-based advances. A negative threshold is meaningless
/// and rejected.
pub fn validate_advance_threshold(threshold_poisha: i64) -> bool {
    threshold_poisha >= 0
}

/// How much advance (prepayment) a COD order must collect before
/// it ships, in poisha. The pure money rule for Wave 17:
///
/// The advance is `bps` basis points of the order's grand total,
/// rounded UP (a partial poisha of advance is collected whole).
/// It applies only when the order reaches the threshold — a
/// grand total strictly below `threshold_poisha` needs no
/// advance — unless `force` is set (the buyer's risk profile
/// demands an advance regardless of size, see
/// [`assess_buyer_risk`]). A threshold of 0 disables the
/// threshold rule entirely (only forced advances remain), and a
/// rate of 0 disables advances altogether. The result never
/// exceeds the grand total and is never negative.
pub fn compute_advance_poisha(
    grand_total_poisha: i64,
    bps: u32,
    threshold_poisha: i64,
    force: bool,
) -> i64 {
    if grand_total_poisha <= 0 || bps == 0 {
        return 0;
    }
    if !force && (threshold_poisha <= 0 || grand_total_poisha < threshold_poisha) {
        return 0;
    }
    // Ceiling division in i128 so a large total times a large
    // rate cannot overflow before the division.
    let advance = (i128::from(grand_total_poisha) * i128::from(bps) + 9_999) / 10_000;
    advance.min(i128::from(grand_total_poisha)) as i64
}

/// A buyer's platform track record, counted over their orders
/// and sub-orders. The single input shape for
/// [`assess_buyer_risk`]: the API aggregates these counts in one
/// query, and the pure rule below decides what they mean — the
/// SQL never embeds the risk policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BuyerRiskInputs {
    /// Every order the buyer has placed.
    pub total_orders: i64,
    /// Sub-orders that reached the buyer (`delivered`).
    pub delivered: i64,
    /// Sub-orders that came back (`returned`: courier RTO or a
    /// received buyer return).
    pub returned_rto: i64,
    /// Sub-orders cancelled as part of a buyer-cancelled order.
    /// (A seller cancelling one sub-order of a live order does
    /// not count against the buyer.)
    pub buyer_cancelled: i64,
}

/// How risky a buyer looks to the COD fraud shield.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuyerRiskLevel {
    Low,
    Medium,
    High,
}

impl BuyerRiskLevel {
    /// The snake_case string the API returns and the web app
    /// renders (`low`, `medium`, `high`).
    pub fn as_str(&self) -> &'static str {
        match self {
            BuyerRiskLevel::Low => "low",
            BuyerRiskLevel::Medium => "medium",
            BuyerRiskLevel::High => "high",
        }
    }
}

/// The fraud shield's verdict for one buyer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuyerRisk {
    pub level: BuyerRiskLevel,
    /// True when every COD order from this buyer must collect
    /// an advance, whatever its size.
    pub requires_advance: bool,
    /// Returned share of completed sub-orders (delivered +
    /// returned), in basis points; 0 when nothing completed.
    pub return_rate_bps: i64,
}

/// Assess a buyer from their track record. The rules, in order:
///
/// `requires_advance` is set when the buyer has at least 2
/// returned sub-orders AND returns make up at least half of
/// their completed sub-orders (delivered + returned) — a
/// pattern of ordering with no intention to accept delivery.
///
/// The level is `High` when an advance is required, or when the
/// buyer has placed at least 3 orders without a single delivery
/// while racking up at least 2 cancellations/returns (nothing
/// they order ever arrives, by their own doing). It is `Medium`
/// when any return or buyer cancellation exists at all, and
/// `Low` otherwise — a clean record, including a brand-new
/// buyer's empty one.
pub fn assess_buyer_risk(inputs: BuyerRiskInputs) -> BuyerRisk {
    let completed = inputs.delivered + inputs.returned_rto;
    let return_rate_bps = if completed > 0 {
        inputs.returned_rto * 10_000 / completed
    } else {
        0
    };
    let requires_advance = inputs.returned_rto >= 2 && return_rate_bps >= 5_000;
    let level = if requires_advance
        || (inputs.total_orders >= 3
            && inputs.delivered == 0
            && inputs.buyer_cancelled + inputs.returned_rto >= 2)
    {
        BuyerRiskLevel::High
    } else if inputs.returned_rto > 0 || inputs.buyer_cancelled > 0 {
        BuyerRiskLevel::Medium
    } else {
        BuyerRiskLevel::Low
    };
    BuyerRisk {
        level,
        requires_advance,
        return_rate_bps,
    }
}

/// Maximum length of a conversation message body, in characters.
pub const MAX_MESSAGE_BODY_CHARS: usize = 2_000;

/// Validate a conversation message body (buyer, seller and AI
/// messages alike). A message must say something: 1 to
/// [`MAX_MESSAGE_BODY_CHARS`] characters after trimming, counted
/// in Unicode characters, not bytes.
pub fn validate_message_body(body: &str) -> Result<(), &'static str> {
    let len = body.trim().chars().count();
    if len == 0 {
        return Err("message body must not be empty");
    }
    if len > MAX_MESSAGE_BODY_CHARS {
        return Err("message body must be at most 2000 characters");
    }
    Ok(())
}

/// The canonical string a SIDRA bridge signature covers, in ONE
/// place so the signer (SIDRA, and the platform's outbound
/// notifier) and the verifier (the platform's callback endpoint)
/// can never drift apart:
///
/// `<conversation_id>:<message_id>:<text>`
///
/// In the inbound callback, `message_id` is the buyer message the
/// AI reply answers (`in_reply_to`) and `text` is the reply text.
/// In the outbound notification, `message_id` is the buyer message
/// just posted and `text` is its body. Ids are the canonical
/// hyphenated UUID strings.
pub fn sidra_signature_payload(conversation_id: &str, message_id: &str, text: &str) -> String {
    format!("{conversation_id}:{message_id}:{text}")
}

/// Maximum length of a Facebook-import draft title, in characters.
pub const MAX_FB_TITLE_CHARS: usize = 120;

/// Maximum length of a Facebook-import draft description, in
/// characters.
pub const MAX_FB_DESCRIPTION_CHARS: usize = 2_000;

/// Sanity ceiling for a price extracted from free text, in poisha
/// (100,000,000 taka). A labelled number above this is far more
/// likely a reference or account number than a product price, so
/// the extractor skips it instead of inventing an absurd price.
pub const MAX_EXTRACTED_PRICE_POISHA: i64 = 10_000_000_000;

/// Convert Bengali digits (০-৯) in `text` to ASCII digits (0-9);
/// every other character is left untouched. Bangladeshi sellers
/// write prices with Bengali digits in Facebook posts, so the
/// text parsers in this module run their input through here first.
pub fn bn_to_en_digits(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '০' => '0',
            '১' => '1',
            '২' => '2',
            '৩' => '3',
            '৪' => '4',
            '৫' => '5',
            '৬' => '6',
            '৭' => '7',
            '৮' => '8',
            '৯' => '9',
            other => other,
        })
        .collect()
}

/// Does `text` end with a currency marker (`tk` / `bdt`, ASCII
/// case-insensitive) at a word boundary — the character before the
/// marker, when there is one, must not be an ASCII letter, so a
/// word that merely ENDS in the marker's letters does not count?
fn ends_with_ascii_marker(text: &str, marker: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if !lower.ends_with(marker) {
        return false;
    }
    let before = &lower[..lower.len() - marker.len()];
    before.chars().last().is_none_or(|c| !c.is_ascii_alphabetic())
}

/// Does `text` start with a currency marker (`tk` / `bdt`, ASCII
/// case-insensitive) at a word boundary — the character after the
/// marker, when there is one, must not be an ASCII letter?
fn starts_with_ascii_marker(text: &str, marker: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if !lower.starts_with(marker) {
        return false;
    }
    lower[marker.len()..]
        .chars()
        .next()
        .is_none_or(|c| !c.is_ascii_alphabetic())
}

/// Is the number token at `start..end` in `text` labelled as a
/// price? A token is labelled when a currency marker sits directly
/// before or after it (whitespace between marker and number is
/// allowed), or when its line opens with a price label — দাম or
/// "price" — followed only by separators. A bare number with no
/// marker is NEVER a price: phone numbers, stock counts and dates
/// must not turn into prices.
fn is_labelled_price(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].trim_end();
    if before.ends_with('৳') || before.ends_with("টাকা") {
        return true;
    }
    if ends_with_ascii_marker(before, "tk") || ends_with_ascii_marker(before, "bdt") {
        return true;
    }
    let after = text[end..].trim_start();
    if after.starts_with('৳') || after.starts_with("টাকা") {
        return true;
    }
    if starts_with_ascii_marker(after, "tk") || starts_with_ascii_marker(after, "bdt") {
        return true;
    }
    // Line-leading label: the text from the start of this line up
    // to the number is exactly দাম or "price" plus separators.
    let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let prefix = text[line_start..start].trim_end_matches([' ', '\t', ':', 'ঃ', '-', '=']);
    prefix == "দাম" || prefix.eq_ignore_ascii_case("price")
}

/// Extract a price, in poisha, from free post text. Bengali digits
/// are converted first. A price is a number carrying a currency
/// marker — `৳`, `টাকা`, `tk` or `BDT` adjacent to the number on
/// either side — or the first number on a line that opens with
/// দাম / "price". Numbers may group thousands with commas
/// (`1,500`) and may carry a one- or two-digit decimal part
/// (`1250.50`). When several labelled prices exist, the FIRST one
/// in the text wins. Labelled candidates above
/// [`MAX_EXTRACTED_PRICE_POISHA`] are skipped as implausible.
/// Returns `None` when the text has no labelled price.
pub fn extract_price_poisha(text: &str) -> Option<i64> {
    let converted = bn_to_en_digits(text);
    let bytes = converted.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // One number token: digits and commas, then an optional
        // decimal point with one or two fraction digits.
        let start = i;
        let mut end = i;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b',') {
            end += 1;
        }
        let mut int_end = end;
        while int_end > start && bytes[int_end - 1] == b',' {
            int_end -= 1;
        }
        let mut token_end = int_end;
        let mut fraction_poisha: i64 = 0;
        if int_end < bytes.len() && bytes[int_end] == b'.' {
            let frac_start = int_end + 1;
            let mut frac_end = frac_start;
            while frac_end < bytes.len()
                && bytes[frac_end].is_ascii_digit()
                && frac_end - frac_start < 2
            {
                frac_end += 1;
            }
            if frac_end > frac_start {
                token_end = frac_end;
                let digits = &converted[frac_start..frac_end];
                if let Ok(value) = digits.parse::<i64>() {
                    fraction_poisha = if digits.len() == 1 { value * 10 } else { value };
                }
            }
        }
        let int_digits: String = converted[start..int_end]
            .chars()
            .filter(|c| *c != ',')
            .collect();
        let candidate = int_digits
            .parse::<i64>()
            .ok()
            .and_then(|taka| taka.checked_mul(100))
            .and_then(|poisha| poisha.checked_add(fraction_poisha));
        if let Some(poisha) = candidate {
            if poisha <= MAX_EXTRACTED_PRICE_POISHA
                && is_labelled_price(&converted, start, token_end)
            {
                return Some(poisha);
            }
        }
        i = token_end.max(end);
    }
    None
}

/// Truncate `text` to at most `max_chars` Unicode characters.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// A product draft parsed from one Facebook post (Wave 12). The
/// title is the post's first non-empty line; the description is
/// the whole post; the price comes from
/// [`extract_price_poisha`] and is `None` when the post names no
/// labelled price — the seller then fills it in before import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FbProductDraft {
    pub title: String,
    pub description: String,
    pub price_poisha: Option<i64>,
}

/// Parse one Facebook post's text into a product draft. Pure: the
/// import preview and its tests run this without any database.
/// Title and description are truncated to
/// [`MAX_FB_TITLE_CHARS`] / [`MAX_FB_DESCRIPTION_CHARS`]
/// characters so a pasted post can never exceed the draft shape.
pub fn draft_from_fb_post(text: &str) -> FbProductDraft {
    let title = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| truncate_chars(line, MAX_FB_TITLE_CHARS))
        .unwrap_or_default();
    FbProductDraft {
        title,
        description: truncate_chars(text.trim(), MAX_FB_DESCRIPTION_CHARS),
        price_poisha: extract_price_poisha(text),
    }
}

// ---------------------------------------------------------------------------
// Coupons (Wave 14)
// ---------------------------------------------------------------------------

/// The two coupon kinds. A coupon's SCOPE is separate: a coupon
/// with a store is a store coupon (discounts only that store's
/// goods, borne by the seller); a coupon without one is a platform
/// coupon (discounts all goods, borne by the platform). See the
/// README's Wave 14 section for the full money design.
pub fn is_valid_coupon_discount_type(discount_type: &str) -> bool {
    matches!(discount_type, "percent" | "fixed")
}

/// Normalize a coupon code for storage and lookup: trimmed and
/// upper-cased. Codes are case-insensitive for buyers; the
/// database stores exactly this form (and its UNIQUE constraint
/// is on it).
pub fn normalize_coupon_code(code: &str) -> String {
    code.trim().to_uppercase()
}

/// Highest percent a coupon may take off (90%). A 100% coupon
/// would zero out goods while delivery and commission rules still
/// apply — a configuration error, not a promotion.
pub const MAX_COUPON_PERCENT: i64 = 90;

/// Smallest fixed-amount coupon, in poisha (1 taka). Below this a
/// "discount" is rounding noise.
pub const MIN_FIXED_COUPON_POISHA: i64 = 100;

/// Validate the fields of a coupon being created. The code is
/// checked in its normalized form: 3 to 32 characters of
/// `A-Z 0-9 _ -`. A percent coupon's value is 1..=90; a fixed
/// coupon's value is at least [`MIN_FIXED_COUPON_POISHA`]. The
/// minimum order must be non-negative. The discount cap
/// (`max_discount_poisha`) only takes effect on percent coupons;
/// when present it must be non-negative for either kind.
/// Error strings are user-facing.
pub fn validate_coupon_input(
    code: &str,
    discount_type: &str,
    discount_value: i64,
    min_order_poisha: i64,
    max_discount_poisha: Option<i64>,
) -> Result<(), &'static str> {
    let normalized = normalize_coupon_code(code);
    if normalized.chars().count() < 3 || normalized.chars().count() > 32 {
        return Err("coupon code must be 3 to 32 characters");
    }
    if !normalized
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err("coupon code may only contain A-Z, 0-9, '_' and '-'");
    }
    match discount_type {
        "percent" => {
            if !(1..=MAX_COUPON_PERCENT).contains(&discount_value) {
                return Err("percent coupon value must be between 1 and 90");
            }
        }
        "fixed" => {
            if discount_value < MIN_FIXED_COUPON_POISHA {
                return Err("fixed coupon value must be at least 100 poisha (1 taka)");
            }
        }
        _ => return Err("discount_type must be 'percent' or 'fixed'"),
    }
    if min_order_poisha < 0 {
        return Err("min_order_poisha must not be negative");
    }
    if max_discount_poisha.is_some_and(|cap| cap < 0) {
        return Err("max_discount_poisha must not be negative");
    }
    Ok(())
}

/// The discount a coupon gives on an eligible goods subtotal, in
/// poisha. Percent: subtotal × value / 100, floored, then capped
/// by `max_discount_poisha` when one is set. Fixed: the value,
/// reduced to the subtotal when it exceeds it. Either way the
/// result never exceeds the subtotal and never goes negative;
/// a zero (or negative) eligible subtotal, and an unknown
/// discount type, give zero.
pub fn compute_coupon_discount(
    discount_type: &str,
    discount_value: i64,
    max_discount_poisha: Option<i64>,
    eligible_subtotal_poisha: i64,
) -> i64 {
    if eligible_subtotal_poisha <= 0 {
        return 0;
    }
    let raw = match discount_type {
        "percent" => {
            let percent_off = eligible_subtotal_poisha * discount_value / 100;
            match max_discount_poisha {
                Some(cap) => percent_off.min(cap),
                None => percent_off,
            }
        }
        "fixed" => discount_value,
        _ => return 0,
    };
    raw.clamp(0, eligible_subtotal_poisha)
}

/// Split an order-level discount across groups, proportional to
/// each group's goods subtotal: every group gets the floor of its
/// exact share, and the leftover poisha go one at a time to the
/// largest subtotals first (ties: the earliest group), never
/// pushing a group above its own subtotal. The shares therefore
/// sum EXACTLY to the discount — the property checkout's money
/// maths relies on. A zero discount, no groups, or all-zero
/// subtotals give all-zero shares; a discount above the subtotal
/// sum is clamped to it (checkout never produces one — the
/// discount comes from [`compute_coupon_discount`] — the clamp
/// only keeps this function total).
pub fn allocate_discount(subtotals: &[i64], discount: i64) -> Vec<i64> {
    let mut shares = vec![0i64; subtotals.len()];
    let total: i64 = subtotals.iter().sum();
    if discount <= 0 || total <= 0 {
        return shares;
    }
    let discount = discount.min(total);
    let mut allocated = 0i64;
    for (i, subtotal) in subtotals.iter().enumerate() {
        // i128 keeps subtotal * discount exact for any realistic
        // poisha figures before the floor division.
        shares[i] = (i128::from(*subtotal) * i128::from(discount) / i128::from(total)) as i64;
        allocated += shares[i];
    }
    // Leftover distribution order: largest subtotal first, ties
    // keep the original order. One pass always suffices: each
    // floor loses less than one poisha, so the remainder is
    // smaller than the number of groups, and whenever a remainder
    // exists every non-zero group still has headroom (a group at
    // its subtotal would mean discount == total, whose shares are
    // exact and leave no remainder).
    let mut order: Vec<usize> = (0..subtotals.len()).collect();
    order.sort_by(|&a, &b| subtotals[b].cmp(&subtotals[a]).then(a.cmp(&b)));
    let mut remainder = discount - allocated;
    for idx in order {
        if remainder == 0 {
            break;
        }
        if shares[idx] < subtotals[idx] {
            shares[idx] += 1;
            remainder -= 1;
        }
    }
    debug_assert_eq!(remainder, 0, "allocation must sum to the discount");
    shares
}

// ---------------------------------------------------------------------------
// Loyalty points (Wave 35)
// ---------------------------------------------------------------------------

/// Net goods (goods minus the coupon discount, in poisha) that
/// earn ONE loyalty point: a buyer earns 1 point per 100 taka
/// (10_000 poisha) of net goods on each DELIVERED sub-order.
pub const LOYALTY_POISHA_PER_POINT: i64 = 10_000;

/// What one loyalty point is worth when redeemed at checkout,
/// in poisha: 1 point = 1 taka (100 poisha).
pub const LOYALTY_POINT_VALUE_POISHA: i64 = 100;

/// Days an earned point stays spendable. Expiry is computed at
/// read time from the earn row's `expires_at` — no sweeper job
/// deletes or rewrites expired rows.
pub const LOYALTY_EXPIRY_DAYS: i64 = 365;

/// Points earned by one delivered sub-order whose net goods
/// (goods subtotal minus its coupon discount share) are
/// `net_goods_poisha`: one point per full
/// [`LOYALTY_POISHA_PER_POINT`] poisha, floored — a sub-order
/// below 100 taka of net goods earns nothing, and a negative
/// net (never produced by checkout; the clamp only keeps this
/// function total) earns nothing either.
pub fn loyalty_points_for_goods(net_goods_poisha: i64) -> i64 {
    if net_goods_poisha <= 0 {
        return 0;
    }
    net_goods_poisha / LOYALTY_POISHA_PER_POINT
}

/// The checkout discount a redemption of `points` buys, in
/// poisha: points times [`LOYALTY_POINT_VALUE_POISHA`]. `None`
/// on a negative point count or on overflow — the caller
/// treats both as "no redemption" rather than wrapping into a
/// bogus discount.
pub fn loyalty_discount_poisha(points: i64) -> Option<i64> {
    if points < 0 {
        return None;
    }
    points.checked_mul(LOYALTY_POINT_VALUE_POISHA)
}

/// The most points a buyer may redeem on one order: their
/// balance, capped so the redemption discount never exceeds
/// HALF the order's post-coupon goods total (delivery is never
/// discounted by points, and at least half the goods are
/// always paid with money). The cap is floored at each step —
/// half the goods, floored, then whole points only — so the
/// resulting discount can never cross the half-goods line.
pub fn loyalty_max_redeemable_points(balance_points: i64, post_coupon_goods_poisha: i64) -> i64 {
    if balance_points <= 0 || post_coupon_goods_poisha <= 0 {
        return 0;
    }
    let cap_points = (post_coupon_goods_poisha / 2) / LOYALTY_POINT_VALUE_POISHA;
    balance_points.min(cap_points)
}

/// Validate a checkout loyalty request. Zero is a valid
/// request (it redeems nothing, exactly like omitting the
/// field); a negative count is a client error. There is no
/// upper-bound error: an over-large request is CLAMPED to the
/// redeemable maximum by [`loyalty_max_redeemable_points`],
/// never rejected — the checkout response reports the points
/// actually used.
pub fn validate_loyalty_request(requested_points: i64) -> Result<(), &'static str> {
    if requested_points < 0 {
        return Err("loyalty_points must not be negative");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Search (Wave 15)
// ---------------------------------------------------------------------------

/// Maximum length of a product category label, in characters.
pub const MAX_CATEGORY_CHARS: usize = 60;

/// Validate a product category label. The label is OPTIONAL: `None`
/// is valid and means "uncategorised", and a blank label is valid
/// too (callers normalize blank to `None`). A present label may be
/// at most [`MAX_CATEGORY_CHARS`] characters, counted after
/// trimming, in Unicode characters, not bytes.
pub fn validate_category(category: Option<&str>) -> Result<(), &'static str> {
    match category {
        None => Ok(()),
        Some(raw) => {
            if raw.trim().chars().count() > MAX_CATEGORY_CHARS {
                Err("category must be at most 60 characters")
            } else {
                Ok(())
            }
        }
    }
}

/// Maximum search text length, in characters.
pub const MAX_SEARCH_QUERY_CHARS: usize = 100;

/// Default search page size when the caller omits `per_page`.
pub const DEFAULT_SEARCH_PER_PAGE: i64 = 20;

/// Largest search page size (search pages stay small enough to
/// rank and render quickly).
pub const MAX_SEARCH_PER_PAGE: i64 = 48;

/// The sort orders product search understands. The API layer maps
/// each variant to a fixed ORDER BY fragment — user input never
/// reaches the SQL text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    /// Best match first (only meaningful with search text; the
    /// validator falls back to it only when text is present).
    Relevance,
    /// Cheapest first.
    PriceAsc,
    /// Most expensive first.
    PriceDesc,
    /// Most recently created first.
    Newest,
    /// Highest rated first (unreviewed products last).
    Rating,
}

/// Validated product-search parameters (Wave 15). The API layer
/// hands the raw query-string values to [`validate_search_params`]
/// and then works only with this struct, so the search rules live
/// in exactly one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchParams {
    /// The trimmed search text, or `None` for a pure filter browse.
    pub q: Option<String>,
    /// Inclusive lower price bound in poisha, when set.
    pub min_price_poisha: Option<i64>,
    /// Inclusive upper price bound in poisha, when set.
    pub max_price_poisha: Option<i64>,
    pub sort: SearchSort,
    /// 1-based page number.
    pub page: i64,
    /// Page size, 1..=[`MAX_SEARCH_PER_PAGE`].
    pub per_page: i64,
}

/// Validate raw product-search parameters. Rules: the text is
/// trimmed, blank means absent, and at most
/// [`MAX_SEARCH_QUERY_CHARS`] characters; price bounds are
/// non-negative and, when both are set, min must not exceed max;
/// the sort defaults to relevance when text is present and to
/// newest otherwise; the page is 1-based; the page size is
/// 1..=[`MAX_SEARCH_PER_PAGE`], defaulting to
/// [`DEFAULT_SEARCH_PER_PAGE`]. Error strings are user-facing.
pub fn validate_search_params(
    q: Option<&str>,
    min_price_poisha: Option<i64>,
    max_price_poisha: Option<i64>,
    sort: Option<&str>,
    page: Option<i64>,
    per_page: Option<i64>,
) -> Result<SearchParams, &'static str> {
    let q = match q {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else if trimmed.chars().count() > MAX_SEARCH_QUERY_CHARS {
                return Err("search text must be at most 100 characters");
            } else {
                Some(trimmed.to_string())
            }
        }
        None => None,
    };
    if min_price_poisha.is_some_and(|min| min < 0) || max_price_poisha.is_some_and(|max| max < 0)
    {
        return Err("price bounds must not be negative");
    }
    if let (Some(min), Some(max)) = (min_price_poisha, max_price_poisha) {
        if min > max {
            return Err("min_price_poisha must not exceed max_price_poisha");
        }
    }
    let sort = match sort {
        None => {
            if q.is_some() {
                SearchSort::Relevance
            } else {
                SearchSort::Newest
            }
        }
        Some("relevance") => SearchSort::Relevance,
        Some("price_asc") => SearchSort::PriceAsc,
        Some("price_desc") => SearchSort::PriceDesc,
        Some("newest") => SearchSort::Newest,
        Some("rating") => SearchSort::Rating,
        Some(_) => {
            return Err("sort must be one of: relevance, price_asc, price_desc, newest, rating")
        }
    };
    let page = page.unwrap_or(1);
    if page < 1 {
        return Err("page must be at least 1");
    }
    let per_page = per_page.unwrap_or(DEFAULT_SEARCH_PER_PAGE);
    if !(1..=MAX_SEARCH_PER_PAGE).contains(&per_page) {
        return Err("per_page must be between 1 and 48");
    }
    Ok(SearchParams {
        q,
        min_price_poisha,
        max_price_poisha,
        sort,
        page,
        per_page,
    })
}

/// Escape the SQL LIKE metacharacters in a search text: each
/// backslash, percent and underscore gains a backslash prefix
/// (the queries rely on the Postgres default `ESCAPE '\'`).
/// Keeping this in the domain crate means every ILIKE caller
/// escapes the same way — user text can never widen its own match.
fn escape_like_text(q: &str) -> String {
    let mut escaped = String::with_capacity(q.len());
    for ch in q.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// Build the contains-match ILIKE pattern for a search text: the
/// text escaped by [`escape_like_text`], wrapped in `%...%`.
pub fn search_like_pattern(q: &str) -> String {
    format!("%{}%", escape_like_text(q))
}

/// Build the prefix-match ILIKE pattern for a search text: the
/// text escaped by [`escape_like_text`], with a single trailing
/// `%`. Used by the search rank's title-prefix boost.
pub fn search_prefix_pattern(q: &str) -> String {
    format!("{}%", escape_like_text(q))
}

/// Maximum length of a buyer's return reason, in characters.
pub const MAX_RETURN_REASON_CHARS: usize = 500;

/// Return-request lifecycle statuses — mirrors the DB CHECK
/// constraint on `return_requests.status` (migration 0018). The
/// refund has its OWN status field (`none` / `pending` / `done`)
/// on the same row: a request can be `received` while its refund
/// is still `pending`, and conflating the two would force fake
/// states like "received_but_unpaid" into this set.
pub fn is_valid_return_status(status: &str) -> bool {
    matches!(status, "requested" | "approved" | "rejected" | "received")
}

/// May the buyer open a return request for a sub-order in this
/// status? Only `delivered`: a return is the buyer sending goods
/// back, and goods can only come back after they arrived. A
/// not-yet-delivered parcel is handled by cancellation (before
/// shipping) or by the courier's own return path (RTO) — never by
/// a buyer return request.
pub fn can_request_return(sub_order_status: &str) -> bool {
    sub_order_status == "delivered"
}

/// May a return request move from `from` to `to`? The single
/// return transition table (Wave 16): the seller decides a fresh
/// request (`requested` → `approved` or `rejected`), and only an
/// approved request can be marked `received` when the goods come
/// back. `rejected` and `received` are terminal for the REQUEST;
/// the refund's own lifecycle continues on its separate field.
/// Same-state moves and skips (e.g. `requested` → `received`) are
/// rejected — the seller must approve before goods are accepted
/// back, so a request can never jump the decision.
pub fn can_transition_return(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("requested", "approved") | ("requested", "rejected") | ("approved", "received")
    )
}

/// The refund owed when a returned sub-order is received back:
/// the goods the buyer paid for, minus any coupon discount that
/// was allocated to the sub-order (the buyer never paid that
/// part, so it is not refunded). The DELIVERY charge is never
/// part of a refund — the courier trip happened and was paid
/// for, whichever way the parcel travelled; refunding it would
/// make the platform pay the courier twice for one delivery.
/// Clamped at zero so a corrupt snapshot can never produce a
/// negative refund.
pub fn compute_refund_poisha(goods_poisha: i64, discount_poisha: i64) -> i64 {
    (goods_poisha - discount_poisha).max(0)
}

/// Validate a buyer's return reason: required (a seller cannot
/// judge a request with no reason), 1 to
/// [`MAX_RETURN_REASON_CHARS`] characters after trimming.
pub fn validate_return_reason(reason: &str) -> Result<(), &'static str> {
    let len = reason.trim().chars().count();
    if len == 0 {
        return Err("return reason must not be empty");
    }
    if len > MAX_RETURN_REASON_CHARS {
        return Err("return reason must be at most 500 characters");
    }
    Ok(())
}

/// Does a phone number supplied to the PUBLIC tracking endpoint
/// match the phone stored on the order? The public endpoint has
/// no login: the order id plus the delivery phone ARE the
/// credential, so this comparison is the whole gate. Both sides
/// are normalized with [`normalize_bd_phone`] first, so the
/// local form (`01...`), the `880...` form and the canonical
/// `+880...` form all match each other. An invalid supplied
/// number simply does not match — the function returns `false`
/// rather than an error, so the endpoint can answer every
/// mismatch (wrong number, malformed number, unknown order)
/// with the same indistinguishable 404.
pub fn phones_match_for_tracking(stored_phone: &str, supplied_phone: &str) -> bool {
    match (
        normalize_bd_phone(stored_phone),
        normalize_bd_phone(supplied_phone),
    ) {
        (Some(stored), Some(supplied)) => stored == supplied,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Store branding (Wave 18)
// ---------------------------------------------------------------------------

/// The storefront theme keys a seller may pick. A theme is a
/// LAYOUT choice for the storefront page, never seller-supplied
/// markup: the web app renders exactly these three variants of
/// the one shared storefront view, so a theme key can never
/// smuggle CSS or HTML into the page.
pub const THEME_KEYS: [&str; 3] = ["classic", "minimal", "bold"];

/// The theme a store renders with until its seller picks one —
/// also the database default (migration 0020).
pub const DEFAULT_THEME_KEY: &str = "classic";

/// Validate a storefront theme key: an exact match against
/// [`THEME_KEYS`] (case-sensitive — keys are machine values, not
/// display text). Returns the key itself on success so callers
/// can store the validated value.
pub fn validate_theme_key(key: &str) -> Result<&str, &'static str> {
    if THEME_KEYS.contains(&key) {
        Ok(key)
    } else {
        Err("theme_key must be one of: classic, minimal, bold")
    }
}

/// Validate a brand accent colour: exactly `#RRGGBB` (a `#` plus
/// six hex digits). Returns the colour normalized to UPPERCASE,
/// so the stored value has one canonical spelling however the
/// seller typed it (`#ff5722` and `#FF5722` are the same brand).
pub fn validate_hex_color(raw: &str) -> Result<String, &'static str> {
    let hex = raw.strip_prefix('#').unwrap_or("");
    if raw.len() == 7 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(raw.to_uppercase())
    } else {
        Err("brand_color must be a hex colour like #FF5722")
    }
}

/// Maximum length of a branding image URL, in characters.
pub const MAX_BRANDING_URL_CHARS: usize = 500;

/// Validate an optional branding image URL (store logo /
/// banner). `None` and blank input are valid and normalize to
/// `None` (no image). A present URL must be an absolute
/// `http://` or `https://` URL of at most
/// [`MAX_BRANDING_URL_CHARS`] characters with no whitespace —
/// the web app hotlinks it into an `<img>` tag, so relative
/// paths, other schemes (`ftp:`, `data:`, `javascript:`) and
/// embedded spaces are all rejected here, at the one gate every
/// write passes through.
pub fn validate_branding_url(url: Option<&str>) -> Result<Option<String>, &'static str> {
    let Some(raw) = url else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_BRANDING_URL_CHARS {
        return Err("image URL must be at most 500 characters");
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("image URL must not contain spaces");
    }
    if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") {
        return Err("image URL must start with https:// or http://");
    }
    Ok(Some(trimmed.to_string()))
}

/// Maximum length of a store announcement, in characters.
pub const MAX_ANNOUNCEMENT_CHARS: usize = 120;

/// Validate an optional store announcement (the one-line banner
/// text at the top of a storefront, e.g. an Eid offer). `None`
/// and blank input normalize to `None` (no announcement). A
/// present announcement is trimmed and may be at most
/// [`MAX_ANNOUNCEMENT_CHARS`] characters, counted in Unicode
/// characters, not bytes — Bangla text is multi-byte, and the
/// limit is about how much a visitor reads, not storage size.
pub fn validate_announcement(text: Option<&str>) -> Result<Option<String>, &'static str> {
    validate_optional_text(text, MAX_ANNOUNCEMENT_CHARS, "announcement")
}

/// Maximum length of a store description, in characters.
pub const MAX_STORE_DESCRIPTION_CHARS: usize = 1_000;

/// Validate an optional store description. Same contract as
/// [`validate_announcement`] with the larger
/// [`MAX_STORE_DESCRIPTION_CHARS`] limit: `None` and blank
/// normalize to `None`, a present text is trimmed and
/// char-counted.
pub fn validate_store_description(text: Option<&str>) -> Result<Option<String>, &'static str> {
    validate_optional_text(text, MAX_STORE_DESCRIPTION_CHARS, "description")
}

/// The shared shape behind the optional-text validators: `None`
/// and blank become `None`; a present value is trimmed, must fit
/// `max_chars` Unicode characters, and is returned owned. The
/// `label` only names the field in the error message.
fn validate_optional_text(
    text: Option<&str>,
    max_chars: usize,
    label: &'static str,
) -> Result<Option<String>, &'static str> {
    let Some(raw) = text else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > max_chars {
        return Err(match label {
            "announcement" => "announcement must be at most 120 characters",
            _ => "description must be at most 1000 characters",
        });
    }
    Ok(Some(trimmed.to_string()))
}

// ---------------------------------------------------------------------------
// Seller verification / KYC (Wave 21)
// ---------------------------------------------------------------------------

/// The verification tiers a store can hold, lowest first. A
/// tier is TRUST metadata earned through document review (see
/// the API's `verification` module), never a seller-chosen
/// label: `unverified` is every new store, `basic` means the
/// platform has seen and approved the seller's NID, and
/// `verified` means NID plus a trade license were approved.
/// The database CHECK (migration 0022) mirrors this list.
pub const VERIFICATION_TIERS: [&str; 3] = ["unverified", "basic", "verified"];

/// The tier every store starts with — also the database
/// default (migration 0022).
pub const DEFAULT_VERIFIED_TIER: &str = "unverified";

/// Rank a verification tier for comparisons (higher is more
/// trusted): `unverified` is 0, `basic` is 1, `verified` is 2.
/// An unknown string ranks 0 — it claims no trust — so a
/// corrupted value can never outrank a real tier.
pub fn tier_rank(tier: &str) -> u8 {
    match tier {
        "basic" => 1,
        "verified" => 2,
        _ => 0,
    }
}

/// Validate a verification tier: an exact match against
/// [`VERIFICATION_TIERS`] (case-sensitive — tiers are machine
/// values, not display text). Returns the tier itself on
/// success so callers can store the validated value.
pub fn validate_tier(tier: &str) -> Result<&str, &'static str> {
    if VERIFICATION_TIERS.contains(&tier) {
        Ok(tier)
    } else {
        Err("tier must be one of: unverified, basic, verified")
    }
}

/// Validate a Bangladeshi national ID (NID) number. NIDs are
/// issued in three lengths — 10, 13 and 17 digits — and sellers
/// routinely type them with spaces between groups, so spaces
/// are removed before the check; everything else must be an
/// ASCII digit. Returns the bare digit string for storage.
/// This checks the SHAPE only: no checksum exists for NIDs
/// that a platform could verify offline, and authenticity is
/// established by the human document review, never by this
/// function (the README says so plainly).
pub fn validate_nid_number(raw: &str) -> Result<String, &'static str> {
    let digits: String = raw.trim().chars().filter(|c| *c != ' ').collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err("NID number must contain digits only");
    }
    if !matches!(digits.len(), 10 | 13 | 17) {
        return Err("NID number must be 10, 13 or 17 digits");
    }
    Ok(digits)
}

/// Mask an NID number for display: every digit but the last
/// four becomes a `*`, the same convention the stored
/// `nid_last4` column supports. Inputs of four digits or fewer
/// are fully masked (there is nothing safe to show). The full
/// number is an identity document identifier: it leaves the
/// database through exactly one admin endpoint (see the API's
/// `verification` module), and every other surface — seller
/// views included — shows only this masked form.
pub fn mask_nid(nid: &str) -> String {
    let chars: Vec<char> = nid.chars().collect();
    if chars.len() <= 4 {
        return "*".repeat(chars.len());
    }
    let masked: String = chars[..chars.len() - 4].iter().map(|_| '*').collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{masked}{tail}")
}

/// Maximum length of a trade license number, in characters.
pub const MAX_TRADE_LICENSE_CHARS: usize = 40;

/// Minimum length of a trade license number, in characters.
pub const MIN_TRADE_LICENSE_CHARS: usize = 4;

/// Validate an optional trade license number. `None` and
/// blank input normalize to `None` (a seller without a trade
/// license can still reach the `basic` tier — the license is
/// what the `verified` tier adds). A present value is trimmed
/// and must be [`MIN_TRADE_LICENSE_CHARS`] to
/// [`MAX_TRADE_LICENSE_CHARS`] characters: license numbers
/// are issued by many local authorities in many formats, so
/// shape beyond length is all that can honestly be checked.
pub fn validate_trade_license(text: Option<&str>) -> Result<Option<String>, &'static str> {
    let Some(raw) = text else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let len = trimmed.chars().count();
    if !(MIN_TRADE_LICENSE_CHARS..=MAX_TRADE_LICENSE_CHARS).contains(&len) {
        return Err("trade license number must be 4 to 40 characters");
    }
    Ok(Some(trimmed.to_string()))
}

/// Verification application statuses. `none` is virtual — it
/// means the store has never applied (there is no row in
/// `store_verifications`); the stored statuses are `pending`,
/// `approved` and `rejected`.
pub fn is_valid_verification_status(status: &str) -> bool {
    matches!(status, "none" | "pending" | "approved" | "rejected")
}

/// May a verification application move from `from` to `to`?
/// The single verification transition table (Wave 21): a store
/// applies from `none` (first application) or from `rejected`
/// (resubmission after fixing what the reviewer flagged); a
/// `pending` application is decided to `approved` or
/// `rejected` by an admin; `approved` is terminal — changing
/// an approved store's tier is a fresh admin action, not an
/// application move. Same-state moves are NOT transitions: a
/// seller editing a still-`pending` application replaces it
/// in place (see the API module), which this table neither
/// needs nor allows.
pub fn can_transition_verification(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("none", "pending")
            | ("rejected", "pending")
            | ("pending", "approved")
            | ("pending", "rejected")
    )
}

/// Maximum length of an admin's verification review note, in
/// characters.
pub const MAX_REVIEWER_NOTE_CHARS: usize = 500;

/// Validate an admin's rejection note for a verification
/// application. The note is REQUIRED on rejection (a seller
/// told only "no" cannot fix the application), so blank is an
/// error here — unlike the optional text validators, this one
/// takes a plain `&str` and always returns text. The value is
/// trimmed and may be at most [`MAX_REVIEWER_NOTE_CHARS`]
/// characters, counted in Unicode characters.
pub fn validate_reviewer_note(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("a rejection note is required");
    }
    if trimmed.chars().count() > MAX_REVIEWER_NOTE_CHARS {
        return Err("reviewer note must be at most 500 characters");
    }
    Ok(trimmed.to_string())
}

/// The `platform_settings` key for the KYC payout gate
/// (Wave 21). When the row holds `on`, a store must hold at
/// least the `basic` verification tier before it can request
/// a payout; `off` (or no row) keeps the Wave 9 behaviour.
/// Stored as text (`on` / `off`) because the gate is a flag,
/// not a number — the settings table already stores text for
/// every key.
pub const KYC_PAYOUT_GATE_KEY: &str = "kyc_payout_gate";

// ---------------------------------------------------------------------------
// Invoices (Wave 22)
// ---------------------------------------------------------------------------

/// Format one store's invoice sequence number as the printed
/// invoice number: `INV-` plus the sequence zero-padded to six
/// digits (`INV-000001`). A sequence past 999_999 simply grows
/// wider — the padding is a minimum width, never a truncation.
///
/// The sequence is PER-STORE (each store has its own counter,
/// migration 0023), so two different stores can both issue an
/// `INV-000001`. An invoice is identified by the pair (store,
/// number): the printed invoice carries the store's name and
/// slug next to the number, and the database enforces
/// uniqueness on `(store_id, seq)`.
pub fn format_invoice_number(store_seq: i64) -> String {
    format!("INV-{store_seq:06}")
}

/// The filename stem for one invoice's print view:
/// `invoice-<slug>-<seq zero-padded to six digits>`. The web
/// invoice page sets it as the document title, which is what
/// browsers propose as the filename when the reader saves the
/// print view as a PDF.
pub fn invoice_filename(store_slug: &str, store_seq: i64) -> String {
    format!("invoice-{store_slug}-{store_seq:06}")
}

// ---------------------------------------------------------------------------
// Inventory & stock audit (Wave 23)
// ---------------------------------------------------------------------------

/// The low-stock threshold a product uses when the seller has
/// not set a per-product one (migration 0024 leaves
/// `products.low_stock_threshold` NULL in that case): at or
/// below this many units on hand, the product counts as low.
pub const LOW_STOCK_DEFAULT: i64 = 5;

/// The stock health of one product, from its on-hand count and
/// its low-stock threshold (the seller's own, or
/// [`LOW_STOCK_DEFAULT`] when unset):
///
/// - `out_of_stock` — nothing on hand (zero or, defensively,
///   a negative count, which the write paths refuse to create);
/// - `low` — on hand but at or below the threshold;
/// - `in_stock` — comfortably above the threshold.
pub fn stock_status(stock: i64, threshold: Option<i64>) -> &'static str {
    if stock <= 0 {
        "out_of_stock"
    } else if stock <= threshold.unwrap_or(LOW_STOCK_DEFAULT) {
        "low"
    } else {
        "in_stock"
    }
}

/// The largest single manual stock adjustment a seller may
/// make in one call (either direction). Bigger corrections
/// are almost certainly typos, and a seller can always make
/// two adjustments — each one is individually audited.
pub const MAX_STOCK_ADJUSTMENT: i64 = 100_000;

/// The reasons a SELLER may give for a manual stock
/// adjustment. System movements use their own vocabulary
/// (`order`, `cancel_restock`, `return_restock`,
/// `rto_restock`) written by the checkout / cancel / return
/// code paths themselves; those are never accepted from a
/// seller payload, so a seller can never forge a system row.
pub const STOCK_ADJUSTMENT_REASONS: [&str; 5] =
    ["restock", "correction", "damage", "count_fix", "other"];

/// Validate one manual stock adjustment: the delta must be
/// non-zero and within ±[`MAX_STOCK_ADJUSTMENT`], and the
/// reason must be one of [`STOCK_ADJUSTMENT_REASONS`]
/// (matched case-insensitively). On success the canonical
/// (lowercase) reason is returned for storage.
pub fn validate_stock_adjustment(delta: i64, reason: &str) -> Result<&'static str, &'static str> {
    if delta == 0 {
        return Err("stock adjustment delta must not be zero");
    }
    if delta.abs() > MAX_STOCK_ADJUSTMENT {
        return Err("stock adjustment delta is too large");
    }
    let lowered = reason.trim().to_lowercase();
    STOCK_ADJUSTMENT_REASONS
        .iter()
        .find(|known| **known == lowered)
        .copied()
        .ok_or("stock adjustment reason must be one of: restock, correction, damage, count_fix, other")
}

/// The highest low-stock threshold a seller may set on one
/// product. The threshold is a warning level, not a stock
/// figure, so it stays small; zero is allowed and means
/// "only warn me when the product is actually out".
pub const MAX_LOW_STOCK_THRESHOLD: i64 = 10_000;

/// Validate a per-product low-stock threshold: `None` clears
/// the override (the product falls back to
/// [`LOW_STOCK_DEFAULT`]); a value must lie in
/// 0..=[`MAX_LOW_STOCK_THRESHOLD`].
pub fn validate_low_stock_threshold(
    threshold: Option<i64>,
) -> Result<Option<i64>, &'static str> {
    match threshold {
        None => Ok(None),
        Some(value) if (0..=MAX_LOW_STOCK_THRESHOLD).contains(&value) => Ok(Some(value)),
        Some(_) => Err("low-stock threshold must be between 0 and 10000"),
    }
}

/// How many products a recommendation surface shows when
/// the caller does not ask for a specific count (Wave 24):
/// the related, also-bought and personalised rails all share
/// this one default.
pub const DEFAULT_RECOMMENDATION_LIMIT: i64 = 8;

/// The most products any recommendation surface returns in
/// one call (Wave 24). The rails are discovery aids, not
/// catalogue listings — a caller who wants the whole
/// catalogue uses the listing and search endpoints.
pub const MAX_RECOMMENDATION_LIMIT: i64 = 24;

/// Normalise a caller-supplied recommendation limit
/// (Wave 24): an absent limit means
/// [`DEFAULT_RECOMMENDATION_LIMIT`]; a supplied one clamps
/// into 1..=[`MAX_RECOMMENDATION_LIMIT`] (zero or negative
/// asks still get one product rather than an error — the
/// rail simply shows less).
pub fn recommendation_limit(limit: Option<i64>) -> i64 {
    limit
        .unwrap_or(DEFAULT_RECOMMENDATION_LIMIT)
        .clamp(1, MAX_RECOMMENDATION_LIMIT)
}

// ---------------------------------------------------------------------------
// Wave 27 — analytics windows and CSV export
// ---------------------------------------------------------------------------

/// How many days of history the analytics endpoints cover
/// when the caller does not ask for a specific window
/// (Wave 27).
pub const DEFAULT_ANALYTICS_DAYS: i64 = 30;

/// The shortest analytics window (Wave 27): a single day is
/// a snapshot, not an analysis, so the floor is a week.
pub const MIN_ANALYTICS_DAYS: i64 = 7;

/// The longest analytics window (Wave 27): analytics are
/// computed per request from the ledger tables (no rollups
/// yet), so the cap keeps a single request bounded.
pub const MAX_ANALYTICS_DAYS: i64 = 90;

/// Normalise a caller-supplied analytics window (Wave 27):
/// an absent window means [`DEFAULT_ANALYTICS_DAYS`]; a
/// supplied one clamps into
/// [`MIN_ANALYTICS_DAYS`]..=[`MAX_ANALYTICS_DAYS`] — the
/// caller is never errored for asking for too little or too
/// much history, they just get the nearest supported window.
pub fn analytics_days(days: Option<i64>) -> i64 {
    days.unwrap_or(DEFAULT_ANALYTICS_DAYS)
        .clamp(MIN_ANALYTICS_DAYS, MAX_ANALYTICS_DAYS)
}

/// Escape one CSV field (Wave 27, RFC 4180): the field is
/// wrapped in double quotes when it contains a comma, a
/// double quote, or a line break, and every internal double
/// quote is doubled. Anything else passes through untouched.
pub fn csv_escape(field: &str) -> String {
    if !field.contains([',', '"', '\n', '\r']) {
        return field.to_string();
    }
    let mut out = String::with_capacity(field.len() + 2);
    out.push('"');
    for ch in field.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// The header line of the daily analytics CSV (Wave 27).
/// The `orders` column counts SUB-ORDERS (seller slices) —
/// the seller export's day rows are sub-order rows; see
/// [`build_daily_csv`].
pub fn daily_csv_header() -> &'static str {
    "date,orders,goods_poisha,earnings_poisha"
}

/// Build the seller analytics CSV (Wave 27) from daily rows
/// of `(date, orders, goods_poisha, earnings_poisha)`: the
/// shared header, then one line per row, every field run
/// through [`csv_escape`] (dates and numbers never need it,
/// but the escaper is the one rule for every field), lines
/// joined by `\n` with a trailing newline at the end.
pub fn build_daily_csv(rows: &[(String, i64, i64, i64)]) -> String {
    let mut out = String::from(daily_csv_header());
    out.push('\n');
    for (date, orders, goods_poisha, earnings_poisha) in rows {
        out.push_str(&csv_escape(date));
        out.push(',');
        out.push_str(&csv_escape(&orders.to_string()));
        out.push(',');
        out.push_str(&csv_escape(&goods_poisha.to_string()));
        out.push(',');
        out.push_str(&csv_escape(&earnings_poisha.to_string()));
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Wave 28 — security limits (pure rules; the API enforces them)
// ---------------------------------------------------------------------------

/// Maximum number of OTP codes one phone number may REQUEST
/// per rolling hour (Wave 28). The request endpoint counts the
/// number's requests in the last hour (from the append-only
/// `otp_request_log` table) and answers 429 once this cap is
/// reached.
///
/// Why 20 and not a tighter number: the design target was 5,
/// but the integration suites share ONE Postgres database
/// across 25+ test binaries in a single CI run, and the
/// busiest fixture phone (`01799999999`) legitimately logs in
/// 9 times across those suites inside the run's hour — a cap
/// of 5 (or 10) would throttle the test suite itself. 20 per
/// hour is still a hard abuse cap: it bounds SMS cost to at
/// most 480 messages per number per day, and brute force
/// stays impractical because every issued code still expires
/// in 5 minutes and allows only 5 verify attempts against a
/// 6-digit space. Deployments that want the tighter cap can
/// lower this constant; the rule and its counting do not
/// change.
pub const OTP_REQUEST_LIMIT_PER_HOUR: i64 = 20;

/// May a phone that already requested `recent_requests` OTP
/// codes in the last hour request one more (Wave 28)? Pure
/// comparison against [`OTP_REQUEST_LIMIT_PER_HOUR`]; the API
/// counts the requests, this function decides.
pub fn otp_request_allowed(recent_requests: i64) -> bool {
    recent_requests < OTP_REQUEST_LIMIT_PER_HOUR
}

// ---------------------------------------------------------------------------
// Wave 29 — notification bodies (pure rendering; delivery is the
// notify crate's outbox + the worker)
// ---------------------------------------------------------------------------

/// Format a poisha amount as taka for human-facing text
/// (Wave 29): whole taka when the amount divides evenly
/// (`৳2060`), otherwise two decimal places (`৳10.50`). No
/// thousands grouping — the string appears in SMS bodies,
/// where plain digits are the least ambiguous.
pub fn format_taka(poisha: i64) -> String {
    let sign = if poisha < 0 { "-" } else { "" };
    let abs = poisha.unsigned_abs();
    let taka = abs / 100;
    let remainder = abs % 100;
    if remainder == 0 {
        format!("{sign}৳{taka}")
    } else {
        format!("{sign}৳{taka}.{remainder:02}")
    }
}

/// The notification kinds (Wave 29). The string values are
/// stored on `notifications.kind` and are the vocabulary the
/// admin notifications view groups by.
pub const NOTIFICATION_ORDER_PLACED: &str = "order_placed";
pub const NOTIFICATION_SUB_ORDER_SHIPPED: &str = "sub_order_shipped";
pub const NOTIFICATION_SUB_ORDER_DELIVERED: &str = "sub_order_delivered";
pub const NOTIFICATION_SUB_ORDER_CANCELLED: &str = "sub_order_cancelled";
pub const NOTIFICATION_SUB_ORDER_RETURNED: &str = "sub_order_returned";
pub const NOTIFICATION_PAYOUT_PAID: &str = "payout_paid";
pub const NOTIFICATION_RETURN_UPDATE: &str = "return_update";

/// The values a notification template renders (Wave 29).
/// Every field is optional in effect: a kind uses the fields
/// it names and ignores the rest, so one struct serves all
/// kinds. `order_id` is the full order UUID as text; the
/// templates print only its first 8 characters (the short
/// display form buyers and sellers already see in invoices).
#[derive(Debug, Clone, Default)]
pub struct NotificationVars {
    pub order_id: String,
    pub store_name: String,
    pub amount_poisha: Option<i64>,
    /// Lifecycle status for the kinds that report one
    /// (`return_update` reads `approved` / `rejected` /
    /// `received`).
    pub status: String,
}

/// The short display form of an order id: its first 8
/// characters, matching the invoice consignment prefix the
/// platform already prints.
fn short_order_id(order_id: &str) -> &str {
    let end = order_id
        .char_indices()
        .nth(8)
        .map(|(index, _)| index)
        .unwrap_or(order_id.len());
    &order_id[..end]
}

/// Render the SMS body for one notification (Wave 29). Pure:
/// the notify crate's `enqueue` calls this at write time and
/// stores the rendered body, so a queued message can never
/// change wording after the fact. Templates are Bangla and
/// deliberately short — one SMS segment-ish each. An unknown
/// kind renders a generic update line rather than panicking:
/// a body must exist for every queued row.
pub fn render_body(kind: &str, vars: &NotificationVars) -> String {
    let short = short_order_id(&vars.order_id);
    let store = vars.store_name.as_str();
    match kind {
        NOTIFICATION_ORDER_PLACED => match vars.amount_poisha {
            Some(amount) => format!(
                "BD Marketplace: আপনার অর্ডার #{short} সফলভাবে গৃহীত হয়েছে। মোট {}। ধন্যবাদ!",
                format_taka(amount)
            ),
            None => format!(
                "BD Marketplace: আপনার অর্ডার #{short} সফলভাবে গৃহীত হয়েছে। ধন্যবাদ!"
            ),
        },
        NOTIFICATION_SUB_ORDER_SHIPPED => format!(
            "BD Marketplace: {store} থেকে আপনার অর্ডার #{short}-এর পণ্য পাঠানো হয়েছে।"
        ),
        NOTIFICATION_SUB_ORDER_DELIVERED => format!(
            "BD Marketplace: আপনার অর্ডার #{short} ডেলিভারি সম্পন্ন হয়েছে। {store} থেকে কেনাকাটার জন্য ধন্যবাদ!"
        ),
        NOTIFICATION_SUB_ORDER_CANCELLED => format!(
            "BD Marketplace: দুঃখিত, {store} আপনার অর্ডার #{short} বাতিল করেছে।"
        ),
        NOTIFICATION_SUB_ORDER_RETURNED => format!(
            "BD Marketplace: আপনার অর্ডার #{short}-এর পণ্য {store}-তে ফেরত এসেছে।"
        ),
        NOTIFICATION_PAYOUT_PAID => match vars.amount_poisha {
            Some(amount) => format!(
                "BD Marketplace: আপনার {} পে-আউট পাঠানো হয়েছে।",
                format_taka(amount)
            ),
            None => "BD Marketplace: আপনার পে-আউট পাঠানো হয়েছে।".to_string(),
        },
        NOTIFICATION_RETURN_UPDATE => match vars.status.as_str() {
            "approved" => format!(
                "BD Marketplace: আপনার রিটার্ন অনুমোদিত হয়েছে ({store}, অর্ডার #{short})। পণ্যটি ফেরত পাঠিয়ে দিন।"
            ),
            "rejected" => format!(
                "BD Marketplace: আপনার রিটার্ন অনুরোধটি গৃহীত হয়নি ({store}, অর্ডার #{short})।"
            ),
            "received" => match vars.amount_poisha {
                Some(amount) => format!(
                    "BD Marketplace: আপনার ফেরত পণ্য {store} পেয়েছে (অর্ডার #{short})। রিফান্ড {} প্রক্রিয়াধীন।",
                    format_taka(amount)
                ),
                None => format!(
                    "BD Marketplace: আপনার ফেরত পণ্য {store} পেয়েছে (অর্ডার #{short})। রিফান্ড প্রক্রিয়াধীন।"
                ),
            },
            _ => format!(
                "BD Marketplace: আপনার রিটার্ন অনুরোধের হালনাগাদ অবস্থা: {} ({store}, অর্ডার #{short})।",
                vars.status
            ),
        },
        _ => format!("BD Marketplace: অর্ডার #{short} সম্পর্কে একটি আপডেট আছে।"),
    }
}

// ---------------------------------------------------------------------------
// Data portability (Wave 31)
// ---------------------------------------------------------------------------

/// The download filename for a data export (Wave 31):
/// `bd-marketplace-<kind>-<slug>-<yyyymmdd>.json`, where
/// `<slug>` is the store slug for a seller export or the
/// account phone for a buyer export, sanitized to
/// `[a-z0-9-]`: ASCII letters are lowercased, digits and
/// hyphens pass through, and every other character (the `+`
/// of an international phone, spaces, punctuation) becomes
/// a hyphen. The result is always a safe, portable filename
/// — no path separators, no spaces, no characters a
/// filesystem or a `Content-Disposition` header would choke
/// on. `kind` and `yyyymmdd` are caller-supplied and used
/// as-is (the API passes its own fixed kind strings and a
/// `chrono` `%Y%m%d` date).
pub fn export_filename(kind: &str, slug_or_phone: &str, yyyymmdd: &str) -> String {
    let sanitized: String = slug_or_phone
        .chars()
        .map(|c| {
            let lower = c.to_ascii_lowercase();
            if lower.is_ascii_alphanumeric() || lower == '-' {
                lower
            } else {
                '-'
            }
        })
        .collect();
    format!("bd-marketplace-{kind}-{sanitized}-{yyyymmdd}.json")
}

// ---------------------------------------------------------------------------
// Product variants (Wave 33)
// ---------------------------------------------------------------------------

/// The longest name a product variant may carry (Wave 33):
/// a size, a colour, or a size-plus-colour label ("XL",
/// "লাল", "XL — লাল") — eighty characters is generous for
/// a label a buyer picks from a row of buttons.
pub const MAX_VARIANT_NAME_CHARS: usize = 80;

/// Validate a product variant's name (Wave 33): after
/// trimming it must hold 1..=[`MAX_VARIANT_NAME_CHARS`]
/// characters (counted as characters, not bytes, so Bangla
/// labels measure fairly). On success the trimmed name is
/// returned for storage.
pub fn validate_variant_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    let count = trimmed.chars().count();
    if count == 0 {
        return Err("variant name must not be empty".to_string());
    }
    if count > MAX_VARIANT_NAME_CHARS {
        return Err(format!(
            "variant name must be at most {MAX_VARIANT_NAME_CHARS} characters"
        ));
    }
    Ok(trimmed.to_string())
}

/// The largest price delta a variant may carry in either
/// direction (Wave 33): 100_000_000 poisha is 1,000,000
/// taka — far above any real size/colour price difference.
/// A bigger delta is a typo (an extra zero or two), and
/// refusing it here keeps a fat-fingered delta from
/// silently repricing a variant by lakhs.
pub const MAX_VARIANT_PRICE_DELTA_POISHA: i64 = 100_000_000;

/// Validate a variant's price delta (Wave 33): its
/// absolute value must not exceed
/// [`MAX_VARIANT_PRICE_DELTA_POISHA`]. Zero is a valid
/// delta (a variant that differs only by size or colour,
/// not price).
pub fn validate_variant_delta(delta_poisha: i64) -> Result<i64, String> {
    if delta_poisha.checked_abs().is_none_or(|abs| abs > MAX_VARIANT_PRICE_DELTA_POISHA) {
        return Err(
            "variant price delta is too large (at most 1000000 taka in either direction)"
                .to_string(),
        );
    }
    Ok(delta_poisha)
}

/// The price a buyer actually pays for one unit of a
/// variant (Wave 33): the product's base price plus the
/// variant's delta. `None` when the sum overflows or is
/// not positive — a variant must always cost something,
/// so the API refuses to create or reprice a variant
/// whose final price would be zero or negative, and
/// checkout refuses a line whose stored delta no longer
/// clears the (possibly since-lowered) base price.
pub fn variant_final_price(base_poisha: i64, delta_poisha: i64) -> Option<i64> {
    base_poisha
        .checked_add(delta_poisha)
        .filter(|price| *price > 0)
}

// ---------------------------------------------------------------------------
// Wave 37 — third-party / mobile API keys (pure rules; the API stores
// the rows, hashes the secrets and enforces everything)
// ---------------------------------------------------------------------------

// API keys (Wave 37) are the long-lived bearer credentials
// external apps authenticate with — a future mobile app, a
// seller's own integration — in place of the 7-day JWT a
// human login issues. A key belongs to ONE user and acts as
// that user, narrowed by its scopes. Only the SHA-256 hash
// of a key is ever stored (the hashing itself lives in the
// API crate, beside the OTP hashing it mirrors); the full
// key is shown exactly once, at creation. The rules in this
// section are the pure vocabulary the API enforces: what a
// key may be named, which scope sets are legal, what a scope
// set permits, how long a key may live, and which part of a
// key is safe to display.

/// The longest name an API key may carry (Wave 37): a label
/// the owner picks to recognise the key later ("Mobile app",
/// "হিসাব সফটওয়্যার") — sixty characters is generous for a
/// label in a list.
pub const API_KEY_NAME_MAX_CHARS: usize = 60;

/// How many leading characters of a full key are safe to
/// store and display as its prefix (Wave 37): enough to tell
/// two keys apart in a list (`bdm_live_ab3…` — the marker
/// plus the first few random characters), far too few to
/// reconstruct or brute-force the secret behind them.
pub const API_KEY_PREFIX_CHARS: usize = 12;

/// The read scope (Wave 37): the key may call GET endpoints
/// (subject to the API's JWT-only route exclusions, which no
/// scope overrides).
pub const API_KEY_SCOPE_READ: &str = "read";

/// The write scope (Wave 37): the key may also call
/// non-GET endpoints. Write implies read — a key that can
/// change things can obviously look at them.
pub const API_KEY_SCOPE_WRITE: &str = "write";

/// Every scope a key may carry, in canonical order. Stored
/// scope sets are normalised against this list, so the
/// stored form is always `read` or `read,write` — never a
/// duplicate, never an unknown word.
pub const API_KEY_SCOPES: [&str; 2] = [API_KEY_SCOPE_READ, API_KEY_SCOPE_WRITE];

/// The longest lifetime a key may be created with, in days
/// (Wave 37): a year. Keys are bearer secrets that outlive
/// any session, so an unbounded lifetime is refused at
/// creation; a key with NO expiry is a separate, deliberate
/// choice (`expires_in_days` absent), not an accident of a
/// huge number.
pub const API_KEY_MAX_EXPIRY_DAYS: i64 = 365;

/// Validate an API key's name (Wave 37): after trimming it
/// must hold 1..=[`API_KEY_NAME_MAX_CHARS`] characters
/// (counted as characters, not bytes, so Bangla labels
/// measure fairly). On success the trimmed name is returned
/// for storage.
pub fn validate_api_key_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    let count = trimmed.chars().count();
    if count == 0 {
        return Err("api key name must not be empty".to_string());
    }
    if count > API_KEY_NAME_MAX_CHARS {
        return Err(format!(
            "api key name must be at most {API_KEY_NAME_MAX_CHARS} characters"
        ));
    }
    Ok(trimmed.to_string())
}

/// The displayable prefix of a full API key (Wave 37): its
/// first [`API_KEY_PREFIX_CHARS`] characters. A key shorter
/// than the prefix yields the whole string — that can only
/// happen for a malformed key, and the prefix is a display
/// aid, never a credential check.
pub fn api_key_prefix(full_key: &str) -> String {
    full_key.chars().take(API_KEY_PREFIX_CHARS).collect()
}

/// Validate and normalise a requested scope set (Wave 37):
/// the set must be non-empty, every entry must be one of
/// [`API_KEY_SCOPES`], and the result is deduplicated into
/// canonical order (`read` before `write`). Anything else —
/// an empty set, an invented scope like `admin` — is an
/// error: a key must never silently gain or lose a scope on
/// its way to storage.
pub fn validate_api_key_scopes(scopes: &[String]) -> Result<Vec<String>, String> {
    if scopes.is_empty() {
        return Err("an api key needs at least one scope".to_string());
    }
    for scope in scopes {
        if !API_KEY_SCOPES.contains(&scope.as_str()) {
            return Err(format!(
                "unknown api key scope '{scope}' — scopes are 'read' and 'write'"
            ));
        }
    }
    // Plain loop over the const array: iterating `[&str; 2]`
    // by value yields `&str`, so `scope` compares directly
    // with `s.as_str()` (two earlier forms — a filter closure
    // and a deref — tripped E0277 in CI; this is the shape
    // `API_KEY_SCOPES.contains(&scope.as_str())` above uses).
    let mut out: Vec<String> = Vec::new();
    for scope in API_KEY_SCOPES {
        if scopes.iter().any(|s| s.as_str() == scope) {
            out.push(scope.to_string());
        }
    }
    Ok(out)
}

/// May a key carrying `scopes` make this call (Wave 37)?
/// Reads (GET and friends) need the read scope — or the
/// write scope, which implies it; writes need the write
/// scope. Unknown stored scopes grant nothing: the function
/// fails closed, like the plan and role rules, so a garbled
/// stored scope set can never mint access. This is the SCOPE
/// rule only — the JWT-only route exclusions (admin, money
/// movement, key management, exports) are a separate API
/// layer rule that applies before scopes are even read.
pub fn api_key_scope_allows(scopes: &[String], method_is_write: bool) -> bool {
    let has = |wanted: &str| scopes.iter().any(|scope| scope == wanted);
    if method_is_write {
        has(API_KEY_SCOPE_WRITE)
    } else {
        has(API_KEY_SCOPE_READ) || has(API_KEY_SCOPE_WRITE)
    }
}

/// Validate a requested key lifetime (Wave 37): `None` (no
/// expiry) is a legal, deliberate choice; a given day count
/// must lie in 1..=[`API_KEY_MAX_EXPIRY_DAYS`]. Zero or
/// negative would create a key that is born expired, and
/// anything past a year is refused rather than stored.
pub fn validate_api_key_expiry_days(days: Option<i64>) -> Result<Option<i64>, String> {
    match days {
        None => Ok(None),
        Some(days) if (1..=API_KEY_MAX_EXPIRY_DAYS).contains(&days) => Ok(Some(days)),
        Some(_) => Err(format!(
            "api key expiry must be between 1 and {API_KEY_MAX_EXPIRY_DAYS} days"
        )),
    }
}

// ---------------------------------------------------------------------------
// Wave 43 — request ids (pure rule; the API's observability
// middleware honours a client id that passes it, mints a fresh
// one otherwise, and echoes the result on the response)
// ---------------------------------------------------------------------------

/// The longest request id the API will honour from an inbound
/// `X-Request-Id` header (Wave 43). A request id is a correlation
/// handle, not a credential: 64 characters fits a dashed UUID
/// (36) with room to spare, and stays short enough that a log
/// line remains a log line.
pub const REQUEST_ID_MAX_CHARS: usize = 64;

/// Is this string usable as a request id (Wave 43)? The rule
/// exists because the value is echoed into a response header
/// and printed into the request log, so its shape is a safety
/// property, not a courtesy: it must hold
/// 1..=[`REQUEST_ID_MAX_CHARS`] characters, every one of them
/// printable ASCII (0x20..=0x7E — no control bytes that could
/// forge a second log line, no DEL, nothing non-ASCII), and it
/// must not be padded with leading or trailing spaces (a padded
/// id smuggles a second field into a space-separated log line;
/// spaces INSIDE the value are fine). A refused id is never an
/// error — the caller simply mints a fresh one instead.
pub fn validate_request_id(id: &str) -> bool {
    if id.is_empty() || id.len() > REQUEST_ID_MAX_CHARS {
        return false;
    }
    if id.trim() != id {
        return false;
    }
    id.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

// ---------------------------------------------------------------------------
// Wave 44 — payout idempotency keys (pure rule; the payout
// handler validates an inbound `Idempotency-Key` header with
// it before any money moves)
// ---------------------------------------------------------------------------

/// The longest idempotency key a payout request may carry
/// (Wave 44). A key is a client-minted dedup handle — the web
/// form sends a UUID (36 characters) — so 128 leaves generous
/// room for namespaced client schemes (`web-<uuid>`,
/// `app-retry-<n>-<uuid>`) while staying far below anything
/// that could bloat the payouts row or its unique index.
pub const IDEMPOTENCY_KEY_MAX_CHARS: usize = 128;

/// Is this string usable as a payout `Idempotency-Key`
/// (Wave 44)? The value arrives as an HTTP header and is
/// stored verbatim on the payout row, where the partial
/// unique index matches it byte-for-byte — so its alphabet
/// is the strict one: 1..=[`IDEMPOTENCY_KEY_MAX_CHARS`]
/// characters, every one VISIBLE ASCII (0x21..=0x7E). Unlike
/// the request-id rule there are no spaces at all, inside or
/// out: a space in a stored dedup key is an invisible
/// difference between two keys a human would swear are the
/// same, and the whole point of the key is that equality is
/// obvious. Control bytes, DEL and non-ASCII are refused for
/// the same header-safety reasons as request ids. A refused
/// key is a client error (the handler answers 400), never a
/// silently-dropped header — silently ignoring a key the
/// client believed in would recreate the duplicate it sent
/// the key to prevent.
pub fn validate_idempotency_key(key: &str) -> bool {
    if key.is_empty() || key.len() > IDEMPOTENCY_KEY_MAX_CHARS {
        return false;
    }
    key.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

// ---------------------------------------------------------------------------
// Wave 51 — product media (pure rules; the API stores the
// bytes, processes the pixels and serves the files)
// ---------------------------------------------------------------------------

/// The largest single image upload the platform accepts
/// (Wave 51): 10 MiB. Phone photos — the realistic seller
/// upload — sit far below this; the cap exists so one
/// request cannot stream unbounded bytes at the image
/// decoder. The multipart route's own body limit sits a
/// little above this (the file plus its form fields), and
/// the API enforces this exact figure on the file itself.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// How many images one product may carry (Wave 51). Twelve
/// is generous for a marketplace gallery (AliExpress shows
/// fewer on most listings) and bounds both the seller's
/// upload work and the detail page's payload.
pub const MAX_IMAGES_PER_PRODUCT: i64 = 12;

/// The longest alt text an image may carry (Wave 51),
/// counted as characters like every other text rule here.
pub const MAX_IMAGE_ALT_TEXT_CHARS: usize = 200;

/// The longest external image URL the platform will store
/// (Wave 51). Real CDN URLs are a few hundred characters;
/// 2,048 is the de-facto browser ceiling and far above any
/// honest photo link.
pub const MAX_EXTERNAL_IMAGE_URL_CHARS: usize = 2_048;

/// The largest width or height (in pixels) an uploaded
/// image may declare (Wave 51). The decoder's memory use
/// grows with the pixel count, so an absurd header
/// (100,000 x 100,000) is refused before a single pixel is
/// decoded. Real cameras stay far below this.
pub const MAX_IMAGE_DIMENSION: u32 = 10_000;

/// The widths the media pipeline derives from every upload
/// (Wave 51), widest first: a zoom view, a gallery view and
/// a card thumbnail. A width is only ever derived when it
/// is strictly smaller than the original — the pipeline
/// never upscales (see [`plan_derived_sizes`]).
pub const DERIVED_IMAGE_WIDTHS: [u32; 3] = [1_200, 600, 300];

/// The width a product CARD wants (Wave 51): the smallest
/// derived size, used by every list/rail/cart surface.
pub const CARD_IMAGE_WIDTH: u32 = 300;

/// The width a product GALLERY (detail page) wants
/// (Wave 51) — also the width behind every product view's
/// single `image_url`.
pub const GALLERY_IMAGE_WIDTH: u32 = 600;

/// The width the gallery's zoom / lightbox wants (Wave 51).
pub const ZOOM_IMAGE_WIDTH: u32 = 1_200;

/// One accepted image format (Wave 51). The set is
/// deliberately the three formats every browser renders
/// and every phone camera produces; anything else (GIF,
/// BMP, TIFF, SVG) is refused at the sniff, before decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Jpeg,
    Png,
    Webp,
}

impl ImageKind {
    /// The wire content type for this format.
    pub fn content_type(self) -> &'static str {
        match self {
            ImageKind::Jpeg => "image/jpeg",
            ImageKind::Png => "image/png",
            ImageKind::Webp => "image/webp",
        }
    }

    /// The file extension stored objects carry for this
    /// format (the original upload's re-encode keeps its
    /// own format; derived sizes are always WebP).
    pub fn extension(self) -> &'static str {
        match self {
            ImageKind::Jpeg => "jpg",
            ImageKind::Png => "png",
            ImageKind::Webp => "webp",
        }
    }

    /// Parse a declared content type into a kind: the three
    /// accepted types only, case-insensitive, with any
    /// `; ...` parameter suffix ignored (multipart fields
    /// sometimes carry one). Anything else is `None`.
    pub fn from_content_type(content_type: &str) -> Option<ImageKind> {
        let base = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match base.as_str() {
            "image/jpeg" | "image/jpg" => Some(ImageKind::Jpeg),
            "image/png" => Some(ImageKind::Png),
            "image/webp" => Some(ImageKind::Webp),
            _ => None,
        }
    }
}

/// Sniff an image's real format from its leading bytes
/// (Wave 51) — the magic numbers, never the file name or
/// the declared type: JPEG opens with the SOI marker
/// `FF D8 FF`, PNG with its fixed 8-byte signature, and
/// WebP is a RIFF container whose form type at bytes 8..12
/// is `WEBP`. Returns `None` for anything else, including
/// input too short to hold a signature.
pub fn sniff_image_kind(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Some(ImageKind::Jpeg);
    }
    if bytes.len() >= 8 && bytes[..8] == [137, 80, 78, 71, 13, 10, 26, 10] {
        return Some(ImageKind::Png);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(ImageKind::Webp);
    }
    None
}

/// Decide the format of an upload (Wave 51) from its bytes
/// and its declared content type. The BYTES are the truth:
/// an upload whose bytes sniff as no accepted format is
/// refused outright. When the client also declared a type,
/// the declaration must parse as an accepted format AND
/// agree with the sniff — a PNG wearing a JPEG declaration
/// is refused rather than silently relabelled, because the
/// stored objects and their served content types are built
/// from this answer.
pub fn resolve_image_kind(
    declared_content_type: Option<&str>,
    bytes: &[u8],
) -> Result<ImageKind, String> {
    let sniffed = sniff_image_kind(bytes).ok_or_else(|| {
        "the file is not a supported image (JPEG, PNG or WebP) — \
         ফাইলটি সমর্থিত ছবি নয় (JPEG, PNG বা WebP হতে হবে)"
            .to_string()
    })?;
    if let Some(declared) = declared_content_type {
        let declared_kind = ImageKind::from_content_type(declared).ok_or_else(|| {
            format!(
                "unsupported image content type '{declared}' — use JPEG, PNG or WebP"
            )
        })?;
        if declared_kind != sniffed {
            return Err(format!(
                "the file's contents ({}) do not match its declared type ({})",
                sniffed.content_type(),
                declared_kind.content_type()
            ));
        }
    }
    Ok(sniffed)
}

/// Validate an upload's byte length (Wave 51): non-empty
/// and at most [`MAX_IMAGE_BYTES`]. Checked before the
/// sniff and the decode, so an oversize body costs one
/// comparison, not a decode.
pub fn validate_image_size(byte_len: usize) -> Result<(), String> {
    if byte_len == 0 {
        return Err("the uploaded file is empty — খালি ফাইল আপলোড করা যাবে না".to_string());
    }
    if byte_len > MAX_IMAGE_BYTES {
        return Err(format!(
            "the image is too large ({} bytes) — at most {} bytes (10 MiB) — \
             ছবিটি খুব বড়, সর্বোচ্চ ১০ MiB",
            byte_len, MAX_IMAGE_BYTES
        ));
    }
    Ok(())
}

/// Validate a decoded image's dimensions (Wave 51): both
/// sides at least one pixel and at most
/// [`MAX_IMAGE_DIMENSION`]. Runs on the decoder's answer,
/// before any resize work is planned.
pub fn validate_image_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("the image has no pixels — ছবিতে কোনো পিক্সেল নেই".to_string());
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(format!(
            "the image is {width}x{height} — at most {MAX_IMAGE_DIMENSION} pixels per side"
        ));
    }
    Ok(())
}

/// May one more image be added to a product that already
/// has `current_count` (Wave 51)? The count is the stored
/// row count; the cap is [`MAX_IMAGES_PER_PRODUCT`].
pub fn can_add_product_image(current_count: i64) -> bool {
    (0..MAX_IMAGES_PER_PRODUCT).contains(&current_count)
}

/// Validate image alt text (Wave 51): optional; a provided
/// value is trimmed, a blank value becomes `None` (no alt
/// text), and the stored text is at most
/// [`MAX_IMAGE_ALT_TEXT_CHARS`] characters. Alt text is
/// accessibility copy, not a caption field, so the cap is
/// tight.
pub fn validate_alt_text(alt: Option<&str>) -> Result<Option<String>, String> {
    let Some(trimmed) = alt.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if trimmed.chars().count() > MAX_IMAGE_ALT_TEXT_CHARS {
        return Err(format!(
            "alt text must be at most {MAX_IMAGE_ALT_TEXT_CHARS} characters"
        ));
    }
    Ok(Some(trimmed.to_string()))
}

/// Validate an external image URL (Wave 51) — the hotlink
/// an import or a URL-based seller attaches instead of an
/// upload. The rule: http or https only (never `ftp:`,
/// `file:`, `data:` or a scheme-relative URL), a non-empty
/// host, no whitespace or control characters anywhere, and
/// at most [`MAX_EXTERNAL_IMAGE_URL_CHARS`] characters.
/// The URL is stored verbatim (trimmed) and handed to
/// browsers as-is, so its shape is a safety property: a
/// `javascript:` or credential-shaped oddity must never
/// reach an `<img src>` through this field. On success the
/// trimmed URL is returned for storage.
pub fn validate_external_image_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("the image URL must not be empty".to_string());
    }
    if trimmed.chars().count() > MAX_EXTERNAL_IMAGE_URL_CHARS {
        return Err(format!(
            "the image URL must be at most {MAX_EXTERNAL_IMAGE_URL_CHARS} characters"
        ));
    }
    if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the image URL must not contain spaces or control characters".to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    let rest = if let Some(rest) = lower.strip_prefix("http://") {
        rest
    } else if let Some(rest) = lower.strip_prefix("https://") {
        rest
    } else {
        return Err(
            "the image URL must start with http:// or https:// — \
             ছবির URL http:// বা https:// দিয়ে শুরু হতে হবে"
                .to_string(),
        );
    };
    // The host runs to the first path/query/fragment mark.
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        return Err("the image URL has no host".to_string());
    }
    Ok(trimmed.to_string())
}

/// One derived size the pipeline should produce (Wave 51):
/// a target width and the height that keeps the original's
/// aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedSize {
    pub width: u32,
    pub height: u32,
}

/// Plan the derived sizes for an original of `orig_width`
/// x `orig_height` (Wave 51): every width in
/// [`DERIVED_IMAGE_WIDTHS`] strictly smaller than the
/// original's width, widest first, each with its
/// aspect-preserving height (rounded to the nearest pixel,
/// at least 1). An original no wider than the smallest
/// derived width plans NOTHING — the pipeline never
/// upscales, and small originals are served as-is. A zero
/// dimension plans nothing too (the dimension validator
/// refuses such images before planning in practice).
pub fn plan_derived_sizes(orig_width: u32, orig_height: u32) -> Vec<DerivedSize> {
    if orig_width == 0 || orig_height == 0 {
        return Vec::new();
    }
    DERIVED_IMAGE_WIDTHS
        .iter()
        .copied()
        .filter(|width| *width < orig_width)
        .map(|width| {
            // height = orig_height * width / orig_width,
            // rounded to nearest, in u64 so the product of
            // two u32s can never overflow.
            let height = ((u64::from(orig_height) * u64::from(width)
                + u64::from(orig_width) / 2)
                / u64::from(orig_width)) as u32;
            DerivedSize {
                width,
                height: height.max(1),
            }
        })
        .collect()
}

/// The derived width to serve when a surface wants
/// `target` pixels from an original of `orig_width`
/// (Wave 51): the smallest planned width that is at least
/// the target (never serve a needlessly huge file), else
/// the largest planned width (the best available), else
/// `None` — no derived size exists, serve the original.
pub fn best_derived_width(orig_width: u32, target: u32) -> Option<u32> {
    let planned: Vec<u32> = plan_derived_sizes(orig_width, orig_width)
        .into_iter()
        .map(|size| size.width)
        .collect();
    if let Some(width) = planned.iter().copied().filter(|w| *w >= target).min() {
        return Some(width);
    }
    planned.into_iter().max()
}

/// One image's placement in a gallery reorder (Wave 51):
/// the caller's key for the image (its id), the requested
/// position, and whether the caller flagged it primary.
/// Generic over the key so the domain rule stays free of
/// the API's id type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageOrderItem<T> {
    pub key: T,
    pub position: i32,
    pub is_primary: bool,
}

/// Normalise a gallery layout (Wave 51) — THE rule every
/// reorder, delete-promotion and first-upload path shares:
///
/// - Items are ordered by requested position; ties keep
///   the caller's order (the sort is stable).
/// - Positions are reassigned densely: 0, 1, 2, ...
/// - Exactly one item ends primary when the list is
///   non-empty: the first primary-flagged item in the new
///   order keeps the flag (later flags are cleared); when
///   NO item is flagged, the first item becomes primary —
///   which is what promotes the next image when the
///   primary is deleted, and what makes a product's first
///   image its primary.
///
/// The input is not mutated; the returned vector is in the
/// new display order.
pub fn normalize_image_order<T: Clone>(items: &[ImageOrderItem<T>]) -> Vec<ImageOrderItem<T>> {
    let mut indexed: Vec<(usize, &ImageOrderItem<T>)> = items.iter().enumerate().collect();
    indexed.sort_by_key(|(index, item)| (item.position, *index));
    let primary_index = indexed
        .iter()
        .find(|(_, item)| item.is_primary)
        .map(|(index, _)| *index)
        .or_else(|| indexed.first().map(|(index, _)| *index));
    indexed
        .into_iter()
        .enumerate()
        .map(|(new_position, (index, item))| ImageOrderItem {
            key: item.key.clone(),
            position: new_position as i32,
            is_primary: Some(index) == primary_index,
        })
        .collect()
}

/// Validate a media storage key (Wave 51) — the path-like
/// name an object lives under in the media store
/// (`products/<product-id>/<image-id>-600.webp`). Keys are
/// built by the API from ids it minted, and the serve route
/// takes one from the URL, so the rule is the traversal
/// guard for both: non-empty, at most 512 characters, only
/// ASCII letters, digits, `-`, `_`, `.` and `/`, no empty
/// segment, no `.` or `..` segment, no leading or trailing
/// slash, no backslash. A key passing this rule names a
/// file strictly inside the store root.
pub fn validate_media_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("the media key must not be empty".to_string());
    }
    if key.chars().count() > 512 {
        return Err("the media key is too long".to_string());
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        return Err("the media key contains characters outside [A-Za-z0-9._/-]".to_string());
    }
    for segment in key.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("the media key must not contain empty, '.' or '..' segments".to_string());
        }
    }
    Ok(())
}

/// The per-store media quota (Wave 51): the total bytes of
/// UPLOADED product-image originals one store may hold
/// (500 MiB). External hotlinks cost the platform no
/// storage and never count. The cap exists so one store
/// cannot fill the disk the whole marketplace shares; it
/// is enforced on the upload paths (single, bulk, fetch
/// and replace) against the store's current usage.
pub const MAX_STORE_MEDIA_BYTES: i64 = 500 * 1024 * 1024;

/// Would adding `adding_bytes` of uploads to a store
/// already holding `current_bytes` stay within the media
/// quota? (Wave 51.) Exactly-at-the-cap is allowed; the
/// arithmetic is saturating so a corrupt usage figure
/// fails closed rather than wrapping.
pub fn store_media_within_quota(current_bytes: i64, adding_bytes: i64) -> bool {
    within_media_quota(current_bytes, adding_bytes, MAX_STORE_MEDIA_BYTES)
}

/// The quota rule against an explicit cap (Wave 51
/// extension): the platform settings may move the cap
/// (see `validate_media_store_quota_mb`), and this is the
/// arithmetic every enforcement point shares.
pub fn within_media_quota(current_bytes: i64, adding_bytes: i64, quota_bytes: i64) -> bool {
    current_bytes.saturating_add(adding_bytes) <= quota_bytes
}

/// Is this a legal platform-set per-product image cap
/// (Wave 51 extension)? The settings surface accepts
/// 1..=50 — wide enough for any real catalogue, narrow
/// enough that a typo cannot uncap the gallery.
pub fn validate_media_max_images(value: i64) -> bool {
    (1..=50).contains(&value)
}

/// Validate (and normalise) an uploaded file's original
/// name (Wave 51 extension): display metadata the media
/// library shows back to the seller. A name is a NAME —
/// path separators and control characters are refused
/// outright (the value is never used as a path, and the
/// rule keeps it that way), the length is capped at 120
/// characters, and an empty/blank name normalises to
/// `None` (the file field simply carried none).
pub fn validate_original_filename(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let name = raw.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > 120 {
        return Err("the file name is too long (max 120 characters)".to_string());
    }
    if name
        .chars()
        .any(|c| c == '/' || c == '\\' || c.is_control())
    {
        return Err("the file name must be a name, not a path".to_string());
    }
    Ok(Some(name.to_string()))
}

/// Is this a legal platform-set per-store media quota,
/// in megabytes (Wave 51 extension)? 10 MiB ..= 100 GiB.
pub fn validate_media_store_quota_mb(value: i64) -> bool {
    (10..=102_400).contains(&value)
}

/// Suggest alt text from an uploaded file's name (Wave 51
/// extension): sellers upload `red-running-shoe_front.JPG`
/// and leave the alt blank, which screen readers and
/// search engines then get nothing from. The suggestion
/// drops the extension, turns `-`/`_`/`.` separators into
/// spaces, collapses whitespace, and capitalises the
/// first letter — a starting point the seller can edit,
/// never a claim about the photo's content. Returns
/// `None` when nothing usable remains (a name of pure
/// separators, or the result would exceed the alt cap).
pub fn suggest_alt_text(filename: &str) -> Option<String> {
    let stem = filename.rsplit_once('.').map_or(filename, |(stem, _)| stem);
    let mut words: Vec<String> = Vec::new();
    for word in stem.split(['-', '_', '.', ' ']) {
        let word = word.trim();
        if !word.is_empty() {
            words.push(word.to_string());
        }
    }
    if words.is_empty() {
        return None;
    }
    let joined = words.join(" ");
    let mut chars = joined.chars();
    let suggestion = {
        let first = chars.next()?;
        first.to_uppercase().collect::<String>() + chars.as_str()
    };
    if suggestion.chars().count() > MAX_IMAGE_ALT_TEXT_CHARS {
        return None;
    }
    Some(suggestion)
}

/// Quality warnings for one stored image (Wave 51
/// extension), as stable codes the clients render in
/// their own words: `low_resolution` (either side under
/// 300px, or the long side under 600 — the gallery will
/// look soft), `extreme_aspect` (wider or taller than
/// 4:1 — cards crop hard), `very_large` (a side over
/// 4,000px — decode cost on low-end phones). Unknown
/// dimensions (external images) warn about nothing: the
/// platform never measured them.
pub fn image_quality_warnings(width: Option<i32>, height: Option<i32>) -> Vec<&'static str> {
    let (Some(w), Some(h)) = (width, height) else {
        return Vec::new();
    };
    if w <= 0 || h <= 0 {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    if w < 300 || h < 300 || w.max(h) < 600 {
        warnings.push("low_resolution");
    }
    if w > h * 4 || h > w * 4 {
        warnings.push("extreme_aspect");
    }
    if w > 4_000 || h > 4_000 {
        warnings.push("very_large");
    }
    warnings
}

/// The first image URL in a block of free text (Wave 51
/// extension): Facebook posts carry their photo as a bare
/// link in the post body, and the import preview lifts it
/// into the draft's `image_url`. A candidate is an
/// http(s) URL whose path ends in a known image extension
/// (query strings allowed); trailing sentence punctuation
/// stuck to the URL is trimmed. `None` when the text
/// carries no image link.
pub fn extract_image_url(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let candidate = token.trim_matches(|c| matches!(c, '"' | '\'' | '<' | '>' | '(' | ')'));
        let candidate = candidate.trim_end_matches(['.', ',', ';', '!']);
        if !(candidate.starts_with("https://") || candidate.starts_with("http://")) {
            continue;
        }
        let path = candidate
            .split(['?', '#'])
            .next()
            .unwrap_or(candidate)
            .to_ascii_lowercase();
        if [".jpg", ".jpeg", ".png", ".webp"]
            .iter()
            .any(|ext| path.ends_with(ext))
        {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Validate a URL the server itself will FETCH an image
/// from (Wave 51, the from-web attach) — stricter than
/// [`validate_external_image_url`] (which only ever
/// renders a hotlink in a browser): the scheme must be
/// http/https, credentials in the URL are refused (they
/// would leak to logs and the fetch log line), the host
/// must be present and must not be a loopback NAME
/// (`localhost`, `*.localhost`, `*.local` mDNS names).
/// The resolved-IP half of the SSRF rule is
/// [`is_blocked_fetch_ip`], applied by the caller to every
/// address the host resolves to. Returns the URL trimmed.
pub fn validate_fetch_url(raw: &str) -> Result<String, String> {
    let url = validate_external_image_url(raw)?;
    let after_scheme = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.contains('@') {
        return Err(
            "the image URL must not contain credentials — \
             ছবির URL-এ লগইন তথ্য থাকা যাবে না"
                .to_string(),
        );
    }
    let host = authority
        .rsplit_once(':')
        .map(|(host, _port)| host)
        .unwrap_or(authority)
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err("the image URL has no host".to_string());
    }
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return Err(
            "the image URL host is not a public host — \
             ছবির URL-এর হোস্ট পাবলিক হতে হবে"
                .to_string(),
        );
    }
    Ok(url)
}

/// Is this IP address one a server-side image fetch must
/// never connect to (Wave 51, the SSRF half of the
/// from-web rule)? Blocked: loopback, private (RFC 1918 +
/// IPv6 unique-local), link-local (incl. the cloud
/// metadata 169.254.169.254), multicast, unspecified, and
/// the IPv4 documentation/reserved ranges this platform
/// treats as non-public. An unparseable string is blocked
/// (fail closed — a fetch target we cannot classify is a
/// fetch we do not make).
pub fn is_blocked_fetch_ip(raw: &str) -> bool {
    let Ok(ip) = raw.trim().parse::<std::net::IpAddr>() else {
        return true;
    };
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.octets()[0] == 0
        }
        std::net::IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_multicast() || v6.is_unspecified() {
                return true;
            }
            let segments = v6.segments();
            // Unique-local fc00::/7 and link-local fe80::/10.
            (segments[0] & 0xfe00) == 0xfc00 || (segments[0] & 0xffc0) == 0xfe80
        }
    }
}

// ---------------------------------------------------------------------------
// Wave 52 — merchandising: home banners, collections, flash sales
// ---------------------------------------------------------------------------
//
// The storefront-parity program's data rules. Banners and
// collections are admin-curated home-page furniture; flash
// sales are time-boxed price events whose prices the cart and
// checkout honour. Every rule here is pure and unit-tested;
// the API layer adds only the database facts (a product's
// current base price, the units already sold at a sale price)
// and never re-implements a rule.

/// Longest merchandising title (banner, collection, flash
/// sale): a headline, not a paragraph.
pub const MAX_MERCH_TITLE_CHARS: usize = 120;

/// Longest banner subtitle: one supporting line under the
/// headline.
pub const MAX_BANNER_SUBTITLE_CHARS: usize = 200;

/// Longest banner link: room for a deep link with a query
/// string, not for a pasted document.
pub const MAX_BANNER_LINK_CHARS: usize = 500;

/// Longest collection subtitle.
pub const MAX_COLLECTION_SUBTITLE_CHARS: usize = 200;

/// Validate a merchandising title (home banner, collection or
/// flash sale): trimmed, 1..=[`MAX_MERCH_TITLE_CHARS`]
/// characters. Returns the trimmed title on success. Error
/// strings are user-facing (the admin forms show them).
pub fn validate_merch_title(title: &str) -> Result<String, &'static str> {
    let trimmed = title.trim();
    let len = trimmed.chars().count();
    if len == 0 {
        return Err("the title must not be empty");
    }
    if len > MAX_MERCH_TITLE_CHARS {
        return Err("the title is too long (120 characters at most)");
    }
    Ok(trimmed.to_string())
}

/// Validate an optional merchandising subtitle: absent and
/// blank both store as "no subtitle"; a present one is
/// trimmed and capped at `max_chars`. Returns the value to
/// store.
pub fn validate_merch_subtitle(
    subtitle: Option<&str>,
    max_chars: usize,
) -> Result<Option<String>, &'static str> {
    match subtitle {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.chars().count() > max_chars {
                return Err("the subtitle is too long");
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

/// Validate a home banner's link. A banner links either to a
/// page of this site — a site-relative path beginning with a
/// single `/` (`//` would be a protocol-relative URL pointing
/// off-site, and is refused) — or to an absolute `http://` /
/// `https://` URL (a partner campaign). Anything else
/// (`javascript:`, a bare host, an empty string) is refused:
/// the carousel renders the value straight into an anchor, so
/// the scheme whitelist is the whole defence. Returns the
/// trimmed link on success.
pub fn validate_banner_link(link: &str) -> Result<String, &'static str> {
    let trimmed = link.trim();
    if trimmed.is_empty() {
        return Err("the link must not be empty");
    }
    if trimmed.chars().count() > MAX_BANNER_LINK_CHARS {
        return Err("the link is too long (500 characters at most)");
    }
    if trimmed.chars().any(char::is_control) || trimmed.contains(char::is_whitespace) {
        return Err("the link must not contain spaces or control characters");
    }
    if let Some(rest) = trimmed.strip_prefix('/') {
        if rest.starts_with('/') {
            return Err("the link must be a site path or an http(s) URL");
        }
        return Ok(trimmed.to_string());
    }
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return Ok(trimmed.to_string());
    }
    Err("the link must be a site path (starting with /) or an http(s) URL")
}

/// Is a home banner visible at `now` (unix seconds)? The one
/// visibility rule: the banner must be switched on, and its
/// optional window must contain `now` — half-open
/// [starts_at, ends_at), the same convention as promotions
/// and flash sales: a banner appears exactly at its start
/// second and disappears exactly at its end second. A null
/// bound is unbounded on that side. The home feed's SQL
/// implements this same rule over the stored timestamps.
pub fn banner_is_visible(
    is_active: bool,
    starts_at_unix: Option<i64>,
    ends_at_unix: Option<i64>,
    now_unix: i64,
) -> bool {
    if !is_active {
        return false;
    }
    if let Some(starts) = starts_at_unix {
        if now_unix < starts {
            return false;
        }
    }
    if let Some(ends) = ends_at_unix {
        if now_unix >= ends {
            return false;
        }
    }
    true
}

/// Validate an optional merchandising window (a banner's):
/// when both bounds are present the window must be non-empty
/// — it ends strictly after it starts. A single bound (or
/// none) is always a valid window.
pub fn validate_optional_window(
    starts_at_unix: Option<i64>,
    ends_at_unix: Option<i64>,
) -> Result<(), &'static str> {
    if let (Some(starts), Some(ends)) = (starts_at_unix, ends_at_unix) {
        if ends <= starts {
            return Err("the window must end after it starts");
        }
    }
    Ok(())
}

/// Validate a collection slug: the landing-page segment of
/// `/collections/<slug>`. The rule is the store-slug rule
/// ([`is_valid_slug`]) — one slug grammar for the whole site,
/// so a collection can never squat on a shape a store could
/// not use and the two namespaces stay indistinguishable in
/// form. Returns the slug unchanged on success.
pub fn validate_collection_slug(slug: &str) -> Result<&str, &'static str> {
    if is_valid_slug(slug) {
        Ok(slug)
    } else {
        Err("the slug must be 3-40 characters of lowercase letters, digits and hyphens, \
             starting and ending with a letter or digit")
    }
}

/// Validate a flash sale's window (unix seconds): both
/// bounds are required for a flash sale (a sale with no end
/// is not a flash sale), and the window must be non-empty.
pub fn validate_flash_window(starts_at_unix: i64, ends_at_unix: i64) -> Result<(), &'static str> {
    if ends_at_unix <= starts_at_unix {
        return Err("the flash sale must end after it starts");
    }
    Ok(())
}

/// Is a flash sale live at `now` (unix seconds)? Active flag
/// plus the half-open window [starts_at, ends_at) — the same
/// convention as [`promotion_is_active`], so every timed
/// placement on the platform starts and stops on the same
/// rule.
pub fn flash_sale_is_live(
    is_active: bool,
    starts_at_unix: i64,
    ends_at_unix: i64,
    now_unix: i64,
) -> bool {
    is_active && starts_at_unix <= now_unix && now_unix < ends_at_unix
}

/// Do two half-open windows [a_start, a_end) and [b_start,
/// b_end) overlap? The overlap test behind the "at most one
/// live flash sale" rule: windows that merely TOUCH (one ends
/// exactly when the other starts) do not overlap, so a sale
/// may be scheduled back-to-back after another.
pub fn windows_overlap(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> bool {
    a_start < b_end && b_start < a_end
}

/// Validate a flash sale item's price against the product's
/// CURRENT base price: a sale price is real only when it is a
/// genuine reduction — strictly positive and strictly below
/// the base. A "sale" at or above the base price is refused
/// at write time, so the storefront can never advertise a
/// discount that is not one. Returns the price on success.
pub fn validate_flash_sale_price(
    sale_price_poisha: i64,
    base_price_poisha: i64,
) -> Result<i64, &'static str> {
    if sale_price_poisha <= 0 {
        return Err("the sale price must be positive");
    }
    if sale_price_poisha >= base_price_poisha {
        return Err("the sale price must be below the product's current price");
    }
    Ok(sale_price_poisha)
}

/// Validate a flash sale item's stock cap: when present it
/// must allow at least one unit (a cap of zero would be a
/// sale that can never sell — refused at write time rather
/// than silently inert). Returns the cap on success.
pub fn validate_flash_stock_cap(stock_cap: Option<i64>) -> Result<Option<i64>, &'static str> {
    match stock_cap {
        None => Ok(None),
        Some(cap) if cap >= 1 => Ok(Some(cap)),
        Some(_) => Err("the stock cap must be at least 1"),
    }
}

/// THE flash price resolution rule, given the gathered
/// facts: the candidate sale price applies — and is returned
/// — unless the item's stock cap is already exhausted by
/// units sold at the sale price, in which case there is no
/// flash price and the product sells at its base price
/// everywhere. A non-positive candidate is never a price
/// (defence in depth: write-time validation already refuses
/// one). Cap accounting's definition of "sold at the sale
/// price" lives with the API's one aggregate; this function
/// is only the comparison.
pub fn resolve_flash_price(
    sale_price_poisha: i64,
    stock_cap: Option<i64>,
    sold_at_sale_price: i64,
) -> Option<i64> {
    if sale_price_poisha <= 0 {
        return None;
    }
    if let Some(cap) = stock_cap {
        if sold_at_sale_price >= cap {
            return None;
        }
    }
    Some(sale_price_poisha)
}

/// The whole-percent discount of a sale price against a base
/// price, for the "-X%" badge: floor of
/// (base − sale) × 100 / base, computed in i128 so no price
/// the platform can store overflows it. Zero when the inputs
/// cannot describe a discount (non-positive base, or a sale
/// price that is not below the base) — the badge simply does
/// not render.
pub fn price_drop_percent(base_price_poisha: i64, sale_price_poisha: i64) -> i64 {
    if base_price_poisha <= 0 || sale_price_poisha < 0 || sale_price_poisha >= base_price_poisha {
        return 0;
    }
    let drop = i128::from(base_price_poisha - sale_price_poisha) * 100 / i128::from(base_price_poisha);
    drop as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(store: &str, product: &str, qty: u32, price_bdt: i64) -> CartItem {
        witem(store, product, qty, price_bdt, DEFAULT_WEIGHT_GRAMS)
    }

    fn witem(
        store: &str,
        product: &str,
        qty: u32,
        price_bdt: i64,
        unit_weight_grams: i64,
    ) -> CartItem {
        CartItem {
            product_id: product.to_string(),
            store_id: store.to_string(),
            quantity: qty,
            unit_price_poisha: price_bdt * 100,
            unit_weight_grams,
            variant_id: None,
        }
    }

    #[test]
    fn store_role_matrix_owner_can_do_everything() {
        for action in STORE_ACTIONS {
            assert!(store_role_can("owner", action), "owner must allow {action}");
        }
    }

    #[test]
    fn store_role_matrix_manager_except_members_and_payouts() {
        for action in STORE_ACTIONS {
            let expected = !matches!(
                action,
                STORE_ACTION_MANAGE_MEMBERS | STORE_ACTION_MANAGE_PAYOUTS
            );
            assert_eq!(
                store_role_can("manager", action),
                expected,
                "manager / {action}"
            );
        }
        // The two denials, named explicitly so the rule reads
        // as policy, not as a table accident.
        assert!(!store_role_can("manager", "manage_members"));
        assert!(!store_role_can("manager", "manage_payouts"));
    }

    #[test]
    fn store_role_matrix_staff_works_the_floor_only() {
        let allowed = [
            STORE_ACTION_MANAGE_CATALOG,
            STORE_ACTION_MANAGE_ORDERS,
            STORE_ACTION_ANSWER_QUESTIONS,
        ];
        for action in STORE_ACTIONS {
            assert_eq!(
                store_role_can("staff", action),
                allowed.contains(&action),
                "staff / {action}"
            );
        }
    }

    #[test]
    fn store_role_matrix_fails_closed() {
        // Unknown roles get nothing, unknown actions are
        // granted to nobody (not even the owner), and role
        // matching is exact — no case folding surprises.
        for action in STORE_ACTIONS {
            assert!(!store_role_can("superuser", action));
            assert!(!store_role_can("", action));
        }
        assert!(!store_role_can("owner", "delete_store"));
        assert!(!store_role_can("manager", "delete_store"));
        assert!(!store_role_can("Owner", "manage_catalog"));
    }

    #[test]
    fn store_role_validation_accepts_member_roles_only() {
        assert_eq!(validate_store_role("manager").unwrap(), "manager");
        assert_eq!(validate_store_role(" staff ").unwrap(), "staff");
        // `owner` is never a member row.
        assert!(validate_store_role("owner").is_err());
        assert!(validate_store_role("admin").is_err());
        assert!(validate_store_role("").is_err());
    }

    #[test]
    fn variant_name_validation_trims_and_bounds() {
        assert_eq!(validate_variant_name("  XL  ").unwrap(), "XL");
        assert_eq!(validate_variant_name("লাল").unwrap(), "লাল");
        assert!(validate_variant_name("").is_err());
        assert!(validate_variant_name("   ").is_err());
        let longest_ok = "x".repeat(MAX_VARIANT_NAME_CHARS);
        assert!(validate_variant_name(&longest_ok).is_ok());
        let too_long = "x".repeat(MAX_VARIANT_NAME_CHARS + 1);
        assert!(validate_variant_name(&too_long).is_err());
        // Bangla characters count as characters, not bytes.
        let bangla_ok = "ক".repeat(MAX_VARIANT_NAME_CHARS);
        assert!(validate_variant_name(&bangla_ok).is_ok());
    }

    #[test]
    fn variant_delta_validation_bounds_both_directions() {
        assert_eq!(validate_variant_delta(0).unwrap(), 0);
        assert_eq!(validate_variant_delta(5_000).unwrap(), 5_000);
        assert_eq!(validate_variant_delta(-5_000).unwrap(), -5_000);
        assert_eq!(
            validate_variant_delta(MAX_VARIANT_PRICE_DELTA_POISHA).unwrap(),
            MAX_VARIANT_PRICE_DELTA_POISHA
        );
        assert_eq!(
            validate_variant_delta(-MAX_VARIANT_PRICE_DELTA_POISHA).unwrap(),
            -MAX_VARIANT_PRICE_DELTA_POISHA
        );
        assert!(validate_variant_delta(MAX_VARIANT_PRICE_DELTA_POISHA + 1).is_err());
        assert!(validate_variant_delta(-MAX_VARIANT_PRICE_DELTA_POISHA - 1).is_err());
        assert!(validate_variant_delta(i64::MIN).is_err());
        assert!(validate_variant_delta(i64::MAX).is_err());
    }

    #[test]
    fn variant_final_price_must_stay_positive() {
        assert_eq!(variant_final_price(10_000, 0), Some(10_000));
        assert_eq!(variant_final_price(10_000, 2_500), Some(12_500));
        assert_eq!(variant_final_price(10_000, -2_500), Some(7_500));
        assert_eq!(variant_final_price(10_000, -9_999), Some(1));
        // Zero and negative final prices are refused.
        assert_eq!(variant_final_price(10_000, -10_000), None);
        assert_eq!(variant_final_price(10_000, -10_001), None);
        // Overflow is refused, never wrapped.
        assert_eq!(variant_final_price(i64::MAX, 1), None);
    }

    #[test]
    fn delivery_charge_first_kg_per_zone() {
        assert_eq!(calculate_delivery_charge(DeliveryZone::InsideDhaka, 1, 0), Ok(6_000));
        assert_eq!(calculate_delivery_charge(DeliveryZone::DhakaSuburb, 1, 0), Ok(10_000));
        assert_eq!(calculate_delivery_charge(DeliveryZone::OutsideDhaka, 1, 0), Ok(12_000));
    }

    #[test]
    fn delivery_charge_extra_weight() {
        // Outside Dhaka, 3 kg: 120 + 2 * 40 = 200 BDT.
        assert_eq!(calculate_delivery_charge(DeliveryZone::OutsideDhaka, 3, 0), Ok(20_000));
        // Zero weight is billed as 1 kg.
        assert_eq!(calculate_delivery_charge(DeliveryZone::InsideDhaka, 0, 0), Ok(6_000));
    }

    #[test]
    fn split_cart_groups_by_seller_and_preserves_order() {
        let cart = vec![
            item("store-a", "p1", 1, 1_000),
            item("store-b", "p2", 2, 500),
            item("store-a", "p3", 1, 250),
        ];
        let subs = split_cart_by_seller(&cart).expect("split succeeds");
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].store_id, "store-a");
        assert_eq!(subs[0].subtotal_poisha, 125_000);
        assert_eq!(subs[0].items.len(), 2);
        assert_eq!(subs[1].store_id, "store-b");
        assert_eq!(subs[1].subtotal_poisha, 100_000);
    }

    #[test]
    fn split_empty_cart_gives_no_sub_orders() {
        assert!(split_cart_by_seller(&[]).expect("empty split succeeds").is_empty());
    }

    #[test]
    fn commission_eight_percent() {
        let q = compute_commission(100_000, 800).expect("commission computes"); // 1000 BDT at 8%
        assert_eq!(q.commission_poisha, 8_000);
        assert_eq!(q.seller_payout_poisha, 92_000);
    }

    /// A raw-poisha cart line for the Wave 39 money-safety tests
    /// (the `item`/`witem` helpers take taka and scale, which
    /// cannot express the boundary values these tests need).
    fn raw_item(store: &str, quantity: u32, unit_price_poisha: i64) -> CartItem {
        CartItem {
            product_id: "p-raw".to_string(),
            store_id: store.to_string(),
            quantity,
            unit_price_poisha,
            unit_weight_grams: 1_000,
            variant_id: None,
        }
    }

    fn raw_group(items: Vec<CartItem>) -> OrderGroupInput {
        OrderGroupInput {
            store_id: "store-a".to_string(),
            items,
            discount_poisha: 0,
            commission_on_discounted: false,
            commission_rate_bps: None,
        }
    }

    #[test]
    fn money_safety_negative_inputs_are_errors_not_panics() {
        // Wave 39 (audit CR-11): a negative unit price used to
        // sail through the totals and detonate the assert inside
        // the delivery-charge rule. Every boundary now returns an
        // error value instead.
        let groups = vec![raw_group(vec![raw_item("store-a", 1, -1)])];
        assert!(build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).is_err());

        // A negative unit weight is rejected at the same boundary.
        let mut bad_weight = raw_item("store-a", 1, 1_000);
        bad_weight.unit_weight_grams = -5;
        let groups = vec![raw_group(vec![bad_weight])];
        assert!(build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).is_err());

        // The delivery rule itself: negative COD is an error,
        // never a panic.
        assert!(calculate_delivery_charge(DeliveryZone::InsideDhaka, 1, -1).is_err());

        // Commission: negative base and a rate above 100% are
        // errors (audit HI-19).
        assert!(compute_commission(-100, 800).is_err());
        assert!(compute_commission(100_000, 10_001).is_err());
    }

    #[test]
    fn money_safety_overflow_is_an_error_not_a_wrap() {
        // Wave 39 (audit CR-12): i64::MAX poisha x 2 must not wrap
        // into a negative line total.
        let huge = raw_item("store-a", 2, i64::MAX);
        assert!(huge.line_total_poisha().is_err());
        let groups = vec![raw_group(vec![huge])];
        assert!(build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).is_err());

        // Two individually-legal lines whose SUM overflows are an
        // error too, in both the totals and the cart split.
        let cart = vec![
            raw_item("store-a", 1, i64::MAX),
            raw_item("store-a", 1, i64::MAX),
        ];
        assert!(split_cart_by_seller(&cart).is_err());
        let groups = vec![raw_group(cart)];
        assert!(build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).is_err());

        // Commission on a near-max subtotal stays EXACT via the
        // i128 intermediate (an i64 multiplication would wrap):
        // the quote equals the true mathematical result.
        let q = compute_commission(i64::MAX, 800).expect("commission computes");
        assert_eq!(
            q.commission_poisha,
            (i128::from(i64::MAX) * 800 / 10_000) as i64
        );
        assert_eq!(q.commission_poisha + q.seller_payout_poisha, i64::MAX);
    }

    #[test]
    fn worked_example_from_mvp_plan() {
        // Doc example: goods 4,450 BDT from two sellers, delivery 240 BDT,
        // total payable 4,690 BDT; 8% commission on goods only = 356 BDT;
        // sellers together receive 4,450 - 356 = 4,094 BDT for goods.
        let cart = vec![
            item("store-a", "p1", 1, 2_450),
            item("store-b", "p2", 1, 2_000),
        ];
        let subs = split_cart_by_seller(&cart).expect("split succeeds");
        let goods_total: i64 = subs.iter().map(|s| s.subtotal_poisha).sum();
        assert_eq!(goods_total, 445_000);

        // Each seller ships one 1 kg parcel outside Dhaka: 120 BDT each.
        let delivery_total: i64 = subs
            .iter()
            .map(|_| calculate_delivery_charge(DeliveryZone::OutsideDhaka, 1, 0).expect("delivery charge"))
            .sum();
        assert_eq!(delivery_total, 24_000);
        assert_eq!(goods_total + delivery_total, 469_000); // 4,690 BDT payable

        let mut commission_total = 0;
        let mut payout_total = 0;
        for s in &subs {
            let q = compute_commission(s.subtotal_poisha, 800).expect("commission computes");
            commission_total += q.commission_poisha;
            payout_total += q.seller_payout_poisha;
        }
        assert_eq!(commission_total, 35_600); // 356 BDT
        assert_eq!(payout_total, 409_400); // 4,094 BDT
    }

    #[test]
    fn bd_phone_accepts_local_and_international_forms() {
        assert!(is_valid_bd_phone("01712345678"));
        assert!(is_valid_bd_phone("01312345678"));
        assert!(is_valid_bd_phone("01912345678"));
        assert!(is_valid_bd_phone("+8801712345678"));
    }

    #[test]
    fn bd_phone_rejects_bad_numbers() {
        assert!(!is_valid_bd_phone("01212345678")); // third digit 2 — no such prefix
        assert!(!is_valid_bd_phone("01012345678")); // third digit 0
        assert!(!is_valid_bd_phone("0171234567")); // 10 digits
        assert!(!is_valid_bd_phone("017123456789")); // 12 digits
        assert!(!is_valid_bd_phone("02712345678")); // landline-style prefix
        assert!(!is_valid_bd_phone("0171234567a")); // letter
        assert!(!is_valid_bd_phone("+8801212345678")); // bad prefix, intl form
        assert!(!is_valid_bd_phone(""));
    }

    #[test]
    fn bd_phone_normalizes_to_canonical_form() {
        assert_eq!(
            normalize_bd_phone("01712345678").as_deref(),
            Some("+8801712345678")
        );
        assert_eq!(
            normalize_bd_phone("8801712345678").as_deref(),
            Some("+8801712345678")
        );
        assert_eq!(
            normalize_bd_phone("+8801712345678").as_deref(),
            Some("+8801712345678")
        );
        assert_eq!(normalize_bd_phone(" 01712 345678 ").as_deref(), Some("+8801712345678"));
        assert_eq!(normalize_bd_phone("01212345678"), None);
    }

    #[test]
    fn rating_rules() {
        assert!(validate_rating(1));
        assert!(validate_rating(5));
        assert!(!validate_rating(0));
        assert!(!validate_rating(6));
        assert!(!validate_rating(-1));
    }

    #[test]
    fn review_body_rules() {
        // Optional: empty and whitespace-only bodies are allowed.
        assert!(validate_review_body("").is_ok());
        assert!(validate_review_body("   ").is_ok());
        assert!(validate_review_body("ভালো পণ্য").is_ok());
        assert!(validate_review_body(&"a".repeat(1_000)).is_ok());
        assert!(validate_review_body(&"a".repeat(1_001)).is_err());
        // Surrounding whitespace does not count towards the limit.
        assert!(validate_review_body(&format!("  {}  ", "a".repeat(1_000))).is_ok());
    }

    #[test]
    fn reply_body_rules() {
        assert!(validate_reply_body("ধন্যবাদ!").is_ok());
        assert!(validate_reply_body(&"a".repeat(500)).is_ok());
        assert!(validate_reply_body("").is_err());
        assert!(validate_reply_body("   ").is_err());
        assert!(validate_reply_body(&"a".repeat(501)).is_err());
    }

    #[test]
    fn question_body_rules() {
        // Too short: empty, whitespace-only and under 3 characters.
        assert!(validate_question_body("").is_err());
        assert!(validate_question_body("   ").is_err());
        assert!(validate_question_body("ab").is_err());
        assert!(validate_question_body("  ab  ").is_err());
        // Boundaries: exactly 3 and exactly 500 are fine, 501 is not.
        assert_eq!(validate_question_body("abc").as_deref(), Ok("abc"));
        assert!(validate_question_body(&"a".repeat(500)).is_ok());
        assert!(validate_question_body(&"a".repeat(501)).is_err());
        // Bangla text counts in characters, not bytes, and the
        // stored value is the trimmed body.
        assert_eq!(
            validate_question_body("  ডেলিভারি কতদিনে পাবো?  ").as_deref(),
            Ok("ডেলিভারি কতদিনে পাবো?")
        );
        assert!(validate_question_body(&"ক".repeat(500)).is_ok());
        assert!(validate_question_body(&"ক".repeat(501)).is_err());
    }

    #[test]
    fn answer_body_rules() {
        // Empty answers are meaningless; 1 char is enough.
        assert!(validate_answer_body("").is_err());
        assert!(validate_answer_body("   ").is_err());
        assert_eq!(validate_answer_body("হ্যাঁ").as_deref(), Ok("হ্যাঁ"));
        // Boundaries: exactly 1000 is fine, 1001 is not, and the
        // stored value is trimmed.
        assert!(validate_answer_body(&"a".repeat(1_000)).is_ok());
        assert!(validate_answer_body(&"a".repeat(1_001)).is_err());
        assert_eq!(
            validate_answer_body("  ২-৩ দিন লাগবে  ").as_deref(),
            Ok("২-৩ দিন লাগবে")
        );
    }

    #[test]
    fn phone_masking() {
        // Canonical numbers are shown in local form, middle hidden.
        assert_eq!(mask_phone("+8801712345678"), "017****678");
        // Local input stays local.
        assert_eq!(mask_phone("01712345678"), "017****678");
        // Too short to mask safely: fully hidden.
        assert_eq!(mask_phone("12345"), "****");
    }

    #[test]
    fn slug_rules() {
        assert!(is_valid_slug("rahim-store"));
        assert!(is_valid_slug("shop123"));
        assert!(is_valid_slug("abc"));
        assert!(!is_valid_slug("ab")); // too short
        assert!(!is_valid_slug("Rahim-Store")); // uppercase
        assert!(!is_valid_slug("rahim_store")); // underscore
        assert!(!is_valid_slug("-rahim")); // starts with hyphen
        assert!(!is_valid_slug("rahim-")); // ends with hyphen
        assert!(!is_valid_slug(&"a".repeat(41))); // too long
    }

    #[test]
    fn product_input_validation() {
        let ok = ProductInput {
            title: "Panjabi".to_string(),
            price_poisha: 150_000,
            stock: 10,
            status: "active".to_string(),
            weight_grams: 1_000,
        };
        assert!(validate_product_input(&ok).is_ok());

        let mut bad = ok.clone();
        bad.title = "  ".to_string();
        assert!(validate_product_input(&bad).is_err());

        let mut bad = ok.clone();
        bad.price_poisha = -1;
        assert!(validate_product_input(&bad).is_err());

        let mut bad = ok.clone();
        bad.stock = -1;
        assert!(validate_product_input(&bad).is_err());

        let mut bad = ok.clone();
        bad.weight_grams = 0;
        assert!(validate_product_input(&bad).is_err());

        let mut bad = ok;
        bad.status = "live".to_string();
        assert!(validate_product_input(&bad).is_err());
    }

    #[test]
    fn weight_grams_bounds() {
        assert_eq!(validate_weight_grams(1), Ok(1));
        assert_eq!(validate_weight_grams(1_000), Ok(1_000));
        assert_eq!(validate_weight_grams(50_000), Ok(50_000));
        assert!(validate_weight_grams(0).is_err());
        assert!(validate_weight_grams(50_001).is_err());
        assert!(validate_weight_grams(-250).is_err());
    }

    #[test]
    fn grams_convert_to_kilograms() {
        assert_eq!(weight_kg_from_grams(0), 0.0);
        assert_eq!(weight_kg_from_grams(-1), 0.0);
        assert_eq!(weight_kg_from_grams(500), 0.5);
        assert_eq!(weight_kg_from_grams(1_000), 1.0);
        assert_eq!(weight_kg_from_grams(2_500), 2.5);
        assert_eq!(weight_kg_from_grams(5_000), 5.0);
    }

    #[test]
    fn order_totals_bill_group_weight_from_item_grams() {
        // One seller, mixed weights: 2 x 500 g + 1 x 2,500 g = 3,500 g
        // -> billed as 4 kg (per started kilogram). Outside Dhaka:
        // 120 + 3*40 = 240 BDT delivery. Goods: 2 x 1,000 + 2,000 =
        // 4,000 BDT; 8% commission = 320 BDT.
        let groups = vec![group(
            "store-a",
            vec![
                witem("store-a", "p1", 2, 1_000, 500),
                witem("store-a", "p2", 1, 2_000, 2_500),
            ],
        )];
        let totals = build_order_totals(&groups, DeliveryZone::OutsideDhaka, 800).expect("totals compute");
        assert_eq!(totals.goods_total_poisha, 400_000);
        assert_eq!(totals.delivery_total_poisha, 24_000);
        assert_eq!(totals.grand_total_poisha, 424_000);
        assert_eq!(totals.sub_orders[0].delivery_charge_poisha, 24_000);
        assert_eq!(totals.sub_orders[0].commission_poisha, 32_000);
        assert_eq!(totals.sub_orders[0].seller_payout_poisha, 368_000);

        // The same items inside Dhaka at 4 kg: 60 + 3*20 = 120 BDT.
        let totals = build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).expect("totals compute");
        assert_eq!(totals.delivery_total_poisha, 12_000);

        // An exact whole number of kilograms is NOT rounded further:
        // 2 x 2,500 g = 5,000 g -> exactly 5 kg, outside Dhaka:
        // 120 + 4*40 = 280 BDT.
        let groups = vec![group(
            "store-a",
            vec![witem("store-a", "p1", 2, 1_000, 2_500)],
        )];
        let totals = build_order_totals(&groups, DeliveryZone::OutsideDhaka, 800).expect("totals compute");
        assert_eq!(totals.delivery_total_poisha, 28_000);
    }

    #[test]
    fn buyer_cancel_requires_every_sub_order_placed() {
        assert!(can_buyer_cancel(&["placed"]));
        assert!(can_buyer_cancel(&["placed", "placed", "placed"]));
        assert!(!can_buyer_cancel(&[]));
        assert!(!can_buyer_cancel(&["placed", "confirmed"]));
        assert!(!can_buyer_cancel(&["confirmed"]));
        assert!(!can_buyer_cancel(&["placed", "shipped"]));
        assert!(!can_buyer_cancel(&["placed", "cancelled"]));
        assert!(!can_buyer_cancel(&["delivered"]));
    }

    #[test]
    fn delivery_zone_string_round_trip() {
        for zone in [
            DeliveryZone::InsideDhaka,
            DeliveryZone::DhakaSuburb,
            DeliveryZone::OutsideDhaka,
        ] {
            assert_eq!(parse_delivery_zone(zone.as_str()), Some(zone));
        }
        assert_eq!(DeliveryZone::InsideDhaka.as_str(), "inside_dhaka");
        assert_eq!(DeliveryZone::DhakaSuburb.as_str(), "dhaka_suburb");
        assert_eq!(DeliveryZone::OutsideDhaka.as_str(), "outside_dhaka");
        assert_eq!(parse_delivery_zone("InsideDhaka"), None);
        assert_eq!(parse_delivery_zone("dhaka"), None);
        assert_eq!(parse_delivery_zone(""), None);
    }

    #[test]
    fn cart_quantity_bounds() {
        assert_eq!(validate_cart_quantity(1), Ok(1));
        assert_eq!(validate_cart_quantity(99), Ok(99));
        assert!(validate_cart_quantity(0).is_err());
        assert!(validate_cart_quantity(100).is_err());
        assert!(validate_cart_quantity(-3).is_err());
    }

    fn group(store: &str, items: Vec<CartItem>) -> OrderGroupInput {
        OrderGroupInput {
            store_id: store.to_string(),
            items,
            discount_poisha: 0,
            commission_on_discounted: false,
            commission_rate_bps: None,
        }
    }

    fn discounted_group(
        store: &str,
        items: Vec<CartItem>,
        discount_poisha: i64,
        commission_on_discounted: bool,
    ) -> OrderGroupInput {
        OrderGroupInput {
            store_id: store.to_string(),
            items,
            discount_poisha,
            commission_on_discounted,
            commission_rate_bps: None,
        }
    }

    #[test]
    fn order_totals_two_sellers_match_worked_example() {
        // The MVP-plan worked example, through build_order_totals:
        // two sellers, goods 2,450 + 2,000 BDT, one 1 kg parcel each,
        // outside Dhaka -> delivery 120 BDT per seller.
        let groups = vec![
            group("store-a", vec![item("store-a", "p1", 1, 2_450)]),
            group("store-b", vec![item("store-b", "p2", 1, 2_000)]),
        ];
        let totals = build_order_totals(&groups, DeliveryZone::OutsideDhaka, 800).expect("totals compute");
        assert_eq!(totals.goods_total_poisha, 445_000);
        assert_eq!(totals.delivery_total_poisha, 24_000);
        assert_eq!(totals.grand_total_poisha, 469_000); // 4,690 BDT
        assert_eq!(totals.sub_orders.len(), 2);
        assert_eq!(totals.sub_orders[0].goods_subtotal_poisha, 245_000);
        assert_eq!(totals.sub_orders[0].delivery_charge_poisha, 12_000);
        assert_eq!(totals.sub_orders[0].commission_poisha, 19_600); // 8% of 2,450
        assert_eq!(totals.sub_orders[0].seller_payout_poisha, 225_400);
        assert_eq!(totals.sub_orders[1].commission_poisha, 16_000); // 8% of 2,000
        assert_eq!(totals.sub_orders[1].seller_payout_poisha, 184_000);
        let payout_sum: i64 = totals
            .sub_orders
            .iter()
            .map(|s| s.seller_payout_poisha)
            .sum();
        assert_eq!(payout_sum, 409_400); // sellers receive 4,094 BDT for goods
    }

    #[test]
    fn order_totals_single_seller_with_weight_and_quantities() {
        // One seller: 2 x 1,500 BDT + 1 x 500 BDT = 3,500 BDT goods,
        // 3 units at the default 1,000 g each -> 3 kg, inside Dhaka:
        // 60 + 2*20 = 100 BDT delivery.
        let groups = vec![group(
            "store-a",
            vec![
                item("store-a", "p1", 2, 1_500),
                item("store-a", "p2", 1, 500),
            ],
        )];
        let totals = build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).expect("totals compute");
        assert_eq!(totals.goods_total_poisha, 350_000);
        assert_eq!(totals.delivery_total_poisha, 10_000);
        assert_eq!(totals.grand_total_poisha, 360_000);
        assert_eq!(totals.sub_orders[0].commission_poisha, 28_000);
        assert_eq!(totals.sub_orders[0].seller_payout_poisha, 322_000);
    }

    #[test]
    fn order_totals_platform_coupon_keeps_commission_on_full_goods() {
        // Platform coupon: the platform bears the discount, so
        // commission stays on the FULL goods while the payout and
        // the grand total drop by the discount.
        let groups = vec![
            discounted_group("store-a", vec![item("store-a", "p1", 1, 2_450)], 2_753, false),
            discounted_group("store-b", vec![item("store-b", "p2", 1, 2_000)], 2_247, false),
        ];
        let totals = build_order_totals(&groups, DeliveryZone::OutsideDhaka, 800).expect("totals compute");
        assert_eq!(totals.discount_total_poisha, 5_000);
        assert_eq!(totals.grand_total_poisha, 469_000 - 5_000);
        // Commission unchanged vs the no-coupon worked example.
        assert_eq!(totals.sub_orders[0].commission_poisha, 19_600);
        assert_eq!(totals.sub_orders[1].commission_poisha, 16_000);
        // Payout = goods - discount - commission.
        assert_eq!(totals.sub_orders[0].seller_payout_poisha, 245_000 - 2_753 - 19_600);
        assert_eq!(totals.sub_orders[1].seller_payout_poisha, 200_000 - 2_247 - 16_000);
        assert_eq!(totals.sub_orders[0].discount_poisha, 2_753);
    }

    #[test]
    fn order_totals_store_coupon_moves_commission_to_discounted_goods() {
        // Store coupon: the seller bears the discount, so store
        // A's commission is 8% of (245,000 - 10,000) = 18,800 and
        // store B is completely untouched.
        let groups = vec![
            discounted_group("store-a", vec![item("store-a", "p1", 1, 2_450)], 10_000, true),
            group("store-b", vec![item("store-b", "p2", 1, 2_000)]),
        ];
        let totals = build_order_totals(&groups, DeliveryZone::OutsideDhaka, 800).expect("totals compute");
        assert_eq!(totals.discount_total_poisha, 10_000);
        assert_eq!(totals.sub_orders[0].commission_poisha, 18_800);
        assert_eq!(totals.sub_orders[0].seller_payout_poisha, 245_000 - 10_000 - 18_800);
        assert_eq!(totals.sub_orders[1].discount_poisha, 0);
        assert_eq!(totals.sub_orders[1].commission_poisha, 16_000);
        assert_eq!(totals.sub_orders[1].seller_payout_poisha, 184_000);
        assert_eq!(totals.grand_total_poisha, 469_000 - 10_000);
    }

    #[test]
    fn coupon_code_normalization_and_validation() {
        assert_eq!(normalize_coupon_code(" eid10 "), "EID10");
        assert_eq!(normalize_coupon_code("save-20_x"), "SAVE-20_X");

        // Happy paths.
        assert!(validate_coupon_input("EID10", "percent", 10, 0, Some(5_000)).is_ok());
        assert!(validate_coupon_input("FLAT100", "fixed", 10_000, 50_000, None).is_ok());
        assert!(validate_coupon_input("A1-", "percent", 90, 0, None).is_ok());

        // Code shape: too short, too long, illegal characters.
        assert!(validate_coupon_input("AB", "percent", 10, 0, None).is_err());
        assert!(validate_coupon_input(&"A".repeat(33), "percent", 10, 0, None).is_err());
        assert!(validate_coupon_input("BAD CODE", "percent", 10, 0, None).is_err());
        assert!(validate_coupon_input("কুপন", "percent", 10, 0, None).is_err());

        // Percent bounds: 1..=90.
        assert!(validate_coupon_input("P0", "percent", 0, 0, None).is_err());
        assert!(validate_coupon_input("P91", "percent", 91, 0, None).is_err());
        assert!(validate_coupon_input("P100", "percent", 100, 0, None).is_err());

        // Fixed floor: at least 100 poisha.
        assert!(validate_coupon_input("F99", "fixed", 99, 0, None).is_err());
        assert!(validate_coupon_input("F100", "fixed", 100, 0, None).is_ok());

        // Unknown type, negative minimum order, negative cap.
        assert!(validate_coupon_input("XYZ", "bogus", 10, 0, None).is_err());
        assert!(validate_coupon_input("XYZ", "percent", 10, -1, None).is_err());
        assert!(validate_coupon_input("XYZ", "percent", 10, 0, Some(-1)).is_err());
    }

    #[test]
    fn coupon_discount_computation() {
        // Percent, uncapped and floored: 10% of 445,000 = 44,500.
        assert_eq!(compute_coupon_discount("percent", 10, None, 445_000), 44_500);
        // Percent capped: the Wave 14 checkout example — cap 5,000 wins.
        assert_eq!(
            compute_coupon_discount("percent", 10, Some(5_000), 445_000),
            5_000
        );
        // Percent floor: 10% of 999 = 99 (not 100).
        assert_eq!(compute_coupon_discount("percent", 10, None, 999), 99);
        // A cap above the subtotal never inflates past the subtotal.
        assert_eq!(
            compute_coupon_discount("percent", 90, Some(9_999_999), 10_000),
            9_000
        );
        // Fixed: the value, or the subtotal when the value exceeds it.
        assert_eq!(compute_coupon_discount("fixed", 10_000, None, 245_000), 10_000);
        assert_eq!(compute_coupon_discount("fixed", 10_000, None, 8_000), 8_000);
        // Zero eligible subtotal and unknown types give zero.
        assert_eq!(compute_coupon_discount("percent", 10, None, 0), 0);
        assert_eq!(compute_coupon_discount("fixed", 10_000, None, 0), 0);
        assert_eq!(compute_coupon_discount("bogus", 10, None, 10_000), 0);
    }

    #[test]
    fn discount_allocation_sums_exactly() {
        // The checkout example: 5,000 over [245,000, 200,000] —
        // floors are 2,752 and 2,247; the leftover poisha goes to
        // the largest subtotal (store A).
        assert_eq!(
            allocate_discount(&[245_000, 200_000], 5_000),
            vec![2_753, 2_247]
        );
        // Exact division leaves no remainder.
        assert_eq!(allocate_discount(&[100, 100], 50), vec![25, 25]);
        // Ties: the earliest largest group takes the remainder.
        assert_eq!(allocate_discount(&[100, 100], 51), vec![26, 25]);
        // A single group takes everything.
        assert_eq!(allocate_discount(&[245_000], 10_000), vec![10_000]);
        // Zero discount, no groups, all-zero subtotals.
        assert_eq!(allocate_discount(&[100, 200], 0), vec![0, 0]);
        assert!(allocate_discount(&[], 1_000).is_empty());
        assert_eq!(allocate_discount(&[0, 0], 1_000), vec![0, 0]);
        // Zero-subtotal groups never receive a share.
        assert_eq!(allocate_discount(&[0, 300], 90), vec![0, 90]);
        // Near-total discount stays within every subtotal.
        assert_eq!(
            allocate_discount(&[100, 100, 100], 299),
            vec![100, 100, 99]
        );
        // A discount above the total is clamped to it.
        assert_eq!(allocate_discount(&[100, 50], 1_000), vec![100, 50]);
    }

    #[test]
    fn loyalty_points_earned_per_full_hundred_taka() {
        // The standard flow: 200_000 poisha of net goods earns 20.
        assert_eq!(loyalty_points_for_goods(200_000), 20);
        // Floored: a partial 100 taka earns nothing extra.
        assert_eq!(loyalty_points_for_goods(199_999), 19);
        assert_eq!(loyalty_points_for_goods(10_000), 1);
        assert_eq!(loyalty_points_for_goods(9_999), 0);
        // Zero and negative nets earn nothing.
        assert_eq!(loyalty_points_for_goods(0), 0);
        assert_eq!(loyalty_points_for_goods(-50_000), 0);
    }

    #[test]
    fn loyalty_discount_is_points_times_one_taka() {
        assert_eq!(loyalty_discount_poisha(0), Some(0));
        assert_eq!(loyalty_discount_poisha(15), Some(1_500));
        assert_eq!(loyalty_discount_poisha(1_000), Some(100_000));
        // Negative counts and overflow buy nothing.
        assert_eq!(loyalty_discount_poisha(-1), None);
        assert_eq!(loyalty_discount_poisha(i64::MAX), None);
    }

    #[test]
    fn loyalty_redemption_is_capped_at_half_the_goods() {
        // Balance binds when it is the smaller side.
        assert_eq!(loyalty_max_redeemable_points(15, 200_000), 15);
        // The cap binds: half of 200_000 goods is 100_000 poisha
        // = 1_000 points, however large the balance or request.
        assert_eq!(loyalty_max_redeemable_points(10_000, 200_000), 1_000);
        // The cap floors twice: half of 199 poisha is 99, which
        // buys no whole point.
        assert_eq!(loyalty_max_redeemable_points(10, 199), 0);
        // Half of 300 is 150 poisha = exactly 1 point.
        assert_eq!(loyalty_max_redeemable_points(10, 300), 1);
        // No balance or no goods: nothing redeemable.
        assert_eq!(loyalty_max_redeemable_points(0, 200_000), 0);
        assert_eq!(loyalty_max_redeemable_points(-5, 200_000), 0);
        assert_eq!(loyalty_max_redeemable_points(100, 0), 0);
        assert_eq!(loyalty_max_redeemable_points(100, -1), 0);
    }

    #[test]
    fn loyalty_request_validation() {
        assert!(validate_loyalty_request(0).is_ok());
        assert!(validate_loyalty_request(250).is_ok());
        assert!(validate_loyalty_request(-1).is_err());
    }

    #[test]
    fn order_totals_empty_groups_are_zero() {
        let totals = build_order_totals(&[], DeliveryZone::DhakaSuburb, 800).expect("totals compute");
        assert!(totals.sub_orders.is_empty());
        assert_eq!(totals.goods_total_poisha, 0);
        assert_eq!(totals.delivery_total_poisha, 0);
        assert_eq!(totals.grand_total_poisha, 0);
    }

    #[test]
    fn payment_transition_table() {
        // The allowed moves.
        for (from, to) in [
            ("pending", "paid"),
            ("pending", "failed"),
            ("paid", "refunded"),
            ("failed", "pending"),
        ] {
            assert!(can_transition_payment(from, to), "{from} -> {to}");
        }
        // Everything else is rejected: same-state, backwards, skipping,
        // moves out of refunded, the COD order-level status, unknowns.
        for (from, to) in [
            ("pending", "pending"),
            ("pending", "refunded"),
            ("paid", "pending"),
            ("paid", "failed"),
            ("paid", "paid"),
            ("failed", "paid"),
            ("failed", "refunded"),
            ("failed", "failed"),
            ("refunded", "pending"),
            ("refunded", "paid"),
            ("pay_on_delivery", "paid"),
            ("bogus", "paid"),
            ("pending", "bogus"),
        ] {
            assert!(!can_transition_payment(from, to), "{from} -> {to}");
        }
        assert!(is_valid_payment_status("pending"));
        assert!(is_valid_payment_status("refunded"));
        assert!(!is_valid_payment_status("pay_on_delivery"));
        assert!(!is_valid_payment_status("bogus"));
    }

    #[test]
    fn callback_amount_must_match_exactly() {
        assert!(validate_callback_amount(469_000, 469_000));
        assert!(validate_callback_amount(0, 0));
        assert!(!validate_callback_amount(469_000, 469_001));
        assert!(!validate_callback_amount(469_000, 468_999));
        assert!(!validate_callback_amount(469_000, 0));
        assert!(!validate_callback_amount(0, 100));
    }

    #[test]
    fn sub_order_transition_table() {
        // The allowed forward/cancel/return moves.
        for (from, to) in [
            ("placed", "confirmed"),
            ("confirmed", "shipped"),
            ("shipped", "delivered"),
            ("placed", "cancelled"),
            ("confirmed", "cancelled"),
            ("shipped", "returned"),
        ] {
            assert!(can_transition_sub_order(from, to), "{from} -> {to}");
        }
        // Everything else is rejected: skips, backwards, same-state,
        // moves out of terminal states, unknown statuses.
        for (from, to) in [
            ("placed", "shipped"),
            ("placed", "delivered"),
            ("placed", "returned"),
            ("placed", "placed"),
            ("confirmed", "delivered"),
            ("confirmed", "returned"),
            ("confirmed", "placed"),
            ("shipped", "cancelled"),
            ("shipped", "confirmed"),
            ("delivered", "returned"),
            ("delivered", "cancelled"),
            ("cancelled", "confirmed"),
            ("returned", "shipped"),
            ("bogus", "confirmed"),
            ("placed", "bogus"),
        ] {
            assert!(!can_transition_sub_order(from, to), "{from} -> {to}");
        }
        assert!(is_valid_sub_order_status("placed"));
        assert!(is_valid_sub_order_status("returned"));
        assert!(!is_valid_sub_order_status("bogus"));
    }

    #[test]
    fn shipment_status_set() {
        for status in [
            "created",
            "picked_up",
            "in_transit",
            "out_for_delivery",
            "delivered",
            "partial_delivered",
            "return_in_transit",
            "returned",
            "cancelled",
            "unknown",
        ] {
            assert!(is_valid_shipment_status(status), "{status}");
        }
        assert!(!is_valid_shipment_status("pending"));
        assert!(!is_valid_shipment_status("shipped"));
        assert!(!is_valid_shipment_status("bogus"));
        assert!(!is_valid_shipment_status(""));
    }

    #[test]
    fn shipment_transition_table() {
        // The allowed moves: forward flow, partial delivery, the
        // return path, and cancellation of any non-final parcel.
        for (from, to) in [
            ("created", "picked_up"),
            ("picked_up", "in_transit"),
            ("in_transit", "out_for_delivery"),
            ("out_for_delivery", "delivered"),
            ("out_for_delivery", "partial_delivered"),
            ("partial_delivered", "delivered"),
            ("partial_delivered", "returned"),
            ("delivered", "return_in_transit"),
            ("return_in_transit", "returned"),
            ("created", "cancelled"),
            ("picked_up", "cancelled"),
            ("in_transit", "cancelled"),
            ("out_for_delivery", "cancelled"),
            ("partial_delivered", "cancelled"),
            ("return_in_transit", "cancelled"),
        ] {
            assert!(can_transition_shipment(from, to), "{from} -> {to}");
        }
        // Everything else is rejected: skips, backwards, same-state,
        // cancelling a delivered parcel, moves out of the final
        // states, and anything touching `unknown`.
        for (from, to) in [
            ("created", "in_transit"),
            ("created", "delivered"),
            ("created", "returned"),
            ("created", "created"),
            ("picked_up", "delivered"),
            ("picked_up", "created"),
            ("in_transit", "delivered"),
            ("in_transit", "picked_up"),
            ("out_for_delivery", "in_transit"),
            ("out_for_delivery", "returned"),
            ("delivered", "cancelled"),
            ("delivered", "delivered"),
            ("delivered", "returned"),
            ("partial_delivered", "in_transit"),
            ("return_in_transit", "delivered"),
            ("returned", "cancelled"),
            ("returned", "return_in_transit"),
            ("cancelled", "picked_up"),
            ("cancelled", "cancelled"),
            ("unknown", "picked_up"),
            ("unknown", "cancelled"),
            ("created", "unknown"),
            ("delivered", "unknown"),
            ("bogus", "picked_up"),
            ("created", "bogus"),
        ] {
            assert!(!can_transition_shipment(from, to), "{from} -> {to}");
        }
    }

    #[test]
    fn fulfillment_bucket_mapping() {
        use FulfillmentBucket::*;
        // The forward flow: placed waits for confirmation, a
        // confirmed sub-order without a shipment waits for a
        // booking, shipped is in the pipeline, delivered is done.
        assert_eq!(fulfillment_bucket("placed", false, None), ToConfirm);
        assert_eq!(fulfillment_bucket("confirmed", false, None), ToShip);
        assert_eq!(
            fulfillment_bucket("confirmed", true, Some("created")),
            InTransit
        );
        assert_eq!(
            fulfillment_bucket("shipped", true, Some("created")),
            InTransit
        );
        assert_eq!(
            fulfillment_bucket("shipped", true, Some("picked_up")),
            InTransit
        );
        assert_eq!(
            fulfillment_bucket("shipped", true, Some("in_transit")),
            InTransit
        );
        assert_eq!(
            fulfillment_bucket("shipped", true, Some("out_for_delivery")),
            InTransit
        );
        assert_eq!(
            fulfillment_bucket("shipped", true, Some("partial_delivered")),
            InTransit
        );
        // An `unknown` courier report does not change the pile:
        // the sub-order's own status stays in charge.
        assert_eq!(
            fulfillment_bucket("shipped", true, Some("unknown")),
            InTransit
        );
        assert_eq!(
            fulfillment_bucket("delivered", true, Some("delivered")),
            Done
        );
        assert_eq!(fulfillment_bucket("delivered", false, None), Done);
        // Everything that came back or was cancelled needs a
        // human — by sub-order status OR by shipment status,
        // and the shipment verdict wins over a `delivered`
        // sub-order (the parcel came back after delivery).
        assert_eq!(fulfillment_bucket("returned", false, None), Attention);
        assert_eq!(fulfillment_bucket("cancelled", false, None), Attention);
        assert_eq!(
            fulfillment_bucket("shipped", true, Some("returned")),
            Attention
        );
        assert_eq!(
            fulfillment_bucket("shipped", true, Some("cancelled")),
            Attention
        );
        assert_eq!(
            fulfillment_bucket("shipped", true, Some("return_in_transit")),
            Attention
        );
        assert_eq!(
            fulfillment_bucket("delivered", true, Some("return_in_transit")),
            Attention
        );
        assert_eq!(
            fulfillment_bucket("delivered", true, Some("returned")),
            Attention
        );
        // A placed/confirmed sub-order keeps its actionable
        // bucket even if a stale shipment row claims a moving
        // state — only the three failure states override.
        assert_eq!(
            fulfillment_bucket("placed", true, Some("in_transit")),
            ToConfirm
        );
        // An unrecognised sub-order status fails visible.
        assert_eq!(fulfillment_bucket("bogus", false, None), Attention);
        assert_eq!(fulfillment_bucket("", false, None), Attention);
        // The wire strings.
        assert_eq!(ToConfirm.as_str(), "to_confirm");
        assert_eq!(ToShip.as_str(), "to_ship");
        assert_eq!(InTransit.as_str(), "in_transit");
        assert_eq!(Done.as_str(), "done");
        assert_eq!(Attention.as_str(), "attention");
    }

    #[test]
    fn fulfillment_sla_breach_boundaries() {
        use FulfillmentBucket::*;
        // Confirm SLA 24h: below is fine, AT the limit the
        // promise is already broken, above is breached.
        assert!(!sla_breached(ToConfirm, 0, 24, 48));
        assert!(!sla_breached(ToConfirm, 23, 24, 48));
        assert!(sla_breached(ToConfirm, 24, 24, 48));
        assert!(sla_breached(ToConfirm, 100, 24, 48));
        // Ship SLA 48h, independent of the confirm limit.
        assert!(!sla_breached(ToShip, 24, 24, 48));
        assert!(!sla_breached(ToShip, 47, 24, 48));
        assert!(sla_breached(ToShip, 48, 24, 48));
        // The other buckets never breach, at any age.
        for bucket in [InTransit, Done, Attention] {
            assert!(!sla_breached(bucket, 10_000, 24, 48), "{bucket:?}");
        }
        // A negative age (clock skew) breaches nothing.
        assert!(!sla_breached(ToConfirm, -5, 24, 48));
        assert!(!sla_breached(ToShip, -5, 24, 48));
    }

    #[test]
    fn fulfillment_sla_hours_validation() {
        assert_eq!(DEFAULT_FULFILLMENT_CONFIRM_SLA_HOURS, 24);
        assert_eq!(DEFAULT_FULFILLMENT_SHIP_SLA_HOURS, 48);
        assert!(validate_fulfillment_sla_hours(1));
        assert!(validate_fulfillment_sla_hours(24));
        assert!(validate_fulfillment_sla_hours(720));
        assert!(!validate_fulfillment_sla_hours(0));
        assert!(!validate_fulfillment_sla_hours(-1));
        assert!(!validate_fulfillment_sla_hours(721));
    }

    #[test]
    fn manual_courier_validation() {
        // Every canonical key validates to itself, with its
        // display name attached.
        for (key, display) in MANUAL_COURIERS {
            assert_eq!(validate_manual_courier(key), Ok(key));
            assert_eq!(manual_courier_display_name(key), Some(display));
        }
        // Trimmed and case-insensitive on the key.
        assert_eq!(validate_manual_courier("  Sundarban "), Ok("sundarban"));
        assert_eq!(validate_manual_courier("SA_PARIBAHAN"), Ok("sa_paribahan"));
        assert_eq!(validate_manual_courier("Karatoa"), Ok("karatoa"));
        assert_eq!(validate_manual_courier("CARRYBEE"), Ok("carrybee"));
        // API couriers, display names, blanks and unknowns are
        // not manual couriers.
        assert!(validate_manual_courier("steadfast").is_err());
        assert!(validate_manual_courier("Sundarban Courier").is_err());
        assert!(validate_manual_courier("").is_err());
        assert!(validate_manual_courier("   ").is_err());
        assert!(validate_manual_courier("sundarbans").is_err());
        assert_eq!(manual_courier_display_name("steadfast"), None);
        assert_eq!(manual_courier_display_name("bogus"), None);
    }

    #[test]
    fn manual_tracking_id_validation() {
        // Absent and blank both mean "not recorded yet".
        assert_eq!(validate_manual_tracking_id(None), Ok(None));
        assert_eq!(validate_manual_tracking_id(Some("")), Ok(None));
        assert_eq!(validate_manual_tracking_id(Some("   ")), Ok(None));
        // A real receipt number is kept verbatim, trimmed.
        assert_eq!(
            validate_manual_tracking_id(Some("SB-12345")),
            Ok(Some("SB-12345".to_string()))
        );
        assert_eq!(
            validate_manual_tracking_id(Some("  KT-9999  ")),
            Ok(Some("KT-9999".to_string()))
        );
        // Boundary lengths: 4 and 64 pass, 3 and 65 fail.
        assert!(validate_manual_tracking_id(Some("A1B2")).is_ok());
        assert!(validate_manual_tracking_id(Some(&"X".repeat(64))).is_ok());
        assert!(validate_manual_tracking_id(Some("A1B")).is_err());
        assert!(validate_manual_tracking_id(Some(&"X".repeat(65))).is_err());
        // No whitespace inside, however it is written.
        assert!(validate_manual_tracking_id(Some("SB 12345")).is_err());
        assert!(validate_manual_tracking_id(Some("SB\t12345")).is_err());
    }

    #[test]
    fn steadfast_status_mapping() {
        // Every documented Steadfast delivery_status value.
        assert_eq!(map_steadfast_status("pending"), "created");
        assert_eq!(map_steadfast_status("delivered"), "delivered");
        assert_eq!(map_steadfast_status("partial_delivered"), "partial_delivered");
        assert_eq!(map_steadfast_status("cancelled"), "cancelled");
        assert_eq!(map_steadfast_status("returned"), "returned");
        assert_eq!(map_steadfast_status("hold"), "in_transit");
        // Unrecognised values are honestly unknown, never guessed.
        assert_eq!(map_steadfast_status("in_review"), "unknown");
        assert_eq!(map_steadfast_status(""), "unknown");
        assert_eq!(map_steadfast_status("bogus"), "unknown");
        // Whitespace and case do not change the answer.
        assert_eq!(map_steadfast_status(" Delivered "), "delivered");
        assert_eq!(map_steadfast_status("PENDING"), "created");
        // Every mapping result is a member of the unified set.
        for raw in [
            "pending",
            "delivered",
            "partial_delivered",
            "cancelled",
            "returned",
            "hold",
            "whatever",
        ] {
            assert!(is_valid_shipment_status(map_steadfast_status(raw)), "{raw}");
        }
    }

    #[test]
    fn courier_status_normalization() {
        // The one normalization rule every mapping shares: trim,
        // lowercase, spaces and hyphens fold to underscores.
        assert_eq!(normalize_courier_status(" Delivered "), "delivered");
        assert_eq!(normalize_courier_status("Out for Delivery"), "out_for_delivery");
        assert_eq!(normalize_courier_status("out-for-delivery"), "out_for_delivery");
        assert_eq!(normalize_courier_status("OUT_FOR_DELIVERY"), "out_for_delivery");
        // CamelCase splits at word boundaries; an all-caps word
        // stays one word.
        assert_eq!(normalize_courier_status("InTransit"), "in_transit");
        assert_eq!(normalize_courier_status("PickupRequested"), "pickup_requested");
        assert_eq!(normalize_courier_status("PENDING"), "pending");
        assert_eq!(normalize_courier_status(""), "");
        // Refactoring map_steadfast_status onto the helper changed
        // none of its results (also covered by the test above).
        assert_eq!(map_steadfast_status("Partial Delivered"), "partial_delivered");
        assert_eq!(map_steadfast_status("HOLD"), "in_transit");
    }

    #[test]
    fn pathao_status_mapping() {
        // Every documented Pathao order_status value.
        assert_eq!(map_pathao_status("Pending"), "created");
        assert_eq!(map_pathao_status("PickupRequested"), "created");
        assert_eq!(map_pathao_status("PickupFailed"), "created");
        assert_eq!(map_pathao_status("Assigned"), "picked_up");
        assert_eq!(map_pathao_status("Picked"), "picked_up");
        assert_eq!(map_pathao_status("InTransit"), "in_transit");
        assert_eq!(map_pathao_status("OnHold"), "in_transit");
        assert_eq!(map_pathao_status("FailedDelivery"), "out_for_delivery");
        assert_eq!(map_pathao_status("Delivered"), "delivered");
        assert_eq!(map_pathao_status("PartialDelivered"), "partial_delivered");
        assert_eq!(map_pathao_status("ReturnRequested"), "return_in_transit");
        assert_eq!(map_pathao_status("Returned"), "returned");
        assert_eq!(map_pathao_status("Cancelled"), "cancelled");
        // Normalization: case, spaces, underscores and hyphens are
        // all the same value.
        assert_eq!(map_pathao_status("in_transit"), "in_transit");
        assert_eq!(map_pathao_status("IN TRANSIT"), "in_transit");
        assert_eq!(map_pathao_status("in-transit"), "in_transit");
        assert_eq!(map_pathao_status(" return requested "), "return_in_transit");
        // Unrecognised values are honestly unknown, never guessed.
        assert_eq!(map_pathao_status(""), "unknown");
        assert_eq!(map_pathao_status("bogus"), "unknown");
        assert_eq!(map_pathao_status("PaymentDone"), "unknown");
        for raw in ["Pending", "Picked", "Delivered", "Returned", "whatever"] {
            assert!(is_valid_shipment_status(map_pathao_status(raw)), "{raw}");
        }
    }

    #[test]
    fn redx_status_mapping() {
        // The RedX human-readable tracking strings.
        assert_eq!(map_redx_status("created"), "created");
        assert_eq!(map_redx_status("package is ready for delivery"), "created");
        assert_eq!(map_redx_status("picked-up"), "picked_up");
        assert_eq!(map_redx_status("picked"), "picked_up");
        assert_eq!(map_redx_status("in-transit"), "in_transit");
        assert_eq!(map_redx_status("at-hub"), "in_transit");
        assert_eq!(map_redx_status("received at hub"), "in_transit");
        assert_eq!(map_redx_status("on-hold"), "in_transit");
        assert_eq!(map_redx_status("out-for-delivery"), "out_for_delivery");
        assert_eq!(map_redx_status("failed-delivery"), "out_for_delivery");
        assert_eq!(map_redx_status("delivered"), "delivered");
        assert_eq!(map_redx_status("partial-delivered"), "partial_delivered");
        assert_eq!(map_redx_status("return-in-transit"), "return_in_transit");
        assert_eq!(map_redx_status("returned"), "returned");
        assert_eq!(map_redx_status("delivered-returned"), "returned");
        assert_eq!(map_redx_status("cancelled"), "cancelled");
        // Normalization covers the spelling variants too.
        assert_eq!(map_redx_status("Out For Delivery"), "out_for_delivery");
        assert_eq!(map_redx_status("DELIVERED"), "delivered");
        assert_eq!(map_redx_status("canceled"), "cancelled");
        // Unrecognised strings are honestly unknown, never guessed.
        assert_eq!(map_redx_status(""), "unknown");
        assert_eq!(map_redx_status("agent is nearby"), "unknown");
        for raw in ["picked-up", "delivered", "returned", "whatever"] {
            assert!(is_valid_shipment_status(map_redx_status(raw)), "{raw}");
        }
    }

    #[test]
    fn ecourier_status_mapping() {
        // The eCourier title-case tracking strings.
        assert_eq!(map_ecourier_status("Processing"), "created");
        assert_eq!(map_ecourier_status("pending"), "created");
        assert_eq!(map_ecourier_status("Picked"), "picked_up");
        assert_eq!(map_ecourier_status("picked-up"), "picked_up");
        assert_eq!(map_ecourier_status("Shipped"), "in_transit");
        assert_eq!(map_ecourier_status("In Transit"), "in_transit");
        assert_eq!(map_ecourier_status("On Hold"), "in_transit");
        assert_eq!(map_ecourier_status("Out for Delivery"), "out_for_delivery");
        assert_eq!(map_ecourier_status("Delivered"), "delivered");
        assert_eq!(map_ecourier_status("Partial Delivered"), "partial_delivered");
        assert_eq!(map_ecourier_status("Partially Delivered"), "partial_delivered");
        assert_eq!(map_ecourier_status("Return in Transit"), "return_in_transit");
        assert_eq!(map_ecourier_status("Returned"), "returned");
        assert_eq!(map_ecourier_status("Cancelled"), "cancelled");
        // Normalization covers the spelling variants too.
        assert_eq!(map_ecourier_status(" DELIVERED "), "delivered");
        assert_eq!(map_ecourier_status("canceled"), "cancelled");
        // Unrecognised strings are honestly unknown, never guessed.
        assert_eq!(map_ecourier_status(""), "unknown");
        assert_eq!(map_ecourier_status("agent is nearby"), "unknown");
        for raw in ["Processing", "Picked", "Delivered", "Returned", "whatever"] {
            assert!(is_valid_shipment_status(map_ecourier_status(raw)), "{raw}");
        }
    }

    #[test]
    fn paperfly_status_mapping() {
        // The Paperfly sentence-style tracking strings.
        assert_eq!(map_paperfly_status("Order Received"), "created");
        assert_eq!(map_paperfly_status("Pickup Requested"), "created");
        assert_eq!(map_paperfly_status("Picked"), "picked_up");
        assert_eq!(map_paperfly_status("In Transit"), "in_transit");
        assert_eq!(map_paperfly_status("On Hold"), "in_transit");
        assert_eq!(map_paperfly_status("Out for Delivery"), "out_for_delivery");
        assert_eq!(map_paperfly_status("Delivered"), "delivered");
        assert_eq!(map_paperfly_status("Partially Delivered"), "partial_delivered");
        assert_eq!(map_paperfly_status("Partial Delivered"), "partial_delivered");
        assert_eq!(map_paperfly_status("Return in Transit"), "return_in_transit");
        assert_eq!(map_paperfly_status("Returned"), "returned");
        assert_eq!(map_paperfly_status("Cancelled"), "cancelled");
        // Normalization covers the spelling variants too.
        assert_eq!(map_paperfly_status(" DELIVERED "), "delivered");
        assert_eq!(map_paperfly_status("order_received"), "created");
        // Unrecognised strings are honestly unknown, never guessed.
        assert_eq!(map_paperfly_status(""), "unknown");
        assert_eq!(map_paperfly_status("rider assigned"), "unknown");
        for raw in ["Order Received", "Picked", "Delivered", "Returned", "whatever"] {
            assert!(is_valid_shipment_status(map_paperfly_status(raw)), "{raw}");
        }
    }

    #[test]
    fn courier_choice_picks_cheapest_available() {
        let quotes = [
            CourierQuote { courier: "steadfast", available: true, price_poisha: 6_000 },
            CourierQuote { courier: "pathao", available: true, price_poisha: 5_500 },
            CourierQuote { courier: "redx", available: true, price_poisha: 7_000 },
        ];
        assert_eq!(choose_courier(&quotes), Some("pathao"));
    }

    #[test]
    fn courier_choice_breaks_ties_by_priority() {
        // Equal prices: platform priority decides, regardless of
        // the order candidates are listed in.
        let quotes = [
            CourierQuote { courier: "redx", available: true, price_poisha: 6_000 },
            CourierQuote { courier: "paperfly", available: true, price_poisha: 6_000 },
            CourierQuote { courier: "pathao", available: true, price_poisha: 6_000 },
            CourierQuote { courier: "steadfast", available: true, price_poisha: 6_000 },
        ];
        assert_eq!(choose_courier(&quotes), Some("steadfast"));
        let quotes = [
            CourierQuote { courier: "paperfly", available: true, price_poisha: 6_000 },
            CourierQuote { courier: "ecourier", available: true, price_poisha: 6_000 },
            CourierQuote { courier: "redx", available: true, price_poisha: 6_000 },
        ];
        assert_eq!(choose_courier(&quotes), Some("redx"));
    }

    #[test]
    fn courier_choice_skips_unavailable_and_handles_none() {
        // The cheapest candidate is unavailable: its price is never
        // trusted, the next cheapest available courier wins.
        let quotes = [
            CourierQuote { courier: "pathao", available: false, price_poisha: 1 },
            CourierQuote { courier: "redx", available: true, price_poisha: 7_000 },
            CourierQuote { courier: "steadfast", available: true, price_poisha: 6_000 },
        ];
        assert_eq!(choose_courier(&quotes), Some("steadfast"));
        // Nobody available, and the empty list, both yield None.
        let quotes = [
            CourierQuote { courier: "steadfast", available: false, price_poisha: 6_000 },
        ];
        assert_eq!(choose_courier(&quotes), None);
        assert_eq!(choose_courier(&[]), None);
    }

    #[test]
    fn local_phone_conversion() {
        // Canonical, 880-prefixed and already-local inputs all land
        // on the same 11-digit local form.
        assert_eq!(to_local_phone("+8801712345678").as_deref(), Some("01712345678"));
        assert_eq!(to_local_phone("8801712345678").as_deref(), Some("01712345678"));
        assert_eq!(to_local_phone("01712345678").as_deref(), Some("01712345678"));
        assert_eq!(to_local_phone(" +880 1912 345678 ").as_deref(), Some("01912345678"));
        // Invalid numbers convert to nothing, never to a guess.
        assert_eq!(to_local_phone("01212345678"), None);
        assert_eq!(to_local_phone("+8801212345678"), None);
        assert_eq!(to_local_phone("12345"), None);
        assert_eq!(to_local_phone(""), None);
    }

    #[test]
    fn available_balance_is_raw_arithmetic() {
        // Earned minus paid-out minus pending.
        assert_eq!(compute_available_balance(184_000, 0, 0), 184_000);
        assert_eq!(compute_available_balance(184_000, 50_000, 100_000), 34_000);
        assert_eq!(compute_available_balance(0, 0, 0), 0);
        // Deliberately NOT clamped: inconsistent inputs surface as a
        // negative result instead of being silently hidden.
        assert_eq!(compute_available_balance(10_000, 20_000, 0), -10_000);
    }

    #[test]
    fn payout_transition_table() {
        for status in ["requested", "approved", "paid", "rejected"] {
            assert!(is_valid_payout_status(status), "{status}");
        }
        assert!(!is_valid_payout_status("pending"));
        assert!(!is_valid_payout_status(""));

        // The legal path: requested -> approved -> paid, and
        // requested -> rejected.
        assert!(can_transition_payout("requested", "approved"));
        assert!(can_transition_payout("requested", "rejected"));
        assert!(can_transition_payout("approved", "paid"));
        // Everything else is illegal: skips, backwards moves,
        // same-state, and anything out of the final states.
        assert!(!can_transition_payout("requested", "paid"));
        assert!(!can_transition_payout("approved", "rejected"));
        assert!(!can_transition_payout("approved", "requested"));
        assert!(!can_transition_payout("paid", "requested"));
        assert!(!can_transition_payout("rejected", "approved"));
        assert!(!can_transition_payout("requested", "requested"));
        assert!(!can_transition_payout("paid", "paid"));
    }

    #[test]
    fn payout_account_validation() {
        // Wallets: the account number is a BD mobile number; both
        // the local and the international form are accepted.
        assert!(validate_payout_account("bkash", "Rahim Uddin", "01712345678").is_ok());
        assert!(validate_payout_account("nagad", "Rahim Uddin", "+8801712345678").is_ok());
        assert!(validate_payout_account("bkash", "Rahim Uddin", "01212345678").is_err());
        assert!(validate_payout_account("bkash", "Rahim Uddin", "12345").is_err());
        assert!(validate_payout_account("nagad", "Rahim Uddin", "").is_err());
        // Bank: holder name required; number 6..=24 alphanumeric.
        assert!(validate_payout_account("bank", "Rahim Uddin", "12345678901").is_ok());
        assert!(validate_payout_account("bank", "Rahim Uddin", "AC123456").is_ok());
        assert!(validate_payout_account("bank", "", "12345678901").is_err());
        assert!(validate_payout_account("bank", "  ", "12345678901").is_err());
        assert!(validate_payout_account("bank", "Rahim Uddin", "12345").is_err());
        assert!(validate_payout_account("bank", "Rahim Uddin", "1234567890123456789012345").is_err());
        assert!(validate_payout_account("bank", "Rahim Uddin", "1234-5678").is_err());
        // Unknown methods are rejected outright.
        assert!(validate_payout_account("rocket", "Rahim Uddin", "01712345678").is_err());
        assert!(validate_payout_account("", "Rahim Uddin", "01712345678").is_err());
    }

    #[test]
    fn payout_amount_validation() {
        // The minimum is 50_000 poisha (500 taka).
        assert_eq!(MIN_PAYOUT_POISHA, 50_000);
        assert!(validate_payout_amount(49_999, 184_000).is_err());
        assert!(validate_payout_amount(0, 184_000).is_err());
        assert!(validate_payout_amount(-1, 184_000).is_err());
        // At the minimum and within the balance: fine.
        assert!(validate_payout_amount(50_000, 184_000).is_ok());
        assert!(validate_payout_amount(184_000, 184_000).is_ok());
        // Above the available balance: rejected, even past the minimum.
        assert!(validate_payout_amount(184_001, 184_000).is_err());
        assert!(validate_payout_amount(50_000, 0).is_err());
    }

    #[test]
    fn payout_amount_validation_with_custom_min() {
        // A lower admin-set minimum admits smaller payouts.
        assert!(validate_payout_amount_with_min(10_000, 184_000, 10_000).is_ok());
        assert!(validate_payout_amount_with_min(9_999, 184_000, 10_000).is_err());
        // A higher minimum blocks what the default would allow.
        assert!(validate_payout_amount_with_min(50_000, 184_000, 100_000).is_err());
        assert!(validate_payout_amount_with_min(100_000, 184_000, 100_000).is_ok());
        // The balance fit is independent of the minimum.
        assert!(validate_payout_amount_with_min(184_001, 184_000, 10_000).is_err());
        // The delegating wrapper always uses the built-in minimum.
        assert_eq!(
            validate_payout_amount(49_999, 184_000),
            validate_payout_amount_with_min(49_999, 184_000, MIN_PAYOUT_POISHA)
        );
    }

    #[test]
    fn promotion_days_validation() {
        // The legal window is 1..=30 days.
        assert_eq!(PROMOTION_MAX_DAYS, 30);
        assert_eq!(validate_promotion_days(1), Ok(1));
        assert_eq!(validate_promotion_days(30), Ok(30));
        assert!(validate_promotion_days(0).is_err());
        assert!(validate_promotion_days(-3).is_err());
        assert!(validate_promotion_days(31).is_err());
    }

    #[test]
    fn promotion_cost_is_days_times_price() {
        // The default price is 10_000 poisha (100 taka) a day.
        assert_eq!(DEFAULT_PROMOTION_PRICE_PER_DAY_POISHA, 10_000);
        assert_eq!(promotion_cost_poisha(3, 10_000), Some(30_000));
        assert_eq!(promotion_cost_poisha(1, 5_000), Some(5_000));
        assert_eq!(promotion_cost_poisha(30, 10_000), Some(300_000));
        // A non-positive price or day count is never a charge.
        assert_eq!(promotion_cost_poisha(3, 0), None);
        assert_eq!(promotion_cost_poisha(3, -10_000), None);
        assert_eq!(promotion_cost_poisha(0, 10_000), None);
        assert_eq!(promotion_cost_poisha(-1, 10_000), None);
        // Overflow refuses the sale instead of wrapping.
        assert_eq!(promotion_cost_poisha(i64::MAX, 10_000), None);
    }

    #[test]
    fn promotion_price_validation() {
        assert!(validate_promotion_price_per_day(1));
        assert!(validate_promotion_price_per_day(10_000));
        assert!(validate_promotion_price_per_day(10_000_000));
        assert!(!validate_promotion_price_per_day(0));
        assert!(!validate_promotion_price_per_day(-1));
        assert!(!validate_promotion_price_per_day(10_000_001));
        // The setting key is the contract with the settings table.
        assert_eq!(PROMOTION_PRICE_PER_DAY_KEY, "promotion_price_per_day_poisha");
    }

    #[test]
    fn promotion_activity_window() {
        // Live inside the half-open window [start, end).
        assert!(promotion_is_active(1_000, 2_000, "active", 1_000));
        assert!(promotion_is_active(1_000, 2_000, "active", 1_999));
        // Not before the start, and over exactly at the end.
        assert!(!promotion_is_active(1_000, 2_000, "active", 999));
        assert!(!promotion_is_active(1_000, 2_000, "active", 2_000));
        assert!(!promotion_is_active(1_000, 2_000, "active", 5_000));
        // A cancelled promotion is never active, even in-window.
        assert!(!promotion_is_active(1_000, 2_000, "cancelled", 1_500));
    }

    #[test]
    fn subscription_plan_keys_and_discounts() {
        assert_eq!(validate_plan_key("free"), Ok("free"));
        assert_eq!(validate_plan_key(" pro "), Ok("pro"));
        assert_eq!(validate_plan_key("business"), Ok("business"));
        assert!(validate_plan_key("enterprise").is_err());
        assert!(validate_plan_key("").is_err());
        // Every plan in the catalogue validates and has a row
        // in each of the plan tables below.
        for plan in SUBSCRIPTION_PLANS {
            assert_eq!(validate_plan_key(plan), Ok(plan));
        }
        assert_eq!(plan_commission_discount_bps("free"), 0);
        assert_eq!(plan_commission_discount_bps("pro"), 100);
        assert_eq!(plan_commission_discount_bps("business"), 200);
        // Unknown keys fail closed: no discount.
        assert_eq!(plan_commission_discount_bps("enterprise"), 0);
        assert_eq!(plan_commission_discount_bps(""), 0);
    }

    #[test]
    fn subscription_plan_caps_and_prices() {
        assert_eq!(plan_product_cap("free"), DEFAULT_FREE_PLAN_PRODUCT_CAP);
        assert_eq!(plan_product_cap("free"), 50);
        assert_eq!(plan_product_cap("pro"), 500);
        // Business is unlimited, via the documented sentinel.
        assert_eq!(plan_product_cap("business"), i64::MAX);
        // Unknown keys get the free cap — failing closed.
        assert_eq!(plan_product_cap("enterprise"), 50);
        assert_eq!(DEFAULT_SUBSCRIPTION_PRICE_PRO_POISHA, 49_900);
        assert_eq!(DEFAULT_SUBSCRIPTION_PRICE_BUSINESS_POISHA, 149_900);
        assert_eq!(SUBSCRIPTION_PERIOD_DAYS, 30);
        // The setting keys are the contract with the settings table.
        assert_eq!(SUBSCRIPTION_PRICE_PRO_KEY, "subscription_price_pro_poisha");
        assert_eq!(
            SUBSCRIPTION_PRICE_BUSINESS_KEY,
            "subscription_price_business_poisha"
        );
        assert_eq!(PLAN_FREE_PRODUCT_CAP_KEY, "plan_free_product_cap");
    }

    #[test]
    fn subscription_setting_validators_cover_the_legal_ranges() {
        assert!(validate_subscription_price(1));
        assert!(validate_subscription_price(49_900));
        assert!(validate_subscription_price(10_000_000));
        assert!(!validate_subscription_price(0));
        assert!(!validate_subscription_price(-1));
        assert!(!validate_subscription_price(10_000_001));

        assert!(validate_plan_free_product_cap(0));
        assert!(validate_plan_free_product_cap(2));
        assert!(validate_plan_free_product_cap(1_000_000));
        assert!(!validate_plan_free_product_cap(-1));
        assert!(!validate_plan_free_product_cap(1_000_001));
    }

    #[test]
    fn order_totals_group_rate_overrides_order_rate() {
        // Two stores with identical goods: one group carries
        // its own (plan-discounted) rate, the other falls back
        // to the order-wide rate. 200_000 poisha of goods at
        // 800 bps pays 16_000 commission; at 700 bps, 14_000.
        let mut discounted = group("store-pro", vec![item("store-pro", "p1", 1, 2_000)]);
        discounted.commission_rate_bps = Some(700);
        let groups = vec![
            group("store-free", vec![item("store-free", "p2", 1, 2_000)]),
            discounted,
        ];
        let totals = build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).expect("totals compute");
        assert_eq!(totals.sub_orders[0].commission_poisha, 16_000);
        assert_eq!(totals.sub_orders[0].seller_payout_poisha, 184_000);
        assert_eq!(totals.sub_orders[1].commission_poisha, 14_000);
        assert_eq!(totals.sub_orders[1].seller_payout_poisha, 186_000);
        // The order-wide figures are the plain sums either way.
        assert_eq!(totals.goods_total_poisha, 400_000);
    }

    #[test]
    fn commission_bps_validation() {
        assert!(validate_commission_bps(0));
        assert!(validate_commission_bps(800));
        assert!(validate_commission_bps(3_000));
        assert!(!validate_commission_bps(3_001));
        assert!(!validate_commission_bps(5_000));
        assert!(!validate_commission_bps(10_000));
        assert!(!validate_commission_bps(-1));
    }

    #[test]
    fn setting_value_parsing() {
        assert_eq!(parse_setting_i64("800"), Some(800));
        assert_eq!(parse_setting_i64("  1000  "), Some(1_000));
        assert_eq!(parse_setting_i64("0"), Some(0));
        assert_eq!(parse_setting_i64("-5"), Some(-5));
        assert_eq!(parse_setting_i64(""), None);
        assert_eq!(parse_setting_i64("   "), None);
        assert_eq!(parse_setting_i64("abc"), None);
        assert_eq!(parse_setting_i64("10.5"), None);
        assert_eq!(parse_setting_i64("1_000"), None);
        assert_eq!(parse_setting_i64("99999999999999999999999999"), None);
        // The setting keys are the contract with the settings table.
        assert_eq!(COMMISSION_BPS_KEY, "commission_bps");
        assert_eq!(MIN_PAYOUT_KEY, "min_payout_poisha");
        assert_eq!(ADVANCE_PERCENT_BPS_KEY, "advance_percent_bps");
        assert_eq!(ADVANCE_THRESHOLD_KEY, "advance_threshold_poisha");
    }

    #[test]
    fn advance_setting_validators_cover_the_legal_ranges() {
        assert!(validate_advance_bps(0));
        assert!(validate_advance_bps(2_000));
        assert!(validate_advance_bps(10_000));
        assert!(!validate_advance_bps(10_001));
        assert!(!validate_advance_bps(-1));

        assert!(validate_advance_threshold(0));
        assert!(validate_advance_threshold(200_000));
        assert!(!validate_advance_threshold(-1));
    }

    #[test]
    fn advance_math_thresholds_ceil_and_caps() {
        // Below the threshold: no advance.
        assert_eq!(compute_advance_poisha(106_000, 2_000, 200_000, false), 0);
        assert_eq!(compute_advance_poisha(199_999, 2_000, 200_000, false), 0);
        // Exactly AT the threshold the advance applies.
        assert_eq!(
            compute_advance_poisha(200_000, 2_000, 200_000, false),
            40_000
        );
        // The Wave 17 worked example: 20% of 206,000.
        assert_eq!(
            compute_advance_poisha(206_000, 2_000, 200_000, false),
            41_200
        );
        // Ceiling: a fractional poisha of advance is collected whole.
        assert_eq!(compute_advance_poisha(101, 2_000, 100, false), 21);
        assert_eq!(compute_advance_poisha(100_001, 3_333, 1, false), 33_331);
        // Threshold 0 disables the threshold rule (not forced).
        assert_eq!(compute_advance_poisha(500_000, 2_000, 0, false), 0);
        // Rate 0 disables advances entirely, even forced.
        assert_eq!(compute_advance_poisha(500_000, 0, 200_000, true), 0);
        // Forced: the threshold is ignored, small orders included.
        assert_eq!(compute_advance_poisha(50_000, 2_000, 200_000, true), 10_000);
        assert_eq!(compute_advance_poisha(50_000, 2_000, 0, true), 10_000);
        // A 100% rate takes the whole total, never more.
        assert_eq!(
            compute_advance_poisha(123_456, 10_000, 1, false),
            123_456
        );
        // Nothing to advance on an empty total.
        assert_eq!(compute_advance_poisha(0, 2_000, 0, true), 0);
    }

    #[test]
    fn buyer_risk_rules_cover_each_branch() {
        // A brand-new buyer: clean, low, no forced advance.
        let risk = assess_buyer_risk(BuyerRiskInputs::default());
        assert_eq!(risk.level, BuyerRiskLevel::Low);
        assert!(!risk.requires_advance);
        assert_eq!(risk.return_rate_bps, 0);

        // A happy buyer with deliveries only stays low.
        let risk = assess_buyer_risk(BuyerRiskInputs {
            total_orders: 5,
            delivered: 5,
            returned_rto: 0,
            buyer_cancelled: 0,
        });
        assert_eq!(risk.level, BuyerRiskLevel::Low);

        // One return: medium, but no forced advance.
        let risk = assess_buyer_risk(BuyerRiskInputs {
            total_orders: 3,
            delivered: 2,
            returned_rto: 1,
            buyer_cancelled: 0,
        });
        assert_eq!(risk.level, BuyerRiskLevel::Medium);
        assert!(!risk.requires_advance);
        assert_eq!(risk.return_rate_bps, 3_333);

        // One buyer cancellation alone is also just medium.
        let risk = assess_buyer_risk(BuyerRiskInputs {
            total_orders: 2,
            delivered: 1,
            returned_rto: 0,
            buyer_cancelled: 1,
        });
        assert_eq!(risk.level, BuyerRiskLevel::Medium);
        assert!(!risk.requires_advance);

        // Two returns but a minority of completions (2 of 6):
        // medium, still no forced advance.
        let risk = assess_buyer_risk(BuyerRiskInputs {
            total_orders: 6,
            delivered: 4,
            returned_rto: 2,
            buyer_cancelled: 0,
        });
        assert_eq!(risk.level, BuyerRiskLevel::Medium);
        assert!(!risk.requires_advance);
        assert_eq!(risk.return_rate_bps, 3_333);

        // Two returns at exactly half of completions: the shield
        // forces an advance and the level is high.
        let risk = assess_buyer_risk(BuyerRiskInputs {
            total_orders: 4,
            delivered: 2,
            returned_rto: 2,
            buyer_cancelled: 0,
        });
        assert!(risk.requires_advance);
        assert_eq!(risk.level, BuyerRiskLevel::High);
        assert_eq!(risk.return_rate_bps, 5_000);

        // All returns, no deliveries: forced advance, high.
        let risk = assess_buyer_risk(BuyerRiskInputs {
            total_orders: 2,
            delivered: 0,
            returned_rto: 2,
            buyer_cancelled: 0,
        });
        assert!(risk.requires_advance);
        assert_eq!(risk.level, BuyerRiskLevel::High);
        assert_eq!(risk.return_rate_bps, 10_000);

        // Three orders, nothing ever delivered, two buyer
        // cancellations: high even though no advance is forced
        // (no returns at all).
        let risk = assess_buyer_risk(BuyerRiskInputs {
            total_orders: 3,
            delivered: 0,
            returned_rto: 0,
            buyer_cancelled: 2,
        });
        assert_eq!(risk.level, BuyerRiskLevel::High);
        assert!(!risk.requires_advance);

        // The same shape with only two orders stays medium — the
        // never-delivered rule needs the 3-order history.
        let risk = assess_buyer_risk(BuyerRiskInputs {
            total_orders: 2,
            delivered: 0,
            returned_rto: 0,
            buyer_cancelled: 2,
        });
        assert_eq!(risk.level, BuyerRiskLevel::Medium);

        // Level strings are the API contract.
        assert_eq!(BuyerRiskLevel::Low.as_str(), "low");
        assert_eq!(BuyerRiskLevel::Medium.as_str(), "medium");
        assert_eq!(BuyerRiskLevel::High.as_str(), "high");
    }

    #[test]
    fn bengali_digits_convert_to_ascii() {
        assert_eq!(bn_to_en_digits("দাম ১,৫০০ টাকা"), "দাম 1,500 টাকা");
        assert_eq!(bn_to_en_digits("০১২৩৪৫৬৭৮৯"), "0123456789");
        assert_eq!(bn_to_en_digits("a১b২"), "a1b2");
        assert_eq!(bn_to_en_digits("no digits here"), "no digits here");
        assert_eq!(bn_to_en_digits(""), "");
    }

    #[test]
    fn price_extraction_labelled_forms() {
        // Bengali digits + টাকা after the number.
        assert_eq!(extract_price_poisha("দামঃ ১,৫০০ টাকা"), Some(150_000));
        // English label + tk after.
        assert_eq!(extract_price_poisha("Price: 1200 tk"), Some(120_000));
        // Taka sign before, no space.
        assert_eq!(extract_price_poisha("৳950"), Some(95_000));
        // Decimal taka.
        assert_eq!(extract_price_poisha("1250.50 টাকা"), Some(125_050));
        // BDT on either side.
        assert_eq!(extract_price_poisha("BDT 2000 only"), Some(200_000));
        assert_eq!(extract_price_poisha("2000 BDT"), Some(200_000));
        // Bengali digits glued to the marker.
        assert_eq!(extract_price_poisha("৯৫০টাকা"), Some(95_000));
        // A line opening with the price label needs no currency word.
        assert_eq!(extract_price_poisha("দাম 500"), Some(50_000));
        assert_eq!(extract_price_poisha("price 750"), Some(75_000));
    }

    #[test]
    fn price_extraction_ignores_bare_numbers() {
        // A phone number is not a price.
        assert_eq!(extract_price_poisha("যোগাযোগ: 01712345678"), None);
        assert_eq!(
            extract_price_poisha("Call 01712345678 to order"),
            None
        );
        // Counts and dates are not prices either.
        assert_eq!(extract_price_poisha("Only 3 left in stock"), None);
        assert_eq!(extract_price_poisha("Offer ends 2026-11-30"), None);
        assert_eq!(extract_price_poisha(""), None);
        // A labelled candidate above the sanity ceiling is skipped.
        assert_eq!(extract_price_poisha("BDT 999999999999"), None);
    }

    #[test]
    fn price_extraction_first_labelled_wins() {
        // The bare 2 is skipped; the labelled 800 is the price.
        assert_eq!(
            extract_price_poisha("2 pcs available, দাম 800 টাকা"),
            Some(80_000)
        );
        // Two labelled prices: the first in the text wins.
        assert_eq!(
            extract_price_poisha("আগে ৳100 ছিল, এখন দাম 800 টাকা"),
            Some(10_000)
        );
        // An unlabelled phone number before the price changes nothing.
        assert_eq!(
            extract_price_poisha("Hotline 01712345678, Price: 450 tk"),
            Some(45_000)
        );
    }

    #[test]
    fn fb_post_draft_shape() {
        let post = "নতুন শাড়ি এসেছে!\nদামঃ ১,৫০০ টাকা\nঅর্ডার করতে ইনবক্স করুন";
        let draft = draft_from_fb_post(post);
        assert_eq!(draft.title, "নতুন শাড়ি এসেছে!");
        assert_eq!(draft.description, post);
        assert_eq!(draft.price_poisha, Some(150_000));

        // Leading blank lines are skipped for the title.
        let draft = draft_from_fb_post("\n\n  Cotton Panjabi  \nPrice: 1200 tk");
        assert_eq!(draft.title, "Cotton Panjabi");
        assert_eq!(draft.price_poisha, Some(120_000));

        // No labelled price -> None, never a guessed number.
        let draft = draft_from_fb_post("Coming soon!\nযোগাযোগ: 01812345678");
        assert_eq!(draft.title, "Coming soon!");
        assert_eq!(draft.price_poisha, None);

        // Empty input drafts empty.
        let draft = draft_from_fb_post("   ");
        assert_eq!(draft.title, "");
        assert_eq!(draft.description, "");
        assert_eq!(draft.price_poisha, None);

        // Over-long titles and descriptions are truncated by chars.
        let long_line = "x".repeat(250);
        let draft = draft_from_fb_post(&long_line);
        assert_eq!(draft.title.chars().count(), MAX_FB_TITLE_CHARS);
        let long_post = "y".repeat(2_500);
        let draft = draft_from_fb_post(&long_post);
        assert_eq!(draft.description.chars().count(), MAX_FB_DESCRIPTION_CHARS);
    }

    #[test]
    fn message_body_validation() {
        assert!(validate_message_body("আসসালামু আলাইকুম").is_ok());
        assert!(validate_message_body("  padded  ").is_ok());
        // Empty and whitespace-only bodies say nothing.
        assert!(validate_message_body("").is_err());
        assert!(validate_message_body("   \n\t ").is_err());
        // The limit counts characters, not bytes (Bangla chars are
        // multi-byte and must not be penalised).
        let at_limit = "ক".repeat(MAX_MESSAGE_BODY_CHARS);
        assert!(validate_message_body(&at_limit).is_ok());
        let over_limit = "x".repeat(MAX_MESSAGE_BODY_CHARS + 1);
        assert!(validate_message_body(&over_limit).is_err());
    }

    #[test]
    fn sidra_signature_payload_shape() {
        // The canonical signed string: conversation, message, text,
        // colon-joined, nothing trimmed or escaped.
        assert_eq!(
            sidra_signature_payload("conv-1", "msg-2", "দাম ৫০০ টাকা"),
            "conv-1:msg-2:দাম ৫০০ টাকা"
        );
        assert_eq!(sidra_signature_payload("", "", ""), "::");
    }

    #[test]
    fn category_validation() {
        // Absent and blank labels are valid (uncategorised).
        assert!(validate_category(None).is_ok());
        assert!(validate_category(Some("")).is_ok());
        assert!(validate_category(Some("   ")).is_ok());
        assert!(validate_category(Some("clothing")).is_ok());
        // The limit counts characters, not bytes: 60 Bangla chars
        // (multi-byte) pass, 61 of anything fail.
        assert!(validate_category(Some(&"ক".repeat(MAX_CATEGORY_CHARS))).is_ok());
        assert!(validate_category(Some(&"x".repeat(MAX_CATEGORY_CHARS + 1))).is_err());
    }

    #[test]
    fn search_params_validation() {
        // Defaults: text present -> relevance; absent -> newest.
        let with_q = validate_search_params(Some("shirt"), None, None, None, None, None)
            .expect("valid");
        assert_eq!(with_q.q.as_deref(), Some("shirt"));
        assert_eq!(with_q.sort, SearchSort::Relevance);
        assert_eq!(with_q.page, 1);
        assert_eq!(with_q.per_page, DEFAULT_SEARCH_PER_PAGE);

        let no_q = validate_search_params(None, None, None, None, None, None).expect("valid");
        assert_eq!(no_q.q, None);
        assert_eq!(no_q.sort, SearchSort::Newest);

        // Blank text is absent text; surrounding space is trimmed.
        let blank = validate_search_params(Some("   "), None, None, None, None, None)
            .expect("valid");
        assert_eq!(blank.q, None);
        assert_eq!(blank.sort, SearchSort::Newest);
        let padded = validate_search_params(Some("  শাড়ি  "), None, None, None, None, None)
            .expect("valid");
        assert_eq!(padded.q.as_deref(), Some("শাড়ি"));

        // Every named sort parses; an unknown one is a 400.
        for (raw, expected) in [
            ("relevance", SearchSort::Relevance),
            ("price_asc", SearchSort::PriceAsc),
            ("price_desc", SearchSort::PriceDesc),
            ("newest", SearchSort::Newest),
            ("rating", SearchSort::Rating),
        ] {
            let parsed =
                validate_search_params(None, None, None, Some(raw), None, None).expect("valid");
            assert_eq!(parsed.sort, expected);
        }
        assert!(validate_search_params(None, None, None, Some("popular"), None, None).is_err());

        // Price bounds: negative is invalid, min above max is
        // invalid, equal bounds are a valid single-price filter.
        assert!(validate_search_params(None, Some(-1), None, None, None, None).is_err());
        assert!(validate_search_params(None, None, Some(-5), None, None, None).is_err());
        assert!(validate_search_params(None, Some(1_000), Some(999), None, None, None).is_err());
        assert!(validate_search_params(None, Some(1_000), Some(1_000), None, None, None).is_ok());

        // Pagination bounds: page is 1-based, per_page 1..=48.
        assert!(validate_search_params(None, None, None, None, Some(0), None).is_err());
        assert!(validate_search_params(None, None, None, None, None, Some(0)).is_err());
        assert!(
            validate_search_params(None, None, None, None, None, Some(MAX_SEARCH_PER_PAGE + 1))
                .is_err()
        );
        assert!(
            validate_search_params(None, None, None, None, Some(3), Some(MAX_SEARCH_PER_PAGE))
                .is_ok()
        );

        // Over-long text is rejected (counted in characters).
        let long_q = "x".repeat(MAX_SEARCH_QUERY_CHARS + 1);
        assert!(validate_search_params(Some(&long_q), None, None, None, None, None).is_err());
        let at_limit = "ক".repeat(MAX_SEARCH_QUERY_CHARS);
        assert!(validate_search_params(Some(&at_limit), None, None, None, None, None).is_ok());
    }

    #[test]
    fn search_like_patterns_escape_metacharacters() {
        assert_eq!(search_like_pattern("shirt"), "%shirt%");
        assert_eq!(search_prefix_pattern("shirt"), "shirt%");
        // %, _ and \ in user text must match themselves literally.
        assert_eq!(search_like_pattern("50% off"), "%50\\% off%");
        assert_eq!(search_like_pattern("a_b\\c"), "%a\\_b\\\\c%");
        assert_eq!(search_prefix_pattern("a_b"), "a\\_b%");
        // Bangla text passes through untouched.
        assert_eq!(search_like_pattern("শাড়ি"), "%শাড়ি%");
    }

    #[test]
    fn return_request_eligibility_and_transitions() {
        // Only a delivered sub-order can be returned by the buyer.
        assert!(can_request_return("delivered"));
        for status in ["placed", "confirmed", "shipped", "returned", "cancelled"] {
            assert!(!can_request_return(status), "status {status}");
        }

        // The decision flow: requested -> approved/rejected,
        // approved -> received; everything else is illegal.
        assert!(can_transition_return("requested", "approved"));
        assert!(can_transition_return("requested", "rejected"));
        assert!(can_transition_return("approved", "received"));
        for (from, to) in [
            ("requested", "received"),
            ("requested", "requested"),
            ("approved", "approved"),
            ("approved", "rejected"),
            ("rejected", "approved"),
            ("rejected", "received"),
            ("received", "approved"),
            ("received", "received"),
        ] {
            assert!(!can_transition_return(from, to), "{from} -> {to}");
        }

        assert!(is_valid_return_status("requested"));
        assert!(is_valid_return_status("received"));
        assert!(!is_valid_return_status("refunded"));
    }

    #[test]
    fn refund_is_goods_minus_discount_never_delivery() {
        // No coupon: the whole goods subtotal comes back. The
        // delivery charge is not an input at all — it can never
        // leak into a refund.
        assert_eq!(compute_refund_poisha(200_000, 0), 200_000);
        // A store coupon's discount was never paid by the buyer,
        // so it is not refunded either.
        assert_eq!(compute_refund_poisha(245_000, 10_000), 235_000);
        // A corrupt snapshot can never produce a negative refund.
        assert_eq!(compute_refund_poisha(100, 500), 0);
    }

    #[test]
    fn return_reason_validation() {
        assert!(validate_return_reason("পণ্যটি ছিঁড়ে গেছে").is_ok());
        assert!(validate_return_reason("  wrong size  ").is_ok());
        assert!(validate_return_reason("").is_err());
        assert!(validate_return_reason("   ").is_err());
        let at_limit = "x".repeat(MAX_RETURN_REASON_CHARS);
        assert!(validate_return_reason(&at_limit).is_ok());
        let over_limit = "x".repeat(MAX_RETURN_REASON_CHARS + 1);
        assert!(validate_return_reason(&over_limit).is_err());
    }

    #[test]
    fn tracking_phone_matching_normalizes_both_sides() {
        // Stored canonical form vs every accepted supplied form.
        assert!(phones_match_for_tracking(
            "+8801712345678",
            "01712345678"
        ));
        assert!(phones_match_for_tracking(
            "+8801712345678",
            "8801712345678"
        ));
        assert!(phones_match_for_tracking(
            "+8801712345678",
            "+8801712345678"
        ));
        assert!(phones_match_for_tracking(
            "01712345678",
            "+880 1712 345678"
        ));
        // A different valid number does not match.
        assert!(!phones_match_for_tracking(
            "+8801712345678",
            "01712345679"
        ));
        // Invalid supplied input is a plain non-match, never an
        // error the endpoint could leak.
        assert!(!phones_match_for_tracking("+8801712345678", "12345"));
        assert!(!phones_match_for_tracking("+8801712345678", ""));
        assert!(!phones_match_for_tracking("not-a-phone", "01712345678"));
    }

    #[test]
    fn theme_keys_are_exact_matches() {
        assert_eq!(THEME_KEYS, ["classic", "minimal", "bold"]);
        assert_eq!(DEFAULT_THEME_KEY, "classic");
        for key in THEME_KEYS {
            assert_eq!(validate_theme_key(key), Ok(key));
        }
        // Case, padding and unknown keys are all rejected: the key
        // selects a layout variant, it is not free text.
        assert!(validate_theme_key("Classic").is_err());
        assert!(validate_theme_key(" classic").is_err());
        assert!(validate_theme_key("fancy").is_err());
        assert!(validate_theme_key("").is_err());
    }

    #[test]
    fn hex_color_normalizes_to_uppercase() {
        assert_eq!(validate_hex_color("#ff5722").unwrap(), "#FF5722");
        assert_eq!(validate_hex_color("#FF5722").unwrap(), "#FF5722");
        assert_eq!(validate_hex_color("#a1B2c3").unwrap(), "#A1B2C3");
        assert_eq!(validate_hex_color("#000000").unwrap(), "#000000");
    }

    #[test]
    fn hex_color_rejects_non_rrggbb_shapes() {
        assert!(validate_hex_color("red").is_err());
        assert!(validate_hex_color("FF5722").is_err()); // missing '#'
        assert!(validate_hex_color("#12345").is_err()); // too short
        assert!(validate_hex_color("#1234567").is_err()); // too long
        assert!(validate_hex_color("#GGGGGG").is_err()); // not hex
        assert!(validate_hex_color("#ff 722").is_err()); // space inside
        assert!(validate_hex_color("").is_err());
    }

    #[test]
    fn branding_url_accepts_absolute_http_urls() {
        assert_eq!(validate_branding_url(None).unwrap(), None);
        assert_eq!(validate_branding_url(Some("")).unwrap(), None);
        assert_eq!(validate_branding_url(Some("   ")).unwrap(), None);
        assert_eq!(
            validate_branding_url(Some("https://example.com/logo.png")).unwrap(),
            Some("https://example.com/logo.png".to_string())
        );
        assert_eq!(
            validate_branding_url(Some("http://cdn.example.com/b.jpg")).unwrap(),
            Some("http://cdn.example.com/b.jpg".to_string())
        );
        // Surrounding whitespace is trimmed before storing.
        assert_eq!(
            validate_branding_url(Some("  https://example.com/a.png  ")).unwrap(),
            Some("https://example.com/a.png".to_string())
        );
        // Exactly at the cap is fine.
        let at_cap = format!("https://e.co/{}", "a".repeat(MAX_BRANDING_URL_CHARS - 13));
        assert_eq!(at_cap.chars().count(), MAX_BRANDING_URL_CHARS);
        assert!(validate_branding_url(Some(&at_cap)).is_ok());
    }

    #[test]
    fn branding_url_rejects_bad_shapes() {
        assert!(validate_branding_url(Some("ftp://example.com/x.png")).is_err());
        assert!(validate_branding_url(Some("example.com/x.png")).is_err());
        assert!(validate_branding_url(Some("/images/x.png")).is_err());
        assert!(validate_branding_url(Some("data:image/png;base64,AAAA")).is_err());
        assert!(validate_branding_url(Some("https://exa mple.com/x.png")).is_err());
        let over_cap = format!("https://e.co/{}", "a".repeat(MAX_BRANDING_URL_CHARS));
        assert!(validate_branding_url(Some(&over_cap)).is_err());
    }

    #[test]
    fn announcement_is_optional_trimmed_and_char_counted() {
        assert_eq!(validate_announcement(None).unwrap(), None);
        assert_eq!(validate_announcement(Some("")).unwrap(), None);
        assert_eq!(validate_announcement(Some("   ")).unwrap(), None);
        assert_eq!(
            validate_announcement(Some("  ঈদের অফার চলছে!  ")).unwrap(),
            Some("ঈদের অফার চলছে!".to_string())
        );
        // Bangla characters count as characters, not bytes: 120
        // copies of a 3-byte character fit, 121 do not.
        assert!(validate_announcement(Some(&"ক".repeat(MAX_ANNOUNCEMENT_CHARS))).is_ok());
        assert!(validate_announcement(Some(&"ক".repeat(MAX_ANNOUNCEMENT_CHARS + 1))).is_err());
        assert!(validate_announcement(Some(&"x".repeat(MAX_ANNOUNCEMENT_CHARS + 1))).is_err());
    }

    #[test]
    fn store_description_shares_the_announcement_contract() {
        assert_eq!(validate_store_description(None).unwrap(), None);
        assert_eq!(validate_store_description(Some("")).unwrap(), None);
        assert_eq!(
            validate_store_description(Some("  সেরা পণ্যের দোকান  ")).unwrap(),
            Some("সেরা পণ্যের দোকান".to_string())
        );
        assert!(
            validate_store_description(Some(&"ক".repeat(MAX_STORE_DESCRIPTION_CHARS))).is_ok()
        );
        assert!(
            validate_store_description(Some(&"ক".repeat(MAX_STORE_DESCRIPTION_CHARS + 1)))
                .is_err()
        );
    }

    #[test]
    fn verification_tiers_are_exact_and_ranked() {
        assert_eq!(VERIFICATION_TIERS, ["unverified", "basic", "verified"]);
        assert_eq!(DEFAULT_VERIFIED_TIER, "unverified");
        for tier in VERIFICATION_TIERS {
            assert_eq!(validate_tier(tier), Ok(tier));
        }
        assert!(validate_tier("Basic").is_err());
        assert!(validate_tier("premium").is_err());
        assert!(validate_tier("").is_err());
        assert_eq!(tier_rank("unverified"), 0);
        assert_eq!(tier_rank("basic"), 1);
        assert_eq!(tier_rank("verified"), 2);
        // An unknown tier claims no trust at all.
        assert_eq!(tier_rank("platinum"), 0);
        assert!(tier_rank("verified") > tier_rank("basic"));
        assert!(tier_rank("basic") > tier_rank("unverified"));
    }

    #[test]
    fn nid_number_accepts_the_three_issued_lengths() {
        // 10, 13 and 17 digits are the three issued NID lengths.
        assert_eq!(
            validate_nid_number("1234567890").unwrap(),
            "1234567890"
        );
        assert_eq!(
            validate_nid_number("1234567890123").unwrap(),
            "1234567890123"
        );
        assert_eq!(
            validate_nid_number("12345678901234567").unwrap(),
            "12345678901234567"
        );
        // Surrounding whitespace is trimmed and the spaces
        // sellers type between digit groups are removed.
        assert_eq!(
            validate_nid_number("  1234 5678 9012 3  ").unwrap(),
            "1234567890123"
        );
    }

    #[test]
    fn nid_number_rejects_other_lengths_and_non_digits() {
        assert!(validate_nid_number("123456789").is_err()); // 9 digits
        assert!(validate_nid_number("12345678901").is_err()); // 11 digits
        assert!(validate_nid_number("123456789012").is_err()); // 12 digits
        assert!(validate_nid_number("12345678901234").is_err()); // 14 digits
        assert!(validate_nid_number("123456789012345678").is_err()); // 18 digits
        assert!(validate_nid_number("").is_err());
        assert!(validate_nid_number("   ").is_err());
        assert!(validate_nid_number("123456789A").is_err()); // letter
        assert!(validate_nid_number("1234-5678-90").is_err()); // hyphens are not spaces
        // Bengali digits are digits to a human, but the stored
        // form is the ASCII digit string — reject, never guess.
        assert!(validate_nid_number("১২৩৪৫৬৭৮৯০").is_err());
    }

    #[test]
    fn mask_nid_keeps_only_the_last_four() {
        assert_eq!(mask_nid("1234567890123"), "*********0123");
        assert_eq!(mask_nid("1234567890"), "******7890");
        assert_eq!(mask_nid("12345678901234567"), "*************4567");
        // Four digits or fewer: nothing safe to show.
        assert_eq!(mask_nid("1234"), "****");
        assert_eq!(mask_nid("123"), "***");
        assert_eq!(mask_nid(""), "");
        assert_eq!(mask_nid("12345"), "*2345");
    }

    #[test]
    fn trade_license_is_optional_and_length_bounded() {
        assert_eq!(validate_trade_license(None).unwrap(), None);
        assert_eq!(validate_trade_license(Some("")).unwrap(), None);
        assert_eq!(validate_trade_license(Some("   ")).unwrap(), None);
        assert_eq!(
            validate_trade_license(Some("  TL-2024-9988  ")).unwrap(),
            Some("TL-2024-9988".to_string())
        );
        // Bounds: 4 and 40 characters pass, 3 and 41 do not.
        assert!(validate_trade_license(Some("ABCD")).is_ok());
        assert!(validate_trade_license(Some("ABC")).is_err());
        assert!(validate_trade_license(Some(&"L".repeat(MAX_TRADE_LICENSE_CHARS))).is_ok());
        assert!(
            validate_trade_license(Some(&"L".repeat(MAX_TRADE_LICENSE_CHARS + 1))).is_err()
        );
    }

    #[test]
    fn verification_transitions_follow_the_application_lifecycle() {
        assert!(is_valid_verification_status("none"));
        assert!(is_valid_verification_status("pending"));
        assert!(is_valid_verification_status("approved"));
        assert!(is_valid_verification_status("rejected"));
        assert!(!is_valid_verification_status("verified"));

        // Applying and resubmitting reach pending; a pending
        // application can be decided either way.
        assert!(can_transition_verification("none", "pending"));
        assert!(can_transition_verification("rejected", "pending"));
        assert!(can_transition_verification("pending", "approved"));
        assert!(can_transition_verification("pending", "rejected"));
        // Approved is terminal, decisions need a pending row, and
        // same-state moves are not transitions (a pending edit
        // replaces the application instead).
        assert!(!can_transition_verification("approved", "pending"));
        assert!(!can_transition_verification("approved", "rejected"));
        assert!(!can_transition_verification("none", "approved"));
        assert!(!can_transition_verification("none", "rejected"));
        assert!(!can_transition_verification("rejected", "approved"));
        assert!(!can_transition_verification("pending", "pending"));
        assert!(!can_transition_verification("pending", "none"));
    }

    #[test]
    fn reviewer_note_is_required_trimmed_and_capped() {
        assert_eq!(validate_reviewer_note("  ঝাপসা ছবি  ").unwrap(), "ঝাপসা ছবি");
        assert!(validate_reviewer_note("").is_err());
        assert!(validate_reviewer_note("   ").is_err());
        assert!(validate_reviewer_note(&"x".repeat(MAX_REVIEWER_NOTE_CHARS)).is_ok());
        assert!(validate_reviewer_note(&"x".repeat(MAX_REVIEWER_NOTE_CHARS + 1)).is_err());
    }

    #[test]
    fn kyc_payout_gate_key_is_stable() {
        assert_eq!(KYC_PAYOUT_GATE_KEY, "kyc_payout_gate");
    }

    #[test]
    fn invoice_number_is_zero_padded_to_six_digits() {
        assert_eq!(format_invoice_number(1), "INV-000001");
        assert_eq!(format_invoice_number(42), "INV-000042");
        assert_eq!(format_invoice_number(999_999), "INV-999999");
        // Past the padded width the number grows; it is never
        // truncated back into a collision.
        assert_eq!(format_invoice_number(1_000_000), "INV-1000000");
    }

    #[test]
    fn invoice_filename_combines_slug_and_padded_seq() {
        assert_eq!(
            invoice_filename("rahim-store", 7),
            "invoice-rahim-store-000007"
        );
        assert_eq!(invoice_filename("a", 123_456), "invoice-a-123456");
    }

    #[test]
    fn export_filename_combines_kind_slug_and_date() {
        assert_eq!(
            export_filename("seller", "rahim-store", "20261126"),
            "bd-marketplace-seller-rahim-store-20261126.json"
        );
        assert_eq!(
            export_filename("buyer", "01712345678", "20260101"),
            "bd-marketplace-buyer-01712345678-20260101.json"
        );
    }

    #[test]
    fn export_filename_sanitizes_to_lowercase_filename_chars() {
        // Uppercase letters fold down; existing hyphens and
        // digits pass through untouched.
        assert_eq!(
            export_filename("seller", "Rahim-Store-24", "20261126"),
            "bd-marketplace-seller-rahim-store-24-20261126.json"
        );
        // The `+` of an international phone becomes a hyphen.
        assert_eq!(
            export_filename("buyer", "+8801712345678", "20261126"),
            "bd-marketplace-buyer--8801712345678-20261126.json"
        );
        // Spaces, underscores, dots and slashes all become
        // hyphens — the result can never escape a directory.
        assert_eq!(
            export_filename("seller", "My Shop_2.0/dhaka", "20261126"),
            "bd-marketplace-seller-my-shop-2-0-dhaka-20261126.json"
        );
        // Non-ASCII characters (e.g. Bangla) become hyphens too
        // — "রহিম" is four chars, so four hyphens sit between
        // the two separator hyphens.
        assert_eq!(
            export_filename("seller", "রহিম", "20261126"),
            "bd-marketplace-seller------20261126.json"
        );
        // An empty slug still yields a well-formed filename.
        assert_eq!(
            export_filename("buyer", "", "20261126"),
            "bd-marketplace-buyer--20261126.json"
        );
    }

    #[test]
    fn stock_status_uses_default_threshold_when_unset() {
        // Out of stock wins over every threshold.
        assert_eq!(stock_status(0, None), "out_of_stock");
        assert_eq!(stock_status(-3, None), "out_of_stock");
        assert_eq!(stock_status(0, Some(0)), "out_of_stock");
        // At or below the default of 5 is low; above is fine.
        assert_eq!(stock_status(1, None), "low");
        assert_eq!(stock_status(LOW_STOCK_DEFAULT, None), "low");
        assert_eq!(stock_status(LOW_STOCK_DEFAULT + 1, None), "in_stock");
        assert_eq!(stock_status(500, None), "in_stock");
    }

    #[test]
    fn stock_status_honours_a_custom_threshold() {
        assert_eq!(stock_status(3, Some(3)), "low");
        assert_eq!(stock_status(4, Some(3)), "in_stock");
        // A zero threshold means only an empty shelf warns.
        assert_eq!(stock_status(1, Some(0)), "in_stock");
        // A high threshold can flag a well-stocked product.
        assert_eq!(stock_status(50, Some(100)), "low");
    }

    #[test]
    fn stock_adjustment_needs_a_real_delta_and_a_known_reason() {
        assert_eq!(validate_stock_adjustment(5, "restock").unwrap(), "restock");
        assert_eq!(
            validate_stock_adjustment(-2, "Damage").unwrap(),
            "damage"
        );
        assert_eq!(
            validate_stock_adjustment(1, "  COUNT_FIX ").unwrap(),
            "count_fix"
        );
        assert_eq!(validate_stock_adjustment(-1, "other").unwrap(), "other");
        assert_eq!(
            validate_stock_adjustment(3, "correction").unwrap(),
            "correction"
        );

        // Zero moves nothing and audits nothing.
        assert!(validate_stock_adjustment(0, "restock").is_err());
        // The cap applies in both directions, boundary included.
        assert!(validate_stock_adjustment(MAX_STOCK_ADJUSTMENT, "restock").is_ok());
        assert!(validate_stock_adjustment(MAX_STOCK_ADJUSTMENT + 1, "restock").is_err());
        assert!(validate_stock_adjustment(-MAX_STOCK_ADJUSTMENT, "restock").is_ok());
        assert!(validate_stock_adjustment(-MAX_STOCK_ADJUSTMENT - 1, "restock").is_err());
        // System reasons are not a seller's to claim.
        assert!(validate_stock_adjustment(5, "order").is_err());
        assert!(validate_stock_adjustment(5, "cancel_restock").is_err());
        assert!(validate_stock_adjustment(5, "").is_err());
        assert!(validate_stock_adjustment(5, "whatever").is_err());
    }

    #[test]
    fn low_stock_threshold_allows_clearing_and_bounds_values() {
        assert_eq!(validate_low_stock_threshold(None).unwrap(), None);
        assert_eq!(validate_low_stock_threshold(Some(0)).unwrap(), Some(0));
        assert_eq!(validate_low_stock_threshold(Some(7)).unwrap(), Some(7));
        assert_eq!(
            validate_low_stock_threshold(Some(MAX_LOW_STOCK_THRESHOLD)).unwrap(),
            Some(MAX_LOW_STOCK_THRESHOLD)
        );
        assert!(validate_low_stock_threshold(Some(-1)).is_err());
        assert!(validate_low_stock_threshold(Some(MAX_LOW_STOCK_THRESHOLD + 1)).is_err());
    }

    #[test]
    fn recommendation_limit_defaults_and_clamps() {
        // Absent means the shared default.
        assert_eq!(recommendation_limit(None), DEFAULT_RECOMMENDATION_LIMIT);
        assert_eq!(recommendation_limit(None), 8);
        // In-range values pass through untouched.
        assert_eq!(recommendation_limit(Some(1)), 1);
        assert_eq!(recommendation_limit(Some(12)), 12);
        assert_eq!(
            recommendation_limit(Some(MAX_RECOMMENDATION_LIMIT)),
            MAX_RECOMMENDATION_LIMIT
        );
        // Zero and negative asks still show one product.
        assert_eq!(recommendation_limit(Some(0)), 1);
        assert_eq!(recommendation_limit(Some(-7)), 1);
        // Oversized asks clamp to the cap, never error.
        assert_eq!(
            recommendation_limit(Some(MAX_RECOMMENDATION_LIMIT + 1)),
            MAX_RECOMMENDATION_LIMIT
        );
        assert_eq!(recommendation_limit(Some(10_000)), MAX_RECOMMENDATION_LIMIT);
    }

    #[test]
    fn analytics_days_defaults_and_clamps() {
        // Absent means the shared default.
        assert_eq!(analytics_days(None), DEFAULT_ANALYTICS_DAYS);
        assert_eq!(analytics_days(None), 30);
        // In-range values pass through untouched.
        assert_eq!(analytics_days(Some(7)), 7);
        assert_eq!(analytics_days(Some(45)), 45);
        assert_eq!(analytics_days(Some(90)), 90);
        // Below the floor clamps up to a week, never errors.
        assert_eq!(analytics_days(Some(3)), MIN_ANALYTICS_DAYS);
        assert_eq!(analytics_days(Some(0)), MIN_ANALYTICS_DAYS);
        assert_eq!(analytics_days(Some(-30)), MIN_ANALYTICS_DAYS);
        // Above the cap clamps down to a quarter.
        assert_eq!(analytics_days(Some(500)), MAX_ANALYTICS_DAYS);
        assert_eq!(analytics_days(Some(10_000)), MAX_ANALYTICS_DAYS);
    }

    #[test]
    fn csv_escape_quotes_only_when_needed() {
        // Plain fields pass through untouched.
        assert_eq!(csv_escape("2026-11-26"), "2026-11-26");
        assert_eq!(csv_escape("184000"), "184000");
        assert_eq!(csv_escape(""), "");
        // A comma forces quoting.
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        // A double quote forces quoting and doubles itself.
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        // Line breaks (LF and CR) force quoting.
        assert_eq!(csv_escape("a\nb"), "\"a\nb\"");
        assert_eq!(csv_escape("a\rb"), "\"a\rb\"");
    }

    #[test]
    fn build_daily_csv_has_header_and_one_line_per_row() {
        let rows = vec![
            ("2026-11-25".to_string(), 0, 0, 0),
            ("2026-11-26".to_string(), 2, 250_000, 230_000),
        ];
        assert_eq!(
            build_daily_csv(&rows),
            "date,orders,goods_poisha,earnings_poisha\n\
             2026-11-25,0,0,0\n\
             2026-11-26,2,250000,230000\n"
        );
        // No rows: just the header line.
        assert_eq!(
            build_daily_csv(&[]),
            "date,orders,goods_poisha,earnings_poisha\n"
        );
    }

    #[test]
    fn otp_request_allowed_up_to_the_hourly_cap() {
        // The cap is the evidence-based 20 (see the constant's
        // doc): below it a request is allowed, at it the next
        // one is refused.
        assert_eq!(OTP_REQUEST_LIMIT_PER_HOUR, 20);
        assert!(otp_request_allowed(0));
        assert!(otp_request_allowed(19));
        assert!(!otp_request_allowed(20));
        assert!(!otp_request_allowed(21));
        assert!(!otp_request_allowed(1_000));
    }

    #[test]
    fn format_taka_whole_and_fractional() {
        assert_eq!(format_taka(0), "৳0");
        assert_eq!(format_taka(206_000), "৳2060");
        assert_eq!(format_taka(100), "৳1");
        assert_eq!(format_taka(1_050), "৳10.50");
        assert_eq!(format_taka(99), "৳0.99");
        assert_eq!(format_taka(-150), "-৳1.50");
    }

    #[test]
    fn notification_bodies_render_their_key_contents() {
        let vars = NotificationVars {
            order_id: "12345678-9abc-def0-1234-56789abcdef0".to_string(),
            store_name: "ঢাকা গ্যাজেট".to_string(),
            amount_poisha: Some(206_000),
            status: String::new(),
        };
        // The short order id (first 8 chars) is what prints.
        let placed = render_body(NOTIFICATION_ORDER_PLACED, &vars);
        assert!(placed.contains("#12345678"), "{placed}");
        assert!(placed.contains("৳2060"), "{placed}");
        assert!(!placed.contains("12345678-9abc"), "{placed}");

        let shipped = render_body(NOTIFICATION_SUB_ORDER_SHIPPED, &vars);
        assert!(shipped.contains("ঢাকা গ্যাজেট"), "{shipped}");
        assert!(shipped.contains("#12345678"), "{shipped}");

        let delivered = render_body(NOTIFICATION_SUB_ORDER_DELIVERED, &vars);
        assert!(delivered.contains("ডেলিভারি সম্পন্ন"), "{delivered}");

        let cancelled = render_body(NOTIFICATION_SUB_ORDER_CANCELLED, &vars);
        assert!(cancelled.contains("বাতিল"), "{cancelled}");

        let returned = render_body(NOTIFICATION_SUB_ORDER_RETURNED, &vars);
        assert!(returned.contains("ফেরত এসেছে"), "{returned}");

        let payout = render_body(NOTIFICATION_PAYOUT_PAID, &vars);
        assert!(payout.contains("৳2060"), "{payout}");
        assert!(payout.contains("পে-আউট"), "{payout}");

        // return_update reads its status var.
        let mut return_vars = vars.clone();
        return_vars.status = "approved".to_string();
        let approved = render_body(NOTIFICATION_RETURN_UPDATE, &return_vars);
        assert!(approved.contains("অনুমোদিত"), "{approved}");
        return_vars.status = "rejected".to_string();
        let rejected = render_body(NOTIFICATION_RETURN_UPDATE, &return_vars);
        assert!(rejected.contains("গৃহীত হয়নি"), "{rejected}");
        return_vars.status = "received".to_string();
        let received = render_body(NOTIFICATION_RETURN_UPDATE, &return_vars);
        assert!(received.contains("রিফান্ড ৳2060"), "{received}");

        // Unknown kinds still render a body.
        let other = render_body("some_future_kind", &vars);
        assert!(other.contains("#12345678"), "{other}");
    }

    // -- Wave 37: API keys ---------------------------------------------

    #[test]
    fn api_key_name_trims_and_bounds() {
        assert_eq!(validate_api_key_name("  Mobile app  ").unwrap(), "Mobile app");
        assert!(validate_api_key_name("").is_err());
        assert!(validate_api_key_name("   ").is_err());
        // The bound counts characters, not bytes: 60 Bangla
        // characters pass, 61 fail.
        assert!(validate_api_key_name(&"ক".repeat(60)).is_ok());
        assert!(validate_api_key_name(&"ক".repeat(61)).is_err());
        assert!(validate_api_key_name(&"a".repeat(60)).is_ok());
        assert!(validate_api_key_name(&"a".repeat(61)).is_err());
    }

    #[test]
    fn api_key_prefix_takes_the_first_twelve_characters() {
        let key = "bdm_live_abcdefghij0123456789ABCDEFGHIJ";
        assert_eq!(api_key_prefix(key), "bdm_live_abc");
        assert_eq!(api_key_prefix(key).chars().count(), API_KEY_PREFIX_CHARS);
        // A short (malformed) key yields itself, never a panic.
        assert_eq!(api_key_prefix("bdm"), "bdm");
        assert_eq!(api_key_prefix(""), "");
    }

    #[test]
    fn api_key_scopes_validate_dedupe_and_order() {
        let read = validate_api_key_scopes(&["read".to_string()]).unwrap();
        assert_eq!(read, vec!["read".to_string()]);
        // Input order and duplicates never reach storage.
        let both = validate_api_key_scopes(&[
            "write".to_string(),
            "read".to_string(),
            "write".to_string(),
        ])
        .unwrap();
        assert_eq!(both, vec!["read".to_string(), "write".to_string()]);
        // Empty and invented scopes are refused outright.
        assert!(validate_api_key_scopes(&[]).is_err());
        assert!(validate_api_key_scopes(&["admin".to_string()]).is_err());
        assert!(validate_api_key_scopes(&["read".to_string(), "all".to_string()]).is_err());
    }

    #[test]
    fn api_key_scope_allows_the_full_matrix() {
        let read_only = vec!["read".to_string()];
        let read_write = vec!["read".to_string(), "write".to_string()];
        let write_only = vec!["write".to_string()];
        let garbled = vec!["everything".to_string()];
        let empty: Vec<String> = Vec::new();

        assert!(api_key_scope_allows(&read_only, false));
        assert!(!api_key_scope_allows(&read_only, true));
        assert!(api_key_scope_allows(&read_write, false));
        assert!(api_key_scope_allows(&read_write, true));
        // Write implies read.
        assert!(api_key_scope_allows(&write_only, false));
        assert!(api_key_scope_allows(&write_only, true));
        // Unknown or empty scope sets fail closed.
        assert!(!api_key_scope_allows(&garbled, false));
        assert!(!api_key_scope_allows(&garbled, true));
        assert!(!api_key_scope_allows(&empty, false));
        assert!(!api_key_scope_allows(&empty, true));
    }

    #[test]
    fn api_key_expiry_days_bounds() {
        assert_eq!(validate_api_key_expiry_days(None).unwrap(), None);
        assert_eq!(validate_api_key_expiry_days(Some(1)).unwrap(), Some(1));
        assert_eq!(validate_api_key_expiry_days(Some(365)).unwrap(), Some(365));
        assert!(validate_api_key_expiry_days(Some(0)).is_err());
        assert!(validate_api_key_expiry_days(Some(-30)).is_err());
        assert!(validate_api_key_expiry_days(Some(366)).is_err());
    }

    #[test]
    fn request_id_validation_rules() {
        // Empty and oversized ids are refused; the boundary
        // itself (64 characters) is accepted.
        assert!(!validate_request_id(""));
        assert!(!validate_request_id(&"a".repeat(65)));
        assert!(validate_request_id(&"a".repeat(64)));
        assert!(validate_request_id("x"));
        // The shapes real clients send: a dashed UUID and a
        // slug-like token.
        assert!(validate_request_id("550e8400-e29b-41d4-a716-446655440000"));
        assert!(validate_request_id("wave43-test-request-1"));
        // Control bytes could forge a second log line or split
        // the response header — all refused.
        assert!(!validate_request_id("bad\nid"));
        assert!(!validate_request_id("bad\tid"));
        assert!(!validate_request_id("bad\u{7f}id"));
        // Non-ASCII is refused: the echo must stay inside the
        // printable-ASCII header alphabet.
        assert!(!validate_request_id("ঢাকা-request"));
        // A space inside the value is fine; padding at the
        // edges is a smuggled field and is refused.
        assert!(validate_request_id("two words"));
        assert!(!validate_request_id(" leading"));
        assert!(!validate_request_id("trailing "));
    }

    #[test]
    fn idempotency_key_validation_rules() {
        // Empty and oversized keys are refused; the boundary
        // itself (128 characters) is accepted.
        assert!(!validate_idempotency_key(""));
        assert!(validate_idempotency_key(&"k".repeat(128)));
        assert!(!validate_idempotency_key(&"k".repeat(129)));
        assert!(validate_idempotency_key("x"));
        // The shape the web form actually sends: a dashed
        // UUID; namespaced client schemes are fine too.
        assert!(validate_idempotency_key(
            "550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(validate_idempotency_key("web-retry-2-550e8400"));
        // NO spaces anywhere — stricter than the request-id
        // rule, because the key is stored and matched
        // byte-for-byte and a space is an invisible difference.
        assert!(!validate_idempotency_key("two words"));
        assert!(!validate_idempotency_key(" leading"));
        assert!(!validate_idempotency_key("trailing "));
        // Control bytes and DEL are refused (header safety).
        assert!(!validate_idempotency_key("bad\nkey"));
        assert!(!validate_idempotency_key("bad\tkey"));
        assert!(!validate_idempotency_key("bad\u{7f}key"));
        // Non-ASCII is refused: the stored key stays inside
        // the visible-ASCII header alphabet.
        assert!(!validate_idempotency_key("ঢাকা-key"));
    }

    // -----------------------------------------------------------------
    // Wave 51 — product media rules
    // -----------------------------------------------------------------

    #[test]
    fn image_kind_content_types_and_extensions() {
        assert_eq!(ImageKind::Jpeg.content_type(), "image/jpeg");
        assert_eq!(ImageKind::Png.content_type(), "image/png");
        assert_eq!(ImageKind::Webp.content_type(), "image/webp");
        assert_eq!(ImageKind::Jpeg.extension(), "jpg");
        assert_eq!(ImageKind::Png.extension(), "png");
        assert_eq!(ImageKind::Webp.extension(), "webp");
        // Declared types parse case-insensitively, tolerate a
        // parameter suffix, and accept the common jpg alias.
        assert_eq!(
            ImageKind::from_content_type("image/jpeg"),
            Some(ImageKind::Jpeg)
        );
        assert_eq!(
            ImageKind::from_content_type("IMAGE/PNG"),
            Some(ImageKind::Png)
        );
        assert_eq!(
            ImageKind::from_content_type("image/webp; charset=binary"),
            Some(ImageKind::Webp)
        );
        assert_eq!(
            ImageKind::from_content_type("image/jpg"),
            Some(ImageKind::Jpeg)
        );
        // Everything else is not an accepted upload format.
        assert_eq!(ImageKind::from_content_type("image/gif"), None);
        assert_eq!(ImageKind::from_content_type("image/svg+xml"), None);
        assert_eq!(ImageKind::from_content_type("application/octet-stream"), None);
        assert_eq!(ImageKind::from_content_type(""), None);
    }

    #[test]
    fn sniff_image_kind_reads_magic_bytes() {
        // JPEG: SOI marker FF D8 FF (the fourth byte varies).
        assert_eq!(
            sniff_image_kind(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]),
            Some(ImageKind::Jpeg)
        );
        // PNG: the fixed 8-byte signature.
        assert_eq!(
            sniff_image_kind(&[137, 80, 78, 71, 13, 10, 26, 10, 0, 0]),
            Some(ImageKind::Png)
        );
        // WebP: RIFF + size + WEBP form type.
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[36, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        webp.extend_from_slice(b"VP8 ");
        assert_eq!(sniff_image_kind(&webp), Some(ImageKind::Webp));
        // A RIFF container that is not WebP (e.g. WAV) is not
        // an image here.
        let mut wave = b"RIFF".to_vec();
        wave.extend_from_slice(&[36, 0, 0, 0]);
        wave.extend_from_slice(b"WAVE");
        assert_eq!(sniff_image_kind(&wave), None);
        // Non-images and too-short inputs sniff as nothing.
        assert_eq!(sniff_image_kind(b"%PDF-1.7"), None);
        assert_eq!(sniff_image_kind(&[0xFF, 0xD8]), None);
        assert_eq!(sniff_image_kind(&[]), None);
        assert_eq!(sniff_image_kind(b"GIF89a"), None);
    }

    #[test]
    fn resolve_image_kind_trusts_bytes_over_declaration() {
        let png = [137, 80, 78, 71, 13, 10, 26, 10];
        // No declaration: the sniff alone decides.
        assert_eq!(resolve_image_kind(None, &png), Ok(ImageKind::Png));
        // A matching declaration is fine.
        assert_eq!(
            resolve_image_kind(Some("image/png"), &png),
            Ok(ImageKind::Png)
        );
        // A mismatched declaration is refused, not relabelled.
        assert!(resolve_image_kind(Some("image/jpeg"), &png).is_err());
        // An unaccepted declared type is refused even when the
        // bytes are a fine image.
        assert!(resolve_image_kind(Some("image/gif"), &png).is_err());
        // Bytes that are no accepted image are refused with or
        // without a declaration.
        assert!(resolve_image_kind(None, b"plain text").is_err());
        assert!(resolve_image_kind(Some("image/png"), b"plain text").is_err());
    }

    #[test]
    fn image_size_and_dimension_bounds() {
        assert!(validate_image_size(1).is_ok());
        assert!(validate_image_size(MAX_IMAGE_BYTES).is_ok());
        assert!(validate_image_size(MAX_IMAGE_BYTES + 1).is_err());
        assert!(validate_image_size(0).is_err());

        assert!(validate_image_dimensions(1, 1).is_ok());
        assert!(validate_image_dimensions(4_000, 3_000).is_ok());
        assert!(validate_image_dimensions(MAX_IMAGE_DIMENSION, 1).is_ok());
        assert!(validate_image_dimensions(0, 100).is_err());
        assert!(validate_image_dimensions(100, 0).is_err());
        assert!(validate_image_dimensions(MAX_IMAGE_DIMENSION + 1, 1).is_err());
    }

    #[test]
    fn product_image_count_cap() {
        assert!(can_add_product_image(0));
        assert!(can_add_product_image(MAX_IMAGES_PER_PRODUCT - 1));
        assert!(!can_add_product_image(MAX_IMAGES_PER_PRODUCT));
        assert!(!can_add_product_image(MAX_IMAGES_PER_PRODUCT + 5));
        // A nonsense negative count never opens the gate.
        assert!(!can_add_product_image(-1));
    }

    #[test]
    fn alt_text_validation_rules() {
        assert_eq!(validate_alt_text(None), Ok(None));
        assert_eq!(validate_alt_text(Some("")), Ok(None));
        assert_eq!(validate_alt_text(Some("   ")), Ok(None));
        // Provided text is trimmed for storage.
        assert_eq!(
            validate_alt_text(Some("  লাল শাড়ি  ")),
            Ok(Some("লাল শাড়ি".to_string()))
        );
        // The cap counts characters, not bytes: 200 Bangla
        // characters pass, 201 fail.
        assert!(validate_alt_text(Some(&"ক".repeat(200))).is_ok());
        assert!(validate_alt_text(Some(&"ক".repeat(201))).is_err());
    }

    #[test]
    fn external_image_url_rules() {
        // Ordinary http/https photo URLs pass, trimmed.
        assert_eq!(
            validate_external_image_url("  https://cdn.example.com/a/b.jpg  "),
            Ok("https://cdn.example.com/a/b.jpg".to_string())
        );
        assert!(validate_external_image_url("http://localhost:9000/x.png").is_ok());
        assert!(
            validate_external_image_url("HTTPS://EXAMPLE.COM/x.webp?w=600").is_ok()
        );
        // Dangerous or unusable schemes are refused.
        assert!(validate_external_image_url("ftp://example.com/x.jpg").is_err());
        assert!(validate_external_image_url("file:///etc/passwd").is_err());
        assert!(validate_external_image_url("data:image/png;base64,AAAA").is_err());
        assert!(validate_external_image_url("javascript:alert(1)").is_err());
        assert!(validate_external_image_url("//example.com/x.jpg").is_err());
        // Missing host, spaces and empties are refused.
        assert!(validate_external_image_url("https://").is_err());
        assert!(validate_external_image_url("https:///path-only").is_err());
        assert!(validate_external_image_url("https://exa mple.com/x.jpg").is_err());
        assert!(validate_external_image_url("").is_err());
        // The length cap is exact.
        let long = format!("https://example.com/{}", "a".repeat(2_048));
        assert!(validate_external_image_url(&long).is_err());
    }

    #[test]
    fn derived_size_planning_never_upscales() {
        // A large original plans all three widths, widest
        // first, heights proportional (2000x1000 -> halves).
        assert_eq!(
            plan_derived_sizes(2_000, 1_000),
            vec![
                DerivedSize { width: 1_200, height: 600 },
                DerivedSize { width: 600, height: 300 },
                DerivedSize { width: 300, height: 150 },
            ]
        );
        // 700x60: 1200 is not derived (never upscale); 600
        // and 300 are, heights rounded (60*600/700 = 51.4).
        assert_eq!(
            plan_derived_sizes(700, 60),
            vec![
                DerivedSize { width: 600, height: 51 },
                DerivedSize { width: 300, height: 26 },
            ]
        );
        // An original exactly AT a width does not derive it
        // (strictly smaller only).
        assert_eq!(
            plan_derived_sizes(600, 400),
            vec![DerivedSize { width: 300, height: 200 }]
        );
        // Small originals plan nothing — served as-is.
        assert!(plan_derived_sizes(300, 200).is_empty());
        assert!(plan_derived_sizes(4, 4).is_empty());
        // Zero dimensions plan nothing.
        assert!(plan_derived_sizes(0, 100).is_empty());
        assert!(plan_derived_sizes(100, 0).is_empty());
        // A very flat image still gets a height of at least 1.
        assert_eq!(
            plan_derived_sizes(2_000, 3),
            vec![
                DerivedSize { width: 1_200, height: 2 },
                DerivedSize { width: 600, height: 1 },
                DerivedSize { width: 300, height: 1 },
            ]
        );
    }

    #[test]
    fn best_derived_width_picks_the_smallest_sufficient() {
        // From a 2000px original: exact targets pick exact
        // sizes; in-between targets round UP to the next
        // planned width, never down.
        assert_eq!(best_derived_width(2_000, CARD_IMAGE_WIDTH), Some(300));
        assert_eq!(best_derived_width(2_000, GALLERY_IMAGE_WIDTH), Some(600));
        assert_eq!(best_derived_width(2_000, ZOOM_IMAGE_WIDTH), Some(1_200));
        assert_eq!(best_derived_width(2_000, 450), Some(600));
        // Beyond the widest plan, the widest plan is the best
        // available (the original is the only larger source).
        assert_eq!(best_derived_width(2_000, 1_600), Some(1_200));
        // From a 700px original the zoom target falls back to
        // the largest planned width.
        assert_eq!(best_derived_width(700, ZOOM_IMAGE_WIDTH), Some(600));
        assert_eq!(best_derived_width(700, CARD_IMAGE_WIDTH), Some(300));
        // No plan -> no derived width (serve the original).
        assert_eq!(best_derived_width(250, CARD_IMAGE_WIDTH), None);
    }

    #[test]
    fn image_quality_warning_rules() {
        // A healthy product photo warns about nothing.
        assert!(image_quality_warnings(Some(1200), Some(900)).is_empty());
        // Unknown dimensions warn about nothing.
        assert!(image_quality_warnings(None, None).is_empty());
        assert!(image_quality_warnings(Some(0), Some(0)).is_empty());
        // Small images are soft in the gallery.
        assert_eq!(image_quality_warnings(Some(200), Some(200)), ["low_resolution"]);
        assert_eq!(image_quality_warnings(Some(500), Some(400)), ["low_resolution"]);
        // A banner strip is an extreme aspect.
        assert_eq!(
            image_quality_warnings(Some(2000), Some(300)),
            ["extreme_aspect"]
        );
        // Both at once, in rule order.
        assert_eq!(
            image_quality_warnings(Some(100), Some(500)),
            ["low_resolution", "extreme_aspect"]
        );
        // Oversized originals cost decode time.
        assert_eq!(image_quality_warnings(Some(5000), Some(3000)), ["very_large"]);
    }

    #[test]
    fn alt_text_suggestion_from_filenames() {
        assert_eq!(
            suggest_alt_text("red-running-shoe_front.JPG").as_deref(),
            Some("Red running shoe front")
        );
        assert_eq!(suggest_alt_text("IMG_2041.png").as_deref(), Some("IMG 2041"));
        assert_eq!(suggest_alt_text("photo.webp").as_deref(), Some("Photo"));
        // Nothing usable in the name -> no suggestion.
        assert_eq!(suggest_alt_text("---___.png"), None);
        assert_eq!(suggest_alt_text(""), None);
        // A suggestion never exceeds the alt cap.
        let long = format!("{}.png", "word ".repeat(60));
        assert_eq!(suggest_alt_text(&long), None);
    }

    #[test]
    fn image_url_extraction_from_post_text() {
        assert_eq!(
            extract_image_url("দাম ১৫০০ টাকা https://fbcdn.example.com/p/shoe.jpg?size=large order now")
                .as_deref(),
            Some("https://fbcdn.example.com/p/shoe.jpg?size=large")
        );
        // Trailing punctuation is not part of the URL.
        assert_eq!(
            extract_image_url("see this: http://x.example.com/a.PNG.").as_deref(),
            Some("http://x.example.com/a.PNG")
        );
        // Non-image links and bare text yield nothing.
        assert_eq!(extract_image_url("visit https://example.com/page.html"), None);
        assert_eq!(extract_image_url("no links here"), None);
        assert_eq!(extract_image_url("ftp://x.example.com/a.jpg"), None);
    }

    #[test]
    fn image_order_normalisation_rules() {
        let item = |key: &str, position: i32, is_primary: bool| ImageOrderItem {
            key: key.to_string(),
            position,
            is_primary,
        };
        // Empty stays empty.
        assert!(normalize_image_order::<String>(&[]).is_empty());
        // A single unflagged item becomes primary at 0 (the
        // first-upload rule).
        assert_eq!(
            normalize_image_order(&[item("a", 7, false)]),
            vec![item("a", 0, true)]
        );
        // Positions are reassigned densely in requested
        // order; ties keep the caller's order; the flagged
        // item keeps primary.
        assert_eq!(
            normalize_image_order(&[
                item("c", 5, false),
                item("a", 1, false),
                item("b", 1, true),
            ]),
            vec![item("a", 0, false), item("b", 1, true), item("c", 2, false)]
        );
        // Multiple flags: the first flagged item in the new
        // order wins, the rest are cleared.
        assert_eq!(
            normalize_image_order(&[item("a", 0, true), item("b", 1, true)]),
            vec![item("a", 0, true), item("b", 1, false)]
        );
        // No flags at all (the delete-the-primary case): the
        // first item by position is promoted.
        assert_eq!(
            normalize_image_order(&[item("b", 3, false), item("a", 1, false)]),
            vec![item("a", 0, true), item("b", 1, false)]
        );
        // The input vector is not mutated.
        let input = vec![item("b", 3, false), item("a", 1, true)];
        let _ = normalize_image_order(&input);
        assert_eq!(input, vec![item("b", 3, false), item("a", 1, true)]);
    }

    #[test]
    fn media_key_traversal_rules() {
        // Ordinary store keys pass.
        assert!(validate_media_key("products/abc/img-600.webp").is_ok());
        assert!(validate_media_key("reviews/r1/photo-orig.png").is_ok());
        assert!(validate_media_key("a").is_ok());
        // Traversal and absolute paths are refused.
        assert!(validate_media_key("../secret").is_err());
        assert!(validate_media_key("products/../../secret").is_err());
        assert!(validate_media_key("/etc/passwd").is_err());
        assert!(validate_media_key("products//x.webp").is_err());
        assert!(validate_media_key("products/./x.webp").is_err());
        assert!(validate_media_key("products/x.webp/").is_err());
        assert!(validate_media_key("").is_err());
        // Backslashes and odd characters are refused.
        assert!(validate_media_key("products\\x.webp").is_err());
        assert!(validate_media_key("products/x y.webp").is_err());
        assert!(validate_media_key("products/x%2e%2e").is_err());
        // The length cap is enforced.
        assert!(validate_media_key(&"a/".repeat(300)).is_err());
    }

    #[test]
    fn original_filename_rules() {
        assert_eq!(validate_original_filename(None).unwrap(), None);
        assert_eq!(validate_original_filename(Some("  ")).unwrap(), None);
        assert_eq!(
            validate_original_filename(Some(" IMG_2041.jpg ")).unwrap(),
            Some("IMG_2041.jpg".to_string())
        );
        assert_eq!(
            validate_original_filename(Some("লাল-জুতা.png")).unwrap(),
            Some("লাল-জুতা.png".to_string())
        );
        // Paths and control characters are refused.
        assert!(validate_original_filename(Some("a/b.jpg")).is_err());
        assert!(validate_original_filename(Some("a\\b.jpg")).is_err());
        assert!(validate_original_filename(Some("a\tb.jpg")).is_err());
        assert!(validate_original_filename(Some(&"x".repeat(121))).is_err());
        assert!(validate_original_filename(Some(&"x".repeat(120))).is_ok());
    }

    #[test]
    fn media_limit_setting_validators() {
        assert!(validate_media_max_images(1));
        assert!(validate_media_max_images(12));
        assert!(validate_media_max_images(50));
        assert!(!validate_media_max_images(0));
        assert!(!validate_media_max_images(51));
        assert!(validate_media_store_quota_mb(10));
        assert!(validate_media_store_quota_mb(500));
        assert!(validate_media_store_quota_mb(102_400));
        assert!(!validate_media_store_quota_mb(9));
        assert!(!validate_media_store_quota_mb(102_401));
        // The explicit-cap arithmetic matches the default rule.
        assert!(within_media_quota(90, 10, 100));
        assert!(!within_media_quota(90, 11, 100));
    }

    #[test]
    fn store_media_quota_rule() {
        // Under and exactly-at the cap are allowed.
        assert!(store_media_within_quota(0, 1));
        assert!(store_media_within_quota(MAX_STORE_MEDIA_BYTES - 10, 10));
        assert!(store_media_within_quota(MAX_STORE_MEDIA_BYTES, 0));
        // One byte over is refused.
        assert!(!store_media_within_quota(MAX_STORE_MEDIA_BYTES - 10, 11));
        assert!(!store_media_within_quota(MAX_STORE_MEDIA_BYTES, 1));
        // A corrupt (huge) usage figure fails closed, no wrap.
        assert!(!store_media_within_quota(i64::MAX, 1));
    }

    #[test]
    fn fetch_url_rules() {
        // Ordinary public URLs pass, trimmed.
        assert_eq!(
            validate_fetch_url("  https://cdn.example.com/a.jpg ").unwrap(),
            "https://cdn.example.com/a.jpg"
        );
        assert!(validate_fetch_url("http://images.shop.com.bd/x.png?w=800").is_ok());
        assert!(validate_fetch_url("https://example.com:8443/x.jpg").is_ok());
        // Scheme and shape rules (inherited from the external rule).
        assert!(validate_fetch_url("ftp://cdn.example.com/a.jpg").is_err());
        assert!(validate_fetch_url("file:///etc/passwd").is_err());
        assert!(validate_fetch_url("https://").is_err());
        // Credentials in the URL are refused.
        assert!(validate_fetch_url("https://user:pass@example.com/a.jpg").is_err());
        assert!(validate_fetch_url("https://user@example.com/a.jpg").is_err());
        // Loopback NAMES are refused before any DNS happens.
        assert!(validate_fetch_url("http://localhost:9000/a.jpg").is_err());
        assert!(validate_fetch_url("http://minio.localhost/a.jpg").is_err());
        assert!(validate_fetch_url("http://printer.local/a.jpg").is_err());
        // An IPv6 literal host is fine at THIS layer — the
        // resolved-IP rule judges addresses.
        assert!(validate_fetch_url("https://[2606:4700:4700::1111]/a.jpg").is_ok());
    }

    #[test]
    fn blocked_fetch_ip_rules() {
        // Public addresses pass.
        assert!(!is_blocked_fetch_ip("8.8.8.8"));
        assert!(!is_blocked_fetch_ip("93.184.216.34"));
        assert!(!is_blocked_fetch_ip("2606:4700:4700::1111"));
        // Loopback, private and this-network are blocked.
        assert!(is_blocked_fetch_ip("127.0.0.1"));
        assert!(is_blocked_fetch_ip("10.0.0.5"));
        assert!(is_blocked_fetch_ip("172.16.4.1"));
        assert!(is_blocked_fetch_ip("192.168.1.1"));
        assert!(is_blocked_fetch_ip("0.0.0.0"));
        assert!(is_blocked_fetch_ip("0.1.2.3"));
        // Link-local — including the cloud metadata address.
        assert!(is_blocked_fetch_ip("169.254.169.254"));
        assert!(is_blocked_fetch_ip("169.254.0.1"));
        // Multicast, broadcast and documentation ranges.
        assert!(is_blocked_fetch_ip("224.0.0.1"));
        assert!(is_blocked_fetch_ip("255.255.255.255"));
        assert!(is_blocked_fetch_ip("192.0.2.1"));
        // IPv6: loopback, unique-local, link-local.
        assert!(is_blocked_fetch_ip("::1"));
        assert!(is_blocked_fetch_ip("fd00::1"));
        assert!(is_blocked_fetch_ip("fe80::1"));
        assert!(is_blocked_fetch_ip("::"));
        // Unparseable input fails closed.
        assert!(is_blocked_fetch_ip("not-an-ip"));
        assert!(is_blocked_fetch_ip(""));
    }

    // -- Wave 52: merchandising rules -------------------------------------

    #[test]
    fn merch_title_rules() {
        assert_eq!(validate_merch_title("  Eid Sale  ").unwrap(), "Eid Sale");
        assert!(validate_merch_title("").is_err());
        assert!(validate_merch_title("   ").is_err());
        assert!(validate_merch_title(&"x".repeat(120)).is_ok());
        assert!(validate_merch_title(&"x".repeat(121)).is_err());
        // Multi-byte characters count as characters, not bytes.
        assert!(validate_merch_title(&"অ".repeat(120)).is_ok());
        assert!(validate_merch_title(&"অ".repeat(121)).is_err());
    }

    #[test]
    fn merch_subtitle_rules() {
        assert_eq!(validate_merch_subtitle(None, 200).unwrap(), None);
        assert_eq!(validate_merch_subtitle(Some("   "), 200).unwrap(), None);
        assert_eq!(
            validate_merch_subtitle(Some("  up to 70% off "), 200).unwrap(),
            Some("up to 70% off".to_string())
        );
        assert!(validate_merch_subtitle(Some(&"x".repeat(201)), 200).is_err());
        assert!(validate_merch_subtitle(Some(&"x".repeat(200)), 200).is_ok());
    }

    #[test]
    fn banner_link_rules() {
        // Site-relative paths pass, trimmed.
        assert_eq!(validate_banner_link(" /collections/eid ").unwrap(), "/collections/eid");
        assert_eq!(validate_banner_link("/").unwrap(), "/");
        // Absolute http(s) passes.
        assert!(validate_banner_link("https://example.com/sale").is_ok());
        assert!(validate_banner_link("http://example.com").is_ok());
        // Protocol-relative and scheme tricks are refused.
        assert!(validate_banner_link("//evil.example.com").is_err());
        assert!(validate_banner_link("javascript:alert(1)").is_err());
        assert!(validate_banner_link("example.com/sale").is_err());
        assert!(validate_banner_link("ftp://example.com").is_err());
        // Spaces, controls and emptiness are refused.
        assert!(validate_banner_link("/a b").is_err());
        assert!(validate_banner_link("").is_err());
        assert!(validate_banner_link("   ").is_err());
        assert!(validate_banner_link(&format!("/{}", "x".repeat(500))).is_err());
    }

    #[test]
    fn banner_visibility_window() {
        // No window: the flag alone decides.
        assert!(banner_is_visible(true, None, None, 1_000));
        assert!(!banner_is_visible(false, None, None, 1_000));
        // Half-open window: visible at the start second, gone
        // at the end second.
        assert!(!banner_is_visible(true, Some(2_000), Some(3_000), 1_999));
        assert!(banner_is_visible(true, Some(2_000), Some(3_000), 2_000));
        assert!(banner_is_visible(true, Some(2_000), Some(3_000), 2_999));
        assert!(!banner_is_visible(true, Some(2_000), Some(3_000), 3_000));
        // One-sided windows.
        assert!(banner_is_visible(true, Some(2_000), None, 9_999));
        assert!(!banner_is_visible(true, None, Some(2_000), 2_000));
        // The flag beats the window.
        assert!(!banner_is_visible(false, Some(0), Some(9_999), 1_000));
    }

    #[test]
    fn optional_window_rules() {
        assert!(validate_optional_window(None, None).is_ok());
        assert!(validate_optional_window(Some(10), None).is_ok());
        assert!(validate_optional_window(None, Some(10)).is_ok());
        assert!(validate_optional_window(Some(10), Some(11)).is_ok());
        assert!(validate_optional_window(Some(10), Some(10)).is_err());
        assert!(validate_optional_window(Some(11), Some(10)).is_err());
    }

    #[test]
    fn collection_slug_rules() {
        assert_eq!(validate_collection_slug("eid-picks").unwrap(), "eid-picks");
        assert_eq!(validate_collection_slug("under-999").unwrap(), "under-999");
        // The store-slug grammar, reused whole: length,
        // lowercase, no underscores, no edge hyphens.
        assert!(validate_collection_slug("ab").is_err());
        assert!(validate_collection_slug("Eid-Picks").is_err());
        assert!(validate_collection_slug("eid_picks").is_err());
        assert!(validate_collection_slug("-eid").is_err());
        assert!(validate_collection_slug("eid-").is_err());
        assert!(validate_collection_slug(&"x".repeat(41)).is_err());
    }

    #[test]
    fn flash_window_and_liveness_rules() {
        assert!(validate_flash_window(1_000, 2_000).is_ok());
        assert!(validate_flash_window(2_000, 2_000).is_err());
        assert!(validate_flash_window(3_000, 2_000).is_err());
        assert!(flash_sale_is_live(true, 1_000, 2_000, 1_000));
        assert!(flash_sale_is_live(true, 1_000, 2_000, 1_999));
        assert!(!flash_sale_is_live(true, 1_000, 2_000, 999));
        assert!(!flash_sale_is_live(true, 1_000, 2_000, 2_000));
        assert!(!flash_sale_is_live(false, 1_000, 2_000, 1_500));
    }

    #[test]
    fn windows_overlap_rules() {
        // Genuine overlaps in both directions and containment.
        assert!(windows_overlap(0, 100, 50, 150));
        assert!(windows_overlap(50, 150, 0, 100));
        assert!(windows_overlap(0, 200, 50, 60));
        // Touching windows do NOT overlap (back-to-back sales
        // are schedulable).
        assert!(!windows_overlap(0, 100, 100, 200));
        assert!(!windows_overlap(100, 200, 0, 100));
        // Disjoint windows.
        assert!(!windows_overlap(0, 50, 100, 200));
    }

    #[test]
    fn flash_price_validation_rules() {
        assert_eq!(validate_flash_sale_price(80_000, 100_000).unwrap(), 80_000);
        assert_eq!(validate_flash_sale_price(1, 100_000).unwrap(), 1);
        assert!(validate_flash_sale_price(0, 100_000).is_err());
        assert!(validate_flash_sale_price(-5, 100_000).is_err());
        // At or above the base is not a sale.
        assert!(validate_flash_sale_price(100_000, 100_000).is_err());
        assert!(validate_flash_sale_price(100_001, 100_000).is_err());
        assert!(validate_flash_stock_cap(None).unwrap().is_none());
        assert_eq!(validate_flash_stock_cap(Some(1)).unwrap(), Some(1));
        assert!(validate_flash_stock_cap(Some(0)).is_err());
        assert!(validate_flash_stock_cap(Some(-2)).is_err());
    }

    #[test]
    fn flash_price_resolution_rules() {
        // Uncapped: the sale price always resolves.
        assert_eq!(resolve_flash_price(80_000, None, 0), Some(80_000));
        assert_eq!(resolve_flash_price(80_000, None, 999_999), Some(80_000));
        // Capped: resolves until the cap is reached, then the
        // price reverts (None = sell at base).
        assert_eq!(resolve_flash_price(80_000, Some(3), 2), Some(80_000));
        assert_eq!(resolve_flash_price(80_000, Some(3), 3), None);
        assert_eq!(resolve_flash_price(80_000, Some(3), 4), None);
        assert_eq!(resolve_flash_price(80_000, Some(1), 1), None);
        // A non-positive candidate never resolves.
        assert_eq!(resolve_flash_price(0, None, 0), None);
        assert_eq!(resolve_flash_price(-1, None, 0), None);
    }

    #[test]
    fn price_drop_percent_rules() {
        assert_eq!(price_drop_percent(100_000, 80_000), 20);
        assert_eq!(price_drop_percent(100_000, 99_999), 0);
        assert_eq!(price_drop_percent(300, 200), 33);
        assert_eq!(price_drop_percent(100_000, 1), 99);
        // Not a discount at all: no badge.
        assert_eq!(price_drop_percent(100_000, 100_000), 0);
        assert_eq!(price_drop_percent(100_000, 120_000), 0);
        assert_eq!(price_drop_percent(0, 0), 0);
        assert_eq!(price_drop_percent(-5, -10), 0);
    }
}
