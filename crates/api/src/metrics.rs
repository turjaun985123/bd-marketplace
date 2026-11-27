//! Admin metrics snapshot (Wave 43): one point-in-time
//! answer to "is the platform healthy right now?", behind
//! the one `require_admin` guard (and, like every
//! `/v1/admin/*` route, unreachable for API keys — the
//! Wave 37 route exclusions refuse key authentication on
//! the whole admin prefix before scope checks run).
//!
//! Every block is ONE aggregate query — three queries
//! total, no per-row loops, nothing cached: the numbers
//! are recomputed from the ledger tables on each request,
//! exactly like the Wave 27 analytics they reuse
//! definitions from. The blocks:
//!
//! - `orders_today`: parent orders created on the current
//!   Asia/Dhaka day (the Wave 27 day-boundary convention:
//!   `(created_at AT TIME ZONE 'Asia/Dhaka')::date` against
//!   today's Dhaka date), plus the day's GMV — the Wave 27
//!   admin formula, SUM(goods − discount + delivery) over
//!   the day's NON-CANCELLED sub-orders.
//! - `orders_by_status`: parent-order counts grouped by
//!   the `orders.status` vocabulary (`placed` / `confirmed`
//!   / `delivered` / `cancelled`); only statuses present in
//!   the data appear.
//! - `pending_payouts`: the payout queue an admin still
//!   has to decide — status `requested`, count and total
//!   poisha. This is the Wave 10 overview's definition of
//!   pending (an `approved` payout is already decided; it
//!   is money in flight, not a decision waiting).
//! - `open_returns`: return requests in `requested` or
//!   `approved`. The returns lifecycle's own vocabulary
//!   (see the domain transition table): `rejected` and
//!   `received` are terminal FOR THE REQUEST — a received
//!   return's refund continues on the row's separate
//!   `refund_status` field and is not an open request.
//! - `notification_backlog`: the Wave 29 outbox counted
//!   per status (`pending` / `sending` / `sent` / `failed`).
//!   A `sending` count that never drains is the stuck-batch
//!   signal documented since Wave 29: a worker that dies
//!   mid-batch leaves its claimed rows in `sending`.
//! - `stale_shipments`: non-manual shipments whose status
//!   is not terminal ([`POLL_TERMINAL_STATUSES`], reused —
//!   not re-listed) and whose `updated_at` is older than
//!   [`STALE_SHIPMENT_MINUTES`] — the same population the
//!   Wave 30 poller picks up, i.e. work the poller owes.
//! - `users_total` / `stores_total` / `products_active`:
//!   plain catalogue totals (active = `status = 'active'`).

use std::collections::BTreeMap;

use axum::{extract::State, Json};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::admin::require_admin;
use crate::auth::AuthUser;
use crate::shipments::POLL_TERMINAL_STATUSES;
use crate::{ApiError, AppState};

/// How old a non-terminal API-courier shipment's stored
/// status may get before the snapshot calls it stale
/// (Wave 43): the same 15 minutes the Wave 30 poller uses
/// by default (`SHIPMENT_POLL_STALE_MINUTES` in the
/// worker). A shipment stale by this measure is one the
/// poller would refresh on its next cycle.
pub const STALE_SHIPMENT_MINUTES: i64 = 15;

/// Parent orders created today (Asia/Dhaka) and the day's
/// GMV in poisha (Wave 27 formula; see the module doc).
#[derive(Debug, Serialize)]
pub struct OrdersToday {
    pub count: i64,
    pub gmv_poisha: i64,
}

/// Payout requests awaiting an admin decision (status
/// `requested`): how many, and how much poisha they total.
#[derive(Debug, Serialize)]
pub struct PendingPayouts {
    pub count: i64,
    pub amount_poisha: i64,
}

/// The notification outbox counted per status. All four
/// statuses are always present (zero when the outbox holds
/// none) so a dashboard never has to guess at a missing key.
#[derive(Debug, Serialize)]
pub struct NotificationBacklog {
    pub pending: i64,
    pub sending: i64,
    pub sent: i64,
    pub failed: i64,
}

