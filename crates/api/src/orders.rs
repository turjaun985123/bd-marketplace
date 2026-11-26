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

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    allocate_discount, build_order_totals, can_buyer_cancel, can_transition_sub_order,
    compute_advance_poisha, compute_coupon_discount, is_valid_sub_order_status,
    loyalty_discount_poisha, loyalty_max_redeemable_points, normalize_bd_phone,
    normalize_coupon_code, parse_delivery_zone, split_cart_by_seller, validate_loyalty_request,
    variant_final_price, CartItem as DomainCartItem, OrderGroupInput,
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
    pub items: Vec<OrderItem>,
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
    delivery_name, delivery_phone, delivery_address, delivery_zone, goods_total_poisha, \
    coupon_code, discount_total_poisha, loyalty_points_used, loyalty_discount_poisha, \
    delivery_total_poisha, grand_total_poisha, created_at";

const SUB_ORDER_COLUMNS: &str = "so.id, so.order_id, so.store_id, s.slug AS store_slug, \
    so.status, so.goods_subtotal_poisha, so.discount_poisha, so.loyalty_discount_poisha, \
    so.delivery_charge_poisha, so.commission_poisha, \
    so.seller_payout_poisha, so.created_at";

async fn load_order_items(pool: &PgPool, sub_order_id: Uuid) -> Result<Vec<OrderItem>, ApiError> {
    sqlx::query_as(
        "SELECT id, sub_order_id, product_id, variant_id, variant_name, title_snapshot, \
         unit_price_poisha, quantity, line_total_poisha \
         FROM order_items WHERE sub_order_id = $1 ORDER BY id",
    )
    .bind(sub_order_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load order items", e))
}

async fn sub_order_from_row(pool: &PgPool, row: SubOrderRow) -> Result<SubOrder, ApiError> {
    let items = load_order_items(pool, row.id).await?;
    let shipment = load_shipment_summary_for_sub_order(pool, row.id).await?;
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
        group_goods.push(GroupGoods {
            store_id,
            goods_subtotal_poisha: group.items.iter().map(|item| item.line_total_poisha()).sum(),
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
    if input.delivery_name.trim().is_empty() {
        return Err(ApiError::bad_request("delivery_name must not be empty"));
    }
    if input.delivery_address.trim().is_empty() {
        return Err(ApiError::bad_request("delivery_address must not be empty"));
    }
    if let Some(points) = input.loyalty_points {
        validate_loyalty_request(points).map_err(ApiError::bad_request)?;
    }
    let delivery_phone = normalize_bd_phone(&input.delivery_phone)
        .ok_or_else(|| ApiError::bad_request("invalid Bangladeshi phone number"))?;
    let zone = parse_delivery_zone(&input.delivery_zone).ok_or_else(|| {
        ApiError::bad_request("delivery_zone must be one of: inside_dhaka, dhaka_suburb, outside_dhaka")
    })?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin checkout transaction", e))?;

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
         WHERE ci.cart_id = $1 FOR UPDATE OF p",
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

    // The FINAL price of every line, computed once here (base
    // + delta through the domain rule for variant lines) and
    // reused by the totals and the snapshots below.
    let mut final_price_by_line: HashMap<(Uuid, Option<Uuid>), i64> = HashMap::new();
    for item in &locked {
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
                let price = variant_final_price(item.unit_price_poisha, variant.price_delta_poisha)
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
                item.unit_price_poisha
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
    let mut groups: Vec<OrderGroupInput> = split_cart_by_seller(&domain_items)
        .iter()
        .map(|sub| OrderGroupInput {
            store_id: sub.store_id.clone(),
            items: sub.items.clone(),
            discount_poisha: 0,
            commission_on_discounted: false,
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
    let commission_rate_bps = crate::admin::effective_commission_bps(state).await;
    let totals = build_order_totals(&groups, zone, commission_rate_bps);

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
         loyalty_points_used, loyalty_discount_poisha) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING id",
    )
    .bind(user.user_id)
    .bind(method.as_str())
    .bind(order_payment_status)
    .bind(input.delivery_name.trim())
    .bind(&delivery_phone)
    .bind(input.delivery_address.trim())
    .bind(zone.as_str())
    .bind(totals.goods_total_poisha)
    .bind(totals.delivery_total_poisha)
    .bind(payable_grand_total)
    .bind(applied_coupon.as_ref().map(|coupon| coupon.code.clone()))
    .bind(totals.discount_total_poisha)
    .bind(advance_required_poisha)
    .bind(i32::try_from(loyalty_points_used).unwrap_or(i32::MAX))
    .bind(loyalty_discount_total)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("insert order", e))?;

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
            .bind(item.line_total_poisha())
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

    sqlx::query("DELETE FROM cart_items WHERE cart_id = $1")
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

    let buyer_user_id: Uuid = sqlx::query_scalar(
        "SELECT buyer_user_id FROM orders WHERE id = $1 FOR UPDATE",
    )
    .bind(order_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock order for cancel", e))?
    .ok_or_else(|| ApiError::not_found("order not found"))?;
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

    sqlx::query("UPDATE orders SET status = 'cancelled' WHERE id = $1")
        .bind(order_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("cancel order", e))?;
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
            items: order_items,
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
