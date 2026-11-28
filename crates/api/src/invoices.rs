//! Invoices (Wave 22): the per-SELLER legal document for one
//! sub-order, numbered sequentially per store.
//!
//! The Wave 4 receipt and this invoice are different documents
//! on purpose. The receipt is the BUYER's whole-order summary
//! (every seller's slice, no commission or payout figures). The
//! invoice belongs to ONE sub-order of ONE store: it is the
//! document that store issues for its sale, carrying that
//! store's own sequence number (`INV-000001`, `INV-000002`, …),
//! its items, and the money the buyer owes (or owed) for that
//! slice alone.
//!
//! Issuing is lazy and idempotent. Nothing is numbered at
//! checkout or at delivery; the first entitled request for a
//! DELIVERED (or later RETURNED — the sale happened, so its
//! invoice stands) sub-order's invoice assigns the store's
//! next sequence inside one transaction, and every later
//! request returns the same row with the same number. A
//! sequence is never reused: the per-store counter only moves
//! forward, under a row lock, so concurrent issues for one
//! store draw distinct numbers. Sub-orders that have not been
//! delivered (placed, confirmed, shipped, cancelled) have no
//! invoice yet — asking is a 409, because there is no completed
//! sale to document.
//!
//! The money block mirrors the receipt's definitions, scoped
//! to the sub-order: the goods, discount and delivery figures
//! are the sub-order's own stored snapshot columns; the
//! sub-order grand is goods minus discount minus any
//! gift-card redemption (Wave 60 — a sub-order snapshot like
//! the discount, unlike order-level loyalty) plus delivery. A
//! collected COD advance (Wave 17) lives on the parent order,
//! so the invoice allocates it across the order's sub-orders
//! in proportion to each sub-order's share of the order grand
//! total — for the common single-seller order that is exactly
//! the receipt's `grand_total - advance_paid`, and the cash
//! due on the invoice is this sub-order's grand minus its
//! advance share (zero for online orders, which are never
//! settled in cash on delivery). Integer division can leave a
//! poisha or two of a multi-seller order's advance unallocated
//! across its invoices; the whole-order receipt remains the
//! authority for the order-level due.
//!
//! Access mirrors the rest of the API: the buyer who placed
//! the parent order, the seller who owns the sub-order's
//! store, or an admin. Anyone else gets a 404 — a sub-order id
//! must not leak another party's sale, not even its existence.

use axum::{
    extract::{Path, State},
    Json,
};
use chrono::{DateTime, Utc};
use domain::format_invoice_number;
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::admin::require_admin;
use crate::auth::AuthUser;
use crate::orders::{load_order, OrderItem};
use crate::stores::get_store_by_id;
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------------
// Public shapes
// ---------------------------------------------------------------------------

/// The issuing store's block on the invoice: display name and
/// slug (the slug is half of the invoice's identity — numbers
/// are per-store), its verification tier, and its brand colour
/// for the print view's accent.
#[derive(Debug, Serialize)]
pub struct InvoiceStore {
    pub name: String,
    pub slug: String,
    pub verified_tier: String,
    pub brand_color: Option<String>,
}

/// The buyer block: the parent order's delivery details, the
/// same fields the receipt prints.
#[derive(Debug, Serialize)]
pub struct InvoiceBuyer {
    pub name: String,
    pub phone: String,
    pub address: String,
    pub zone: String,
    /// The order's delivery district (Wave 55), when the
    /// order recorded one; null for zone-only orders.
    pub district: Option<String>,
}

/// The invoice's money block, all in poisha and all scoped to
/// THIS sub-order (see the module docs for the advance rule).
#[derive(Debug, Serialize)]
pub struct InvoiceMoney {
    pub goods_subtotal_poisha: i64,
    pub discount_poisha: i64,
    /// The gift-card redemption on this sub-order (Wave 60):
    /// stored value the buyer paid with, so it comes off the
    /// grand exactly like the coupon discount does. (Loyalty
    /// stays off the invoice — it is an ORDER-level figure,
    /// while the gift redemption is this sub-order's own
    /// snapshot column.)
    pub gift_card_discount_poisha: i64,
    pub delivery_charge_poisha: i64,
    /// Goods minus discount minus gift-card redemption plus
    /// delivery: what this slice of the order costs the
    /// buyer in total.
    pub grand_total_poisha: i64,
    /// This sub-order's allocated share of any advance the
    /// parent COD order collected online (Wave 17); 0 when no
    /// advance was collected.
    pub advance_paid_poisha: i64,
    /// Cash still to hand over on delivery for this slice:
    /// the grand minus the advance share for COD orders, 0
    /// for online orders.
    pub cod_amount_due_poisha: i64,
}