/// The media library's footprint (Wave 51): how many
/// product images exist, how many are stored uploads vs
/// external hotlinks, and the total bytes of the original
/// uploads (external images carry no byte count — the
/// platform never held their bytes). The operator's
/// storage-growth signal for the media store.
#[derive(Debug, Serialize)]
pub struct MediaUsage {
    pub images_total: i64,
    pub stored_images: i64,
    pub external_images: i64,
    pub stored_bytes: i64,
    /// The five stores holding the most stored bytes
    /// (Wave 51 extension) — where a storage bill would
    /// come from, named.
    pub top_stores: Vec<StoreMediaUsage>,
}

/// One store's media footprint (Wave 51 extension).
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct StoreMediaUsage {
    pub store_id: Uuid,
    pub store_slug: String,
    pub store_name: String,
    pub images: i64,
    pub stored_bytes: i64,
}

/// The whole snapshot (Wave 43). Every field is defined in
/// the module doc; nothing here is a time series — each
/// value is the database's state at the instant of the call.
#[derive(Debug, Serialize)]
pub struct AdminMetrics {
    pub orders_today: OrdersToday,
    pub orders_by_status: BTreeMap<String, i64>,
    pub pending_payouts: PendingPayouts,
    pub open_returns: i64,
    pub notification_backlog: NotificationBacklog,
    pub stale_shipments: i64,
    pub users_total: i64,
    pub stores_total: i64,
    pub products_active: i64,
    /// Wave 51: the media library's footprint (see
    /// [`MediaUsage`]).
    pub media: MediaUsage,
    /// Wave 52: merchandising engagement — banner clicks and
    /// collection views, all-time and today (see
    /// [`crate::merch::MerchandisingStats`]).
    pub merchandising: crate::merch::MerchandisingStats,
}

/// One (status, count) row, shared by the two GROUP BY
/// queries below.
#[derive(FromRow)]
struct StatusCount {
    status: String,
    count: i64,
}

