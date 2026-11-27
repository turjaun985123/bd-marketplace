//! Orders: checkout (COD), buyer order history, and seller sub-orders.
//!
//! Checkout is the heart of the marketplace: ONE transaction that locks
//! the cart's product rows, validates stock, splits the cart per seller
//! (`split_cart_by_seller`), computes every money figure with the pure
//! domain function `build_order_totals` (which itself reuses the Wave 1
//! delivery-charge and commission functions), writes the order, its
//! sub_orders and order_items with price/title snapshots, decrements
//! stock and clears the cart. If any step fails the whole checkout
//! rolls back — a buyer
//! can never be charged for half an order.
//!
//! Wave 3 payment rule: COD only. Other methods are rejected with a
//! clear 400 (online payments land in Wave 5).
//!
//! Wave 5: checkout accepts every known payment method. COD orders
//! are stored with `payment_status = 'pay_on_delivery'` and no
//! payment row. An online method is initiated through the payments
//! factory inside the same transaction: a configured or
//! dev-simulated gateway creates a `pending` payment row and the
//! order is stored with `payment_status = 'pending'`, while an
//! unconfigured gateway fails the whole checkout with a 503 and the
//! transaction rolls back (cart and stock untouched). Settlement
//! happens later, only via the signed webhook in the payments module.
//!
//! Wave 4: delivery charges are weight-based (each product carries
//! `weight_grams`; a sub-order's parcel weight is the sum of unit
//! weight times quantity, billed per started kilogram by the domain
//! functions). Buyers can cancel an order while every sub-order is
//! still `placed`, which restores stock for all items in the same
//! transaction; a seller cancelling a sub-order likewise restores that
//! sub-order's stock. A buyer receipt view is served per order.
//!
//! Wave 14: checkout accepts an optional coupon code. The coupon
//! row is locked and re-validated inside the checkout transaction
//! (the shared rule in the `coupons` module); the discount is
//! allocated across the in-scope seller groups by the pure domain
//! functions before totals are computed, so every money figure —
//! the buyer's grand total, each sub-order's payout snapshot, the
//! payment amount — comes from the one discounted totals run. The
//! redemption row and the coupon's `used_count` increment commit
//! or roll back with the order.
//!
//! Wave 17: COD checkout also runs the advance decision (see the
//! block in `place_order`): the pure domain rule over the
//! effective policy and the buyer's risk verdict can require a
//! partial advance, collected online through the same payments
//! factory before any seller may ship. The receipt's cash-due
//! figure nets out a collected advance. Cancelling an
//! `advance_pending` order stays a plain buyer cancel — its
//! payment row is still `pending` (no money ever moved) and is
//! left as the audit record of the abandoned advance.
//!
//! Wave 33: checkout understands variant lines. A line's
//! identity is (product, variant); variant rows are locked
//! alongside the product rows, the line enters the totals at
//! its FINAL price (base + delta, the domain rule), the
//! order item snapshots the variant id and name, and the
//! stock decrement lands on the VARIANT row with a
//! variant-labelled audit row — the product's own stock is
//! never touched by a variant line. The shared restock
//! helper mirrors this: variant lines restock their variant.
//! A variant-less line for a product that has since gained
//! active variants is refused (the buyer must re-pick), so a
//! stale cart can never draw on stock the seller moved into
//! variants. Delivery weight stays the product's — variants
//! carry no weight of their own.
//!
//! Wave 35: checkout also redeems loyalty points. After the
//! coupon is applied, the buyer's requested points are clamped
//! to the redeemable maximum (their balance, capped at half
//! the post-coupon goods — the pure domain rule) and the
//! discount is allocated across the seller groups by the same
//! `allocate_discount` the coupon uses, snapshotted per
//! sub-order. The stored grand total is the REDUCED payable:
//! the advance decision, the online payment amount and the
//! receipt's cash-due all read it, so every figure the buyer
//! sees already nets the points out. The platform funds the
//! redemption — `seller_payout_poisha` and the commission
//! basis are computed from the pre-loyalty totals and never
//! change. The ledger side (balance, earn, refunds, clawback)
//! lives in the `loyalty` module; checkout writes only the
//! `redeem` row, inside this transaction, under a lock on the
//! buyer's user row so concurrent checkouts cannot spend the
//! same points twice.
//!
//! Wave 55: checkout gains three additive inputs, none of
//! which touches the money canon above. `address_id` checks
//! out against one of the buyer's saved addresses (the
//! `addresses` module): the order's delivery fields and
//! zone come from THAT row — the client-sent fields are
//! ignored on this path — and the address's `last_used_at`
//! is stamped inside the checkout transaction. Without an
//! address, the optional `delivery_district` names the
//! district (validated against the domain district table)
//! and the zone DERIVES from it; without either, the
//! pre-Wave-55 body-fields path runs byte-identically.
//! `order_note` is the buyer's note to the sellers
//! (domain-capped at 500 chars), stored on the order and
//! surfaced on every seller view. Saved-for-later cart
//! lines (the cart module's `saved` flag) are invisible to
//! checkout: the lock scan and the post-checkout clear
//! both filter them out. This module also gains REORDER
//! ("buy again"): a past order's lines re-added at TODAY's
//! prices through the cart module's one add path, with a
//! per-line report — the domain's `reorder_decision` names
//! every skip, and old prices are never resurrected.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    allocate_discount, build_order_totals, can_buyer_cancel, can_transition_sub_order,
    compose_delivery_address, compute_advance_poisha, compute_coupon_discount, district_by_name,
    is_valid_sub_order_status, loyalty_discount_poisha, loyalty_max_redeemable_points,
    normalize_bd_phone, normalize_coupon_code, parse_delivery_zone, reorder_decision,
    split_cart_by_seller, validate_loyalty_request, validate_order_note, variant_final_price,
    CartItem as DomainCartItem, DeliveryZone, OrderGroupInput, ReorderDecision,
    ReorderVariantState,
};
use payments::{payment_adapter_for, parse_payment_method, PaymentMethod};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use std::collections::HashMap;
use uuid::Uuid;

use crate::access::{accessible_store_ids, require_role, store_role_on};
use crate::auth::AuthUser;
use crate::coupons::{
    coupon_rejection_reason, eligible_subtotal_for, Coupon, GroupGoods, COUPON_COLUMNS,
};
use crate::payments::{
    advance_gateway_available, insert_pending_payment, load_latest_payment_summary, PaymentSummary,
};
use crate::risk::buyer_risk_for_user;
use crate::products::clamp_pagination;
use crate::shipments::{load_shipment_summary_for_sub_order, ShipmentSummary};
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// One purchased line, with the title/price snapshot taken at checkout.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct OrderItem {
    pub id: Uuid,
    pub sub_order_id: Uuid,
    pub product_id: Uuid,
    /// The variant bought (Wave 33), or null for a
    /// variant-less line. The restock paths restore stock to
    /// this variant when it is set.
    pub variant_id: Option<Uuid>,
    /// The variant's name as it was at checkout (Wave 33) —
    /// a snapshot like `title_snapshot`, so a later rename
    /// never rewrites the order.
    pub variant_name: Option<String>,
    pub title_snapshot: String,
    pub unit_price_poisha: i64,
    pub quantity: i32,
    pub line_total_poisha: i64,
    /// The product's CURRENT primary image URL (Wave 51
    /// extension) — a live thumbnail for the order and
    /// invoice pages, filled by `load_order_items` in one
    /// batch query. Deliberately NOT a snapshot: the title
    /// and price freeze at checkout (they are the contract),
    /// while the photo is presentation — if the seller
    /// replaces it, the newer photo shows. Null when the
    /// product has no image.
    #[sqlx(default)]
    pub image_url: Option<String>,
}

/// One seller's slice of an order, with its own status and money split.
#[derive(Debug, Clone, Serialize)]
pub struct SubOrder {
    pub id: Uuid,
    pub order_id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub status: String,
    pub goods_subtotal_poisha: i64,
    /// Coupon discount allocated to this sub-order (Wave 14).
    pub discount_poisha: i64,
    /// Loyalty discount allocated to this sub-order (Wave 35):
    /// the buyer's redeemed points, spread over the groups by
    /// post-coupon goods. Platform-funded — it never reduces
    /// the seller payout below.
    pub loyalty_discount_poisha: i64,
    pub delivery_charge_poisha: i64,
    pub commission_poisha: i64,
    pub seller_payout_poisha: i64,
    pub created_at: DateTime<Utc>,
    pub items: Vec<OrderItem>,
    /// The sub-order's event timeline (Wave 55), built by
    /// the `timeline` module from the shipment log, the
    /// return request and the status — the same events
    /// the public tracking view shows.
    pub timeline: Vec<crate::timeline::TimelineEvent>,
    /// The tracking stepper (Wave 55): the domain's one
    /// derivation over this sub-order's status + courier
    /// leg, served so every client renders the same steps.
    pub steps: Vec<crate::timeline::TimelineStep>,
    /// The courier shipment booked for this sub-order (Wave 6), or
    /// `None` until the seller books one.
    pub shipment: Option<ShipmentSummary>,
    /// The buyer's return request status for this sub-order
    /// (Wave 16: `requested`, `approved`, `rejected` or
    /// `received`), or `None` when no return was requested. The
    /// refund details live on the return-request views; this is
    /// the summary the order page renders.
    pub return_status: Option<String>,
}

/// The buyer's unified order, with its per-seller sub-orders.
#[derive(Debug, Clone, Serialize)]
pub struct Order {
    pub id: Uuid,
    pub buyer_user_id: Uuid,
    pub status: String,
    pub payment_method: String,
    /// `pay_on_delivery` for COD; otherwise the latest payment
    /// attempt's lifecycle (`pending`, `paid`, `failed`, `refunded`).
    /// Wave 17 adds the COD advance states: `advance_pending`
    /// (an advance is required and not yet collected) and
    /// `advance_paid` (advance collected; the balance is still
    /// due in cash on delivery).
    pub payment_status: String,
    /// Advance this COD order must collect before shipping
    /// (Wave 17), snapshotted at checkout from the effective
    /// policy and the buyer's risk verdict; 0 for ordinary COD
    /// and for all online orders. A positive value with
    /// `payment_status = 'pay_on_delivery'` is the deliberate
    /// degradation flag: an advance was required but no gateway
    /// could collect it (see `place_order`).
    pub advance_required_poisha: i64,
    /// Advance actually collected, set ONLY by the signed
    /// payment callback path when the advance settles (Wave 17).
    pub advance_paid_poisha: i64,
    /// The latest payment attempt, or `None` for COD orders (which
    /// deliberately have no payment row).
    pub payment: Option<PaymentSummary>,
    pub delivery_name: String,
    pub delivery_phone: String,
    pub delivery_address: String,
    pub delivery_zone: String,
    /// The district the order ships to (Wave 55), when
    /// the platform knows it — from the saved address
    /// used, or the district picked at checkout. `None`
    /// for zone-only checkouts: no district is claimed
    /// for an order that never named one.
    pub delivery_district: Option<String>,
    /// The buyer's note to the sellers (Wave 55), or
    /// `None` when they wrote none.
    pub order_note: Option<String>,
    pub goods_total_poisha: i64,
    /// The coupon code applied at checkout (Wave 14), or `None`.
    pub coupon_code: Option<String>,
    /// Total coupon discount across the sub-orders (Wave 14).
    pub discount_total_poisha: i64,
    /// Loyalty points the buyer redeemed on this order
    /// (Wave 35); 0 when none were used.
    pub loyalty_points_used: i64,
    /// The discount those points bought (Wave 35). The grand
    /// total below is already net of it.
    pub loyalty_discount_poisha: i64,
    pub delivery_total_poisha: i64,
    pub grand_total_poisha: i64,
    pub created_at: DateTime<Utc>,
    pub sub_orders: Vec<SubOrder>,
}