/// One issued invoice, ready to render or print.
#[derive(Debug, Serialize)]
pub struct Invoice {
    pub id: Uuid,
    pub sub_order_id: Uuid,
    pub order_id: Uuid,
    pub invoice_number: String,
    /// The store-local sequence behind `invoice_number`.
    pub seq: i64,
    pub issued_at: DateTime<Utc>,
    /// The sub-order's status when this view was built:
    /// `delivered`, or `returned` for a sale that later came
    /// back (the invoice of the sale stands).
    pub sub_order_status: String,
    pub store: InvoiceStore,
    pub buyer: InvoiceBuyer,
    /// The buyer's order note (Wave 55) from the parent
    /// order; null when none was written.
    pub order_note: Option<String>,
    pub items: Vec<OrderItem>,
    pub money: InvoiceMoney,
    pub payment_method: String,
    /// The parent order's payment lifecycle (see `Order`).
    pub payment_status: String,
}

/// Who is asking for the invoice. The three endpoints differ
/// only in this: the service function applies the matching
/// ownership rule (admins pass `require_admin` in their
/// handler first and may read any sub-order's invoice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceActor {
    Buyer,
    Seller,
    Admin,
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(FromRow)]
struct InvoiceRow {
    id: Uuid,
    store_id: Uuid,
    sub_order_id: Uuid,
    seq: i64,
    invoice_number: String,
    issued_at: DateTime<Utc>,
}

/// Everything the service needs to decide access and status
/// for one sub-order, in one join.
#[derive(FromRow)]
struct SubOrderContext {
    order_id: Uuid,
    store_id: Uuid,
    status: String,
    buyer_user_id: Uuid,
    store_owner_user_id: Uuid,
}

const INVOICE_COLUMNS: &str = "id, store_id, sub_order_id, seq, invoice_number, issued_at";

async fn load_invoice_row(
    pool: &PgPool,
    sub_order_id: Uuid,
) -> Result<Option<InvoiceRow>, ApiError> {
    sqlx::query_as(&format!(
        "SELECT {INVOICE_COLUMNS} FROM invoices WHERE sub_order_id = $1"
    ))
    .bind(sub_order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load invoice", e))
}

/// Postgres unique-violation (SQLSTATE 23505): the backstop
/// signal that a concurrent issuer won the race for this
/// sub-order's invoice row.
fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(|db| db.code())
        .is_some_and(|code| code.as_ref() == "23505")
}

// ---------------------------------------------------------------------------
// The one service function
// ---------------------------------------------------------------------------

/// Get the invoice for one sub-order, issuing it first when
/// this is the first entitled request. See the module docs
/// for the numbering, status and access rules.
pub async fn get_or_issue_invoice(
    pool: &PgPool,
    user: &AuthUser,
    sub_order_id: Uuid,
    actor: InvoiceActor,
) -> Result<Invoice, ApiError> {
    let ctx: SubOrderContext = sqlx::query_as(
        "SELECT so.order_id, so.store_id, so.status, o.buyer_user_id, \
         s.owner_user_id AS store_owner_user_id \
         FROM sub_orders so \
         JOIN orders o ON o.id = so.order_id \
         JOIN stores s ON s.id = so.store_id \
         WHERE so.id = $1",
    )
    .bind(sub_order_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("load sub-order for invoice", e))?
    .ok_or_else(|| ApiError::not_found("sub-order not found"))?;

    // Access: a stranger's sub-order is a 404, never a 403 —
    // the same no-leak rule the order and return endpoints use.
    let entitled = match actor {
        InvoiceActor::Buyer => ctx.buyer_user_id == user.user_id,
        InvoiceActor::Seller => ctx.store_owner_user_id == user.user_id,
        InvoiceActor::Admin => true,
    };
    if !entitled {
        return Err(ApiError::not_found("sub-order not found"));
    }

    // Only a completed sale has an invoice. `returned` counts:
    // the goods were delivered and invoiced before they came
    // back, and the document of that sale must stay readable.
    if ctx.status != "delivered" && ctx.status != "returned" {
        return Err(ApiError::conflict(format!(
            "an invoice is issued once the sub-order is delivered \
             (current status: '{}')",
            ctx.status
        )));
    }

    let row = match load_invoice_row(pool, sub_order_id).await? {
        Some(existing) => existing,
        None => issue_invoice(pool, &ctx, sub_order_id).await?,
    };

    build_invoice_view(pool, &ctx, &row).await
}

