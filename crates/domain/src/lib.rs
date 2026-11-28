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
    /// Gift-card redemption applied to this group (Wave 60), in
    /// poisha; zero when the order uses no gift card. A gift card
    /// belongs to ONE store, so at most one group in an order
    /// carries a non-zero value. The redemption is SELLER-funded
    /// (see the Wave 60 section): it clamps to the group's
    /// post-coupon goods, it always reduces the commission base,
    /// and it reduces the seller payout — exactly like a store
    /// coupon's discount, whatever coupon the order also used.
    pub gift_card_discount_poisha: i64,
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
    /// Gift-card redemption applied to this sub-order (Wave 60);
    /// non-zero on at most one sub-order of an order.
    pub gift_card_discount_poisha: i64,
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
    /// Total gift-card redemption across all sub-orders
    /// (Wave 60) — one card, one store, so this equals the
    /// single carrying sub-order's redemption.
    pub gift_card_discount_total_poisha: i64,
    pub delivery_total_poisha: i64,
    /// What the buyer pays: goods − discount − gift card + delivery.
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
/// Gift-card redemptions (Wave 60) are applied per group from
/// the group's `gift_card_discount_poisha`, clamped to the
/// group's POST-COUPON goods (goods − coupon discount) — a
/// card can never discount delivery or eat into another
/// discount. The redemption is seller-funded in BOTH coupon
/// cases: the commission base is the coupon-adjusted base
/// above MINUS the redemption, and the payout is goods −
/// coupon discount − redemption − commission. The buyer pays
/// goods − discount − redemption + delivery.
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
    let mut gift_card_discount_total_poisha = 0i64;
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
        // Wave 60: the gift-card redemption clamps to the
        // POST-COUPON goods — the card prices against what
        // the buyer still owes for this group's goods after
        // the coupon, never against delivery or the coupon
        // itself. A negative stored value clamps to zero.
        let gift_card_discount_poisha = group
            .gift_card_discount_poisha
            .clamp(0, goods_subtotal_poisha - discount_poisha);
        // The commission base: the coupon rule first (the
        // group bears its coupon discount only for a store
        // coupon), then the gift-card redemption comes off
        // in BOTH cases — the card is the seller's own
        // money, so the platform never takes commission on
        // value the seller gave away.
        let commission_base_poisha = if group.commission_on_discounted {
            goods_subtotal_poisha - discount_poisha - gift_card_discount_poisha
        } else {
            goods_subtotal_poisha - gift_card_discount_poisha
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
        gift_card_discount_total_poisha = gift_card_discount_total_poisha
            .checked_add(gift_card_discount_poisha)
            .ok_or_else(|| {
                "অর্ডারের মোট হিসাব সীমা ছাড়িয়ে গেছে — order gift card total overflows the money range"
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
            gift_card_discount_poisha,
            delivery_charge_poisha,
            commission_poisha,
            seller_payout_poisha: goods_subtotal_poisha
                - discount_poisha
                - gift_card_discount_poisha
                - commission_poisha,
        });
    }

    let grand_total_poisha = goods_total_poisha
        .checked_sub(discount_total_poisha)
        .and_then(|net| net.checked_sub(gift_card_discount_total_poisha))
        .and_then(|net| net.checked_add(delivery_total_poisha))
        .ok_or_else(|| {
            "অর্ডারের মোট হিসাব সীমা ছাড়িয়ে গেছে — order grand total overflows the money range"
                .to_string()
        })?;

    Ok(OrderTotals {
        sub_orders,
        goods_total_poisha,
        discount_total_poisha,
        gift_card_discount_total_poisha,
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
/// Every problem with a product input, as (field, message)
/// pairs in the canonical check order — the accumulating
/// form of [`validate_product_input`] (Wave 57: the bulk
/// importer reports ALL of a row's field errors at once
/// instead of making the seller fix them one at a time).
pub fn product_input_errors(input: &ProductInput) -> Vec<(&'static str, String)> {
    let mut errors: Vec<(&'static str, String)> = Vec::new();
    if input.title.trim().is_empty() {
        errors.push(("title", "title must not be empty".to_string()));
    }
    if input.price_poisha < 0 {
        errors.push(("price_poisha", "price_poisha must be >= 0".to_string()));
    }
    if input.stock < 0 {
        errors.push(("stock", "stock must be >= 0".to_string()));
    }
    if !is_valid_product_status(&input.status) {
        errors.push((
            "status",
            "status must be one of: draft, active, archived".to_string(),
        ));
    }
    if let Err(message) = validate_weight_grams(input.weight_grams) {
        errors.push(("weight_grams", message));
    }
    errors
}

pub fn validate_product_input(input: &ProductInput) -> Result<(), String> {
    match product_input_errors(input).into_iter().next() {
        None => Ok(()),
        Some((_, message)) => Err(message),
    }
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
/// The SIDRA auto-reply toggle.
pub const STORE_ACTION_MANAGE_STORE_SETTINGS: &str = "manage_store_settings";
/// Reading and submitting the store's KYC verification
/// application (admin verifications remake). Owner-only: the
/// application carries the owner's national ID number, trade
/// license and document image, so unlike day-to-day settings
/// it is never delegated to a manager or staff member.
pub const STORE_ACTION_MANAGE_VERIFICATION: &str = "manage_verification";

/// Every store action, in the order the README's matrix table
/// lists them. Tests iterate this so a new action can never
/// silently miss a matrix row.
pub const STORE_ACTIONS: [&str; 9] = [
    STORE_ACTION_MANAGE_MEMBERS,
    STORE_ACTION_MANAGE_PAYOUTS,
    STORE_ACTION_VIEW_FINANCES,
    STORE_ACTION_MANAGE_CATALOG,
    STORE_ACTION_MANAGE_ORDERS,
    STORE_ACTION_MANAGE_MARKETING,
    STORE_ACTION_ANSWER_QUESTIONS,
    STORE_ACTION_MANAGE_STORE_SETTINGS,
    STORE_ACTION_MANAGE_VERIFICATION,
];

/// The store role permission matrix (Wave 34) — THE one place
/// that decides what a role may do inside a store:
///
/// - `owner` (the store's `owner_user_id`; never a member row)
///   may do everything.
/// - `manager` may do everything EXCEPT `manage_members`,
///   `manage_payouts` and `manage_verification` — a manager
///   runs the shop day to day but cannot add people, move
///   money out, or touch the owner's identity documents.
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
///
/// Admin payouts remake: `approved → rejected` is legal. An
/// approval commits the amount (it counts as paid out in the
/// earnings aggregate); without an exit, an approval whose
/// coverage later disappears (a return is received, a charge
/// lands) could only sit in `approved` forever or be marked
/// paid against money the store no longer has. Rejecting an
/// approved payout releases the committed amount back to the
/// seller's available balance, under the same required-reason
/// rule as rejecting a request.
pub fn can_transition_payout(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("requested", "approved")
            | ("requested", "rejected")
            | ("approved", "paid")
            | ("approved", "rejected")
    )
}

/// Maximum length of an admin's payout rejection reason, in
/// characters.
pub const MAX_PAYOUT_REJECTION_REASON_CHARS: usize = 500;

/// Validate an admin's reason for rejecting a payout. The
/// reason is REQUIRED (a seller told only "no" cannot fix
/// anything, and the amount silently returning to their
/// available balance is not an explanation), so blank is an
/// error — the same rule as the verification reviewer note
/// ([`validate_reviewer_note`]), with payout wording. The
/// value is trimmed and may be at most
/// [`MAX_PAYOUT_REJECTION_REASON_CHARS`] characters, counted
/// in Unicode characters (Bangla reasons are expected).
pub fn validate_payout_rejection_reason(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("a payout rejection reason is required");
    }
    if trimmed.chars().count() > MAX_PAYOUT_REJECTION_REASON_CHARS {
        return Err("payout rejection reason must be at most 500 characters");
    }
    Ok(trimmed.to_string())
}

/// Maximum length of a payout payment reference, in
/// characters; mirrors the `payment_reference` CHECK on the
/// payouts table (migration 0072).
pub const MAX_PAYMENT_REFERENCE_CHARS: usize = 128;

/// Validate the payment reference an admin records when
/// marking a payout paid: the bKash/Nagad transaction id or
/// the bank transfer reference proving the money was sent.
/// Required, 4 to 128 characters after trimming (the floor
/// keeps a stray keystroke from becoming "proof"), printable
/// ASCII only — internal spaces are allowed because bank
/// references carry them, and no gateway-specific shape is
/// imposed because bank references are free-form. The value
/// is stored verbatim after the trim: it must match the
/// gateway/bank record byte for byte when reconciled, so
/// nothing is case-folded or stripped.
pub fn validate_payment_reference(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    let char_count = trimmed.chars().count();
    if char_count < 4 {
        return Err("a payment reference of at least 4 characters is required");
    }
    if char_count > MAX_PAYMENT_REFERENCE_CHARS {
        return Err("payment reference must be at most 128 characters");
    }
    if !trimmed.chars().all(|c| (' '..='~').contains(&c)) {
        return Err("payment reference must contain only printable ASCII characters");
    }
    Ok(trimmed.to_string())
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
    /// Best sellers first (Wave 53): the shared view's
    /// delivered-units `sold_count`, descending. An explicit
    /// buyer-chosen sort, so — like the price and rating
    /// sorts — it stays pure (no sponsored boost).
    Sold,
}

/// The query-string key for a sort order — the inverse of
/// the parse in [`validate_search_params`]. Surfaces that
/// need to WRITE a sort back into a URL or a stored filter
/// (Wave 53 saved searches) go through here so the spelling
/// lives in exactly one place.
pub fn search_sort_key(sort: SearchSort) -> &'static str {
    match sort {
        SearchSort::Relevance => "relevance",
        SearchSort::PriceAsc => "price_asc",
        SearchSort::PriceDesc => "price_desc",
        SearchSort::Newest => "newest",
        SearchSort::Rating => "rating",
        SearchSort::Sold => "sold",
    }
}

/// Validate the plain product LIST's sort parameter (Wave
/// 53). The list endpoint (`GET /v1/products`) has no search
/// text, so it accepts the browse sort vocabulary minus the
/// text rank: `relevance` is accepted and means the list's
/// own default order (newest, sponsored first — the Wave 26
/// rule), exactly as an absent sort does. Everything else
/// maps to the same enum the search endpoint orders by.
pub fn validate_product_list_sort(sort: Option<&str>) -> Result<SearchSort, &'static str> {
    match sort {
        None | Some("newest") | Some("relevance") => Ok(SearchSort::Newest),
        Some("price_asc") => Ok(SearchSort::PriceAsc),
        Some("price_desc") => Ok(SearchSort::PriceDesc),
        Some("rating") => Ok(SearchSort::Rating),
        Some("sold") => Ok(SearchSort::Sold),
        Some(_) => Err(
            "sort must be one of: newest, price_asc, price_desc, rating, sold",
        ),
    }
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
        Some("sold") => SearchSort::Sold,
        Some(_) => {
            return Err(
                "sort must be one of: relevance, price_asc, price_desc, newest, rating, sold",
            )
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

/// The largest price bound a browse filter may name, in
/// poisha (Wave 53). ৳10,00,000 — a million taka — is far
/// above anything the catalogue plausibly holds; the ceiling
/// exists so a fat-fingered or hostile `max_price_poisha`
/// cannot turn the facet aggregates into an unbounded scan
/// plan, and so the rule is stated once, here.
pub const MAX_PRICE_FILTER_POISHA: i64 = 100_000_000;

/// Default page size for the faceted browse (Wave 53) when
/// neither `limit` nor the legacy `per_page` is given.
pub const DEFAULT_BROWSE_LIMIT: i64 = 24;

/// Largest page size the faceted browse serves (Wave 53).
/// Bigger than the Wave 15 search cap (48) because the
/// browse page is a catalogue grid, not a ranked result
/// list — but still one bounded page.
pub const MAX_BROWSE_LIMIT: i64 = 60;

/// Most values one multi-value browse filter (categories or
/// store slugs) may carry in a single request (Wave 53).
/// Twenty is far past any honest faceted navigation; the
/// cap keeps the `= ANY(...)` binds bounded.
pub const MAX_FILTER_VALUES: usize = 20;

/// Longest single category filter value, in characters —
/// the same bound [`validate_category`] puts on a stored
/// label, so a filter can name anything storable.
pub const MAX_CATEGORY_FILTER_CHARS: usize = MAX_CATEGORY_CHARS;

/// Longest single store-slug filter value, in characters.
/// Slugs are minted under the store rules (well under this);
/// the bound only rejects junk.
pub const MAX_STORE_FILTER_CHARS: usize = 80;

/// The rating thresholds the browse facet reports counts
/// for (Wave 53): "4★ & up", "3★ & up", "2★ & up" — the
/// Daraz/AliExpress facet shape. An unrated product counts
/// as 0 stars everywhere in the browse (see
/// [`BrowseFilters::min_rating`]), so it appears in no
/// threshold's count.
pub const RATING_FACET_THRESHOLDS: [f64; 3] = [4.0, 3.0, 2.0];

/// Raw faceted-browse filter input (Wave 53), exactly as
/// the query string carries it: multi-value filters as the
/// repeated parameters arrived, booleans still unparsed
/// text, pagination in both spellings (`limit` is the
/// browse spelling; `per_page` is the Wave 15 legacy one).
/// [`validate_browse_filters`] is the one place this shape
/// is judged.
#[derive(Debug, Clone, Default)]
pub struct BrowseFilterInput {
    /// The search text, unparsed.
    pub q: Option<String>,
    /// Category labels as they arrived — repeated
    /// `category` parameters; each may itself be a
    /// comma-separated list.
    pub categories: Vec<String>,
    /// Store slugs as they arrived — repeated `store`
    /// parameters plus the legacy single `store_slug`,
    /// already merged by the handler.
    pub store_slugs: Vec<String>,
    /// Inclusive lower price bound in poisha (effective
    /// price — see [`BrowseFilters`]).
    pub min_price_poisha: Option<i64>,
    /// Inclusive upper price bound in poisha.
    pub max_price_poisha: Option<i64>,
    /// Minimum rating (0..=5) as it arrived.
    pub min_rating: Option<f64>,
    /// The `in_stock` flag, unparsed (`"true"`/`"false"`).
    pub in_stock: Option<String>,
    /// The `on_sale` flag, unparsed.
    pub on_sale: Option<String>,
    /// The sort key, unparsed.
    pub sort: Option<String>,
    /// 1-based page number.
    pub page: Option<i64>,
    /// Legacy Wave 15 page size spelling.
    pub per_page: Option<i64>,
    /// Browse page size spelling; wins over `per_page`
    /// when both arrive.
    pub limit: Option<i64>,
}

/// Validated faceted-browse filters (Wave 53) — the only
/// shape the search service reads. Every rule that can
/// reject a browse request lives in
/// [`validate_browse_filters`], so the handler has exactly
/// one validation call and the saved-search write path can
/// validate a stored filter with the same rules.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowseFilters {
    /// The trimmed search text, or `None` for a pure
    /// filter browse.
    pub q: Option<String>,
    /// Category labels to match (OR), normalised:
    /// comma-split, trimmed, blanks dropped, duplicates
    /// removed keeping first occurrence. Matching is
    /// case-insensitive exact (the Wave 15 rule).
    pub categories: Vec<String>,
    /// Store slugs to match (OR), normalised like
    /// `categories`.
    pub store_slugs: Vec<String>,
    /// Inclusive lower bound on the EFFECTIVE price in
    /// poisha (the live flash price when one resolves,
    /// else the base price) — a filter or sort that
    /// disagreed with the price the product view shows
    /// would be a bug, so the browse never prices by the
    /// base alone.
    pub min_price_poisha: Option<i64>,
    /// Inclusive upper bound on the effective price.
    pub max_price_poisha: Option<i64>,
    /// Minimum rating, 0..=5, when set. An unrated product
    /// counts as 0, so any positive minimum excludes it —
    /// the same convention the rating facet counts use.
    pub min_rating: Option<f64>,
    /// Keep only products whose effective stock (the
    /// Wave 33 variant-sum rule) is above zero.
    pub in_stock: bool,
    /// Keep only products on sale: in the live flash sale
    /// (price resolved, cap not exhausted) OR carrying a
    /// `compare_at_poisha` above the base price. No other
    /// fact makes a product "on sale" — the badge never
    /// invents a discount.
    pub on_sale: bool,
    /// The validated sort order.
    pub sort: SearchSort,
    /// 1-based page number.
    pub page: i64,
    /// The effective page size: `limit` when given
    /// (1..=[`MAX_BROWSE_LIMIT`]), else the legacy
    /// `per_page` under its Wave 15 bounds, else
    /// [`DEFAULT_BROWSE_LIMIT`].
    pub per_page: i64,
}

/// Parse one boolean filter flag as it arrives in a query
/// string (Wave 53). Absent means false; `"true"`/`"1"`
/// mean true; `"false"`/`"0"` mean false; anything else is
/// a bilingual 400 — silently treating a typo as either
/// value would filter (or not) without the buyer asking.
pub fn parse_filter_bool(raw: Option<&str>, label: &str) -> Result<bool, String> {
    match raw {
        None => Ok(false),
        Some("true") | Some("1") => Ok(true),
        Some("false") | Some("0") => Ok(false),
        Some(_) => Err(format!(
            "{label} must be true or false — {label} true অথবা false হতে হবে"
        )),
    }
}

/// Normalise one multi-value filter list (Wave 53): split
/// each arrived value on commas, trim every piece, drop
/// blanks, drop duplicates (exact match after trimming,
/// first occurrence wins), and refuse more than
/// [`MAX_FILTER_VALUES`] surviving values or any value
/// longer than `max_chars` characters. The label names the
/// filter in the bilingual error.
fn normalize_filter_values(
    values: &[String],
    max_chars: usize,
    label: &str,
) -> Result<Vec<String>, String> {
    let mut normalised: Vec<String> = Vec::new();
    for value in values {
        for piece in value.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            if piece.chars().count() > max_chars {
                return Err(format!(
                    "{label} values must be at most {max_chars} characters — \
                     {label}-এর মান সর্বোচ্চ {max_chars} অক্ষরের হতে হবে"
                ));
            }
            if !normalised.iter().any(|seen| seen == piece) {
                normalised.push(piece.to_string());
            }
        }
    }
    // Canonical order: the lists are only ever consumed
    // as sets (SQL ANY, facet membership), so sorting
    // them here makes the validated form — and with it
    // the analytics fingerprint and the saved-search
    // stored form — identical for every spelling of the
    // same filter set.
    normalised.sort();
    if normalised.len() > MAX_FILTER_VALUES {
        return Err(format!(
            "at most {MAX_FILTER_VALUES} {label} values per search — \
             একটি খোঁজে সর্বোচ্চ {MAX_FILTER_VALUES}টি {label} দেওয়া যাবে"
        ));
    }
    Ok(normalised)
}

/// Validate raw faceted-browse input (Wave 53) — THE one
/// validation function behind `GET /v1/search`, also reused
/// by the saved-search write path so a stored filter obeys
/// exactly the rules a live one does.
///
/// The Wave 15 core (text, price bounds, sort, page) is
/// judged by [`validate_search_params`] and its established
/// error wording is preserved; everything this wave adds —
/// the price ceiling, the `limit` spelling, the rating
/// range, the boolean flags, the multi-value lists — is
/// judged here with bilingual errors.
pub fn validate_browse_filters(input: BrowseFilterInput) -> Result<BrowseFilters, String> {
    // The price ceiling is this wave's addition to the
    // core price rules (the core judges sign and order).
    for bound in [input.min_price_poisha, input.max_price_poisha]
        .into_iter()
        .flatten()
    {
        if bound > MAX_PRICE_FILTER_POISHA {
            return Err(format!(
                "price bounds must be at most {MAX_PRICE_FILTER_POISHA} poisha — \
                 দামের সীমা সর্বোচ্চ {MAX_PRICE_FILTER_POISHA} পয়সা হতে হবে"
            ));
        }
    }
    // When `limit` arrives it owns the page size, so the
    // legacy `per_page` is not handed to the core validator
    // (its 48 cap is the legacy spelling's rule, not the
    // browse spelling's).
    let core_per_page = if input.limit.is_some() {
        None
    } else {
        input.per_page
    };
    let core = validate_search_params(
        input.q.as_deref(),
        input.min_price_poisha,
        input.max_price_poisha,
        input.sort.as_deref(),
        input.page,
        core_per_page,
    )
    .map_err(str::to_string)?;
    let per_page = match input.limit {
        Some(limit) => {
            if !(1..=MAX_BROWSE_LIMIT).contains(&limit) {
                return Err(format!(
                    "limit must be between 1 and {MAX_BROWSE_LIMIT} — \
                     limit ১ থেকে {MAX_BROWSE_LIMIT}-এর মধ্যে হতে হবে"
                ));
            }
            limit
        }
        None => {
            if input.per_page.is_none() {
                // Neither spelling arrived: the browse
                // default, larger than the Wave 15 search
                // default (a grid page, not a result list).
                DEFAULT_BROWSE_LIMIT
            } else {
                core.per_page
            }
        }
    };
    let min_rating = match input.min_rating {
        None => None,
        Some(rating) => {
            if !rating.is_finite() || !(0.0..=5.0).contains(&rating) {
                return Err(
                    "min_rating must be between 0 and 5 — min_rating ০ থেকে ৫-এর মধ্যে হতে হবে"
                        .to_string(),
                );
            }
            Some(rating)
        }
    };
    Ok(BrowseFilters {
        q: core.q,
        categories: normalize_filter_values(
            &input.categories,
            MAX_CATEGORY_FILTER_CHARS,
            "category",
        )?,
        store_slugs: normalize_filter_values(
            &input.store_slugs,
            MAX_STORE_FILTER_CHARS,
            "store",
        )?,
        min_price_poisha: core.min_price_poisha,
        max_price_poisha: core.max_price_poisha,
        min_rating,
        in_stock: parse_filter_bool(input.in_stock.as_deref(), "in_stock")?,
        on_sale: parse_filter_bool(input.on_sale.as_deref(), "on_sale")?,
        sort: core.sort,
        page: core.page,
        per_page,
    })
}

/// A stable fingerprint of a validated browse filter set
/// (Wave 53): the grouping key the search-analytics log
/// stores beside the query text, so the operator can see
/// WHICH filter combinations shoppers run without the log
/// storing anything about WHO ran them. FNV-1a (64-bit)
/// over a canonical rendering — a grouping key, not a
/// security hash; two different filter sets colliding
/// would only blur an analytics count.
pub fn search_filters_fingerprint(filters: &BrowseFilters) -> String {
    let canonical = format!(
        "q={}|cat={}|store={}|min={}|max={}|rating={}|stock={}|sale={}|sort={}",
        filters.q.as_deref().unwrap_or(""),
        filters.categories.join(","),
        filters.store_slugs.join(","),
        filters
            .min_price_poisha
            .map(|value| value.to_string())
            .unwrap_or_default(),
        filters
            .max_price_poisha
            .map(|value| value.to_string())
            .unwrap_or_default(),
        filters
            .min_rating
            .map(|value| value.to_string())
            .unwrap_or_default(),
        filters.in_stock,
        filters.on_sale,
        search_sort_key(filters.sort),
    );
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// How many searches one buyer may save (Wave 53). A saved
/// search is a convenience bookmark, not a data store; the
/// cap keeps the account list a list.
pub const MAX_SAVED_SEARCHES_PER_USER: i64 = 20;

/// Longest saved-search name, in characters (Wave 53).
pub const MAX_SAVED_SEARCH_NAME_CHARS: usize = 60;

/// Validate a saved-search name (Wave 53): trimmed, 1..=60
/// characters. Returns the trimmed name to store.
pub fn validate_saved_search_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(
            "a saved search needs a name — সংরক্ষিত খোঁজের একটি নাম দিন".to_string(),
        );
    }
    if trimmed.chars().count() > MAX_SAVED_SEARCH_NAME_CHARS {
        return Err(format!(
            "a saved search name must be at most {MAX_SAVED_SEARCH_NAME_CHARS} characters — \
             নাম সর্বোচ্চ {MAX_SAVED_SEARCH_NAME_CHARS} অক্ষরের হতে হবে"
        ));
    }
    Ok(trimmed.to_string())
}

/// How many buckets the browse price histogram splits the
/// facet span into (Wave 53).
pub const PRICE_HISTOGRAM_BUCKETS: usize = 8;

/// Plan the price histogram's bucket ranges (Wave 53) for
/// a facet span: `buckets` half-open ranges `[lo, hi)`
/// covering `[min, max]` — the last range's `hi` is
/// `max + 1` so the dearest product lands inside it.
/// Ranges are whole poisha at least 1 wide; a span
/// narrower than the bucket count yields one range per
/// poisha value instead of empty ranges, and a single
/// price (or an inverted span, which the caller never
/// passes) yields one range. Pure: the API counts the
/// exclude-price set into exactly these ranges, and the
/// web renders them verbatim.
pub fn price_histogram_ranges(min_poisha: i64, max_poisha: i64) -> Vec<(i64, i64)> {
    if max_poisha <= min_poisha {
        return vec![(min_poisha, max_poisha.saturating_add(1))];
    }
    let span = max_poisha - min_poisha + 1;
    let buckets = (PRICE_HISTOGRAM_BUCKETS as i64).min(span) as usize;
    let width = span / buckets as i64;
    let mut ranges = Vec::with_capacity(buckets);
    let mut lo = min_poisha;
    for index in 0..buckets {
        let hi = if index + 1 == buckets {
            max_poisha.saturating_add(1)
        } else {
            lo + width
        };
        ranges.push((lo, hi));
        lo = hi;
    }
    ranges
}

/// The default search-analytics window, in days (Wave 53).
pub const DEFAULT_ANALYTICS_WINDOW_DAYS: i64 = 30;

/// The widest search-analytics window the admin read
/// allows, in days (Wave 53): a quarter of search history
/// is an operations view; beyond that the log is an
/// archive question, not a dashboard one.
pub const MAX_ANALYTICS_WINDOW_DAYS: i64 = 90;

/// Validate the admin search-analytics window (Wave 53):
/// absent means the default; a present value must be
/// 1..=MAX_ANALYTICS_WINDOW_DAYS.
pub fn validate_analytics_window_days(days: Option<i64>) -> Result<i64, String> {
    match days {
        None => Ok(DEFAULT_ANALYTICS_WINDOW_DAYS),
        Some(days) if (1..=MAX_ANALYTICS_WINDOW_DAYS).contains(&days) => Ok(days),
        Some(_) => Err(format!(
            "the analytics window must be 1..={MAX_ANALYTICS_WINDOW_DAYS} days — \
             সময়সীমা ১ থেকে {MAX_ANALYTICS_WINDOW_DAYS} দিনের মধ্যে হতে হবে"
        )),
    }
}

// ---------------------------------------------------------------------------
// Store follows (Wave 54)
// ---------------------------------------------------------------------------

/// How many stores one user may follow at once (Wave 54).
/// A follow is a subscription to a store's new products —
/// the cap keeps the /following page (and the notice
/// fan-out) a human-scale list rather than a scrape of
/// the whole marketplace, and it is high enough that no
/// genuine shopper reaches it by accident.
pub const MAX_FOLLOWED_STORES: i64 = 200;

/// May this user follow this store (Wave 54)? The one
/// refusal: a store's owner following their own store —
/// the owner already sees their catalogue in the seller
/// dashboard, and a self-follow would inflate the public
/// follower count the storefront advertises. Everyone
/// else (buyers, other sellers, the store's own staff)
/// may follow.
pub fn can_follow_store(follower_is_owner: bool) -> bool {
    !follower_is_owner
}

/// The bilingual refusal for a self-follow (Wave 54),
/// shared by the follow endpoint so its wording lives
/// with the rule.
pub const SELF_FOLLOW_ERROR: &str =
    "you cannot follow your own store — নিজের স্টোর ফলো করা যায় না";

/// Validate a follow attempt against the caller's current
/// follow count (Wave 54): at [`MAX_FOLLOWED_STORES`] the
/// next follow is refused. Re-following a store already
/// followed is NOT routed through here — the write is
/// idempotent and never counts twice (the API checks for
/// the existing row first).
pub fn validate_follow_count(current_follows: i64) -> Result<(), String> {
    if current_follows < 0 {
        return Err("follow count cannot be negative".to_string());
    }
    if current_follows >= MAX_FOLLOWED_STORES {
        return Err(format!(
            "you already follow {MAX_FOLLOWED_STORES} stores, the most allowed — \
             unfollow one first — আপনি ইতিমধ্যে সর্বোচ্চ {MAX_FOLLOWED_STORES}টি \
             স্টোর ফলো করেছেন, নতুন স্টোর ফলো করতে আগে একটি আনফলো করুন"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Store profile texts (Wave 54)
// ---------------------------------------------------------------------------

/// Longest store "about" text, in characters (Wave 54).
pub const MAX_STORE_ABOUT_CHARS: usize = 2_000;

/// Longest store return-policy text, in characters
/// (Wave 54).
pub const MAX_STORE_RETURN_POLICY_CHARS: usize = 2_000;

/// Validate a store's "about" text (Wave 54): the
/// [`validate_store_description`] shape with the larger
/// [`MAX_STORE_ABOUT_CHARS`] limit — `None` and blank
/// become `None` (the storefront hides the section), a
/// present value is trimmed and returned owned.
pub fn validate_store_about(text: Option<&str>) -> Result<Option<String>, &'static str> {
    validate_optional_text(text, MAX_STORE_ABOUT_CHARS, "about")
}

/// Validate a store's return-policy text (Wave 54): the
/// same optional-text shape as [`validate_store_about`],
/// with its own label so the error names the right field.
pub fn validate_store_return_policy(
    text: Option<&str>,
) -> Result<Option<String>, &'static str> {
    validate_optional_text(text, MAX_STORE_RETURN_POLICY_CHARS, "return_policy")
}

// ---------------------------------------------------------------------------
// Delivery-time estimates (Wave 54)
// ---------------------------------------------------------------------------

/// The platform-settings keys holding each zone's
/// delivery-time estimate (Wave 54): the stored value is
/// the `"<min>-<max>"` day range written by
/// [`format_delivery_day_range`] and read back by
/// [`parse_delivery_day_range`]. An absent or garbled row
/// falls back to the zone's built-in default — a bad
/// settings row must never take the product page down.
pub const DELIVERY_DAYS_INSIDE_DHAKA_KEY: &str = "delivery_days_inside_dhaka";
pub const DELIVERY_DAYS_DHAKA_SUBURB_KEY: &str = "delivery_days_dhaka_suburb";
pub const DELIVERY_DAYS_OUTSIDE_DHAKA_KEY: &str = "delivery_days_outside_dhaka";

/// The settings key for one zone's estimate (Wave 54) —
/// the one place the zone → key mapping lives.
pub fn delivery_days_setting_key(zone: DeliveryZone) -> &'static str {
    match zone {
        DeliveryZone::InsideDhaka => DELIVERY_DAYS_INSIDE_DHAKA_KEY,
        DeliveryZone::DhakaSuburb => DELIVERY_DAYS_DHAKA_SUBURB_KEY,
        DeliveryZone::OutsideDhaka => DELIVERY_DAYS_OUTSIDE_DHAKA_KEY,
    }
}

/// The built-in delivery-time estimate per zone, in days
/// (Wave 54): (earliest, latest) a buyer should expect
/// once the seller hands the parcel to a courier. These
/// are the platform's published promises, deliberately
/// conservative for the outside-Dhaka districts; an admin
/// may narrow or widen them per zone in the settings.
pub fn default_delivery_day_range(zone: DeliveryZone) -> (i64, i64) {
    match zone {
        DeliveryZone::InsideDhaka => (1, 2),
        DeliveryZone::DhakaSuburb => (2, 3),
        DeliveryZone::OutsideDhaka => (3, 5),
    }
}

/// The widest single bound a delivery estimate may carry,
/// in days (Wave 54): a month covers every honest promise
/// a Bangladeshi courier makes; beyond it the number is a
/// data-entry error, not an estimate.
pub const MAX_DELIVERY_ESTIMATE_DAYS: i64 = 30;

/// Validate one delivery day range (Wave 54): both bounds
/// within 0..=[`MAX_DELIVERY_ESTIMATE_DAYS`] and the
/// earliest no later than the latest. Zero is allowed —
/// same-day delivery inside Dhaka is a real promise a
/// platform may choose to make.
pub fn validate_delivery_day_range(min_days: i64, max_days: i64) -> Result<(i64, i64), String> {
    if !(0..=MAX_DELIVERY_ESTIMATE_DAYS).contains(&min_days)
        || !(0..=MAX_DELIVERY_ESTIMATE_DAYS).contains(&max_days)
    {
        return Err(format!(
            "delivery estimate days must be between 0 and {MAX_DELIVERY_ESTIMATE_DAYS} — \
             ডেলিভারির সময় ০ থেকে {MAX_DELIVERY_ESTIMATE_DAYS} দিনের মধ্যে হতে হবে"
        ));
    }
    if min_days > max_days {
        return Err(
            "the earliest delivery day cannot be after the latest — \
             সবচেয়ে কম দিন সবচেয়ে বেশি দিনের চেয়ে বড় হতে পারে না"
                .to_string(),
        );
    }
    Ok((min_days, max_days))
}

/// The stored form of a day range (Wave 54): `"<min>-<max>"`,
/// the exact string the platform-settings row carries.
pub fn format_delivery_day_range(min_days: i64, max_days: i64) -> String {
    format!("{min_days}-{max_days}")
}

/// Parse a stored day range (Wave 54): the inverse of
/// [`format_delivery_day_range`], validated by
/// [`validate_delivery_day_range`]. Anything else — a
/// missing dash, a non-number, an inverted or absurd
/// range — is `None`, and the caller falls back to the
/// zone default.
pub fn parse_delivery_day_range(raw: &str) -> Option<(i64, i64)> {
    let (min_raw, max_raw) = raw.trim().split_once('-')?;
    let min_days: i64 = min_raw.trim().parse().ok()?;
    let max_days: i64 = max_raw.trim().parse().ok()?;
    validate_delivery_day_range(min_days, max_days).ok()
}

/// Resolve one zone's effective day range (Wave 54) from
/// its raw stored setting: a parseable, valid stored
/// range wins; anything else is the zone default. THE
/// resolution rule — the estimate endpoint and the admin
/// settings view both read through here, so the number a
/// buyer is promised is the number the admin sees.
pub fn resolve_delivery_day_range(zone: DeliveryZone, stored: Option<&str>) -> (i64, i64) {
    stored
        .and_then(parse_delivery_day_range)
        .unwrap_or_else(|| default_delivery_day_range(zone))
}

// ---------------------------------------------------------------------------
// Review sorting + rating distribution (Wave 54)
// ---------------------------------------------------------------------------

/// The review-list sort orders (Wave 54). `Newest` is the
/// pre-Wave-54 behaviour and the default; `Photos` puts
/// reviews carrying a photo first (the Wave 51 photos are
/// the most informative reviews a product page has), then
/// newest — it is a sort, never a filter: photo-less
/// reviews still appear, after the photographed ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewSort {
    Newest,
    Highest,
    Lowest,
    Photos,
    /// Most helpful first (Wave 54 extension): by the
    /// live helpful-vote count, then newest — the sort a
    /// buyer wants when a product has hundreds of reviews.
    Helpful,
}

/// Validate a review-list `sort` parameter (Wave 54):
/// absent means [`ReviewSort::Newest`]; an unknown value
/// is an error naming the allowed set, in the style of
/// [`validate_product_list_sort`].
pub fn validate_review_sort(sort: Option<&str>) -> Result<ReviewSort, &'static str> {
    match sort {
        None | Some("newest") => Ok(ReviewSort::Newest),
        Some("highest") => Ok(ReviewSort::Highest),
        Some("lowest") => Ok(ReviewSort::Lowest),
        Some("photos") => Ok(ReviewSort::Photos),
        Some("helpful") => Ok(ReviewSort::Helpful),
        Some(_) => Err("sort must be one of: newest, highest, lowest, photos, helpful"),
    }
}

/// The admin payout queue's sort orders (admin payouts
/// remake). `Newest` is the pre-remake behaviour and the
/// default; `Oldest` is the working order for a processing
/// queue (first in, first decided); the amount orders serve
/// triage and reconciliation. The API layer maps each
/// variant to a fixed ORDER BY fragment — user input never
/// reaches the SQL text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayoutSort {
    Newest,
    Oldest,
    AmountDesc,
    AmountAsc,
}

/// Validate an admin payout-list `sort` parameter (admin
/// payouts remake): absent means [`PayoutSort::Newest`]; an
/// unknown value is an error naming the allowed set, in the
/// style of [`validate_review_sort`].
pub fn validate_payout_list_sort(sort: Option<&str>) -> Result<PayoutSort, &'static str> {
    match sort {
        None | Some("newest") => Ok(PayoutSort::Newest),
        Some("oldest") => Ok(PayoutSort::Oldest),
        Some("amount_desc") => Ok(PayoutSort::AmountDesc),
        Some("amount_asc") => Ok(PayoutSort::AmountAsc),
        Some(_) => Err("sort must be one of: newest, oldest, amount_desc, amount_asc"),
    }
}

/// The admin store queue's sort orders (admin stores
/// remake). `Newest` is the pre-remake behaviour and the
/// default; `Oldest` surfaces the longest-standing stores;
/// the GMV and follower orders serve triage (the biggest
/// businesses first — a suspension there has the widest
/// blast radius). The API layer maps each variant to a
/// fixed ORDER BY fragment — user input never reaches the
/// SQL text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreSort {
    Newest,
    Oldest,
    NameAsc,
    GmvDesc,
    FollowersDesc,
}

/// Validate an admin store-list `sort` parameter (admin
/// stores remake): absent means [`StoreSort::Newest`]; an
/// unknown value is an error naming the allowed set, in
/// the style of [`validate_payout_list_sort`].
pub fn validate_store_list_sort(sort: Option<&str>) -> Result<StoreSort, &'static str> {
    match sort {
        None | Some("newest") => Ok(StoreSort::Newest),
        Some("oldest") => Ok(StoreSort::Oldest),
        Some("name_asc") => Ok(StoreSort::NameAsc),
        Some("gmv_desc") => Ok(StoreSort::GmvDesc),
        Some("followers_desc") => Ok(StoreSort::FollowersDesc),
        Some(_) => Err(
            "sort must be one of: newest, oldest, name_asc, gmv_desc, followers_desc",
        ),
    }
}

/// May this user cast a helpful vote on a review (Wave 54
/// extension)? Everyone except the review's AUTHOR: a
/// vote is other buyers certifying the review helped
/// them, and self-certification would make the count a
/// vanity metric. (Sellers may vote on reviews of other
/// stores' products — they are buyers there too.)
pub fn can_vote_review_helpful(voter_is_author: bool) -> bool {
    !voter_is_author
}

/// The bilingual refusal for voting on your own review
/// (Wave 54 extension), returned by the vote endpoint.
pub const OWN_REVIEW_VOTE_ERROR: &str =
    "নিজের রিভিউতে ভোট দেওয়া যায় না — you cannot vote on your own review";

/// How a product's question list is ordered (Wave 54
/// extension): newest first (the Wave 25 default), or
/// answered first — a buyer scanning for information
/// wants the questions the seller actually answered at
/// the top, unanswered ones after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionSort {
    Newest,
    AnsweredFirst,
}

/// Validate a question-list `sort` parameter (Wave 54
/// extension): absent means [`QuestionSort::Newest`]; an
/// unknown value is an error naming the allowed set.
pub fn validate_question_sort(sort: Option<&str>) -> Result<QuestionSort, &'static str> {
    match sort {
        None | Some("newest") => Ok(QuestionSort::Newest),
        Some("answered") => Ok(QuestionSort::AnsweredFirst),
        Some(_) => Err("sort must be one of: newest, answered"),
    }
}

/// One star level's share of a rating distribution, as a
/// whole percent (Wave 54): the floor of count / total,
/// and 0 when there are no reviews at all — the summary's
/// bars never divide by zero and never round a level UP
/// into a share it does not have.
pub fn rating_share_percent(count: i64, total: i64) -> i64 {
    if count <= 0 || total <= 0 {
        return 0;
    }
    (count.min(total) * 100) / total
}

// ---------------------------------------------------------------------------
// Store collections (Wave 54 extension)
// ---------------------------------------------------------------------------

/// How many collections one store may curate at once
/// (Wave 54 extension). A storefront is a shop window,
/// not an archive: the cap keeps the seller's collection
/// list a merchandising tool and bounds the storefront's
/// section rendering.
pub const MAX_STORE_COLLECTIONS: i64 = 20;

/// How many products one store collection may hold at
/// once (Wave 54 extension). A collection is a curated
/// shelf; past this size it is the catalogue itself,
/// which the shop page already renders with filters.
pub const MAX_COLLECTION_PRODUCTS: i64 = 100;

/// Longest store-collection title, in characters.
pub const MAX_STORE_COLLECTION_TITLE_CHARS: usize = 120;

/// Longest store-collection description, in characters.
pub const MAX_STORE_COLLECTION_DESCRIPTION_CHARS: usize = 1_000;

/// Validate a store collection's title (Wave 54
/// extension): required, trimmed, at most
/// [`MAX_STORE_COLLECTION_TITLE_CHARS`] characters.
/// Returns the trimmed title.
pub fn validate_store_collection_title(title: &str) -> Result<String, String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err(
            "কালেকশনের নাম আবশ্যক — a collection title is required".to_string(),
        );
    }
    if trimmed.chars().count() > MAX_STORE_COLLECTION_TITLE_CHARS {
        return Err(format!(
            "কালেকশনের নাম সর্বোচ্চ {} অক্ষরের হতে হবে — collection title must be \
             at most {} characters",
            MAX_STORE_COLLECTION_TITLE_CHARS, MAX_STORE_COLLECTION_TITLE_CHARS
        ));
    }
    Ok(trimmed.to_string())
}

/// Validate a store collection's slug (Wave 54 extension):
/// the platform's one slug grammar
/// ([`validate_collection_slug`]), with the refusal
/// worded bilingually like the other storefront rules.
/// The slug is unique within the store (the DB constraint
/// is the backstop; this is the shape rule).
pub fn validate_store_collection_slug(slug: &str) -> Result<String, String> {
    match validate_collection_slug(slug) {
        Ok(valid) => Ok(valid.to_string()),
        Err(_) => Err(
            "স্লাগটি ৩-৪০ অক্ষরের হতে হবে (ছোট হাতের অক্ষর, সংখ্যা ও হাইফেন) — \
             the slug must be 3-40 characters of lowercase letters, digits and \
             hyphens"
                .to_string(),
        ),
    }
}

/// Validate a store collection's description (Wave 54
/// extension): optional text under the same convention as
/// the store profile texts — blank clears (returns
/// `None`), a value is trimmed and capped at
/// [`MAX_STORE_COLLECTION_DESCRIPTION_CHARS`] characters.
pub fn validate_store_collection_description(
    text: Option<&str>,
) -> Result<Option<String>, String> {
    validate_optional_text(
        text,
        MAX_STORE_COLLECTION_DESCRIPTION_CHARS,
        "description",
    )
    .map_err(|message| message.to_string())
}

/// Validate a store's collection count before creating
/// one more (Wave 54 extension): at
/// [`MAX_STORE_COLLECTIONS`] the store must retire a
/// collection first.
pub fn validate_store_collection_count(current: i64) -> Result<(), String> {
    if !(0..MAX_STORE_COLLECTIONS).contains(&current) {
        return Err(format!(
            "একটি স্টোর সর্বোচ্চ {}টি কালেকশন রাখতে পারে — a store may keep at \
             most {} collections",
            MAX_STORE_COLLECTIONS, MAX_STORE_COLLECTIONS
        ));
    }
    Ok(())
}

/// Validate a collection's membership size (Wave 54
/// extension): the count AFTER duplicates collapse (the
/// membership is a set — the API dedupes before calling)
/// must fit [`MAX_COLLECTION_PRODUCTS`].
pub fn validate_collection_product_count(count: i64) -> Result<(), String> {
    if !(0..=MAX_COLLECTION_PRODUCTS).contains(&count) {
        return Err(format!(
            "একটি কালেকশনে সর্বোচ্চ {}টি পণ্য থাকতে পারে — a collection may hold \
             at most {} products",
            MAX_COLLECTION_PRODUCTS, MAX_COLLECTION_PRODUCTS
        ));
    }
    Ok(())
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
            "about" => "about must be at most 2000 characters",
            "return_policy" => "return policy must be at most 2000 characters",
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
        Err("স্তরটি সঠিক নয় (tier must be one of: unverified, basic, verified)")
    }
}

/// Validate a Bangladeshi national ID (NID) number. NIDs are
/// issued in three lengths — 10, 13 and 17 digits — and sellers
/// routinely type them with spaces between groups, so spaces
/// are removed before the check; everything else must be an
/// ASCII digit. Returns the bare digit string for storage.
///
/// Bengali digits (০-৯) are transliterated to ASCII first
/// (admin verifications remake — a deliberate policy change
/// from the original "reject, never guess" rule, recorded in
/// the README): sellers type NIDs on Bengali keyboards, the
/// [`bn_to_en_digits`] map is a deterministic 1:1
/// transliteration, and the returned — therefore stored —
/// form is always the canonical ASCII string, so masking,
/// duplicate matching and the reviewer's full view all work
/// on one representation. Other digit scripts are not
/// converted and still fail the ASCII check.
///
/// This checks the SHAPE only: no checksum exists for NIDs
/// that a platform could verify offline, and authenticity is
/// established by the human document review, never by this
/// function (the README says so plainly).
pub fn validate_nid_number(raw: &str) -> Result<String, &'static str> {
    let converted = bn_to_en_digits(raw);
    let digits: String = converted.trim().chars().filter(|c| *c != ' ').collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err("NID নম্বরে শুধু সংখ্যা থাকতে হবে (NID number must contain digits only)");
    }
    if !matches!(digits.len(), 10 | 13 | 17) {
        return Err("NID নম্বর ১০, ১৩ বা ১৭ সংখ্যার হতে হবে (NID number must be 10, 13 or 17 digits)");
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

/// Rebuild [`mask_nid`]'s output from the two facts a masked
/// read path is allowed to hold: the number's length and its
/// stored last four (`store_verifications.nid_last4`, whose
/// match against the full number migration 0074 enforces).
/// Byte-for-byte identical to `mask_nid` for every input the
/// mask is defined on — including the short-input rule: a
/// length of four or fewer masks EVERYTHING and `last4` is
/// ignored, so a malformed short row can never leak itself
/// through this helper. The point of the split is that masked
/// surfaces (admin queue, seller view, payout summary) never
/// need to SELECT the full number at all.
pub fn mask_nid_parts(length: usize, last4: &str) -> String {
    if length <= 4 {
        return "*".repeat(length);
    }
    format!("{}{}", "*".repeat(length - 4), last4)
}

/// Maximum length of the applicant name on a verification
/// application, in characters (matches the migration 0074
/// CHECK on `store_verifications.applicant_name`).
pub const MAX_APPLICANT_NAME_CHARS: usize = 80;

/// Validate the applicant name a seller supplies with a
/// verification application: the name as it appears on the
/// NID, so the reviewer can compare name, number and document
/// image. Optional at the API (the seller page predates the
/// field) — `None` and blank input normalise to `None`; a
/// present value is trimmed and length-capped. The name is
/// self-asserted: nothing can check it against the NID
/// itself, which is exactly why the reviewer sees it next to
/// the document rather than the platform trusting it.
pub fn validate_applicant_name(text: Option<&str>) -> Result<Option<String>, &'static str> {
    let Some(raw) = text else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_APPLICANT_NAME_CHARS {
        return Err("আবেদনকারীর নাম সর্বোচ্চ ৮০ অক্ষরের হতে হবে (applicant name must be at most 80 characters)");
    }
    Ok(Some(trimmed.to_string()))
}

/// Maximum length of an admin's stated reason for viewing a
/// full NID, in characters.
pub const MAX_VIEW_REASON_CHARS: usize = 500;

/// Minimum length of that reason, in characters — a reason
/// shorter than this cannot say anything auditable.
pub const MIN_VIEW_REASON_CHARS: usize = 4;

/// Validate the reason an admin must state before the full
/// NID endpoint releases a number (admin verifications
/// remake). The reason is written to the admin audit log, so
/// it is held to two rules beyond shape: it must be long
/// enough to mean something, and it must never contain a run
/// of ten or more digits (ASCII or Bengali) — a reason is a
/// place admins paste things, and a pasted NID in the audit
/// detail would quietly create a second, unaudited copy of
/// the very number the endpoint exists to protect.
pub fn validate_view_reason(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("NID দেখার কারণ আবশ্যক (a reason for viewing the full NID is required)");
    }
    let len = trimmed.chars().count();
    if len < MIN_VIEW_REASON_CHARS {
        return Err("কারণটি খুবই ছোট — আরেকটু বিস্তারিত লিখুন (the reason is too short to be auditable)");
    }
    if len > MAX_VIEW_REASON_CHARS {
        return Err("কারণ সর্বোচ্চ ৫০০ অক্ষরের হতে হবে (reason must be at most 500 characters)");
    }
    let converted = bn_to_en_digits(trimmed);
    let mut run = 0usize;
    for c in converted.chars() {
        if c.is_ascii_digit() {
            run += 1;
            if run >= 10 {
                return Err("কারণে কোনো NID বা দীর্ঘ নম্বর লেখা যাবে না (the reason must not contain an NID or other long number)");
            }
        } else if c != ' ' && c != '-' {
            run = 0;
        }
    }
    Ok(trimmed.to_string())
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
        return Err("ট্রেড লাইসেন্স নম্বর ৪ থেকে ৪০ অক্ষরের হতে হবে (trade license number must be 4 to 40 characters)");
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
        return Err("প্রত্যাখ্যানের কারণ আবশ্যক (a rejection note is required)");
    }
    if trimmed.chars().count() > MAX_REVIEWER_NOTE_CHARS {
        return Err("পর্যালোচকের নোট সর্বোচ্চ ৫০০ অক্ষরের হতে হবে (reviewer note must be at most 500 characters)");
    }
    Ok(trimmed.to_string())
}

/// Maximum length of an admin's store suspension note, in
/// characters (admin stores remake) — the same cap as the
/// reviewer note: one vocabulary of admin reasons.
pub const MAX_SUSPENSION_NOTE_CHARS: usize = 500;

/// Validate an admin's suspension note for a store (admin
/// stores remake). The note is REQUIRED on suspension: it
/// is stored on the store row, written into the audit
/// detail, shown to the seller on their dashboard, and
/// quoted in the suspension notice — a seller whose
/// business is switched off is owed the reason in every
/// place the decision lands. Blank is an error; the value
/// is trimmed and may be at most
/// [`MAX_SUSPENSION_NOTE_CHARS`] characters, counted in
/// Unicode characters. Mirrors [`validate_reviewer_note`].
pub fn validate_suspension_note(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("স্থগিতের কারণ আবশ্যক (a suspension note is required)");
    }
    if trimmed.chars().count() > MAX_SUSPENSION_NOTE_CHARS {
        return Err("স্থগিতের নোট সর্বোচ্চ ৫০০ অক্ষরের হতে হবে (suspension note must be at most 500 characters)");
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
/// Admin payouts remake: a payout request was approved.
/// The wording must never claim the money was SENT —
/// approval only commits it; the `payout_paid` notice is
/// the one that announces the send.
pub const NOTIFICATION_PAYOUT_APPROVED: &str = "payout_approved";
/// Admin payouts remake: a payout request (or an approved
/// payout) was rejected. The body carries the admin's
/// reason via [`NotificationVars::note`], and the amount
/// returns to the seller's available balance.
pub const NOTIFICATION_PAYOUT_REJECTED: &str = "payout_rejected";
/// Admin verifications remake: a store's KYC verification
/// application was approved after MANUAL platform review.
/// The wording must never claim a government or database
/// NID check — no such check exists; a human compared the
/// submitted documents. The granted tier rides in
/// [`NotificationVars::verification_tier`].
pub const NOTIFICATION_VERIFICATION_APPROVED: &str = "verification_approved";
/// Admin verifications remake: a store's KYC verification
/// application was rejected. The body carries the reviewer's
/// note via [`NotificationVars::note`] (truncated for SMS —
/// the full note is on the seller's verification page), so
/// the seller knows what to fix before resubmitting.
pub const NOTIFICATION_VERIFICATION_REJECTED: &str = "verification_rejected";
/// Admin stores remake: a store was suspended by an admin.
/// The body says TEMPORARY (সাময়িকভাবে) and carries the
/// admin's note via [`NotificationVars::note`] (truncated
/// for SMS — the full note stands on the seller's
/// dashboard). It must never imply deletion or permanent
/// closure: suspension deletes nothing and is reversible.
pub const NOTIFICATION_STORE_SUSPENDED: &str = "store_suspended";
/// Admin stores remake: a suspended store was reactivated
/// by an admin and can sell again. No note rides along —
/// reactivation takes none.
pub const NOTIFICATION_STORE_REACTIVATED: &str = "store_reactivated";
pub const NOTIFICATION_RETURN_UPDATE: &str = "return_update";
/// Wave 54: a store a user follows published a new product;
/// the notice goes to each follower (see the API's
/// `notices` fan-out), one row per follower per product.
pub const NOTIFICATION_STORE_NEW_PRODUCT: &str = "store_new_product";

/// A product of a followed store joined a LIVE flash sale
/// (Wave 54 extension). Enqueued by the one funnel in the
/// API's notices module when the admin item-set of a
/// currently-running sale gains the product; muted
/// follows are excluded there, exactly like the
/// new-product kind.
pub const NOTIFICATION_STORE_FLASH_SALE: &str = "store_flash_sale";

/// Wave 59: a store a user follows published a blog post
/// that is live now. Enqueued by the one funnel in the
/// API's notices module inside the publish write;
/// muted follows are excluded there, exactly like the
/// new-product kind (the follow's one news mute covers
/// products, sales, and posts). A scheduled post does
/// not fire it — the post is not live yet, and nothing
/// fires later when the moment passes (the read-time
/// liveness rule has no transition to hook; documented
/// in the Wave 59 README section).
pub const NOTIFICATION_STORE_NEW_POST: &str = "store_new_post";

/// Wave 60: a gift card a seller issued still holds a
/// balance and its expiry is days away. Enqueued by the
/// worker's gift-card expiry sweep (one reminder per
/// card, stamped on the card row) to the ISSUING seller —
/// the person who can still reach the recipient before
/// the value lapses.
pub const NOTIFICATION_GIFT_CARD_EXPIRING: &str = "gift_card_expiring";

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
    /// The new product's title (Wave 54) — read only by
    /// the `store_new_product` template; empty for every
    /// other kind.
    pub product_title: String,
    /// The flash sale's title (Wave 54 extension) — read
    /// only by the `store_flash_sale` template; empty for
    /// every other kind.
    pub sale_title: String,
    /// The blog post's title (Wave 59) — read only by
    /// the `store_new_post` template; empty for every
    /// other kind.
    pub post_title: String,
    /// The gift card's last four characters (Wave 60) —
    /// read only by the `gift_card_expiring` template;
    /// empty for every other kind. Never more than the
    /// last4: a notification body is not a place for a
    /// bearer credential.
    pub card_last4: String,
    /// Free text a template must quote back to the reader
    /// (admin payouts remake) — read only by the
    /// `payout_rejected` template, which prints it as the
    /// rejection reason; empty for every other kind.
    pub note: String,
    /// The payment reference recorded when a payout was
    /// marked paid (admin payouts remake) — read only by
    /// the `payout_paid` template, which appends it so the
    /// seller can match the credit in their wallet or bank
    /// statement; empty when no reference was recorded
    /// (every payout paid before the remake).
    pub payment_reference: String,
    /// The verification tier granted by an approval
    /// (admin verifications remake) — read only by the
    /// `verification_approved` template, which maps it to
    /// the same Bangla tier label the VerificationBadge
    /// component prints; empty for every other kind. Named
    /// for the KYC tier specifically: it is NOT a
    /// subscription/plan tier.
    pub verification_tier: String,
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
        NOTIFICATION_PAYOUT_PAID => {
            // The reference (when one was recorded) rides
            // along so the seller can match the credit
            // against their wallet history or bank statement
            // — it is the only proof tying the message to a
            // real transfer.
            let reference = if vars.payment_reference.is_empty() {
                String::new()
            } else {
                format!(" রেফারেন্স: {}।", vars.payment_reference)
            };
            match vars.amount_poisha {
                Some(amount) => format!(
                    "BD Marketplace: আপনার {} পে-আউট পাঠানো হয়েছে।{reference}",
                    format_taka(amount)
                ),
                None => format!("BD Marketplace: আপনার পে-আউট পাঠানো হয়েছে।{reference}"),
            }
        }
        NOTIFICATION_PAYOUT_APPROVED => match vars.amount_poisha {
            // Approval commits the money; it does NOT send
            // it. The wording says exactly that — the paid
            // notice above is the only one that may claim a
            // send.
            Some(amount) => format!(
                "BD Marketplace: আপনার {} পে-আউট অনুরোধ অনুমোদিত হয়েছে। টাকা পাঠানো হলে আবার জানানো হবে।",
                format_taka(amount)
            ),
            None => "BD Marketplace: আপনার পে-আউট অনুরোধ অনুমোদিত হয়েছে। টাকা পাঠানো হলে আবার জানানো হবে।"
                .to_string(),
        },
        NOTIFICATION_PAYOUT_REJECTED => {
            let reason = if vars.note.is_empty() {
                String::new()
            } else {
                format!(" কারণ: {}।", vars.note)
            };
            match vars.amount_poisha {
                Some(amount) => format!(
                    "BD Marketplace: আপনার {} পে-আউট অনুরোধটি গৃহীত হয়নি।{reason} টাকাটি আপনার উপলব্ধ ব্যালেন্সে ফেরত গেছে।",
                    format_taka(amount)
                ),
                None => format!(
                    "BD Marketplace: আপনার পে-আউট অনুরোধটি গৃহীত হয়নি।{reason} টাকাটি আপনার উপলব্ধ ব্যালেন্সে ফেরত গেছে।"
                ),
            }
        }
        NOTIFICATION_VERIFICATION_APPROVED => {
            // The tier label is the VerificationBadge's own
            // wording (basic = যাচাইকৃত বিক্রেতা, verified =
            // যাচাইকৃত ব্যবসা) so the SMS and the storefront
            // badge can never drift apart. An unknown or empty
            // tier omits the tier phrase entirely — printing
            // the unverified label on an approval would lie.
            // The claim is approval of the APPLICATION after
            // the platform team's manual document review —
            // never that the NID itself was authenticated
            // against any government database (no such check
            // exists).
            let tier_phrase = match vars.verification_tier.as_str() {
                "basic" => " আপনার স্টোর এখন যাচাইকৃত বিক্রেতা।",
                "verified" => " আপনার স্টোর এখন যাচাইকৃত ব্যবসা।",
                _ => "",
            };
            format!(
                "BD Marketplace: প্ল্যাটফর্ম দলের পর্যালোচনায় আপনার যাচাইকরণ আবেদন অনুমোদিত হয়েছে।{tier_phrase}"
            )
        }
        NOTIFICATION_VERIFICATION_REJECTED => {
            // The reviewer note is the actionable content, but
            // it can run to 500 characters — at UCS-2 widths
            // that is many SMS segments, and a gateway may cut
            // it mid-sentence. The SMS therefore carries a
            // truncated note and points at the seller page,
            // where the full note always stands. This
            // deliberately diverges from `payout_rejected`
            // (whose reasons are short operational text):
            // KYC notes are about identity documents and are
            // long by nature.
            let reason = if vars.note.is_empty() {
                String::new()
            } else {
                let note: String = vars.note.chars().take(120).collect();
                let ellipsis = if vars.note.chars().count() > 120 { "…" } else { "" };
                format!(" কারণ: {note}{ellipsis}। বিস্তারিত আপনার যাচাইকরণ পেজে দেখুন।")
            };
            format!(
                "BD Marketplace: আপনার যাচাইকরণ আবেদনটি গৃহীত হয়নি।{reason} সংশোধন করে আবার আবেদন করুন।"
            )
        }
        NOTIFICATION_STORE_SUSPENDED => {
            // The note is the actionable content, and it can
            // run to 500 characters — the SMS carries a
            // truncated note and points at the seller
            // dashboard, where the full note stands (the
            // verification_rejected truncation rule).
            // The wording says TEMPORARY and claims nothing
            // stronger: nothing was deleted, no payout was
            // cancelled, and the store can be restored.
            let reason = if vars.note.is_empty() {
                String::new()
            } else {
                let note: String = vars.note.chars().take(120).collect();
                let ellipsis = if vars.note.chars().count() > 120 { "…" } else { "" };
                format!(" কারণ: {note}{ellipsis}। বিস্তারিত আপনার ড্যাশবোর্ডে দেখুন।")
            };
            format!(
                "BD Marketplace: আপনার স্টোর {store} সাময়িকভাবে স্থগিত করা হয়েছে।{reason}"
            )
        }
        NOTIFICATION_STORE_REACTIVATED => format!(
            "BD Marketplace: আপনার স্টোর {store} আবার চালু করা হয়েছে। এখন আবার বিক্রি করতে পারবেন।"
        ),
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
        NOTIFICATION_STORE_NEW_PRODUCT => {
            if vars.product_title.is_empty() {
                format!("BD Marketplace: {store} নতুন পণ্য যোগ করেছে। স্টোরটি ঘুরে দেখুন!")
            } else {
                format!(
                    "BD Marketplace: {store} নতুন পণ্য যোগ করেছে: {}।",
                    vars.product_title
                )
            }
        }
        NOTIFICATION_STORE_FLASH_SALE => {
            if vars.sale_title.is_empty() {
                format!("BD Marketplace: {store} স্টোরের পণ্যে ফ্ল্যাশ সেল চলছে!")
            } else {
                format!(
                    "BD Marketplace: {store} স্টোরের পণ্যে ফ্ল্যাশ সেল চলছে: {}!",
                    vars.sale_title
                )
            }
        }
        NOTIFICATION_STORE_NEW_POST => {
            if vars.post_title.is_empty() {
                format!("BD Marketplace: {store} নতুন ব্লগ পোস্ট প্রকাশ করেছে। পড়ে দেখুন!")
            } else {
                format!(
                    "BD Marketplace: {store} নতুন ব্লগ পোস্ট প্রকাশ করেছে: {}।",
                    vars.post_title
                )
            }
        }
        NOTIFICATION_GIFT_CARD_EXPIRING => match vars.amount_poisha {
            Some(amount) => format!(
                "BD Marketplace: {store}-এর গিফট কার্ড ••{}-এর মেয়াদ শীঘ্রই শেষ হচ্ছে। বকেয়া ব্যালেন্স {}।",
                vars.card_last4,
                format_taka(amount)
            ),
            None => format!(
                "BD Marketplace: {store}-এর গিফট কার্ড ••{}-এর মেয়াদ শীঘ্রই শেষ হচ্ছে।",
                vars.card_last4
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

// ---------------------------------------------------------------------------
// Bangladesh districts + delivery zones (Wave 55)
// ---------------------------------------------------------------------------

/// One of Bangladesh's 64 districts (Wave 55), with the
/// delivery zone the platform assigns it. The mapping is
/// PLATFORM POLICY at district granularity:
///
/// - `InsideDhaka` is the Dhaka district alone — the
///   metro courier zone every Bangladeshi courier prices
///   as "inside Dhaka".
/// - `DhakaSuburb` is the five districts that BORDER
///   Dhaka district (Gazipur, Narayanganj, Munshiganj,
///   Narsingdi, Manikganj) — the ring couriers price as
///   the Dhaka periphery. Tangail is deliberately NOT in
///   the ring: it does not border Dhaka district (Gazipur
///   sits between), so it prices as outside.
/// - Everything else is `OutsideDhaka`.
///
/// Thana-level refinement (Keraniganj pricing as suburb
/// although it sits inside Dhaka district, say) is a
/// documented limit of this wave: the district is the
/// finest geography the address book records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct District {
    /// Canonical English name — the value stored on
    /// addresses and orders and sent by clients.
    pub name_en: &'static str,
    /// The district's Bangla name, for the picker UI.
    pub name_bn: &'static str,
    /// The delivery zone this district prices under.
    pub zone: DeliveryZone,
}

/// The 64 districts of Bangladesh (Wave 55), alphabetical
/// by canonical English name, each with its zone under
/// the policy on [`District`]. This table is the ONE
/// district → zone mapping: the address book validates
/// against it, checkout derives zones from it, and the
/// public `GET /v1/districts` serves it verbatim.
pub const DISTRICTS: &[District] = &[
    District { name_en: "Bagerhat", name_bn: "বাগেরহাট", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Bandarban", name_bn: "বান্দরবান", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Barguna", name_bn: "বরগুনা", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Barishal", name_bn: "বরিশাল", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Bhola", name_bn: "ভোলা", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Bogura", name_bn: "বগুড়া", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Brahmanbaria", name_bn: "ব্রাহ্মণবাড়িয়া", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Chandpur", name_bn: "চাঁদপুর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Chapai Nawabganj", name_bn: "চাঁপাইনবাবগঞ্জ", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Chattogram", name_bn: "চট্টগ্রাম", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Chuadanga", name_bn: "চুয়াডাঙ্গা", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Cox's Bazar", name_bn: "কক্সবাজার", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Cumilla", name_bn: "কুমিল্লা", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Dhaka", name_bn: "ঢাকা", zone: DeliveryZone::InsideDhaka },
    District { name_en: "Dinajpur", name_bn: "দিনাজপুর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Faridpur", name_bn: "ফরিদপুর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Feni", name_bn: "ফেনী", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Gaibandha", name_bn: "গাইবান্ধা", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Gazipur", name_bn: "গাজীপুর", zone: DeliveryZone::DhakaSuburb },
    District { name_en: "Gopalganj", name_bn: "গোপালগঞ্জ", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Habiganj", name_bn: "হবিগঞ্জ", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Jamalpur", name_bn: "জামালপুর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Jashore", name_bn: "যশোর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Jhalokati", name_bn: "ঝালকাঠি", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Jhenaidah", name_bn: "ঝিনাইদহ", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Joypurhat", name_bn: "জয়পুরহাট", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Khagrachari", name_bn: "খাগড়াছড়ি", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Khulna", name_bn: "খুলনা", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Kishoreganj", name_bn: "কিশোরগঞ্জ", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Kurigram", name_bn: "কুড়িগ্রাম", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Kushtia", name_bn: "কুষ্টিয়া", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Lakshmipur", name_bn: "লক্ষ্মীপুর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Lalmonirhat", name_bn: "লালমনিরহাট", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Madaripur", name_bn: "মাদারীপুর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Magura", name_bn: "মাগুরা", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Manikganj", name_bn: "মানিকগঞ্জ", zone: DeliveryZone::DhakaSuburb },
    District { name_en: "Meherpur", name_bn: "মেহেরপুর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Moulvibazar", name_bn: "মৌলভীবাজার", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Munshiganj", name_bn: "মুন্সীগঞ্জ", zone: DeliveryZone::DhakaSuburb },
    District { name_en: "Mymensingh", name_bn: "ময়মনসিংহ", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Naogaon", name_bn: "নওগাঁ", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Narail", name_bn: "নড়াইল", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Narayanganj", name_bn: "নারায়ণগঞ্জ", zone: DeliveryZone::DhakaSuburb },
    District { name_en: "Narsingdi", name_bn: "নরসিংদী", zone: DeliveryZone::DhakaSuburb },
    District { name_en: "Natore", name_bn: "নাটোর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Netrokona", name_bn: "নেত্রকোনা", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Nilphamari", name_bn: "নীলফামারী", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Noakhali", name_bn: "নোয়াখালী", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Pabna", name_bn: "পাবনা", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Panchagarh", name_bn: "পঞ্চগড়", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Patuakhali", name_bn: "পটুয়াখালী", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Pirojpur", name_bn: "পিরোজপুর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Rajbari", name_bn: "রাজবাড়ী", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Rajshahi", name_bn: "রাজশাহী", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Rangamati", name_bn: "রাঙ্গামাটি", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Rangpur", name_bn: "রংপুর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Satkhira", name_bn: "সাতক্ষীরা", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Shariatpur", name_bn: "শরীয়তপুর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Sherpur", name_bn: "শেরপুর", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Sirajganj", name_bn: "সিরাজগঞ্জ", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Sunamganj", name_bn: "সুনামগঞ্জ", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Sylhet", name_bn: "সিলেট", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Tangail", name_bn: "টাঙ্গাইল", zone: DeliveryZone::OutsideDhaka },
    District { name_en: "Thakurgaon", name_bn: "ঠাকুরগাঁও", zone: DeliveryZone::OutsideDhaka },
];

/// Look a district up by name (Wave 55): the canonical
/// English name matched case-insensitively after
/// trimming, or the Bangla name matched exactly. Anything
/// else is `None` — a free-typed district the table does
/// not know can never silently acquire a zone.
pub fn district_by_name(name: &str) -> Option<&'static District> {
    let trimmed = name.trim();
    DISTRICTS.iter().find(|district| {
        district.name_en.eq_ignore_ascii_case(trimmed) || district.name_bn == trimmed
    })
}

/// The delivery zone of a district by name (Wave 55) —
/// [`district_by_name`] projected to its zone.
pub fn zone_for_district(name: &str) -> Option<DeliveryZone> {
    district_by_name(name).map(|district| district.zone)
}

/// Search the district table by a free-text query (Wave
/// 55) — for the address form's picker, where scrolling
/// 64 entries on a phone is miserable. Matching is
/// deliberately simple and predictable: the query is
/// trimmed; a district matches when its English name
/// STARTS with the query (rank 0, case-insensitive),
/// CONTAINS it (rank 1), or its Bangla name contains it
/// (rank 2). Results sort by rank, then by the table's
/// canonical order, capped at `limit` (zero yields
/// nothing). An empty query returns the first `limit`
/// districts in canonical order — a picker's initial
/// state.
pub fn search_districts(query: &str, limit: usize) -> Vec<&'static District> {
    if limit == 0 {
        return Vec::new();
    }
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return DISTRICTS.iter().take(limit).collect();
    }
    let trimmed = query.trim();
    let mut ranked: Vec<(u8, usize, &'static District)> = DISTRICTS
        .iter()
        .enumerate()
        .filter_map(|(index, district)| {
            let en = district.name_en.to_lowercase();
            let rank = if en.starts_with(&needle) {
                0
            } else if en.contains(&needle) {
                1
            } else if district.name_bn.contains(trimmed) {
                2
            } else {
                return None;
            };
            Some((rank, index, district))
        })
        .collect();
    ranked.sort_by_key(|(rank, index, _)| (*rank, *index));
    ranked
        .into_iter()
        .take(limit)
        .map(|(_, _, district)| district)
        .collect()
}

// ---------------------------------------------------------------------------
// Buyer address book (Wave 55)
// ---------------------------------------------------------------------------

/// Longest address-book label, in characters (Wave 55) —
/// "বাসা", "অফিস", "আম্মুর বাসা" are short by nature.
pub const MAX_ADDRESS_LABEL_CHARS: usize = 40;

/// Longest recipient name on an address (Wave 55).
pub const MAX_ADDRESS_RECIPIENT_CHARS: usize = 80;

/// Longest thana/area text on an address (Wave 55).
pub const MAX_ADDRESS_AREA_CHARS: usize = 80;

/// Longest street-address line on an address (Wave 55).
pub const MAX_ADDRESS_LINE_CHARS: usize = 200;

/// How many addresses one buyer may keep (Wave 55): a
/// book, not a database — the cap keeps the checkout
/// picker a short radio list.
pub const MAX_ADDRESSES_PER_USER: i64 = 10;

/// The raw address payload, as the API receives it
/// (Wave 55) — every field unvalidated text; the
/// validated form is [`ValidatedAddress`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressInput {
    pub label: String,
    pub recipient_name: String,
    pub phone: String,
    pub district: String,
    pub area: String,
    pub address_line: String,
}

/// An address that passed [`validate_address_input`]
/// (Wave 55): trimmed fields, the phone in canonical
/// `+880…` form, the district in its canonical English
/// spelling, and the zone the district table assigns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedAddress {
    pub label: String,
    pub recipient_name: String,
    /// Canonical phone (`+8801…`), via
    /// [`normalize_bd_phone`].
    pub phone: String,
    /// Canonical English district name from
    /// [`DISTRICTS`].
    pub district: String,
    pub area: String,
    pub address_line: String,
    /// The district's zone — derived, never stored.
    pub zone: DeliveryZone,
}

/// Trim one address text field and enforce its length
/// cap (Wave 55): blank is an error (every address field
/// is required — a half-known address misdelivers), over
/// the cap is an error naming the field in both
/// languages.
fn validate_address_field(
    raw: &str,
    max_chars: usize,
    label_bn: &str,
    label_en: &str,
) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{label_bn} দিতে হবে — {label_en} is required"));
    }
    if trimmed.chars().count() > max_chars {
        return Err(format!(
            "{label_bn} সর্বোচ্চ {max_chars} অক্ষরের হতে হবে — \
             {label_en} must be at most {max_chars} characters"
        ));
    }
    Ok(trimmed.to_string())
}

/// Validate one address-book entry (Wave 55): every text
/// field present and within its cap, the phone a valid
/// Bangladeshi mobile (the platform's one phone rule),
/// and the district a real district from [`DISTRICTS`] —
/// its zone comes from the table, so a stored address
/// can never carry a zone the district disagrees with.
pub fn validate_address_input(input: &AddressInput) -> Result<ValidatedAddress, String> {
    let label = validate_address_field(
        &input.label,
        MAX_ADDRESS_LABEL_CHARS,
        "লেবেল",
        "label",
    )?;
    let recipient_name = validate_address_field(
        &input.recipient_name,
        MAX_ADDRESS_RECIPIENT_CHARS,
        "গ্রহীতার নাম",
        "recipient name",
    )?;
    let area = validate_address_field(
        &input.area,
        MAX_ADDRESS_AREA_CHARS,
        "থানা/এলাকা",
        "thana/area",
    )?;
    let address_line = validate_address_field(
        &input.address_line,
        MAX_ADDRESS_LINE_CHARS,
        "ঠিকানা",
        "address",
    )?;
    let phone = normalize_bd_phone(&input.phone).ok_or_else(|| {
        "সঠিক বাংলাদেশি মোবাইল নম্বর দিন — enter a valid Bangladeshi mobile number"
            .to_string()
    })?;
    let district = district_by_name(&input.district).ok_or_else(|| {
        "জেলাটি তালিকায় নেই — choose a district from the list".to_string()
    })?;
    Ok(ValidatedAddress {
        label,
        recipient_name,
        phone,
        district: district.name_en.to_string(),
        area,
        address_line,
        zone: district.zone,
    })
}

/// May this buyer save one more address (Wave 55)? At
/// [`MAX_ADDRESSES_PER_USER`] the next save is refused;
/// the count is the caller's CURRENT row count.
pub fn validate_address_count(current_addresses: i64) -> Result<(), String> {
    if current_addresses < 0 {
        return Err("address count cannot be negative".to_string());
    }
    if current_addresses >= MAX_ADDRESSES_PER_USER {
        return Err(format!(
            "আপনার সর্বোচ্চ {MAX_ADDRESSES_PER_USER}টি ঠিকানা সংরক্ষিত আছে — \
             you already keep the maximum of {MAX_ADDRESSES_PER_USER} addresses; \
             delete one first"
        ));
    }
    Ok(())
}

/// Compose the one-line delivery address an order
/// stores (Wave 55): street line, thana/area, district —
/// the same composition the courier label prints, so
/// the order's free-text address and the address book
/// can never drift apart in format.
pub fn compose_delivery_address(address_line: &str, area: &str, district: &str) -> String {
    format!("{address_line}, {area}, {district}")
}

// ---------------------------------------------------------------------------
// Order note (Wave 55)
// ---------------------------------------------------------------------------

/// Longest buyer order note, in characters (Wave 55) —
/// a delivery instruction, not a letter.
pub const MAX_ORDER_NOTE_CHARS: usize = 500;

/// Validate a buyer's order note (Wave 55): `None` and
/// blank become `None` (no note), a present note is
/// trimmed, and over [`MAX_ORDER_NOTE_CHARS`] is an
/// error. The note rides the ORDER (every seller of a
/// split order reads the same note — it is addressed to
/// whoever delivers, and checkout has one note box, not
/// one per seller).
pub fn validate_order_note(note: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = note else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_ORDER_NOTE_CHARS {
        return Err(format!(
            "অর্ডার নোট সর্বোচ্চ {MAX_ORDER_NOTE_CHARS} অক্ষরের হতে হবে — \
             the order note must be at most {MAX_ORDER_NOTE_CHARS} characters"
        ));
    }
    Ok(Some(trimmed.to_string()))
}

// ---------------------------------------------------------------------------
// Reorder / buy-again (Wave 55)
// ---------------------------------------------------------------------------

/// Why one line of a past order could not be re-added
/// (Wave 55). The codes are the API's report vocabulary;
/// [`ReorderSkipReason::message`] is the buyer-facing
/// bilingual explanation the orders page prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReorderSkipReason {
    /// The product is gone or no longer sells (draft /
    /// archived / suspended-store stock is not buyable).
    ProductUnavailable,
    /// The bought variant no longer exists as an active
    /// variant, or the product has since gained variants
    /// the buyer never picked.
    VariantUnavailable,
    /// The line's stock pool is empty.
    OutOfStock,
    /// The cart line is already at the platform's
    /// per-line maximum — nothing more may be added.
    CartLimit,
    /// The product is fine, but its STORE is suspended
    /// (admin stores remake): a suspended store takes no
    /// orders, so its lines cannot re-enter a cart until
    /// the store is restored. Kept distinct from
    /// `ProductUnavailable` so the buyer hears the true,
    /// fixable reason instead of "no longer sold".
    StoreSuspended,
}

impl ReorderSkipReason {
    /// The stable machine code for the report.
    pub fn as_str(&self) -> &'static str {
        match self {
            ReorderSkipReason::ProductUnavailable => "product_unavailable",
            ReorderSkipReason::VariantUnavailable => "variant_unavailable",
            ReorderSkipReason::OutOfStock => "out_of_stock",
            ReorderSkipReason::CartLimit => "cart_limit",
            ReorderSkipReason::StoreSuspended => "store_suspended",
        }
    }

    /// The buyer-facing bilingual explanation.
    pub fn message(&self) -> &'static str {
        match self {
            ReorderSkipReason::ProductUnavailable => {
                "পণ্যটি এখন আর বিক্রি হচ্ছে না — the product is no longer sold"
            }
            ReorderSkipReason::VariantUnavailable => {
                "আকার/রংটি এখন আর নেই — that size/colour is no longer available"
            }
            ReorderSkipReason::OutOfStock => "স্টক শেষ — out of stock",
            ReorderSkipReason::CartLimit => {
                "কার্টে ইতিমধ্যে সর্বোচ্চ পরিমাণ আছে — the cart already holds the maximum"
            }
            ReorderSkipReason::StoreSuspended => {
                "স্টোরটি সাময়িকভাবে স্থগিত আছে — the store is temporarily suspended"
            }
        }
    }
}

/// What reorder should do with one past-order line
/// (Wave 55): add this many units at TODAY's price, or
/// skip the line with a named reason. A partial add
/// (fewer units than the original line) is an `Add`
/// with the clamped quantity — the report shows the
/// number actually added, never the number wished for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReorderDecision {
    Add { quantity: i64 },
    Skip(ReorderSkipReason),
}

/// The state of the variant a past-order line bought
/// (Wave 55), as the reorder service reads it today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReorderVariantState {
    /// The line bought no variant, and the product has
    /// none to pick today either.
    NotApplicable,
    /// The bought variant exists and is active.
    Active,
    /// The bought variant exists but is deactivated.
    Inactive,
    /// The bought variant row is gone entirely.
    Missing,
    /// The line bought no variant, but the product has
    /// since gained active variants — the buyer must
    /// re-pick (the same stale-line rule checkout
    /// enforces in Wave 33).
    ProductGainedVariants,
}

/// Decide one reorder line (Wave 55) from today's
/// facts: the product's current status (`None` = the
/// row is gone), the product's STORE's current status
/// (`None` = the store row is gone; anything but
/// `active` skips the line — admin stores remake), the
/// bought variant's state, the stock
/// of the line's pool (variant stock for a variant
/// line, product stock otherwise), the originally
/// ordered quantity, and the room left in the cart
/// line under [`MAX_CART_QUANTITY`] (0 when the line
/// already sits at the cap). Checks run in report
/// order: product, store, variant, stock, cart room — the
/// FIRST failing one is the reported reason.
pub fn reorder_decision(
    product_status: Option<&str>,
    store_status: Option<&str>,
    variant_state: ReorderVariantState,
    stock: i64,
    requested: i64,
    cart_headroom: i64,
) -> ReorderDecision {
    if product_status != Some("active") {
        return ReorderDecision::Skip(ReorderSkipReason::ProductUnavailable);
    }
    if store_status != Some("active") {
        return ReorderDecision::Skip(ReorderSkipReason::StoreSuspended);
    }
    match variant_state {
        ReorderVariantState::NotApplicable | ReorderVariantState::Active => {}
        ReorderVariantState::Inactive
        | ReorderVariantState::Missing
        | ReorderVariantState::ProductGainedVariants => {
            return ReorderDecision::Skip(ReorderSkipReason::VariantUnavailable);
        }
    }
    if stock <= 0 {
        return ReorderDecision::Skip(ReorderSkipReason::OutOfStock);
    }
    let quantity = requested.min(stock).min(cart_headroom);
    if quantity <= 0 {
        return ReorderDecision::Skip(ReorderSkipReason::CartLimit);
    }
    ReorderDecision::Add { quantity }
}

// ---------------------------------------------------------------------------
// Order tracking steps (Wave 55)
// ---------------------------------------------------------------------------

/// Where one tracking step stands (Wave 55): `Done` —
/// it happened; `Current` — the parcel is here now;
/// `Upcoming` — still ahead; `Alert` — the branch that
/// ended the happy path (cancelled / returned), shown
/// distinctly rather than as a failure of the buyer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackingStepState {
    Done,
    Current,
    Upcoming,
    Alert,
}

impl TrackingStepState {
    /// The wire string the steppers render from.
    pub fn as_str(&self) -> &'static str {
        match self {
            TrackingStepState::Done => "done",
            TrackingStepState::Current => "current",
            TrackingStepState::Upcoming => "upcoming",
            TrackingStepState::Alert => "alert",
        }
    }
}

/// One step of a sub-order's tracking stepper (Wave 55):
/// a stable `kind` (`placed`, `confirmed`, `shipped`,
/// `delivered`, `returned`, `cancelled`) and its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackingStep {
    pub kind: &'static str,
    pub state: TrackingStepState,
}

/// The tracking stepper of one sub-order (Wave 55): the
/// steps are derived from the sub-order's status ALONE
/// (plus whether a courier shipment is under way, which
/// distinguishes a seller-marked `shipped` from a
/// courier-carried one only in copy, never in shape) —
/// the SAME derivation the buyer order page, the public
/// tracking page and this crate's tests all read, so no
/// surface invents its own progress model.
///
/// The happy path is placed → confirmed → shipped →
/// delivered. `returned` keeps the travelled steps Done
/// and ends in an Alert step (a return may follow a
/// delivery or a courier RTO — the status alone cannot
/// say which, so no `delivered` step is claimed for it).
/// `cancelled` ends the path where it stands.
pub fn sub_order_tracking_steps(
    status: &str,
    shipment_status: Option<&str>,
) -> Vec<TrackingStep> {
    use TrackingStepState::{Alert, Current, Done, Upcoming};
    let step = |kind: &'static str, state: TrackingStepState| TrackingStep { kind, state };
    // A courier leg in a terminal failure state (the
    // shipment itself was cancelled) reads as the
    // cancelled branch even before the sub-order's own
    // status catches up.
    if shipment_status == Some("cancelled") && status != "delivered" && status != "returned" {
        return vec![
            step("placed", Done),
            step("confirmed", Done),
            step("cancelled", Alert),
        ];
    }
    match status {
        "confirmed" => vec![
            step("placed", Done),
            step("confirmed", Current),
            step("shipped", Upcoming),
            step("delivered", Upcoming),
        ],
        "shipped" => vec![
            step("placed", Done),
            step("confirmed", Done),
            step("shipped", Current),
            step("delivered", Upcoming),
        ],
        "delivered" => vec![
            step("placed", Done),
            step("confirmed", Done),
            step("shipped", Done),
            step("delivered", Done),
        ],
        "returned" => vec![
            step("placed", Done),
            step("confirmed", Done),
            step("shipped", Done),
            step("returned", Alert),
        ],
        "cancelled" => vec![step("placed", Done), step("cancelled", Alert)],
        // `placed` — and, defensively, any status this
        // crate does not know: the path has only begun.
        _ => vec![
            step("placed", Current),
            step("confirmed", Upcoming),
            step("shipped", Upcoming),
            step("delivered", Upcoming),
        ],
    }
}

// ---------------------------------------------------------------------------
// Category taxonomy (Wave 56)
// ---------------------------------------------------------------------------
//
// The managed category tree that replaces free-text-only
// categorisation. A product still carries its seller-typed
// text label (`products.category` — the denormalised display
// name every existing surface reads), and now also an
// optional link (`products.category_id`) into this tree. The
// rules below are the whole taxonomy contract, in pure
// functions over flat rows so the API layer, tests and any
// future tooling share one implementation:
//
// - A slug is the node's stable public identifier, unique
//   across the WHOLE tree (a slug is a URL segment).
// - The tree is at most [`MAX_CATEGORY_DEPTH`] levels deep:
//   top level, child, grandchild. Deeper trees are a
//   navigation burden on a phone-first storefront.
// - Reparenting may never create a cycle, and may never push
//   any part of the moved subtree past the depth cap.
// - A category with children, or with products still linked
//   to it, cannot be deleted — the admin reassigns or empties
//   it first. (Merging is the supported bulk alternative.)
// - A free-text label links to a node only on an EXACT match
//   ([`resolve_category_link`]) — the same rule the Wave 56
//   backfill migration applies, so behaviour is identical
//   whether a product was linked at migration time or at
//   write time.

/// The deepest a category may sit: 1 = top level, 2 = child,
/// 3 = grandchild. The cap is a merchandising rule (phone
/// navigation), enforced here rather than in a database
/// constraint so the error can name the offending move.
pub const MAX_CATEGORY_DEPTH: i64 = 3;

/// A category name (either language) may carry at most this
/// many characters.
pub const MAX_CATEGORY_NAME_CHARS: usize = 80;

/// The icon a category falls back to when none is chosen.
pub const DEFAULT_CATEGORY_ICON: &str = "tag";

/// The built-in icon set a category's `icon_key` may name.
/// Icons are KEYS into this set — the web app renders each
/// key with its own glyph — never uploaded images, so a
/// category can never point at a broken or hostile asset.
pub const CATEGORY_ICON_KEYS: &[&str] = &[
    "tag",
    "shirt",
    "phone",
    "cart",
    "basket",
    "home",
    "sparkles",
    "baby",
    "trophy",
    "car",
    "leaf",
    "book",
    "grid",
    "watch",
    "shoe",
    "laptop",
    "heart",
    "gift",
];

/// One flat taxonomy row, as the API layer loads it from the
/// `categories` table. Ids are strings so the pure tree
/// functions stay storage-agnostic (the API converts its
/// uuids at the boundary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryFlat {
    pub id: String,
    pub parent_id: Option<String>,
    pub slug: String,
    pub name_en: String,
    pub name_bn: Option<String>,
    pub icon_key: String,
    pub sort_order: i64,
    pub is_active: bool,
    pub is_featured: bool,
}

/// One node of the built taxonomy tree: the flat row, its
/// 1-based depth, and its children in display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub slug: String,
    pub name_en: String,
    pub name_bn: Option<String>,
    pub icon_key: String,
    pub sort_order: i64,
    pub is_active: bool,
    pub is_featured: bool,
    pub depth: i64,
    pub children: Vec<CategoryNode>,
}

/// Validate a category slug. Slugs follow the platform-wide
/// slug rule ([`is_valid_slug`]): 3–40 chars, lowercase ASCII
/// letters/digits/hyphens, starting and ending with a letter
/// or digit. The error is bilingual like every storefront
/// validation.
pub fn validate_category_slug(slug: &str) -> Result<String, String> {
    let slug = slug.trim();
    if is_valid_slug(slug) {
        Ok(slug.to_string())
    } else {
        Err(
            "category slug must be 3-40 chars of lowercase letters, digits and hyphens, \
             starting and ending with a letter or digit — \
             ক্যাটাগরির স্লাগ ৩–৪০ অক্ষরের হতে হবে (ছোট হাতের অক্ষর, সংখ্যা ও হাইফেন)"
                .to_string(),
        )
    }
}

/// Validate a category's names: the English name is required
/// (1..=[`MAX_CATEGORY_NAME_CHARS`] chars after trimming);
/// the Bangla name is optional, blank clears it, and the
/// same cap applies. Returns the normalised pair.
pub fn validate_category_names(
    name_en: &str,
    name_bn: Option<&str>,
) -> Result<(String, Option<String>), String> {
    let name_en = name_en.trim();
    if name_en.is_empty() {
        return Err(
            "category needs an English name — ক্যাটাগরির ইংরেজি নাম আবশ্যক".to_string(),
        );
    }
    if name_en.chars().count() > MAX_CATEGORY_NAME_CHARS {
        return Err(format!(
            "category name is too long (max {MAX_CATEGORY_NAME_CHARS} characters) — \
             ক্যাটাগরির নাম সর্বোচ্চ {MAX_CATEGORY_NAME_CHARS} অক্ষরের হতে পারবে"
        ));
    }
    let name_bn = match name_bn {
        Some(raw) if !raw.trim().is_empty() => {
            let trimmed = raw.trim();
            if trimmed.chars().count() > MAX_CATEGORY_NAME_CHARS {
                return Err(format!(
                    "category Bangla name is too long (max {MAX_CATEGORY_NAME_CHARS} characters) — \
                     ক্যাটাগরির বাংলা নাম সর্বোচ্চ {MAX_CATEGORY_NAME_CHARS} অক্ষরের হতে পারবে"
                ));
            }
            Some(trimmed.to_string())
        }
        _ => None,
    };
    Ok((name_en.to_string(), name_bn))
}

/// Validate an icon key: absent (or blank) means the
/// [`DEFAULT_CATEGORY_ICON`]; a present key must name a
/// member of the built-in set.
pub fn validate_category_icon(icon_key: Option<&str>) -> Result<String, String> {
    match icon_key {
        Some(raw) if !raw.trim().is_empty() => {
            let key = raw.trim();
            if CATEGORY_ICON_KEYS.contains(&key) {
                Ok(key.to_string())
            } else {
                Err(format!(
                    "unknown category icon '{key}' — অজানা ক্যাটাগরি আইকন '{key}'"
                ))
            }
        }
        _ => Ok(DEFAULT_CATEGORY_ICON.to_string()),
    }
}

/// Look up one flat row by id.
pub fn category_by_id<'a>(rows: &'a [CategoryFlat], id: &str) -> Option<&'a CategoryFlat> {
    rows.iter().find(|row| row.id == id)
}

/// Look up one flat row by slug.
pub fn category_by_slug<'a>(rows: &'a [CategoryFlat], slug: &str) -> Option<&'a CategoryFlat> {
    rows.iter().find(|row| row.slug == slug)
}

/// The 1-based depth of a node (top level = 1), computed by
/// walking the parent chain. Returns `None` when the id is
/// unknown. The walk is cycle-safe: a chain longer than the
/// row count is by definition corrupt, and the walk stops
/// there (reporting the cap-exceeding depth so validators
/// reject the state rather than loop forever).
pub fn category_depth(rows: &[CategoryFlat], id: &str) -> Option<i64> {
    let mut current = category_by_id(rows, id)?;
    let mut depth: i64 = 1;
    while let Some(parent_id) = current.parent_id.as_deref() {
        depth += 1;
        if depth > rows.len() as i64 + 1 {
            return Some(depth);
        }
        current = category_by_id(rows, parent_id)?;
    }
    Some(depth)
}

/// Every id in a node's subtree, the node itself first, then
/// descendants in tree order (siblings by sort order, then
/// name). Empty when the id is unknown. This is the id set a
/// subtree browse resolves a slug to.
pub fn subtree_ids(rows: &[CategoryFlat], id: &str) -> Vec<String> {
    let mut out = Vec::new();
    if category_by_id(rows, id).is_none() {
        return out;
    }
    collect_subtree_ids(rows, id, &mut out);
    out
}

fn collect_subtree_ids(rows: &[CategoryFlat], id: &str, out: &mut Vec<String>) {
    if out.iter().any(|seen| seen == id) {
        return; // corrupt cycle in the stored rows: stop, never loop
    }
    out.push(id.to_string());
    let mut children: Vec<&CategoryFlat> = rows
        .iter()
        .filter(|row| row.parent_id.as_deref() == Some(id))
        .collect();
    children.sort_by(|a, b| category_display_order(a, b));
    for child in children {
        collect_subtree_ids(rows, &child.id, out);
    }
}

/// The height of a node's subtree: 1 for a leaf, +1 per level
/// of descendants. 0 when the id is unknown. Cycle-safe like
/// [`category_depth`].
pub fn subtree_height(rows: &[CategoryFlat], id: &str) -> i64 {
    if category_by_id(rows, id).is_none() {
        return 0;
    }
    subtree_ids(rows, id)
        .iter()
        .filter_map(|member| category_depth(rows, member))
        .max()
        .map(|deepest| deepest - category_depth(rows, id).unwrap_or(1) + 1)
        .unwrap_or(1)
}

/// Would hanging `id` under `new_parent_id` create a cycle?
/// True when the new parent is the node itself or one of its
/// descendants (or when either id is unknown — an unknown
/// parent is not a valid move either, and callers surface
/// both through the same bilingual error).
pub fn would_create_cycle(rows: &[CategoryFlat], id: &str, new_parent_id: &str) -> bool {
    if category_by_id(rows, new_parent_id).is_none() {
        return true;
    }
    subtree_ids(rows, id).iter().any(|member| member == new_parent_id)
}

/// Validate a create-or-reparent placement and return the
/// depth the node would sit at.
///
/// - `node_id`: `None` for a create (a new leaf), `Some` for
///   a reparent of an existing subtree.
/// - `parent_id`: `None` places the node at the top level.
///
/// Rejects, bilingually: an unknown parent, a cycle (parent
/// is the node or inside its subtree), and any placement
/// whose subtree would reach past [`MAX_CATEGORY_DEPTH`].
pub fn validate_category_parent(
    rows: &[CategoryFlat],
    node_id: Option<&str>,
    parent_id: Option<&str>,
) -> Result<i64, String> {
    let parent_depth = match parent_id {
        None => 0,
        Some(parent) => {
            if let Some(node) = node_id {
                if would_create_cycle(rows, node, parent) {
                    return Err(
                        "a category cannot be moved under itself or one of its own \
                         sub-categories — একটি ক্যাটাগরিকে নিজের বা নিজের উপ-ক্যাটাগরির \
                         অধীনে সরানো যাবে না"
                            .to_string(),
                    );
                }
            }
            match category_depth(rows, parent) {
                Some(depth) => depth,
                None => {
                    return Err(
                        "parent category not found — মূল ক্যাটাগরিটি পাওয়া যায়নি"
                            .to_string(),
                    )
                }
            }
        }
    };
    let height = node_id.map(|node| subtree_height(rows, node)).unwrap_or(1);
    let deepest = parent_depth + height;
    if deepest > MAX_CATEGORY_DEPTH {
        return Err(format!(
            "categories can be at most {MAX_CATEGORY_DEPTH} levels deep — \
             ক্যাটাগরি সর্বোচ্চ {MAX_CATEGORY_DEPTH} স্তর গভীর হতে পারবে"
        ));
    }
    Ok(parent_depth + 1)
}

/// The display order of siblings: the admin's `sort_order`
/// first, then the English name, then the slug (a total
/// order, so tree building is deterministic).
pub fn category_display_order(a: &CategoryFlat, b: &CategoryFlat) -> std::cmp::Ordering {
    a.sort_order
        .cmp(&b.sort_order)
        .then_with(|| a.name_en.cmp(&b.name_en))
        .then_with(|| a.slug.cmp(&b.slug))
}

/// Build the nested tree from flat rows. Roots are rows with
/// no parent PLUS orphans whose parent id is missing from
/// the row set (a deleted or filtered-out parent must not
/// make a whole branch vanish — it surfaces at the top
/// instead; the admin tree passes the full row set, so
/// orphans only appear when a caller filters, e.g. the
/// public tree dropping an inactive parent keeps its active
/// children visible at the top rather than hiding them).
/// Siblings are ordered by [`category_display_order`] and
/// every node carries its true depth.
pub fn build_category_tree(rows: &[CategoryFlat]) -> Vec<CategoryNode> {
    let known: std::collections::HashSet<&str> =
        rows.iter().map(|row| row.id.as_str()).collect();
    let mut roots: Vec<&CategoryFlat> = rows
        .iter()
        .filter(|row| match row.parent_id.as_deref() {
            None => true,
            Some(parent) => !known.contains(parent),
        })
        .collect();
    roots.sort_by(|a, b| category_display_order(a, b));
    roots
        .into_iter()
        .map(|root| build_category_node(rows, root, 1))
        .collect()
}

fn build_category_node(rows: &[CategoryFlat], row: &CategoryFlat, depth: i64) -> CategoryNode {
    let mut children: Vec<&CategoryFlat> = rows
        .iter()
        .filter(|child| child.parent_id.as_deref() == Some(row.id.as_str()))
        .collect();
    children.sort_by(|a, b| category_display_order(a, b));
    CategoryNode {
        id: row.id.clone(),
        parent_id: row.parent_id.clone(),
        slug: row.slug.clone(),
        name_en: row.name_en.clone(),
        name_bn: row.name_bn.clone(),
        icon_key: row.icon_key.clone(),
        sort_order: row.sort_order,
        is_active: row.is_active,
        is_featured: row.is_featured,
        depth,
        children: children
            .into_iter()
            .map(|child| build_category_node(rows, child, depth + 1))
            .collect(),
    }
}

/// The breadcrumb path of a node: the chain from the top
/// level down to the node itself, in display order. `None`
/// when the id is unknown. A broken chain (missing parent)
/// yields the partial path from the highest reachable
/// ancestor — a breadcrumb with a gap is still honest.
pub fn category_path(rows: &[CategoryFlat], id: &str) -> Option<Vec<CategoryFlat>> {
    let mut chain = Vec::new();
    let mut current = category_by_id(rows, id)?;
    loop {
        chain.push(current.clone());
        match current.parent_id.as_deref() {
            Some(parent) => {
                if chain.iter().any(|row: &CategoryFlat| row.id == parent) {
                    break; // corrupt cycle: stop at the repeat
                }
                match category_by_id(rows, parent) {
                    Some(next) => current = next,
                    None => break,
                }
            }
            None => break,
        }
    }
    chain.reverse();
    Some(chain)
}

/// The delete guard, as one rule: a category with children
/// cannot be deleted (reparent or delete the children
/// first), and a category with products still linked cannot
/// be deleted (reassign the products first — the merge
/// endpoint is the bulk way). The errors are bilingual and
/// carry the counts so the admin sees exactly what blocks.
pub fn validate_category_delete(child_count: i64, product_count: i64) -> Result<(), String> {
    if child_count > 0 {
        return Err(format!(
            "this category still has {child_count} sub-categories — move or delete them first — \
             এই ক্যাটাগরিতে এখনো {child_count}টি উপ-ক্যাটাগরি আছে — আগে সেগুলো সরান বা মুছুন"
        ));
    }
    if product_count > 0 {
        return Err(format!(
            "this category still has {product_count} products linked — reassign them first — \
             এই ক্যাটাগরিতে এখনো {product_count}টি পণ্য যুক্ত আছে — আগে সেগুলো অন্য ক্যাটাগরিতে সরান"
        ));
    }
    Ok(())
}

/// Validate a merge of `source_id` into `target_id`: the two
/// must differ, both must exist, and the target must not sit
/// inside the source's subtree (the source's children move
/// under the target — a target inside the source would cycle
/// the tree). The merged subtree must still fit the depth
/// cap. Returns the depth the source's children would land
/// at under the target.
pub fn validate_category_merge(
    rows: &[CategoryFlat],
    source_id: &str,
    target_id: &str,
) -> Result<(), String> {
    if source_id == target_id {
        return Err(
            "a category cannot be merged into itself — \
             একটি ক্যাটাগরিকে নিজের মধ্যেই মার্জ করা যাবে না"
                .to_string(),
        );
    }
    category_by_id(rows, source_id).ok_or_else(|| {
        "source category not found — সোর্স ক্যাটাগরিটি পাওয়া যায়নি".to_string()
    })?;
    category_by_id(rows, target_id).ok_or_else(|| {
        "target category not found — টার্গেট ক্যাটাগরিটি পাওয়া যায়নি".to_string()
    })?;
    if subtree_ids(rows, source_id)
        .iter()
        .any(|member| member == target_id)
    {
        return Err(
            "a category cannot be merged into one of its own sub-categories — \
             একটি ক্যাটাগরিকে নিজের উপ-ক্যাটাগরির মধ্যে মার্জ করা যাবে না"
                .to_string(),
        );
    }
    // The source's children re-root under the target: the
    // deepest moved branch is the source's height minus the
    // source level itself.
    let moved_height = subtree_height(rows, source_id) - 1;
    let target_depth = category_depth(rows, target_id).unwrap_or(1);
    if target_depth + moved_height > MAX_CATEGORY_DEPTH {
        return Err(format!(
            "merging here would push categories past {MAX_CATEGORY_DEPTH} levels — \
             এখানে মার্জ করলে ক্যাটাগরি {MAX_CATEGORY_DEPTH} স্তরের বেশি গভীর হয়ে যাবে"
        ));
    }
    Ok(())
}

/// THE text→taxonomy resolution rule, shared by product
/// create/patch and mirrored by the Wave 56 backfill
/// migration: a seller-typed label links to a node only on
/// an EXACT match against an ACTIVE node, tried in this
/// order —
///
/// 1. the slug (the label lowercased: `fashion`),
/// 2. the English name, case-insensitively (`Fashion`),
/// 3. the Bangla name, exactly (`ফ্যাশন`).
///
/// The label itself is never rewritten by a link — it stays
/// the product's display name exactly as typed. No match
/// means no link (`None`): free-text products remain
/// possible, by design. When several nodes could match at
/// the same priority (an admin-created name clash), the
/// first in display order wins, deterministically.
pub fn resolve_category_link(text: &str, rows: &[CategoryFlat]) -> Option<String> {
    let label = text.trim();
    if label.is_empty() {
        return None;
    }
    let mut active: Vec<&CategoryFlat> = rows.iter().filter(|row| row.is_active).collect();
    active.sort_by(|a, b| category_display_order(a, b));
    let lowered = label.to_lowercase();
    for row in &active {
        if row.slug == lowered {
            return Some(row.id.clone());
        }
    }
    for row in &active {
        if row.name_en.to_lowercase() == lowered {
            return Some(row.id.clone());
        }
    }
    for row in &active {
        if row.name_bn.as_deref() == Some(label) {
            return Some(row.id.clone());
        }
    }
    None
}

/// Derive a slug candidate from a category name (the
/// taxonomy import's helper for rows that arrive without
/// one): lowercase, collapse every run of non-alphanumeric
/// characters to a single hyphen, trim hyphens. The result
/// may still fail [`validate_category_slug`] (too short, or
/// empty for a Bangla-only name) — the import reports those
/// rows instead of inventing a different name silently.
pub fn slugify_category_name(name: &str) -> String {
    let mut out = String::new();
    let mut last_was_hyphen = true; // also trims leading hyphens
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            out.push('-');
            last_was_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

// ---------------------------------------------------------------------------
// Wave 57: bulk catalogue CSV — parser, writer, row rules, batch math
// ---------------------------------------------------------------------------
//
// The seller bulk import speaks ONE format: a CSV sheet whose
// columns are [`BULK_IMPORT_COLUMNS`]. Everything about the
// format that can be decided without the database lives here,
// pure: the CSV grammar itself (a small dependency-free
// RFC-4180-style parser/writer), the header contract, the
// per-row field rules (which reuse the SAME product validators
// the single-create path uses, so a row can never smuggle in a
// product the form would refuse), and the batch price math for
// bulk edits. The API layer adds the database facts on top:
// which skus already exist, the plan cap, the taxonomy nodes a
// slug or label resolves to.

/// The most data rows one bulk import file may carry. A
/// seller with more splits the sheet — the cap keeps one
/// request's work (and its report) bounded, and 500 rows is
/// far beyond a hand-maintained catalogue page.
pub const MAX_BULK_IMPORT_ROWS: usize = 500;

/// The most products one bulk-edit request may name. Bulk
/// edits run per product through the audited single-product
/// paths, so the cap bounds the request's transaction count.
pub const MAX_BULK_EDIT_ITEMS: usize = 200;

/// The bulk sheet's columns, in the export's canonical
/// order. The import accepts any order (it maps by name);
/// the export and the template always write this order.
///
/// - `sku` — the seller's own stock-keeping id; the
///   idempotency key. Stored as the product's `source_ref`
///   with `source = 'bulk'` (the Wave 12 shape).
/// - `price_poisha` / `compare_at_poisha` — integer poisha,
///   like every money field in the API (৳1 = 100).
/// - `category_slug` — a Wave 56 taxonomy slug; wins over
///   `category_label` when both are present.
/// - `category_label` — a free-text label, linked on the
///   taxonomy's exact-match rule and stored exactly as
///   typed when it matches nothing.
/// - `image_url` — one external image URL, attached as a
///   hotlink through the Wave 51 media path.
pub const BULK_IMPORT_COLUMNS: &[&str] = &[
    "sku",
    "title",
    "title_bn",
    "description",
    "price_poisha",
    "compare_at_poisha",
    "stock",
    "weight_grams",
    "status",
    "category_slug",
    "category_label",
    "image_url",
];

/// The longest sku a bulk row may carry. The sku lands in
/// `products.source_ref` (a text column), so the cap is a
/// sanity bound, not a storage limit: real seller skus are
/// short codes, and a 121-character "sku" is a paste error.
pub const MAX_BULK_SKU_CHARS: usize = 120;

/// The longest image URL a bulk row may carry (the media
/// module's own URL validation runs again at attach time;
/// this is the row-level sanity bound).
pub const MAX_BULK_IMAGE_URL_CHARS: usize = 2_048;

// --- The CSV grammar -------------------------------------------------------

/// Parse CSV text into rows of fields. The grammar is the
/// common spreadsheet dialect: fields may be double-quoted,
/// a quoted field may contain commas, CR/LF newlines and
/// `""` as an escaped quote, rows end at CRLF, LF or a lone
/// CR, and a leading UTF-8 BOM is stripped (Excel writes
/// one). A trailing newline does NOT produce a phantom empty
/// row. The one hard error is an unterminated quoted field —
/// the file is malformed and no row boundary after it can be
/// trusted, so the caller refuses the whole file.
pub fn parse_csv(input: &str) -> Result<Vec<Vec<String>>, String> {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    // True once the current row has seen any content (a
    // character, a quote or a comma) — how the trailing
    // newline at end-of-file avoids a phantom row while a
    // genuinely empty line mid-file still parses as a
    // one-field row the caller may skip or reject.
    let mut row_started = false;
    let mut chars = input.chars().peekable();
    let mut line: usize = 1;
    let mut quote_start_line: usize = 1;
    #[derive(PartialEq)]
    enum State {
        FieldStart,
        InField,
        InQuotes,
        AfterQuote,
    }
    let mut state = State::FieldStart;
    while let Some(ch) = chars.next() {
        match state {
            State::FieldStart => match ch {
                '"' => {
                    state = State::InQuotes;
                    quote_start_line = line;
                    row_started = true;
                }
                ',' => {
                    row.push(std::mem::take(&mut field));
                    row_started = true;
                }
                '\r' | '\n' => {
                    if ch == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    line += 1;
                    if row_started || !row.is_empty() {
                        row.push(std::mem::take(&mut field));
                        rows.push(std::mem::take(&mut row));
                    } else if chars.peek().is_some() {
                        // A genuinely empty line mid-file:
                        // emit its one-field row so callers'
                        // line numbers stay sheet-true (the
                        // bulk parser skips it as blank). A
                        // trailing newline at end-of-file has
                        // nothing after it and emits no
                        // phantom row.
                        rows.push(vec![String::new()]);
                    }
                    row_started = false;
                }
                _ => {
                    field.push(ch);
                    state = State::InField;
                    row_started = true;
                }
            },
            State::InField => match ch {
                ',' => {
                    row.push(std::mem::take(&mut field));
                    state = State::FieldStart;
                }
                '\r' | '\n' => {
                    if ch == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    line += 1;
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    row_started = false;
                    state = State::FieldStart;
                }
                _ => field.push(ch),
            },
            State::InQuotes => match ch {
                '"' => state = State::AfterQuote,
                '\n' => {
                    line += 1;
                    field.push(ch);
                }
                _ => field.push(ch),
            },
            State::AfterQuote => match ch {
                '"' => {
                    field.push('"');
                    state = State::InQuotes;
                }
                ',' => {
                    row.push(std::mem::take(&mut field));
                    state = State::FieldStart;
                }
                '\r' | '\n' => {
                    if ch == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    line += 1;
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    row_started = false;
                    state = State::FieldStart;
                }
                // A stray character right after a closing
                // quote (spreadsheets occasionally emit
                // `"abc" `) — kept as literal field content
                // rather than failing the whole file.
                _ => {
                    field.push(ch);
                    state = State::InField;
                }
            },
        }
    }
    if state == State::InQuotes {
        return Err(format!(
            "unterminated quoted field starting at line {quote_start_line} — \
             উদ্ধৃতি চিহ্ন বন্ধ হয়নি (লাইন {quote_start_line})"
        ));
    }
    if row_started || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

/// Escape one field for CSV output: quoted exactly when it
/// contains a comma, a quote or a newline (the minimal
/// quoting spreadsheets expect), inner quotes doubled.
pub fn csv_escape_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r')
    {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Serialise rows to CSV text: CRLF row endings (the
/// spreadsheet-native form), every row terminated.
pub fn write_csv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for row in rows {
        let cells: Vec<String> = row.iter().map(|f| csv_escape_field(f)).collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    }
    out
}

// --- Bulk row mapping ------------------------------------------------------

/// One field-level problem with a bulk row. `message` is
/// bilingual like every seller-facing string in the API.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BulkFieldError {
    pub field: String,
    pub message: String,
}

/// One validated bulk row, ready for the service layer to
/// match against the store's existing skus.
#[derive(Debug, Clone, PartialEq)]
pub struct BulkProductRow {
    /// Spreadsheet row number: the header is row 1, so the
    /// first data row is row 2 — the number the seller sees
    /// in their sheet when the report names a problem.
    pub row_number: usize,
    pub sku: String,
    pub title: String,
    pub title_bn: Option<String>,
    pub description: Option<String>,
    pub price_poisha: i64,
    pub compare_at_poisha: Option<i64>,
    pub stock: i32,
    pub weight_grams: i64,
    pub status: String,
    pub category_slug: Option<String>,
    pub category_label: Option<String>,
    pub image_url: Option<String>,
}

/// Every problem with one row, keyed by its spreadsheet row
/// number. The sku rides along when the row had one (the
/// report's rows are findable by either).
#[derive(Debug, Clone, PartialEq)]
pub struct BulkRowErrors {
    pub row_number: usize,
    pub sku: Option<String>,
    pub errors: Vec<BulkFieldError>,
}

/// The outcome of parsing a bulk sheet: the rows that
/// validated, and the rows that did not (with every field
/// problem found, not just the first — a seller fixing a
/// sheet should not need one round-trip per mistake).
#[derive(Debug, Clone, PartialEq)]
pub struct BulkCsvParse {
    pub rows: Vec<BulkProductRow>,
    pub row_errors: Vec<BulkRowErrors>,
}

fn bulk_field_error(field: &str, message: impl Into<String>) -> BulkFieldError {
    BulkFieldError {
        field: field.to_string(),
        message: message.into(),
    }
}

/// Parse one integer cell. Empty (after trimming) yields
/// `None` from the caller's perspective via the `raw`
/// check; this helper only judges non-empty text. Whole
/// numbers only — a decimal point in a poisha cell is a
/// taka/poisha mix-up and must be an error, not a rounding.
fn parse_bulk_i64(raw: &str, field: &str) -> Result<i64, BulkFieldError> {
    raw.parse::<i64>().map_err(|_| {
        bulk_field_error(
            field,
            format!(
                "'{raw}' is not a whole number — '{raw}' পূর্ণসংখ্যা নয় \
                 (poisha amounts are whole numbers)"
            ),
        )
    })
}

/// Parse and validate a whole bulk sheet. A `Err` is a
/// FATAL problem with the file itself (unparseable CSV, a
/// missing or unknown header column, too many rows) — the
/// service refuses the request with it. Per-row problems
/// come back inside the outcome and never abort the batch.
///
/// Header contract: the first row names the columns
/// (trimmed, case-insensitive). `sku`, `title` and
/// `price_poisha` are required; unknown or duplicated
/// columns are fatal (a mistyped column name must not
/// silently drop a seller's data). Blank rows (every cell
/// empty) are skipped without a murmur — spreadsheets emit
/// them freely.
pub fn parse_bulk_product_csv(text: &str) -> Result<BulkCsvParse, String> {
    let grid = parse_csv(text)?;
    let Some((header, data_rows)) = grid.split_first() else {
        return Err(
            "the file is empty — a header row is required — ফাইলটি খালি, হেডার সারি আবশ্যক"
                .to_string(),
        );
    };
    let columns: Vec<String> = header
        .iter()
        .map(|cell| cell.trim().to_lowercase())
        .collect();
    for name in &columns {
        if !BULK_IMPORT_COLUMNS.contains(&name.as_str()) {
            return Err(format!(
                "unknown column '{name}' — allowed columns: {} — অজানা কলাম '{name}'",
                BULK_IMPORT_COLUMNS.join(", ")
            ));
        }
    }
    for required in ["sku", "title", "price_poisha"] {
        if !columns.iter().any(|name| name == required) {
            return Err(format!(
                "the header is missing the required column '{required}' — \
                 হেডারে আবশ্যক কলাম '{required}' নেই"
            ));
        }
    }
    let mut seen_columns: Vec<&str> = Vec::new();
    for name in &columns {
        if seen_columns.contains(&name.as_str()) {
            return Err(format!(
                "the column '{name}' appears twice in the header — \
                 হেডারে '{name}' কলামটি দুইবার আছে"
            ));
        }
        seen_columns.push(name.as_str());
    }
    let column_index = |name: &str| columns.iter().position(|c| c == name);

    let data_count = data_rows
        .iter()
        .filter(|cells| cells.iter().any(|cell| !cell.trim().is_empty()))
        .count();
    if data_count > MAX_BULK_IMPORT_ROWS {
        return Err(format!(
            "the file carries {data_count} data rows — at most {MAX_BULK_IMPORT_ROWS} per import; \
             split the sheet — ফাইলে {data_count}টি সারি আছে, একবারে সর্বোচ্চ {MAX_BULK_IMPORT_ROWS}টি"
        ));
    }

    let mut rows: Vec<BulkProductRow> = Vec::new();
    let mut row_errors: Vec<BulkRowErrors> = Vec::new();
    let mut seen_skus: Vec<String> = Vec::new();

    for (offset, cells) in data_rows.iter().enumerate() {
        // +2: the header is spreadsheet row 1 and `offset`
        // is 0-based over the data rows.
        let row_number = offset + 2;
        if cells.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        let mut errors: Vec<BulkFieldError> = Vec::new();
        if cells.len() != columns.len() {
            errors.push(bulk_field_error(
                "row",
                format!(
                    "expected {} columns, found {} — {}টি কলাম থাকা উচিত ছিল, পাওয়া গেছে {}টি",
                    columns.len(),
                    cells.len(),
                    columns.len(),
                    cells.len()
                ),
            ));
            row_errors.push(BulkRowErrors {
                row_number,
                sku: None,
                errors,
            });
            continue;
        }
        let get = |name: &str| -> &str {
            column_index(name)
                .map(|idx| cells[idx].trim())
                .unwrap_or("")
        };
        let opt = |name: &str| -> Option<String> {
            let value = get(name);
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        };

        // sku — the idempotency key, required and unique
        // within the file (the first occurrence wins; a
        // later duplicate is the row's error).
        let sku = get("sku").to_string();
        if sku.is_empty() {
            errors.push(bulk_field_error(
                "sku",
                "sku is required — SKU আবশ্যক (এটি পণ্যের নিজস্ব কোড)",
            ));
        } else if sku.chars().count() > MAX_BULK_SKU_CHARS {
            errors.push(bulk_field_error(
                "sku",
                format!(
                    "sku must be at most {MAX_BULK_SKU_CHARS} characters — \
                     SKU সর্বোচ্চ {MAX_BULK_SKU_CHARS} অক্ষরের হতে পারবে"
                ),
            ));
        } else if sku.chars().any(|c| c.is_control()) {
            errors.push(bulk_field_error(
                "sku",
                "sku must not contain control characters — SKU-তে নিয়ন্ত্রণ অক্ষর থাকতে পারবে না",
            ));
        } else if seen_skus.contains(&sku) {
            errors.push(bulk_field_error(
                "sku",
                format!(
                    "sku '{sku}' appears more than once in this file — \
                     এই ফাইলে '{sku}' SKU একাধিকবার আছে"
                ),
            ));
        } else {
            seen_skus.push(sku.clone());
        }

        let title = get("title").to_string();

        // price — required, a whole number of poisha.
        let price_raw = get("price_poisha");
        let price_poisha = if price_raw.is_empty() {
            errors.push(bulk_field_error(
                "price_poisha",
                "price_poisha is required — মূল্য (পয়সায়) আবশ্যক",
            ));
            0
        } else {
            match parse_bulk_i64(price_raw, "price_poisha") {
                Ok(value) => value,
                Err(error) => {
                    errors.push(error);
                    0
                }
            }
        };

        let compare_at_poisha = match opt("compare_at_poisha") {
            None => None,
            Some(raw) => match parse_bulk_i64(&raw, "compare_at_poisha") {
                Ok(value) if value >= 0 => Some(value),
                Ok(_) => {
                    errors.push(bulk_field_error(
                        "compare_at_poisha",
                        "compare_at_poisha must be >= 0 — তুলনামূলক মূল্য ঋণাত্মক হতে পারবে না",
                    ));
                    None
                }
                Err(error) => {
                    errors.push(error);
                    None
                }
            },
        };

        let stock = match opt("stock") {
            None => 0,
            Some(raw) => match parse_bulk_i64(&raw, "stock") {
                Ok(value) => match i32::try_from(value) {
                    Ok(stock) => stock,
                    Err(_) => {
                        errors.push(bulk_field_error(
                            "stock",
                            "stock is out of range — স্টকের পরিমাণ সীমার বাইরে",
                        ));
                        0
                    }
                },
                Err(error) => {
                    errors.push(error);
                    0
                }
            },
        };

        let weight_grams = match opt("weight_grams") {
            None => DEFAULT_WEIGHT_GRAMS,
            Some(raw) => match parse_bulk_i64(&raw, "weight_grams") {
                Ok(value) => value,
                Err(error) => {
                    errors.push(error);
                    DEFAULT_WEIGHT_GRAMS
                }
            },
        };

        let status = opt("status").unwrap_or_else(|| "draft".to_string());

        // Category cells: the slug must at least be
        // slug-shaped (existence is the service's fact);
        // the label runs the shared category validator.
        let category_slug = match opt("category_slug") {
            None => None,
            Some(raw) => match validate_category_slug(&raw) {
                Ok(slug) => Some(slug),
                Err(message) => {
                    errors.push(bulk_field_error("category_slug", message));
                    None
                }
            },
        };
        let category_label = opt("category_label");
        if let Some(label) = &category_label {
            if let Err(message) = validate_category(Some(label)) {
                errors.push(bulk_field_error("category_label", message));
            }
        }

        let image_url = opt("image_url");
        if let Some(url) = &image_url {
            if url.chars().count() > MAX_BULK_IMAGE_URL_CHARS {
                errors.push(bulk_field_error(
                    "image_url",
                    format!(
                        "image_url must be at most {MAX_BULK_IMAGE_URL_CHARS} characters — \
                         ছবির লিংক সর্বোচ্চ {MAX_BULK_IMAGE_URL_CHARS} অক্ষরের হতে পারবে"
                    ),
                ));
            }
        }

        // The shared product validator — the same gate the
        // single-create form passes — judges the mapped
        // row as a whole, in its accumulating form: every
        // failing field is reported, not just the first.
        let candidate = ProductInput {
            title: title.clone(),
            price_poisha,
            stock,
            status: status.clone(),
            weight_grams,
        };
        for (field, message) in product_input_errors(&candidate) {
            errors.push(bulk_field_error(field, message));
        }
        // The import-only price rule (the Facebook import's
        // precedent): a product going on sale needs a real
        // price — a zero price on an ACTIVE row means the
        // sheet did not name one the seller has confirmed.
        if status == "active"
            && price_poisha == 0
            && !errors.iter().any(|e| e.field == "price_poisha")
        {
            errors.push(bulk_field_error(
                "price_poisha",
                "an active product needs a price greater than zero — \
                     সক্রিয় পণ্যের মূল্য শূন্যের বেশি হতে হবে",
            ));
        }

        if errors.is_empty() {
            rows.push(BulkProductRow {
                row_number,
                sku,
                title,
                title_bn: opt("title_bn"),
                description: opt("description"),
                price_poisha,
                compare_at_poisha,
                stock,
                weight_grams,
                status,
                category_slug,
                category_label,
                image_url,
            });
        } else {
            row_errors.push(BulkRowErrors {
                row_number,
                sku: if sku.is_empty() { None } else { Some(sku) },
                errors,
            });
        }
    }

    Ok(BulkCsvParse { rows, row_errors })
}

/// The downloadable example sheet: the canonical header
/// plus two fully-worked example rows (a live panjabi and
/// a draft grocery staple), generated from
/// [`BULK_IMPORT_COLUMNS`] so the template can never drift
/// from the parser's contract.
pub fn bulk_template_csv() -> String {
    let header: Vec<String> = BULK_IMPORT_COLUMNS
        .iter()
        .map(|name| name.to_string())
        .collect();
    let example_one: Vec<String> = [
        "PANJABI-001",
        "Premium Cotton Panjabi",
        "প্রিমিয়াম সুতি পাঞ্জাবি",
        "Soft combed cotton, festive fit",
        "185000",
        "220000",
        "25",
        "400",
        "active",
        "panjabi",
        "",
        "",
    ]
    .iter()
    .map(|cell| cell.to_string())
    .collect();
    let example_two: Vec<String> = [
        "RICE-NAZIR-5KG",
        "Nazirshail Rice 5kg",
        "নাজিরশাইল চাল ৫ কেজি",
        "Aromatic fine rice, Dinajpur",
        "62000",
        "",
        "120",
        "5000",
        "draft",
        "rice-grains",
        "",
        "",
    ]
    .iter()
    .map(|cell| cell.to_string())
    .collect();
    write_csv(&[header, example_one, example_two])
}

// --- Bulk price math -------------------------------------------------------

/// The valid range for a bulk-edit percent: a cut may not
/// exceed 99% (the price must stay real), and an increase
/// beyond +500% is treated as a typo, not an intention.
pub const BULK_PERCENT_MIN: i64 = -99;
pub const BULK_PERCENT_MAX: i64 = 500;

/// Validate a bulk-edit percent (whole percent points).
pub fn validate_bulk_percent(percent: i64) -> Result<i64, &'static str> {
    if (BULK_PERCENT_MIN..=BULK_PERCENT_MAX).contains(&percent) {
        Ok(percent)
    } else {
        Err("percent must be between -99 and 500")
    }
}

/// A price after a percent change, in poisha: computed in
/// i128 so no catalogue price can overflow, rounded half
/// up to the nearest poisha, and floored at 1 poisha — a
/// bulk percent cut may make a product cheap, never free
/// (a free product is a promotion decision, not a bulk
/// edit). `None` for an out-of-range percent or a
/// non-positive starting price.
pub fn bulk_price_after_percent(price_poisha: i64, percent: i64) -> Option<i64> {
    if price_poisha < 1 || validate_bulk_percent(percent).is_err() {
        return None;
    }
    let scaled = i128::from(price_poisha) * i128::from(100 + percent);
    let rounded = (scaled + 50) / 100;
    let result = i64::try_from(rounded).ok()?;
    Some(result.max(1))
}

/// A price after a fixed poisha delta, checked: the
/// result must stay at or above 1 poisha (same floor as
/// the percent rule) and inside i64. `None` otherwise.
pub fn bulk_price_after_delta(price_poisha: i64, delta_poisha: i64) -> Option<i64> {
    let result = price_poisha.checked_add(delta_poisha)?;
    if result < 1 {
        None
    } else {
        Some(result)
    }
}

/// A bulk stock SET target: the products table stores
/// stock as i32 and the domain validator already requires
/// non-negative, so the target must fit both.
pub fn validate_bulk_stock_target(stock: i64) -> Result<i32, &'static str> {
    if stock < 0 {
        return Err("stock must be >= 0");
    }
    i32::try_from(stock).map_err(|_| "stock is out of range")
}

// ---------------------------------------------------------------------------
// Wave 58 — storefront section layouts (the section editor)
// ---------------------------------------------------------------------------

/// The layout document version this build writes. The
/// reader is forward-tolerant (unknown fields ignored,
/// unknown section types preserved — see
/// [`validate_storefront_layout`]), but a WRITE must name
/// the current version: silently accepting a future
/// document and re-serialising it with today's types could
/// drop fields this build does not know it dropped.
pub const STOREFRONT_LAYOUT_VERSION: i64 = 1;

/// The most sections one storefront layout may hold. The
/// editor composes a page, not a directory — twenty blocks
/// is already a very long storefront, and the cap keeps the
/// stored document (and its resolution) bounded.
pub const MAX_STOREFRONT_SECTIONS: usize = 20;

/// Section type: the storefront's crown — headline,
/// subtext, image and an optional call-to-action link.
/// Every field is optional; an unset field falls back to
/// the store's branding at resolution time (the Wave 18
/// banner / announcement / name), so an empty hero is
/// exactly today's storefront header art.
pub const SECTION_TYPE_HERO: &str = "hero";
/// Section type: a heading plus a free-text body — the
/// seller's own words (a story, a delivery note, a
/// campaign explanation).
pub const SECTION_TYPE_RICH_TEXT: &str = "rich_text";
/// Section type: a horizontal rail of product cards fed
/// from one source (newest / best sellers / on sale / a
/// hand-picked list).
pub const SECTION_TYPE_PRODUCT_RAIL: &str = "product_rail";
/// Section type: the store's own collections (Wave 54) —
/// tiles for every shelf, or one named shelf's products.
pub const SECTION_TYPE_COLLECTION_SHELF: &str = "collection_shelf";
/// Section type: the categories this store sells in, as
/// tiles with counts — the taxonomy specialisations
/// (Wave 56) or the free-text labels (Wave 54).
pub const SECTION_TYPE_CATEGORY_GRID: &str = "category_grid";
/// Section type: the store's full catalogue grid — the
/// one section every storefront keeps, movable like the
/// rest.
pub const SECTION_TYPE_PRODUCT_GRID: &str = "product_grid";
/// Section type: one wide image with an optional link
/// and caption — a campaign banner of the seller's own.
pub const SECTION_TYPE_IMAGE_BANNER: &str = "image_banner";
/// Section type: the store's long profile blocks (Wave
/// 54): the about text and the return policy, with the
/// store's trust numbers riding the resolved payload.
pub const SECTION_TYPE_STORE_INFO: &str = "store_info";
/// Section type: question/answer pairs — delivery,
/// payment and returns answers a buyer reads before
/// asking.
pub const SECTION_TYPE_FAQ: &str = "faq";

/// Every section type this build knows, in the order the
/// editor's add-section picker lists them.
pub const STOREFRONT_SECTION_TYPES: [&str; 9] = [
    SECTION_TYPE_HERO,
    SECTION_TYPE_RICH_TEXT,
    SECTION_TYPE_PRODUCT_RAIL,
    SECTION_TYPE_COLLECTION_SHELF,
    SECTION_TYPE_CATEGORY_GRID,
    SECTION_TYPE_PRODUCT_GRID,
    SECTION_TYPE_IMAGE_BANNER,
    SECTION_TYPE_STORE_INFO,
    SECTION_TYPE_FAQ,
];

/// Product-rail source: the store's newest products.
pub const RAIL_SOURCE_NEWEST: &str = "newest";
/// Product-rail source: the store's best sellers (real
/// delivered units only — the platform's one "sold").
pub const RAIL_SOURCE_BEST_SELLERS: &str = "best_sellers";
/// Product-rail source: products on sale (a live flash
/// price, or a compare-at above the price — the search
/// module's one on-sale predicate).
pub const RAIL_SOURCE_ON_SALE: &str = "on_sale";
/// Product-rail source: the seller's hand-picked product
/// ids, in the seller's order.
pub const RAIL_SOURCE_MANUAL: &str = "manual";

/// Every product-rail source.
pub const RAIL_SOURCES: [&str; 4] = [
    RAIL_SOURCE_NEWEST,
    RAIL_SOURCE_BEST_SELLERS,
    RAIL_SOURCE_ON_SALE,
    RAIL_SOURCE_MANUAL,
];

/// Category-grid mode: the managed taxonomy (Wave 56).
pub const CATEGORY_GRID_MODE_TAXONOMY: &str = "taxonomy";
/// Category-grid mode: the free-text category labels
/// (Wave 54).
pub const CATEGORY_GRID_MODE_LABELS: &str = "labels";

/// Every category-grid mode.
pub const CATEGORY_GRID_MODES: [&str; 2] = [
    CATEGORY_GRID_MODE_TAXONOMY,
    CATEGORY_GRID_MODE_LABELS,
];

/// Longest hero headline, in Unicode characters.
pub const MAX_SECTION_HEADLINE_CHARS: usize = 120;
/// Longest hero subtext, in Unicode characters.
pub const MAX_SECTION_SUBTEXT_CHARS: usize = 300;
/// Longest rich-text heading, in Unicode characters.
pub const MAX_SECTION_HEADING_CHARS: usize = 120;
/// Longest rich-text body, in Unicode characters (the
/// store about text's cap — the same reading length).
pub const MAX_SECTION_BODY_CHARS: usize = 2_000;
/// Longest section title (rails, shelves, grids), in
/// Unicode characters.
pub const MAX_SECTION_TITLE_CHARS: usize = 80;
/// Longest call-to-action link label, in Unicode
/// characters.
pub const MAX_SECTION_LINK_LABEL_CHARS: usize = 60;
/// Longest image-banner caption, in Unicode characters.
pub const MAX_SECTION_CAPTION_CHARS: usize = 120;
/// The fewest products a rail / shelf / grid may show.
pub const MIN_SECTION_LIMIT: i64 = 4;
/// The most products a rail may show.
pub const MAX_RAIL_LIMIT: i64 = 24;
/// The most products a single-collection shelf may show.
pub const MAX_SHELF_LIMIT: i64 = 24;
/// The most tiles a category grid may show.
pub const MAX_CATEGORY_GRID_LIMIT: i64 = 24;
/// A rail's product count when the seller does not say.
pub const DEFAULT_RAIL_LIMIT: i64 = 10;
/// A single-collection shelf's product count default.
pub const DEFAULT_SHELF_LIMIT: i64 = 8;
/// A category grid's tile count default.
pub const DEFAULT_CATEGORY_GRID_LIMIT: i64 = 12;
/// The most hand-picked products in one manual rail.
pub const MAX_MANUAL_RAIL_PRODUCTS: usize = 24;
/// The most question/answer pairs in one FAQ section.
pub const MAX_FAQ_PAIRS: usize = 10;
/// Longest FAQ question, in Unicode characters.
pub const MAX_FAQ_QUESTION_CHARS: usize = 200;
/// Longest FAQ answer, in Unicode characters.
pub const MAX_FAQ_ANSWER_CHARS: usize = 1_000;
/// Longest section id, in characters (ids are the
/// seller-facing handle the analytics events key on).
pub const MAX_SECTION_ID_CHARS: usize = 40;

/// One question/answer pair of an FAQ section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaqItem {
    pub question: String,
    pub answer: String,
}

/// One section of a storefront layout. `config` is the
/// type's own JSON object — validated and normalised by
/// [`validate_storefront_layout`] for the types this
/// build knows, and carried verbatim for types it does
/// not (the forward-tolerance rule). `starts_at` /
/// `ends_at` are unix seconds (the domain crate's time
/// convention); the API translates to and from RFC 3339
/// at its boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorefrontSection {
    pub id: String,
    #[serde(rename = "type")]
    pub section_type: String,
    #[serde(default = "default_section_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<i64>,
}

fn default_section_enabled() -> bool {
    true
}

/// A store's whole storefront layout: an ordered list of
/// sections under a document version. Stored as JSONB
/// (migration 0065); a store with no stored layout uses
/// [`default_storefront_layout`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorefrontLayout {
    #[serde(default = "default_layout_version")]
    pub version: i64,
    #[serde(default)]
    pub sections: Vec<StorefrontSection>,
}

fn default_layout_version() -> i64 {
    STOREFRONT_LAYOUT_VERSION
}

/// Is this one of the section types this build knows?
pub fn is_known_section_type(section_type: &str) -> bool {
    STOREFRONT_SECTION_TYPES.contains(&section_type)
}

/// A section id: 1–40 characters of lowercase letters,
/// digits and hyphens, not starting or ending with a
/// hyphen (the slug shape, minus the length floor — a
/// one-letter id is a fine handle). Ids key the section
/// analytics, so they must stay URL- and JSON-safe.
pub fn validate_section_id(id: &str) -> Result<(), &'static str> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_SECTION_ID_CHARS {
        return Err("section id must be 1-40 characters");
    }
    if trimmed.starts_with('-') || trimmed.ends_with('-') {
        return Err("section id must not start or end with a hyphen");
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err("section id must be lowercase letters, digits and hyphens");
    }
    Ok(())
}

/// Is a section type string at least well-formed (the
/// catalogue's charset: lowercase letters, digits,
/// underscores and hyphens)? A well-formed type this
/// build does not know is a FUTURE type — tolerated and
/// preserved. A malformed one is garbage and refused.
fn is_well_formed_section_type(section_type: &str) -> bool {
    !section_type.is_empty()
        && section_type.chars().count() <= MAX_SECTION_ID_CHARS
        && section_type
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Read an optional string out of a section config: a
/// missing key or an explicit null is `None`; a present
/// non-string is an error (a number where text belongs is
/// a broken document, not a default).
fn section_config_str(
    config: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match config.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(raw)) => Ok(Some(raw.clone())),
        Some(_) => Err(format!("config key '{key}' must be a string")),
    }
}

/// Read an optional integer out of a section config.
fn section_config_i64(
    config: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<i64>, String> {
    match config.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("config key '{key}' must be a whole number")),
    }
}

/// Read an optional string list out of a section config.
fn section_config_str_list(
    config: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Vec<String>, String> {
    match config.get(key) {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(raw) => out.push(raw.to_string()),
                    None => {
                        return Err(format!(
                            "config key '{key}' must be a list of strings"
                        ))
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(format!("config key '{key}' must be a list of strings")),
    }
}

/// Validate + normalise one optional config text: trim,
/// blank becomes absent, over the cap is an error naming
/// the key. `label` is the human name used in messages.
fn section_config_text(
    config: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    label: &str,
    max_chars: usize,
) -> Result<Option<String>, String> {
    let Some(raw) = section_config_str(config, key)? else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > max_chars {
        return Err(format!(
            "{label} must be at most {max_chars} characters — \
             {label} সর্বোচ্চ {max_chars} অক্ষরের হতে পারবে"
        ));
    }
    Ok(Some(trimmed.to_string()))
}

/// Validate + normalise one optional config URL through
/// the branding rule (Wave 18: http(s) or site-relative,
/// blank clears): an image a storefront renders straight
/// into an `<img>` gets exactly the branding defence.
fn section_config_image_url(
    config: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    label: &str,
) -> Result<Option<String>, String> {
    let Some(raw) = section_config_str(config, key)? else {
        return Ok(None);
    };
    validate_branding_url(Some(&raw))
        .map_err(|message| format!("{label}: {message}"))
}

/// Validate + normalise one optional config link through
/// the banner link rule (Wave 52: a site path with a
/// single leading `/`, or an absolute http(s) URL — the
/// scheme whitelist is the whole defence for a value
/// rendered straight into an anchor).
fn section_config_link_url(
    config: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    let Some(raw) = section_config_str(config, key)? else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    validate_banner_link(&raw).map(Some).map_err(str::to_string)
}

/// Validate + normalise a rail/shelf/grid limit: absent
/// takes the type's default; present must sit inside
/// [MIN_SECTION_LIMIT, max].
fn section_config_limit(
    config: &serde_json::Map<String, serde_json::Value>,
    default: i64,
    max: i64,
) -> Result<i64, String> {
    let Some(value) = section_config_i64(config, "limit")? else {
        return Ok(default);
    };
    if !(MIN_SECTION_LIMIT..=max).contains(&value) {
        return Err(format!(
            "limit must be between {MIN_SECTION_LIMIT} and {max} — \
             limit {MIN_SECTION_LIMIT} থেকে {max}-এর মধ্যে হতে হবে"
        ));
    }
    Ok(value)
}

/// Is this string shaped like a uuid (8-4-4-4-12 hex)?
/// The domain has no uuid crate; the API re-parses every
/// id it resolves, so this shape check only keeps obvious
/// garbage out of a stored document.
fn is_uuid_shaped(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    })
}

/// Build one normalised config value from the keys a
/// section type actually carries — the stored document
/// keeps only its type's own keys, in a stable shape.
fn normalised_config(
    pairs: Vec<(&str, serde_json::Value)>,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in pairs {
        if !value.is_null() {
            map.insert(key.to_string(), value);
        }
    }
    serde_json::Value::Object(map)
}

fn json_text(value: Option<String>) -> serde_json::Value {
    value.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null)
}

/// Validate + normalise ONE known section type's config.
/// The returned value replaces the submitted config in
/// the stored document. Unknown types never reach here
/// (they are preserved verbatim by the caller).
fn validate_section_config(
    section_type: &str,
    config: &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, String> {
    match section_type {
        SECTION_TYPE_HERO => Ok(normalised_config(vec![
            (
                "headline",
                json_text(section_config_text(
                    config,
                    "headline",
                    "headline",
                    MAX_SECTION_HEADLINE_CHARS,
                )?),
            ),
            (
                "subtext",
                json_text(section_config_text(
                    config,
                    "subtext",
                    "subtext",
                    MAX_SECTION_SUBTEXT_CHARS,
                )?),
            ),
            (
                "image_url",
                json_text(section_config_image_url(config, "image_url", "hero image")?),
            ),
            (
                "link_url",
                json_text(section_config_link_url(config, "link_url")?),
            ),
            (
                "link_label",
                json_text(section_config_text(
                    config,
                    "link_label",
                    "link label",
                    MAX_SECTION_LINK_LABEL_CHARS,
                )?),
            ),
        ])),
        SECTION_TYPE_RICH_TEXT => {
            let heading = section_config_text(
                config,
                "heading",
                "heading",
                MAX_SECTION_HEADING_CHARS,
            )?;
            let body = section_config_text(config, "body", "body", MAX_SECTION_BODY_CHARS)?
                .ok_or_else(|| {
                    "a rich text section needs a body — \
                     রিচ টেক্সট সেকশনে লেখা আবশ্যক"
                        .to_string()
                })?;
            Ok(normalised_config(vec![
                ("heading", json_text(heading)),
                ("body", serde_json::Value::String(body)),
            ]))
        }
        SECTION_TYPE_PRODUCT_RAIL => {
            let source = section_config_str(config, "source")?
                .map(|raw| raw.trim().to_string())
                .filter(|raw| !raw.is_empty())
                .ok_or_else(|| {
                    "a product rail needs a source — \
                     প্রোডাক্ট রেলের উৎস (source) আবশ্যক"
                        .to_string()
                })?;
            if !RAIL_SOURCES.contains(&source.as_str()) {
                return Err(format!(
                    "rail source must be one of: {} — \
                     রেলের উৎস হতে হবে: {}",
                    RAIL_SOURCES.join(", "),
                    RAIL_SOURCES.join(", ")
                ));
            }
            let title = section_config_text(config, "title", "title", MAX_SECTION_TITLE_CHARS)?;
            let limit = section_config_limit(config, DEFAULT_RAIL_LIMIT, MAX_RAIL_LIMIT)?;
            let mut product_ids = section_config_str_list(config, "product_ids")?;
            // Duplicates collapse, first occurrence keeps
            // its place (the collection membership rule).
            let mut deduped: Vec<String> = Vec::with_capacity(product_ids.len());
            for id in product_ids.drain(..) {
                let id = id.trim().to_string();
                if !deduped.contains(&id) {
                    deduped.push(id);
                }
            }
            if source == RAIL_SOURCE_MANUAL {
                if deduped.is_empty() {
                    return Err(
                        "a manual rail needs at least one product — \
                         হাতে-বাছাই রেলে অন্তত একটি পণ্য আবশ্যক"
                            .to_string(),
                    );
                }
                if deduped.len() > MAX_MANUAL_RAIL_PRODUCTS {
                    return Err(format!(
                        "a manual rail holds at most {MAX_MANUAL_RAIL_PRODUCTS} products — \
                         হাতে-বাছাই রেলে সর্বোচ্চ {MAX_MANUAL_RAIL_PRODUCTS}টি পণ্য রাখা যাবে"
                    ));
                }
                for id in &deduped {
                    if !is_uuid_shaped(id) {
                        return Err(format!(
                            "product id '{id}' is not a valid id — \
                             '{id}' সঠিক পণ্য আইডি নয়"
                        ));
                    }
                }
            } else if !deduped.is_empty() {
                return Err(
                    "product_ids only applies to a manual rail — \
                     product_ids শুধু হাতে-বাছাই রেলের জন্য"
                        .to_string(),
                );
            }
            let mut pairs = vec![
                ("source", serde_json::Value::String(source.clone())),
                ("title", json_text(title)),
                ("limit", serde_json::Value::Number(limit.into())),
            ];
            if source == RAIL_SOURCE_MANUAL {
                pairs.push((
                    "product_ids",
                    serde_json::Value::Array(
                        deduped.into_iter().map(serde_json::Value::String).collect(),
                    ),
                ));
            }
            Ok(normalised_config(pairs))
        }
        SECTION_TYPE_COLLECTION_SHELF => {
            let collection_id = match section_config_str(config, "collection_id")? {
                Some(raw) => {
                    let raw = raw.trim().to_string();
                    if raw.is_empty() {
                        None
                    } else if is_uuid_shaped(&raw) {
                        Some(raw)
                    } else {
                        return Err(format!(
                            "collection id '{raw}' is not a valid id — \
                             '{raw}' সঠিক কালেকশন আইডি নয়"
                        ));
                    }
                }
                None => None,
            };
            let title = section_config_text(config, "title", "title", MAX_SECTION_TITLE_CHARS)?;
            let limit = section_config_limit(config, DEFAULT_SHELF_LIMIT, MAX_SHELF_LIMIT)?;
            Ok(normalised_config(vec![
                ("collection_id", json_text(collection_id)),
                ("title", json_text(title)),
                ("limit", serde_json::Value::Number(limit.into())),
            ]))
        }
        SECTION_TYPE_CATEGORY_GRID => {
            let mode = section_config_str(config, "mode")?
                .map(|raw| raw.trim().to_string())
                .filter(|raw| !raw.is_empty())
                .unwrap_or_else(|| CATEGORY_GRID_MODE_TAXONOMY.to_string());
            if mode != CATEGORY_GRID_MODE_TAXONOMY && mode != CATEGORY_GRID_MODE_LABELS {
                return Err(format!(
                    "category grid mode must be '{CATEGORY_GRID_MODE_TAXONOMY}' or \
                     '{CATEGORY_GRID_MODE_LABELS}' — \
                     মোড হতে হবে '{CATEGORY_GRID_MODE_TAXONOMY}' অথবা '{CATEGORY_GRID_MODE_LABELS}'"
                ));
            }
            let title = section_config_text(config, "title", "title", MAX_SECTION_TITLE_CHARS)?;
            let limit = section_config_limit(
                config,
                DEFAULT_CATEGORY_GRID_LIMIT,
                MAX_CATEGORY_GRID_LIMIT,
            )?;
            Ok(normalised_config(vec![
                ("mode", serde_json::Value::String(mode)),
                ("title", json_text(title)),
                ("limit", serde_json::Value::Number(limit.into())),
            ]))
        }
        SECTION_TYPE_PRODUCT_GRID => {
            let title = section_config_text(config, "title", "title", MAX_SECTION_TITLE_CHARS)?;
            Ok(normalised_config(vec![("title", json_text(title))]))
        }
        SECTION_TYPE_IMAGE_BANNER => {
            let image_url = section_config_image_url(config, "image_url", "banner image")?
                .ok_or_else(|| {
                    "an image banner needs an image — \
                     ইমেজ ব্যানারে ছবি আবশ্যক"
                        .to_string()
                })?;
            let link_url = section_config_link_url(config, "link_url")?;
            let caption = section_config_text(
                config,
                "caption",
                "caption",
                MAX_SECTION_CAPTION_CHARS,
            )?;
            Ok(normalised_config(vec![
                ("image_url", serde_json::Value::String(image_url)),
                ("link_url", json_text(link_url)),
                ("caption", json_text(caption)),
            ]))
        }
        SECTION_TYPE_STORE_INFO => {
            // The store info section carries no config of
            // its own: its content is the store's profile
            // (about / return policy), edited on the
            // branding surface, never duplicated here.
            Ok(normalised_config(Vec::new()))
        }
        SECTION_TYPE_FAQ => {
            let Some(items_value) = config.get("items") else {
                return Err(
                    "an FAQ section needs its question list — \
                     FAQ সেকশনে প্রশ্নের তালিকা আবশ্যক"
                        .to_string(),
                );
            };
            let Some(items) = items_value.as_array() else {
                return Err("config key 'items' must be a list".to_string());
            };
            if items.is_empty() || items.len() > MAX_FAQ_PAIRS {
                return Err(format!(
                    "an FAQ section holds between 1 and {MAX_FAQ_PAIRS} questions — \
                     FAQ সেকশনে ১ থেকে {MAX_FAQ_PAIRS}টি প্রশ্ন রাখা যাবে"
                ));
            }
            let mut normalised_items = Vec::with_capacity(items.len());
            for (position, item) in items.iter().enumerate() {
                let Some(pair) = item.as_object() else {
                    return Err(format!("FAQ item {} must be an object", position + 1));
                };
                let question = section_config_text(
                    pair,
                    "question",
                    "question",
                    MAX_FAQ_QUESTION_CHARS,
                )?
                .ok_or_else(|| format!("FAQ item {} needs a question", position + 1))?;
                let answer = section_config_text(pair, "answer", "answer", MAX_FAQ_ANSWER_CHARS)?
                    .ok_or_else(|| format!("FAQ item {} needs an answer", position + 1))?;
                normalised_items.push(serde_json::json!({
                    "question": question,
                    "answer": answer,
                }));
            }
            Ok(normalised_config(vec![(
                "items",
                serde_json::Value::Array(normalised_items),
            )]))
        }
        _ => Ok(serde_json::Value::Object(config.clone())),
    }
}

/// Validate a whole storefront layout for WRITING and
/// return the normalised document to store. Rules, in
/// check order:
///
/// - the version must be the current one (a future
///   document is refused, never silently rewritten);
/// - at most [`MAX_STOREFRONT_SECTIONS`] sections;
/// - every section id is well-formed and unique — the
///   error names the section's 1-based position, so the
///   seller can find the offending block in the editor;
/// - a scheduling window, when both bounds are present,
///   must be non-empty (the shared window rule);
/// - a KNOWN type's config passes its per-type
///   validation and is normalised to the type's own keys;
/// - an UNKNOWN type is a future section: tolerated and
///   preserved verbatim (its config is not ours to judge)
///   as long as its type string is well-formed — a
///   malformed type string is refused as garbage. The
///   public renderer skips unknown types; the editor
///   shows them as locked blocks, so a seller on an older
///   build can never destroy a newer layout by saving.
pub fn validate_storefront_layout(
    layout: &StorefrontLayout,
) -> Result<StorefrontLayout, String> {
    if layout.version != STOREFRONT_LAYOUT_VERSION {
        return Err(format!(
            "layout version must be {STOREFRONT_LAYOUT_VERSION} — \
             লেআউটের সংস্করণ হতে হবে {STOREFRONT_LAYOUT_VERSION}"
        ));
    }
    if layout.sections.len() > MAX_STOREFRONT_SECTIONS {
        return Err(format!(
            "a layout holds at most {MAX_STOREFRONT_SECTIONS} sections — \
             একটি লেআউটে সর্বোচ্চ {MAX_STOREFRONT_SECTIONS}টি সেকশন রাখা যাবে"
        ));
    }
    let mut seen_ids: Vec<&str> = Vec::with_capacity(layout.sections.len());
    let mut normalised_sections = Vec::with_capacity(layout.sections.len());
    for (index, section) in layout.sections.iter().enumerate() {
        let position = index + 1;
        let prefix = format!("section {position} ({})", section.section_type);
        validate_section_id(&section.id)
            .map_err(|message| format!("{prefix}: {message}"))?;
        if seen_ids.contains(&section.id.as_str()) {
            return Err(format!(
                "{prefix}: section id '{}' is used more than once — \
                 '{}' আইডিটি একাধিকবার ব্যবহৃত হয়েছে",
                section.id, section.id
            ));
        }
        seen_ids.push(section.id.as_str());
        validate_optional_window(section.starts_at, section.ends_at)
            .map_err(|message| format!("{prefix}: {message}"))?;
        if !is_well_formed_section_type(&section.section_type) {
            return Err(format!(
                "{prefix}: section type '{}' is not a valid type name — \
                 '{}' সঠিক সেকশন টাইপ নয়",
                section.section_type, section.section_type
            ));
        }
        let config_map = match &section.config {
            serde_json::Value::Object(map) => map.clone(),
            serde_json::Value::Null => serde_json::Map::new(),
            _ => {
                return Err(format!(
                    "{prefix}: config must be an object — \
                     config একটি অবজেক্ট হতে হবে"
                ))
            }
        };
        let config = if is_known_section_type(&section.section_type) {
            validate_section_config(&section.section_type, &config_map)
                .map_err(|message| format!("{prefix}: {message}"))?
        } else {
            // A future type: preserved verbatim.
            section.config.clone()
        };
        normalised_sections.push(StorefrontSection {
            id: section.id.trim().to_string(),
            section_type: section.section_type.clone(),
            enabled: section.enabled,
            config,
            starts_at: section.starts_at,
            ends_at: section.ends_at,
        });
    }
    Ok(StorefrontLayout {
        version: layout.version,
        sections: normalised_sections,
    })
}

/// Is a section live at `now` (unix seconds)? The one
/// visibility rule, shared with banners: switched on, and
/// its optional window contains `now` — half-open
/// [starts_at, ends_at), so a section appears exactly at
/// its start second and disappears exactly at its end.
pub fn storefront_section_is_live(section: &StorefrontSection, now_unix: i64) -> bool {
    banner_is_visible(section.enabled, section.starts_at, section.ends_at, now_unix)
}

/// One section of the platform default layout, built by
/// [`default_storefront_layout`] and the presets.
fn default_section(
    id: &str,
    section_type: &str,
    config: serde_json::Value,
) -> StorefrontSection {
    StorefrontSection {
        id: id.to_string(),
        section_type: section_type.to_string(),
        enabled: true,
        config,
        starts_at: None,
        ends_at: None,
    }
}

/// The default layout's rail for one source, with the
/// title the pre-Wave-58 storefront showed for it.
fn default_rail(id: &str, source: &str, title: &str) -> StorefrontSection {
    default_section(
        id,
        SECTION_TYPE_PRODUCT_RAIL,
        serde_json::json!({
            "source": source,
            "title": title,
            "limit": DEFAULT_RAIL_LIMIT,
        }),
    )
}

/// The platform default storefront layout — the
/// composition a store renders when it has no stored
/// layout, equivalent to the pre-Wave-58 storefront:
/// the hero (resolved from the store's branding), the
/// three merchandising rails in their historical order
/// (on sale, best sellers, new arrivals), the collection
/// shelf, the taxonomy specialties, the label categories,
/// the full catalogue grid, and the store info blocks.
/// This is CODE, never a seeded row: resetting a layout
/// deletes the row and lands back here.
pub fn default_storefront_layout() -> StorefrontLayout {
    StorefrontLayout {
        version: STOREFRONT_LAYOUT_VERSION,
        sections: vec![
            default_section("hero", SECTION_TYPE_HERO, serde_json::json!({})),
            default_rail(
                "rail-on-sale",
                RAIL_SOURCE_ON_SALE,
                "অফারে আছে — On sale at this store",
            ),
            default_rail(
                "rail-best-sellers",
                RAIL_SOURCE_BEST_SELLERS,
                "এই স্টোরের সেরা বিক্রিত — This store's best sellers",
            ),
            default_rail(
                "rail-new-arrivals",
                RAIL_SOURCE_NEWEST,
                "নতুন এসেছে — New at this store",
            ),
            default_section(
                "collections",
                SECTION_TYPE_COLLECTION_SHELF,
                serde_json::json!({ "limit": DEFAULT_SHELF_LIMIT }),
            ),
            default_section(
                "specialties",
                SECTION_TYPE_CATEGORY_GRID,
                serde_json::json!({
                    "mode": CATEGORY_GRID_MODE_TAXONOMY,
                    "title": "এই স্টোরের বিশেষত্ব — This store specialises in",
                    "limit": 8,
                }),
            ),
            default_section(
                "categories",
                SECTION_TYPE_CATEGORY_GRID,
                serde_json::json!({
                    "mode": CATEGORY_GRID_MODE_LABELS,
                    "title": "ক্যাটাগরি অনুযায়ী কিনুন — Shop by category",
                    "limit": DEFAULT_CATEGORY_GRID_LIMIT,
                }),
            ),
            default_section(
                "all-products",
                SECTION_TYPE_PRODUCT_GRID,
                serde_json::json!({}),
            ),
            default_section("store-info", SECTION_TYPE_STORE_INFO, serde_json::json!({})),
        ],
    }
}

/// Layout preset: the platform default composition.
pub const LAYOUT_PRESET_DEFAULT: &str = "default";
/// Layout preset: the quiet storefront — hero, catalogue,
/// info; no merchandising blocks.
pub const LAYOUT_PRESET_MINIMAL: &str = "minimal";
/// Layout preset: the merchandising-forward storefront —
/// rails and shelves ahead of the catalogue, an FAQ
/// before the info blocks.
pub const LAYOUT_PRESET_MERCHANDISING: &str = "merchandising";

/// Every layout preset key, in picker order.
pub const STOREFRONT_LAYOUT_PRESETS: [&str; 3] = [
    LAYOUT_PRESET_DEFAULT,
    LAYOUT_PRESET_MINIMAL,
    LAYOUT_PRESET_MERCHANDISING,
];

/// Build one named layout preset (the editor's one-click
/// starting points). `None` for an unknown key. Every
/// preset passes [`validate_storefront_layout`] — they
/// are starting points a seller then edits, never a
/// separate storage path.
pub fn storefront_layout_preset(key: &str) -> Option<StorefrontLayout> {
    match key {
        LAYOUT_PRESET_DEFAULT => Some(default_storefront_layout()),
        LAYOUT_PRESET_MINIMAL => Some(StorefrontLayout {
            version: STOREFRONT_LAYOUT_VERSION,
            sections: vec![
                default_section("hero", SECTION_TYPE_HERO, serde_json::json!({})),
                default_section(
                    "all-products",
                    SECTION_TYPE_PRODUCT_GRID,
                    serde_json::json!({}),
                ),
                default_section(
                    "store-info",
                    SECTION_TYPE_STORE_INFO,
                    serde_json::json!({}),
                ),
            ],
        }),
        LAYOUT_PRESET_MERCHANDISING => Some(StorefrontLayout {
            version: STOREFRONT_LAYOUT_VERSION,
            sections: vec![
                default_section("hero", SECTION_TYPE_HERO, serde_json::json!({})),
                default_rail(
                    "rail-on-sale",
                    RAIL_SOURCE_ON_SALE,
                    "অফারে আছে — On sale at this store",
                ),
                default_rail(
                    "rail-best-sellers",
                    RAIL_SOURCE_BEST_SELLERS,
                    "এই স্টোরের সেরা বিক্রিত — This store's best sellers",
                ),
                default_section(
                    "collections",
                    SECTION_TYPE_COLLECTION_SHELF,
                    serde_json::json!({ "limit": DEFAULT_SHELF_LIMIT }),
                ),
                default_rail(
                    "rail-new-arrivals",
                    RAIL_SOURCE_NEWEST,
                    "নতুন এসেছে — New at this store",
                ),
                default_section(
                    "categories",
                    SECTION_TYPE_CATEGORY_GRID,
                    serde_json::json!({
                        "mode": CATEGORY_GRID_MODE_LABELS,
                        "title": "ক্যাটাগরি অনুযায়ী কিনুন — Shop by category",
                        "limit": DEFAULT_CATEGORY_GRID_LIMIT,
                    }),
                ),
                default_section(
                    "all-products",
                    SECTION_TYPE_PRODUCT_GRID,
                    serde_json::json!({}),
                ),
                default_section(
                    "faq",
                    SECTION_TYPE_FAQ,
                    serde_json::json!({
                        "items": [
                            {
                                "question": "ক্যাশ অন ডেলিভারি আছে কি?",
                                "answer": "হ্যাঁ — পণ্য হাতে পেয়ে মূল্য পরিশোধ করুন।",
                            },
                            {
                                "question": "ডেলিভারি পেতে কতদিন লাগবে?",
                                "answer": "ঢাকার ভিতরে ১–২ দিন, ঢাকার বাইরে ৩–৫ দিন।",
                            },
                        ],
                    }),
                ),
                default_section(
                    "store-info",
                    SECTION_TYPE_STORE_INFO,
                    serde_json::json!({}),
                ),
            ],
        }),
        _ => None,
    }
}

/// One section type's editor metadata: the names and the
/// one-line description the add-section picker shows.
/// The catalogue is data (not UI copy buried in the web
/// app) so the picker and any future client describe the
/// types identically.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SectionTypeMeta {
    pub section_type: &'static str,
    pub label_en: &'static str,
    pub label_bn: &'static str,
    pub description_en: &'static str,
    pub description_bn: &'static str,
}

/// The section catalogue: every known type with its
/// editor labels and descriptions, in picker order.
pub fn storefront_section_catalogue() -> Vec<SectionTypeMeta> {
    vec![
        SectionTypeMeta {
            section_type: SECTION_TYPE_HERO,
            label_en: "Hero",
            label_bn: "হিরো",
            description_en: "Headline, subtext, image and a call-to-action — the storefront's crown. Empty fields fall back to your branding.",
            description_bn: "শিরোনাম, উপ-লেখা, ছবি ও একটি বোতাম — স্টোরফ্রন্টের মুকুট। খালি ঘর ব্র্যান্ডিং থেকে পূরণ হয়।",
        },
        SectionTypeMeta {
            section_type: SECTION_TYPE_RICH_TEXT,
            label_en: "Rich text",
            label_bn: "রিচ টেক্সট",
            description_en: "A heading and a paragraph in your own words — your story, a delivery note, a campaign.",
            description_bn: "আপনার নিজের কথায় একটি শিরোনাম ও অনুচ্ছেদ — আপনার গল্প, ডেলিভারির নোট, কোনো প্রচারণা।",
        },
        SectionTypeMeta {
            section_type: SECTION_TYPE_PRODUCT_RAIL,
            label_en: "Product rail",
            label_bn: "পণ্যের রেল",
            description_en: "A sliding row of product cards: newest, best sellers, on sale, or hand-picked by you.",
            description_bn: "পণ্যের কার্ডের একটি স্লাইডিং সারি: নতুন, সেরা বিক্রিত, অফারের, অথবা আপনার হাতে-বাছাই।",
        },
        SectionTypeMeta {
            section_type: SECTION_TYPE_COLLECTION_SHELF,
            label_en: "Collection shelf",
            label_bn: "কালেকশন শেলফ",
            description_en: "Your collections as tiles, or one collection's products as a shelf.",
            description_bn: "আপনার কালেকশনগুলো টাইল আকারে, অথবা একটি কালেকশনের পণ্য শেলফ আকারে।",
        },
        SectionTypeMeta {
            section_type: SECTION_TYPE_CATEGORY_GRID,
            label_en: "Category grid",
            label_bn: "ক্যাটাগরি গ্রিড",
            description_en: "The categories you sell in, as tiles with live counts.",
            description_bn: "আপনি যেসব ক্যাটাগরিতে বিক্রি করেন, লাইভ সংখ্যাসহ টাইল আকারে।",
        },
        SectionTypeMeta {
            section_type: SECTION_TYPE_PRODUCT_GRID,
            label_en: "Product grid",
            label_bn: "পণ্যের গ্রিড",
            description_en: "Your full catalogue grid — every active product, paged.",
            description_bn: "আপনার পুরো ক্যাটালগের গ্রিড — সব সক্রিয় পণ্য, পেজ আকারে।",
        },
        SectionTypeMeta {
            section_type: SECTION_TYPE_IMAGE_BANNER,
            label_en: "Image banner",
            label_bn: "ইমেজ ব্যানার",
            description_en: "One wide image with an optional link and caption — a campaign of your own.",
            description_bn: "একটি চওড়া ছবি, চাইলে লিংক ও ক্যাপশনসহ — আপনার নিজের প্রচারণা।",
        },
        SectionTypeMeta {
            section_type: SECTION_TYPE_STORE_INFO,
            label_en: "Store info",
            label_bn: "স্টোরের তথ্য",
            description_en: "Your about text and return policy (written on the branding page), with your trust numbers.",
            description_bn: "আপনার পরিচিতি ও রিটার্ন নীতি (ব্র্যান্ডিং পেজে লেখা), আস্থার সংখ্যাসহ।",
        },
        SectionTypeMeta {
            section_type: SECTION_TYPE_FAQ,
            label_en: "FAQ",
            label_bn: "সাধারণ প্রশ্ন",
            description_en: "Question and answer pairs — delivery, payment and returns, answered before buyers ask.",
            description_bn: "প্রশ্ন-উত্তরের জোড়া — ডেলিভারি, পেমেন্ট ও রিটার্নের উত্তর, ক্রেতা জিজ্ঞেস করার আগেই।",
        },
    ]
}

/// Storefront section analytics: a section was seen.
pub const STOREFRONT_EVENT_SECTION_VIEW: &str = "section_view";
/// Storefront section analytics: a section's link or
/// call-to-action was clicked.
pub const STOREFRONT_EVENT_SECTION_CLICK: &str = "section_click";

/// Every storefront event kind.
pub const STOREFRONT_EVENT_KINDS: [&str; 2] = [
    STOREFRONT_EVENT_SECTION_VIEW,
    STOREFRONT_EVENT_SECTION_CLICK,
];

/// Validate a storefront section event kind (the public
/// beacon's only vocabulary — anything else is refused,
/// the merch-events rule).
pub fn validate_storefront_event_kind(kind: &str) -> Result<&str, &'static str> {
    match kind {
        STOREFRONT_EVENT_SECTION_VIEW => Ok(STOREFRONT_EVENT_SECTION_VIEW),
        STOREFRONT_EVENT_SECTION_CLICK => Ok(STOREFRONT_EVENT_SECTION_CLICK),
        _ => Err("kind must be 'section_view' or 'section_click'"),
    }
}

/// The seller section-stats window: counts are all-time
/// plus the last this many days (Asia/Dhaka calendar days
/// at the API, like every analytics surface).
pub const STOREFRONT_STATS_WINDOW_DAYS: i64 = 30;

// ---------------------------------------------------------------------------
// Store content: custom pages and the store blog (Wave 59)
// ---------------------------------------------------------------------------

/// Content lifecycle statuses (Wave 59), shared by store
/// pages and store blog posts and mirrored by the CHECK
/// constraints in migrations 0067/0068. A `draft` is the
/// seller's private work; `published` is public subject
/// to the post scheduling rule ([`content_is_live`]);
/// `archived` is retired from every public surface but
/// kept in the seller's list, restorable at any time.
pub const CONTENT_STATUS_DRAFT: &str = "draft";
pub const CONTENT_STATUS_PUBLISHED: &str = "published";
pub const CONTENT_STATUS_ARCHIVED: &str = "archived";

/// Every content status, for validators and the web's
/// status pickers (one vocabulary, served by the API).
pub const CONTENT_STATUSES: [&str; 3] = [
    CONTENT_STATUS_DRAFT,
    CONTENT_STATUS_PUBLISHED,
    CONTENT_STATUS_ARCHIVED,
];

/// Is this one of the content lifecycle statuses?
pub fn is_valid_content_status(status: &str) -> bool {
    CONTENT_STATUSES.contains(&status)
}

/// May content move from `from` to `to` (Wave 59)? The
/// state machine is deliberately small: any status may
/// move to any OTHER status (draft publishes, published
/// unpublishes back to draft or retires to archived, an
/// archived piece restores to draft or republishes
/// directly), and staying in place is not a transition —
/// the API answers a no-op status write with a 400
/// naming the current state rather than pretending a
/// change happened. Unknown statuses never transition.
pub fn can_transition_content_status(from: &str, to: &str) -> bool {
    is_valid_content_status(from) && is_valid_content_status(to) && from != to
}

/// Is a piece of content visible to the public right
/// now (Wave 59)? The one visibility rule every public
/// content read applies: the status must be `published`
/// and, for posts, the publish moment must have arrived.
/// Pages carry no schedule (their `published_at` is
/// `None` in this call and the status alone decides);
/// a published post with a future `published_at` is
/// SCHEDULED — written, committed, and not yet public.
pub fn content_is_live(status: &str, published_at_unix: Option<i64>, now_unix: i64) -> bool {
    if status != CONTENT_STATUS_PUBLISHED {
        return false;
    }
    match published_at_unix {
        Some(at) => at <= now_unix,
        None => true,
    }
}

/// How many custom pages one store may keep (Wave 59).
/// Pages are the store's standing documents (about,
/// shipping, size guide, FAQ); past this count the store
/// is writing a wiki, not a storefront.
pub const MAX_STORE_PAGES_PER_STORE: i64 = 25;

/// How many blog posts one store may keep (Wave 59),
/// across all statuses. A generous ceiling — a post a
/// day for well over a year — that still bounds a runaway
/// import or a script gone wrong.
pub const MAX_STORE_POSTS_PER_STORE: i64 = 500;

/// Longest custom-page title, in characters (Wave 59).
pub const MAX_STORE_PAGE_TITLE_CHARS: usize = 120;

/// Longest blog-post title, in characters (Wave 59).
pub const MAX_STORE_POST_TITLE_CHARS: usize = 160;

/// Longest custom-page body, in characters (Wave 59):
/// twenty thousand characters is a long policy document
/// or guide, far past what a storefront page should be.
pub const MAX_STORE_PAGE_BODY_CHARS: usize = 20_000;

/// Longest blog-post body, in characters (Wave 59):
/// forty thousand characters is a substantial article.
pub const MAX_STORE_POST_BODY_CHARS: usize = 40_000;

/// Longest hand-written post excerpt, in characters
/// (Wave 59). A blank excerpt is not an error — it means
/// "derive one from the body" ([`derive_post_excerpt`]).
pub const MAX_POST_EXCERPT_CHARS: usize = 300;

/// The character budget a derived excerpt aims for
/// (Wave 59): the teaser shown on blog cards and in the
/// feed. The derivation cuts on a word boundary inside
/// this budget, so a derived excerpt is usually a little
/// shorter, never longer.
pub const POST_EXCERPT_DERIVE_CHARS: usize = 240;

/// How many tags one post may carry (Wave 59).
pub const MAX_POST_TAGS: usize = 8;

/// Longest single post tag, in characters (Wave 59).
pub const MAX_POST_TAG_CHARS: usize = 30;

/// Reading-speed assumption for the reading-time
/// estimate (Wave 59), in words per minute: the usual
/// adult silent-reading average used by blogging
/// platforms. The estimate is a display hint, never a
/// promise, and is labelled as approximate in the UI.
pub const READING_WORDS_PER_MINUTE: i64 = 200;

/// How many posts the store's Atom feed carries (Wave
/// 59): the most recent published ones.
pub const STORE_POST_FEED_ITEMS: i64 = 20;

/// The seller post-stats window: counts are all-time
/// plus the last this many days (Asia/Dhaka calendar
/// days at the API, like every analytics surface).
pub const STORE_POST_STATS_WINDOW_DAYS: i64 = 30;

/// Validate a content slug (Wave 59) — the platform's
/// one slug grammar ([`is_valid_slug`]: 3–40 chars of
/// lowercase letters, digits and hyphens, starting and
/// ending alphanumeric), with the refusal worded
/// bilingually like the other storefront rules. Page
/// slugs are unique within their store and post slugs
/// within theirs; the DB constraints are the backstop
/// and this is the shape rule. Returns the slug.
pub fn validate_content_slug(slug: &str) -> Result<String, String> {
    if is_valid_slug(slug) {
        Ok(slug.to_string())
    } else {
        Err(
            "স্লাগটি ৩-৪০ অক্ষরের হতে হবে (ছোট হাতের অক্ষর, সংখ্যা ও হাইফেন) — \
             the slug must be 3-40 characters of lowercase letters, digits and \
             hyphens"
                .to_string(),
        )
    }
}

/// The shared shape behind the two content title
/// validators: required, trimmed, char-capped, with the
/// refusal naming the field in both languages.
fn validate_content_title(
    raw: &str,
    max_chars: usize,
    bn_label: &str,
    en_label: &str,
) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{bn_label} আবশ্যক — a {en_label} is required"));
    }
    if trimmed.chars().count() > max_chars {
        return Err(format!(
            "{bn_label} সর্বোচ্চ {max_chars} অক্ষরের হতে হবে — the {en_label} \
             must be at most {max_chars} characters"
        ));
    }
    Ok(trimmed.to_string())
}

/// Validate a custom page's title (Wave 59).
pub fn validate_store_page_title(title: &str) -> Result<String, String> {
    validate_content_title(title, MAX_STORE_PAGE_TITLE_CHARS, "পেজের শিরোনাম", "page title")
}

/// Validate a blog post's title (Wave 59).
pub fn validate_store_post_title(title: &str) -> Result<String, String> {
    validate_content_title(title, MAX_STORE_POST_TITLE_CHARS, "পোস্টের শিরোনাম", "post title")
}

/// The shared shape behind the two content body
/// validators: required, edge-trimmed (the interior is
/// the author's, untouched), char-capped. Bodies are
/// PLAIN TEXT — the platform renders paragraphs, never
/// seller markup (see the Wave 59 README section).
fn validate_content_body(
    raw: &str,
    max_chars: usize,
    bn_label: &str,
    en_label: &str,
) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{bn_label} আবশ্যক — the {en_label} is required"));
    }
    if trimmed.chars().count() > max_chars {
        return Err(format!(
            "{bn_label} সর্বোচ্চ {max_chars} অক্ষরের হতে হবে — the {en_label} \
             must be at most {max_chars} characters"
        ));
    }
    Ok(trimmed.to_string())
}

/// Validate a custom page's body (Wave 59): plain text,
/// at most [`MAX_STORE_PAGE_BODY_CHARS`] characters.
pub fn validate_store_page_body(body: &str) -> Result<String, String> {
    validate_content_body(body, MAX_STORE_PAGE_BODY_CHARS, "পেজের লেখা", "page body")
}

/// Validate a blog post's body (Wave 59): plain text,
/// at most [`MAX_STORE_POST_BODY_CHARS`] characters.
pub fn validate_store_post_body(body: &str) -> Result<String, String> {
    validate_content_body(body, MAX_STORE_POST_BODY_CHARS, "পোস্টের লেখা", "post body")
}

/// Validate a store's page count before creating one
/// more (Wave 59): at [`MAX_STORE_PAGES_PER_STORE`] the
/// store must retire a page first.
pub fn validate_store_page_count(current: i64) -> Result<(), String> {
    if !(0..MAX_STORE_PAGES_PER_STORE).contains(&current) {
        return Err(format!(
            "একটি স্টোর সর্বোচ্চ {}টি পেজ রাখতে পারে — a store may keep at \
             most {} pages",
            MAX_STORE_PAGES_PER_STORE, MAX_STORE_PAGES_PER_STORE
        ));
    }
    Ok(())
}

/// Validate a store's post count before creating one
/// more (Wave 59): at [`MAX_STORE_POSTS_PER_STORE`] the
/// store must retire a post first.
pub fn validate_store_post_count(current: i64) -> Result<(), String> {
    if !(0..MAX_STORE_POSTS_PER_STORE).contains(&current) {
        return Err(format!(
            "একটি স্টোর সর্বোচ্চ {}টি ব্লগ পোস্ট রাখতে পারে — a store may \
             keep at most {} blog posts",
            MAX_STORE_POSTS_PER_STORE, MAX_STORE_POSTS_PER_STORE
        ));
    }
    Ok(())
}

/// Normalize a post's tag list (Wave 59): each tag is
/// trimmed, lowercased (Unicode), and its interior
/// whitespace runs collapse to single hyphens, so
/// "Eid Collection", "eid-collection" and "  EID
/// COLLECTION " are the same tag. Blank entries drop
/// out silently (a trailing comma in the editor is not
/// an error). The result is deduplicated in first-seen
/// order and capped at [`MAX_POST_TAGS`]; a tag over
/// [`MAX_POST_TAG_CHARS`] characters, or carrying any
/// character that is not a letter, digit or hyphen, is
/// refused by name.
pub fn normalize_post_tags(tags: &[String]) -> Result<Vec<String>, String> {
    let mut normalized: Vec<String> = Vec::with_capacity(tags.len());
    for raw in tags {
        let lowered = raw.trim().to_lowercase();
        if lowered.is_empty() {
            continue;
        }
        let mut tag = String::with_capacity(lowered.len());
        let mut last_was_space = false;
        for ch in lowered.chars() {
            if ch.is_whitespace() {
                last_was_space = true;
                continue;
            }
            if last_was_space && !tag.is_empty() {
                tag.push('-');
            }
            last_was_space = false;
            tag.push(ch);
        }
        if tag.chars().count() > MAX_POST_TAG_CHARS {
            return Err(format!(
                "ট্যাগ '{tag}' সর্বোচ্চ {MAX_POST_TAG_CHARS} অক্ষরের হতে পারে — \
                 the tag '{tag}' may be at most {MAX_POST_TAG_CHARS} characters"
            ));
        }
        if !tag
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-')
        {
            return Err(format!(
                "ট্যাগ '{tag}'-এ শুধু অক্ষর, সংখ্যা ও হাইফেন থাকতে পারে — \
                 the tag '{tag}' may contain only letters, digits and hyphens"
            ));
        }
        if !normalized.contains(&tag) {
            normalized.push(tag);
        }
    }
    if normalized.len() > MAX_POST_TAGS {
        return Err(format!(
            "একটি পোস্টে সর্বোচ্চ {}টি ট্যাগ থাকতে পারে — a post may carry \
             at most {} tags",
            MAX_POST_TAGS, MAX_POST_TAGS
        ));
    }
    Ok(normalized)
}

/// Derive a post's excerpt from its body (Wave 59): the
/// body's whitespace runs (newlines included) collapse
/// to single spaces; a body that fits
/// [`POST_EXCERPT_DERIVE_CHARS`] is its own excerpt.
/// A longer body is cut at the last word boundary
/// inside the budget, so the excerpt ends mid-article
/// but never mid-word; a single unbroken word longer
/// than the budget is hard-cut at the budget. No
/// ellipsis is appended — the excerpt simply ends, and
/// the surfaces showing it link to the full post; an
/// ellipsis would claim a truncation style the text
/// does not have.
pub fn derive_post_excerpt(body: &str) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<&str>>().join(" ");
    if collapsed.chars().count() <= POST_EXCERPT_DERIVE_CHARS {
        return collapsed;
    }
    let head: String = collapsed.chars().take(POST_EXCERPT_DERIVE_CHARS).collect();
    match head.rfind(' ') {
        Some(idx) if idx > 0 => head[..idx].to_string(),
        _ => head,
    }
}

/// Validate a hand-written post excerpt (Wave 59):
/// blank becomes `None` (the post then derives its
/// excerpt from the body), a present value is trimmed
/// and capped at [`MAX_POST_EXCERPT_CHARS`].
pub fn validate_post_excerpt(text: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = text else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_POST_EXCERPT_CHARS {
        return Err(format!(
            "সারাংশ সর্বোচ্চ {} অক্ষরের হতে হবে — the excerpt must be at \
             most {} characters",
            MAX_POST_EXCERPT_CHARS, MAX_POST_EXCERPT_CHARS
        ));
    }
    Ok(Some(trimmed.to_string()))
}

/// Count the words in a text (Wave 59): maximal runs
/// separated by any whitespace. Bangla and English text
/// both word-separate on spaces in practice, and the
/// count only feeds the reading-time estimate.
pub fn count_words(text: &str) -> i64 {
    text.split_whitespace().count() as i64
}

/// The reading-time estimate for a word count (Wave
/// 59): whole minutes at [`READING_WORDS_PER_MINUTE`],
/// rounded up, never below one minute — a 30-word post
/// still takes a moment to read, and "0 min read" would
/// be a false claim about any real text.
pub fn reading_minutes_for_words(words: i64) -> i64 {
    if words <= 0 {
        return 1;
    }
    (words + READING_WORDS_PER_MINUTE - 1) / READING_WORDS_PER_MINUTE
}

/// The reading-time estimate for a body of text (Wave
/// 59): [`reading_minutes_for_words`] over
/// [`count_words`].
pub fn estimate_reading_minutes(body: &str) -> i64 {
    reading_minutes_for_words(count_words(body))
}

/// Escape a text for XML content or attribute use
/// (Wave 59): the five predefined entities. The store
/// blog's Atom feed is hand-built with this at every
/// interpolation — a post title carrying `&` or `<`
/// reaches feed readers as text, never as markup.
pub fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

// =====================================================================
// Wave 60 — gift cards: store-scoped stored value.
//
// A gift card is a BEARER instrument: the code is the
// credential, exactly like a Wave 37 API key. The full code
// is shown exactly once at issuance; only its SHA-256 hash
// is stored. Anyone holding the code can check its balance
// and spend it — that is what a gift card IS (Shopify's work
// the same way); the protection is the code's entropy and
// the hash-only storage, and the tradeoff is documented in
// the README's Wave 60 section.
//
// Scope: STORE-scoped cards only. A card is issued by one
// store and redeems only against that store's sub-order in
// a checkout. Platform-wide cards are a documented future.
//
// Funding model — read this before touching the money:
// issuance in this wave is PROMOTIONAL / MANUAL (a seller
// or an admin creates cards; there is NO purchase flow and
// NO money is collected at issuance). Redemption is
// therefore SELLER-FUNDED: it behaves exactly like a store
// coupon discount on that store's sub-order — the
// commission base and the seller payout snapshot both net
// it out (see `build_order_totals`). The platform never
// funds a gift-card redemption.
//
// Redemption ordering at checkout (pinned by tests):
// coupon first, then the gift card against the sub-order's
// POST-COUPON goods, then loyalty — loyalty's 50% cap base
// is the goods net of coupon AND gift card, and the COD
// advance base is the grand total net of all three.
// =====================================================================

/// The display prefix of every gift-card code (Wave 60).
/// Codes look like `bdgc_K7QX…`: the prefix says what the
/// string is when a buyer pastes it into the wrong field,
/// and it keeps gift-card codes visually distinct from
/// Wave 37 API keys (`bdm_live_…`).
pub const GIFT_CARD_CODE_PREFIX: &str = "bdgc_";

/// Random characters after the prefix (Wave 60). Sampled
/// from uppercase letters + digits (36 symbols), 24 chars
/// carry ≈ 124 bits of entropy — comfortably beyond
/// guessing, short enough to read off a printed card.
pub const GIFT_CARD_CODE_RANDOM_CHARS: usize = 24;

/// Total length of a full gift-card code, prefix included
/// (Wave 60).
pub const GIFT_CARD_CODE_LENGTH: usize = 29;

/// Smallest issuable card value (Wave 60): ৳10. Below this
/// a card is confetti, not stored value.
pub const GIFT_CARD_MIN_ISSUE_POISHA: i64 = 1_000;

/// Largest issuable card value (Wave 60): ৳50,000. A
/// promotional instrument issued without payment should
/// not be able to create an unbounded store liability in
/// one click; bigger campaigns issue more cards.
pub const GIFT_CARD_MAX_ISSUE_POISHA: i64 = 5_000_000;

/// Most cards one bulk-issue call may create (Wave 60).
pub const GIFT_CARD_MAX_BULK_ISSUE: i64 = 50;

/// Default validity when neither the issuer nor the
/// platform setting says otherwise (Wave 60): one year.
pub const GIFT_CARD_DEFAULT_VALIDITY_DAYS: i64 = 365;

/// Longest validity an issuer may pick (Wave 60): ten
/// years. Zero or negative validity is rejected at issue —
/// a card that is born expired is a support ticket, not a
/// feature; a card with NO expiry is expressed by the
/// issuer picking the no-expiry option, not by 0 days.
pub const GIFT_CARD_MAX_VALIDITY_DAYS: i64 = 3_650;

/// The platform_settings key for the default validity
/// (Wave 60). DB value wins over
/// [`GIFT_CARD_DEFAULT_VALIDITY_DAYS`] when it parses to a
/// valid day count (the `platform_setting_i64` precedent).
pub const GIFT_CARD_VALIDITY_SETTING_KEY: &str = "gift_card_default_validity_days";

/// How far ahead the worker's expiry sweep looks (Wave 60
/// extension): a card expiring within this many days, still
/// holding a balance and not yet reminded about, earns its
/// issuing seller ONE reminder (the card row is stamped so
/// the sweep never repeats it). A week is long enough to
/// reach the recipient and short enough that the balance
/// figure in the message is still roughly true.
pub const GIFT_CARD_EXPIRY_REMINDER_WINDOW_DAYS: i64 = 7;

/// Longest issuer note on a card (Wave 60) — "ঈদ মোবারক"
/// territory, not an essay.
pub const GIFT_CARD_MAX_NOTE_CHARS: usize = 200;

/// Longest recipient name on a card (Wave 60).
pub const GIFT_CARD_MAX_RECIPIENT_CHARS: usize = 80;

/// Ledger kind: the issuance movement (Wave 60). Delta is
/// the card's full initial value, positive.
pub const GIFT_CARD_LEDGER_ISSUE: &str = "issue";

/// Ledger kind: a checkout redemption (Wave 60). Delta is
/// negative; the row references the order + sub-order.
pub const GIFT_CARD_LEDGER_REDEEM: &str = "redeem";

/// Ledger kind: a redemption restored because the order or
/// sub-order was cancelled before delivery (Wave 60).
/// Delta is positive, exactly the redeemed amount.
pub const GIFT_CARD_LEDGER_CANCEL_RESTORE: &str = "cancel_restore";

/// Ledger kind: a redemption restored because a return on
/// the funded sub-order was received (Wave 60). Delta is
/// positive, proportional to the refund basis (see
/// [`gift_card_restore_for_return`]).
pub const GIFT_CARD_LEDGER_RETURN_RESTORE: &str = "return_restore";

/// Ledger kind: an admin's manual balance correction
/// (Wave 60). Delta is signed and never takes the balance
/// below zero; every such row is written in the same
/// transaction as an admin audit-log entry.
pub const GIFT_CARD_LEDGER_ADMIN_ADJUST: &str = "admin_adjust";

/// The whole ledger-kind vocabulary (Wave 60). The API
/// writes only these; the database CHECK mirrors the list.
///
/// Expiry is deliberately NOT a ledger kind: expiry is
/// time-derived (the loyalty precedent), never a stored
/// flip, so there is no movement to record — the balance
/// does not change when a card expires.
pub const GIFT_CARD_LEDGER_KINDS: [&str; 5] = [
    GIFT_CARD_LEDGER_ISSUE,
    GIFT_CARD_LEDGER_REDEEM,
    GIFT_CARD_LEDGER_CANCEL_RESTORE,
    GIFT_CARD_LEDGER_RETURN_RESTORE,
    GIFT_CARD_LEDGER_ADMIN_ADJUST,
];

/// Derived card status: live and spendable (Wave 60).
pub const GIFT_CARD_STATUS_ACTIVE: &str = "active";

/// Derived card status: frozen by the seller or an admin
/// (Wave 60). A stored fact (`disabled_at`), reversible.
pub const GIFT_CARD_STATUS_DISABLED: &str = "disabled";

/// Derived card status: past its expiry moment (Wave 60).
/// Time-derived at read; no stored flip.
pub const GIFT_CARD_STATUS_EXPIRED: &str = "expired";

/// Derived card status: fully redeemed, nothing left to
/// spend (Wave 60). Distinct from expired — the card kept
/// its promise completely.
pub const GIFT_CARD_STATUS_DEPLETED: &str = "depleted";

/// Normalise a gift-card code as typed or pasted by a
/// holder (Wave 60): whitespace and hyphens are dropped
/// (printed cards group the code for readability) and the
/// rest is upper-cased, then the shape is checked. Returns
/// the canonical form — the string that is hashed and
/// looked up — or `None` when the input cannot be a code
/// this platform issued. Generation samples from the same
/// uppercase alphabet, so a genuinely issued code always
/// normalises to itself (modulo the prefix's case).
pub fn normalize_gift_card_code(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_ascii_uppercase();
    if gift_card_code_shape_ok(&cleaned) {
        Some(cleaned)
    } else {
        None
    }
}

/// The shape check on a NORMALISED code (Wave 60): the
/// upper-cased prefix `BDGC_`, then exactly
/// [`GIFT_CARD_CODE_RANDOM_CHARS`] uppercase-alphanumeric
/// characters, nothing else.
pub fn gift_card_code_shape_ok(normalised: &str) -> bool {
    if normalised.len() != GIFT_CARD_CODE_LENGTH {
        return false;
    }
    let Some(rest) = normalised.strip_prefix("BDGC_") else {
        return false;
    };
    rest.len() == GIFT_CARD_CODE_RANDOM_CHARS
        && rest.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// The display prefix of an issued code (Wave 60): the
/// first 12 characters — enough to recognise a card in a
/// list or a support chat, never enough to spend it. The
/// argument is the code AS ISSUED (prefix in its display
/// case); shorter inputs return whole.
pub fn gift_card_display_prefix(code: &str) -> String {
    code.chars().take(12).collect()
}

/// The last four characters of an issued code (Wave 60):
/// the "••1234" tail every card product shows. The
/// argument is the code AS ISSUED; shorter inputs return
/// whole.
pub fn gift_card_last4(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    let start = chars.len().saturating_sub(4);
    chars[start..].iter().collect()
}

/// Validate one issuance (Wave 60): the amount must sit
/// inside the documented caps, the optional note and
/// recipient inside their character caps, and an explicit
/// validity inside 1..=[`GIFT_CARD_MAX_VALIDITY_DAYS`]
/// days. `validity_days: None` means "the default" and is
/// always valid here — the API resolves it against the
/// platform setting before calling anything else.
pub fn validate_gift_card_issue(
    amount_poisha: i64,
    note: Option<&str>,
    recipient_name: Option<&str>,
    validity_days: Option<i64>,
) -> Result<(), &'static str> {
    if amount_poisha < GIFT_CARD_MIN_ISSUE_POISHA {
        return Err("gift card amount is below the minimum (10 taka)");
    }
    if amount_poisha > GIFT_CARD_MAX_ISSUE_POISHA {
        return Err("gift card amount exceeds the maximum (50,000 taka)");
    }
    if let Some(note) = note {
        if note.chars().count() > GIFT_CARD_MAX_NOTE_CHARS {
            return Err("gift card note is too long");
        }
    }
    if let Some(name) = recipient_name {
        if name.chars().count() > GIFT_CARD_MAX_RECIPIENT_CHARS {
            return Err("gift card recipient name is too long");
        }
    }
    if let Some(days) = validity_days {
        if !(1..=GIFT_CARD_MAX_VALIDITY_DAYS).contains(&days) {
            return Err("gift card validity days out of range");
        }
    }
    Ok(())
}

/// Validate a bulk-issue count (Wave 60): 1..=
/// [`GIFT_CARD_MAX_BULK_ISSUE`].
pub fn validate_gift_card_bulk_count(count: i64) -> Result<(), &'static str> {
    if !(1..=GIFT_CARD_MAX_BULK_ISSUE).contains(&count) {
        return Err("gift card bulk count out of range");
    }
    Ok(())
}

/// Validate an admin balance adjustment (Wave 60) and
/// return the resulting balance: the delta must be
/// non-zero (a zero adjust is a no-op wearing a costume)
/// and the result must stay within [0, i64] — an adjust
/// can zero a card but never drive it negative, and the
/// addition is checked (the Wave 39 money canon).
pub fn validate_gift_card_adjust(
    balance_poisha: i64,
    delta_poisha: i64,
) -> Result<i64, &'static str> {
    if delta_poisha == 0 {
        return Err("gift card adjustment must be non-zero");
    }
    let next = balance_poisha
        .checked_add(delta_poisha)
        .ok_or("gift card adjustment overflows the money range")?;
    if next < 0 {
        return Err("gift card adjustment would take the balance below zero");
    }
    Ok(next)
}

/// How much of a card's balance a checkout may apply
/// (Wave 60): the smaller of the balance and the eligible
/// goods — the target sub-order's POST-COUPON goods (see
/// the section header for the ordering). Either side at or
/// below zero means nothing applies; the result is never
/// negative and never exceeds either input.
pub fn gift_card_applicable_poisha(balance_poisha: i64, eligible_goods_poisha: i64) -> i64 {
    balance_poisha.min(eligible_goods_poisha).max(0)
}

/// The redemption restored to a card when a return on the
/// funded sub-order is received (Wave 60):
/// `floor(redemption × refund ÷ refundable_goods)`,
/// clamped to [0, redemption]. Returns in this platform
/// are whole-sub-order, so the refund normally EQUALS the
/// refundable base (goods − coupon discount) and the FULL
/// redemption comes back; the proportion keeps the rule
/// exact if a partial-refund basis ever appears, and the
/// floor means rounding never invents value. A
/// non-positive basis or refund restores nothing. The
/// multiplication runs in i128 so no realistic money
/// figures overflow before the division.
pub fn gift_card_restore_for_return(
    redemption_poisha: i64,
    refund_poisha: i64,
    refundable_goods_poisha: i64,
) -> i64 {
    if redemption_poisha <= 0 || refund_poisha <= 0 || refundable_goods_poisha <= 0 {
        return 0;
    }
    let restored = (redemption_poisha as i128 * refund_poisha as i128)
        / refundable_goods_poisha as i128;
    restored.clamp(0, redemption_poisha as i128) as i64
}

/// Whether a card is past its expiry at `now` (Wave 60):
/// time-derived, the loyalty precedent — no stored status
/// flip exists anywhere. A card with no expiry never
/// expires. The boundary is inclusive: at the expiry
/// moment itself the card is already dead, matching how a
/// printed "valid until" date reads.
pub fn gift_card_is_expired(expires_at_unix: Option<i64>, now_unix: i64) -> bool {
    match expires_at_unix {
        Some(at) => at <= now_unix,
        None => false,
    }
}

/// A card's derived status (Wave 60), in precedence
/// order: disabled beats expired (the freeze is a human
/// act the holder should see named), expired beats
/// depleted (a dead card with money left is "expired",
/// the actionable fact), depleted beats active. Nothing
/// here is stored — the API derives it from
/// `disabled_at`, `expires_at` and the balance at read.
pub fn gift_card_status(
    disabled: bool,
    balance_poisha: i64,
    expires_at_unix: Option<i64>,
    now_unix: i64,
) -> &'static str {
    if disabled {
        return GIFT_CARD_STATUS_DISABLED;
    }
    if gift_card_is_expired(expires_at_unix, now_unix) {
        return GIFT_CARD_STATUS_EXPIRED;
    }
    if balance_poisha <= 0 {
        return GIFT_CARD_STATUS_DEPLETED;
    }
    GIFT_CARD_STATUS_ACTIVE
}

/// Whether a card can be redeemed right now (Wave 60):
/// its derived status is active, which by construction
/// also means the balance is positive.
pub fn gift_card_is_redeemable(
    disabled: bool,
    balance_poisha: i64,
    expires_at_unix: Option<i64>,
    now_unix: i64,
) -> bool {
    gift_card_status(disabled, balance_poisha, expires_at_unix, now_unix)
        == GIFT_CARD_STATUS_ACTIVE
}

/// The expiry moment for a card issued at `now_unix` with
/// a validity of `validity_days` (Wave 60). Validity is
/// validated to 1..=3,650 days before this is called, so
/// the multiplication cannot overflow; the final addition
/// is still checked, saturating at the end of time rather
/// than wrapping.
pub fn gift_card_expiry_unix(now_unix: i64, validity_days: i64) -> i64 {
    let span = validity_days.saturating_mul(86_400);
    now_unix.checked_add(span).unwrap_or(i64::MAX)
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
                STORE_ACTION_MANAGE_MEMBERS
                    | STORE_ACTION_MANAGE_PAYOUTS
                    | STORE_ACTION_MANAGE_VERIFICATION
            );
            assert_eq!(
                store_role_can("manager", action),
                expected,
                "manager / {action}"
            );
        }
        // The denials, named explicitly so the rule reads
        // as policy, not as a table accident.
        assert!(!store_role_can("manager", "manage_members"));
        assert!(!store_role_can("manager", "manage_payouts"));
        assert!(!store_role_can("manager", "manage_verification"));
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
            gift_card_discount_poisha: 0,
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
            gift_card_discount_poisha: 0,
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
            gift_card_discount_poisha: 0,
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
        // requested -> rejected. The admin payouts remake adds
        // approved -> rejected: the escape hatch that releases
        // a committed amount whose coverage later disappears.
        assert!(can_transition_payout("requested", "approved"));
        assert!(can_transition_payout("requested", "rejected"));
        assert!(can_transition_payout("approved", "paid"));
        assert!(can_transition_payout("approved", "rejected"));
        // Everything else is illegal: skips, backwards moves,
        // same-state, and anything out of the final states.
        assert!(!can_transition_payout("requested", "paid"));
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
    fn payout_rejection_reason_is_required_trimmed_and_capped() {
        assert!(validate_payout_rejection_reason("").is_err());
        assert!(validate_payout_rejection_reason("   ").is_err());
        assert_eq!(
            validate_payout_rejection_reason("  অ্যাকাউন্ট নম্বর ভুল  ").unwrap(),
            "অ্যাকাউন্ট নম্বর ভুল"
        );
        // The cap counts Unicode characters, not bytes: 500
        // Bangla characters pass, 501 fail.
        assert!(validate_payout_rejection_reason(&"ক".repeat(500)).is_ok());
        assert!(validate_payout_rejection_reason(&"ক".repeat(501)).is_err());
        assert_eq!(
            validate_payout_rejection_reason(""),
            Err("a payout rejection reason is required")
        );
    }

    #[test]
    fn payment_reference_validation_rules() {
        // Blank and too-short references are not proof.
        assert!(validate_payment_reference("").is_err());
        assert!(validate_payment_reference("   ").is_err());
        assert!(validate_payment_reference("AB1").is_err());
        // A bKash-shaped TrxID passes unchanged.
        assert_eq!(
            validate_payment_reference("BKASH9X2KLM4").unwrap(),
            "BKASH9X2KLM4"
        );
        // Trimmed at the edges, verbatim inside: bank
        // references carry spaces, slashes and hyphens.
        assert_eq!(
            validate_payment_reference("  FT 2026/11-28.09  ").unwrap(),
            "FT 2026/11-28.09"
        );
        // The cap is 128 characters.
        assert!(validate_payment_reference(&"A".repeat(128)).is_ok());
        assert!(validate_payment_reference(&"A".repeat(129)).is_err());
        // Control characters and non-ASCII are refused.
        assert!(validate_payment_reference("ABC\nDEF1").is_err());
        assert!(validate_payment_reference("ABC\tDEF1").is_err());
        assert!(validate_payment_reference("রেফারেন্স1234").is_err());
    }

    #[test]
    fn payout_list_sort_validation() {
        assert_eq!(validate_payout_list_sort(None), Ok(PayoutSort::Newest));
        assert_eq!(
            validate_payout_list_sort(Some("newest")),
            Ok(PayoutSort::Newest)
        );
        assert_eq!(
            validate_payout_list_sort(Some("oldest")),
            Ok(PayoutSort::Oldest)
        );
        assert_eq!(
            validate_payout_list_sort(Some("amount_desc")),
            Ok(PayoutSort::AmountDesc)
        );
        assert_eq!(
            validate_payout_list_sort(Some("amount_asc")),
            Ok(PayoutSort::AmountAsc)
        );
        assert_eq!(
            validate_payout_list_sort(Some("")),
            Err("sort must be one of: newest, oldest, amount_desc, amount_asc")
        );
        assert!(validate_payout_list_sort(Some("largest")).is_err());
        assert!(validate_payout_list_sort(Some("AmountDesc")).is_err());
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
    fn browse_filters_defaults_and_core_rules() {
        // An empty input is a valid unfiltered browse: no
        // text, newest-first (the core's no-text default),
        // page 1 at the browse page size.
        let filters = validate_browse_filters(BrowseFilterInput::default())
            .expect("empty input is valid");
        assert_eq!(filters.q, None);
        assert_eq!(filters.sort, SearchSort::Newest);
        assert_eq!(filters.page, 1);
        assert_eq!(filters.per_page, DEFAULT_BROWSE_LIMIT);
        assert!(filters.categories.is_empty());
        assert!(filters.store_slugs.is_empty());
        assert_eq!(filters.min_rating, None);
        assert!(!filters.in_stock);
        assert!(!filters.on_sale);

        // Text present -> relevance default, and the Wave 15
        // core still judges the shared fields (bad sort,
        // inverted prices, page 0 all stay 400s).
        let with_q = validate_browse_filters(BrowseFilterInput {
            q: Some("  shirt ".to_string()),
            ..BrowseFilterInput::default()
        })
        .expect("text is valid");
        assert_eq!(with_q.q.as_deref(), Some("shirt"));
        assert_eq!(with_q.sort, SearchSort::Relevance);
        assert!(validate_browse_filters(BrowseFilterInput {
            sort: Some("banana".to_string()),
            ..BrowseFilterInput::default()
        })
        .is_err());
        assert!(validate_browse_filters(BrowseFilterInput {
            min_price_poisha: Some(1_000),
            max_price_poisha: Some(999),
            ..BrowseFilterInput::default()
        })
        .is_err());
        assert!(validate_browse_filters(BrowseFilterInput {
            page: Some(0),
            ..BrowseFilterInput::default()
        })
        .is_err());
    }

    #[test]
    fn browse_filters_limit_spelling_and_bounds() {
        // `limit` wins over the legacy `per_page` when both
        // arrive, and it answers to the browse bounds
        // (1..=60), not the legacy 48 cap.
        let both = validate_browse_filters(BrowseFilterInput {
            per_page: Some(10),
            limit: Some(50),
            ..BrowseFilterInput::default()
        })
        .expect("limit above the legacy cap is fine");
        assert_eq!(both.per_page, 50);
        let legacy = validate_browse_filters(BrowseFilterInput {
            per_page: Some(48),
            ..BrowseFilterInput::default()
        })
        .expect("legacy spelling at its cap");
        assert_eq!(legacy.per_page, 48);
        assert!(validate_browse_filters(BrowseFilterInput {
            per_page: Some(49),
            ..BrowseFilterInput::default()
        })
        .is_err());
        assert!(validate_browse_filters(BrowseFilterInput {
            limit: Some(0),
            ..BrowseFilterInput::default()
        })
        .is_err());
        assert!(validate_browse_filters(BrowseFilterInput {
            limit: Some(MAX_BROWSE_LIMIT + 1),
            ..BrowseFilterInput::default()
        })
        .is_err());
        assert!(validate_browse_filters(BrowseFilterInput {
            limit: Some(MAX_BROWSE_LIMIT),
            ..BrowseFilterInput::default()
        })
        .is_ok());
    }

    #[test]
    fn browse_filters_price_ceiling() {
        // At the ceiling is fine; one poisha above is a 400.
        assert!(validate_browse_filters(BrowseFilterInput {
            max_price_poisha: Some(MAX_PRICE_FILTER_POISHA),
            ..BrowseFilterInput::default()
        })
        .is_ok());
        assert!(validate_browse_filters(BrowseFilterInput {
            max_price_poisha: Some(MAX_PRICE_FILTER_POISHA + 1),
            ..BrowseFilterInput::default()
        })
        .is_err());
        assert!(validate_browse_filters(BrowseFilterInput {
            min_price_poisha: Some(MAX_PRICE_FILTER_POISHA + 1),
            ..BrowseFilterInput::default()
        })
        .is_err());
    }

    #[test]
    fn browse_filters_rating_and_bool_flags() {
        let rated = validate_browse_filters(BrowseFilterInput {
            min_rating: Some(4.5),
            in_stock: Some("true".to_string()),
            on_sale: Some("1".to_string()),
            ..BrowseFilterInput::default()
        })
        .expect("valid rating and flags");
        assert_eq!(rated.min_rating, Some(4.5));
        assert!(rated.in_stock);
        assert!(rated.on_sale);
        let unrated = validate_browse_filters(BrowseFilterInput {
            min_rating: Some(0.0),
            in_stock: Some("false".to_string()),
            on_sale: Some("0".to_string()),
            ..BrowseFilterInput::default()
        })
        .expect("zero rating and false flags are valid");
        assert_eq!(unrated.min_rating, Some(0.0));
        assert!(!unrated.in_stock);
        // Out of range, NaN and infinities are all refused;
        // a mistyped flag is refused rather than guessed.
        for bad in [5.1, -0.1, f64::NAN, f64::INFINITY] {
            assert!(
                validate_browse_filters(BrowseFilterInput {
                    min_rating: Some(bad),
                    ..BrowseFilterInput::default()
                })
                .is_err(),
                "rating {bad} refused"
            );
        }
        assert!(validate_browse_filters(BrowseFilterInput {
            in_stock: Some("yes".to_string()),
            ..BrowseFilterInput::default()
        })
        .is_err());
        assert_eq!(parse_filter_bool(None, "in_stock"), Ok(false));
        assert_eq!(parse_filter_bool(Some("true"), "in_stock"), Ok(true));
    }

    #[test]
    fn browse_filters_multi_value_normalisation() {
        // Repeated parameters, comma-joined values, stray
        // blanks and duplicates all normalise to one clean
        // OR list — deduplicated and SORTED, so the stored
        // form and the analytics fingerprint are canonical
        // regardless of the order the values arrived in.
        let filters = validate_browse_filters(BrowseFilterInput {
            categories: vec![
                " clothing , shoes ".to_string(),
                "clothing".to_string(),
                "".to_string(),
                "electronics".to_string(),
            ],
            store_slugs: vec!["alpha,beta".to_string(), "alpha".to_string()],
            ..BrowseFilterInput::default()
        })
        .expect("normalisable lists");
        assert_eq!(filters.categories, vec!["clothing", "electronics", "shoes"]);
        assert_eq!(filters.store_slugs, vec!["alpha", "beta"]);

        // The value cap and the per-value length cap bite.
        let too_many: Vec<String> = (0..=MAX_FILTER_VALUES)
            .map(|n| format!("cat{n}"))
            .collect();
        assert!(validate_browse_filters(BrowseFilterInput {
            categories: too_many,
            ..BrowseFilterInput::default()
        })
        .is_err());
        assert!(validate_browse_filters(BrowseFilterInput {
            categories: vec!["x".repeat(MAX_CATEGORY_FILTER_CHARS + 1)],
            ..BrowseFilterInput::default()
        })
        .is_err());
        assert!(validate_browse_filters(BrowseFilterInput {
            store_slugs: vec!["x".repeat(MAX_STORE_FILTER_CHARS + 1)],
            ..BrowseFilterInput::default()
        })
        .is_err());
    }

    #[test]
    fn browse_filters_sold_sort_and_fingerprint() {
        // The new sort parses through the core path, and
        // its key round-trips.
        let filters = validate_browse_filters(BrowseFilterInput {
            sort: Some("sold".to_string()),
            ..BrowseFilterInput::default()
        })
        .expect("sold sort is valid");
        assert_eq!(filters.sort, SearchSort::Sold);
        for sort in [
            SearchSort::Relevance,
            SearchSort::PriceAsc,
            SearchSort::PriceDesc,
            SearchSort::Newest,
            SearchSort::Rating,
            SearchSort::Sold,
        ] {
            let parsed = validate_search_params(
                None,
                None,
                None,
                Some(search_sort_key(sort)),
                None,
                None,
            )
            .expect("sort key parses");
            assert_eq!(parsed.sort, sort);
        }

        // The fingerprint is stable for equal filter sets
        // and moves when any component moves.
        let base = validate_browse_filters(BrowseFilterInput {
            q: Some("shirt".to_string()),
            categories: vec!["clothing".to_string()],
            ..BrowseFilterInput::default()
        })
        .expect("base filters");
        let same = validate_browse_filters(BrowseFilterInput {
            q: Some("shirt".to_string()),
            categories: vec!["clothing".to_string()],
            ..BrowseFilterInput::default()
        })
        .expect("same filters");
        assert_eq!(
            search_filters_fingerprint(&base),
            search_filters_fingerprint(&same)
        );
        let moved = validate_browse_filters(BrowseFilterInput {
            q: Some("shirt".to_string()),
            categories: vec!["clothing".to_string()],
            in_stock: Some("true".to_string()),
            ..BrowseFilterInput::default()
        })
        .expect("moved filters");
        assert_ne!(
            search_filters_fingerprint(&base),
            search_filters_fingerprint(&moved)
        );
        // The list-sort validator: relevance degrades to the
        // list default, the rest of the vocabulary maps.
        assert_eq!(validate_product_list_sort(None), Ok(SearchSort::Newest));
        assert_eq!(
            validate_product_list_sort(Some("relevance")),
            Ok(SearchSort::Newest)
        );
        assert_eq!(
            validate_product_list_sort(Some("sold")),
            Ok(SearchSort::Sold)
        );
        assert!(validate_product_list_sort(Some("banana")).is_err());
    }

    #[test]
    fn browse_fingerprint_ignores_input_order_and_duplicates() {
        // The analytics grouping property: the same browse
        // spelled differently — values reordered, repeated,
        // comma-joined — is ONE filter set with ONE
        // fingerprint.
        let first = validate_browse_filters(BrowseFilterInput {
            categories: vec!["clothing".to_string(), "footwear".to_string()],
            store_slugs: vec!["alpha".to_string(), "beta".to_string()],
            ..BrowseFilterInput::default()
        })
        .expect("first spelling");
        let second = validate_browse_filters(BrowseFilterInput {
            categories: vec!["footwear,clothing".to_string(), "clothing".to_string()],
            store_slugs: vec!["beta".to_string(), "alpha".to_string()],
            ..BrowseFilterInput::default()
        })
        .expect("second spelling");
        assert_eq!(
            search_filters_fingerprint(&first),
            search_filters_fingerprint(&second),
            "canonicalisation precedes fingerprinting"
        );
        // And the canonical lists themselves are the
        // sorted, deduplicated sets.
        assert_eq!(first.categories, vec!["clothing", "footwear"]);
        assert_eq!(second.categories, vec!["clothing", "footwear"]);
        assert_eq!(first.store_slugs, vec!["alpha", "beta"]);
        assert_eq!(second.store_slugs, vec!["alpha", "beta"]);
    }

    #[test]
    fn browse_filters_value_caps() {
        // More distinct values than the cap is a 400, not a
        // silent truncation — the shopper's filter must be
        // the filter that ran.
        let many: Vec<String> = (0..=MAX_FILTER_VALUES)
            .map(|index| format!("cat-{index}"))
            .collect();
        assert!(validate_browse_filters(BrowseFilterInput {
            categories: many,
            ..BrowseFilterInput::default()
        })
        .is_err());
        // A value longer than the label cap is a 400.
        assert!(validate_browse_filters(BrowseFilterInput {
            store_slugs: vec!["x".repeat(MAX_STORE_FILTER_CHARS + 1)],
            ..BrowseFilterInput::default()
        })
        .is_err());
        // Whitespace-only values drop out entirely.
        let filters = validate_browse_filters(BrowseFilterInput {
            categories: vec!["  ".to_string(), " clothing ".to_string()],
            ..BrowseFilterInput::default()
        })
        .expect("blanks drop");
        assert_eq!(filters.categories, vec!["clothing"]);
    }

    #[test]
    fn saved_search_name_rules() {
        assert_eq!(
            validate_saved_search_name("  Eid shoes  "),
            Ok("Eid shoes".to_string())
        );
        assert!(validate_saved_search_name("").is_err());
        assert!(validate_saved_search_name("   ").is_err());
        assert!(validate_saved_search_name(
            &"x".repeat(MAX_SAVED_SEARCH_NAME_CHARS + 1)
        )
        .is_err());
        assert!(validate_saved_search_name(
            &"ক".repeat(MAX_SAVED_SEARCH_NAME_CHARS)
        )
        .is_ok());
    }

    #[test]
    fn price_histogram_ranges_cover_the_span() {
        // The catalogue span of the wave's own tests:
        // contiguous, non-overlapping, first lo = min,
        // last hi = max + 1 (so the dearest product counts).
        let ranges = price_histogram_ranges(20_000, 300_000);
        assert_eq!(ranges.len(), PRICE_HISTOGRAM_BUCKETS);
        assert_eq!(ranges[0].0, 20_000);
        assert_eq!(ranges[ranges.len() - 1].1, 300_001);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].1, pair[1].0, "ranges are contiguous");
            assert!(pair[0].0 < pair[0].1, "no empty range");
        }
        // Every price in the span falls in exactly one range.
        for price in [20_000, 50_000, 150_000, 299_999, 300_000] {
            let hits = ranges
                .iter()
                .filter(|(lo, hi)| price >= *lo && price < *hi)
                .count();
            assert_eq!(hits, 1, "price {price} in exactly one range");
        }
        // A single price is one range; a tiny span yields
        // one range per poisha, never an empty range.
        assert_eq!(price_histogram_ranges(500, 500), vec![(500, 501)]);
        let tiny = price_histogram_ranges(0, 3);
        assert_eq!(tiny, vec![(0, 1), (1, 2), (2, 3), (3, 4)]);
    }

    #[test]
    fn price_histogram_ranges_small_spans() {
        // A span of exactly the bucket count: one poisha
        // per range.
        let ranges = price_histogram_ranges(0, 7);
        assert_eq!(ranges.len(), 8);
        assert_eq!(ranges[7], (7, 8));
        // A span one wider than the bucket count: the last
        // range absorbs the remainder and still ends at
        // max + 1.
        let ranges = price_histogram_ranges(0, 8);
        assert_eq!(ranges.len(), 8);
        assert_eq!(ranges[7], (7, 9));
        // A large span divides evenly-ish and never loses
        // the top price.
        let ranges = price_histogram_ranges(0, 79_999);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[ranges.len() - 1].1, 80_000);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].1, pair[1].0);
        }
    }

    #[test]
    fn browse_fingerprint_distinguishes_real_changes() {
        // The fingerprint is the analytics grouping key:
        // two browses that differ in a real filter must
        // land in different groups, or the operator's
        // combination counts would merge unlike searches.
        let base = validate_browse_filters(BrowseFilterInput {
            q: Some("shirt".to_string()),
            ..BrowseFilterInput::default()
        })
        .expect("base");
        let sorted = validate_browse_filters(BrowseFilterInput {
            q: Some("shirt".to_string()),
            sort: Some("price_asc".to_string()),
            ..BrowseFilterInput::default()
        })
        .expect("sorted");
        let stocked = validate_browse_filters(BrowseFilterInput {
            q: Some("shirt".to_string()),
            in_stock: Some("true".to_string()),
            ..BrowseFilterInput::default()
        })
        .expect("stocked");
        let base_hash = search_filters_fingerprint(&base);
        assert_ne!(base_hash, search_filters_fingerprint(&sorted));
        assert_ne!(base_hash, search_filters_fingerprint(&stocked));
        assert_ne!(
            search_filters_fingerprint(&sorted),
            search_filters_fingerprint(&stocked)
        );
    }

    #[test]
    fn browse_bool_flag_spellings() {
        // The accepted spellings are the two query-string
        // conventions; anything else is a 400, never a
        // silent false.
        let filters = validate_browse_filters(BrowseFilterInput {
            in_stock: Some("1".to_string()),
            on_sale: Some("true".to_string()),
            ..BrowseFilterInput::default()
        })
        .expect("1/true parse");
        assert!(filters.in_stock);
        assert!(filters.on_sale);
        let filters = validate_browse_filters(BrowseFilterInput {
            in_stock: Some("0".to_string()),
            on_sale: Some("false".to_string()),
            ..BrowseFilterInput::default()
        })
        .expect("0/false parse");
        assert!(!filters.in_stock);
        assert!(!filters.on_sale);
        assert!(validate_browse_filters(BrowseFilterInput {
            in_stock: Some("perhaps".to_string()),
            ..BrowseFilterInput::default()
        })
        .is_err());
    }

    #[test]
    fn product_list_sort_vocabulary() {
        // The whole browse vocabulary maps for the plain
        // list; relevance degrades to the list's default
        // (the list has no text rank of its own).
        assert_eq!(
            validate_product_list_sort(Some("price_asc")),
            Ok(SearchSort::PriceAsc)
        );
        assert_eq!(
            validate_product_list_sort(Some("price_desc")),
            Ok(SearchSort::PriceDesc)
        );
        assert_eq!(
            validate_product_list_sort(Some("newest")),
            Ok(SearchSort::Newest)
        );
        assert_eq!(
            validate_product_list_sort(Some("rating")),
            Ok(SearchSort::Rating)
        );
        assert!(validate_product_list_sort(Some("")).is_err());
        assert!(validate_product_list_sort(Some("cheapest")).is_err());
    }

    #[test]
    fn analytics_window_days_rules() {
        assert_eq!(validate_analytics_window_days(None), Ok(30));
        assert_eq!(validate_analytics_window_days(Some(1)), Ok(1));
        assert_eq!(validate_analytics_window_days(Some(90)), Ok(90));
        assert!(validate_analytics_window_days(Some(0)).is_err());
        assert!(validate_analytics_window_days(Some(91)).is_err());
        assert!(validate_analytics_window_days(Some(-3)).is_err());
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
        // Bengali digits are transliterated to ASCII before
        // the check (admin verifications remake — a policy
        // change from the original reject-never-guess rule):
        // the conversion is a deterministic 1:1 map, and the
        // RETURNED (stored) form is always the ASCII string.
        assert_eq!(validate_nid_number("১২৩৪৫৬৭৮৯০").unwrap(), "1234567890");
        // Mixed ASCII/Bengali input canonicalises the same way.
        assert_eq!(validate_nid_number("১২৩৪567890").unwrap(), "1234567890");
        // Other digit scripts are NOT converted and still fail.
        assert!(validate_nid_number("١٢٣٤٥٦٧٨٩٠").is_err());
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
    fn mask_nid_parts_matches_mask_nid() {
        // The parts form must reproduce mask_nid exactly for
        // every length the mask is defined on — the masked
        // read paths now hold only (length, last4).
        assert_eq!(mask_nid_parts(13, "0123"), mask_nid("1234567890123"));
        assert_eq!(mask_nid_parts(10, "7890"), mask_nid("1234567890"));
        assert_eq!(mask_nid_parts(17, "4567"), mask_nid("12345678901234567"));
        assert_eq!(mask_nid_parts(5, "2345"), "*2345");
        // Short inputs mask everything and ignore last4 — a
        // malformed short row must not leak itself.
        assert_eq!(mask_nid_parts(4, "1234"), "****");
        assert_eq!(mask_nid_parts(3, "123"), "***");
        assert_eq!(mask_nid_parts(0, ""), "");
    }

    #[test]
    fn view_reason_must_be_meaningful_and_number_free() {
        assert_eq!(
            validate_view_reason("  buyer dispute check  ").unwrap(),
            "buyer dispute check"
        );
        assert!(validate_view_reason("").is_err());
        assert!(validate_view_reason("   ").is_err());
        assert!(validate_view_reason("ok").is_err()); // too short to audit
        // A pasted NID (ASCII or Bengali digits) is refused:
        // the audit detail must never become a second copy.
        assert!(validate_view_reason("checking 1234567890 against doc").is_err());
        assert!(validate_view_reason("checking ১২৩৪৫৬৭৮৯০ against doc").is_err());
        // Short digit runs (an order number fragment) are fine.
        assert!(validate_view_reason("dispute 4521 re-check").is_ok());
    }

    #[test]
    fn applicant_name_is_optional_and_capped() {
        assert_eq!(validate_applicant_name(None).unwrap(), None);
        assert_eq!(validate_applicant_name(Some("   ")).unwrap(), None);
        assert_eq!(
            validate_applicant_name(Some("  মোঃ রফিকুল ইসলাম ")).unwrap(),
            Some("মোঃ রফিকুল ইসলাম".to_string())
        );
        let too_long = "a".repeat(MAX_APPLICANT_NAME_CHARS + 1);
        assert!(validate_applicant_name(Some(&too_long)).is_err());
    }

    #[test]
    fn verification_notification_bodies_are_honest() {
        let mut vars = NotificationVars {
            verification_tier: "basic".to_string(),
            ..NotificationVars::default()
        };
        let approved = render_body(NOTIFICATION_VERIFICATION_APPROVED, &vars);
        assert!(approved.contains("অনুমোদিত হয়েছে"), "{approved}");
        assert!(approved.contains("যাচাইকৃত বিক্রেতা"), "{approved}");
        // The body must never claim a government/database
        // check — review is manual, by the platform team.
        assert!(approved.contains("প্ল্যাটফর্ম দলের পর্যালোচনায়"), "{approved}");
        assert!(!approved.contains("সরকার"), "{approved}");

        vars.verification_tier = "verified".to_string();
        let approved_verified = render_body(NOTIFICATION_VERIFICATION_APPROVED, &vars);
        assert!(approved_verified.contains("যাচাইকৃত ব্যবসা"), "{approved_verified}");

        // An unknown tier omits the tier phrase rather than
        // printing a label the seller does not hold.
        vars.verification_tier = String::new();
        let approved_bare = render_body(NOTIFICATION_VERIFICATION_APPROVED, &vars);
        assert!(!approved_bare.contains("যাচাইকৃত বিক্রেতা"), "{approved_bare}");
        assert!(!approved_bare.contains("যাচাইকৃত ব্যবসা"), "{approved_bare}");

        // Rejection carries the note (truncated past 120
        // chars) and the fix-and-resubmit path.
        vars.note = "ছবি ঝাপসা".to_string();
        let rejected = render_body(NOTIFICATION_VERIFICATION_REJECTED, &vars);
        assert!(rejected.contains("গৃহীত হয়নি"), "{rejected}");
        assert!(rejected.contains("ছবি ঝাপসা"), "{rejected}");
        assert!(rejected.contains("আবার আবেদন করুন"), "{rejected}");
        vars.note = "x".repeat(300);
        let rejected_long = render_body(NOTIFICATION_VERIFICATION_REJECTED, &vars);
        assert!(rejected_long.contains("…"), "{rejected_long}");
        assert!(rejected_long.chars().count() < 300, "{rejected_long}");
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
    fn suspension_note_is_required_trimmed_and_capped() {
        assert_eq!(
            validate_suspension_note("  জালিয়াতির অভিযোগ  ").unwrap(),
            "জালিয়াতির অভিযোগ"
        );
        assert!(validate_suspension_note("").is_err());
        assert!(validate_suspension_note("   ").is_err());
        assert!(validate_suspension_note(&"x".repeat(MAX_SUSPENSION_NOTE_CHARS)).is_ok());
        assert!(validate_suspension_note(&"x".repeat(MAX_SUSPENSION_NOTE_CHARS + 1)).is_err());
    }

    #[test]
    fn store_suspension_notification_bodies_are_honest() {
        let mut vars = NotificationVars {
            store_name: "ঢাকা ফ্যাশন".to_string(),
            note: "জালিয়াতির অভিযোগ তদন্তাধীন".to_string(),
            ..NotificationVars::default()
        };
        let suspended = render_body(NOTIFICATION_STORE_SUSPENDED, &vars);
        // Temporary, with the reason — and never a word
        // that implies deletion or permanent closure.
        assert!(suspended.contains("সাময়িকভাবে স্থগিত"), "{suspended}");
        assert!(suspended.contains("জালিয়াতির অভিযোগ তদন্তাধীন"), "{suspended}");
        assert!(!suspended.contains("মুছে"), "{suspended}");
        assert!(!suspended.contains("স্থায়ী"), "{suspended}");
        assert!(!suspended.contains("বন্ধ করে দেওয়া"), "{suspended}");

        // A long note truncates at 120 chars with an
        // ellipsis and points at the dashboard.
        vars.note = "x".repeat(200);
        let truncated = render_body(NOTIFICATION_STORE_SUSPENDED, &vars);
        assert!(truncated.contains("…"), "{truncated}");
        assert!(truncated.contains("ড্যাশবোর্ডে"), "{truncated}");

        // Reactivation announces selling again; it takes
        // no note and claims nothing else.
        let reactivated = render_body(NOTIFICATION_STORE_REACTIVATED, &vars);
        assert!(reactivated.contains("আবার চালু করা হয়েছে"), "{reactivated}");
        assert!(reactivated.contains("বিক্রি করতে পারবেন"), "{reactivated}");
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
            product_title: String::new(),
            sale_title: String::new(),
            post_title: String::new(),
            card_last4: String::new(),
            note: String::new(),
            payment_reference: String::new(),
            verification_tier: String::new(),
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
        assert!(!payout.contains("রেফারেন্স"), "{payout}");

        // Admin payouts remake: a recorded reference rides the
        // paid body so the seller can match the credit.
        let mut paid_vars = vars.clone();
        paid_vars.payment_reference = "BKASH9X2KLM4".to_string();
        let paid_ref = render_body(NOTIFICATION_PAYOUT_PAID, &paid_vars);
        assert!(paid_ref.contains("রেফারেন্স: BKASH9X2KLM4"), "{paid_ref}");

        // The approved body announces approval only — it must
        // never claim the money was sent.
        let approved_payout = render_body(NOTIFICATION_PAYOUT_APPROVED, &vars);
        assert!(approved_payout.contains("অনুমোদিত হয়েছে"), "{approved_payout}");
        assert!(!approved_payout.contains("পাঠানো হয়েছে"), "{approved_payout}");

        // The rejected body carries the reason and the
        // balance-release fact; without a reason it still
        // renders both facts.
        let mut rejected_vars = vars.clone();
        rejected_vars.note = "অ্যাকাউন্ট নম্বর মিলছে না".to_string();
        let rejected_payout = render_body(NOTIFICATION_PAYOUT_REJECTED, &rejected_vars);
        assert!(rejected_payout.contains("গৃহীত হয়নি"), "{rejected_payout}");
        assert!(
            rejected_payout.contains("কারণ: অ্যাকাউন্ট নম্বর মিলছে না"),
            "{rejected_payout}"
        );
        assert!(rejected_payout.contains("ফেরত গেছে"), "{rejected_payout}");
        let rejected_no_reason = render_body(NOTIFICATION_PAYOUT_REJECTED, &vars);
        assert!(rejected_no_reason.contains("গৃহীত হয়নি"), "{rejected_no_reason}");
        assert!(!rejected_no_reason.contains("কারণ:"), "{rejected_no_reason}");

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

        // Wave 54: the follower notice names the store and,
        // when the payload carries it, the product itself.
        let mut follow_vars = vars.clone();
        follow_vars.product_title = "নতুন স্মার্টওয়াচ".to_string();
        let with_title = render_body(NOTIFICATION_STORE_NEW_PRODUCT, &follow_vars);
        assert!(with_title.contains("ঢাকা গ্যাজেট"), "{with_title}");
        assert!(with_title.contains("নতুন স্মার্টওয়াচ"), "{with_title}");
        let without_title = render_body(NOTIFICATION_STORE_NEW_PRODUCT, &vars);
        assert!(without_title.contains("নতুন পণ্য যোগ করেছে"), "{without_title}");

        // The flash-sale body names the store and, when
        // known, the sale title.
        let mut sale_vars = vars.clone();
        sale_vars.sale_title = "মেগা সেল".to_string();
        let with_sale = render_body(NOTIFICATION_STORE_FLASH_SALE, &sale_vars);
        assert!(with_sale.contains("ঢাকা গ্যাজেট"), "{with_sale}");
        assert!(with_sale.contains("মেগা সেল"), "{with_sale}");
        let without_sale = render_body(NOTIFICATION_STORE_FLASH_SALE, &vars);
        assert!(without_sale.contains("ফ্ল্যাশ সেল চলছে"), "{without_sale}");
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

    // -- Wave 54: store follows -----------------------------------------

    #[test]
    fn follow_rules_refuse_only_the_owner() {
        assert!(!can_follow_store(true));
        assert!(can_follow_store(false));
        // The refusal wording is bilingual, like every
        // buyer-facing refusal in the API.
        assert!(SELF_FOLLOW_ERROR.contains("নিজের স্টোর"));
    }

    #[test]
    fn follow_count_cap_bites_at_the_limit() {
        assert!(validate_follow_count(0).is_ok());
        assert!(validate_follow_count(MAX_FOLLOWED_STORES - 1).is_ok());
        assert!(validate_follow_count(MAX_FOLLOWED_STORES).is_err());
        assert!(validate_follow_count(MAX_FOLLOWED_STORES + 1).is_err());
        assert!(validate_follow_count(-1).is_err());
    }

    // -- Wave 54: store profile texts -----------------------------------

    #[test]
    fn store_profile_texts_trim_cap_and_clear() {
        // Absent and blank both clear to None.
        assert_eq!(validate_store_about(None).unwrap(), None);
        assert_eq!(validate_store_about(Some("   ")).unwrap(), None);
        assert_eq!(validate_store_return_policy(None).unwrap(), None);
        // A present value is trimmed and kept.
        assert_eq!(
            validate_store_about(Some("  আমরা ঢাকার কারিগর।  ")).unwrap(),
            Some("আমরা ঢাকার কারিগর।".to_string())
        );
        assert_eq!(
            validate_store_return_policy(Some("৭ দিনে ফেরত")).unwrap(),
            Some("৭ দিনে ফেরত".to_string())
        );
        // The caps count characters, not bytes: 2,000 Bangla
        // characters pass, 2,001 fail, and the error names
        // the field that overflowed.
        let at_cap = "ক".repeat(MAX_STORE_ABOUT_CHARS);
        assert!(validate_store_about(Some(&at_cap)).is_ok());
        let over_cap = "ক".repeat(MAX_STORE_ABOUT_CHARS + 1);
        assert_eq!(
            validate_store_about(Some(&over_cap)).unwrap_err(),
            "about must be at most 2000 characters"
        );
        let over_policy = "ক".repeat(MAX_STORE_RETURN_POLICY_CHARS + 1);
        assert_eq!(
            validate_store_return_policy(Some(&over_policy)).unwrap_err(),
            "return policy must be at most 2000 characters"
        );
    }

    // -- Wave 54: delivery-time estimates -------------------------------

    #[test]
    fn delivery_day_defaults_match_the_published_zones() {
        assert_eq!(default_delivery_day_range(DeliveryZone::InsideDhaka), (1, 2));
        assert_eq!(default_delivery_day_range(DeliveryZone::DhakaSuburb), (2, 3));
        assert_eq!(default_delivery_day_range(DeliveryZone::OutsideDhaka), (3, 5));
        // Every zone has its own settings key.
        assert_eq!(
            delivery_days_setting_key(DeliveryZone::InsideDhaka),
            DELIVERY_DAYS_INSIDE_DHAKA_KEY
        );
        assert_eq!(
            delivery_days_setting_key(DeliveryZone::DhakaSuburb),
            DELIVERY_DAYS_DHAKA_SUBURB_KEY
        );
        assert_eq!(
            delivery_days_setting_key(DeliveryZone::OutsideDhaka),
            DELIVERY_DAYS_OUTSIDE_DHAKA_KEY
        );
    }

    #[test]
    fn delivery_day_range_validation() {
        assert_eq!(validate_delivery_day_range(1, 2).unwrap(), (1, 2));
        // Same-day (0) is a legitimate promise.
        assert_eq!(validate_delivery_day_range(0, 0).unwrap(), (0, 0));
        assert_eq!(
            validate_delivery_day_range(0, MAX_DELIVERY_ESTIMATE_DAYS).unwrap(),
            (0, MAX_DELIVERY_ESTIMATE_DAYS)
        );
        // Inverted, negative and absurd ranges all refuse.
        assert!(validate_delivery_day_range(3, 2).is_err());
        assert!(validate_delivery_day_range(-1, 2).is_err());
        assert!(validate_delivery_day_range(1, MAX_DELIVERY_ESTIMATE_DAYS + 1).is_err());
    }

    #[test]
    fn delivery_day_range_storage_round_trips() {
        assert_eq!(format_delivery_day_range(1, 2), "1-2");
        assert_eq!(format_delivery_day_range(0, 30), "0-30");
        assert_eq!(parse_delivery_day_range("1-2"), Some((1, 2)));
        assert_eq!(parse_delivery_day_range(" 3 - 5 "), Some((3, 5)));
        // Garbage, inverted and out-of-range stored values
        // all read as "no override".
        assert_eq!(parse_delivery_day_range(""), None);
        assert_eq!(parse_delivery_day_range("soon"), None);
        assert_eq!(parse_delivery_day_range("5-3"), None);
        assert_eq!(parse_delivery_day_range("1-99"), None);
        assert_eq!(parse_delivery_day_range("1-2-3"), None);
        assert_eq!(parse_delivery_day_range("-1-2"), None);
    }

    #[test]
    fn delivery_day_resolution_prefers_a_valid_stored_range() {
        let zone = DeliveryZone::OutsideDhaka;
        assert_eq!(resolve_delivery_day_range(zone, Some("2-4")), (2, 4));
        assert_eq!(resolve_delivery_day_range(zone, None), (3, 5));
        assert_eq!(resolve_delivery_day_range(zone, Some("garbage")), (3, 5));
        assert_eq!(resolve_delivery_day_range(zone, Some("9-1")), (3, 5));
    }

    // -- Wave 54: review sorts + rating shares --------------------------

    #[test]
    fn review_sort_validation() {
        assert_eq!(validate_review_sort(None), Ok(ReviewSort::Newest));
        assert_eq!(validate_review_sort(Some("newest")), Ok(ReviewSort::Newest));
        assert_eq!(validate_review_sort(Some("highest")), Ok(ReviewSort::Highest));
        assert_eq!(validate_review_sort(Some("lowest")), Ok(ReviewSort::Lowest));
        assert_eq!(validate_review_sort(Some("photos")), Ok(ReviewSort::Photos));
        assert!(validate_review_sort(Some("banana")).is_err());
        assert!(validate_review_sort(Some("")).is_err());
    }

    #[test]
    fn rating_share_percent_floors_and_never_divides_by_zero() {
        assert_eq!(rating_share_percent(0, 0), 0);
        assert_eq!(rating_share_percent(5, 0), 0);
        assert_eq!(rating_share_percent(0, 10), 0);
        assert_eq!(rating_share_percent(10, 10), 100);
        assert_eq!(rating_share_percent(1, 3), 33);
        assert_eq!(rating_share_percent(2, 3), 66);
        // A corrupt count above the total clamps to the total.
        assert_eq!(rating_share_percent(15, 10), 100);
        assert_eq!(rating_share_percent(-1, 10), 0);
    }

    #[test]
    fn helpful_sort_and_own_review_vote_rules() {
        assert_eq!(
            validate_review_sort(Some("helpful")),
            Ok(ReviewSort::Helpful)
        );
        assert!(can_vote_review_helpful(false));
        assert!(!can_vote_review_helpful(true));
    }

    #[test]
    fn question_sort_accepts_only_its_two_orders() {
        assert_eq!(validate_question_sort(None), Ok(QuestionSort::Newest));
        assert_eq!(
            validate_question_sort(Some("answered")),
            Ok(QuestionSort::AnsweredFirst)
        );
        assert!(validate_question_sort(Some("oldest")).is_err());
    }

    #[test]
    fn store_collection_title_trims_caps_and_is_required() {
        assert_eq!(
            validate_store_collection_title("  ঈদ কালেকশন  "),
            Ok("ঈদ কালেকশন".to_string())
        );
        assert!(validate_store_collection_title("").is_err());
        assert!(validate_store_collection_title("   ").is_err());
        let at_cap = "a".repeat(MAX_STORE_COLLECTION_TITLE_CHARS);
        assert!(validate_store_collection_title(&at_cap).is_ok());
        let over_cap = "a".repeat(MAX_STORE_COLLECTION_TITLE_CHARS + 1);
        assert!(validate_store_collection_title(&over_cap).is_err());
    }

    #[test]
    fn store_collection_slug_uses_the_platform_grammar() {
        assert_eq!(
            validate_store_collection_slug("eid-2026"),
            Ok("eid-2026".to_string())
        );
        assert!(validate_store_collection_slug("Eid").is_err());
        assert!(validate_store_collection_slug("ab").is_err());
        assert!(validate_store_collection_slug("-eid").is_err());
        assert!(validate_store_collection_slug("eid_2026").is_err());
    }

    #[test]
    fn store_collection_description_blank_clears_and_caps() {
        assert_eq!(validate_store_collection_description(None), Ok(None));
        assert_eq!(validate_store_collection_description(Some("  ")), Ok(None));
        assert_eq!(
            validate_store_collection_description(Some("  হাতে বোনা  ")),
            Ok(Some("হাতে বোনা".to_string()))
        );
        let over_cap = "a".repeat(MAX_STORE_COLLECTION_DESCRIPTION_CHARS + 1);
        assert!(validate_store_collection_description(Some(&over_cap)).is_err());
    }

    #[test]
    fn store_collection_caps_are_bounds_not_targets() {
        assert!(validate_store_collection_count(0).is_ok());
        assert!(validate_store_collection_count(MAX_STORE_COLLECTIONS - 1).is_ok());
        assert!(validate_store_collection_count(MAX_STORE_COLLECTIONS).is_err());
        assert!(validate_store_collection_count(-1).is_err());
        assert!(validate_collection_product_count(0).is_ok());
        assert!(validate_collection_product_count(MAX_COLLECTION_PRODUCTS).is_ok());
        assert!(validate_collection_product_count(MAX_COLLECTION_PRODUCTS + 1).is_err());
        assert!(validate_collection_product_count(-1).is_err());
    }

    // ------------------------------------------------------------------
    // Wave 55: districts, addresses, order notes, reorder, tracking
    // ------------------------------------------------------------------

    #[test]
    fn district_table_has_64_unique_districts_all_zoned() {
        assert_eq!(DISTRICTS.len(), 64, "Bangladesh has 64 districts");
        let mut names: Vec<&str> = DISTRICTS.iter().map(|d| d.name_en).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 64, "district names are unique");
        // Every district resolves by its own name (any
        // case) and by its Bangla name.
        for district in DISTRICTS {
            let found = district_by_name(district.name_en)
                .expect("district resolves by English name");
            assert_eq!(found.name_en, district.name_en);
            assert_eq!(found.zone, district.zone);
            assert_eq!(
                district_by_name(&district.name_en.to_lowercase()).map(|d| d.name_en),
                Some(district.name_en),
                "lookup is case-insensitive"
            );
            assert_eq!(
                district_by_name(district.name_bn).map(|d| d.name_en),
                Some(district.name_en),
                "lookup accepts the Bangla name"
            );
        }
        assert!(district_by_name("Not A District").is_none());
        assert!(district_by_name("").is_none());
    }

    #[test]
    fn district_zone_policy_dhaka_ring_and_outside() {
        assert_eq!(zone_for_district("Dhaka"), Some(DeliveryZone::InsideDhaka));
        // The suburb ring is exactly the five districts
        // bordering Dhaka district.
        for name in ["Gazipur", "Narayanganj", "Munshiganj", "Narsingdi", "Manikganj"] {
            assert_eq!(
                zone_for_district(name),
                Some(DeliveryZone::DhakaSuburb),
                "{name} is in the suburb ring"
            );
        }
        let suburb_count = DISTRICTS
            .iter()
            .filter(|d| d.zone == DeliveryZone::DhakaSuburb)
            .count();
        assert_eq!(suburb_count, 5, "exactly five suburb districts");
        // Tangail does not border Dhaka district — outside.
        assert_eq!(zone_for_district("Tangail"), Some(DeliveryZone::OutsideDhaka));
        assert_eq!(zone_for_district("Khulna"), Some(DeliveryZone::OutsideDhaka));
        assert_eq!(zone_for_district("Sylhet"), Some(DeliveryZone::OutsideDhaka));
        assert_eq!(zone_for_district("Atlantis"), None);
    }

    fn sample_address_input() -> AddressInput {
        AddressInput {
            label: " বাসা ".to_string(),
            recipient_name: " Rahim Uddin ".to_string(),
            phone: "01712345678".to_string(),
            district: "dhaka".to_string(),
            area: " Mirpur 10 ".to_string(),
            address_line: " House 9, Road 3 ".to_string(),
        }
    }

    #[test]
    fn address_validation_trims_canonicalises_and_zones() {
        let validated = validate_address_input(&sample_address_input())
            .expect("the sample address is valid");
        assert_eq!(validated.label, "বাসা");
        assert_eq!(validated.recipient_name, "Rahim Uddin");
        // The phone lands in the canonical +880 form and
        // the district in its canonical spelling + zone.
        assert_eq!(validated.phone, "+8801712345678");
        assert_eq!(validated.district, "Dhaka");
        assert_eq!(validated.zone, DeliveryZone::InsideDhaka);
        assert_eq!(validated.area, "Mirpur 10");
        assert_eq!(validated.address_line, "House 9, Road 3");
        assert_eq!(
            compose_delivery_address(&validated.address_line, &validated.area, &validated.district),
            "House 9, Road 3, Mirpur 10, Dhaka"
        );

        // A suburb district derives the suburb zone.
        let mut suburb = sample_address_input();
        suburb.district = "Gazipur".to_string();
        assert_eq!(
            validate_address_input(&suburb).expect("suburb address").zone,
            DeliveryZone::DhakaSuburb
        );
    }

    #[test]
    fn address_validation_rejects_bad_phone_district_and_lengths() {
        let mut bad_phone = sample_address_input();
        bad_phone.phone = "01212345678".to_string();
        assert!(validate_address_input(&bad_phone).is_err());

        let mut bad_district = sample_address_input();
        bad_district.district = "Dhaka City".to_string();
        assert!(validate_address_input(&bad_district).is_err());

        let mut blank_label = sample_address_input();
        blank_label.label = "   ".to_string();
        assert!(validate_address_input(&blank_label).is_err());

        let mut long_line = sample_address_input();
        long_line.address_line = "x".repeat(MAX_ADDRESS_LINE_CHARS + 1);
        assert!(validate_address_input(&long_line).is_err());
        long_line.address_line = "x".repeat(MAX_ADDRESS_LINE_CHARS);
        assert!(validate_address_input(&long_line).is_ok());

        let mut long_label = sample_address_input();
        long_label.label = "x".repeat(MAX_ADDRESS_LABEL_CHARS + 1);
        assert!(validate_address_input(&long_label).is_err());
    }

    #[test]
    fn address_count_cap() {
        assert!(validate_address_count(0).is_ok());
        assert!(validate_address_count(MAX_ADDRESSES_PER_USER - 1).is_ok());
        assert!(validate_address_count(MAX_ADDRESSES_PER_USER).is_err());
        assert!(validate_address_count(-1).is_err());
    }

    #[test]
    fn order_note_validation() {
        assert_eq!(validate_order_note(None), Ok(None));
        assert_eq!(validate_order_note(Some("   ")), Ok(None));
        assert_eq!(
            validate_order_note(Some("  গার্ডের কাছে দিন  ")),
            Ok(Some("গার্ডের কাছে দিন".to_string()))
        );
        let at_cap = "x".repeat(MAX_ORDER_NOTE_CHARS);
        assert!(validate_order_note(Some(&at_cap)).is_ok());
        let over_cap = "x".repeat(MAX_ORDER_NOTE_CHARS + 1);
        assert!(validate_order_note(Some(&over_cap)).is_err());
    }

    #[test]
    fn reorder_decision_branches() {
        use ReorderVariantState::{Active, Inactive, Missing, NotApplicable, ProductGainedVariants};
        // Happy path: full quantity when stock and cart
        // room both allow it.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), NotApplicable, 10, 3, 99),
            ReorderDecision::Add { quantity: 3 }
        );
        // Partial stock clamps the add — never a skip
        // while anything can be added.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), Active, 2, 5, 99),
            ReorderDecision::Add { quantity: 2 }
        );
        // Cart headroom clamps too.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), NotApplicable, 10, 5, 1),
            ReorderDecision::Add { quantity: 1 }
        );
        // Product gone / draft / archived.
        for status in [None, Some("draft"), Some("archived")] {
            assert_eq!(
                reorder_decision(status, Some("active"), NotApplicable, 10, 1, 99),
                ReorderDecision::Skip(ReorderSkipReason::ProductUnavailable)
            );
        }
        // Variant problems beat the stock check.
        for state in [Inactive, Missing, ProductGainedVariants] {
            assert_eq!(
                reorder_decision(Some("active"), Some("active"), state, 0, 1, 99),
                ReorderDecision::Skip(ReorderSkipReason::VariantUnavailable)
            );
        }
        // Empty pool.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), NotApplicable, 0, 1, 99),
            ReorderDecision::Skip(ReorderSkipReason::OutOfStock)
        );
        // A cart line already at the cap.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), NotApplicable, 10, 1, 0),
            ReorderDecision::Skip(ReorderSkipReason::CartLimit)
        );
        // Reason codes are the report vocabulary.
        assert_eq!(ReorderSkipReason::OutOfStock.as_str(), "out_of_stock");
        assert!(!ReorderSkipReason::ProductUnavailable.message().is_empty());
        // A suspended store skips the line even when the
        // product itself is healthy (admin stores remake);
        // a gone store row reads the same way.
        assert_eq!(
            reorder_decision(Some("active"), Some("suspended"), NotApplicable, 10, 1, 99),
            ReorderDecision::Skip(ReorderSkipReason::StoreSuspended)
        );
        assert_eq!(
            reorder_decision(Some("active"), None, NotApplicable, 10, 1, 99),
            ReorderDecision::Skip(ReorderSkipReason::StoreSuspended)
        );
        assert_eq!(ReorderSkipReason::StoreSuspended.as_str(), "store_suspended");
    }

    #[test]
    fn tracking_steps_follow_the_sub_order_status() {
        use TrackingStepState::{Alert, Current, Done, Upcoming};
        let kinds: Vec<(&str, TrackingStepState)> = sub_order_tracking_steps("placed", None)
            .iter()
            .map(|s| (s.kind, s.state))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("placed", Current),
                ("confirmed", Upcoming),
                ("shipped", Upcoming),
                ("delivered", Upcoming)
            ]
        );
        let confirmed = sub_order_tracking_steps("confirmed", None);
        assert_eq!(confirmed[1].state, Current);
        let shipped = sub_order_tracking_steps("shipped", Some("in_transit"));
        assert_eq!(shipped[2].state, Current);
        assert_eq!(shipped[3].state, Upcoming);
        let delivered = sub_order_tracking_steps("delivered", Some("delivered"));
        assert!(delivered.iter().all(|s| s.state == Done));
        // Returned keeps the travelled steps and ends in
        // the alert step — no delivered step is claimed.
        let returned = sub_order_tracking_steps("returned", Some("returned"));
        assert_eq!(returned.len(), 4);
        assert_eq!(returned[3].kind, "returned");
        assert_eq!(returned[3].state, Alert);
        let cancelled = sub_order_tracking_steps("cancelled", None);
        assert_eq!(cancelled.len(), 2);
        assert_eq!(cancelled[1].state, Alert);
        // A courier-cancelled shipment reads as the
        // cancelled branch even mid-path.
        let courier_cancelled = sub_order_tracking_steps("shipped", Some("cancelled"));
        assert_eq!(courier_cancelled[2].kind, "cancelled");
        assert_eq!(courier_cancelled[2].state, Alert);
        assert_eq!(TrackingStepState::Done.as_str(), "done");
    }

    #[test]
    fn district_table_bangla_names_and_zone_counts() {
        // Every district carries BOTH names — the Bangla
        // name is what the buyer reads in the select, so
        // an empty or duplicated one is a data bug.
        let mut bangla_names: Vec<&str> = DISTRICTS.iter().map(|d| d.name_bn).collect();
        for district in DISTRICTS {
            assert!(!district.name_en.trim().is_empty());
            assert!(!district.name_bn.trim().is_empty());
        }
        bangla_names.sort_unstable();
        bangla_names.dedup();
        assert_eq!(bangla_names.len(), 64, "Bangla names are unique too");

        // The zone policy as COUNTS: exactly one inside
        // district, exactly the five bordering districts
        // in the suburb ring, the other 58 outside.
        let count = |zone: DeliveryZone| DISTRICTS.iter().filter(|d| d.zone == zone).count();
        assert_eq!(count(DeliveryZone::InsideDhaka), 1);
        assert_eq!(count(DeliveryZone::DhakaSuburb), 5);
        assert_eq!(count(DeliveryZone::OutsideDhaka), 58);
        for suburb in ["Gazipur", "Narayanganj", "Munshiganj", "Narsingdi", "Manikganj"] {
            assert_eq!(
                zone_for_district(suburb),
                Some(DeliveryZone::DhakaSuburb),
                "{suburb} borders Dhaka district"
            );
        }

        // Lookup works by Bangla name and trims padding.
        assert_eq!(
            district_by_name("  ঢাকা ").map(|d| d.name_en),
            Some("Dhaka")
        );
        assert_eq!(district_by_name("খুলনা").map(|d| d.name_en), Some("Khulna"));
        assert_eq!(zone_for_district("atlantis"), None);
    }

    #[test]
    fn address_validation_field_caps_at_the_boundary() {
        let at_caps = AddressInput {
            label: "x".repeat(MAX_ADDRESS_LABEL_CHARS),
            recipient_name: "x".repeat(MAX_ADDRESS_RECIPIENT_CHARS),
            phone: "01712345678".to_string(),
            district: "Dhaka".to_string(),
            area: "x".repeat(MAX_ADDRESS_AREA_CHARS),
            address_line: "x".repeat(MAX_ADDRESS_LINE_CHARS),
        };
        assert!(validate_address_input(&at_caps).is_ok(), "exactly at the caps passes");
        let over_label = AddressInput {
            label: "x".repeat(MAX_ADDRESS_LABEL_CHARS + 1),
            ..at_caps.clone()
        };
        assert!(validate_address_input(&over_label).is_err(), "label over its cap fails");
        let over_line = AddressInput {
            address_line: "x".repeat(MAX_ADDRESS_LINE_CHARS + 1),
            ..at_caps.clone()
        };
        assert!(validate_address_input(&over_line).is_err(), "line over its cap fails");
        // Every field is required: an all-whitespace area
        // is as missing as an empty one.
        let blank_area = AddressInput {
            area: "   ".to_string(),
            ..at_caps
        };
        assert!(validate_address_input(&blank_area).is_err());
    }

    #[test]
    fn order_note_boundaries() {
        assert_eq!(validate_order_note(None), Ok(None));
        assert_eq!(validate_order_note(Some("   ")), Ok(None), "blank is no note");
        assert_eq!(
            validate_order_note(Some("  ring the bell  ")),
            Ok(Some("ring the bell".to_string())),
            "the stored note is trimmed"
        );
        let at_cap = "x".repeat(MAX_ORDER_NOTE_CHARS);
        assert_eq!(
            validate_order_note(Some(&at_cap)).map(|n| n.map(|s| s.len())),
            Ok(Some(MAX_ORDER_NOTE_CHARS)),
            "exactly at the cap passes"
        );
        let over_cap = "x".repeat(MAX_ORDER_NOTE_CHARS + 1);
        assert!(validate_order_note(Some(&over_cap)).is_err(), "one over fails");
        // The cap counts CHARACTERS, not bytes — 500 Bangla
        // chars (3 bytes each in UTF-8) are within it.
        let bangla = "অ".repeat(MAX_ORDER_NOTE_CHARS);
        assert!(validate_order_note(Some(&bangla)).is_ok());
    }

    #[test]
    fn reorder_decision_matrix() {
        use ReorderSkipReason::*;
        // A product that gained variants since the order:
        // the old variant-less line names nothing buyable.
        assert_eq!(
            reorder_decision(
                Some("active"),
                Some("active"),
                ReorderVariantState::ProductGainedVariants,
                10,
                1,
                99
            ),
            ReorderDecision::Skip(VariantUnavailable)
        );
        // A full cart line refuses even a healthy product.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), ReorderVariantState::NotApplicable, 10, 1, 0),
            ReorderDecision::Skip(CartLimit)
        );
        // Partial stock clamps the add to what exists.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), ReorderVariantState::NotApplicable, 2, 5, 99),
            ReorderDecision::Add { quantity: 2 }
        );
        // Cart headroom clamps below stock when tighter.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), ReorderVariantState::Active, 50, 5, 3),
            ReorderDecision::Add { quantity: 3 }
        );
        // Draft products are as unbuyable as archived ones,
        // and a deleted product (no row) likewise.
        assert_eq!(
            reorder_decision(Some("draft"), Some("active"), ReorderVariantState::NotApplicable, 5, 1, 99),
            ReorderDecision::Skip(ProductUnavailable)
        );
        assert_eq!(
            reorder_decision(None, Some("active"), ReorderVariantState::NotApplicable, 5, 1, 99),
            ReorderDecision::Skip(ProductUnavailable)
        );
        // A missing variant row (hard-removed since) skips
        // like a deactivated one.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), ReorderVariantState::Missing, 5, 1, 99),
            ReorderDecision::Skip(VariantUnavailable)
        );
        // The wire codes + bilingual messages are stable.
        assert_eq!(ProductUnavailable.as_str(), "product_unavailable");
        assert_eq!(VariantUnavailable.as_str(), "variant_unavailable");
        assert_eq!(OutOfStock.as_str(), "out_of_stock");
        assert_eq!(CartLimit.as_str(), "cart_limit");
        for reason in [ProductUnavailable, VariantUnavailable, OutOfStock, CartLimit] {
            assert!(reason.message().contains('—'), "bilingual message: {reason:?}");
        }
    }

    #[test]
    fn tracking_steps_edge_branches() {
        use TrackingStepState::{Alert, Current, Done};
        // An unknown status takes the placed branch
        // (defensive — the path has only begun).
        let unknown = sub_order_tracking_steps("mystery", None);
        assert_eq!(unknown[0].kind, "placed");
        assert_eq!(unknown[0].state, Current);
        // A courier-cancelled leg does NOT rewrite a
        // delivered or returned story.
        let delivered = sub_order_tracking_steps("delivered", Some("cancelled"));
        assert!(delivered.iter().all(|s| s.state == Done));
        let returned = sub_order_tracking_steps("returned", Some("cancelled"));
        assert_eq!(returned[3].kind, "returned");
        assert_eq!(returned[3].state, Alert);
        // Every state has its wire string.
        assert_eq!(TrackingStepState::Current.as_str(), "current");
        assert_eq!(TrackingStepState::Upcoming.as_str(), "upcoming");
        assert_eq!(TrackingStepState::Alert.as_str(), "alert");
    }

    #[test]
    fn compose_delivery_address_joins_line_area_district() {
        assert_eq!(
            compose_delivery_address("House 5", "Uttara", "Gazipur"),
            "House 5, Uttara, Gazipur"
        );
    }

    #[test]
    fn district_search_ranks_prefix_then_substring_then_bangla() {
        // Prefix beats substring: "dha" finds Dhaka first,
        // and every result contains the needle somewhere.
        let hits = search_districts("dha", 10);
        assert_eq!(hits[0].name_en, "Dhaka");
        assert!(hits
            .iter()
            .all(|d| d.name_en.to_lowercase().contains("dha")));
        // Case does not matter for the English side.
        assert_eq!(search_districts("DHAKA", 5)[0].name_en, "Dhaka");
        // A Bangla query matches on the Bangla name.
        let hits = search_districts("ঢাকা", 10);
        assert!(hits.iter().any(|d| d.name_en == "Dhaka"));
        // A substring-only match still lands (rank 1).
        let hits = search_districts("ganj", 10);
        assert!(hits.iter().any(|d| d.name_en == "Narayanganj"));
        assert!(hits.iter().any(|d| d.name_en == "Munshiganj"));
        // No match is an empty list, never a panic or the
        // whole table.
        assert!(search_districts("Zxqwvbn", 10).is_empty());
        // The limit is honoured, and zero yields nothing.
        assert_eq!(search_districts("a", 3).len(), 3);
        assert!(search_districts("a", 0).is_empty());
        // An empty query is the picker's initial state:
        // the canonical head of the table.
        let initial = search_districts("  ", 4);
        assert_eq!(initial.len(), 4);
        assert_eq!(initial[0].name_en, DISTRICTS[0].name_en);
    }

    #[test]
    fn reorder_decision_exact_boundaries_add_in_full() {
        use ReorderVariantState::NotApplicable;
        // Stock exactly equal to the request adds in full —
        // the clamp is a ceiling, not a haircut.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), NotApplicable, 4, 4, 99),
            ReorderDecision::Add { quantity: 4 }
        );
        // Headroom exactly equal to the request adds in full.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), NotApplicable, 10, 4, 4),
            ReorderDecision::Add { quantity: 4 }
        );
        // One unit of headroom with plenty of stock adds
        // exactly one.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), NotApplicable, 10, 4, 1),
            ReorderDecision::Add { quantity: 1 }
        );
        // A zero-quantity request can only be a skip
        // (defensive: order lines are never zero, but the
        // rule must not Add{0}) — it lands on the
        // quantity<=0 guard, reported as the cart limit.
        assert_eq!(
            reorder_decision(Some("active"), Some("active"), NotApplicable, 10, 0, 99),
            ReorderDecision::Skip(ReorderSkipReason::CartLimit)
        );
    }

    #[test]
    fn tracking_steps_confirmed_with_cancelled_shipment_alerts() {
        use TrackingStepState::{Alert, Done};
        // Confirmed, then the courier cancelled the leg:
        // the cancelled branch wins over the in-flight one
        // (the parcel is NOT on its way).
        let steps = sub_order_tracking_steps("confirmed", Some("cancelled"));
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].kind, "placed");
        assert_eq!(steps[0].state, Done);
        assert_eq!(steps[1].kind, "confirmed");
        assert_eq!(steps[1].state, Done);
        assert_eq!(steps[2].kind, "cancelled");
        assert_eq!(steps[2].state, Alert);
        // A shipped sub-order reads placed/confirmed done,
        // shipped current.
        let steps = sub_order_tracking_steps("shipped", Some("in_transit"));
        assert_eq!(steps[2].kind, "shipped");
        assert_eq!(steps[2].state, TrackingStepState::Current);
        assert_eq!(steps[3].state, TrackingStepState::Upcoming);
    }

    #[test]
    fn address_validation_canonicalises_bangla_district_and_phone_forms() {
        // The district may arrive as its Bangla name; the
        // stored form is the canonical English name and the
        // zone follows it.
        let validated = validate_address_input(&AddressInput {
            label: "বাসা".to_string(),
            recipient_name: "Karim".to_string(),
            phone: "8801712345678".to_string(),
            district: "ঢাকা".to_string(),
            area: "মিরপুর".to_string(),
            address_line: "ব্লক সি".to_string(),
        })
        .expect("bangla district validates");
        assert_eq!(validated.district, "Dhaka");
        assert_eq!(validated.zone, DeliveryZone::InsideDhaka);
        assert_eq!(validated.phone, "+8801712345678");
        // The +880 form normalises identically.
        let validated = validate_address_input(&AddressInput {
            phone: "+8801712345678".to_string(),
            ..sample_address_input()
        })
        .expect("international form validates");
        assert_eq!(validated.phone, "+8801712345678");
        // An unknown district is refused, never zoned by
        // guesswork.
        let unknown = AddressInput {
            district: "Atlantis".to_string(),
            ..sample_address_input()
        };
        assert!(validate_address_input(&unknown).is_err());
        assert_eq!(zone_for_district("Atlantis"), None);
    }

    #[test]
    fn order_note_trims_edges_and_keeps_the_middle() {
        assert_eq!(
            validate_order_note(Some("  গেটে  ফোন  করুন  ")).expect("note ok"),
            Some("গেটে  ফোন  করুন".to_string())
        );
        assert_eq!(validate_order_note(None).expect("no note"), None);
        assert_eq!(validate_order_note(Some("")).expect("empty note"), None);
    }

    #[test]
    fn reorder_skip_messages_are_bilingual_for_every_reason() {
        for reason in [
            ReorderSkipReason::ProductUnavailable,
            ReorderSkipReason::VariantUnavailable,
            ReorderSkipReason::OutOfStock,
            ReorderSkipReason::CartLimit,
            ReorderSkipReason::StoreSuspended,
        ] {
            let message = reason.message();
            assert!(message.contains(" — "), "{reason:?} message: {message}");
            assert!(
                message.chars().any(|c| ('\u{0980}'..='\u{09FF}').contains(&c)),
                "{reason:?} message carries Bangla: {message}"
            );
            assert!(
                message.chars().any(|c| c.is_ascii_alphabetic()),
                "{reason:?} message carries English: {message}"
            );
            assert!(!reason.as_str().is_empty());
        }
    }

    #[test]
    fn district_search_limit_above_match_count_returns_all_matches() {
        let hits = search_districts("khulna", 64);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name_en, "Khulna");
        // Every district is reachable by its own name.
        for district in DISTRICTS.iter() {
            let hits = search_districts(district.name_en, 64);
            assert!(
                hits.iter().any(|hit| hit.name_en == district.name_en),
                "{} finds itself",
                district.name_en
            );
        }
    }

    #[test]
    fn suburb_five_are_exactly_the_dhaka_bordering_districts() {
        // The platform policy, pinned by name: the suburb
        // ring is the five districts that share a border
        // with Dhaka district. Tangail is the famous
        // near-miss — it does NOT border Dhaka and stays
        // Outside Dhaka (documented in the README).
        for name in ["Gazipur", "Narayanganj", "Munshiganj", "Narsingdi", "Manikganj"] {
            assert_eq!(
                zone_for_district(name),
                Some(DeliveryZone::DhakaSuburb),
                "{name} is suburb"
            );
        }
        assert_eq!(
            zone_for_district("Tangail"),
            Some(DeliveryZone::OutsideDhaka)
        );
        // Keraniganj is a thana of Dhaka district, not a
        // district — the table is district-level, so the
        // name is unknown here (Dhaka district covers it).
        assert!(district_by_name("Keraniganj").is_none());
    }

    // -- Wave 56: category taxonomy -------------------------------------

    fn cat(
        id: &str,
        parent: Option<&str>,
        slug: &str,
        name_en: &str,
        sort_order: i64,
    ) -> CategoryFlat {
        CategoryFlat {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            slug: slug.to_string(),
            name_en: name_en.to_string(),
            name_bn: None,
            icon_key: DEFAULT_CATEGORY_ICON.to_string(),
            sort_order,
            is_active: true,
            is_featured: false,
        }
    }

    /// A small three-level tree:
    /// fashion ─┬─ mens ─── panjabi
    ///          └─ womens
    /// electronics ── phones
    fn taxonomy_fixture() -> Vec<CategoryFlat> {
        vec![
            cat("electronics", None, "electronics", "Electronics", 20),
            cat("fashion", None, "fashion", "Fashion", 10),
            cat("womens", Some("fashion"), "womens-fashion", "Women's Fashion", 20),
            cat("mens", Some("fashion"), "mens-fashion", "Men's Fashion", 10),
            cat("panjabi", Some("mens"), "panjabi", "Panjabi", 10),
            cat("phones", Some("electronics"), "mobile-phones", "Mobile Phones", 10),
        ]
    }

    #[test]
    fn category_slug_rule_reuses_the_platform_slug_rule() {
        assert_eq!(validate_category_slug("fashion").unwrap(), "fashion");
        assert_eq!(
            validate_category_slug(" mens-fashion ").unwrap(),
            "mens-fashion"
        );
        for bad in ["ab", "Fashion", "has space", "-lead", "trail-", "under_score", ""] {
            assert!(
                validate_category_slug(bad).is_err(),
                "slug {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn category_names_are_trimmed_capped_and_bn_optional() {
        let (en, bn) = validate_category_names("  Fashion ", Some(" ফ্যাশন ")).unwrap();
        assert_eq!(en, "Fashion");
        assert_eq!(bn.as_deref(), Some("ফ্যাশন"));
        let (_, bn) = validate_category_names("Fashion", Some("   ")).unwrap();
        assert_eq!(bn, None);
        assert!(validate_category_names("   ", None).is_err());
        let long = "x".repeat(MAX_CATEGORY_NAME_CHARS + 1);
        assert!(validate_category_names(&long, None).is_err());
        assert!(validate_category_names("ok", Some(&long)).is_err());
        let at_cap = "x".repeat(MAX_CATEGORY_NAME_CHARS);
        assert!(validate_category_names(&at_cap, None).is_ok());
    }

    #[test]
    fn category_icon_must_be_a_builtin_key() {
        assert_eq!(validate_category_icon(None).unwrap(), DEFAULT_CATEGORY_ICON);
        assert_eq!(validate_category_icon(Some(" ")).unwrap(), DEFAULT_CATEGORY_ICON);
        assert_eq!(validate_category_icon(Some("shirt")).unwrap(), "shirt");
        assert!(validate_category_icon(Some("uploaded.png")).is_err());
        for key in CATEGORY_ICON_KEYS {
            assert!(validate_category_icon(Some(key)).is_ok(), "key {key}");
        }
    }

    #[test]
    fn category_depth_and_subtree_walk_the_fixture_tree() {
        let rows = taxonomy_fixture();
        assert_eq!(category_depth(&rows, "fashion"), Some(1));
        assert_eq!(category_depth(&rows, "mens"), Some(2));
        assert_eq!(category_depth(&rows, "panjabi"), Some(3));
        assert_eq!(category_depth(&rows, "missing"), None);
        assert_eq!(subtree_height(&rows, "fashion"), 3);
        assert_eq!(subtree_height(&rows, "panjabi"), 1);
        assert_eq!(subtree_height(&rows, "missing"), 0);
        // Subtree ids: self first, then descendants in
        // display order (mens before womens by sort_order).
        assert_eq!(
            subtree_ids(&rows, "fashion"),
            vec!["fashion", "mens", "panjabi", "womens"]
        );
        assert_eq!(subtree_ids(&rows, "missing"), Vec::<String>::new());
    }

    #[test]
    fn category_tree_builds_nested_and_sorts_siblings() {
        let rows = taxonomy_fixture();
        let tree = build_category_tree(&rows);
        // Roots in display order: fashion (10) before
        // electronics (20), regardless of row order.
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].slug, "fashion");
        assert_eq!(tree[0].depth, 1);
        assert_eq!(tree[1].slug, "electronics");
        let fashion = &tree[0];
        assert_eq!(fashion.children.len(), 2);
        assert_eq!(fashion.children[0].slug, "mens-fashion");
        assert_eq!(fashion.children[0].depth, 2);
        assert_eq!(fashion.children[0].children.len(), 1);
        assert_eq!(fashion.children[0].children[0].slug, "panjabi");
        assert_eq!(fashion.children[0].children[0].depth, 3);
        assert_eq!(fashion.children[1].slug, "womens-fashion");
    }

    #[test]
    fn category_tree_promotes_orphans_to_roots() {
        // A row whose parent is absent from the set (e.g. the
        // parent was filtered out as inactive) surfaces at
        // the top instead of vanishing with its branch.
        let rows = vec![
            cat("child", Some("ghost"), "child-cat", "Child", 5),
            cat("root", None, "root-cat", "Root", 10),
        ];
        let tree = build_category_tree(&rows);
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].slug, "child-cat");
        assert_eq!(tree[0].depth, 1);
    }

    #[test]
    fn category_path_is_the_root_to_node_chain() {
        let rows = taxonomy_fixture();
        let path = category_path(&rows, "panjabi").unwrap();
        let slugs: Vec<&str> = path.iter().map(|row| row.slug.as_str()).collect();
        assert_eq!(slugs, vec!["fashion", "mens-fashion", "panjabi"]);
        let root_path = category_path(&rows, "fashion").unwrap();
        assert_eq!(root_path.len(), 1);
        assert!(category_path(&rows, "missing").is_none());
    }

    #[test]
    fn category_parent_validation_blocks_cycles_and_depth_overflow() {
        let rows = taxonomy_fixture();
        // Plain placements that fit.
        assert_eq!(validate_category_parent(&rows, None, None).unwrap(), 1);
        assert_eq!(
            validate_category_parent(&rows, None, Some("mens")).unwrap(),
            3
        );
        assert_eq!(
            validate_category_parent(&rows, Some("womens"), Some("electronics")).unwrap(),
            2
        );
        // A new child under a grandchild would be level 4.
        assert!(validate_category_parent(&rows, None, Some("panjabi")).is_err());
        // Moving fashion (height 3) under phones (depth 2)
        // would reach level 5.
        assert!(validate_category_parent(&rows, Some("fashion"), Some("phones")).is_err());
        // Cycles: under itself, and under its own descendant.
        assert!(would_create_cycle(&rows, "fashion", "fashion"));
        assert!(would_create_cycle(&rows, "fashion", "panjabi"));
        assert!(!would_create_cycle(&rows, "fashion", "phones"));
        assert!(validate_category_parent(&rows, Some("fashion"), Some("panjabi")).is_err());
        assert!(validate_category_parent(&rows, Some("fashion"), Some("fashion")).is_err());
        // Unknown parent.
        assert!(validate_category_parent(&rows, None, Some("ghost")).is_err());
    }

    #[test]
    fn category_delete_guard_names_both_blockers() {
        assert!(validate_category_delete(0, 0).is_ok());
        let err = validate_category_delete(2, 0).unwrap_err();
        assert!(err.contains('2'), "child count in the error: {err}");
        let err = validate_category_delete(0, 7).unwrap_err();
        assert!(err.contains('7'), "product count in the error: {err}");
        // Children are reported before products.
        let err = validate_category_delete(1, 7).unwrap_err();
        assert!(err.contains('1'), "children first: {err}");
    }

    #[test]
    fn category_merge_guard_blocks_self_descendant_and_depth() {
        let rows = taxonomy_fixture();
        assert!(validate_category_merge(&rows, "womens", "mens").is_ok());
        assert!(validate_category_merge(&rows, "fashion", "fashion").is_err());
        // Target inside the source's own subtree.
        assert!(validate_category_merge(&rows, "fashion", "panjabi").is_err());
        // Unknown ids.
        assert!(validate_category_merge(&rows, "ghost", "mens").is_err());
        assert!(validate_category_merge(&rows, "mens", "ghost").is_err());
        // Merging fashion (children reach height 2 below it)
        // into phones (depth 2) would reach level 4.
        assert!(validate_category_merge(&rows, "fashion", "phones").is_err());
    }

    #[test]
    fn category_link_resolution_is_exact_match_in_priority_order() {
        let mut rows = taxonomy_fixture();
        rows[1].name_bn = Some("ফ্যাশন".to_string()); // fashion
        // Slug match, case-insensitive via lowercasing.
        assert_eq!(
            resolve_category_link("FASHION", &rows).as_deref(),
            Some("fashion")
        );
        // English name match, case-insensitive.
        assert_eq!(
            resolve_category_link("mobile phones", &rows).as_deref(),
            Some("phones")
        );
        // Bangla name match, exact.
        assert_eq!(
            resolve_category_link("ফ্যাশন", &rows).as_deref(),
            Some("fashion")
        );
        // Trimming is part of the rule.
        assert_eq!(
            resolve_category_link("  Panjabi ", &rows).as_deref(),
            Some("panjabi")
        );
        // No fuzzy matching: near-misses and substrings stay
        // unlinked, as do blank labels.
        assert_eq!(resolve_category_link("Fashions", &rows), None);
        assert_eq!(resolve_category_link("Fash", &rows), None);
        assert_eq!(resolve_category_link("Men's", &rows), None);
        assert_eq!(resolve_category_link("   ", &rows), None);
        // Inactive nodes never resolve.
        let mut inactive = taxonomy_fixture();
        inactive[1].is_active = false; // fashion
        assert_eq!(resolve_category_link("Fashion", &inactive), None);
    }

    #[test]
    fn category_slugify_derives_import_slugs() {
        assert_eq!(slugify_category_name("Home & Living"), "home-living");
        assert_eq!(slugify_category_name("  Baby  &  Kids "), "baby-kids");
        assert_eq!(slugify_category_name("Rice & Grains!"), "rice-grains");
        assert_eq!(slugify_category_name("মুদি বাজার"), "");
        assert_eq!(slugify_category_name("T-Shirts"), "t-shirts");
    }

    #[test]
    fn category_resolution_prefers_a_slug_over_a_colliding_name() {
        // One node's SLUG equals another node's NAME: the
        // slug match wins — the documented priority is
        // what keeps resolution deterministic when the two
        // namespaces collide.
        let rows = vec![
            cat("a", None, "phones", "Gadgets", 10),
            cat("b", None, "gadgets", "Phones", 20),
        ];
        assert_eq!(resolve_category_link("phones", &rows).as_deref(), Some("a"));
        assert_eq!(resolve_category_link("PHONES", &rows).as_deref(), Some("a"));
        // The priority is symmetric: "Gadgets" is a's NAME
        // but b's SLUG, so the slug claim wins again and
        // the label resolves to b.
        assert_eq!(resolve_category_link("Gadgets", &rows).as_deref(), Some("b"));
        // And a name still resolves when no slug claims
        // the label at all.
        let name_only = vec![cat("c", None, "shoes", "Footwear", 10)];
        assert_eq!(
            resolve_category_link("Footwear", &name_only).as_deref(),
            Some("c")
        );
    }

    #[test]
    fn category_walks_are_total_on_unknown_and_cyclic_rows() {
        let rows = taxonomy_fixture();
        // Unknown ids yield None / empty, never a panic.
        assert_eq!(category_depth(&rows, "ghost"), None);
        assert!(subtree_ids(&rows, "ghost").is_empty());
        assert_eq!(category_path(&rows, "ghost"), None);
        assert_eq!(subtree_height(&rows, "ghost"), 0);
        // A root's path is itself; a leaf's subtree is
        // itself; a root's subtree is the whole branch.
        let path = category_path(&rows, "fashion").expect("root path");
        assert_eq!(path.len(), 1);
        assert_eq!(subtree_ids(&rows, "panjabi"), vec!["panjabi".to_string()]);
        assert_eq!(subtree_ids(&rows, "fashion").len(), 4);
        // Cyclic rows (a corrupt import the guards exist
        // to prevent) terminate instead of looping: depth
        // and path bail out past the row count.
        let cyclic = vec![
            cat("x", Some("y"), "x-slug", "X", 10),
            cat("y", Some("x"), "y-slug", "Y", 10),
        ];
        assert!(would_create_cycle(&cyclic, "x", "y"));
        assert!(category_depth(&cyclic, "x").is_none_or(|d| d > 2));
        assert!(subtree_ids(&cyclic, "x").len() <= 2);
    }

    #[test]
    fn category_parent_validation_allows_the_legal_moves() {
        let rows = taxonomy_fixture();
        // A leaf may move to any root or depth-2 parent.
        assert_eq!(
            validate_category_parent(&rows, Some("panjabi"), Some("electronics")),
            Ok(2)
        );
        assert_eq!(
            validate_category_parent(&rows, Some("panjabi"), Some("phones")),
            Ok(3)
        );
        assert_eq!(validate_category_parent(&rows, Some("panjabi"), None), Ok(1));
        // A brand-new node (no id yet — the create call)
        // may hang at depth 3 but not below it.
        assert_eq!(validate_category_parent(&rows, None, Some("mens")), Ok(3));
        assert!(validate_category_parent(&rows, None, Some("panjabi")).is_err());
        // Moving a node under ITSELF is a cycle.
        assert!(validate_category_parent(&rows, Some("mens"), Some("mens")).is_err());
        // An unknown parent is rejected, not treated as a
        // root.
        assert!(validate_category_parent(&rows, Some("panjabi"), Some("ghost")).is_err());
    }

    #[test]
    fn category_display_order_breaks_ties_by_name_then_slug() {
        let mut rows = [
            cat("c", None, "zeta", "Same", 10),
            cat("a", None, "alpha", "Same", 10),
            cat("b", None, "beta", "Another", 10),
            cat("d", None, "delta", "Zulu", 5),
        ];
        rows.sort_by(category_display_order);
        let order: Vec<&str> = rows.iter().map(|row| row.slug.as_str()).collect();
        // sort_order first (delta at 5), then name_en
        // (Another before Same), then slug (alpha < zeta).
        assert_eq!(order, vec!["delta", "beta", "alpha", "zeta"]);
    }

    #[test]
    fn category_tree_keeps_grandchildren_nested_at_depth_three() {
        let rows = taxonomy_fixture();
        let tree = build_category_tree(&rows);
        let fashion = tree
            .iter()
            .find(|node| node.slug == "fashion")
            .expect("fashion root");
        assert_eq!(fashion.depth, 1);
        let mens = fashion
            .children
            .iter()
            .find(|node| node.slug == "mens-fashion")
            .expect("mens child");
        assert_eq!(mens.depth, 2);
        assert_eq!(mens.children.len(), 1);
        assert_eq!(mens.children[0].slug, "panjabi");
        assert_eq!(mens.children[0].depth, 3);
        assert!(mens.children[0].children.is_empty());
    }

    // -- Wave 57: bulk CSV -------------------------------------------

    #[test]
    fn csv_parser_handles_quotes_commas_and_embedded_newlines() {
        let parsed = parse_csv("a,b,c\n\"x,y\",\"he said \"\"hi\"\"\",\"line1\nline2\"\n")
            .expect("parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], vec!["a", "b", "c"]);
        assert_eq!(parsed[1], vec!["x,y", "he said \"hi\"", "line1\nline2"]);
    }

    #[test]
    fn csv_parser_handles_crlf_cr_and_bom() {
        let parsed = parse_csv("\u{feff}a,b\r\nc,d\re,f").expect("parse");
        assert_eq!(
            parsed,
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["c".to_string(), "d".to_string()],
                vec!["e".to_string(), "f".to_string()],
            ]
        );
    }

    #[test]
    fn csv_parser_trailing_newline_makes_no_phantom_row() {
        assert_eq!(parse_csv("a,b\n").expect("parse").len(), 1);
        assert_eq!(parse_csv("a,b").expect("parse").len(), 1);
        assert!(parse_csv("").expect("parse").is_empty());
    }

    #[test]
    fn csv_parser_rejects_an_unterminated_quote() {
        let err = parse_csv("a,\"never closed\nnext").expect_err("must fail");
        assert!(err.contains("unterminated"), "got: {err}");
    }

    #[test]
    fn csv_writer_round_trips_through_the_parser() {
        let rows = vec![
            vec!["sku".to_string(), "title".to_string()],
            vec![
                "A-1".to_string(),
                "Comma, quote \"and\" newline\ninside".to_string(),
            ],
            vec!["B-2".to_string(), "পাঞ্জাবি".to_string()],
        ];
        let text = write_csv(&rows);
        assert_eq!(parse_csv(&text).expect("reparse"), rows);
    }

    #[test]
    fn csv_escape_field_quotes_only_when_needed() {
        assert_eq!(csv_escape_field("plain"), "plain");
        assert_eq!(csv_escape_field("a,b"), "\"a,b\"");
        assert_eq!(csv_escape_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_escape_field(""), "");
    }

    fn bulk_sheet(rows: &[&str]) -> String {
        let mut text = String::from(
            "sku,title,title_bn,description,price_poisha,compare_at_poisha,stock,\
             weight_grams,status,category_slug,category_label,image_url\n",
        );
        for row in rows {
            text.push_str(row);
            text.push('\n');
        }
        text
    }

    #[test]
    fn bulk_rows_parse_with_defaults_and_trimming() {
        // Columns: sku,title,title_bn,description,
        // price_poisha,compare_at_poisha,stock,
        // weight_grams,status,category_slug,
        // category_label,image_url
        let parsed = parse_bulk_product_csv(&bulk_sheet(&[
            " SKU-1 , Cotton Panjabi ,,সুতির পাঞ্জাবি,185000,,25,,active,panjabi,,",
            "SKU-2,Draft Thing,,,5000,,,,,,,",
        ]))
        .expect("parse");
        assert!(parsed.row_errors.is_empty(), "{:?}", parsed.row_errors);
        assert_eq!(parsed.rows.len(), 2);
        let row = &parsed.rows[0];
        assert_eq!(row.row_number, 2);
        assert_eq!(row.sku, "SKU-1");
        assert_eq!(row.title, "Cotton Panjabi");
        assert_eq!(row.price_poisha, 185_000);
        assert_eq!(row.stock, 25);
        assert_eq!(row.weight_grams, DEFAULT_WEIGHT_GRAMS);
        assert_eq!(row.status, "active");
        assert_eq!(row.category_slug.as_deref(), Some("panjabi"));
        assert_eq!(row.title_bn, None);
        // Blank status defaults to draft (a sheet that does
        // not say otherwise never publishes by accident).
        assert_eq!(parsed.rows[1].status, "draft");
        assert_eq!(parsed.rows[1].row_number, 3);
    }

    #[test]
    fn bulk_rows_accumulate_every_field_error() {
        let parsed = parse_bulk_product_csv(&bulk_sheet(&[
            // Empty sku, empty title, non-numeric price,
            // negative stock, bad status, bad slug.
            ",,,,\"not-a-number\",,-5,,bogus,Bad Slug,,",
            "OK-1,Good Row,,,1000,,,,active,,,",
        ]))
        .expect("parse");
        assert_eq!(parsed.rows.len(), 1, "the good row survives");
        assert_eq!(parsed.row_errors.len(), 1);
        let bad = &parsed.row_errors[0];
        assert_eq!(bad.row_number, 2);
        let fields: Vec<&str> = bad.errors.iter().map(|e| e.field.as_str()).collect();
        assert!(fields.contains(&"sku"), "{fields:?}");
        assert!(fields.contains(&"title"), "{fields:?}");
        assert!(fields.contains(&"price_poisha"), "{fields:?}");
        assert!(fields.contains(&"stock"), "{fields:?}");
        assert!(fields.contains(&"status"), "{fields:?}");
        assert!(fields.contains(&"category_slug"), "{fields:?}");
    }

    #[test]
    fn bulk_rows_reject_a_decimal_price_and_a_zero_active_price() {
        let parsed = parse_bulk_product_csv(&bulk_sheet(&[
            "D-1,Decimal Price,,,1850.50,,,,active,,,",
            "Z-1,Zero Price,,,0,,,,active,,,",
            "Z-2,Zero Draft,,,0,,,,draft,,,",
        ]))
        .expect("parse");
        // The decimal is not a whole number of poisha; the
        // zero price is refused only for the ACTIVE row —
        // a draft may sit priceless until it is priced.
        assert_eq!(parsed.rows.len(), 1);
        assert_eq!(parsed.rows[0].sku, "Z-2");
        assert_eq!(parsed.row_errors.len(), 2);
        assert!(parsed.row_errors[0]
            .errors
            .iter()
            .any(|e| e.field == "price_poisha"));
        assert!(parsed.row_errors[1]
            .errors
            .iter()
            .any(|e| e.field == "price_poisha"));
    }

    #[test]
    fn bulk_rows_flag_duplicate_skus_and_skip_blank_rows() {
        let parsed = parse_bulk_product_csv(&bulk_sheet(&[
            "DUP-1,First,,,1000,,,,draft,,,",
            "",
            "DUP-1,Second,,,2000,,,,draft,,,",
        ]))
        .expect("parse");
        assert_eq!(parsed.rows.len(), 1);
        assert_eq!(parsed.rows[0].title, "First");
        assert_eq!(parsed.row_errors.len(), 1);
        assert_eq!(parsed.row_errors[0].row_number, 4);
        assert_eq!(parsed.row_errors[0].sku.as_deref(), Some("DUP-1"));
        assert!(parsed.row_errors[0]
            .errors
            .iter()
            .any(|e| e.field == "sku"));
    }

    #[test]
    fn bulk_sheet_header_contract_is_fatal_when_broken() {
        // Missing a required column.
        assert!(parse_bulk_product_csv("sku,title\nA,Thing\n").is_err());
        // Unknown column.
        assert!(parse_bulk_product_csv("sku,title,price_poisha,colour\nA,T,1,red\n").is_err());
        // Duplicated column.
        assert!(parse_bulk_product_csv("sku,sku,title,price_poisha\nA,A,T,1\n").is_err());
        // Empty file.
        assert!(parse_bulk_product_csv("").is_err());
        // Header case and padding are forgiving.
        let parsed =
            parse_bulk_product_csv(" SKU , Title , PRICE_POISHA \nA,Thing,100\n").expect("parse");
        assert_eq!(parsed.rows.len(), 1);
    }

    #[test]
    fn bulk_sheet_row_limit_is_fatal() {
        let mut text = String::from("sku,title,price_poisha\n");
        for index in 0..=MAX_BULK_IMPORT_ROWS {
            text.push_str(&format!("S-{index},Thing,100\n"));
        }
        let err = parse_bulk_product_csv(&text).expect_err("over the cap");
        assert!(err.contains("500"), "got: {err}");
    }

    #[test]
    fn bulk_template_parses_and_its_rows_validate() {
        let template = bulk_template_csv();
        let parsed = parse_bulk_product_csv(&template).expect("template parses");
        assert!(parsed.row_errors.is_empty(), "{:?}", parsed.row_errors);
        assert_eq!(parsed.rows.len(), 2);
        assert_eq!(parsed.rows[0].status, "active");
        assert_eq!(parsed.rows[1].status, "draft");
    }

    #[test]
    fn bulk_percent_math_rounds_half_up_and_floors_at_one() {
        assert_eq!(bulk_price_after_percent(100_000, -10), Some(90_000));
        assert_eq!(bulk_price_after_percent(100_000, 10), Some(110_000));
        // 105 * 1.10 = 115.5 -> 116 (half up).
        assert_eq!(bulk_price_after_percent(105, 10), Some(116));
        // 150 * 0.99 = 148.5 -> 149.
        assert_eq!(bulk_price_after_percent(150, -1), Some(149));
        // A 99% cut on 100 poisha leaves exactly 1.
        assert_eq!(bulk_price_after_percent(100, -99), Some(1));
        // A 99% cut on 50 poisha would leave 0.5 -> the
        // floor keeps the product at 1 poisha, never free.
        assert_eq!(bulk_price_after_percent(50, -99), Some(1));
        assert_eq!(bulk_price_after_percent(0, 10), None);
        assert_eq!(bulk_price_after_percent(100, -100), None);
        assert_eq!(bulk_price_after_percent(100, 501), None);
        assert_eq!(validate_bulk_percent(-99), Ok(-99));
        assert!(validate_bulk_percent(-100).is_err());
    }

    #[test]
    fn bulk_delta_math_is_checked_and_floored() {
        assert_eq!(bulk_price_after_delta(1_000, -250), Some(750));
        assert_eq!(bulk_price_after_delta(1_000, -999), Some(1));
        assert_eq!(bulk_price_after_delta(1_000, -1_000), None);
        assert_eq!(bulk_price_after_delta(i64::MAX, 1), None);
        assert_eq!(validate_bulk_stock_target(0), Ok(0));
        assert!(validate_bulk_stock_target(-1).is_err());
        assert!(validate_bulk_stock_target(i64::from(i32::MAX) + 1).is_err());
    }

    #[test]
    fn csv_parser_quoted_crlf_and_empty_trailing_fields() {
        // A quoted field may carry a full CRLF pair, and a
        // row may end in empty unquoted fields.
        let parsed = parse_csv("\"a\r\nb\",c,\r\n").expect("parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0], vec!["a\r\nb", "c", ""]);
        // A file that is only a header line plus its
        // newline is one row.
        assert_eq!(parse_csv("h1,h2\n").expect("parse").len(), 1);
        // An empty quoted field is a real empty field.
        let parsed = parse_csv("\"\",x").expect("parse");
        assert_eq!(parsed[0], vec!["", "x"]);
    }

    #[test]
    fn csv_writer_quotes_bangla_commas_and_quotes_minimally() {
        let rows = vec![vec![
            "ঢাকা, বাংলাদেশ".to_string(),
            "plain".to_string(),
            "with \"quote\"".to_string(),
        ]];
        let text = write_csv(&rows);
        assert!(text.contains("\"ঢাকা, বাংলাদেশ\""), "{text}");
        assert!(text.contains(",plain,"), "{text}");
        assert!(text.ends_with("\r\n"), "CRLF terminated: {text:?}");
        assert_eq!(parse_csv(&text).expect("reparse"), rows);
    }

    #[test]
    fn bulk_row_maps_every_column() {
        let parsed = parse_bulk_product_csv(&bulk_sheet(&[concat!(
            "FULL-1,Full Title,পূর্ণ শিরোনাম,A description,123456,150000,42,750,",
            "archived,electronics,Gadgets,https://example.com/x.jpg"
        )]))
        .expect("parse");
        assert!(parsed.row_errors.is_empty(), "{:?}", parsed.row_errors);
        let row = &parsed.rows[0];
        assert_eq!(row.sku, "FULL-1");
        assert_eq!(row.title, "Full Title");
        assert_eq!(row.title_bn.as_deref(), Some("পূর্ণ শিরোনাম"));
        assert_eq!(row.description.as_deref(), Some("A description"));
        assert_eq!(row.price_poisha, 123_456);
        assert_eq!(row.compare_at_poisha, Some(150_000));
        assert_eq!(row.stock, 42);
        assert_eq!(row.weight_grams, 750);
        assert_eq!(row.status, "archived");
        assert_eq!(row.category_slug.as_deref(), Some("electronics"));
        assert_eq!(row.category_label.as_deref(), Some("Gadgets"));
        assert_eq!(row.image_url.as_deref(), Some("https://example.com/x.jpg"));
    }

    #[test]
    fn bulk_row_column_order_is_free() {
        // The header maps by name: a sheet with its
        // columns shuffled parses identically.
        let text = "price_poisha,sku,title\n500,SHUF-1,Shuffled\n";
        let parsed = parse_bulk_product_csv(text).expect("parse");
        assert_eq!(parsed.rows.len(), 1);
        assert_eq!(parsed.rows[0].sku, "SHUF-1");
        assert_eq!(parsed.rows[0].price_poisha, 500);
        // Only the three required columns are mandatory;
        // the rest take their defaults.
        assert_eq!(parsed.rows[0].status, "draft");
        assert_eq!(parsed.rows[0].stock, 0);
    }

    #[test]
    fn bulk_row_wrong_cell_count_is_a_row_error() {
        let parsed = parse_bulk_product_csv(&bulk_sheet(&[
            "SHORT-1,Only Two",
            "OK-9,Fine,,,100,,,,draft,,,",
        ]))
        .expect("parse");
        assert_eq!(parsed.rows.len(), 1);
        assert_eq!(parsed.row_errors.len(), 1);
        assert_eq!(parsed.row_errors[0].row_number, 2);
        assert_eq!(parsed.row_errors[0].errors[0].field, "row");
        assert_eq!(parsed.row_errors[0].sku, None);
    }

    #[test]
    fn bulk_row_rejects_negative_compare_and_huge_stock() {
        let parsed = parse_bulk_product_csv(&bulk_sheet(&[
            "NEG-1,Neg Compare,,,100,-5,,,draft,,,",
            "HUGE-1,Huge Stock,,,100,,9999999999999,,draft,,,",
            "W-1,Bad Weight,,,100,,,0,draft,,,",
        ]))
        .expect("parse");
        assert!(parsed.rows.is_empty(), "{:?}", parsed.rows);
        assert_eq!(parsed.row_errors.len(), 3);
        assert!(parsed.row_errors[0]
            .errors
            .iter()
            .any(|e| e.field == "compare_at_poisha"));
        assert!(parsed.row_errors[1]
            .errors
            .iter()
            .any(|e| e.field == "stock"));
        // Weight 0 is below the domain minimum (1 gram).
        assert!(parsed.row_errors[2]
            .errors
            .iter()
            .any(|e| e.field == "weight_grams"));
    }

    #[test]
    fn bulk_row_sku_rules() {
        let long_sku = "X".repeat(MAX_BULK_SKU_CHARS + 1);
        let sheet = bulk_sheet(&[
            &format!("{long_sku},Too Long,,,100,,,,draft,,,"),
            "OK-2,Fine,,,100,,,,draft,,,",
        ]);
        let parsed = parse_bulk_product_csv(&sheet).expect("parse");
        assert_eq!(parsed.rows.len(), 1);
        assert_eq!(parsed.row_errors.len(), 1);
        assert!(parsed.row_errors[0]
            .errors
            .iter()
            .any(|e| e.field == "sku"));
    }

    // -- Wave 58: storefront section layouts --

    fn w58_section(
        id: &str,
        section_type: &str,
        config: serde_json::Value,
    ) -> StorefrontSection {
        StorefrontSection {
            id: id.to_string(),
            section_type: section_type.to_string(),
            enabled: true,
            config,
            starts_at: None,
            ends_at: None,
        }
    }

    fn w58_layout(sections: Vec<StorefrontSection>) -> StorefrontLayout {
        StorefrontLayout {
            version: STOREFRONT_LAYOUT_VERSION,
            sections,
        }
    }

    #[test]
    fn wave58_default_layout_is_valid_and_matches_the_historical_storefront() {
        let layout = default_storefront_layout();
        let validated = validate_storefront_layout(&layout).expect("default validates");
        assert_eq!(validated.version, 1);
        let types: Vec<&str> = validated
            .sections
            .iter()
            .map(|section| section.section_type.as_str())
            .collect();
        assert_eq!(
            types,
            vec![
                "hero",
                "product_rail",
                "product_rail",
                "product_rail",
                "collection_shelf",
                "category_grid",
                "category_grid",
                "product_grid",
                "store_info",
            ]
        );
        let ids: Vec<&str> = validated
            .sections
            .iter()
            .map(|section| section.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "hero",
                "rail-on-sale",
                "rail-best-sellers",
                "rail-new-arrivals",
                "collections",
                "specialties",
                "categories",
                "all-products",
                "store-info",
            ]
        );
        // The rails keep the historical order and titles.
        let sources: Vec<&str> = validated.sections[1..4]
            .iter()
            .map(|section| section.config["source"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(sources, vec!["on_sale", "best_sellers", "newest"]);
        assert_eq!(
            validated.sections[1].config["title"].as_str(),
            Some("অফারে আছে — On sale at this store")
        );
        // Both category grids survive: taxonomy then labels.
        assert_eq!(validated.sections[5].config["mode"].as_str(), Some("taxonomy"));
        assert_eq!(validated.sections[6].config["mode"].as_str(), Some("labels"));
        assert!(validated.sections.iter().all(|section| section.enabled));
    }

    #[test]
    fn wave58_every_preset_validates_and_unknown_preset_is_none() {
        for key in STOREFRONT_LAYOUT_PRESETS {
            let preset = storefront_layout_preset(key).expect("preset exists");
            validate_storefront_layout(&preset)
                .unwrap_or_else(|error| panic!("preset {key} validates: {error}"));
        }
        assert!(storefront_layout_preset("nope").is_none());
        let minimal = storefront_layout_preset(LAYOUT_PRESET_MINIMAL).expect("minimal");
        assert_eq!(minimal.sections.len(), 3);
        let merchandising =
            storefront_layout_preset(LAYOUT_PRESET_MERCHANDISING).expect("merchandising");
        assert_eq!(merchandising.sections.len(), 9);
    }

    #[test]
    fn wave58_layout_version_and_section_caps() {
        let future = StorefrontLayout {
            version: 2,
            sections: Vec::new(),
        };
        assert!(validate_storefront_layout(&future)
            .unwrap_err()
            .contains("version"));
        let mut many = Vec::new();
        for index in 0..=MAX_STOREFRONT_SECTIONS {
            many.push(w58_section(
                &format!("s-{index}"),
                SECTION_TYPE_STORE_INFO,
                serde_json::json!({}),
            ));
        }
        assert!(validate_storefront_layout(&w58_layout(many))
            .unwrap_err()
            .contains("at most"));
        let exact = (0..MAX_STOREFRONT_SECTIONS)
            .map(|index| {
                w58_section(
                    &format!("s-{index}"),
                    SECTION_TYPE_STORE_INFO,
                    serde_json::json!({}),
                )
            })
            .collect::<Vec<_>>();
        assert!(validate_storefront_layout(&w58_layout(exact)).is_ok());
        // An empty layout is legal (a blank canvas).
        assert!(validate_storefront_layout(&w58_layout(Vec::new())).is_ok());
    }

    #[test]
    fn wave58_section_ids_unique_and_well_formed() {
        let duplicate = w58_layout(vec![
            w58_section("hero", SECTION_TYPE_HERO, serde_json::json!({})),
            w58_section("hero", SECTION_TYPE_HERO, serde_json::json!({})),
        ]);
        let error = validate_storefront_layout(&duplicate).unwrap_err();
        // The error names the 1-based position of the dupe.
        assert!(error.contains("section 2"), "error: {error}");
        assert!(error.contains("more than once"), "error: {error}");
        for bad in ["", "Hero", "hero block", "-hero", "hero-", "héros", "a".repeat(41).as_str()] {
            let layout = w58_layout(vec![w58_section(
                bad,
                SECTION_TYPE_HERO,
                serde_json::json!({}),
            )]);
            assert!(
                validate_storefront_layout(&layout).is_err(),
                "id {bad:?} refused"
            );
        }
        let good = w58_layout(vec![w58_section(
            "eid-2026-hero",
            SECTION_TYPE_HERO,
            serde_json::json!({}),
        )]);
        assert!(validate_storefront_layout(&good).is_ok());
    }

    #[test]
    fn wave58_unknown_types_preserved_and_malformed_types_rejected() {
        let future = w58_section(
            "ar-try-on",
            "ar_try_on",
            serde_json::json!({ "model": "v2", "depth": 3 }),
        );
        let validated = validate_storefront_layout(&w58_layout(vec![future]))
            .expect("a future type validates");
        // Config carried verbatim — nothing normalised away.
        assert_eq!(validated.sections[0].config["model"].as_str(), Some("v2"));
        assert_eq!(validated.sections[0].config["depth"].as_i64(), Some(3));
        assert!(!is_known_section_type("ar_try_on"));
        for bad_type in ["", "AR Try On", "hero!", "type with spaces"] {
            let layout = w58_layout(vec![w58_section(
                "block",
                bad_type,
                serde_json::json!({}),
            )]);
            assert!(
                validate_storefront_layout(&layout).is_err(),
                "type {bad_type:?} refused"
            );
        }
        // A known type with a non-object config is refused.
        let layout = w58_layout(vec![w58_section(
            "hero",
            SECTION_TYPE_HERO,
            serde_json::json!([1, 2]),
        )]);
        assert!(validate_storefront_layout(&layout)
            .unwrap_err()
            .contains("config must be an object"));
    }

    #[test]
    fn wave58_layout_read_is_forward_tolerant() {
        // A stored document from a newer build: unknown
        // top-level fields, unknown section fields, a
        // missing `enabled`, a null config — all parse.
        let raw = serde_json::json!({
            "version": 1,
            "future_field": { "x": 1 },
            "sections": [
                {
                    "id": "hero",
                    "type": "hero",
                    "config": null,
                    "future_section_field": true,
                },
                {
                    "id": "mystery",
                    "type": "future_widget",
                    "enabled": false,
                    "config": { "anything": [1, 2, 3] },
                },
            ],
        });
        let layout: StorefrontLayout =
            serde_json::from_value(raw).expect("tolerant parse");
        assert_eq!(layout.sections.len(), 2);
        assert!(layout.sections[0].enabled, "enabled defaults to true");
        assert_eq!(layout.sections[1].section_type, "future_widget");
        assert!(!layout.sections[1].enabled);
        // ... and the parsed document still validates.
        assert!(validate_storefront_layout(&layout).is_ok());
    }

    #[test]
    fn wave58_hero_config_caps_and_normalisation() {
        let ok = w58_layout(vec![w58_section(
            "hero",
            SECTION_TYPE_HERO,
            serde_json::json!({
                "headline": "  Eid Collection  ",
                "subtext": "Up to 50% off",
                "image_url": "https://cdn.example.com/hero.png",
                "link_url": "/shop?on_sale=1",
                "link_label": "Shop now",
            }),
        )]);
        let validated = validate_storefront_layout(&ok).expect("hero validates");
        assert_eq!(
            validated.sections[0].config["headline"].as_str(),
            Some("Eid Collection"),
            "text is trimmed on write"
        );
        // Blank headline clears to absent.
        let blank = w58_layout(vec![w58_section(
            "hero",
            SECTION_TYPE_HERO,
            serde_json::json!({ "headline": "   " }),
        )]);
        let validated = validate_storefront_layout(&blank).expect("blank hero validates");
        assert!(validated.sections[0].config.get("headline").is_none());
        // Caps bite at cap+1.
        let long_headline = "h".repeat(MAX_SECTION_HEADLINE_CHARS + 1);
        let layout = w58_layout(vec![w58_section(
            "hero",
            SECTION_TYPE_HERO,
            serde_json::json!({ "headline": long_headline }),
        )]);
        assert!(validate_storefront_layout(&layout)
            .unwrap_err()
            .contains("section 1 (hero)"));
        let long_subtext = "s".repeat(MAX_SECTION_SUBTEXT_CHARS + 1);
        let layout = w58_layout(vec![w58_section(
            "hero",
            SECTION_TYPE_HERO,
            serde_json::json!({ "subtext": long_subtext }),
        )]);
        assert!(validate_storefront_layout(&layout).is_err());
        // A script-scheme link is refused by the link rule.
        let layout = w58_layout(vec![w58_section(
            "hero",
            SECTION_TYPE_HERO,
            serde_json::json!({ "link_url": "javascript:alert(1)" }),
        )]);
        assert!(validate_storefront_layout(&layout).is_err());
        // A wrong JSON type in a known key is an error,
        // never a silent default.
        let layout = w58_layout(vec![w58_section(
            "hero",
            SECTION_TYPE_HERO,
            serde_json::json!({ "headline": 42 }),
        )]);
        assert!(validate_storefront_layout(&layout)
            .unwrap_err()
            .contains("must be a string"));
    }

    #[test]
    fn wave58_rich_text_requires_a_body_within_the_cap() {
        let missing = w58_layout(vec![w58_section(
            "story",
            SECTION_TYPE_RICH_TEXT,
            serde_json::json!({ "heading": "Our story" }),
        )]);
        assert!(validate_storefront_layout(&missing)
            .unwrap_err()
            .contains("needs a body"));
        let long_body = "b".repeat(MAX_SECTION_BODY_CHARS + 1);
        let too_long = w58_layout(vec![w58_section(
            "story",
            SECTION_TYPE_RICH_TEXT,
            serde_json::json!({ "body": long_body }),
        )]);
        assert!(validate_storefront_layout(&too_long).is_err());
        let at_cap = "b".repeat(MAX_SECTION_BODY_CHARS);
        let ok = w58_layout(vec![w58_section(
            "story",
            SECTION_TYPE_RICH_TEXT,
            serde_json::json!({ "body": at_cap }),
        )]);
        assert!(validate_storefront_layout(&ok).is_ok());
    }

    #[test]
    fn wave58_product_rail_rules() {
        // Source is required and must be in the catalogue.
        let missing = w58_layout(vec![w58_section(
            "rail",
            SECTION_TYPE_PRODUCT_RAIL,
            serde_json::json!({}),
        )]);
        assert!(validate_storefront_layout(&missing)
            .unwrap_err()
            .contains("needs a source"));
        let bogus = w58_layout(vec![w58_section(
            "rail",
            SECTION_TYPE_PRODUCT_RAIL,
            serde_json::json!({ "source": "trending" }),
        )]);
        assert!(validate_storefront_layout(&bogus).is_err());
        // Limit: default fills in; 3 and 25 refuse; 4/24 pass.
        let defaulted = validate_storefront_layout(&w58_layout(vec![w58_section(
            "rail",
            SECTION_TYPE_PRODUCT_RAIL,
            serde_json::json!({ "source": "newest" }),
        )]))
        .expect("rail validates");
        assert_eq!(
            defaulted.sections[0].config["limit"].as_i64(),
            Some(DEFAULT_RAIL_LIMIT)
        );
        for (limit, ok) in [(3_i64, false), (4, true), (24, true), (25, false)] {
            let layout = w58_layout(vec![w58_section(
                "rail",
                SECTION_TYPE_PRODUCT_RAIL,
                serde_json::json!({ "source": "newest", "limit": limit }),
            )]);
            assert_eq!(
                validate_storefront_layout(&layout).is_ok(),
                ok,
                "limit {limit}"
            );
        }
        // product_ids on an automatic rail is a seller
        // mistake, refused loudly.
        let stray = w58_layout(vec![w58_section(
            "rail",
            SECTION_TYPE_PRODUCT_RAIL,
            serde_json::json!({
                "source": "newest",
                "product_ids": ["123e4567-e89b-12d3-a456-426614174000"],
            }),
        )]);
        assert!(validate_storefront_layout(&stray)
            .unwrap_err()
            .contains("only applies to a manual rail"));
    }

    #[test]
    fn wave58_manual_rail_rules() {
        let first = "123e4567-e89b-12d3-a456-426614174000";
        let second = "123e4567-e89b-12d3-a456-426614174001";
        // Empty manual rail refused.
        let empty = w58_layout(vec![w58_section(
            "rail",
            SECTION_TYPE_PRODUCT_RAIL,
            serde_json::json!({ "source": "manual", "product_ids": [] }),
        )]);
        assert!(validate_storefront_layout(&empty)
            .unwrap_err()
            .contains("at least one product"));
        // A garbage id refused, naming the id.
        let garbage = w58_layout(vec![w58_section(
            "rail",
            SECTION_TYPE_PRODUCT_RAIL,
            serde_json::json!({ "source": "manual", "product_ids": ["not-an-id"] }),
        )]);
        assert!(validate_storefront_layout(&garbage)
            .unwrap_err()
            .contains("not-an-id"));
        // Duplicates collapse, order preserved.
        let dupes = validate_storefront_layout(&w58_layout(vec![w58_section(
            "rail",
            SECTION_TYPE_PRODUCT_RAIL,
            serde_json::json!({
                "source": "manual",
                "product_ids": [second, first, second],
            }),
        )]))
        .expect("manual rail validates");
        let ids: Vec<&str> = dupes.sections[0].config["product_ids"]
            .as_array()
            .expect("ids array")
            .iter()
            .map(|value| value.as_str().unwrap_or(""))
            .collect();
        assert_eq!(ids, vec![second, first]);
        // The cap: 24 pass, 25 refuse.
        let make_ids = |count: usize| -> Vec<String> {
            (0..count)
                .map(|index| format!("123e4567-e89b-12d3-a456-42661417{index:04}"))
                .collect()
        };
        let at_cap = w58_layout(vec![w58_section(
            "rail",
            SECTION_TYPE_PRODUCT_RAIL,
            serde_json::json!({ "source": "manual", "product_ids": make_ids(24) }),
        )]);
        assert!(validate_storefront_layout(&at_cap).is_ok());
        let over_cap = w58_layout(vec![w58_section(
            "rail",
            SECTION_TYPE_PRODUCT_RAIL,
            serde_json::json!({ "source": "manual", "product_ids": make_ids(25) }),
        )]);
        assert!(validate_storefront_layout(&over_cap).is_err());
    }

    #[test]
    fn wave58_shelf_grid_banner_and_info_rules() {
        // Shelf: a bad collection id refused; null = all.
        let bad = w58_layout(vec![w58_section(
            "shelf",
            SECTION_TYPE_COLLECTION_SHELF,
            serde_json::json!({ "collection_id": "shelf-1" }),
        )]);
        assert!(validate_storefront_layout(&bad)
            .unwrap_err()
            .contains("not a valid id"));
        let all = validate_storefront_layout(&w58_layout(vec![w58_section(
            "shelf",
            SECTION_TYPE_COLLECTION_SHELF,
            serde_json::json!({}),
        )]))
        .expect("all-collections shelf validates");
        assert_eq!(
            all.sections[0].config["limit"].as_i64(),
            Some(DEFAULT_SHELF_LIMIT)
        );
        // Grid: bad mode refused, default is taxonomy.
        let bad_mode = w58_layout(vec![w58_section(
            "grid",
            SECTION_TYPE_CATEGORY_GRID,
            serde_json::json!({ "mode": "brands" }),
        )]);
        assert!(validate_storefront_layout(&bad_mode).is_err());
        let defaulted = validate_storefront_layout(&w58_layout(vec![w58_section(
            "grid",
            SECTION_TYPE_CATEGORY_GRID,
            serde_json::json!({}),
        )]))
        .expect("grid validates");
        assert_eq!(
            defaulted.sections[0].config["mode"].as_str(),
            Some("taxonomy")
        );
        // Banner: image required.
        let no_image = w58_layout(vec![w58_section(
            "banner",
            SECTION_TYPE_IMAGE_BANNER,
            serde_json::json!({ "caption": "Sale" }),
        )]);
        assert!(validate_storefront_layout(&no_image)
            .unwrap_err()
            .contains("needs an image"));
        // Store info: any stray config keys normalise away.
        let info = validate_storefront_layout(&w58_layout(vec![w58_section(
            "info",
            SECTION_TYPE_STORE_INFO,
            serde_json::json!({ "about": "smuggled" }),
        )]))
        .expect("store info validates");
        assert_eq!(info.sections[0].config, serde_json::json!({}));
    }

    #[test]
    fn wave58_faq_rules() {
        let pair = |question: &str, answer: &str| {
            serde_json::json!({ "question": question, "answer": answer })
        };
        // Missing list, empty list, and 11 pairs refuse.
        let missing = w58_layout(vec![w58_section(
            "faq",
            SECTION_TYPE_FAQ,
            serde_json::json!({}),
        )]);
        assert!(validate_storefront_layout(&missing).is_err());
        let empty = w58_layout(vec![w58_section(
            "faq",
            SECTION_TYPE_FAQ,
            serde_json::json!({ "items": [] }),
        )]);
        assert!(validate_storefront_layout(&empty).is_err());
        let eleven: Vec<serde_json::Value> = (0..11)
            .map(|index| pair(&format!("Q{index}?"), "A"))
            .collect();
        let too_many = w58_layout(vec![w58_section(
            "faq",
            SECTION_TYPE_FAQ,
            serde_json::json!({ "items": eleven }),
        )]);
        assert!(validate_storefront_layout(&too_many)
            .unwrap_err()
            .contains("between 1 and 10"));
        // A pair missing its answer names its position.
        let half = w58_layout(vec![w58_section(
            "faq",
            SECTION_TYPE_FAQ,
            serde_json::json!({
                "items": [pair("Q?", "A"), { "question": "Q2?" }],
            }),
        )]);
        assert!(validate_storefront_layout(&half)
            .unwrap_err()
            .contains("FAQ item 2 needs an answer"));
        // Over-long question refused; at-cap passes.
        let long_q = "q".repeat(MAX_FAQ_QUESTION_CHARS + 1);
        let too_long = w58_layout(vec![w58_section(
            "faq",
            SECTION_TYPE_FAQ,
            serde_json::json!({ "items": [pair(&long_q, "A")] }),
        )]);
        assert!(validate_storefront_layout(&too_long).is_err());
        let ok = w58_layout(vec![w58_section(
            "faq",
            SECTION_TYPE_FAQ,
            serde_json::json!({ "items": [pair("Delivery?", "  2 days  ")] }),
        )]);
        let validated = validate_storefront_layout(&ok).expect("faq validates");
        assert_eq!(
            validated.sections[0].config["items"][0]["answer"].as_str(),
            Some("2 days"),
            "answers are trimmed on write"
        );
    }

    #[test]
    fn wave58_section_scheduling_window_and_liveness() {
        // A backwards window is refused by the shared rule.
        let mut backwards = w58_section("hero", SECTION_TYPE_HERO, serde_json::json!({}));
        backwards.starts_at = Some(2_000);
        backwards.ends_at = Some(1_000);
        assert!(validate_storefront_layout(&w58_layout(vec![backwards])).is_err());
        // Liveness: the half-open window, banner-identical.
        let mut section = w58_section("hero", SECTION_TYPE_HERO, serde_json::json!({}));
        assert!(storefront_section_is_live(&section, 500));
        section.enabled = false;
        assert!(!storefront_section_is_live(&section, 500));
        section.enabled = true;
        section.starts_at = Some(1_000);
        section.ends_at = Some(2_000);
        assert!(!storefront_section_is_live(&section, 999));
        assert!(storefront_section_is_live(&section, 1_000), "live AT the start");
        assert!(storefront_section_is_live(&section, 1_999));
        assert!(!storefront_section_is_live(&section, 2_000), "gone AT the end");
    }

    #[test]
    fn wave58_catalogue_covers_every_type_with_labels() {
        let catalogue = storefront_section_catalogue();
        assert_eq!(catalogue.len(), STOREFRONT_SECTION_TYPES.len());
        for (meta, expected) in catalogue.iter().zip(STOREFRONT_SECTION_TYPES.iter()) {
            assert_eq!(meta.section_type, *expected);
            assert!(!meta.label_en.is_empty());
            assert!(!meta.label_bn.is_empty());
            assert!(!meta.description_en.is_empty());
            assert!(!meta.description_bn.is_empty());
        }
    }

    #[test]
    fn wave58_remaining_per_type_caps() {
        // Hero link label cap.
        let long_label = "l".repeat(MAX_SECTION_LINK_LABEL_CHARS + 1);
        let layout = w58_layout(vec![w58_section(
            "hero",
            SECTION_TYPE_HERO,
            serde_json::json!({ "link_label": long_label }),
        )]);
        assert!(validate_storefront_layout(&layout).is_err());
        // Banner caption cap.
        let long_caption = "c".repeat(MAX_SECTION_CAPTION_CHARS + 1);
        let layout = w58_layout(vec![w58_section(
            "banner",
            SECTION_TYPE_IMAGE_BANNER,
            serde_json::json!({
                "image_url": "https://cdn.example.com/b.png",
                "caption": long_caption,
            }),
        )]);
        assert!(validate_storefront_layout(&layout).is_err());
        // Rail/shelf/grid title cap.
        let long_title = "t".repeat(MAX_SECTION_TITLE_CHARS + 1);
        let layout = w58_layout(vec![w58_section(
            "rail",
            SECTION_TYPE_PRODUCT_RAIL,
            serde_json::json!({ "source": "newest", "title": long_title }),
        )]);
        assert!(validate_storefront_layout(&layout).is_err());
        // Grid limit bounds: 3 and 25 refuse, 24 passes;
        // the default fills in when absent.
        for (limit, ok) in [(3_i64, false), (24, true), (25, false)] {
            let layout = w58_layout(vec![w58_section(
                "grid",
                SECTION_TYPE_CATEGORY_GRID,
                serde_json::json!({ "limit": limit }),
            )]);
            assert_eq!(validate_storefront_layout(&layout).is_ok(), ok, "limit {limit}");
        }
        let shelf = validate_storefront_layout(&w58_layout(vec![w58_section(
            "shelf",
            SECTION_TYPE_COLLECTION_SHELF,
            serde_json::json!({
                "collection_id": "123e4567-e89b-12d3-a456-426614174000",
            }),
        )]))
        .expect("shelf validates");
        assert_eq!(
            shelf.sections[0].config["limit"].as_i64(),
            Some(DEFAULT_SHELF_LIMIT)
        );
        assert_eq!(
            shelf.sections[0].config["collection_id"].as_str(),
            Some("123e4567-e89b-12d3-a456-426614174000")
        );
    }

    #[test]
    fn wave58_window_with_a_single_bound() {
        // Only a start: valid, live from the start on.
        let mut section = w58_section("hero", SECTION_TYPE_HERO, serde_json::json!({}));
        section.starts_at = Some(1_000);
        let validated = validate_storefront_layout(&w58_layout(vec![section.clone()]))
            .expect("single-bound window validates");
        assert_eq!(validated.sections[0].ends_at, None);
        assert!(!storefront_section_is_live(&section, 999));
        assert!(storefront_section_is_live(&section, 1_000));
        assert!(storefront_section_is_live(&section, 9_999_999));
        // Only an end: valid, live until the end.
        let mut section = w58_section("hero", SECTION_TYPE_HERO, serde_json::json!({}));
        section.ends_at = Some(2_000);
        assert!(validate_storefront_layout(&w58_layout(vec![section.clone()])).is_ok());
        assert!(storefront_section_is_live(&section, 0));
        assert!(!storefront_section_is_live(&section, 2_000));
    }

    #[test]
    fn wave58_layout_json_round_trip_is_stable() {
        // The stored document serialises and parses back
        // to the same value — the storage contract the API
        // relies on (write the validated document, read it
        // back whole).
        let layout = validate_storefront_layout(&w58_layout(vec![
            w58_section(
                "hero",
                SECTION_TYPE_HERO,
                serde_json::json!({ "headline": "ঈদ মোবারক" }),
            ),
            w58_section(
                "faq",
                SECTION_TYPE_FAQ,
                serde_json::json!({
                    "items": [{ "question": "Q?", "answer": "A." }],
                }),
            ),
        ]))
        .expect("layout validates");
        let text = serde_json::to_string(&layout).expect("serialise");
        let parsed: StorefrontLayout = serde_json::from_str(&text).expect("parse back");
        assert_eq!(parsed, layout);
        // Scheduling survives the round trip as unix
        // seconds in the domain shape.
        let mut scheduled = layout.clone();
        scheduled.sections[0].starts_at = Some(1_700_000_000);
        let value = serde_json::to_value(&scheduled).expect("to value");
        assert_eq!(value["sections"][0]["starts_at"].as_i64(), Some(1_700_000_000));
        let parsed: StorefrontLayout = serde_json::from_value(value).expect("from value");
        assert_eq!(parsed, scheduled);
    }

    #[test]
    fn wave58_ids_trim_and_unknown_sections_keep_their_flags() {
        // A padded id validates and is STORED trimmed —
        // the id is an analytics key and must not fork on
        // stray spaces.
        let layout = w58_layout(vec![w58_section(
            "  hero  ",
            SECTION_TYPE_HERO,
            serde_json::json!({}),
        )]);
        let validated = validate_storefront_layout(&layout).expect("padded id validates");
        assert_eq!(validated.sections[0].id, "hero");
        // An unknown section keeps its enabled flag and
        // its scheduling window untouched — preservation
        // is total, not just the config.
        let mut future = w58_section(
            "ar-block",
            "ar_try_on",
            serde_json::json!({ "model": "v2" }),
        );
        future.enabled = false;
        future.starts_at = Some(1_800_000_000);
        let validated = validate_storefront_layout(&w58_layout(vec![future]))
            .expect("future section validates");
        assert!(!validated.sections[0].enabled);
        assert_eq!(validated.sections[0].starts_at, Some(1_800_000_000));
        assert!(!storefront_section_is_live(&validated.sections[0], 1_800_000_000));
    }

    #[test]
    fn wave58_event_kinds_are_a_closed_vocabulary() {
        assert_eq!(
            validate_storefront_event_kind("section_view"),
            Ok(STOREFRONT_EVENT_SECTION_VIEW)
        );
        assert_eq!(
            validate_storefront_event_kind("section_click"),
            Ok(STOREFRONT_EVENT_SECTION_CLICK)
        );
        for bad in ["view", "click", "section_hover", ""] {
            assert!(validate_storefront_event_kind(bad).is_err(), "{bad:?} refused");
        }
    }

    // -- Wave 59: store content (pages + blog) --

    #[test]
    fn wave59_content_status_machine_allows_any_real_change() {
        // Every move between two distinct valid statuses
        // is allowed; staying put and unknown statuses
        // are not transitions.
        for from in CONTENT_STATUSES {
            for to in CONTENT_STATUSES {
                assert_eq!(
                    can_transition_content_status(from, to),
                    from != to,
                    "{from} -> {to}"
                );
            }
        }
        assert!(!can_transition_content_status("live", "draft"));
        assert!(!can_transition_content_status("draft", "live"));
        assert!(!can_transition_content_status("", ""));
        assert!(is_valid_content_status("published"));
        assert!(!is_valid_content_status("Published"));
    }

    #[test]
    fn wave59_content_liveness_honours_status_and_schedule() {
        let now = 1_700_000_000;
        // Pages: no schedule — the status alone decides.
        assert!(content_is_live("published", None, now));
        assert!(!content_is_live("draft", None, now));
        assert!(!content_is_live("archived", None, now));
        // Posts: a published post whose moment has come
        // is live; a future moment is scheduled, not live;
        // the exact moment counts as live (the window is
        // half-open at the start, like the banner rule).
        assert!(content_is_live("published", Some(now - 1), now));
        assert!(content_is_live("published", Some(now), now));
        assert!(!content_is_live("published", Some(now + 60), now));
        // A draft with a past date is still a draft.
        assert!(!content_is_live("draft", Some(now - 1), now));
    }

    #[test]
    fn wave59_content_slug_uses_the_platform_grammar() {
        assert_eq!(
            validate_content_slug("shipping-info"),
            Ok("shipping-info".to_string())
        );
        assert_eq!(validate_content_slug("eid-2026"), Ok("eid-2026".to_string()));
        for bad in ["ab", "Has-Caps", "-leading", "trailing-", "under_score", ""] {
            assert!(validate_content_slug(bad).is_err(), "{bad:?} refused");
        }
        let err = validate_content_slug("NO").expect_err("caps refused");
        assert!(err.contains("স্লাগ"), "bilingual refusal: {err}");
    }

    #[test]
    fn wave59_content_titles_trim_and_cap() {
        assert_eq!(
            validate_store_page_title("  ডেলিভারি তথ্য  "),
            Ok("ডেলিভারি তথ্য".to_string())
        );
        assert!(validate_store_page_title("   ").is_err());
        assert!(validate_store_post_title("").is_err());
        let at_cap = "a".repeat(MAX_STORE_PAGE_TITLE_CHARS);
        assert!(validate_store_page_title(&at_cap).is_ok());
        let over_cap = "a".repeat(MAX_STORE_PAGE_TITLE_CHARS + 1);
        assert!(validate_store_page_title(&over_cap).is_err());
        // The post title cap is its own, larger one.
        let post_over_page_cap = "a".repeat(MAX_STORE_PAGE_TITLE_CHARS + 1);
        assert!(validate_store_post_title(&post_over_page_cap).is_ok());
        let post_over = "a".repeat(MAX_STORE_POST_TITLE_CHARS + 1);
        assert!(validate_store_post_title(&post_over).is_err());
    }

    #[test]
    fn wave59_content_bodies_require_text_and_cap_chars() {
        assert_eq!(
            validate_store_page_body("  প্রথম লাইন।\n\nদ্বিতীয় লাইন।  "),
            Ok("প্রথম লাইন।\n\nদ্বিতীয় লাইন।".to_string())
        );
        assert!(validate_store_page_body(" \n ").is_err());
        assert!(validate_store_post_body("").is_err());
        let page_at_cap = "x".repeat(MAX_STORE_PAGE_BODY_CHARS);
        assert!(validate_store_page_body(&page_at_cap).is_ok());
        // A page-overflowing body still fits a post.
        let big = "x".repeat(MAX_STORE_PAGE_BODY_CHARS + 1);
        assert!(validate_store_page_body(&big).is_err());
        assert!(validate_store_post_body(&big).is_ok());
        let post_over = "x".repeat(MAX_STORE_POST_BODY_CHARS + 1);
        assert!(validate_store_post_body(&post_over).is_err());
        // Interior whitespace is the author's — untouched.
        let body = validate_store_post_body("a\n\n\nb").expect("body validates");
        assert_eq!(body, "a\n\n\nb");
    }

    #[test]
    fn wave59_content_count_caps() {
        assert!(validate_store_page_count(0).is_ok());
        assert!(validate_store_page_count(MAX_STORE_PAGES_PER_STORE - 1).is_ok());
        assert!(validate_store_page_count(MAX_STORE_PAGES_PER_STORE).is_err());
        assert!(validate_store_page_count(-1).is_err());
        assert!(validate_store_post_count(MAX_STORE_POSTS_PER_STORE - 1).is_ok());
        assert!(validate_store_post_count(MAX_STORE_POSTS_PER_STORE).is_err());
    }

    #[test]
    fn wave59_tags_normalise_case_space_and_duplicates() {
        let tags = normalize_post_tags(&[
            "Eid Collection".to_string(),
            "eid-collection".to_string(),
            "  নতুন  ".to_string(),
            "".to_string(),
            "   ".to_string(),
            "Sale 2026".to_string(),
        ])
        .expect("tags normalise");
        assert_eq!(tags, ["eid-collection", "নতুন", "sale-2026"]);
        // Multi-space interiors collapse to one hyphen.
        let tags = normalize_post_tags(&["a   b\tc".to_string()]).expect("collapse");
        assert_eq!(tags, ["a-b-c"]);
        // No tags at all is fine — a post need not carry any.
        assert_eq!(normalize_post_tags(&[]), Ok(vec![]));
    }

    #[test]
    fn wave59_tags_refuse_overflow_and_bad_characters() {
        let too_many: Vec<String> = (0..=MAX_POST_TAGS).map(|i| format!("tag{i}")).collect();
        assert!(normalize_post_tags(&too_many).is_err());
        let at_cap: Vec<String> = (0..MAX_POST_TAGS).map(|i| format!("tag{i}")).collect();
        assert!(normalize_post_tags(&at_cap).is_ok());
        let long = "x".repeat(MAX_POST_TAG_CHARS + 1);
        assert!(normalize_post_tags(&[long]).is_err());
        for bad in ["tag!", "a,b", "tag@home", "under_score"] {
            assert!(
                normalize_post_tags(&[bad.to_string()]).is_err(),
                "{bad:?} refused"
            );
        }
    }

    #[test]
    fn wave59_excerpt_derivation_cuts_on_word_boundaries() {
        // A short body is its own excerpt, whitespace
        // collapsed.
        assert_eq!(derive_post_excerpt("hello   world\n\nagain"), "hello world again");
        assert_eq!(derive_post_excerpt(""), "");
        // A long body ends at the last word boundary
        // inside the budget — never mid-word, never with
        // an ellipsis.
        let word = "abcde";
        let body = std::iter::repeat_n(word, 100).collect::<Vec<_>>().join(" ");
        let excerpt = derive_post_excerpt(&body);
        assert!(excerpt.chars().count() <= POST_EXCERPT_DERIVE_CHARS);
        assert!(excerpt.ends_with(word), "ends on a whole word: {excerpt}");
        assert!(!excerpt.contains('…'), "no ellipsis fakery");
        assert!(body.starts_with(&excerpt));
        // One unbroken word longer than the budget is
        // hard-cut at the budget exactly.
        let solid = "z".repeat(POST_EXCERPT_DERIVE_CHARS + 50);
        let excerpt = derive_post_excerpt(&solid);
        assert_eq!(excerpt.chars().count(), POST_EXCERPT_DERIVE_CHARS);
    }

    #[test]
    fn wave59_manual_excerpt_blank_means_derive() {
        assert_eq!(validate_post_excerpt(None), Ok(None));
        assert_eq!(validate_post_excerpt(Some("   ")), Ok(None));
        assert_eq!(
            validate_post_excerpt(Some("  নিজের লেখা সারাংশ  ")),
            Ok(Some("নিজের লেখা সারাংশ".to_string()))
        );
        let over = "x".repeat(MAX_POST_EXCERPT_CHARS + 1);
        assert!(validate_post_excerpt(Some(&over)).is_err());
    }

    #[test]
    fn wave59_reading_time_rounds_up_with_a_floor_of_one() {
        assert_eq!(count_words("  one two\nthree\tfour "), 4);
        assert_eq!(count_words(""), 0);
        assert_eq!(reading_minutes_for_words(0), 1);
        assert_eq!(reading_minutes_for_words(1), 1);
        assert_eq!(reading_minutes_for_words(READING_WORDS_PER_MINUTE), 1);
        assert_eq!(reading_minutes_for_words(READING_WORDS_PER_MINUTE + 1), 2);
        assert_eq!(estimate_reading_minutes("word ".repeat(450).as_str()), 3);
    }

    #[test]
    fn wave59_xml_escape_covers_the_five_entities() {
        assert_eq!(
            xml_escape("Fish & Chips <b>\"tasty\"</b> 'yes'"),
            "Fish &amp; Chips &lt;b&gt;&quot;tasty&quot;&lt;/b&gt; &apos;yes&apos;"
        );
        // Ampersand escapes first — an existing entity in
        // the text stays literal text, never live markup.
        assert_eq!(xml_escape("&lt;"), "&amp;lt;");
        assert_eq!(xml_escape("সাধারণ লেখা"), "সাধারণ লেখা");
        assert_eq!(xml_escape(""), "");
    }

    // ---- Wave 60: gift cards ----

    fn gift_group(
        store: &str,
        items: Vec<CartItem>,
        discount_poisha: i64,
        commission_on_discounted: bool,
        gift_card_discount_poisha: i64,
    ) -> OrderGroupInput {
        OrderGroupInput {
            store_id: store.to_string(),
            items,
            discount_poisha,
            gift_card_discount_poisha,
            commission_on_discounted,
            commission_rate_bps: None,
        }
    }

    #[test]
    fn wave60_code_normalisation_accepts_printed_forms() {
        // As issued: prefix in display case, uppercase body.
        assert_eq!(
            normalize_gift_card_code("bdgc_K7QX2M9P4RT8VW3ZA6BC5N1D"),
            Some("BDGC_K7QX2M9P4RT8VW3ZA6BC5N1D".to_string())
        );
        // As typed off a printed card: grouped with spaces
        // and hyphens, lowercase letters — same canonical
        // form, so the same hash, so the same card.
        assert_eq!(
            normalize_gift_card_code(" bdgc_K7QX-2M9P 4RT8-VW3Z A6BC 5N1D "),
            Some("BDGC_K7QX2M9P4RT8VW3ZA6BC5N1D".to_string())
        );
    }

    #[test]
    fn wave60_code_normalisation_rejects_non_codes() {
        assert_eq!(normalize_gift_card_code(""), None);
        // A Wave 37 API key is not a gift card.
        assert_eq!(
            normalize_gift_card_code("bdm_live_K7QX2M9P4RT8VW3ZA6BC5N1DK7QX2M9P4RT8VW3Z"),
            None
        );
        // Too short, too long, bad characters, wrong prefix.
        assert_eq!(normalize_gift_card_code("bdgc_K7QX"), None);
        assert_eq!(
            normalize_gift_card_code("bdgc_K7QX2M9P4RT8VW3ZA6BC5N1DEXTRA"),
            None
        );
        assert_eq!(
            normalize_gift_card_code("bdgc_K7QX2M9P4RT8VW3ZA6BC5N!D"),
            None
        );
        assert_eq!(
            normalize_gift_card_code("xxxx_K7QX2M9P4RT8VW3ZA6BC5N1D"),
            None
        );
    }

    #[test]
    fn wave60_code_display_helpers() {
        let code = "bdgc_K7QX2M9P4RT8VW3ZA6BC5N1D";
        assert_eq!(gift_card_display_prefix(code), "bdgc_K7QX2M9");
        assert_eq!(gift_card_last4(code), "5N1D");
        // Short inputs return whole instead of panicking.
        assert_eq!(gift_card_display_prefix("bdgc_"), "bdgc_");
        assert_eq!(gift_card_last4("ab"), "ab");
    }

    #[test]
    fn wave60_issue_validation_enforces_the_caps() {
        assert!(validate_gift_card_issue(GIFT_CARD_MIN_ISSUE_POISHA, None, None, None).is_ok());
        assert!(validate_gift_card_issue(GIFT_CARD_MAX_ISSUE_POISHA, None, None, None).is_ok());
        assert!(validate_gift_card_issue(GIFT_CARD_MIN_ISSUE_POISHA - 1, None, None, None).is_err());
        assert!(validate_gift_card_issue(GIFT_CARD_MAX_ISSUE_POISHA + 1, None, None, None).is_err());
        assert!(validate_gift_card_issue(0, None, None, None).is_err());
        assert!(validate_gift_card_issue(-100, None, None, None).is_err());
    }

    #[test]
    fn wave60_issue_validation_checks_texts_and_validity() {
        let note_ok = "x".repeat(GIFT_CARD_MAX_NOTE_CHARS);
        let note_long = "x".repeat(GIFT_CARD_MAX_NOTE_CHARS + 1);
        assert!(validate_gift_card_issue(10_000, Some(&note_ok), None, None).is_ok());
        assert!(validate_gift_card_issue(10_000, Some(&note_long), None, None).is_err());
        let name_long = "x".repeat(GIFT_CARD_MAX_RECIPIENT_CHARS + 1);
        assert!(validate_gift_card_issue(10_000, None, Some(&name_long), None).is_err());
        assert!(validate_gift_card_issue(10_000, None, None, Some(1)).is_ok());
        assert!(validate_gift_card_issue(10_000, None, None, Some(GIFT_CARD_MAX_VALIDITY_DAYS)).is_ok());
        // Zero validity is NOT "no expiry" — it is an error.
        assert!(validate_gift_card_issue(10_000, None, None, Some(0)).is_err());
        assert!(validate_gift_card_issue(10_000, None, None, Some(-5)).is_err());
        assert!(
            validate_gift_card_issue(10_000, None, None, Some(GIFT_CARD_MAX_VALIDITY_DAYS + 1))
                .is_err()
        );
    }

    #[test]
    fn wave60_bulk_count_bounds() {
        assert!(validate_gift_card_bulk_count(1).is_ok());
        assert!(validate_gift_card_bulk_count(GIFT_CARD_MAX_BULK_ISSUE).is_ok());
        assert!(validate_gift_card_bulk_count(0).is_err());
        assert!(validate_gift_card_bulk_count(GIFT_CARD_MAX_BULK_ISSUE + 1).is_err());
    }

    #[test]
    fn wave60_adjust_validation() {
        assert_eq!(validate_gift_card_adjust(50_000, 10_000), Ok(60_000));
        assert_eq!(validate_gift_card_adjust(50_000, -50_000), Ok(0));
        assert!(validate_gift_card_adjust(50_000, 0).is_err());
        // Never below zero, never wrapped.
        assert!(validate_gift_card_adjust(50_000, -50_001).is_err());
        assert!(validate_gift_card_adjust(i64::MAX, 1).is_err());
    }

    #[test]
    fn wave60_applicable_is_the_smaller_side_floored_at_zero() {
        // Balance smaller: the whole balance applies.
        assert_eq!(gift_card_applicable_poisha(30_000, 200_000), 30_000);
        // Goods smaller: the card covers the goods exactly.
        assert_eq!(gift_card_applicable_poisha(500_000, 200_000), 200_000);
        // Equal sides apply fully.
        assert_eq!(gift_card_applicable_poisha(200_000, 200_000), 200_000);
        // Zero or negative on either side applies nothing.
        assert_eq!(gift_card_applicable_poisha(0, 200_000), 0);
        assert_eq!(gift_card_applicable_poisha(200_000, 0), 0);
        assert_eq!(gift_card_applicable_poisha(-5, 200_000), 0);
        assert_eq!(gift_card_applicable_poisha(200_000, -5), 0);
    }

    #[test]
    fn wave60_restore_for_return_is_proportional_floored_and_capped() {
        // Whole-sub-order return: refund == basis, so the
        // FULL redemption comes back.
        assert_eq!(gift_card_restore_for_return(40_000, 160_000, 160_000), 40_000);
        // Half the basis refunded: half the redemption,
        // floored (40_001 / 2 would be 20_000.5 → 20_000).
        assert_eq!(gift_card_restore_for_return(40_001, 80_000, 160_000), 20_000);
        // A refund above the basis (corrupt snapshot) can
        // never restore more than was redeemed.
        assert_eq!(gift_card_restore_for_return(40_000, 999_999, 160_000), 40_000);
        // Degenerate inputs restore nothing.
        assert_eq!(gift_card_restore_for_return(0, 160_000, 160_000), 0);
        assert_eq!(gift_card_restore_for_return(40_000, 0, 160_000), 0);
        assert_eq!(gift_card_restore_for_return(40_000, 160_000, 0), 0);
        assert_eq!(gift_card_restore_for_return(40_000, -1, 160_000), 0);
    }

    #[test]
    fn wave60_expiry_is_time_derived_with_an_inclusive_boundary() {
        assert!(!gift_card_is_expired(None, 1_700_000_000));
        assert!(!gift_card_is_expired(Some(1_700_000_100), 1_700_000_000));
        // At the expiry moment itself the card is dead.
        assert!(gift_card_is_expired(Some(1_700_000_000), 1_700_000_000));
        assert!(gift_card_is_expired(Some(1_699_999_999), 1_700_000_000));
    }

    #[test]
    fn wave60_status_precedence() {
        let now = 1_700_000_000;
        let future = Some(now + 86_400);
        let past = Some(now - 1);
        assert_eq!(gift_card_status(false, 50_000, future, now), GIFT_CARD_STATUS_ACTIVE);
        assert_eq!(gift_card_status(false, 50_000, None, now), GIFT_CARD_STATUS_ACTIVE);
        assert_eq!(gift_card_status(false, 0, future, now), GIFT_CARD_STATUS_DEPLETED);
        assert_eq!(gift_card_status(false, 50_000, past, now), GIFT_CARD_STATUS_EXPIRED);
        // Expired beats depleted: money remains but is dead.
        assert_eq!(gift_card_status(false, 0, past, now), GIFT_CARD_STATUS_EXPIRED);
        // Disabled beats everything.
        assert_eq!(gift_card_status(true, 50_000, future, now), GIFT_CARD_STATUS_DISABLED);
        assert_eq!(gift_card_status(true, 0, past, now), GIFT_CARD_STATUS_DISABLED);
    }

    #[test]
    fn wave60_redeemable_only_when_active() {
        let now = 1_700_000_000;
        assert!(gift_card_is_redeemable(false, 1, None, now));
        assert!(!gift_card_is_redeemable(false, 0, None, now));
        assert!(!gift_card_is_redeemable(true, 50_000, None, now));
        assert!(!gift_card_is_redeemable(false, 50_000, Some(now), now));
    }

    #[test]
    fn wave60_expiry_unix_adds_days() {
        let now = 1_700_000_000;
        assert_eq!(gift_card_expiry_unix(now, 1), now + 86_400);
        assert_eq!(gift_card_expiry_unix(now, 365), now + 365 * 86_400);
        assert_eq!(gift_card_expiry_unix(now, GIFT_CARD_MAX_VALIDITY_DAYS), now + 3_650 * 86_400);
    }

    #[test]
    fn wave60_totals_gift_card_is_seller_funded_with_a_platform_coupon() {
        // Goods 2,000 BDT, platform coupon 200 BDT off
        // (commission stays on the full goods BEFORE the
        // gift card), gift card 500 BDT. Commission 8%:
        // base = 200_000 − 50_000 = 150_000 → 12_000.
        // Payout = 200_000 − 20_000 − 50_000 − 12_000.
        let groups = vec![gift_group(
            "store-a",
            vec![item("store-a", "p1", 1, 2_000)],
            20_000,
            false,
            50_000,
        )];
        let totals = build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).expect("totals");
        let sub = &totals.sub_orders[0];
        assert_eq!(sub.gift_card_discount_poisha, 50_000);
        assert_eq!(sub.commission_poisha, 12_000);
        assert_eq!(sub.seller_payout_poisha, 118_000);
        assert_eq!(totals.gift_card_discount_total_poisha, 50_000);
        // Buyer pays goods − coupon − card + delivery.
        assert_eq!(totals.grand_total_poisha, 200_000 - 20_000 - 50_000 + 6_000);
    }

    #[test]
    fn wave60_totals_gift_card_with_a_store_coupon() {
        // Store coupon: the commission base is the
        // discounted goods, and the card comes off that:
        // base = 200_000 − 20_000 − 50_000 = 130_000 →
        // commission 10_400; payout identical either way.
        let groups = vec![gift_group(
            "store-a",
            vec![item("store-a", "p1", 1, 2_000)],
            20_000,
            true,
            50_000,
        )];
        let totals = build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).expect("totals");
        let sub = &totals.sub_orders[0];
        assert_eq!(sub.commission_poisha, 10_400);
        assert_eq!(sub.seller_payout_poisha, 200_000 - 20_000 - 50_000 - 10_400);
    }

    #[test]
    fn wave60_totals_gift_card_clamps_to_post_coupon_goods() {
        // A card bigger than the post-coupon goods clamps:
        // it can never eat the coupon or the delivery.
        let groups = vec![gift_group(
            "store-a",
            vec![item("store-a", "p1", 1, 2_000)],
            20_000,
            true,
            999_999,
        )];
        let totals = build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).expect("totals");
        let sub = &totals.sub_orders[0];
        assert_eq!(sub.gift_card_discount_poisha, 180_000);
        assert_eq!(sub.commission_poisha, 0);
        assert_eq!(sub.seller_payout_poisha, 0);
        // The buyer still pays the delivery charge in full.
        assert_eq!(totals.grand_total_poisha, 6_000);
        // A negative stored redemption clamps to zero.
        let groups = vec![gift_group(
            "store-a",
            vec![item("store-a", "p1", 1, 2_000)],
            0,
            false,
            -500,
        )];
        let totals = build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).expect("totals");
        assert_eq!(totals.sub_orders[0].gift_card_discount_poisha, 0);
        assert_eq!(totals.grand_total_poisha, 206_000);
    }

    #[test]
    fn wave60_totals_without_a_card_are_unchanged() {
        // The Wave 14 worked numbers still hold when no
        // card is present — the extension is inert.
        let groups = vec![discounted_group(
            "store-a",
            vec![item("store-a", "p1", 1, 2_000)],
            20_000,
            true,
        )];
        let totals = build_order_totals(&groups, DeliveryZone::InsideDhaka, 800).expect("totals");
        assert_eq!(totals.gift_card_discount_total_poisha, 0);
        assert_eq!(totals.sub_orders[0].gift_card_discount_poisha, 0);
        assert_eq!(totals.sub_orders[0].commission_poisha, 14_400);
        assert_eq!(totals.grand_total_poisha, 186_000);
    }

    #[test]
    fn wave60_restore_for_return_rounding_table() {
        // Whole-sub-order returns (refund == basis) restore
        // the full redemption, at any scale.
        assert_eq!(gift_card_restore_for_return(50_000, 180_000, 180_000), 50_000);
        assert_eq!(gift_card_restore_for_return(1, 1, 1), 1);
        // The proportion floors: a third of the basis
        // refunds a third of the redemption, rounded DOWN —
        // rounding never invents value.
        assert_eq!(gift_card_restore_for_return(100, 1, 3), 33);
        assert_eq!(gift_card_restore_for_return(100, 2, 3), 66);
        assert_eq!(gift_card_restore_for_return(99, 1, 2), 49);
        // A refund above the basis (a data anomaly, never a
        // rule output) clamps to the redemption itself.
        assert_eq!(gift_card_restore_for_return(100, 500, 300), 100);
        // Degenerate inputs restore nothing.
        assert_eq!(gift_card_restore_for_return(0, 100, 100), 0);
        assert_eq!(gift_card_restore_for_return(100, 0, 100), 0);
        assert_eq!(gift_card_restore_for_return(100, 100, 0), 0);
        assert_eq!(gift_card_restore_for_return(-5, 100, 100), 0);
        assert_eq!(gift_card_restore_for_return(100, -5, 100), 0);
        // i128 headroom: the largest issuable redemption
        // against the largest plausible basis cannot
        // overflow on the way to the division.
        assert_eq!(
            gift_card_restore_for_return(5_000_000, i64::MAX / 2, i64::MAX / 2),
            5_000_000
        );
    }

    #[test]
    fn wave60_status_precedence_matrix() {
        let now = 1_800_000_000_i64;
        let past = Some(now - 1);
        let future = Some(now + 86_400);
        // Disabled beats everything, even expired + empty.
        assert_eq!(gift_card_status(true, 0, past, now), "disabled");
        assert_eq!(gift_card_status(true, 5_000, future, now), "disabled");
        // Expired beats depleted: a dead card with money
        // left reports the actionable fact.
        assert_eq!(gift_card_status(false, 0, past, now), "expired");
        assert_eq!(gift_card_status(false, 5_000, past, now), "expired");
        // The boundary is inclusive: expiring AT now is dead.
        assert_eq!(gift_card_status(false, 5_000, Some(now), now), "expired");
        assert!(gift_card_is_expired(Some(now), now));
        assert!(!gift_card_is_expired(Some(now + 1), now));
        // Depleted: live window, nothing left.
        assert_eq!(gift_card_status(false, 0, future, now), "depleted");
        assert_eq!(gift_card_status(false, 0, None, now), "depleted");
        // Active: money left, no freeze, no expiry reached
        // (or none at all — a no-expiry card never dies).
        assert_eq!(gift_card_status(false, 1, future, now), "active");
        assert_eq!(gift_card_status(false, 1, None, now), "active");
        assert!(!gift_card_is_expired(None, i64::MAX - 1));
        // Redeemable is exactly "derived status is active".
        assert!(gift_card_is_redeemable(false, 1, None, now));
        assert!(!gift_card_is_redeemable(false, 0, None, now));
        assert!(!gift_card_is_redeemable(true, 1, None, now));
        assert!(!gift_card_is_redeemable(false, 1, past, now));
    }

    #[test]
    fn wave60_normalize_code_table() {
        // The canonical form passes through untouched
        // (bdgc_ + exactly 24 random characters = 29).
        assert_eq!(
            normalize_gift_card_code("BDGC_K7QX2M9P4T6W8Z3N5R1V0ABC").as_deref(),
            Some("BDGC_K7QX2M9P4T6W8Z3N5R1V0ABC")
        );
        // Case, spaces and hyphen grouping all normalise.
        assert_eq!(
            normalize_gift_card_code("bdgc_k7qx2m9p4t6w8z3n5r1v0abc").as_deref(),
            Some("BDGC_K7QX2M9P4T6W8Z3N5R1V0ABC")
        );
        assert_eq!(
            normalize_gift_card_code(" bdgc_K7QX-2M9P 4T6W-8Z3N 5R1V0ABC ").as_deref(),
            Some("BDGC_K7QX2M9P4T6W8Z3N5R1V0ABC")
        );
        // Wrong prefix, wrong length, foreign characters
        // and empty input are not codes this platform made.
        assert_eq!(normalize_gift_card_code("XXXX_K7QX2M9P4T6W8Z3N5R1V0ABC"), None);
        assert_eq!(normalize_gift_card_code("BDGC_K7QX"), None);
        assert_eq!(normalize_gift_card_code("BDGC_K7QX2M9P4T6W8Z3N5R1V0ABCD"), None);
        assert_eq!(normalize_gift_card_code("BDGC_K7QX2M9P4T6W8Z3N5R1V0AB!"), None);
        assert_eq!(normalize_gift_card_code(""), None);
        assert_eq!(normalize_gift_card_code("   "), None);
        assert_eq!(normalize_gift_card_code("bdgc_"), None);
    }

    #[test]
    fn wave60_expiry_reminder_template_renders_last4_and_balance() {
        // The reminder body names the store, the card's
        // last4 (never more of the code) and the balance at
        // risk — and renders sanely without an amount.
        let vars = NotificationVars {
            store_name: "ঢাকা ফ্যাশন হাউস".to_string(),
            amount_poisha: Some(50_000),
            card_last4: "5N1D".to_string(),
            ..NotificationVars::default()
        };
        let body = render_body(NOTIFICATION_GIFT_CARD_EXPIRING, &vars);
        assert!(body.contains("ঢাকা ফ্যাশন হাউস"), "{body}");
        assert!(body.contains("••5N1D"), "{body}");
        assert!(body.contains("৳500"), "{body}");
        let bare = NotificationVars {
            store_name: "ঢাকা ফ্যাশন হাউস".to_string(),
            card_last4: "5N1D".to_string(),
            ..NotificationVars::default()
        };
        let body = render_body(NOTIFICATION_GIFT_CARD_EXPIRING, &bare);
        assert!(body.contains("••5N1D"), "{body}");
        // The sweep window is a positive week.
        assert_eq!(GIFT_CARD_EXPIRY_REMINDER_WINDOW_DAYS, 7);
    }
}