/// A page of orders plus the total count.
#[derive(Debug, Serialize)]
pub struct OrderList {
    pub items: Vec<Order>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// A sub-order as the seller sees it: money split, items, and the
/// delivery details from the parent order (needed to ship the parcel).
#[derive(Debug, Clone, Serialize)]
pub struct SellerSubOrder {
    pub id: Uuid,
    pub order_id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub status: String,
    pub goods_subtotal_poisha: i64,
    /// Coupon discount allocated to this sub-order (Wave 14).
    pub discount_poisha: i64,
    pub delivery_charge_poisha: i64,
    pub commission_poisha: i64,
    pub seller_payout_poisha: i64,
    pub created_at: DateTime<Utc>,
    pub delivery_name: String,
    pub delivery_phone: String,
    pub delivery_address: String,
    pub delivery_zone: String,
    /// The parent order's buyer note (Wave 55) — the
    /// seller reads the same note on every sub-order of
    /// the order, exactly like the delivery fields above.
    pub order_note: Option<String>,
    /// The parent order's district (Wave 55); null for
    /// zone-only orders — sellers route parcels by it.
    pub delivery_district: Option<String>,
    pub items: Vec<OrderItem>,
    /// The sub-order's event timeline (Wave 55) — the
    /// seller watches the same courier story the buyer
    /// does, from the one shared builder.
    pub timeline: Vec<crate::timeline::TimelineEvent>,
    /// The tracking stepper (Wave 55).
    pub steps: Vec<crate::timeline::TimelineStep>,
    /// The courier shipment booked for this sub-order (Wave 6), or
    /// `None` until the seller books one.
    pub shipment: Option<ShipmentSummary>,
    /// The parent order's payment status (Wave 17 surfaces the
    /// advance states here: `advance_pending` warns the seller
    /// that the parcel must not ship yet — booking enforces it).
    pub order_payment_status: String,
    /// Advance the parent COD order must collect (Wave 17); a
    /// positive value while `order_payment_status` is
    /// `pay_on_delivery` means no gateway could collect it and
    /// the order proceeded flagged (see `place_order`).
    pub advance_required_poisha: i64,
    /// Advance the parent order has collected (Wave 17).
    pub advance_paid_poisha: i64,
    /// The buyer's fraud-shield level (`low`, `medium`, `high`)
    /// from the ONE shared risk aggregate (Wave 17) — the same
    /// verdict checkout and the buyer-risk endpoint use.
    pub buyer_risk_level: String,
}

/// A page of seller sub-orders plus the total count.
#[derive(Debug, Serialize)]
pub struct SellerSubOrderList {
    pub items: Vec<SellerSubOrder>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

// ---------------------------------------------------------------------------
// Row types + loaders (one loader per shape, reused by every endpoint)
// ---------------------------------------------------------------------------

#[derive(FromRow)]
struct OrderRow {
    id: Uuid,
    buyer_user_id: Uuid,
    status: String,
    payment_method: String,
    payment_status: String,
    advance_required_poisha: i64,
    advance_paid_poisha: i64,
    delivery_name: String,
    delivery_phone: String,
    delivery_address: String,
    delivery_zone: String,
    delivery_district: Option<String>,
    order_note: Option<String>,
    goods_total_poisha: i64,
    coupon_code: Option<String>,
    discount_total_poisha: i64,
    loyalty_points_used: i32,
    loyalty_discount_poisha: i64,
    delivery_total_poisha: i64,
    grand_total_poisha: i64,
    created_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct SubOrderRow {
    id: Uuid,
    order_id: Uuid,
    store_id: Uuid,
    store_slug: String,
    status: String,
    goods_subtotal_poisha: i64,
    discount_poisha: i64,
    loyalty_discount_poisha: i64,
    delivery_charge_poisha: i64,
    commission_poisha: i64,
    seller_payout_poisha: i64,
    created_at: DateTime<Utc>,
}

const ORDER_COLUMNS: &str = "id, buyer_user_id, status, payment_method, payment_status, \
    advance_required_poisha, advance_paid_poisha, \
    delivery_name, delivery_phone, delivery_address, delivery_zone, delivery_district, \
    order_note, goods_total_poisha, \
    coupon_code, discount_total_poisha, loyalty_points_used, loyalty_discount_poisha, \
    delivery_total_poisha, grand_total_poisha, created_at";

const SUB_ORDER_COLUMNS: &str = "so.id, so.order_id, so.store_id, s.slug AS store_slug, \
    so.status, so.goods_subtotal_poisha, so.discount_poisha, so.loyalty_discount_poisha, \
    so.delivery_charge_poisha, so.commission_poisha, \
    so.seller_payout_poisha, so.created_at";

async fn load_order_items(pool: &PgPool, sub_order_id: Uuid) -> Result<Vec<OrderItem>, ApiError> {
    let mut items: Vec<OrderItem> = sqlx::query_as(
        "SELECT id, sub_order_id, product_id, variant_id, variant_name, title_snapshot, \
         unit_price_poisha, quantity, line_total_poisha \
         FROM order_items WHERE sub_order_id = $1 ORDER BY id",
    )
    .bind(sub_order_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load order items", e))?;
    // Wave 51 extension: fill every line's product
    // thumbnail in ONE batch query (never per-item).
    let product_ids: Vec<Uuid> = items.iter().map(|item| item.product_id).collect();
    let urls = crate::product_images::primary_image_urls(pool, &product_ids).await?;
    for item in &mut items {
        item.image_url = urls.get(&item.product_id).cloned();
    }
    Ok(items)
}

async fn sub_order_from_row(pool: &PgPool, row: SubOrderRow) -> Result<SubOrder, ApiError> {
    let items = load_order_items(pool, row.id).await?;
    // Wave 55: the event timeline rides every sub-order
    // view — one builder (the `timeline` module), the
    // same events the public tracking page shows.
    let timeline =
        crate::timeline::sub_order_timeline(pool, row.id, &row.status, row.created_at).await?;
    let shipment = load_shipment_summary_for_sub_order(pool, row.id).await?;
    // Wave 55: the stepper, from the domain's one
    // derivation over the status + the courier leg.
    let steps = crate::timeline::sub_order_steps(
        &row.status,
        shipment.as_ref().map(|summary| summary.status.as_str()),
    );
    let return_status: Option<String> =
        sqlx::query_scalar("SELECT status FROM return_requests WHERE sub_order_id = $1")
            .bind(row.id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("load return status", e))?;
    Ok(SubOrder {
        id: row.id,
        order_id: row.order_id,
        store_id: row.store_id,
        store_slug: row.store_slug,
        status: row.status,
        goods_subtotal_poisha: row.goods_subtotal_poisha,
        discount_poisha: row.discount_poisha,
        loyalty_discount_poisha: row.loyalty_discount_poisha,
        delivery_charge_poisha: row.delivery_charge_poisha,
        commission_poisha: row.commission_poisha,
        seller_payout_poisha: row.seller_payout_poisha,
        created_at: row.created_at,
        items,
        timeline,
        steps,
        shipment,
        return_status,
    })
}

/// Load one sub-order (with items) by id — used by the seller status
/// endpoint to return the updated sub-order.
pub async fn load_sub_order(pool: &PgPool, sub_order_id: Uuid) -> Result<SubOrder, ApiError> {
    let row: SubOrderRow = sqlx::query_as(&format!(
        "SELECT {SUB_ORDER_COLUMNS} FROM sub_orders so \
         JOIN stores s ON s.id = so.store_id WHERE so.id = $1"
    ))
    .bind(sub_order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load sub-order", e))?
    .ok_or_else(|| ApiError::not_found("sub-order not found"))?;
    sub_order_from_row(pool, row).await
}

/// Load a full order (with sub-orders and items) by id. This is THE
/// order loader: checkout's response, the buyer detail endpoint and the
/// buyer list all build their orders through it.
pub async fn load_order(pool: &PgPool, order_id: Uuid) -> Result<Order, ApiError> {
    let row: OrderRow = sqlx::query_as(&format!("SELECT {ORDER_COLUMNS} FROM orders WHERE id = $1"))
        .bind(order_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load order", e))?
        .ok_or_else(|| ApiError::not_found("order not found"))?;

    let sub_rows: Vec<SubOrderRow> = sqlx::query_as(&format!(
        "SELECT {SUB_ORDER_COLUMNS} FROM sub_orders so \
         JOIN stores s ON s.id = so.store_id \
         WHERE so.order_id = $1 ORDER BY so.created_at, so.id"
    ))
    .bind(order_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load sub-orders", e))?;

    let mut sub_orders = Vec::with_capacity(sub_rows.len());
    for sub_row in sub_rows {
        sub_orders.push(sub_order_from_row(pool, sub_row).await?);
    }

    let payment = load_latest_payment_summary(pool, row.id).await?;

    Ok(Order {
        id: row.id,
        buyer_user_id: row.buyer_user_id,
        status: row.status,
        payment_method: row.payment_method,
        payment_status: row.payment_status,
        advance_required_poisha: row.advance_required_poisha,
        advance_paid_poisha: row.advance_paid_poisha,
        payment,
        delivery_name: row.delivery_name,
        delivery_phone: row.delivery_phone,
        delivery_address: row.delivery_address,
        delivery_zone: row.delivery_zone,
        delivery_district: row.delivery_district,
        order_note: row.order_note,
        goods_total_poisha: row.goods_total_poisha,
        coupon_code: row.coupon_code,
        discount_total_poisha: row.discount_total_poisha,
        loyalty_points_used: i64::from(row.loyalty_points_used),
        loyalty_discount_poisha: row.loyalty_discount_poisha,
        delivery_total_poisha: row.delivery_total_poisha,
        grand_total_poisha: row.grand_total_poisha,
        created_at: row.created_at,
        sub_orders,
    })
}

// ---------------------------------------------------------------------------
// Checkout
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PlaceOrderInput {
    pub delivery_name: String,
    pub delivery_phone: String,
    pub delivery_address: String,
    /// One of: inside_dhaka, dhaka_suburb, outside_dhaka.
    pub delivery_zone: String,
    /// One of: cod, bkash, nagad, sslcommerz, shurjopay. Online
    /// methods need a configured gateway or the DEV simulator,
    /// otherwise checkout fails with a 503 (see module docs).
    #[serde(default = "default_payment_method")]
    pub payment_method: String,
    /// Optional coupon code (Wave 14). When present, the coupon
    /// is loaded and re-validated inside the checkout transaction;
    /// an unusable coupon fails the whole checkout with a 400 and
    /// the cart stays intact.
    #[serde(default)]
    pub coupon_code: Option<String>,
    /// The online method the buyer will pay a COD ADVANCE with
    /// (Wave 17: bkash, nagad, sslcommerz or shurjopay). Used
    /// only when this COD order requires an advance — the
    /// threshold/risk policy in `place_order` decides that from
    /// the order total and the buyer's history, so the buyer
    /// cannot know for certain in advance; when an advance is
    /// required, a gateway can collect it, and no method was
    /// supplied, checkout fails with a 400 asking for one.
    /// Ignored for online checkouts and advance-free COD orders.
    #[serde(default)]
    pub advance_method: Option<String>,
    /// Loyalty points the buyer wants to redeem on this order
    /// (Wave 35). Optional; the server CLAMPS the request to
    /// the redeemable maximum (the buyer's balance, capped at
    /// half the post-coupon goods) instead of rejecting an
    /// over-large request — the placed order reports the points
    /// actually used. A negative count is a 400.
    #[serde(default)]
    pub loyalty_points: Option<i64>,
    /// A saved address to deliver to (Wave 55). When
    /// present, the address must be one of the BUYER's own
    /// (another buyer's id is a 404) and the order's
    /// delivery name/phone/address/zone/district are taken
    /// from THAT row — the delivery fields above are
    /// ignored on this path, so a tampered client cannot
    /// ship an order to fields the address never held.
    /// The address's `last_used_at` is stamped in the
    /// checkout transaction.
    #[serde(default)]
    pub address_id: Option<Uuid>,
    /// The delivery district (Wave 55), for the manual
    /// path: a district name from the domain table
    /// (`GET /v1/districts`). When present, the zone
    /// DERIVES from the district and `delivery_zone` is
    /// not consulted — a district and a zone that
    /// disagree can never both reach the order. An
    /// unknown district is a 400. Ignored when
    /// `address_id` is given (the address carries its
    /// own district).
    #[serde(default)]
    pub delivery_district: Option<String>,
    /// The buyer's note to the sellers (Wave 55):
    /// free text, trimmed, capped at 500 characters by
    /// the domain rule; blank means no note.
    #[serde(default)]
    pub order_note: Option<String>,
}

fn default_payment_method() -> String {
    "cod".to_string()
}

/// A cart line locked for checkout (`FOR UPDATE` on the product row).
#[derive(FromRow)]
struct LockedCartItem {
    product_id: Uuid,
    /// The variant this line buys (Wave 33); null for a
    /// variant-less line. The line's identity at checkout is
    /// (product_id, variant_id) — two variants of one product
    /// are two lines with two stock pools and two prices.
    variant_id: Option<Uuid>,
    store_id: Uuid,
    title: String,
    unit_price_poisha: i64,
    stock: i32,
    status: String,
    /// The selling store's status (Wave 10): a suspended store
    /// cannot sell, so checkout refuses its products with a 409.
    store_status: String,
    quantity: i32,
    weight_grams: i32,
    /// Whether the product currently has at least one ACTIVE
    /// variant (Wave 33). A variant-less line for such a
    /// product is a stale cart entry (the variants arrived
    /// after the add): checkout refuses it and asks the buyer
    /// to pick a variant, rather than drawing on the product
    /// stock the seller has since moved into variants.
    has_active_variants: bool,
}

/// A variant row locked for checkout (`FOR UPDATE` on the
/// variant row) — the variant half of a variant cart line:
/// its current name, price delta and stock, read under the
/// lock so the validation and the decrement below agree.
#[derive(FromRow)]
struct LockedVariant {
    id: Uuid,
    product_id: Uuid,
    name: String,
    price_delta_poisha: i64,
    stock: i32,
    is_active: bool,
}

/// Apply the checkout's coupon (if any) to the order groups, in
/// place: each in-scope group gains its allocated `discount_poisha`
/// share and its `commission_on_discounted` flag (see the domain
/// money design). Runs on the checkout transaction's connection
/// with the coupon row locked `FOR UPDATE`, so the usage-limit
/// check here and the `used_count` increment after the order is
/// written are atomic against concurrent checkouts. Returns the
/// applied coupon, or `None` when the buyer gave no code. Every
/// rejection is a 400 — the caller drops the transaction, so a
/// bad coupon never produces a partial order.
async fn apply_coupon_to_groups(
    conn: &mut sqlx::PgConnection,
    groups: &mut [OrderGroupInput],
    input: &PlaceOrderInput,
) -> Result<Option<Coupon>, ApiError> {
    let Some(raw_code) = input
        .coupon_code
        .as_deref()
        .map(str::trim)
        .filter(|code| !code.is_empty())
    else {
        return Ok(None);
    };
    let code = normalize_coupon_code(raw_code);
    let coupon: Coupon = sqlx::query_as(&format!(
        "SELECT {COUPON_COLUMNS} FROM coupons WHERE code = $1 FOR UPDATE"
    ))
    .bind(&code)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load coupon for checkout", e))?
    .ok_or_else(|| ApiError::bad_request(format!("coupon '{code}' was not found")))?;

    let mut group_goods: Vec<GroupGoods> = Vec::with_capacity(groups.len());
    for group in groups.iter() {
        let store_id = Uuid::parse_str(&group.store_id)
            .map_err(|_| ApiError::internal("parse store id", sqlx::Error::RowNotFound))?;
        // Wave 39 (audit CR-12): line totals are checked — an
        // overflowing group is a 400, never a wrapped subtotal
        // the coupon math would then price against.
        let mut goods_subtotal_poisha: i64 = 0;
        for item in &group.items {
            let line_total = item.line_total_poisha().map_err(ApiError::bad_request)?;
            goods_subtotal_poisha = goods_subtotal_poisha
                .checked_add(line_total)
                .ok_or_else(|| {
                    ApiError::bad_request(
                        "কার্টের মোট হিসাব সীমা ছাড়িয়ে গেছে — cart goods total overflows the money range",
                    )
                })?;
        }
        group_goods.push(GroupGoods {
            store_id,
            goods_subtotal_poisha,
        });
    }

    let eligible = eligible_subtotal_for(&coupon, &group_goods);
    if let Some(reason) = coupon_rejection_reason(&coupon, Utc::now(), eligible) {
        return Err(ApiError::bad_request(format!(
            "coupon '{}': {reason}",
            coupon.code
        )));
    }
    let discount_total = compute_coupon_discount(
        &coupon.discount_type,
        coupon.discount_value,
        coupon.max_discount_poisha,
        eligible,
    );
    if discount_total <= 0 {
        return Err(ApiError::bad_request(format!(
            "coupon '{}' does not reduce this order",
            coupon.code
        )));
    }

    // In-scope groups: all of them for a platform coupon, only the
    // coupon's own store for a store coupon.
    let in_scope: Vec<usize> = group_goods
        .iter()
        .enumerate()
        .filter(|(_, goods)| {
            coupon.store_id.is_none() || coupon.store_id == Some(goods.store_id)
        })
        .map(|(idx, _)| idx)
        .collect();
    let subtotals: Vec<i64> = in_scope
        .iter()
        .map(|&idx| group_goods[idx].goods_subtotal_poisha)
        .collect();
    let shares = allocate_discount(&subtotals, discount_total);
    for (pos, &idx) in in_scope.iter().enumerate() {
        groups[idx].discount_poisha = shares[pos];
        groups[idx].commission_on_discounted = coupon.store_id.is_some();
    }
    Ok(Some(coupon))
}

/// Place an order for everything in the caller's cart. See the module
/// docs for the transaction outline. Delivery is weight-based (Wave
/// 4): each locked product row carries its `weight_grams`, the domain
/// cart items inherit it, and `build_order_totals` sums unit weight
/// times quantity per seller group, billing per started kilogram.
pub async fn place_order(
    pool: &PgPool,
    state: &AppState,
    user: &AuthUser,
    input: PlaceOrderInput,
) -> Result<Order, ApiError> {
    let method = parse_payment_method(&input.payment_method).ok_or_else(|| {
        ApiError::bad_request(
            "payment_method must be one of: cod, bkash, nagad, sslcommerz, shurjopay",
        )
    })?;
    if let Some(points) = input.loyalty_points {
        validate_loyalty_request(points).map_err(ApiError::bad_request)?;
    }
    // Wave 55: the order note is validated once, here —
    // what reaches the order row is the trimmed note or
    // NULL, and an over-long note fails the checkout
    // before anything is written.
    let order_note =
        validate_order_note(input.order_note.as_deref()).map_err(ApiError::bad_request)?;

    // Wave 55: resolve the EFFECTIVE delivery fields —
    // the five values the order row stores — from exactly
    // one of three sources, in this precedence:
    //
    // 1. `address_id`: the buyer's saved address is the
    //    source of truth (name, phone, composed address,
    //    district, and the district's zone); the
    //    client-sent fields are ignored entirely.
    // 2. `delivery_district` on the manual path: the
    //    district is validated against the domain table
    //    and the zone derives from it.
    // 3. The pre-Wave-55 path, unchanged: the buyer's
    //    typed name/address/phone and their chosen zone.
    let (delivery_name, delivery_phone, delivery_address, zone, delivery_district) =
        if let Some(address_id) = input.address_id {
            let address =
                crate::addresses::load_own_address(pool, user.user_id, address_id)
                    .await?
                    .ok_or_else(|| ApiError::not_found("address not found"))?;
            // The district was validated against the
            // table when the address was written; the
            // fallback covers only a district the table
            // has since dropped, and prices it at the
            // widest zone rather than inventing a
            // cheaper one.
            let address_zone = district_by_name(&address.district)
                .map(|district| district.zone)
                .unwrap_or(DeliveryZone::OutsideDhaka);
            (
                address.recipient_name.clone(),
                address.phone.clone(),
                compose_delivery_address(
                    &address.address_line,
                    &address.area,
                    &address.district,
                ),
                address_zone,
                Some(address.district.clone()),
            )
        } else {
            if input.delivery_name.trim().is_empty() {
                return Err(ApiError::bad_request("delivery_name must not be empty"));
            }
            if input.delivery_address.trim().is_empty() {
                return Err(ApiError::bad_request("delivery_address must not be empty"));
            }
            let phone = normalize_bd_phone(&input.delivery_phone)
                .ok_or_else(|| ApiError::bad_request("invalid Bangladeshi phone number"))?;
            let (zone, district) = match input
                .delivery_district
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some(raw_district) => {
                    let district = district_by_name(raw_district).ok_or_else(|| {
                        ApiError::bad_request(
                            "জেলাটি তালিকায় নেই — delivery_district must be a district \
                             from GET /v1/districts",
                        )
                    })?;
                    (district.zone, Some(district.name_en.to_string()))
                }
                None => {
                    let parsed_zone = parse_delivery_zone(&input.delivery_zone)
                        .ok_or_else(|| {
                            ApiError::bad_request(
                                "delivery_zone must be one of: inside_dhaka, dhaka_suburb, outside_dhaka",
                            )
                        })?;
                    (parsed_zone, None)
                }
            };
            (
                input.delivery_name.trim().to_string(),
                phone,
                input.delivery_address.trim().to_string(),
                zone,
                district,
            )
        };

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin checkout transaction", e))?;

    // Wave 52: the transaction's own clock reading, taken
    // once. The flash price resolution below runs against
    // this instant — the database clock, the same clock the
    // stored windows are compared against everywhere else —
    // so "the price at order placement" is one well-defined
    // value for the whole checkout.
    let tx_now: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("read checkout clock", e))?;

    let cart_id: Uuid = sqlx::query_scalar("SELECT id FROM carts WHERE user_id = $1")
        .bind(user.user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("find cart for checkout", e))?
        .ok_or_else(|| ApiError::bad_request("cart is empty"))?;

    // Lock the product rows so stock cannot change under this checkout.
    let locked: Vec<LockedCartItem> = sqlx::query_as(
        "SELECT ci.product_id, ci.variant_id, p.store_id, p.title, \
         p.price_poisha AS unit_price_poisha, p.stock, p.status, \
         s.status AS store_status, ci.quantity, \
         p.weight_grams, \
         EXISTS (SELECT 1 FROM product_variants pv \
                 WHERE pv.product_id = p.id AND pv.is_active) AS has_active_variants \
         FROM cart_items ci JOIN products p ON p.id = ci.product_id \
         JOIN stores s ON s.id = p.store_id \
         WHERE ci.cart_id = $1 AND NOT ci.saved FOR UPDATE OF p",
    )
    .bind(cart_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock cart items", e))?;

    if locked.is_empty() {
        return Err(ApiError::bad_request("cart is empty"));
    }

    // Wave 33: lock the referenced variant rows too, and read
    // their current name / delta / stock under the lock. The
    // cart's foreign key guarantees every referenced variant
    // still exists (variants are never hard-deleted).
    let variant_ids: Vec<Uuid> = locked.iter().filter_map(|item| item.variant_id).collect();
    let locked_variants: Vec<LockedVariant> = if variant_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT id, product_id, name, price_delta_poisha, stock, is_active \
             FROM product_variants WHERE id = ANY($1) FOR UPDATE",
        )
        .bind(&variant_ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("lock cart variants", e))?
    };
    let variant_by_id: HashMap<Uuid, &LockedVariant> =
        locked_variants.iter().map(|v| (v.id, v)).collect();

    // Wave 52: resolve the live flash price for each distinct
    // product in the cart, INSIDE this transaction and AFTER
    // the product rows are locked — so the cap the resolution
    // reads cannot be consumed by a concurrent checkout of
    // the same product between the read and this order's
    // write (that checkout waits on the product-row lock, then
    // reads a cap that includes this order). A resolved flash
    // price replaces the product's BASE price for this order:
    // variant deltas still apply on top of it, and every
    // downstream figure (goods subtotal, commission, payout
    // snapshot, coupon base, COD collection) derives from the
    // snapshot as it always has — the snapshot now carries
    // the price the buyer actually pays.
    let mut flash_by_product: HashMap<Uuid, i64> = HashMap::new();
    {
        let mut seen = std::collections::HashSet::new();
        for item in &locked {
            if seen.insert(item.product_id) {
                if let Some(flash) =
                    crate::merch::active_flash_price_tx(&mut tx, item.product_id, tx_now).await?
                {
                    flash_by_product.insert(item.product_id, flash.sale_price_poisha);
                }
            }
        }
    }

    // The FINAL price of every line, computed once here (base
    // + delta through the domain rule for variant lines) and
    // reused by the totals and the snapshots below.
    let mut final_price_by_line: HashMap<(Uuid, Option<Uuid>), i64> = HashMap::new();
    for item in &locked {
        // The base this line prices from: the flash price
        // when one resolved for its product, else the locked
        // product price (Wave 52).
        let base_price = flash_by_product
            .get(&item.product_id)
            .copied()
            .unwrap_or(item.unit_price_poisha);
        if item.store_status == "suspended" {
            return Err(ApiError::conflict(format!(
                "the store selling '{}' is suspended and cannot take orders right now",
                item.title
            )));
        }
        if item.status != "active" {
            return Err(ApiError::bad_request(format!(
                "product '{}' is no longer available",
                item.title
            )));
        }
        let final_price = match item.variant_id {
            Some(variant_id) => {
                let variant = variant_by_id.get(&variant_id).ok_or_else(|| {
                    ApiError::internal("find locked variant", sqlx::Error::RowNotFound)
                })?;
                if variant.product_id != item.product_id {
                    return Err(ApiError::internal(
                        "variant/product mismatch",
                        sqlx::Error::RowNotFound,
                    ));
                }
                if !variant.is_active {
                    return Err(ApiError::bad_request(format!(
                        "variant '{}' of product '{}' is no longer available",
                        variant.name, item.title
                    )));
                }
                let price = variant_final_price(base_price, variant.price_delta_poisha)
                    .ok_or_else(|| {
                        ApiError::bad_request(format!(
                            "variant '{}' of product '{}' no longer has a valid price",
                            variant.name, item.title
                        ))
                    })?;
                if variant.stock < item.quantity {
                    return Err(ApiError::conflict(format!(
                        "insufficient stock for '{} ({})': requested {}, available {}",
                        item.title, variant.name, item.quantity, variant.stock
                    )));
                }
                price
            }
            None => {
                if item.has_active_variants {
                    return Err(ApiError::bad_request(format!(
                        "আকার/রং বেছে নিন — product '{}' now has variants; \
                         remove it from the cart and add it again with a variant chosen",
                        item.title
                    )));
                }
                if item.stock < item.quantity {
                    return Err(ApiError::conflict(format!(
                        "insufficient stock for '{}': requested {}, available {}",
                        item.title, item.quantity, item.stock
                    )));
                }
                base_price
            }
        };
        final_price_by_line.insert((item.product_id, item.variant_id), final_price);
    }

    // Split per seller and compute ALL money via the domain functions.
    // Wave 33: variant lines enter the totals at their FINAL
    // price and carry their variant id through the split, so
    // the snapshot loop below can find each line's locked row
    // by (product, variant). Delivery weight stays the
    // PRODUCT's weight — variants carry no weight of their own.
    let domain_items: Vec<DomainCartItem> = locked
        .iter()
        .map(|item| DomainCartItem {
            product_id: item.product_id.to_string(),
            store_id: item.store_id.to_string(),
            quantity: item.quantity as u32,
            unit_price_poisha: final_price_by_line[&(item.product_id, item.variant_id)],
            unit_weight_grams: i64::from(item.weight_grams),
            variant_id: item.variant_id.map(|id| id.to_string()),
        })
        .collect();
    // The split items carry their real unit weights, so each group's
    // parcel weight is derived inside `build_order_totals` from the
    // items themselves — no weight is passed in separately.
    // Wave 39 (audit CR-12): the split returns a Result — an
    // overflowing cart is a 400, never a wrapped total.
    let mut groups: Vec<OrderGroupInput> = split_cart_by_seller(&domain_items)
        .map_err(ApiError::bad_request)?
        .iter()
        .map(|sub| OrderGroupInput {
            store_id: sub.store_id.clone(),
            items: sub.items.clone(),
            discount_poisha: 0,
            commission_on_discounted: false,
            commission_rate_bps: None,
        })
        .collect();

    // Wave 14: apply the coupon, if one was given, BEFORE totals —
    // still inside the transaction, with the coupon row locked
    // FOR UPDATE so the usage-limit check and the used_count
    // increment below are atomic against concurrent checkouts.
    // Any rejection returns an error here: the transaction drops
    // uncommitted, nothing is written, and the cart stays intact.
    let applied_coupon = apply_coupon_to_groups(&mut tx, &mut groups, &input).await?;

    // The commission rate is the EFFECTIVE one (Wave 10): an
    // admin-set platform setting wins over the startup value.
    // Wave 36: each group ALSO carries its own store's rate —
    // the effective rate minus the discount of the store's
    // live subscription plan, resolved per store — and the
    // totals function applies the group's rate when present,
    // so every sub-order below snapshots the commission its
    // own store's plan actually charges. The order-wide rate
    // remains the fallback for any group without one.
    for group in groups.iter_mut() {
        let store_id = Uuid::parse_str(&group.store_id)
            .map_err(|_| ApiError::internal("parse store id", sqlx::Error::RowNotFound))?;
        group.commission_rate_bps =
            Some(crate::subscriptions::effective_commission_bps_for_store(state, store_id).await?);
    }
    let commission_rate_bps = crate::admin::effective_commission_bps(state).await;
    // Wave 39 (audit CR-11/CR-12): totals return a Result — a
    // negative or overflowing money input is a 400 here, inside
    // the transaction, so the checkout rolls back cleanly instead
    // of panicking or storing a wrapped figure.
    let totals =
        build_order_totals(&groups, zone, commission_rate_bps).map_err(ApiError::bad_request)?;

    // Wave 35: redeem loyalty points, AFTER the coupon (the
    // brief's order: points price against post-coupon goods).
    // The buyer's user row is locked FOR UPDATE first, so two
    // concurrent checkouts by the same buyer serialize here and
    // can never both read the same balance and spend it twice.
    // The request is clamped to the domain maximum — balance
    // and the half-of-goods cap — never rejected for being too
    // large; the placed order reports what was actually used.
    // The discount is allocated across the groups by
    // post-coupon goods (the coupon's own allocator), so the
    // per-sub-order shares sum exactly to the discount.
    let requested_loyalty = input.loyalty_points.unwrap_or(0);
    let mut loyalty_points_used: i64 = 0;
    let mut loyalty_discount_total: i64 = 0;
    let mut loyalty_shares: Vec<i64> = vec![0; groups.len()];
    if requested_loyalty > 0 {
        sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
            .bind(user.user_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("lock buyer for loyalty", e))?;
        let balance = crate::loyalty::loyalty_balance_on(&mut *tx, user.user_id).await?;
        // Each group's post-coupon goods: its subtotal minus
        // its (clamped) coupon share — the same figures
        // `build_order_totals` just used.
        let post_coupon_goods: Vec<i64> = totals
            .sub_orders
            .iter()
            .map(|sub| sub.goods_subtotal_poisha - sub.discount_poisha)
            .collect();
        let post_coupon_total: i64 = post_coupon_goods.iter().sum();
        let allowed = loyalty_max_redeemable_points(balance.available_points, post_coupon_total);
        loyalty_points_used = requested_loyalty.min(allowed);
        if loyalty_points_used > 0 {
            loyalty_discount_total = loyalty_discount_poisha(loyalty_points_used).ok_or_else(|| {
                ApiError::internal("loyalty discount overflow", sqlx::Error::RowNotFound)
            })?;
            loyalty_shares = allocate_discount(&post_coupon_goods, loyalty_discount_total);
        }
    }
    // What the buyer actually pays: every downstream figure —
    // the advance decision, the online payment amount, the
    // stored grand total, the receipt's cash-due — reads this
    // reduced payable, never the pre-loyalty total.
    let payable_grand_total = totals.grand_total_poisha - loyalty_discount_total;

    // Wave 17: the COD advance decision. COD orders only — an
    // online order prepays in full by construction. The buyer's
    // risk verdict (the ONE shared aggregate in the `risk`
    // module, read on the pool: it counts committed history, and
    // this order is not written yet) can force an advance at any
    // size; otherwise the effective threshold + rate decide via
    // the pure domain rule.
    //
    // Enforcement needs a gateway that can actually collect the
    // advance (merchant credentials present, or the DEV
    // simulator). Three outcomes when an advance is required:
    //
    // a gateway can collect + a method was supplied: enforce —
    // the order is stored `advance_pending` and the advance
    // payment is initiated below, inside this transaction.
    // a gateway can collect + NO method supplied: 400 — the
    // buyer must choose how to pay the advance; the transaction
    // drops and the cart stays intact.
    // NO gateway can collect: deliberate degradation — the
    // order proceeds as plain COD with `advance_required_poisha`
    // recorded on it as a visible risk flag (seller surfaces
    // warn that no advance was taken). Nothing claims money was
    // collected: there is no payment row and `advance_paid`
    // stays 0. This is a safety valve for a platform whose
    // gateways are not live yet, not a fake success.
    let mut advance_required_poisha: i64 = 0;
    let mut advance_payment_method: Option<PaymentMethod> = None;
    let mut order_payment_status = if method.is_online() {
        "pending"
    } else {
        "pay_on_delivery"
    };
    if !method.is_online() {
        let report = buyer_risk_for_user(pool, user.user_id).await?;
        // Wave 35: the advance is measured against the
        // loyalty-reduced payable — points the buyer spent are
        // money the platform already holds for this order.
        let advance = compute_advance_poisha(
            payable_grand_total,
            crate::admin::effective_advance_bps(state).await,
            crate::admin::effective_advance_threshold(state).await,
            report.risk.requires_advance,
        );
        if advance > 0 {
            advance_required_poisha = advance;
            if advance_gateway_available(&state.payments_config) {
                let raw_method = input
                    .advance_method
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        ApiError::bad_request(format!(
                            "this order requires an advance payment of {advance} poisha \
                             before it can ship — choose an advance payment method \
                             (advance_method: bkash, nagad, sslcommerz or shurjopay)"
                        ))
                    })?;
                let parsed = parse_payment_method(raw_method).filter(|m| m.is_online());
                let parsed = parsed.ok_or_else(|| {
                    ApiError::bad_request(
                        "advance_method must be one of: bkash, nagad, sslcommerz, shurjopay",
                    )
                })?;
                advance_payment_method = Some(parsed);
                order_payment_status = "advance_pending";
            }
        }
    }