/// Assign the store's next sequence to this sub-order and
/// insert its invoice row, in ONE transaction: the counter
/// row is created if needed and locked `FOR UPDATE`, which
/// serialises issuance per store — two concurrent issues for
/// one store can never draw the same sequence, and a second
/// issuer for the SAME sub-order (which waited on that lock)
/// re-checks inside the transaction and returns the winner's
/// row instead of burning a second number.
async fn issue_invoice(
    pool: &PgPool,
    ctx: &SubOrderContext,
    sub_order_id: Uuid,
) -> Result<InvoiceRow, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin invoice transaction", e))?;

    sqlx::query(
        "INSERT INTO store_invoice_counters (store_id) VALUES ($1) \
         ON CONFLICT (store_id) DO NOTHING",
    )
    .bind(ctx.store_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("ensure invoice counter", e))?;

    let next_seq: i64 = sqlx::query_scalar(
        "SELECT next_seq FROM store_invoice_counters WHERE store_id = $1 FOR UPDATE",
    )
    .bind(ctx.store_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("lock invoice counter", e))?;

    // The re-check: while this transaction waited on the
    // counter lock, the winning issuer committed — its row is
    // visible now, and it is the answer.
    let existing: Option<InvoiceRow> = sqlx::query_as(&format!(
        "SELECT {INVOICE_COLUMNS} FROM invoices WHERE sub_order_id = $1"
    ))
    .bind(sub_order_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("re-check invoice", e))?;
    if let Some(row) = existing {
        tx.commit()
            .await
            .map_err(|e| ApiError::internal("commit invoice re-check", e))?;
        return Ok(row);
    }

    let invoice_number = format_invoice_number(next_seq);
    let inserted = sqlx::query_as(&format!(
        "INSERT INTO invoices (store_id, sub_order_id, seq, invoice_number) \
         VALUES ($1, $2, $3, $4) RETURNING {INVOICE_COLUMNS}"
    ))
    .bind(ctx.store_id)
    .bind(sub_order_id)
    .bind(next_seq)
    .bind(&invoice_number)
    .fetch_one(&mut *tx)
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(err) if is_unique_violation(&err) => {
            // Backstop for any race the counter lock did not
            // serialise: drop the transaction (it rolls back)
            // and return the winner's committed row.
            drop(tx);
            return load_invoice_row(pool, sub_order_id)
                .await?
                .ok_or_else(|| ApiError::internal("load invoice after race", sqlx::Error::RowNotFound));
        }
        Err(err) => return Err(ApiError::internal("insert invoice", err)),
    };

    sqlx::query("UPDATE store_invoice_counters SET next_seq = next_seq + 1 WHERE store_id = $1")
        .bind(ctx.store_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("advance invoice counter", e))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit invoice", e))?;

    Ok(row)
}

/// Assemble the invoice view from an issued row. The order
/// data comes from the ONE shared order loader (the same
/// items and money snapshots the receipt is built from — the
/// invoice never forks those queries), and the store block
/// from the shared store loader.
async fn build_invoice_view(
    pool: &PgPool,
    ctx: &SubOrderContext,
    row: &InvoiceRow,
) -> Result<Invoice, ApiError> {
    let order = load_order(pool, ctx.order_id).await?;
    let sub = order
        .sub_orders
        .iter()
        .find(|sub| sub.id == row.sub_order_id)
        .ok_or_else(|| ApiError::internal("find invoiced sub-order", sqlx::Error::RowNotFound))?;
    let store = get_store_by_id(pool, row.store_id).await?;

    let grand_total_poisha = sub.goods_subtotal_poisha - sub.discount_poisha
        - sub.gift_card_discount_poisha
        + sub.delivery_charge_poisha;
    // The collected advance is an order-level figure; this
    // sub-order's share is proportional to its share of the
    // order grand total (exact for single-seller orders).
    let advance_share_poisha = if order.payment_method == "cod"
        && order.advance_paid_poisha > 0
        && order.grand_total_poisha > 0
    {
        order.advance_paid_poisha * grand_total_poisha / order.grand_total_poisha
    } else {
        0
    };
    let cod_amount_due_poisha = if order.payment_method == "cod" {
        (grand_total_poisha - advance_share_poisha).max(0)
    } else {
        0
    };

    Ok(Invoice {
        id: row.id,
        sub_order_id: row.sub_order_id,
        order_id: ctx.order_id,
        invoice_number: row.invoice_number.clone(),
        seq: row.seq,
        issued_at: row.issued_at,
        sub_order_status: ctx.status.clone(),
        store: InvoiceStore {
            name: store.name,
            slug: store.slug,
            verified_tier: store.verified_tier,
            brand_color: store.brand_color,
        },
        buyer: InvoiceBuyer {
            name: order.delivery_name,
            phone: order.delivery_phone,
            address: order.delivery_address,
            zone: order.delivery_zone,
            district: order.delivery_district.clone(),
        },
        order_note: order.order_note.clone(),
        items: sub.items.clone(),
        money: InvoiceMoney {
            goods_subtotal_poisha: sub.goods_subtotal_poisha,
            discount_poisha: sub.discount_poisha,
            gift_card_discount_poisha: sub.gift_card_discount_poisha,
            delivery_charge_poisha: sub.delivery_charge_poisha,
            grand_total_poisha,
            advance_paid_poisha: advance_share_poisha,
            cod_amount_due_poisha,
        },
        payment_method: order.payment_method,
        payment_status: order.payment_status,
    })
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn get_buyer_invoice(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Invoice>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        get_or_issue_invoice(pool, &user, id, InvoiceActor::Buyer).await?,
    ))
}

pub async fn get_seller_invoice(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Invoice>, ApiError> {
    let pool = state.require_pool()?;
    Ok(Json(
        get_or_issue_invoice(pool, &user, id, InvoiceActor::Seller).await?,
    ))
}

pub async fn get_admin_invoice(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Invoice>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(
        get_or_issue_invoice(pool, &user, id, InvoiceActor::Admin).await?,
    ))
}
