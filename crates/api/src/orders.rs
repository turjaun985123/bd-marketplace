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

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use domain::{
    build_order_totals, can_buyer_cancel, can_transition_sub_order, is_valid_sub_order_status,
    normalize_bd_phone, parse_delivery_zone, split_cart_by_seller, CartItem as DomainCartItem,
    OrderGroupInput,
};
use payments::{payment_adapter_for, parse_payment_method};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use std::collections::HashMap;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::payments::{insert_pending_payment, load_latest_payment_summary, PaymentSummary};
use crate::products::clamp_pagination;
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
    pub delivery_charge_poisha: i64,
    pub commission_poisha: i64,
    pub seller_payout_poisha: i64,
    pub created_at: DateTime<Utc>,
    pub items: Vec<OrderItem>,
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
    pub payment_status: String,
    /// The latest payment attempt, or `None` for COD orders (which
    /// deliberately have no payment row).
    pub payment: Option<PaymentSummary>,
    pub delivery_name: String,
    pub delivery_phone: String,
    pub delivery_address: String,
    pub delivery_zone: String,
    pub goods_total_poisha: i64,
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
    pub delivery_charge_poisha: i64,
    pub commission_poisha: i64,
    pub seller_payout_poisha: i64,
    pub created_at: DateTime<Utc>,
    pub delivery_name: String,
    pub delivery_phone: String,
    pub delivery_address: String,
    pub delivery_zone: String,
    pub items: Vec<OrderItem>,
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
    delivery_name: String,
    delivery_phone: String,
    delivery_address: String,
    delivery_zone: String,
    goods_total_poisha: i64,
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
    delivery_charge_poisha: i64,
    commission_poisha: i64,
    seller_payout_poisha: i64,
    created_at: DateTime<Utc>,
}

const ORDER_COLUMNS: &str = "id, buyer_user_id, status, payment_method, payment_status, \
    delivery_name, delivery_phone, delivery_address, delivery_zone, goods_total_poisha, \
    delivery_total_poisha, grand_total_poisha, created_at";

const SUB_ORDER_COLUMNS: &str = "so.id, so.order_id, so.store_id, s.slug AS store_slug, \
    so.status, so.goods_subtotal_poisha, so.delivery_charge_poisha, so.commission_poisha, \
    so.seller_payout_poisha, so.created_at";