    let order_id: Uuid = sqlx::query_scalar(
        "INSERT INTO orders (buyer_user_id, payment_method, payment_status, \
         delivery_name, delivery_phone, delivery_address, delivery_zone, \
         goods_total_poisha, delivery_total_poisha, grand_total_poisha, \
         coupon_code, discount_total_poisha, advance_required_poisha, \
         loyalty_points_used, loyalty_discount_poisha, order_note, delivery_district) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) \
         RETURNING id",
    )
    .bind(user.user_id)
    .bind(method.as_str())
    .bind(order_payment_status)
    .bind(&delivery_name)
    .bind(&delivery_phone)
    .bind(&delivery_address)
    .bind(zone.as_str())
    .bind(totals.goods_total_poisha)
    .bind(totals.delivery_total_poisha)
    .bind(payable_grand_total)
    .bind(applied_coupon.as_ref().map(|coupon| coupon.code.clone()))
    .bind(totals.discount_total_poisha)
    .bind(advance_required_poisha)
    .bind(i32::try_from(loyalty_points_used).unwrap_or(i32::MAX))
    .bind(loyalty_discount_total)
    .bind(&order_note)
    .bind(&delivery_district)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("insert order", e))?;

    // Wave 55: an order placed against a saved address
    // stamps the address's `last_used_at` in this same
    // transaction — the book's "recently used" order is a
    // record of real checkouts, and a rolled-back
    // checkout stamps nothing.
    if let Some(address_id) = input.address_id {
        sqlx::query(
            "UPDATE buyer_addresses SET last_used_at = now(), updated_at = now() \
             WHERE id = $1 AND user_id = $2",
        )
        .bind(address_id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("stamp address last used", e))?;
    }

