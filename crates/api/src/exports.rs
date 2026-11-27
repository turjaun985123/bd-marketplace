//! Data portability exports (Wave 31): a signed-in seller or
//! buyer downloads ONE JSON document holding everything the
//! platform stores about them. This is the portability
//! promise made concrete — a seller can leave with their
//! catalogue, sales history and money ledger; a buyer can
//! leave with their orders, reviews, wishlist and questions.
//!
//! The export MIRRORS STORED FIELDS: rows are serialized as
//! they sit in the tables (plus the items nested under their
//! sub-orders), not re-shaped into the public API views. A
//! public view hides and derives; an export must not — the
//! owner is entitled to the record as kept. The one exception
//! is the product section, which reuses the shared `Product`
//! view so ratings and stock health come along in the form the
//! seller already knows.
//!
//! Seller document (`GET /v1/seller/export`, 404 when the
//! caller owns no store):
//!
//! - `store`: the seller's primary store (the same
//!   earliest-created store every seller surface uses),
//!   including its verification tier.
//! - `products`: ALL products of ALL the seller's stores,
//!   drafts and archived included — an export that silently
//!   dropped drafts would not be the seller's catalogue.
//! - `sub_orders`: the newest 1_000 sub-orders across the
//!   seller's stores, each with its order items nested.
//! - `payouts`, `coupons`, `promotions`: the store-scoped
//!   money and marketing rows.
//! - `charges_total_poisha`: the seller's lifetime
//!   `seller_charges` total (Wave 26) — the same figure the
//!   earnings aggregate subtracts.
//!
//! Buyer document (`GET /v1/buyer/export`):
//!
//! - `profile`: phone (in FULL — it is the buyer's own data,
//!   shown to themselves; the masking used on public surfaces
//!   protects users from OTHER people, never from themselves),
//!   name, role and account creation time.
//! - `orders`: the newest 1_000 orders with sub-orders and
//!   items nested. Delivery details live on the order rows,
//!   so the buyer's addresses travel with the orders — there
//!   is no separate address book to export.
//! - `reviews`, `wishlist` (product ids + titles) and
//!   `questions`: the buyer's own contributions.
//!
//! Both responses are file downloads: `Content-Disposition:
//! attachment` with the domain `export_filename` name. An
//! export is a SNAPSHOT for the owner's records — it is not
//! a migration format, and nothing in the platform imports
//! it back.

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue},
    Json,
};
use chrono::{DateTime, Utc};
use domain::export_filename;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use std::collections::HashMap;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::coupons::{Coupon, COUPON_COLUMNS};
use crate::orders::OrderItem;
use crate::payouts::Payout;
use crate::products::{with_stock_status, Product, PRODUCT_COLUMNS, PRODUCT_RATING_JOIN};
use crate::promotions::Promotion;
use crate::stores::{get_store_by_owner, Store};
use crate::{ApiError, AppState};

/// How many orders / sub-orders an export carries at most.
/// The cap keeps one request bounded; the newest rows win,
/// which matches what an owner reconciling recent history
/// needs. Everything uncapped (products, payouts, coupons,
/// promotions, reviews, wishlist, questions) is small
/// per-account data by nature.
const EXPORT_ROW_LIMIT: i64 = 1_000;

// ---------------------------------------------------------------------------
// Shared row shapes
// ---------------------------------------------------------------------------

/// One `sub_orders` row as stored, shared by both exports.
#[derive(Debug, Clone, FromRow, Serialize)]
struct ExportSubOrderRow {
    id: Uuid,
    order_id: Uuid,
    store_id: Uuid,
    status: String,
    goods_subtotal_poisha: i64,
    discount_poisha: i64,
    delivery_charge_poisha: i64,
    commission_poisha: i64,
    seller_payout_poisha: i64,
    created_at: DateTime<Utc>,
}

/// A sub-order row with its order items nested under it.
#[derive(Debug, Serialize)]
struct ExportSubOrder {
    #[serde(flatten)]
    row: ExportSubOrderRow,
    items: Vec<OrderItem>,
}

const EXPORT_SUB_ORDER_COLUMNS: &str = "id, order_id, store_id, status, goods_subtotal_poisha, \
    discount_poisha, delivery_charge_poisha, commission_poisha, seller_payout_poisha, created_at";