async fn load_order_items(pool: &PgPool, sub_order_id: Uuid) -> Result<Vec<OrderItem>, ApiError> {
    sqlx::query_as(
        "SELECT id, sub_order_id, product_id, title_snapshot, unit_price_poisha, \
         quantity, line_total_poisha FROM order_items WHERE sub_order_id = $1 ORDER BY id",
    )
    .bind(sub_order_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load order items", e))
}

async fn sub_order_from_row(pool: &PgPool, row: SubOrderRow) -> Result<SubOrder, ApiError> {
    let items = load_order_items(pool, row.id).await?;
    Ok(SubOrder {
        id: row.id,
        order_id: row.order_id,
        store_id: row.store_id,
        store_slug: row.store_slug,
        status: row.status,
        goods_subtotal_poisha: row.goods_subtotal_poisha,
        delivery_charge_poisha: row.delivery_charge_poisha,
        commission_poisha: row.commission_poisha,
        seller_payout_poisha: row.seller_payout_poisha,
        created_at: row.created_at,
        items,
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
        payment,
        delivery_name: row.delivery_name,
        delivery_phone: row.delivery_phone,
        delivery_address: row.delivery_address,
        delivery_zone: row.delivery_zone,
        goods_total_poisha: row.goods_total_poisha,
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
}

fn default_payment_method() -> String {
    "cod".to_string()
}

/// A cart line locked for checkout (`FOR UPDATE` on the product row).
#[derive(FromRow)]
struct LockedCartItem {
    product_id: Uuid,
    store_id: Uuid,
    title: String,
    unit_price_poisha: i64,
    stock: i32,
    status: String,
    quantity: i32,
    weight_grams: i32,
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
        "SELECT ci.product_id, p.store_id, p.title, \
         p.price_poisha AS unit_price_poisha, p.stock, p.status, ci.quantity, \
         p.weight_grams \
         FROM cart_items ci JOIN products p ON p.id = ci.product_id \
         WHERE ci.cart_id = $1 FOR UPDATE OF p",
    )
    .bind(cart_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock cart items", e))?;

    if locked.is_empty() {
        return Err(ApiError::bad_request("cart is empty"));
    }
    for item in &locked {
        if item.status != "active" {
            return Err(ApiError::bad_request(format!(
                "product '{}' is no longer available",
                item.title
            )));
        }
        if item.stock < item.quantity {
            return Err(ApiError::conflict(format!(
                "insufficient stock for '{}': requested {}, available {}",
                item.title, item.quantity, item.stock
            )));
        }
    }

    // Split per seller and compute ALL money via the domain functions.
    let domain_items: Vec<DomainCartItem> = locked
        .iter()
        .map(|item| DomainCartItem {
            product_id: item.product_id.to_string(),
            store_id: item.store_id.to_string(),
            quantity: item.quantity as u32,
            unit_price_poisha: item.unit_price_poisha,
            unit_weight_grams: i64::from(item.weight_grams),
        })
        .collect();
    // The split items carry their real unit weights, so each group's
    // parcel weight is derived inside `build_order_totals` from the
    // items themselves — no weight is passed in separately.
    let groups: Vec<OrderGroupInput> = split_cart_by_seller(&domain_items)
        .iter()
        .map(|sub| OrderGroupInput {
            store_id: sub.store_id.clone(),
            items: sub.items.clone(),
        })
        .collect();
    let totals = build_order_totals(&groups, zone, state.commission_rate_bps);

    let order_payment_status = if method.is_online() {
        "pending"
    } else {
        "pay_on_delivery"
    };
    let order_id: Uuid = sqlx::query_scalar(
        "INSERT INTO orders (buyer_user_id, payment_method, payment_status, \
         delivery_name, delivery_phone, delivery_address, delivery_zone, \
         goods_total_poisha, delivery_total_poisha, grand_total_poisha) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
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
    .bind(totals.grand_total_poisha)
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
        let intent = payments::PaymentAdapter::initiate(
            &*adapter,
            &order_id.to_string(),
            totals.grand_total_poisha,
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
            totals.grand_total_poisha,
            &intent.reference,
        )
        .await?;
    }

    // Snapshots for order_items, keyed by product id.
    let locked_by_product: std::collections::HashMap<Uuid, &LockedCartItem> =
        locked.iter().map(|item| (item.product_id, item)).collect();

    for (group, sub_totals) in groups.iter().zip(totals.sub_orders.iter()) {
        let store_id = Uuid::parse_str(&group.store_id)
            .map_err(|_| ApiError::internal("parse store id", sqlx::Error::RowNotFound))?;
        let sub_order_id: Uuid = sqlx::query_scalar(
            "INSERT INTO sub_orders (order_id, store_id, goods_subtotal_poisha, \
             delivery_charge_poisha, commission_poisha, seller_payout_poisha) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(order_id)
        .bind(store_id)
        .bind(sub_totals.goods_subtotal_poisha)
        .bind(sub_totals.delivery_charge_poisha)
        .bind(sub_totals.commission_poisha)
        .bind(sub_totals.seller_payout_poisha)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("insert sub-order", e))?;

        for item in &group.items {
            let product_id = Uuid::parse_str(&item.product_id)
                .map_err(|_| ApiError::internal("parse product id", sqlx::Error::RowNotFound))?;
            let locked_item = locked_by_product
                .get(&product_id)
                .ok_or_else(|| ApiError::internal("find locked item", sqlx::Error::RowNotFound))?;
            sqlx::query(
                "INSERT INTO order_items (sub_order_id, product_id, title_snapshot, \
                 unit_price_poisha, quantity, line_total_poisha) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(sub_order_id)
            .bind(product_id)
            .bind(&locked_item.title)
            .bind(item.unit_price_poisha)
            .bind(item.quantity as i32)
            .bind(item.line_total_poisha())
            .execute(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("insert order item", e))?;

            sqlx::query(
                "UPDATE products SET stock = stock - $2, updated_at = now() WHERE id = $1",
            )
            .bind(product_id)
            .bind(item.quantity as i32)
            .execute(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("decrement stock", e))?;
        }
    }

    sqlx::query("DELETE FROM cart_items WHERE cart_id = $1")
        .bind(cart_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("clear cart after checkout", e))?;

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

/// Put one sub-order's items back into stock: for every order item,
/// `products.stock` grows by the ordered quantity. Shared by the buyer
/// whole-order cancel and the seller sub-order cancel — both run it
/// inside their own transaction on the connection handed in here, so a
/// cancel and its restock always commit or roll back together.
async fn restock_sub_order_items(
    conn: &mut sqlx::PgConnection,
    sub_order_id: Uuid,
) -> Result<(), ApiError> {
    let items: Vec<(Uuid, i32)> =
        sqlx::query_as("SELECT product_id, quantity FROM order_items WHERE sub_order_id = $1")
            .bind(sub_order_id)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| ApiError::internal("load items for restock", e))?;
    for (product_id, quantity) in items {
        sqlx::query("UPDATE products SET stock = stock + $2, updated_at = now() WHERE id = $1")
            .bind(product_id)
            .bind(quantity)
            .execute(&mut *conn)
            .await
            .map_err(|e| ApiError::internal("restock product", e))?;
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
        restock_sub_order_items(&mut tx, *sub_order_id).await?;
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
    /// otherwise `pending`, `paid`, `failed` or `refunded` (Wave 5).
    pub payment_status: String,
    /// Amount the buyer hands over on delivery: the grand total for
    /// COD orders, zero for online orders (a paid online order has
    /// nothing due; a pending one is settled online, not in cash).
    pub cod_amount_due_poisha: i64,
    pub delivery: ReceiptDelivery,
    pub sub_orders: Vec<ReceiptSubOrder>,
    pub goods_total_poisha: i64,
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
            order.grand_total_poisha
        } else {
            0
        },
        delivery: ReceiptDelivery {
            name: order.delivery_name.clone(),
            phone: order.delivery_phone.clone(),
            address: order.delivery_address.clone(),
            zone: order.delivery_zone.clone(),
        },
        sub_orders,
        goods_total_poisha: order.goods_total_poisha,
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
    delivery_charge_poisha: i64,
    commission_poisha: i64,
    seller_payout_poisha: i64,
    created_at: DateTime<Utc>,
    delivery_name: String,
    delivery_phone: String,
    delivery_address: String,
    delivery_zone: String,
}

/// Sub-orders belonging to stores the caller owns, newest first.
pub async fn list_seller_sub_orders(
    pool: &PgPool,
    user: &AuthUser,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<SellerSubOrderList, ApiError> {
    let (page, limit) = clamp_pagination(page, limit);
    let offset = (page - 1) * limit;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sub_orders so JOIN stores s ON s.id = so.store_id \
         WHERE s.owner_user_id = $1",
    )
    .bind(user.user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count seller sub-orders", e))?;

    let rows: Vec<SellerSubOrderRow> = sqlx::query_as(
        "SELECT so.id, so.order_id, so.store_id, s.slug AS store_slug, so.status, \
         so.goods_subtotal_poisha, so.delivery_charge_poisha, so.commission_poisha, \
         so.seller_payout_poisha, so.created_at, \
         o.delivery_name, o.delivery_phone, o.delivery_address, o.delivery_zone \
         FROM sub_orders so \
         JOIN stores s ON s.id = so.store_id \
         JOIN orders o ON o.id = so.order_id \
         WHERE s.owner_user_id = $1 \
         ORDER BY so.created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(user.user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list seller sub-orders", e))?;

    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let order_items = load_order_items(pool, row.id).await?;
        items.push(SellerSubOrder {
            id: row.id,
            order_id: row.order_id,
            store_id: row.store_id,
            store_slug: row.store_slug,
            status: row.status,
            goods_subtotal_poisha: row.goods_subtotal_poisha,
            delivery_charge_poisha: row.delivery_charge_poisha,
            commission_poisha: row.commission_poisha,
            seller_payout_poisha: row.seller_payout_poisha,
            created_at: row.created_at,
            delivery_name: row.delivery_name,
            delivery_phone: row.delivery_phone,
            delivery_address: row.delivery_address,
            delivery_zone: row.delivery_zone,
            items: order_items,
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

    let current: Option<(String, Uuid)> = sqlx::query_as(
        "SELECT so.status, s.owner_user_id FROM sub_orders so \
         JOIN stores s ON s.id = so.store_id WHERE so.id = $1 FOR UPDATE OF so",
    )
    .bind(sub_order_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("load sub-order for status update", e))?;
    let (current_status, owner_user_id) =
        current.ok_or_else(|| ApiError::not_found("sub-order not found"))?;
    if owner_user_id != user.user_id {
        return Err(ApiError::forbidden("you do not own this sub-order's store"));
    }
    if !can_transition_sub_order(&current_status, new_status) {
        return Err(ApiError::bad_request(format!(
            "cannot move sub-order from '{current_status}' to '{new_status}'"
        )));
    }

    if new_status == "cancelled" && current_status != "cancelled" {
        restock_sub_order_items(&mut tx, sub_order_id).await?;
    }

    sqlx::query("UPDATE sub_orders SET status = $2 WHERE id = $1")
        .bind(sub_order_id)
        .bind(new_status)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("update sub-order status", e))?;

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