    // Online payment: initiate through the payments factory, still
    // inside the checkout transaction. An unconfigured gateway errors
    // here, the transaction is dropped uncommitted, and the buyer
    // keeps their cart and stock — a 503, never a half-placed order
    // with a payment that cannot be taken. The DEV simulator's
    // initiate is pure (it only mints a reference); real adapters
    // make no network calls yet either.
    if method.is_online() {
        let adapter = payment_adapter_for(method, &state.payments_config);
        // Wave 35: the gateway charges the loyalty-reduced
        // payable — the points are the platform's money, already
        // settled in the ledger row written below.
        let intent = payments::PaymentAdapter::initiate(
            &*adapter,
            &order_id.to_string(),
            payable_grand_total,
        )
        .await
        .map_err(|e| {
            ApiError::service_unavailable(format!(
                "payment method '{}' is not available yet ({e})",
                method.as_str()
            ))
        })?;
        insert_pending_payment(
            &mut tx,
            order_id,
            method.as_str(),
            payable_grand_total,
            &intent.reference,
        )
        .await?;
    }

    // Wave 17: an enforced COD advance is initiated exactly like
    // an online payment — same factory, same pending-row writer —
    // except the amount is the ADVANCE, not the grand total, and
    // the order row keeps payment_method 'cod' (only the payment
    // row carries the advance method). An adapter that cannot
    // take it (for example, the buyer picked a gateway with no
    // credentials while another gateway is configured) errors
    // here: 503, the transaction rolls back, cart intact — the
    // same honesty rule as online checkout. Settlement arrives
    // later through the shared callback in the payments module,
    // which recognises the advance amount and moves the order to
    // `advance_paid`, never `paid`.
    if let Some(advance_method) = advance_payment_method {
        let adapter = payment_adapter_for(advance_method, &state.payments_config);
        let intent = payments::PaymentAdapter::initiate(
            &*adapter,
            &order_id.to_string(),
            advance_required_poisha,
        )
        .await
        .map_err(|e| {
            ApiError::service_unavailable(format!(
                "advance payment method '{}' is not available yet ({e})",
                advance_method.as_str()
            ))
        })?;
        insert_pending_payment(
            &mut tx,
            order_id,
            advance_method.as_str(),
            advance_required_poisha,
            &intent.reference,
        )
        .await?;
    }