/// Compute the snapshot (Wave 43): one scalar aggregate
/// query for the single-number blocks, one GROUP BY for
/// orders by status, one GROUP BY for the outbox. The
/// caller is authorised by the handler through the one
/// `require_admin` guard before this runs.
pub async fn get_admin_metrics(pool: &PgPool) -> Result<AdminMetrics, ApiError> {
    // The terminal-status list is interpolated from the
    // POLL_TERMINAL_STATUSES constant itself (compile-time
    // code values, never user input) so this query can
    // never drift from the poller's own definition.
    let terminal_statuses = POLL_TERMINAL_STATUSES
        .iter()
        .map(|status| format!("'{status}'"))
        .collect::<Vec<_>>()
        .join(", ");

    #[derive(FromRow)]
    struct ScalarRow {
        orders_today: i64,
        orders_today_gmv_poisha: i64,
        pending_payouts_count: i64,
        pending_payouts_amount_poisha: i64,
        open_returns: i64,
        stale_shipments: i64,
        users_total: i64,
        stores_total: i64,
        products_active: i64,
        media_images_total: i64,
        media_stored_images: i64,
        media_external_images: i64,
        media_stored_bytes: i64,
    }

    let scalars: ScalarRow = sqlx::query_as(&format!(
        "SELECT \
           (SELECT COUNT(*) FROM orders \
             WHERE (created_at AT TIME ZONE 'Asia/Dhaka')::date \
                 = (now() AT TIME ZONE 'Asia/Dhaka')::date) \
             AS orders_today, \
           (SELECT COALESCE(SUM(goods_subtotal_poisha - discount_poisha \
                                + delivery_charge_poisha), 0)::bigint \
              FROM sub_orders \
             WHERE status <> 'cancelled' \
               AND (created_at AT TIME ZONE 'Asia/Dhaka')::date \
                 = (now() AT TIME ZONE 'Asia/Dhaka')::date) \
             AS orders_today_gmv_poisha, \
           (SELECT COUNT(*) FROM payouts WHERE status = 'requested') \
             AS pending_payouts_count, \
           (SELECT COALESCE(SUM(amount_poisha), 0)::bigint \
              FROM payouts WHERE status = 'requested') \
             AS pending_payouts_amount_poisha, \
           (SELECT COUNT(*) FROM return_requests \
             WHERE status IN ('requested', 'approved')) \
             AS open_returns, \
           (SELECT COUNT(*) FROM shipments \
             WHERE courier <> 'manual' \
               AND status NOT IN ({terminal_statuses}) \
               AND updated_at < now() - make_interval(mins => $1::int)) \
             AS stale_shipments, \
           (SELECT COUNT(*) FROM users) AS users_total, \
           (SELECT COUNT(*) FROM stores) AS stores_total, \
           (SELECT COUNT(*) FROM products WHERE status = 'active') \
             AS products_active, \
           (SELECT COUNT(*) FROM product_images) AS media_images_total, \
           (SELECT COUNT(*) FROM product_images \
             WHERE storage_key IS NOT NULL) AS media_stored_images, \
           (SELECT COUNT(*) FROM product_images \
             WHERE external_url IS NOT NULL) AS media_external_images, \
           (SELECT COALESCE(SUM(size_bytes), 0)::bigint FROM product_images) \
             AS media_stored_bytes"
    ))
    .bind(STALE_SHIPMENT_MINUTES)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("admin metrics scalars", e))?;

    let order_rows: Vec<StatusCount> = sqlx::query_as(
        "SELECT status, COUNT(*) AS count FROM orders \
         GROUP BY status ORDER BY status",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("admin metrics orders by status", e))?;
    let orders_by_status: BTreeMap<String, i64> = order_rows
        .into_iter()
        .map(|row| (row.status, row.count))
        .collect();

    let notification_rows: Vec<StatusCount> = sqlx::query_as(
        "SELECT status, COUNT(*) AS count FROM notifications GROUP BY status",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("admin metrics notification backlog", e))?;
    let mut notification_backlog = NotificationBacklog {
        pending: 0,
        sending: 0,
        sent: 0,
        failed: 0,
    };
    for row in notification_rows {
        match row.status.as_str() {
            "pending" => notification_backlog.pending = row.count,
            "sending" => notification_backlog.sending = row.count,
            "sent" => notification_backlog.sent = row.count,
            "failed" => notification_backlog.failed = row.count,
            // A status outside the outbox vocabulary is not
            // silently folded into a known bucket; the four
            // documented buckets are the contract.
            _ => {}
        }
    }

    Ok(AdminMetrics {
        orders_today: OrdersToday {
            count: scalars.orders_today,
            gmv_poisha: scalars.orders_today_gmv_poisha,
        },
        orders_by_status,
        pending_payouts: PendingPayouts {
            count: scalars.pending_payouts_count,
            amount_poisha: scalars.pending_payouts_amount_poisha,
        },
        open_returns: scalars.open_returns,
        notification_backlog,
        stale_shipments: scalars.stale_shipments,
        users_total: scalars.users_total,
        stores_total: scalars.stores_total,
        products_active: scalars.products_active,
        media: MediaUsage {
            images_total: scalars.media_images_total,
            stored_images: scalars.media_stored_images,
            external_images: scalars.media_external_images,
            stored_bytes: scalars.media_stored_bytes,
            top_stores: sqlx::query_as(
                "SELECT s.id AS store_id, s.slug AS store_slug, s.name AS store_name, \
                 COUNT(*)::bigint AS images, \
                 COALESCE(SUM(pi.size_bytes), 0)::bigint AS stored_bytes \
                 FROM product_images pi \
                 JOIN products p ON p.id = pi.product_id \
                 JOIN stores s ON s.id = p.store_id \
                 WHERE pi.storage_key IS NOT NULL \
                 GROUP BY s.id, s.slug, s.name \
                 ORDER BY stored_bytes DESC LIMIT 5",
            )
            .fetch_all(pool)
            .await
            .map_err(|e| ApiError::internal("media top stores", e))?,
        },
        merchandising: crate::merch::merchandising_stats(pool).await?,
    })
}

/// GET /v1/admin/metrics (Wave 43): the observability
/// snapshot. Admin only, via the one `require_admin` guard.
pub async fn admin_metrics_handler(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<AdminMetrics>, ApiError> {
    require_admin(&state, &user).await?;
    let pool = state.require_pool()?;
    Ok(Json(get_admin_metrics(pool).await?))
}
