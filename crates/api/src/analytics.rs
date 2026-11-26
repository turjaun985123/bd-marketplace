//! Analytics for sellers and for the platform (Wave 27).
//!
//! Every number on this surface is computed per request from
//! the ledger tables (orders, sub_orders, order_items,
//! payouts, seller_charges, coupon_redemptions) by single
//! GROUP BY aggregates — no rollups, no cached counters, no
//! loading rows into Rust to sum. That is exact today and is
//! the documented scaling limit: at very large volumes the
//! daily series would move to rollup tables, behind these
//! same definitions.
//!
//! The window: the last N days INCLUDING today, normalised by
//! the pure domain function `analytics_days` (absent = 30,
//! clamped to 7..=90). All day boundaries are ASIA/DHAKA
//! calendar days — Bangladesh's own timezone — computed by
//! Postgres (`created_at AT TIME ZONE 'Asia/Dhaka'`), so a
//! sale at 11pm in Dhaka counts on that Dhaka day, not the
//! next UTC day. Sub-orders are the unit of almost every
//! metric, because fulfilment and money move per seller
//! slice; parent orders are counted only where the metric
//! says so.
//!
//! Seller metric definitions (scope: the sub-orders of ALL
//! stores the caller owns, in the window):
//! - `totals.sub_orders`: sub-orders NOT in `cancelled` —
//!   the sales population every seller money figure below
//!   shares. Cancelled slices never moved goods or money.
//! - `totals.goods_poisha`: SUM of `goods_subtotal_poisha`
//!   over that same non-cancelled population (before coupon
//!   discounts — discounts are visible in earnings, because
//!   the payout snapshot already nets them out).
//! - `totals.earnings_poisha`: SUM of the
//!   `seller_payout_poisha` snapshots over DELIVERED
//!   sub-orders only — exactly the Wave 9 earnings rule.
//! - `totals.delivered` / `returned` / `cancelled`: counts
//!   per current sub-order status.
//! - `totals.return_rate_bps`: returned / (delivered +
//!   returned) * 10_000, integer-truncated; 0 when the
//!   denominator is 0 (nothing has come back OR been kept).
//! - `daily`: one row per Dhaka day in the window,
//!   zero-filled by `generate_series` — days without sales
//!   appear with zeros, so the series length is always the
//!   window length. Per day: non-cancelled sub-order count,
//!   their goods, and the delivered slices' earnings.
//! - `top_products`: order items of non-cancelled
//!   sub-orders, grouped by product: units = SUM(quantity),
//!   goods = SUM(line_total_poisha); top 10 by goods.
//! - `status_breakdown`: counts per status over ALL
//!   sub-orders in the window (cancelled included — this is
//!   the operations view, not the money view).
//! - `courier_breakdown`: shipments of the window's
//!   sub-orders, counted per courier; manual bookings count
//!   under their counter name (`manual_courier_name`) rather
//!   than the shared `manual` marker.
//!
//! Admin metric definitions (platform-wide, same window):
//! - `totals.gmv_poisha`: SUM(goods − discount + delivery)
//!   over sub-orders NOT cancelled — what buyers actually
//!   committed to pay for goods and carriage.
//! - `totals.orders`: parent orders CREATED in the window
//!   (the orders table itself, any state).
//! - `totals.sub_orders`: ALL sub-orders created in the
//!   window, cancelled included (the operations volume;
//!   the money populations above say when they exclude).
//! - `totals.commission_poisha`: SUM(goods − discount −
//!   seller_payout) over DELIVERED sub-orders — the
//!   commission the platform has actually earned; for a
//!   delivered slice this equals its stored
//!   `commission_poisha` snapshot.
//! - `totals.new_users` / `new_stores`: rows created in
//!   the window.
//! - `totals.payouts`: `requested_count` and
//!   `requested_poisha` over payouts still in `requested`
//!   that were created in the window; `paid_poisha` over
//!   payouts in `paid` created in the window.
//! - `totals.charges_poisha`: SUM of `seller_charges`
//!   (Wave 26 promotion purchases) created in the window.
//! - `totals.coupons`: `redemptions` = coupon_redemptions
//!   rows created in the window; `discount_poisha` = their
//!   summed discount.
//! - `top_stores`: per store over non-cancelled
//!   sub-orders: GMV by the same formula as the platform
//!   GMV, plus the non-cancelled sub-order count; top 10.
//! - `daily`: per Dhaka day, zero-filled: `orders` =
//!   DISTINCT parent orders among that day's non-cancelled
//!   sub-orders, `gmv_poisha` by the GMV formula.
//!
//! The CSV export carries the seller daily series through
//! the domain CSV builder. Its `orders` column counts
//! SUB-ORDERS (seller slices) for the day — the same value
//! as the JSON daily rows' `sub_orders` field; the column
//! keeps the short name because the file is a seller's own
//! sales ledger, where a slice is the unit of work.

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue},
    Json,
};
use chrono::NaiveDate;
use domain::{analytics_days, build_daily_csv};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::admin::require_admin;
use crate::auth::AuthUser;
use crate::stores::get_store_by_owner;
use crate::{ApiError, AppState};