    // Snapshots for order_items, keyed by (product, variant):
    // Wave 33 makes the variant part of a line's identity, so
    // two variant lines of one product resolve to their own
    // locked rows (and their own stock pools) here.
    let locked_by_line: std::collections::HashMap<(Uuid, Option<Uuid>), &LockedCartItem> = locked
        .iter()
        .map(|item| ((item.product_id, item.variant_id), item))
        .collect();

    for (group_idx, (group, sub_totals)) in
        groups.iter().zip(totals.sub_orders.iter()).enumerate()
    {
        let store_id = Uuid::parse_str(&group.store_id)
            .map_err(|_| ApiError::internal("parse store id", sqlx::Error::RowNotFound))?;
        // Wave 35: the sub-order snapshots its allocated share
        // of the loyalty discount (the audit + cancel-refund
        // basis). The seller payout bind below is the
        // pre-loyalty figure — the platform funds the points.
        let sub_order_id: Uuid = sqlx::query_scalar(
            "INSERT INTO sub_orders (order_id, store_id, goods_subtotal_poisha, \
             discount_poisha, delivery_charge_poisha, commission_poisha, seller_payout_poisha, \
             loyalty_discount_poisha) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        )
        .bind(order_id)
        .bind(store_id)
        .bind(sub_totals.goods_subtotal_poisha)
        .bind(sub_totals.discount_poisha)
        .bind(sub_totals.delivery_charge_poisha)
        .bind(sub_totals.commission_poisha)
        .bind(sub_totals.seller_payout_poisha)
        .bind(loyalty_shares[group_idx])
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("insert sub-order", e))?;

        for item in &group.items {
            let product_id = Uuid::parse_str(&item.product_id)
                .map_err(|_| ApiError::internal("parse product id", sqlx::Error::RowNotFound))?;
            let variant_id: Option<Uuid> = item
                .variant_id
                .as_deref()
                .map(|raw| {
                    Uuid::parse_str(raw).map_err(|_| {
                        ApiError::internal("parse variant id", sqlx::Error::RowNotFound)
                    })
                })
                .transpose()?;
            let locked_item = locked_by_line
                .get(&(product_id, variant_id))
                .ok_or_else(|| ApiError::internal("find locked item", sqlx::Error::RowNotFound))?;
            // The variant name snapshot (Wave 33): the name as
            // it stands under the checkout lock, frozen onto
            // the order line like the title and price.
            let variant_name: Option<String> = match variant_id {
                Some(id) => Some(
                    variant_by_id
                        .get(&id)
                        .ok_or_else(|| {
                            ApiError::internal("find locked variant", sqlx::Error::RowNotFound)
                        })?
                        .name
                        .clone(),
                ),
                None => None,
            };
            // Wave 39 (audit CR-12): the line total is checked
            // (checkout's totals pass already proved it fits, so
            // this cannot fail in practice — but it must never
            // silently wrap if a future caller reaches here
            // without that pass).
            let line_total_poisha = item.line_total_poisha().map_err(ApiError::bad_request)?;
            sqlx::query(
                "INSERT INTO order_items (sub_order_id, product_id, variant_id, variant_name, \
                 title_snapshot, unit_price_poisha, quantity, line_total_poisha) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(sub_order_id)
            .bind(product_id)
            .bind(variant_id)
            .bind(&variant_name)
            .bind(&locked_item.title)
            .bind(item.unit_price_poisha)
            .bind(item.quantity as i32)
            .bind(line_total_poisha)
            .execute(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("insert order item", e))?;

            // Wave 23: the decrement and its audit row commit
            // together — `RETURNING stock` gives the exact
            // post-move count for the trail's snapshot.
            //
            // Wave 33: a variant line decrements the VARIANT
            // row (the product's own stock is not touched) and
            // its audit row carries the variant id with the
            // variant's post-move count; a variant-less line
            // takes the product path exactly as before.
            let quantity = item.quantity as i32;
            let (new_stock, movement_variant_id) = match variant_id {
                Some(variant_id) => {
                    let variant_stock: i32 = sqlx::query_scalar(
                        "UPDATE product_variants SET stock = stock - $2, updated_at = now() \
                         WHERE id = $1 RETURNING stock",
                    )
                    .bind(variant_id)
                    .bind(quantity)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| ApiError::internal("decrement variant stock", e))?;
                    (variant_stock, Some(variant_id))
                }
                None => {
                    let product_stock: i32 = sqlx::query_scalar(
                        "UPDATE products SET stock = stock - $2, updated_at = now() \
                         WHERE id = $1 RETURNING stock",
                    )
                    .bind(product_id)
                    .bind(quantity)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| ApiError::internal("decrement stock", e))?;
                    (product_stock, None)
                }
            };
            crate::inventory::record_stock_adjustment(
                &mut tx,
                &crate::inventory::StockMovement {
                    product_id,
                    variant_id: movement_variant_id,
                    store_id,
                    actor: "system",
                    actor_user_id: None,
                    delta: -quantity,
                    new_stock,
                    reason: "order",
                    note: None,
                },
            )
            .await?;
        }
    }

    // Wave 14: record the redemption and count the use, in the
    // same transaction — a rolled-back checkout never consumes a
    // coupon use, and a committed one always does.
    if let Some(coupon) = &applied_coupon {
        sqlx::query(
            "INSERT INTO coupon_redemptions (coupon_id, order_id, buyer_id, discount_poisha) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(coupon.id)
        .bind(order_id)
        .bind(user.user_id)
        .bind(totals.discount_total_poisha)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("record coupon redemption", e))?;

        sqlx::query("UPDATE coupons SET used_count = used_count + 1 WHERE id = $1")
            .bind(coupon.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("count coupon use", e))?;
    }

    // Wave 35: the redemption's ledger row, in the same
    // transaction — a rolled-back checkout never spends points,
    // and a committed one always does. Written through the
    // loyalty module's balance conventions: negative delta,
    // reason `redeem`, ref = the order.
    if loyalty_points_used > 0 {
        sqlx::query(
            "INSERT INTO loyalty_points (user_id, delta, reason, ref_id) \
             VALUES ($1, $2, 'redeem', $3)",
        )
        .bind(user.user_id)
        .bind(i32::try_from(-loyalty_points_used).unwrap_or(i32::MIN))
        .bind(order_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("record loyalty redemption", e))?;
    }

    // Wave 55: only the ACTIVE lines are consumed —
    // saved-for-later lines survive the checkout on the
    // shelf, exactly as the lock scan above ignored them.
    sqlx::query("DELETE FROM cart_items WHERE cart_id = $1 AND NOT saved")
        .bind(cart_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("clear cart after checkout", e))?;

    // Wave 29: the order-placed notification joins the same
    // transaction (the outbox rule) — one row per ORDER,
    // addressed to the buyer at the delivery phone, carrying
    // the grand total. A rolled-back checkout queues nothing.
    let notice_payload = serde_json::json!({
        "order_id": order_id.to_string(),
        // Wave 35: the amount the buyer actually pays (the
        // loyalty-reduced payable), not the pre-points total.
        "amount_poisha": payable_grand_total,
    });
    notify::enqueue(
        &mut *tx,
        user.user_id,
        &delivery_phone,
        domain::NOTIFICATION_ORDER_PLACED,
        &notice_payload,
    )
    .await
    .map_err(|e| ApiError::internal("enqueue order notification", e))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit checkout", e))?;

    load_order(pool, order_id).await
}

// ---------------------------------------------------------------------------
// Buyer order history
// ---------------------------------------------------------------------------

/// The caller's own orders, newest first, paginated.
pub async fn list_orders_for_buyer(
    pool: &PgPool,
    user: &AuthUser,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<OrderList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders WHERE buyer_user_id = $1")
        .bind(user.user_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("count buyer orders", e))?;

    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM orders WHERE buyer_user_id = $1 \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(user.user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list buyer orders", e))?;

    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        items.push(load_order(pool, id).await?);
    }
    Ok(OrderList {
        items,
        total,
        page,
        limit,
    })
}

/// One of the caller's own orders. Another buyer's order is a 404 —
/// order ids must not leak other buyers' purchases.
pub async fn get_order_for_buyer(
    pool: &PgPool,
    user: &AuthUser,
    order_id: Uuid,
) -> Result<Order, ApiError> {
    let order = load_order(pool, order_id).await?;
    if order.buyer_user_id != user.user_id {
        return Err(ApiError::not_found("order not found"));
    }
    Ok(order)
}

// ---------------------------------------------------------------------------
// Reorder — "buy again" (Wave 55)
// ---------------------------------------------------------------------------

/// One line reorder DID add to the cart, at today's
/// price (the cart view in the report carries the price;
/// this entry carries what was asked of the cart).
#[derive(Debug, Serialize)]
pub struct ReorderAdded {
    pub product_id: Uuid,
    pub variant_id: Option<Uuid>,
    /// The title as bought (the order's snapshot) — the
    /// product's current title rides the cart view.
    pub title: String,
    /// Units actually added: the ordered quantity
    /// clamped by today's stock and the cart line's
    /// remaining room (the domain decision).
    pub quantity: i64,
}

/// One line reorder could NOT add, with the reason.
#[derive(Debug, Serialize)]
pub struct ReorderSkipped {
    /// The title as bought (the order's snapshot).
    pub title: String,
    /// The domain reason code (`product_unavailable`,
    /// `variant_unavailable`, `out_of_stock`,
    /// `cart_limit`).
    pub reason: String,
    /// The buyer-facing bilingual explanation.
    pub message: String,
}

/// The reorder report: honest partial success — what
/// went back into the cart, what did not and why, and
/// the cart itself (the server's truth, at today's
/// prices) for the page to render.
#[derive(Debug, Serialize)]
pub struct ReorderReport {
    pub order_id: Uuid,
    pub added: Vec<ReorderAdded>,
    pub skipped: Vec<ReorderSkipped>,
    pub cart: crate::cart::CartView,
}

/// One past-order line read with TODAY's facts for the
/// reorder decision: the product's current status and
/// stock, the bought variant's current state and stock,
/// and whether the product has active variants now.
#[derive(FromRow)]
struct ReorderLine {
    product_id: Uuid,
    variant_id: Option<Uuid>,
    title_snapshot: String,
    quantity: i32,
    product_status: Option<String>,
    product_stock: Option<i32>,
    variant_active: Option<bool>,
    variant_stock: Option<i32>,
    has_active_variants: bool,
}

