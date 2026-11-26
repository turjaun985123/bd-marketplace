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
}

fn default_unit_weight_grams() -> i64 {
    DEFAULT_WEIGHT_GRAMS
}

impl CartItem {
    /// Line total in poisha.
    pub fn line_total_poisha(&self) -> i64 {
        self.unit_price_poisha * i64::from(self.quantity)
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
) -> i64 {
    assert!(cod_amount_poisha >= 0, "COD amount cannot be negative");
    let billable_weight = weight_kg.max(1);
    let extra_kg = i64::from(billable_weight - 1);
    base_charge_poisha(zone) + extra_kg * extra_kg_charge_poisha(zone)
}

/// Split a unified cart into one sub-order per seller.
///
/// First-appearance order of sellers is preserved. This is the single
/// function used by checkout, order splitting and manual seller orders.
pub fn split_cart_by_seller(items: &[CartItem]) -> Vec<SellerSubOrder> {
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
        sub_orders[idx].subtotal_poisha += item.line_total_poisha();
        sub_orders[idx].items.push(item.clone());
    }

    sub_orders
}

/// Compute platform commission and seller payout for a goods subtotal.
///
/// `rate_bps` is the commission rate in basis points (800 bps = 8%).
/// Commission is applied to the goods subtotal only — never to delivery
/// charges. Integer division truncates towards zero (subtotals are
/// non-negative in practice), keeping the result exact in poisha.
pub fn compute_commission(subtotal_poisha: i64, rate_bps: u32) -> CommissionQuote {
    let commission_poisha = subtotal_poisha * i64::from(rate_bps) / 10_000;
    CommissionQuote {
        commission_poisha,
        seller_payout_poisha: subtotal_poisha - commission_poisha,
    }
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
}

/// Money breakdown for one seller's sub-order, all in poisha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubOrderTotals {
    pub goods_subtotal_poisha: i64,
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
    pub delivery_total_poisha: i64,
    /// What the buyer pays (COD): goods + delivery.
    pub grand_total_poisha: i64,
}