/// The window start instant, as a SQL fragment: the start
/// (Asia/Dhaka) of the Dhaka day `days - 1` days before
/// today (Asia/Dhaka). `$1` is the clamped day count in
/// every query in this module.
const WINDOW_START: &str =
    "(((now() AT TIME ZONE 'Asia/Dhaka')::date - ($1 - 1))::timestamp \
      AT TIME ZONE 'Asia/Dhaka')";

/// Format a Dhaka date for the wire: `YYYY-MM-DD`.
fn date_string(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

// ---------------------------------------------------------------------------
// Public shapes — seller
// ---------------------------------------------------------------------------

/// The seller totals block; populations are defined in the
/// module doc.
#[derive(Debug, Serialize)]
pub struct SellerAnalyticsTotals {
    pub sub_orders: i64,
    pub goods_poisha: i64,
    pub earnings_poisha: i64,
    pub delivered: i64,
    pub returned: i64,
    pub cancelled: i64,
    pub return_rate_bps: i64,
}

/// One day of the seller series (Dhaka calendar day).
#[derive(Debug, Serialize)]
pub struct SellerDailyRow {
    pub date: String,
    pub sub_orders: i64,
    pub goods_poisha: i64,
    pub earnings_poisha: i64,
}

/// One top-selling product of the window.
#[derive(Debug, Serialize, FromRow)]
pub struct TopProduct {
    pub product_id: Uuid,
    pub title: String,
    pub units: i64,
    pub goods_poisha: i64,
}

/// One status bucket of the breakdown.
#[derive(Debug, Serialize, FromRow)]
pub struct StatusCount {
    pub status: String,
    pub count: i64,
}

/// One courier bucket of the breakdown. Shared by the
/// seller and admin responses.
#[derive(Debug, Serialize, FromRow)]
pub struct CourierCount {
    pub courier: String,
    pub count: i64,
}

/// Everything the seller analytics page needs in one
/// response.
#[derive(Debug, Serialize)]
pub struct SellerAnalytics {
    pub days: i64,
    pub totals: SellerAnalyticsTotals,
    pub daily: Vec<SellerDailyRow>,
    pub top_products: Vec<TopProduct>,
    pub status_breakdown: Vec<StatusCount>,
    pub courier_breakdown: Vec<CourierCount>,
}

// ---------------------------------------------------------------------------
// Public shapes — admin
// ---------------------------------------------------------------------------

/// The admin payouts block: still-requested payouts created
/// in the window, and the paid total created in the window.
#[derive(Debug, Serialize)]
pub struct AdminPayoutTotals {
    pub requested_count: i64,
    pub requested_poisha: i64,
    pub paid_poisha: i64,
}

/// The admin coupons block: redemptions in the window and
/// the discount they gave.
#[derive(Debug, Serialize)]
pub struct AdminCouponTotals {
    pub redemptions: i64,
    pub discount_poisha: i64,
}

/// The platform totals block; definitions in the module
/// doc.
#[derive(Debug, Serialize)]
pub struct AdminAnalyticsTotals {
    pub gmv_poisha: i64,
    pub orders: i64,
    pub sub_orders: i64,
    pub commission_poisha: i64,
    pub new_users: i64,
    pub new_stores: i64,
    pub payouts: AdminPayoutTotals,
    pub charges_poisha: i64,
    pub coupons: AdminCouponTotals,
}

/// One top store of the window, by GMV.
#[derive(Debug, Serialize, FromRow)]
pub struct TopStore {
    pub store_id: Uuid,
    pub slug: String,
    pub name: String,
    pub gmv_poisha: i64,
    pub sub_orders: i64,
}

/// One day of the platform series (Dhaka calendar day).
#[derive(Debug, Serialize)]
pub struct AdminDailyRow {
    pub date: String,
    pub orders: i64,
    pub gmv_poisha: i64,
}

/// Everything the admin analytics page needs in one
/// response.
#[derive(Debug, Serialize)]
pub struct AdminAnalytics {
    pub days: i64,
    pub totals: AdminAnalyticsTotals,
    pub top_stores: Vec<TopStore>,
    pub courier_breakdown: Vec<CourierCount>,
    pub daily: Vec<AdminDailyRow>,
}

// ---------------------------------------------------------------------------
// Service functions — seller
// ---------------------------------------------------------------------------

/// The seller daily series for the window, zero-filled.
/// Shared by the JSON response and the CSV export so the
/// two can never disagree.
async fn seller_daily(
    pool: &PgPool,
    owner_user_id: Uuid,
    days: i64,
) -> Result<Vec<SellerDailyRow>, ApiError> {
    #[derive(FromRow)]
    struct DailyRow {
        day: NaiveDate,
        sub_orders: i64,
        goods_poisha: i64,
        earnings_poisha: i64,
    }

    let rows: Vec<DailyRow> = sqlx::query_as(
        "SELECT gs::date AS day, \
                COUNT(so.id) AS sub_orders, \
                COALESCE(SUM(so.goods_subtotal_poisha), 0)::bigint AS goods_poisha, \
                COALESCE(SUM(so.seller_payout_poisha) \
                    FILTER (WHERE so.status = 'delivered'), 0)::bigint AS earnings_poisha \
           FROM generate_series( \
                    (now() AT TIME ZONE 'Asia/Dhaka')::date - ($1 - 1), \
                    (now() AT TIME ZONE 'Asia/Dhaka')::date, \
                    interval '1 day') AS gs \
           LEFT JOIN sub_orders so \
             ON (so.created_at AT TIME ZONE 'Asia/Dhaka')::date = gs::date \
            AND so.status <> 'cancelled' \
            AND so.store_id IN (SELECT id FROM stores WHERE owner_user_id = $2) \
          GROUP BY gs \
          ORDER BY gs",
    )
    .bind(days)
    .bind(owner_user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("seller analytics daily series", e))?;

    Ok(rows
        .into_iter()
        .map(|row| SellerDailyRow {
            date: date_string(row.day),
            sub_orders: row.sub_orders,
            goods_poisha: row.goods_poisha,
            earnings_poisha: row.earnings_poisha,
        })
        .collect())
}

/// Shipments per courier over the window's sub-orders.
/// `owner_user_id` scopes to one seller's stores; `None`
/// is the platform-wide admin view. Manual bookings count
/// under their counter name.
async fn courier_breakdown(
    pool: &PgPool,
    owner_user_id: Option<Uuid>,
    days: i64,
) -> Result<Vec<CourierCount>, ApiError> {
    sqlx::query_as(&format!(
        "SELECT COALESCE(sh.manual_courier_name, sh.courier) AS courier, \
                COUNT(*) AS count \
           FROM shipments sh \
           JOIN sub_orders so ON so.id = sh.sub_order_id \
          WHERE so.created_at >= {WINDOW_START} \
            AND ($2::uuid IS NULL \
                 OR so.store_id IN \
                    (SELECT id FROM stores WHERE owner_user_id = $2)) \
          GROUP BY 1 \
          ORDER BY count DESC, courier"
    ))
    .bind(days)
    .bind(owner_user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("analytics courier breakdown", e))
}

/// The seller analytics for the caller's stores. A caller
/// without a store gets the same 404 as the dashboard —
/// analytics describe a selling business.
pub async fn seller_analytics(
    pool: &PgPool,
    user: &AuthUser,
    days: i64,
) -> Result<SellerAnalytics, ApiError> {
    get_store_by_owner(pool, user.user_id).await?;

    #[derive(FromRow)]
    struct TotalsRow {
        sub_orders: i64,
        goods_poisha: i64,
        earnings_poisha: i64,
        delivered: i64,
        returned: i64,
        cancelled: i64,
    }

    let totals: TotalsRow = sqlx::query_as(&format!(
        "SELECT COUNT(*) FILTER (WHERE so.status <> 'cancelled') AS sub_orders, \
                COALESCE(SUM(so.goods_subtotal_poisha) \
                    FILTER (WHERE so.status <> 'cancelled'), 0)::bigint AS goods_poisha, \
                COALESCE(SUM(so.seller_payout_poisha) \
                    FILTER (WHERE so.status = 'delivered'), 0)::bigint AS earnings_poisha, \
                COUNT(*) FILTER (WHERE so.status = 'delivered') AS delivered, \
                COUNT(*) FILTER (WHERE so.status = 'returned') AS returned, \
                COUNT(*) FILTER (WHERE so.status = 'cancelled') AS cancelled \
           FROM sub_orders so \
           JOIN stores s ON s.id = so.store_id \
          WHERE s.owner_user_id = $2 AND so.created_at >= {WINDOW_START}"
    ))
    .bind(days)
    .bind(user.user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("seller analytics totals", e))?;

    let settled = totals.delivered + totals.returned;
    let return_rate_bps = if settled > 0 {
        totals.returned * 10_000 / settled
    } else {
        0
    };

    let daily = seller_daily(pool, user.user_id, days).await?;

    let top_products: Vec<TopProduct> = sqlx::query_as(&format!(
        "SELECT oi.product_id AS product_id, p.title AS title, \
                COALESCE(SUM(oi.quantity), 0)::bigint AS units, \
                COALESCE(SUM(oi.line_total_poisha), 0)::bigint AS goods_poisha \
           FROM order_items oi \
           JOIN sub_orders so ON so.id = oi.sub_order_id \
           JOIN products p ON p.id = oi.product_id \
          WHERE so.status <> 'cancelled' \
            AND so.store_id IN (SELECT id FROM stores WHERE owner_user_id = $2) \
            AND so.created_at >= {WINDOW_START} \
          GROUP BY oi.product_id, p.title \
          ORDER BY goods_poisha DESC, oi.product_id \
          LIMIT 10"
    ))
    .bind(days)
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("seller analytics top products", e))?;

    let status_breakdown: Vec<StatusCount> = sqlx::query_as(&format!(
        "SELECT so.status AS status, COUNT(*) AS count \
           FROM sub_orders so \
          WHERE so.store_id IN (SELECT id FROM stores WHERE owner_user_id = $2) \
            AND so.created_at >= {WINDOW_START} \
          GROUP BY so.status \
          ORDER BY so.status"
    ))
    .bind(days)
    .bind(user.user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("seller analytics status breakdown", e))?;

    let courier_breakdown = courier_breakdown(pool, Some(user.user_id), days).await?;

    Ok(SellerAnalytics {
        days,
        totals: SellerAnalyticsTotals {
            sub_orders: totals.sub_orders,
            goods_poisha: totals.goods_poisha,
            earnings_poisha: totals.earnings_poisha,
            delivered: totals.delivered,
            returned: totals.returned,
            cancelled: totals.cancelled,
            return_rate_bps,
        },
        daily,
        top_products,
        status_breakdown,
        courier_breakdown,
    })
}