/// Group order items under their sub-orders. Items arrive in
/// one query for the whole export; this is the only assembly
/// step, and sub-orders without items (impossible in
/// practice) export with an empty list rather than vanishing.
fn nest_items(
    rows: Vec<ExportSubOrderRow>,
    items: Vec<OrderItem>,
) -> Vec<ExportSubOrder> {
    let mut items_by_sub_order: HashMap<Uuid, Vec<OrderItem>> = HashMap::new();
    for item in items {
        items_by_sub_order
            .entry(item.sub_order_id)
            .or_default()
            .push(item);
    }
    rows.into_iter()
        .map(|row| {
            let items = items_by_sub_order.remove(&row.id).unwrap_or_default();
            ExportSubOrder { row, items }
        })
        .collect()
}

/// Load the items for a set of sub-orders in one query.
async fn load_items_for(
    pool: &PgPool,
    sub_order_ids: &[Uuid],
) -> Result<Vec<OrderItem>, ApiError> {
    if sub_order_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as(
        "SELECT id, sub_order_id, product_id, variant_id, variant_name, \
         title_snapshot, unit_price_poisha, \
         quantity, line_total_poisha FROM order_items \
         WHERE sub_order_id = ANY($1) ORDER BY id",
    )
    .bind(sub_order_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export order items", e))
}

// ---------------------------------------------------------------------------
// Seller export
// ---------------------------------------------------------------------------

/// The seller document plus the filename it downloads as.
pub struct ExportDownload {
    pub filename: String,
    pub document: Value,
}

/// Assemble the seller's export document. The primary store
/// comes from the one shared owner lookup, so a user without
/// a store gets the same 404 here as on the dashboard; the
/// catalogue and ledger sections span ALL the seller's
/// stores, because tenancy is per store and an export scoped
/// to one store would silently drop the others' history.
pub async fn seller_export(pool: &PgPool, user: &AuthUser) -> Result<ExportDownload, ApiError> {
    let store: Store = get_store_by_owner(pool, user.user_id).await?;

    let store_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM stores WHERE owner_user_id = $1 ORDER BY created_at ASC")
            .bind(user.user_id)
            .fetch_all(pool)
            .await
            .map_err(|e| ApiError::internal("export seller stores", e))?;

    build_seller_export(pool, store, &store_ids, None).await
}

/// The Wave 34 store-scoped export: ONE store's document,
/// for a caller the handler resolved under `view_finances`
/// (the owner, or a manager of that store). The charges
/// total narrows to the charges that store's own promotions
/// caused — the owner's other stores' charges are the
/// owner's data, not this store's team's.
pub async fn seller_export_scoped(
    pool: &PgPool,
    store: Store,
) -> Result<ExportDownload, ApiError> {
    let store_ids = vec![store.id];
    let charges_store = store.id;
    build_seller_export(pool, store, &store_ids, Some(charges_store)).await
}

/// The export assembly shared by both scopes: `store` is
/// the document's primary store, `store_ids` the section
/// scope, and `charges_store` — when set — narrows the
/// charges total to that store's promotions.
async fn build_seller_export(
    pool: &PgPool,
    store: Store,
    store_ids: &[Uuid],
    charges_store: Option<Uuid>,
) -> Result<ExportDownload, ApiError> {

    let mut products: Vec<Product> = sqlx::query_as(&format!(
        "SELECT {PRODUCT_COLUMNS} FROM products p JOIN stores s ON s.id = p.store_id \
         {PRODUCT_RATING_JOIN} \
         WHERE p.store_id = ANY($1) ORDER BY p.created_at ASC"
    ))
    .bind(store_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export products", e))?
    .into_iter()
    .map(with_stock_status)
    .collect();
    // Wave 52: the export's product views are the shared
    // view, flash fields included, like every other surface.
    crate::merch::attach_flash_prices(pool, &mut products, chrono::Utc::now()).await?;

    let sub_order_rows: Vec<ExportSubOrderRow> = sqlx::query_as(&format!(
        "SELECT {EXPORT_SUB_ORDER_COLUMNS} FROM sub_orders \
         WHERE store_id = ANY($1) ORDER BY created_at DESC LIMIT $2"
    ))
    .bind(store_ids)
    .bind(EXPORT_ROW_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export sub-orders", e))?;
    let sub_order_ids: Vec<Uuid> = sub_order_rows.iter().map(|row| row.id).collect();
    let items = load_items_for(pool, &sub_order_ids).await?;
    let sub_orders = nest_items(sub_order_rows, items);

    let payouts: Vec<Payout> = sqlx::query_as(
        "SELECT id, store_id, amount_poisha, method, account_name, account_number, \
         status, note, created_at, processed_at, idempotency_key \
         FROM payouts WHERE store_id = ANY($1) ORDER BY created_at DESC",
    )
    .bind(store_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export payouts", e))?;

    let coupons: Vec<Coupon> = sqlx::query_as(&format!(
        "SELECT {COUPON_COLUMNS} FROM coupons \
         WHERE store_id = ANY($1) ORDER BY created_at DESC"
    ))
    .bind(store_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export coupons", e))?;

    let promotions: Vec<Promotion> = sqlx::query_as(
        "SELECT id, product_id, store_id, seller_user_id, starts_at, ends_at, days, \
         cost_poisha, status, created_at \
         FROM promotions WHERE store_id = ANY($1) ORDER BY created_at DESC",
    )
    .bind(store_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export promotions", e))?;

    let charges_total_poisha: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(sc.amount_poisha), 0)::bigint FROM seller_charges sc \
         WHERE sc.seller_user_id = $1 \
           AND ($2::uuid IS NULL OR EXISTS ( \
                SELECT 1 FROM promotions pr \
                WHERE pr.id = sc.ref_id AND pr.store_id = $2))",
    )
    .bind(store.owner_user_id)
    .bind(charges_store)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("export charges total", e))?;

    let now = Utc::now();
    let filename = export_filename("seller", &store.slug, &now.format("%Y%m%d").to_string());
    let document = json!({
        "exported_at": now,
        "kind": "seller",
        "store": store,
        "products": products,
        "sub_orders": sub_orders,
        "payouts": payouts,
        "coupons": coupons,
        "promotions": promotions,
        "charges_total_poisha": charges_total_poisha,
    });
    Ok(ExportDownload { filename, document })
}

// ---------------------------------------------------------------------------
// Buyer export
// ---------------------------------------------------------------------------

/// The buyer's own account row, as exported. The phone is the
/// buyer's own, in full — see the module doc.
#[derive(Debug, FromRow, Serialize)]
struct BuyerProfile {
    phone: String,
    name: Option<String>,
    role: String,
    created_at: DateTime<Utc>,
}

/// One `orders` row as stored.
#[derive(Debug, Clone, FromRow, Serialize)]
struct ExportOrderRow {
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
    delivery_total_poisha: i64,
    grand_total_poisha: i64,
    created_at: DateTime<Utc>,
}

/// An order row with its sub-orders (and their items) nested.
#[derive(Debug, Serialize)]
struct ExportOrder {
    #[serde(flatten)]
    row: ExportOrderRow,
    sub_orders: Vec<ExportSubOrder>,
}

/// One of the buyer's reviews, as stored.
#[derive(Debug, FromRow, Serialize)]
struct ExportReview {
    id: Uuid,
    product_id: Uuid,
    order_id: Uuid,
    rating: i16,
    body: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// One wishlist entry: the product id plus its current title,
/// so the export stays readable after the fact.
#[derive(Debug, FromRow, Serialize)]
struct ExportWishlistItem {
    product_id: Uuid,
    title: String,
    created_at: DateTime<Utc>,
}

/// One followed store (Wave 54): the store's slug and
/// name plus when the follow began — the buyer's own
/// subscription record, exported like the wishlist.
#[derive(Debug, FromRow, Serialize)]
struct ExportFollowedStore {
    store_id: Uuid,
    slug: String,
    name: String,
    created_at: DateTime<Utc>,
}

/// One question the buyer asked, as stored (hidden ones
/// included — it is the asker's own record).
#[derive(Debug, FromRow, Serialize)]
struct ExportQuestion {
    id: Uuid,
    product_id: Uuid,
    body: String,
    answer_body: Option<String>,
    answered_at: Option<DateTime<Utc>>,
    is_hidden: bool,
    created_at: DateTime<Utc>,
}

/// Assemble the buyer's export document.
pub async fn buyer_export(pool: &PgPool, user: &AuthUser) -> Result<ExportDownload, ApiError> {
    let profile: BuyerProfile =
        sqlx::query_as("SELECT phone, name, role, created_at FROM users WHERE id = $1")
            .bind(user.user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::internal("export buyer profile", e))?
            .ok_or_else(|| ApiError::not_found("user not found"))?;

    let order_rows: Vec<ExportOrderRow> = sqlx::query_as(
        "SELECT id, buyer_user_id, status, payment_method, payment_status, \
         advance_required_poisha, advance_paid_poisha, \
         delivery_name, delivery_phone, delivery_address, delivery_zone, \
         goods_total_poisha, coupon_code, discount_total_poisha, \
         delivery_total_poisha, grand_total_poisha, created_at \
         FROM orders WHERE buyer_user_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(user.user_id)
    .bind(EXPORT_ROW_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export orders", e))?;

    let order_ids: Vec<Uuid> = order_rows.iter().map(|row| row.id).collect();
    let sub_order_rows: Vec<ExportSubOrderRow> = if order_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(&format!(
            "SELECT {EXPORT_SUB_ORDER_COLUMNS} FROM sub_orders \
             WHERE order_id = ANY($1) ORDER BY created_at ASC"
        ))
        .bind(&order_ids)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("export order sub-orders", e))?
    };
    let sub_order_ids: Vec<Uuid> = sub_order_rows.iter().map(|row| row.id).collect();
    let items = load_items_for(pool, &sub_order_ids).await?;
    let sub_orders = nest_items(sub_order_rows, items);
    let mut sub_orders_by_order: HashMap<Uuid, Vec<ExportSubOrder>> = HashMap::new();
    for sub_order in sub_orders {
        sub_orders_by_order
            .entry(sub_order.row.order_id)
            .or_default()
            .push(sub_order);
    }
    let orders: Vec<ExportOrder> = order_rows
        .into_iter()
        .map(|row| {
            let sub_orders = sub_orders_by_order.remove(&row.id).unwrap_or_default();
            ExportOrder { row, sub_orders }
        })
        .collect();

    let reviews: Vec<ExportReview> = sqlx::query_as(
        "SELECT id, product_id, order_id, rating, body, created_at, updated_at \
         FROM reviews WHERE buyer_id = $1 ORDER BY created_at DESC",
    )
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export reviews", e))?;

    let wishlist: Vec<ExportWishlistItem> = sqlx::query_as(
        "SELECT w.product_id, p.title, w.created_at \
         FROM wishlist_items w JOIN products p ON p.id = w.product_id \
         WHERE w.user_id = $1 ORDER BY w.created_at DESC",
    )
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export wishlist", e))?;

    let questions: Vec<ExportQuestion> = sqlx::query_as(
        "SELECT id, product_id, body, answer_body, answered_at, is_hidden, created_at \
         FROM product_questions WHERE asker_user_id = $1 ORDER BY created_at DESC",
    )
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export questions", e))?;

    // Wave 54: the buyer's followed stores ride the same
    // document — a follow is the buyer's data exactly like
    // a wishlist entry.
    let followed_stores: Vec<ExportFollowedStore> = sqlx::query_as(
        "SELECT f.store_id, s.slug, s.name, f.created_at \
         FROM store_follows f JOIN stores s ON s.id = f.store_id \
         WHERE f.user_id = $1 ORDER BY f.created_at DESC",
    )
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export followed stores", e))?;

    let now = Utc::now();
    let filename = export_filename("buyer", &profile.phone, &now.format("%Y%m%d").to_string());
    let document = json!({
        "exported_at": now,
        "kind": "buyer",
        "profile": profile,
        "orders": orders,
        "reviews": reviews,
        "wishlist": wishlist,
        "questions": questions,
        "followed_stores": followed_stores,
    });
    Ok(ExportDownload { filename, document })
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

/// The download headers both exports share: a JSON body served
/// as an attachment under the domain filename (the analytics
/// CSV pattern, Wave 27).
fn download_headers(filename: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    headers
}

pub async fn seller_export_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<crate::access::StoreScopeQuery>,
) -> Result<(HeaderMap, Json<Value>), ApiError> {
    let pool = state.require_pool()?;
    // Wave 34: `?store_id=` exports ONE store's document
    // under `view_finances` (owner or manager); without it,
    // the caller's whole seller record, as before.
    let download = match scope.store_id {
        Some(_) => {
            let (store, _role) = crate::access::resolve_scoped_store(
                pool,
                user.user_id,
                scope.store_id,
                domain::STORE_ACTION_VIEW_FINANCES,
            )
            .await?;
            seller_export_scoped(pool, store).await?
        }
        None => seller_export(pool, &user).await?,
    };
    Ok((download_headers(&download.filename), Json(download.document)))
}

pub async fn buyer_export_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<(HeaderMap, Json<Value>), ApiError> {
    let pool = state.require_pool()?;
    let download = buyer_export(pool, &user).await?;
    Ok((download_headers(&download.filename), Json(download.document)))
}