/// Re-add a past order's items to the buyer's cart at
/// CURRENT prices and availability (Wave 55). The order
/// must be the buyer's own (404 otherwise, the buyer
/// loader's rule) and not cancelled (a cancelled order's
/// goods were never the buyer's to re-buy in one click —
/// 409). Every line is decided by the domain's
/// `reorder_decision` over today's facts and added
/// through the cart module's ONE add path, so the cart
/// the report returns obeys every cart rule (variant
/// choice, the 99 cap, flash pricing) exactly as a
/// manual add would. Old prices are never consulted —
/// the order contributes only (product, variant,
/// quantity).
pub async fn reorder_order(
    pool: &PgPool,
    user: &AuthUser,
    order_id: Uuid,
) -> Result<ReorderReport, ApiError> {
    let order = get_order_for_buyer(pool, user, order_id).await?;
    if order.status == "cancelled" {
        return Err(ApiError::conflict(
            "বাতিল অর্ডার আবার কেনা যায় না — a cancelled order cannot be reordered",
        ));
    }

    let lines: Vec<ReorderLine> = sqlx::query_as(
        "SELECT oi.product_id, oi.variant_id, oi.title_snapshot, oi.quantity, \
         p.status AS product_status, p.stock AS product_stock, \
         v.is_active AS variant_active, v.stock AS variant_stock, \
         EXISTS (SELECT 1 FROM product_variants pv \
                 WHERE pv.product_id = p.id AND pv.is_active) AS has_active_variants \
         FROM order_items oi \
         JOIN sub_orders so ON so.id = oi.sub_order_id \
         LEFT JOIN products p ON p.id = oi.product_id \
         LEFT JOIN product_variants v ON v.id = oi.variant_id \
         WHERE so.order_id = $1 ORDER BY so.created_at, oi.id",
    )
    .bind(order_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load order lines for reorder", e))?;

    // The buyer's current cart quantities per line, for
    // the headroom half of the decision (a line already
    // in the cart — active or saved — accumulates).
    let cart_id = crate::cart::find_cart_id(pool, user.user_id).await?;
    let mut cart_quantities: HashMap<(Uuid, Option<Uuid>), i64> = HashMap::new();
    if let Some(cart_id) = cart_id {
        let rows: Vec<(Uuid, Option<Uuid>, i32)> = sqlx::query_as(
            "SELECT product_id, variant_id, quantity FROM cart_items WHERE cart_id = $1",
        )
        .bind(cart_id)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("load cart for reorder", e))?;
        for (product_id, variant_id, quantity) in rows {
            cart_quantities.insert((product_id, variant_id), i64::from(quantity));
        }
    }

    let mut added = Vec::new();
    let mut skipped = Vec::new();
    for line in &lines {
        let variant_state = match line.variant_id {
            Some(_) => match line.variant_active {
                None => ReorderVariantState::Missing,
                Some(true) => ReorderVariantState::Active,
                Some(false) => ReorderVariantState::Inactive,
            },
            None => {
                if line.has_active_variants {
                    ReorderVariantState::ProductGainedVariants
                } else {
                    ReorderVariantState::NotApplicable
                }
            }
        };
        let stock = i64::from(match line.variant_id {
            Some(_) => line.variant_stock.unwrap_or(0),
            None => line.product_stock.unwrap_or(0),
        });
        let in_cart = cart_quantities
            .get(&(line.product_id, line.variant_id))
            .copied()
            .unwrap_or(0);
        let headroom = i64::from(domain::MAX_CART_QUANTITY) - in_cart;
        let decision = reorder_decision(
            line.product_status.as_deref(),
            variant_state,
            stock,
            i64::from(line.quantity),
            headroom,
        );
        match decision {
            ReorderDecision::Add { quantity } => {
                match crate::cart::add_cart_item(
                    pool,
                    user,
                    line.product_id,
                    quantity,
                    line.variant_id,
                )
                .await
                {
                    Ok(_) => {
                        cart_quantities.insert(
                            (line.product_id, line.variant_id),
                            in_cart + quantity,
                        );
                        added.push(ReorderAdded {
                            product_id: line.product_id,
                            variant_id: line.variant_id,
                            title: line.title_snapshot.clone(),
                            quantity,
                        });
                    }
                    // The facts were read a moment ago;
                    // if the add still refuses (a race
                    // with a seller edit), the line is
                    // reported skipped with the add's own
                    // words rather than failing the whole
                    // reorder.
                    Err(err) => skipped.push(ReorderSkipped {
                        title: line.title_snapshot.clone(),
                        reason: domain::ReorderSkipReason::ProductUnavailable
                            .as_str()
                            .to_string(),
                        message: err.message,
                    }),
                }
            }
            ReorderDecision::Skip(reason) => skipped.push(ReorderSkipped {
                title: line.title_snapshot.clone(),
                reason: reason.as_str().to_string(),
                message: reason.message().to_string(),
            }),
        }
    }

    let cart = crate::cart::cart_view(pool, user.user_id).await?;
    Ok(ReorderReport {
        order_id,
        added,
        skipped,
        cart,
    })
}

// ---------------------------------------------------------------------------
// Buy-again rail (Wave 55) — the products a buyer has
// bought before, offered again at today's facts
// ---------------------------------------------------------------------------

/// How many products the buy-again rail carries at most.
pub const BUY_AGAIN_LIMIT: i64 = 12;

/// One product on the buy-again rail: something the buyer
/// has ordered before that can be bought again TODAY.
#[derive(Debug, Serialize)]
pub struct BuyAgainItem {
    pub product_id: Uuid,
    /// The product's CURRENT title (not the order's
    /// snapshot — the rail sells today's product).
    pub title: String,
    pub title_bn: Option<String>,
    pub store_slug: String,
    pub image_url: Option<String>,
    /// Today's unit price: the live flash price when a
    /// sale governs the product, else the base price —
    /// the same resolution the cart applies.
    pub unit_price_poisha: i64,
    /// The base price when a flash sale governs (the
    /// strike-through figure); equal to
    /// `unit_price_poisha` when no sale does.
    pub list_unit_price_poisha: i64,
    /// The product sells through variants: the rail's
    /// add must send the buyer to pick one (the product
    /// page), never guess.
    pub has_variants: bool,
    /// How many of the buyer's orders included it.
    pub times_ordered: i64,
    pub last_ordered_at: DateTime<Utc>,
    /// Already in the buyer's ACTIVE cart (saved lines
    /// do not count — they are not in the cart proper).
    pub in_cart: bool,
}

/// The buy-again rail response.
#[derive(Debug, Serialize)]
pub struct BuyAgainList {
    pub items: Vec<BuyAgainItem>,
}

/// One rail row as stored, before the image URL and the
/// flash price are resolved.
#[derive(FromRow)]
struct BuyAgainRow {
    product_id: Uuid,
    title: String,
    title_bn: Option<String>,
    store_slug: String,
    unit_price_poisha: i64,
    has_variants: bool,
    times_ordered: i64,
    last_ordered_at: DateTime<Utc>,
    in_cart: bool,
    #[sqlx(default)]
    product_image_key: Option<String>,
    #[sqlx(default)]
    product_image_external_url: Option<String>,
    #[sqlx(default)]
    product_image_width: Option<i32>,
}

/// The buy-again rail (Wave 55): the distinct products
/// from the buyer's non-cancelled orders, most recently
/// ordered first, filtered to what can be bought again
/// today — the product is active AND has stock (its own
/// stock when it sells plainly; any active variant's
/// stock when it sells through variants). Archived,
/// sold-out and cancelled-away products simply do not
/// appear: the rail never offers what the cart would
/// refuse. Prices are today's (flash-resolved like the
/// cart's), never the orders' snapshots.
pub async fn buy_again_rail(pool: &PgPool, user: &AuthUser) -> Result<BuyAgainList, ApiError> {
    let rows: Vec<BuyAgainRow> = sqlx::query_as(
        "SELECT p.id AS product_id, p.title, p.title_bn, s.slug AS store_slug, \
         p.price_poisha AS unit_price_poisha, \
         EXISTS (SELECT 1 FROM product_variants pv \
                 WHERE pv.product_id = p.id AND pv.is_active) AS has_variants, \
         COUNT(DISTINCT o.id)::bigint AS times_ordered, \
         MAX(o.created_at) AS last_ordered_at, \
         EXISTS (SELECT 1 FROM cart_items ci JOIN carts c ON c.id = ci.cart_id \
                 WHERE c.user_id = $1 AND ci.product_id = p.id AND NOT ci.saved) AS in_cart, \
         (SELECT pi.storage_key FROM product_images pi \
           WHERE pi.product_id = p.id \
           ORDER BY pi.is_primary DESC, pi.position ASC, pi.created_at ASC LIMIT 1) \
             AS product_image_key, \
         (SELECT pi.external_url FROM product_images pi \
           WHERE pi.product_id = p.id \
           ORDER BY pi.is_primary DESC, pi.position ASC, pi.created_at ASC LIMIT 1) \
             AS product_image_external_url, \
         (SELECT pi.width FROM product_images pi \
           WHERE pi.product_id = p.id \
           ORDER BY pi.is_primary DESC, pi.position ASC, pi.created_at ASC LIMIT 1) \
             AS product_image_width \
         FROM order_items oi \
         JOIN sub_orders so ON so.id = oi.sub_order_id \
         JOIN orders o ON o.id = so.order_id \
         JOIN products p ON p.id = oi.product_id \
         JOIN stores s ON s.id = p.store_id \
         WHERE o.buyer_user_id = $1 AND o.status <> 'cancelled' \
           AND so.status <> 'cancelled' AND p.status = 'active' \
           AND ((NOT EXISTS (SELECT 1 FROM product_variants pv3 \
                             WHERE pv3.product_id = p.id AND pv3.is_active) \
                 AND p.stock > 0) \
                OR EXISTS (SELECT 1 FROM product_variants pv4 \
                           WHERE pv4.product_id = p.id AND pv4.is_active \
                             AND pv4.stock > 0)) \
         GROUP BY p.id, p.title, p.title_bn, s.slug, p.price_poisha \
         ORDER BY last_ordered_at DESC \
         LIMIT $2",
    )
    .bind(user.user_id)
    .bind(BUY_AGAIN_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load buy-again rail", e))?;

    // Flash resolution, the cart's one rule: a live sale
    // price replaces the base on the rail exactly as in
    // the cart, with the base kept for the strike-through.
    let product_ids: Vec<Uuid> = rows.iter().map(|row| row.product_id).collect();
    let flash = if product_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        crate::merch::flash_price_map(pool, &product_ids, Utc::now()).await?
    };

    let items = rows
        .into_iter()
        .map(|row| {
            let image_url = crate::media::product_image_url(
                row.product_image_key.as_deref(),
                row.product_image_external_url.as_deref(),
                row.product_image_width,
            );
            let (unit_price_poisha, list_unit_price_poisha) = match flash.get(&row.product_id) {
                Some(sale) => (sale.sale_price_poisha, row.unit_price_poisha),
                None => (row.unit_price_poisha, row.unit_price_poisha),
            };
            BuyAgainItem {
                product_id: row.product_id,
                title: row.title,
                title_bn: row.title_bn,
                store_slug: row.store_slug,
                image_url,
                unit_price_poisha,
                list_unit_price_poisha,
                has_variants: row.has_variants,
                times_ordered: row.times_ordered,
                last_ordered_at: row.last_ordered_at,
                in_cart: row.in_cart,
            }
        })
        .collect();
    Ok(BuyAgainList { items })
}

/// GET /v1/buyer/buy-again — the signed-in buyer's rail.
pub async fn buy_again_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<BuyAgainList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(buy_again_rail(pool, &user).await?))
}

// ---------------------------------------------------------------------------
// Cancellation + restock (Wave 4)
// ---------------------------------------------------------------------------

/// One order line read back for restocking: the product, the
/// variant the line bought (Wave 33; null for variant-less
/// lines) and the quantity to put back.
#[derive(FromRow)]
struct RestockItem {
    product_id: Uuid,
    variant_id: Option<Uuid>,
    quantity: i32,
}