/// Compute every money figure for a split order from per-seller groups.
///
/// This is a composition of the existing atomic functions — it does NOT
/// re-implement their maths: the delivery charge comes from
/// [`calculate_delivery_charge`] (the COD amount passed is the group's
/// goods subtotal; the Wave 1 charge rule validates the COD amount but
/// does not price on it, so the charge is exact) and commission/payout
/// from [`compute_commission`]
/// applied to the goods subtotal only.
///
/// Each group's parcel weight is the sum of its items' unit weights
/// times quantities, converted with [`weight_kg_from_grams`] and
/// rounded UP to the next whole kilogram (couriers bill per started
/// kilogram), so a 3.5 kg group is billed as 4 kg.
pub fn build_order_totals(
    groups: &[OrderGroupInput],
    zone: DeliveryZone,
    commission_rate_bps: u32,
) -> OrderTotals {
    let mut sub_orders = Vec::with_capacity(groups.len());
    let mut goods_total_poisha = 0i64;
    let mut delivery_total_poisha = 0i64;

    for group in groups {
        let goods_subtotal_poisha: i64 =
            group.items.iter().map(CartItem::line_total_poisha).sum();
        let total_weight_grams: i64 = group
            .items
            .iter()
            .map(|item| item.unit_weight_grams * i64::from(item.quantity))
            .sum();
        let billable_weight_kg = weight_kg_from_grams(total_weight_grams).ceil() as u32;
        let delivery_charge_poisha = calculate_delivery_charge(
            zone,
            billable_weight_kg,
            goods_subtotal_poisha,
        );
        let quote = compute_commission(goods_subtotal_poisha, commission_rate_bps);
        goods_total_poisha += goods_subtotal_poisha;
        delivery_total_poisha += delivery_charge_poisha;
        sub_orders.push(SubOrderTotals {
            goods_subtotal_poisha,
            delivery_charge_poisha,
            commission_poisha: quote.commission_poisha,
            seller_payout_poisha: quote.seller_payout_poisha,
        });
    }

    OrderTotals {
        sub_orders,
        goods_total_poisha,
        delivery_total_poisha,
        grand_total_poisha: goods_total_poisha + delivery_total_poisha,
    }
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
        }
    }

    #[test]
    fn delivery_charge_first_kg_per_zone() {
        assert_eq!(calculate_delivery_charge(DeliveryZone::InsideDhaka, 1, 0), 6_000);
        assert_eq!(calculate_delivery_charge(DeliveryZone::DhakaSuburb, 1, 0), 10_000);
        assert_eq!(calculate_delivery_charge(DeliveryZone::OutsideDhaka, 1, 0), 12_000);
    }

    #[test]
    fn delivery_charge_extra_weight() {
        // Outside Dhaka, 3 kg: 120 + 2 * 40 = 200 BDT.
        assert_eq!(calculate_delivery_charge(DeliveryZone::OutsideDhaka, 3, 0), 20_000);
        // Zero weight is billed as 1 kg.
        assert_eq!(calculate_delivery_charge(DeliveryZone::InsideDhaka, 0, 0), 6_000);
    }

    #[test]
    fn split_cart_groups_by_seller_and_preserves_order() {
        let cart = vec![
            item("store-a", "p1", 1, 1_000),
            item("store-b", "p2", 2, 500),
            item("store-a", "p3", 1, 250),
        ];
        let subs = split_cart_by_seller(&cart);
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].store_id, "store-a");
        assert_eq!(subs[0].subtotal_poisha, 125_000);
        assert_eq!(subs[0].items.len(), 2);
        assert_eq!(subs[1].store_id, "store-b");
        assert_eq!(subs[1].subtotal_poisha, 100_000);
    }

    #[test]
    fn split_empty_cart_gives_no_sub_orders() {
        assert!(split_cart_by_seller(&[]).is_empty());
    }

    #[test]
    fn commission_eight_percent() {
        let q = compute_commission(100_000, 800); // 1000 BDT at 8%
        assert_eq!(q.commission_poisha, 8_000);
        assert_eq!(q.seller_payout_poisha, 92_000);
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
        let subs = split_cart_by_seller(&cart);
        let goods_total: i64 = subs.iter().map(|s| s.subtotal_poisha).sum();
        assert_eq!(goods_total, 445_000);

        // Each seller ships one 1 kg parcel outside Dhaka: 120 BDT each.
        let delivery_total: i64 = subs
            .iter()
            .map(|_| calculate_delivery_charge(DeliveryZone::OutsideDhaka, 1, 0))
            .sum();
        assert_eq!(delivery_total, 24_000);
        assert_eq!(goods_total + delivery_total, 469_000); // 4,690 BDT payable

        let mut commission_total = 0;
        let mut payout_total = 0;
        for s in &subs {
            let q = compute_commission(s.subtotal_poisha, 800);
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
        let totals = build_order_totals(&groups, DeliveryZone::OutsideDhaka, 800);
        assert_eq!(totals.goods_total_poisha, 400_000);
        assert_eq!(totals.delivery_total_poisha, 24_000);
        assert_eq!(totals.grand_total_poisha, 424_000);
        assert_eq!(totals.sub_orders[0].delivery_charge_poisha, 24_000);
        assert_eq!(totals.sub_orders[0].commission_poisha, 32_000);
        assert_eq!(totals.sub_orders[0].seller_payout_poisha, 368_000);

        // The same items inside Dhaka at 4 kg: 60 + 3*20 = 120 BDT.
        let totals = build_order_totals(&groups, DeliveryZone::InsideDhaka, 800);
        assert_eq!(totals.delivery_total_poisha, 12_000);

        // An exact whole number of kilograms is NOT rounded further:
        // 2 x 2,500 g = 5,000 g -> exactly 5 kg, outside Dhaka:
        // 120 + 4*40 = 280 BDT.
        let groups = vec![group(
            "store-a",
            vec![witem("store-a", "p1", 2, 1_000, 2_500)],
        )];
        let totals = build_order_totals(&groups, DeliveryZone::OutsideDhaka, 800);
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
        let totals = build_order_totals(&groups, DeliveryZone::OutsideDhaka, 800);
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
        let totals = build_order_totals(&groups, DeliveryZone::InsideDhaka, 800);
        assert_eq!(totals.goods_total_poisha, 350_000);
        assert_eq!(totals.delivery_total_poisha, 10_000);
        assert_eq!(totals.grand_total_poisha, 360_000);
        assert_eq!(totals.sub_orders[0].commission_poisha, 28_000);
        assert_eq!(totals.sub_orders[0].seller_payout_poisha, 322_000);
    }

    #[test]
    fn order_totals_empty_groups_are_zero() {
        let totals = build_order_totals(&[], DeliveryZone::DhakaSuburb, 800);
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
    }
}