/// The seller daily series as a CSV download, through the
/// domain CSV builder.
pub async fn seller_analytics_csv(
    pool: &PgPool,
    user: &AuthUser,
    days: i64,
) -> Result<String, ApiError> {
    get_store_by_owner(pool, user.user_id).await?;
    let daily = seller_daily(pool, user.user_id, days).await?;
    let rows: Vec<(String, i64, i64, i64)> = daily
        .into_iter()
        .map(|row| (row.date, row.sub_orders, row.goods_poisha, row.earnings_poisha))
        .collect();
    Ok(build_daily_csv(&rows))
}

// ---------------------------------------------------------------------------
// Service functions — admin
// ---------------------------------------------------------------------------

/// The platform analytics. The caller is authorised by the
/// handler through the one `require_admin` guard before
/// this runs.
pub async fn admin_analytics(pool: &PgPool, days: i64) -> Result<AdminAnalytics, ApiError> {
    #[derive(FromRow)]
    struct TotalsRow {
        gmv_poisha: i64,
        orders: i64,
        sub_orders: i64,
        commission_poisha: i64,
        new_users: i64,
        new_stores: i64,
        payouts_requested_count: i64,
        payouts_requested_poisha: i64,
        payouts_paid_poisha: i64,
        charges_poisha: i64,
        coupon_redemptions: i64,
        coupon_discount_poisha: i64,
    }

    let totals: TotalsRow = sqlx::query_as(&format!(
        "SELECT \
           (SELECT COALESCE(SUM(goods_subtotal_poisha - discount_poisha \
                                + delivery_charge_poisha), 0)::bigint \
              FROM sub_orders \
             WHERE status <> 'cancelled' AND created_at >= {WINDOW_START}) \
             AS gmv_poisha, \
           (SELECT COUNT(*) FROM orders WHERE created_at >= {WINDOW_START}) \
             AS orders, \
           (SELECT COUNT(*) FROM sub_orders WHERE created_at >= {WINDOW_START}) \
             AS sub_orders, \
           (SELECT COALESCE(SUM(goods_subtotal_poisha - discount_poisha \
                                - seller_payout_poisha), 0)::bigint \
              FROM sub_orders \
             WHERE status = 'delivered' AND created_at >= {WINDOW_START}) \
             AS commission_poisha, \
           (SELECT COUNT(*) FROM users WHERE created_at >= {WINDOW_START}) \
             AS new_users, \
           (SELECT COUNT(*) FROM stores WHERE created_at >= {WINDOW_START}) \
             AS new_stores, \
           (SELECT COUNT(*) FROM payouts \
             WHERE status = 'requested' AND created_at >= {WINDOW_START}) \
             AS payouts_requested_count, \
           (SELECT COALESCE(SUM(amount_poisha), 0)::bigint FROM payouts \
             WHERE status = 'requested' AND created_at >= {WINDOW_START}) \
             AS payouts_requested_poisha, \
           (SELECT COALESCE(SUM(amount_poisha), 0)::bigint FROM payouts \
             WHERE status = 'paid' AND created_at >= {WINDOW_START}) \
             AS payouts_paid_poisha, \
           (SELECT COALESCE(SUM(amount_poisha), 0)::bigint \
              FROM seller_charges WHERE created_at >= {WINDOW_START}) \
             AS charges_poisha, \
           (SELECT COUNT(*) FROM coupon_redemptions \
             WHERE created_at >= {WINDOW_START}) \
             AS coupon_redemptions, \
           (SELECT COALESCE(SUM(discount_poisha), 0)::bigint \
              FROM coupon_redemptions WHERE created_at >= {WINDOW_START}) \
             AS coupon_discount_poisha"
    ))
    .bind(days)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("admin analytics totals", e))?;

    let top_stores: Vec<TopStore> = sqlx::query_as(&format!(
        "SELECT so.store_id AS store_id, s.slug AS slug, s.name AS name, \
                COALESCE(SUM(so.goods_subtotal_poisha - so.discount_poisha \
                             + so.delivery_charge_poisha), 0)::bigint AS gmv_poisha, \
                COUNT(*) AS sub_orders \
           FROM sub_orders so \
           JOIN stores s ON s.id = so.store_id \
          WHERE so.status <> 'cancelled' AND so.created_at >= {WINDOW_START} \
          GROUP BY so.store_id, s.slug, s.name \
          ORDER BY gmv_poisha DESC, s.slug \
          LIMIT 10"
    ))
    .bind(days)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("admin analytics top stores", e))?;

    #[derive(FromRow)]
    struct DailyRow {
        day: NaiveDate,
        orders: i64,
        gmv_poisha: i64,
    }

    let daily_rows: Vec<DailyRow> = sqlx::query_as(
        "SELECT gs::date AS day, \
                COUNT(DISTINCT so.order_id) AS orders, \
                COALESCE(SUM(so.goods_subtotal_poisha - so.discount_poisha \
                             + so.delivery_charge_poisha), 0)::bigint AS gmv_poisha \
           FROM generate_series( \
                    (now() AT TIME ZONE 'Asia/Dhaka')::date - ($1 - 1), \
                    (now() AT TIME ZONE 'Asia/Dhaka')::date, \
                    interval '1 day') AS gs \
           LEFT JOIN sub_orders so \
             ON (so.created_at AT TIME ZONE 'Asia/Dhaka')::date = gs::date \
            AND so.status <> 'cancelled' \
          GROUP BY gs \
          ORDER BY gs",
    )
    .bind(days)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("admin analytics daily series", e))?;

    let courier_breakdown = courier_breakdown(pool, None, days).await?;

    Ok(AdminAnalytics {
        days,
        totals: AdminAnalyticsTotals {
            gmv_poisha: totals.gmv_poisha,
            orders: totals.orders,
            sub_orders: totals.sub_orders,
            commission_poisha: totals.commission_poisha,
            new_users: totals.new_users,
            new_stores: totals.new_stores,
            payouts: AdminPayoutTotals {
                requested_count: totals.payouts_requested_count,
                requested_poisha: totals.payouts_requested_poisha,
                paid_poisha: totals.payouts_paid_poisha,
            },
            charges_poisha: totals.charges_poisha,
            coupons: AdminCouponTotals {
                redemptions: totals.coupon_redemptions,
                discount_poisha: totals.coupon_discount_poisha,
            },
        },
        top_stores,
        courier_breakdown,
        daily: daily_rows
            .into_iter()
            .map(|row| AdminDailyRow {
                date: date_string(row.day),
                orders: row.orders,
                gmv_poisha: row.gmv_poisha,
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AnalyticsQuery {
    pub days: Option<i64>,
}

// ---------------------------------------------------------------------------
// Handlers (thin)
// ---------------------------------------------------------------------------

pub async fn seller_analytics_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AnalyticsQuery>,
) -> Result<Json<SellerAnalytics>, ApiError> {
    let pool = state.require_pool()?;
    let days = analytics_days(query.days);
    Ok(Json(seller_analytics(pool, &user, days).await?))
}

pub async fn seller_analytics_csv_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AnalyticsQuery>,
) -> Result<(HeaderMap, String), ApiError> {
    let pool = state.require_pool()?;
    let days = analytics_days(query.days);
    let csv = seller_analytics_csv(pool, &user, days).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"seller-analytics-{days}d.csv\""))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    Ok((headers, csv))
}

pub async fn admin_analytics_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AnalyticsQuery>,
) -> Result<Json<AdminAnalytics>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    let days = analytics_days(query.days);
    Ok(Json(admin_analytics(pool, days).await?))
}