/// Put one sub-order's items back into stock: for every order item,
/// `products.stock` grows by the ordered quantity. Shared by the buyer
/// whole-order cancel and the seller sub-order cancel — both run it
/// inside their own transaction on the connection handed in here, so a
/// cancel and its restock always commit or roll back together. Wave 16
/// adds two more callers on the same rule: a received buyer return
/// (the `returns` module) and a courier-driven return (RTO) applied
/// by the shipment refresh path — returned goods re-enter stock
/// exactly once, inside whichever transaction moves the sub-order to
/// `returned`.
///
/// Wave 23: every restocked unit also writes its `stock_adjustments`
/// audit row in the same transaction, under the `reason` the caller
/// threads in — `cancel_restock` (buyer and seller cancels),
/// `return_restock` (a received buyer return) or `rto_restock`
/// (a courier-driven return). The reason is a caller-supplied
/// constant, never request data.
///
/// Wave 33: a line that bought a variant restocks THE VARIANT
/// (its audit row carries the variant id and the variant's
/// post-move count) — even if the variant has since been
/// deactivated, because the physical goods came back and the
/// seller can reactivate it; a variant-less line restocks the
/// product exactly as before.
pub(crate) async fn restock_sub_order_items(
    conn: &mut sqlx::PgConnection,
    sub_order_id: Uuid,
    reason: &str,
) -> Result<(), ApiError> {
    let store_id: Uuid = sqlx::query_scalar("SELECT store_id FROM sub_orders WHERE id = $1")
        .bind(sub_order_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| ApiError::internal("load sub-order store for restock", e))?;
    let items: Vec<RestockItem> = sqlx::query_as(
        "SELECT product_id, variant_id, quantity FROM order_items WHERE sub_order_id = $1",
    )
    .bind(sub_order_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load items for restock", e))?;
    for item in items {
        let (new_stock, movement_variant_id) = match item.variant_id {
            Some(variant_id) => {
                let variant_stock: i32 = sqlx::query_scalar(
                    "UPDATE product_variants SET stock = stock + $2, updated_at = now() \
                     WHERE id = $1 RETURNING stock",
                )
                .bind(variant_id)
                .bind(item.quantity)
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| ApiError::internal("restock variant", e))?;
                (variant_stock, Some(variant_id))
            }
            None => {
                let product_stock: i32 = sqlx::query_scalar(
                    "UPDATE products SET stock = stock + $2, updated_at = now() \
                     WHERE id = $1 RETURNING stock",
                )
                .bind(item.product_id)
                .bind(item.quantity)
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| ApiError::internal("restock product", e))?;
                (product_stock, None)
            }
        };
        crate::inventory::record_stock_adjustment(
            conn,
            &crate::inventory::StockMovement {
                product_id: item.product_id,
                variant_id: movement_variant_id,
                store_id,
                actor: "system",
                actor_user_id: None,
                delta: item.quantity,
                new_stock,
                reason,
                note: None,
            },
        )
        .await?;
    }
    Ok(())
}

/// Cancel the caller's own order. Allowed only while EVERY sub-order
/// is still `placed` (the pure domain rule `can_buyer_cancel`); after
/// any seller has confirmed, the buyer gets a 409. Runs in ONE
/// transaction: lock the order and its sub-orders, flip the order and
/// all sub-orders to `cancelled`, and restore stock for every item.
pub async fn cancel_order_for_buyer(
    pool: &PgPool,
    user: &AuthUser,
    order_id: Uuid,
) -> Result<Order, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin cancel transaction", e))?;

    let order_facts: Option<(Uuid, String, i64)> = sqlx::query_as(
        "SELECT buyer_user_id, payment_status, advance_paid_poisha \
         FROM orders WHERE id = $1 FOR UPDATE",
    )
    .bind(order_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock order for cancel", e))?;
    let (buyer_user_id, order_payment_status, advance_paid_poisha) =
        order_facts.ok_or_else(|| ApiError::not_found("order not found"))?;
    if buyer_user_id != user.user_id {
        // Same rule as the detail endpoint: never leak another
        // buyer's order through a distinguishable error.
        return Err(ApiError::not_found("order not found"));
    }

    let sub_orders: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status FROM sub_orders WHERE order_id = $1 FOR UPDATE",
    )
    .bind(order_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock sub-orders for cancel", e))?;

    let statuses: Vec<&str> = sub_orders.iter().map(|(_, s)| s.as_str()).collect();
    if !can_buyer_cancel(&statuses) {
        return Err(ApiError::conflict(
            "order can no longer be cancelled — a seller has already confirmed it",
        ));
    }

    // Wave 39 (security audit CR-07): an order whose money was
    // actually COLLECTED online — paid in full, or a COD advance
    // already paid — cannot be cancelled here: this path has no
    // refund machinery, and cancelling used to flip the order
    // while the platform silently kept the buyer's money. The
    // cancel is blocked with a 409 naming the refund path (the
    // admin returns flow); nothing is flipped and nothing is
    // restocked. Orders where nothing was collected (COD
    // pay-on-delivery, an advance still pending, a failed or
    // never-started online payment) cancel exactly as before.
    if order_payment_status == "paid" || advance_paid_poisha > 0 {
        return Err(ApiError::conflict(
            "এই অর্ডারের টাকা অনলাইনে পরিশোধ হয়ে গেছে — বাতিল করতে হলে রিফান্ডের \
             জন্য অ্যাডমিনের রিটার্ন প্রক্রিয়া ব্যবহার করুন (this order was already \
             paid online — it cannot be cancelled directly; the refund goes through \
             the admin returns flow)",
        ));
    }

    sqlx::query("UPDATE orders SET status = 'cancelled' WHERE id = $1")
        .bind(order_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("cancel order", e))?;

    // Wave 39 (security audit CR-08): void the order's still-
    // pending payment row in the same transaction, so nothing
    // settleable survives the cancel — a gateway callback that
    // arrives later finds a `failed` row (and the settlement
    // path's own cancelled-order guard refuses to move money
    // regardless). `failed` is the honest void state: the domain
    // payment lifecycle has no `cancelled`, and the buyer's
    // retry path only revives payments on live orders.
    sqlx::query(
        "UPDATE payments SET status = 'failed', updated_at = now() \
         WHERE order_id = $1 AND status = 'pending'",
    )
    .bind(order_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("void pending payments on cancel", e))?;
    sqlx::query("UPDATE sub_orders SET status = 'cancelled' WHERE order_id = $1")
        .bind(order_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("cancel sub-orders", e))?;

    for (sub_order_id, _) in &sub_orders {
        restock_sub_order_items(&mut tx, *sub_order_id, "cancel_restock").await?;
        // Wave 35: each cancelled sub-order also gives back the
        // points its share of the checkout redemption bought
        // (the `loyalty` module's cancel hook), in this
        // transaction — a whole-order cancel therefore refunds
        // the order's full redemption, share by share.
        crate::loyalty::refund_for_cancelled_sub_order(&mut tx, *sub_order_id).await?;
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit cancel", e))?;

    load_order(pool, order_id).await
}

// ---------------------------------------------------------------------------
// Buyer receipt (Wave 4)
// ---------------------------------------------------------------------------

/// Platform name shown on receipts (placeholder until the product is
/// named — see the MVP plan's owner decisions).
pub const PLATFORM_NAME: &str = "BD Marketplace";

/// Delivery details as printed on the receipt.
#[derive(Debug, Serialize)]
pub struct ReceiptDelivery {
    pub name: String,
    pub phone: String,
    pub address: String,
    pub zone: String,
}

/// One seller's slice of the receipt. Deliberately has NO commission
/// or seller payout fields: those are platform/seller figures, not
/// buyer information.
#[derive(Debug, Serialize)]
pub struct ReceiptSubOrder {
    pub store_name: String,
    pub store_slug: String,
    pub status: String,
    pub goods_subtotal_poisha: i64,
    pub delivery_charge_poisha: i64,
    pub items: Vec<OrderItem>,
    /// The sub-order's event timeline (Wave 55) — the
    /// receipt page doubles as the buyer's order detail
    /// page, and the timeline is its tracking section.
    pub timeline: Vec<crate::timeline::TimelineEvent>,
    /// The tracking stepper (Wave 55), copied from the
    /// sub-order's served steps.
    pub steps: Vec<crate::timeline::TimelineStep>,
}

/// The buyer receipt view-model: everything needed to render or print
/// a receipt, and nothing the buyer should not see.
#[derive(Debug, Serialize)]
pub struct Receipt {
    pub platform_name: &'static str,
    pub order_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub status: String,
    pub payment_method: String,
    /// The order's payment lifecycle: `pay_on_delivery` for COD,
    /// otherwise `pending`, `paid`, `failed` or `refunded` (Wave 5);
    /// Wave 17 adds `advance_pending` / `advance_paid` for COD
    /// orders with an advance.
    pub payment_status: String,
    /// Amount the buyer hands over on delivery: the grand total
    /// minus any advance already collected, for COD orders; zero
    /// for online orders (a paid online order has nothing due; a
    /// pending one is settled online, not in cash). With no
    /// advance this is exactly the Wave 4 behaviour.
    pub cod_amount_due_poisha: i64,
    /// The advance this COD order required at checkout (Wave 17;
    /// 0 when none did — including every online order).
    pub advance_required_poisha: i64,
    /// The advance actually collected online (Wave 17), set only
    /// when the advance payment settled.
    pub advance_paid_poisha: i64,
    pub delivery: ReceiptDelivery,
    /// The buyer's order note (Wave 55), echoed back so
    /// the buyer sees what the sellers were told.
    pub order_note: Option<String>,
    pub sub_orders: Vec<ReceiptSubOrder>,
    pub goods_total_poisha: i64,
    /// The coupon applied at checkout (Wave 14) and its total
    /// discount; the receipt renders the discount as its own line
    /// when it is positive.
    pub coupon_code: Option<String>,
    pub discount_total_poisha: i64,
    /// Loyalty points redeemed at checkout (Wave 35) and the
    /// discount they bought; the grand total (and so the
    /// cash-due above) is already net of it. The receipt
    /// renders the discount as its own line when positive.
    pub loyalty_points_used: i64,
    pub loyalty_discount_poisha: i64,
    pub delivery_total_poisha: i64,
    pub grand_total_poisha: i64,
}

/// Build the receipt view-model from a loaded order plus a
/// store-id → store-name lookup (receipts show the store's display
/// name; the order itself only carries slugs). Pure assembly — the
/// single builder every receipt response goes through.
pub fn build_receipt(order: &Order, store_names: &HashMap<Uuid, String>) -> Receipt {
    let sub_orders = order
        .sub_orders
        .iter()
        .map(|sub| ReceiptSubOrder {
            store_name: store_names
                .get(&sub.store_id)
                .cloned()
                .unwrap_or_else(|| sub.store_slug.clone()),
            store_slug: sub.store_slug.clone(),
            status: sub.status.clone(),
            goods_subtotal_poisha: sub.goods_subtotal_poisha,
            delivery_charge_poisha: sub.delivery_charge_poisha,
            items: sub.items.clone(),
            timeline: sub.timeline.clone(),
            steps: sub.steps.clone(),
        })
        .collect();
    Receipt {
        platform_name: PLATFORM_NAME,
        order_id: order.id,
        created_at: order.created_at,
        status: order.status.clone(),
        payment_method: order.payment_method.clone(),
        payment_status: order.payment_status.clone(),
        cod_amount_due_poisha: if order.payment_method == "cod" {
            // Wave 17: a collected advance is money the buyer has
            // already paid — the courier collects only the rest.
            // `advance_paid_poisha` is 0 unless the advance
            // settled, so advance-free receipts are unchanged.
            order.grand_total_poisha - order.advance_paid_poisha
        } else {
            0
        },
        advance_required_poisha: order.advance_required_poisha,
        advance_paid_poisha: order.advance_paid_poisha,
        delivery: ReceiptDelivery {
            name: order.delivery_name.clone(),
            phone: order.delivery_phone.clone(),
            address: order.delivery_address.clone(),
            zone: order.delivery_zone.clone(),
        },
        order_note: order.order_note.clone(),
        sub_orders,
        goods_total_poisha: order.goods_total_poisha,
        coupon_code: order.coupon_code.clone(),
        discount_total_poisha: order.discount_total_poisha,
        loyalty_points_used: order.loyalty_points_used,
        loyalty_discount_poisha: order.loyalty_discount_poisha,
        delivery_total_poisha: order.delivery_total_poisha,
        grand_total_poisha: order.grand_total_poisha,
    }
}

/// The caller's own receipt: ownership via the shared buyer-order
/// loader rule (another buyer's order is a 404), store display names
/// resolved with one extra lookup, then the shared receipt builder.
pub async fn get_receipt_for_buyer(
    pool: &PgPool,
    user: &AuthUser,
    order_id: Uuid,
) -> Result<Receipt, ApiError> {
    let order = get_order_for_buyer(pool, user, order_id).await?;

    let store_ids: Vec<Uuid> = order.sub_orders.iter().map(|sub| sub.store_id).collect();
    let name_rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM stores WHERE id = ANY($1)")
            .bind(&store_ids)
            .fetch_all(pool)
            .await
            .map_err(|e| ApiError::internal("load store names for receipt", e))?;
    let store_names: HashMap<Uuid, String> = name_rows.into_iter().collect();

    Ok(build_receipt(&order, &store_names))
}

// ---------------------------------------------------------------------------
// Seller sub-orders
// ---------------------------------------------------------------------------

#[derive(FromRow)]
struct SellerSubOrderRow {
    id: Uuid,
    order_id: Uuid,
    store_id: Uuid,
    store_slug: String,
    status: String,
    goods_subtotal_poisha: i64,
    discount_poisha: i64,
    delivery_charge_poisha: i64,
    commission_poisha: i64,
    seller_payout_poisha: i64,
    created_at: DateTime<Utc>,
    delivery_name: String,
    delivery_phone: String,
    delivery_address: String,
    delivery_zone: String,
    order_note: Option<String>,
    delivery_district: Option<String>,
    buyer_user_id: Uuid,
    order_payment_status: String,
    advance_required_poisha: i64,
    advance_paid_poisha: i64,
}

/// Sub-orders belonging to stores the caller owns, newest first.
pub async fn list_seller_sub_orders(
    pool: &PgPool,
    user: &AuthUser,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<SellerSubOrderList, ApiError> {
    // Wave 34: the scope is every store the caller may work
    // orders in — their own stores plus member stores whose
    // role carries `manage_orders` (all roles do). For an
    // owner-only caller the set is exactly their stores, so
    // the list is unchanged.
    let store_ids =
        accessible_store_ids(pool, user.user_id, domain::STORE_ACTION_MANAGE_ORDERS).await?;
    list_seller_sub_orders_scoped(pool, &store_ids, page, limit).await
}

/// The seller sub-order list over an explicit store scope —
/// the shared body behind [`list_seller_sub_orders`] and
/// the Wave 34 store-scoped dashboard (which lists ONE
/// store's recent sub-orders).
pub(crate) async fn list_seller_sub_orders_scoped(
    pool: &PgPool,
    store_ids: &[Uuid],
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<SellerSubOrderList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sub_orders so WHERE so.store_id = ANY($1)",
    )
    .bind(store_ids)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count seller sub-orders", e))?;

    let rows: Vec<SellerSubOrderRow> = sqlx::query_as(
        "SELECT so.id, so.order_id, so.store_id, s.slug AS store_slug, so.status, \
         so.goods_subtotal_poisha, so.discount_poisha, so.delivery_charge_poisha, so.commission_poisha, \
         so.seller_payout_poisha, so.created_at, \
         o.delivery_name, o.delivery_phone, o.delivery_address, o.delivery_zone, \
         o.order_note, o.delivery_district, \
         o.buyer_user_id, o.payment_status AS order_payment_status, \
         o.advance_required_poisha, o.advance_paid_poisha \
         FROM sub_orders so \
         JOIN stores s ON s.id = so.store_id \
         JOIN orders o ON o.id = so.order_id \
         WHERE so.store_id = ANY($1) \
         ORDER BY so.created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(store_ids)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list seller sub-orders", e))?;

    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let order_items = load_order_items(pool, row.id).await?;
        let shipment = load_shipment_summary_for_sub_order(pool, row.id).await?;
        // Wave 55: the seller list carries the same
        // timeline + stepper the buyer sees.
        let timeline =
            crate::timeline::sub_order_timeline(pool, row.id, &row.status, row.created_at)
                .await?;
        let steps = crate::timeline::sub_order_steps(
            &row.status,
            shipment.as_ref().map(|summary| summary.status.as_str()),
        );
        // Wave 17: the buyer's risk level, from the ONE shared
        // aggregate — one call per listed sub-order (pages are
        // small; correctness over micro-optimisation, and the
        // verdict is by construction the same one checkout used).
        let buyer_risk_level = buyer_risk_for_user(pool, row.buyer_user_id)
            .await?
            .risk
            .level
            .as_str()
            .to_string();
        items.push(SellerSubOrder {
            id: row.id,
            order_id: row.order_id,
            store_id: row.store_id,
            store_slug: row.store_slug,
            status: row.status,
            goods_subtotal_poisha: row.goods_subtotal_poisha,
            discount_poisha: row.discount_poisha,
            delivery_charge_poisha: row.delivery_charge_poisha,
            commission_poisha: row.commission_poisha,
            seller_payout_poisha: row.seller_payout_poisha,
            created_at: row.created_at,
            delivery_name: row.delivery_name,
            delivery_phone: row.delivery_phone,
            delivery_address: row.delivery_address,
            delivery_zone: row.delivery_zone,
            order_note: row.order_note,
            delivery_district: row.delivery_district,
            items: order_items,
            timeline,
            steps,
            shipment,
            order_payment_status: row.order_payment_status,
            advance_required_poisha: row.advance_required_poisha,
            advance_paid_poisha: row.advance_paid_poisha,
            buyer_risk_level,
        });
    }
    Ok(SellerSubOrderList {
        items,
        total,
        page,
        limit,
    })
}

/// Move one of the caller's sub-orders to a new fulfilment status.
/// Ownership first (404 unknown id, 403 someone else's store), then the
/// pure domain transition table decides legality (400 on illegal moves).
/// The whole update runs in ONE transaction; when the move is a seller
/// cancellation, the sub-order's items are restocked in the same
/// transaction (the shared restock helper the buyer cancel also uses),
/// so a cancelled parcel's stock can never be lost.
pub async fn update_sub_order_status(
    pool: &PgPool,
    user: &AuthUser,
    sub_order_id: Uuid,
    new_status: &str,
) -> Result<SubOrder, ApiError> {
    if !is_valid_sub_order_status(new_status) {
        return Err(ApiError::bad_request(format!(
            "unknown sub-order status '{new_status}'"
        )));
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin status transaction", e))?;

    // Wave 39 (security audit CR-07): for a CANCELLATION, lock the
    // parent order row and read its payment facts FIRST — before
    // the sub-order lock below, so this transaction takes the
    // order-then-sub-order lock order the buyer-cancel path uses
    // (the reverse order could deadlock the two cancels against
    // each other). The facts decide below whether the cancel may
    // proceed at all; locking the order also means a payment
    // settlement cannot land between the check and the cancel.
    let cancel_parent_facts: Option<(String, i64)> = if new_status == "cancelled" {
        sqlx::query_as(
            "SELECT o.payment_status, o.advance_paid_poisha FROM orders o \
             JOIN sub_orders so ON so.order_id = o.id \
             WHERE so.id = $1 FOR UPDATE OF o",
        )
        .bind(sub_order_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("lock parent order for cancel guard", e))?
    } else {
        None
    };

    let current: Option<(String, Uuid, Uuid)> = sqlx::query_as(
        "SELECT so.status, s.owner_user_id, so.store_id FROM sub_orders so \
         JOIN stores s ON s.id = so.store_id WHERE so.id = $1 FOR UPDATE OF so",
    )
    .bind(sub_order_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("load sub-order for status update", e))?;
    let (current_status, owner_user_id, store_id) =
        current.ok_or_else(|| ApiError::not_found("sub-order not found"))?;
    // Wave 34: the owner, or a member whose role carries
    // `manage_orders` (every member role does). A caller with
    // no relationship keeps this module's historical 403.
    if owner_user_id != user.user_id {
        match store_role_on(&mut *tx, user.user_id, store_id).await? {
            Some(role) => require_role(&role, domain::STORE_ACTION_MANAGE_ORDERS)?,
            None => {
                return Err(ApiError::forbidden(
                    "you do not own this sub-order's store",
                ))
            }
        }
    }
    if !can_transition_sub_order(&current_status, new_status) {
        return Err(ApiError::bad_request(format!(
            "cannot move sub-order from '{current_status}' to '{new_status}'"
        )));
    }

    if new_status == "cancelled" && current_status != "cancelled" {
        // Wave 39 (security audit CR-07): the seller-side mirror
        // of the buyer-cancel rule, judged on the parent order's
        // payment facts locked at the top of this transaction.
        // Cancelling a sub-order whose parent order was already
        // PAID online (in full, or a COD advance collected) would
        // strand the buyer's money with no refund path, so it is
        // blocked with a 409 naming the admin returns flow —
        // before any restock happens. Orders where nothing was
        // collected cancel (and restock) as before.
        if let Some((parent_payment_status, parent_advance_paid)) = &cancel_parent_facts {
            if parent_payment_status == "paid" || *parent_advance_paid > 0 {
                return Err(ApiError::conflict(
                    "এই অর্ডারের টাকা অনলাইনে পরিশোধ হয়ে গেছে — সাব-অর্ডার বাতিল করা \
                     যাবে না; রিফান্ডের জন্য অ্যাডমিনের রিটার্ন প্রক্রিয়া ব্যবহার করুন \
                     (this order was already paid online — the sub-order cannot be \
                     cancelled; the refund goes through the admin returns flow)",
                ));
            }
        }
        restock_sub_order_items(&mut tx, sub_order_id, "cancel_restock").await?;
    }

    sqlx::query("UPDATE sub_orders SET status = $2 WHERE id = $1")
        .bind(sub_order_id)
        .bind(new_status)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("update sub-order status", e))?;

    // Wave 29: notify the buyer of the moves they care
    // about, in the same transaction. The transition table
    // has no self-moves, so reaching this point always means
    // the status really changed — no no-op notification is
    // possible. Delivered can also arrive via the shipment
    // sync; a sub-order reaches it exactly once, so the two
    // paths never both fire (see the `notices` module doc).
    let notice_kind = match new_status {
        "shipped" => Some(domain::NOTIFICATION_SUB_ORDER_SHIPPED),
        "delivered" => Some(domain::NOTIFICATION_SUB_ORDER_DELIVERED),
        "cancelled" => Some(domain::NOTIFICATION_SUB_ORDER_CANCELLED),
        _ => None,
    };
    if let Some(kind) = notice_kind {
        crate::notices::enqueue_sub_order_notice(&mut tx, sub_order_id, kind).await?;
    }

    // Wave 35: the loyalty hooks ride this same funnel, in
    // this transaction — a delivery credits the buyer's earn
    // (the `loyalty` module's earn-once rule makes a repeat
    // impossible, and this transition itself is one-way), and
    // a seller cancellation refunds the points the sub-order's
    // redemption share bought.
    if new_status == "delivered" {
        crate::loyalty::earn_for_sub_order(&mut tx, sub_order_id).await?;
    }
    if new_status == "cancelled" {
        crate::loyalty::refund_for_cancelled_sub_order(&mut tx, sub_order_id).await?;
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit status update", e))?;

    load_sub_order(pool, sub_order_id).await
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListOrdersQuery {
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct UpdateSubOrderStatusRequest {
    pub status: String,
}

pub async fn create_order(
    State(state): State<AppState>,
    user: AuthUser,
    Json(input): Json<PlaceOrderInput>,
) -> Result<(StatusCode, Json<Order>), ApiError> {
    let pool = state.require_pool()?;
    let order = place_order(pool, &state, &user, input).await?;
    Ok((StatusCode::CREATED, Json(order)))
}

pub async fn list_orders(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListOrdersQuery>,
) -> Result<Json<OrderList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_orders_for_buyer(pool, &user, query.page, query.limit).await?,
    ))
}

pub async fn get_order(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Order>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_order_for_buyer(pool, &user, id).await?))
}

pub async fn cancel_order(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Order>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(cancel_order_for_buyer(pool, &user, id).await?))
}

/// POST /v1/orders/:id/reorder — "buy again" (Wave 55):
/// re-add the order's lines at today's prices, with the
/// per-line report. Always 200 for the buyer's own
/// non-cancelled order — partial success IS the success
/// shape here; the report says what did not come back.
pub async fn reorder_order_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ReorderReport>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(reorder_order(pool, &user, id).await?))
}

pub async fn get_receipt(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Receipt>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(get_receipt_for_buyer(pool, &user, id).await?))
}

pub async fn list_seller_sub_orders_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<ListOrdersQuery>,
) -> Result<Json<SellerSubOrderList>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        list_seller_sub_orders(pool, &user, query.page, query.limit).await?,
    ))
}

pub async fn update_sub_order_status_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateSubOrderStatusRequest>,
) -> Result<Json<SubOrder>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        update_sub_order_status(pool, &user, id, &req.status).await?,
    ))
}
