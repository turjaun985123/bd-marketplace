//! Admin metrics snapshot (Wave 43): one point-in-time
//! answer to "is the platform healthy right now?", behind
//! the one `require_admin` guard (and, like every
//! `/v1/admin/*` route, unreachable for API keys — the
//! Wave 37 route exclusions refuse key authentication on
//! the whole admin prefix before scope checks run).
//!
//! Every block is an aggregate query — no per-row loops,
//! nothing cached: the numbers are recomputed from the
//! ledger tables on each request, exactly like the
//! Wave 27 analytics they reuse definitions from. The
//! blocks:
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
//! - `orders_by_district` (Wave 55): parent orders grouped
//!   by their recorded delivery district. Only orders that
//!   NAMED a district are counted — a zone-only checkout
//!   (pre-Wave-55 shape, still the manual default) has no
//!   district to attribute, and inventing one from the
//!   zone would fabricate geography; the admin page says
//!   exactly this next to the block.
//! - `orders_by_zone` (Wave 55): the same grouping by the
//!   zone every order DOES carry — the complete picture
//!   next to the district block's partial one.
//! - `address_book` (Wave 55): the address book's size —
//!   saved addresses, buyers holding at least one, and
//!   how many books currently have a default. Adoption
//!   figures for the checkout shortcut, nothing more.
//! - `gift_cards` (Wave 60): gift-card counts by derived
//!   status plus the issued / outstanding / redeemed /
//!   restored totals — the stored-value liability at a
//!   glance.
//!
//! The overview remake added the operator blocks below.
//! Every one reuses an existing definition; none invents
//! a new money meaning:
//!
//! - `orders_yesterday`: `orders_today`'s two definitions
//!   shifted one Dhaka day back, so the page can show a
//!   today-vs-yesterday delta without a second endpoint.
//! - `sub_orders_by_status` / `orders_by_payment_status`:
//!   the fulfilment pipeline grouped by the sub-order
//!   status vocabulary, and parent orders grouped by
//!   `payment_status` — the axis `advance_pending` actually
//!   lives on (it is a payment status, never an order
//!   status). Sparse maps like `orders_by_status`: only
//!   values present in the data appear.
//! - `advance_pending`: orders whose advance is still
//!   owed (`payment_status = 'advance_pending'`, parent
//!   not cancelled) — how many, how much is still owed,
//!   how much order value is blocked behind them, and how
//!   many have waited over 24h / 72h. No reaper touches
//!   this state; the queue only moves when the buyer pays
//!   or an admin intervenes, so its age is the signal.
//! - `advance_unfulfilled`: the degraded population (the
//!   Wave 17 / CR-15 policy): an advance was required but
//!   the order proceeded on `pay_on_delivery` terms
//!   without it. A DIFFERENT set from `advance_pending` —
//!   these orders are already moving; the platform simply
//!   never collected the protection.
//! - `refunds_pending`: returns received whose refund the
//!   admin has not marked done (`status = 'received'`,
//!   `refund_status = 'pending'`) — the money half of the
//!   returns queue, invisible in `open_returns` because a
//!   received request is terminal for the request itself.
//! - `verifications_pending`: seller KYC applications
//!   awaiting review (`store_verifications.status =
//!   'pending'`) with the oldest application's age. NOT
//!   stores whose tier is `unverified` — that tier also
//!   covers stores that never applied.
//! - `stock_alerts`: active products whose EFFECTIVE
//!   stock (the variant-aware expression shared with the
//!   inventory overview) is zero, or at/under the low
//!   threshold — catalogue holes a buyer would hit.
//! - `growth_today` / `stores_suspended` /
//!   `active_promotions` / `live_flash_sale`: the day's
//!   new users and stores (Dhaka day), suspended stores,
//!   promotions live right now (the domain's
//!   `promotion_is_active` rule in SQL form), and the
//!   flash sale currently live, if any.
//! - Notification ages on `notification_backlog`: how old
//!   the oldest pending row and the oldest `sending`
//!   claim are, how many claims are stale by the notify
//!   crate's own threshold, and the last successful send.
//!   There is deliberately NO "failed in the last 24h":
//!   the outbox records no failure timestamp, so that
//!   figure cannot be computed honestly.

use std::collections::BTreeMap;

use axum::{extract::State, Json};
use chrono::{DateTime, Utc};
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
/// `requested`): how many, how much poisha they total, and
/// the age of the oldest undecided request (the queue is
/// the admin's to work; nothing auto-expires it).
#[derive(Debug, Serialize)]
pub struct PendingPayouts {
    pub count: i64,
    pub amount_poisha: i64,
    /// Seconds since the oldest `requested` payout was
    /// filed; `None` when the queue is empty.
    pub oldest_age_secs: Option<i64>,
}

/// Orders whose advance payment is still owed (overview
/// remake): the buyer has not paid the required advance,
/// so the order cannot be booked with a courier and its
/// stock stays locked. Ages are the operator's signal —
/// nothing reaps this state automatically.
#[derive(Debug, Serialize)]
pub struct AdvancePending {
    pub count: i64,
    /// Advance still owed: SUM(required − paid).
    pub outstanding_poisha: i64,
    /// Full order value blocked behind the unpaid
    /// advances: SUM(grand_total_poisha).
    pub blocked_total_poisha: i64,
    /// Waiting longer than 24 hours.
    pub over_24h: i64,
    /// Waiting longer than 72 hours.
    pub over_72h: i64,
    /// Seconds since the oldest such order was placed;
    /// `None` when there are none.
    pub oldest_age_secs: Option<i64>,
}

/// Orders that proceeded WITHOUT a required advance
/// (overview remake): the degraded population of the
/// Wave 17 advance policy — an advance was required but
/// no gateway could take it, so the order went ahead on
/// pay-on-delivery terms. Distinct from
/// [`AdvancePending`]: these orders are already moving.
#[derive(Debug, Serialize)]
pub struct AdvanceUnfulfilled {
    pub count: i64,
    /// The advance that was required and never collected:
    /// SUM(required − paid).
    pub required_poisha: i64,
}

/// Returns received whose refund is still pending an
/// admin mark-done (overview remake): `status =
/// 'received'` and `refund_status = 'pending'` on
/// `return_requests`.
#[derive(Debug, Serialize)]
pub struct RefundsPending {
    pub count: i64,
    /// SUM of the receipt-time refund snapshots.
    pub amount_poisha: i64,
}

/// Seller verification (KYC) applications awaiting review
/// (overview remake), with the oldest application's age.
#[derive(Debug, Serialize)]
pub struct VerificationsPending {
    pub count: i64,
    /// Seconds since the oldest pending application was
    /// submitted; `None` when the queue is empty.
    pub oldest_age_secs: Option<i64>,
}

/// Active products at or below their low-stock line
/// (overview remake), by the variant-aware effective
/// stock the inventory overview uses.
#[derive(Debug, Serialize)]
pub struct StockAlerts {
    pub out_of_stock: i64,
    pub low_stock: i64,
}

/// New users and stores created on the current Dhaka day
/// (overview remake).
#[derive(Debug, Serialize)]
pub struct GrowthToday {
    pub new_users: i64,
    pub new_stores: i64,
}

/// The flash sale live at the instant of the call, when
/// one is (overview remake): active flag set and `now`
/// inside its window — the same liveness the price
/// resolution uses.
#[derive(Debug, Serialize, FromRow)]
pub struct LiveFlashSale {
    pub id: Uuid,
    pub title: String,
    pub ends_at: DateTime<Utc>,
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
    /// Age in seconds of the oldest row still waiting to
    /// be claimed; `None` when nothing is pending.
    pub oldest_pending_age_secs: Option<i64>,
    /// Age in seconds of the oldest live `sending` claim
    /// (by `claimed_at`, the Wave 48 stamp); `None` when
    /// nothing is claimed. Unstamped legacy claims carry
    /// no `claimed_at` and cannot age — they are counted
    /// separately below instead of being aged by a
    /// fabricated stamp.
    pub oldest_sending_claim_age_secs: Option<i64>,
    /// `sending` rows whose claim is stale by the notify
    /// crate's own threshold (or never stamped): claimed
    /// work no live worker is finishing.
    pub stale_sending_count: i64,
    /// `sending` rows with no claim stamp at all.
    pub sending_unstamped_count: i64,
    /// Rows sent in the last 24 hours — the outbox's
    /// proof of life.
    pub sent_last_24h: i64,
    /// Seconds since the most recent successful send;
    /// `None` when nothing has ever been sent.
    pub last_sent_age_secs: Option<i64>,
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
    /// Wave 55: orders per delivery district (see
    /// [`DistrictOrderCount`]).
    pub orders_by_district: Vec<DistrictOrderCount>,
    /// Wave 55: orders per delivery zone — every order
    /// carries a zone, so this grouping is complete.
    pub orders_by_zone: Vec<ZoneOrderCount>,
    /// Wave 55: the address book's adoption figures.
    pub address_book: AddressBookStats,
    /// Wave 60: gift cards platform-wide — counts by
    /// derived status, issued / outstanding / redeemed /
    /// restored totals (see
    /// [`crate::gift_cards::GiftCardStats`]). The
    /// outstanding figure is the SELLERS' aggregate live
    /// liability on spendable cards: gift cards are
    /// seller-funded (issuance is promotional; redemption
    /// is netted from the issuing store's payout), so the
    /// platform itself holds no gift-card cash.
    pub gift_cards: crate::gift_cards::GiftCardStats,
    /// Overview remake: `orders_today`'s definitions one
    /// Dhaka day earlier — the delta baseline.
    pub orders_yesterday: OrdersToday,
    /// Overview remake: sub-order counts grouped by the
    /// fulfilment status vocabulary. Sparse like
    /// `orders_by_status`.
    pub sub_orders_by_status: BTreeMap<String, i64>,
    /// Overview remake: parent orders grouped by
    /// `payment_status` (the axis advances live on).
    /// Sparse like `orders_by_status`.
    pub orders_by_payment_status: BTreeMap<String, i64>,
    /// Overview remake: advances still owed by buyers.
    pub advance_pending: AdvancePending,
    /// Overview remake: advances required but never
    /// collected (degraded checkouts).
    pub advance_unfulfilled: AdvanceUnfulfilled,
    /// Overview remake: received returns awaiting a
    /// refund mark-done.
    pub refunds_pending: RefundsPending,
    /// Overview remake: KYC applications awaiting review.
    pub verifications_pending: VerificationsPending,
    /// Overview remake: low / out-of-stock active
    /// products by effective stock.
    pub stock_alerts: StockAlerts,
    /// Overview remake: today's new users and stores.
    pub growth_today: GrowthToday,
    /// Overview remake: stores currently suspended.
    pub stores_suspended: i64,
    /// Overview remake: promotions live right now.
    pub active_promotions: i64,
    /// Overview remake: the flash sale live right now,
    /// when one is.
    pub live_flash_sale: Option<LiveFlashSale>,
}

/// One district's order count (Wave 55). Districts are the
/// canonical English names from the domain district table
/// — that is what checkout stores — so the admin reads
/// geography in the same vocabulary the buyer picked.
#[derive(Debug, Serialize, FromRow)]
pub struct DistrictOrderCount {
    pub district: String,
    pub orders: i64,
}

/// One zone's order count (Wave 55).
#[derive(Debug, Serialize, FromRow)]
pub struct ZoneOrderCount {
    pub zone: String,
    pub orders: i64,
}

/// The address book's size (Wave 55): saved addresses in
/// total, buyers holding at least one, and rows currently
/// flagged default (one per book at most — the partial
/// unique index — so this also counts books WITH a
/// default).
#[derive(Debug, Serialize, FromRow)]
pub struct AddressBookStats {
    pub total_addresses: i64,
    pub buyers_with_addresses: i64,
    pub defaults_count: i64,
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
        orders_yesterday: i64,
        orders_yesterday_gmv_poisha: i64,
        pending_payouts_oldest_age_secs: Option<i64>,
        advance_pending_count: i64,
        advance_pending_outstanding_poisha: i64,
        advance_pending_blocked_total_poisha: i64,
        advance_pending_over_24h: i64,
        advance_pending_over_72h: i64,
        advance_pending_oldest_age_secs: Option<i64>,
        advance_unfulfilled_count: i64,
        advance_unfulfilled_required_poisha: i64,
        refunds_pending_count: i64,
        refunds_pending_amount_poisha: i64,
        verifications_pending_count: i64,
        verifications_oldest_age_secs: Option<i64>,
        stock_out_of_stock: i64,
        stock_low_stock: i64,
        new_users_today: i64,
        new_stores_today: i64,
        stores_suspended: i64,
        active_promotions: i64,
    }

    // The variant-aware effective-stock expression is
    // interpolated from the products module's canonical
    // text (compile-time constant over the `p` alias, never
    // user input) so the stock alerts can never drift from
    // the inventory overview's definition.
    let effective_stock = crate::products::EFFECTIVE_STOCK_SQL;

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
             AS media_stored_bytes, \
           (SELECT COUNT(*) FROM orders \
             WHERE (created_at AT TIME ZONE 'Asia/Dhaka')::date \
                 = (now() AT TIME ZONE 'Asia/Dhaka')::date - 1) \
             AS orders_yesterday, \
           (SELECT COALESCE(SUM(goods_subtotal_poisha - discount_poisha \
                                + delivery_charge_poisha), 0)::bigint \
              FROM sub_orders \
             WHERE status <> 'cancelled' \
               AND (created_at AT TIME ZONE 'Asia/Dhaka')::date \
                 = (now() AT TIME ZONE 'Asia/Dhaka')::date - 1) \
             AS orders_yesterday_gmv_poisha, \
           (SELECT EXTRACT(EPOCH FROM (now() - MIN(created_at)))::bigint \
              FROM payouts WHERE status = 'requested') \
             AS pending_payouts_oldest_age_secs, \
           (SELECT COUNT(*) FROM orders \
             WHERE payment_status = 'advance_pending' \
               AND status <> 'cancelled') \
             AS advance_pending_count, \
           (SELECT COALESCE(SUM(advance_required_poisha \
                                - advance_paid_poisha), 0)::bigint \
              FROM orders \
             WHERE payment_status = 'advance_pending' \
               AND status <> 'cancelled') \
             AS advance_pending_outstanding_poisha, \
           (SELECT COALESCE(SUM(grand_total_poisha), 0)::bigint \
              FROM orders \
             WHERE payment_status = 'advance_pending' \
               AND status <> 'cancelled') \
             AS advance_pending_blocked_total_poisha, \
           (SELECT COUNT(*) FROM orders \
             WHERE payment_status = 'advance_pending' \
               AND status <> 'cancelled' \
               AND created_at < now() - interval '24 hours') \
             AS advance_pending_over_24h, \
           (SELECT COUNT(*) FROM orders \
             WHERE payment_status = 'advance_pending' \
               AND status <> 'cancelled' \
               AND created_at < now() - interval '72 hours') \
             AS advance_pending_over_72h, \
           (SELECT EXTRACT(EPOCH FROM (now() - MIN(created_at)))::bigint \
              FROM orders \
             WHERE payment_status = 'advance_pending' \
               AND status <> 'cancelled') \
             AS advance_pending_oldest_age_secs, \
           (SELECT COUNT(*) FROM orders \
             WHERE payment_status = 'pay_on_delivery' \
               AND advance_required_poisha > advance_paid_poisha \
               AND status <> 'cancelled') \
             AS advance_unfulfilled_count, \
           (SELECT COALESCE(SUM(advance_required_poisha \
                                - advance_paid_poisha), 0)::bigint \
              FROM orders \
             WHERE payment_status = 'pay_on_delivery' \
               AND advance_required_poisha > advance_paid_poisha \
               AND status <> 'cancelled') \
             AS advance_unfulfilled_required_poisha, \
           (SELECT COUNT(*) FROM return_requests \
             WHERE status = 'received' AND refund_status = 'pending') \
             AS refunds_pending_count, \
           (SELECT COALESCE(SUM(refund_poisha), 0)::bigint \
              FROM return_requests \
             WHERE status = 'received' AND refund_status = 'pending') \
             AS refunds_pending_amount_poisha, \
           (SELECT COUNT(*) FROM store_verifications \
             WHERE status = 'pending') \
             AS verifications_pending_count, \
           (SELECT EXTRACT(EPOCH FROM (now() - MIN(submitted_at)))::bigint \
              FROM store_verifications WHERE status = 'pending') \
             AS verifications_oldest_age_secs, \
           (SELECT COUNT(*) FROM products p \
             WHERE p.status = 'active' AND {effective_stock} <= 0) \
             AS stock_out_of_stock, \
           (SELECT COUNT(*) FROM products p \
             WHERE p.status = 'active' \
               AND {effective_stock} BETWEEN 1 \
                   AND COALESCE(p.low_stock_threshold, 5)) \
             AS stock_low_stock, \
           (SELECT COUNT(*) FROM users \
             WHERE (created_at AT TIME ZONE 'Asia/Dhaka')::date \
                 = (now() AT TIME ZONE 'Asia/Dhaka')::date) \
             AS new_users_today, \
           (SELECT COUNT(*) FROM stores \
             WHERE (created_at AT TIME ZONE 'Asia/Dhaka')::date \
                 = (now() AT TIME ZONE 'Asia/Dhaka')::date) \
             AS new_stores_today, \
           (SELECT COUNT(*) FROM stores WHERE status = 'suspended') \
             AS stores_suspended, \
           (SELECT COUNT(*) FROM promotions \
             WHERE status = 'active' \
               AND starts_at <= now() AND ends_at > now()) \
             AS active_promotions"
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
        oldest_pending_age_secs: None,
        oldest_sending_claim_age_secs: None,
        stale_sending_count: 0,
        sending_unstamped_count: 0,
        sent_last_24h: 0,
        last_sent_age_secs: None,
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

    // The outbox's age detail (overview remake): one
    // aggregate over the same table. The stale threshold
    // is the notify crate's own constant, bound — never
    // re-stated here — so this snapshot cannot drift from
    // the reaper's definition. MIN/MAX ignore NULL stamps
    // by design: an unstamped claim cannot be aged, so it
    // is counted (sending_unstamped_count) instead.
    #[derive(FromRow)]
    struct NotificationDetail {
        oldest_pending_age_secs: Option<i64>,
        oldest_sending_claim_age_secs: Option<i64>,
        stale_sending_count: i64,
        sending_unstamped_count: i64,
        sent_last_24h: i64,
        last_sent_age_secs: Option<i64>,
    }
    let notification_detail: NotificationDetail = sqlx::query_as(
        "SELECT \
           EXTRACT(EPOCH FROM (now() - MIN(created_at) \
             FILTER (WHERE status = 'pending')))::bigint \
             AS oldest_pending_age_secs, \
           EXTRACT(EPOCH FROM (now() - MIN(claimed_at) \
             FILTER (WHERE status = 'sending')))::bigint \
             AS oldest_sending_claim_age_secs, \
           COUNT(*) FILTER (WHERE status = 'sending' \
             AND (claimed_at IS NULL \
                  OR claimed_at < now() - make_interval(mins => $1::int)))::bigint \
             AS stale_sending_count, \
           COUNT(*) FILTER (WHERE status = 'sending' \
             AND claimed_at IS NULL)::bigint \
             AS sending_unstamped_count, \
           COUNT(*) FILTER (WHERE status = 'sent' \
             AND sent_at >= now() - interval '24 hours')::bigint \
             AS sent_last_24h, \
           EXTRACT(EPOCH FROM (now() - MAX(sent_at)))::bigint \
             AS last_sent_age_secs \
         FROM notifications",
    )
    .bind(notify::DEFAULT_SENDING_STALE_MINUTES)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("admin metrics notification detail", e))?;
    notification_backlog.oldest_pending_age_secs = notification_detail.oldest_pending_age_secs;
    notification_backlog.oldest_sending_claim_age_secs =
        notification_detail.oldest_sending_claim_age_secs;
    notification_backlog.stale_sending_count = notification_detail.stale_sending_count;
    notification_backlog.sending_unstamped_count = notification_detail.sending_unstamped_count;
    notification_backlog.sent_last_24h = notification_detail.sent_last_24h;
    notification_backlog.last_sent_age_secs = notification_detail.last_sent_age_secs;

    let sub_order_rows: Vec<StatusCount> = sqlx::query_as(
        "SELECT status, COUNT(*) AS count FROM sub_orders \
         GROUP BY status ORDER BY status",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("admin metrics sub-orders by status", e))?;
    let sub_orders_by_status: BTreeMap<String, i64> = sub_order_rows
        .into_iter()
        .map(|row| (row.status, row.count))
        .collect();

    let payment_rows: Vec<StatusCount> = sqlx::query_as(
        "SELECT payment_status AS status, COUNT(*) AS count FROM orders \
         GROUP BY payment_status ORDER BY payment_status",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("admin metrics orders by payment status", e))?;
    let orders_by_payment_status: BTreeMap<String, i64> = payment_rows
        .into_iter()
        .map(|row| (row.status, row.count))
        .collect();

    Ok(AdminMetrics {
        orders_today: OrdersToday {
            count: scalars.orders_today,
            gmv_poisha: scalars.orders_today_gmv_poisha,
        },
        orders_by_status,
        pending_payouts: PendingPayouts {
            count: scalars.pending_payouts_count,
            amount_poisha: scalars.pending_payouts_amount_poisha,
            oldest_age_secs: scalars.pending_payouts_oldest_age_secs,
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
        orders_by_district: sqlx::query_as(
            "SELECT delivery_district AS district, COUNT(*)::bigint AS orders \
             FROM orders WHERE delivery_district IS NOT NULL \
             GROUP BY delivery_district ORDER BY orders DESC, delivery_district ASC",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("orders by district", e))?,
        orders_by_zone: sqlx::query_as(
            "SELECT delivery_zone AS zone, COUNT(*)::bigint AS orders \
             FROM orders GROUP BY delivery_zone ORDER BY orders DESC, delivery_zone ASC",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::internal("orders by zone", e))?,
        address_book: sqlx::query_as(
            "SELECT COUNT(*)::bigint AS total_addresses, \
             COUNT(DISTINCT user_id)::bigint AS buyers_with_addresses, \
             COUNT(*) FILTER (WHERE is_default)::bigint AS defaults_count \
             FROM buyer_addresses",
        )
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::internal("address book stats", e))?,
        gift_cards: crate::gift_cards::platform_gift_card_stats(pool).await?,
        orders_yesterday: OrdersToday {
            count: scalars.orders_yesterday,
            gmv_poisha: scalars.orders_yesterday_gmv_poisha,
        },
        sub_orders_by_status,
        orders_by_payment_status,
        advance_pending: AdvancePending {
            count: scalars.advance_pending_count,
            outstanding_poisha: scalars.advance_pending_outstanding_poisha,
            blocked_total_poisha: scalars.advance_pending_blocked_total_poisha,
            over_24h: scalars.advance_pending_over_24h,
            over_72h: scalars.advance_pending_over_72h,
            oldest_age_secs: scalars.advance_pending_oldest_age_secs,
        },
        advance_unfulfilled: AdvanceUnfulfilled {
            count: scalars.advance_unfulfilled_count,
            required_poisha: scalars.advance_unfulfilled_required_poisha,
        },
        refunds_pending: RefundsPending {
            count: scalars.refunds_pending_count,
            amount_poisha: scalars.refunds_pending_amount_poisha,
        },
        verifications_pending: VerificationsPending {
            count: scalars.verifications_pending_count,
            oldest_age_secs: scalars.verifications_oldest_age_secs,
        },
        stock_alerts: StockAlerts {
            out_of_stock: scalars.stock_out_of_stock,
            low_stock: scalars.stock_low_stock,
        },
        growth_today: GrowthToday {
            new_users: scalars.new_users_today,
            new_stores: scalars.new_stores_today,
        },
        stores_suspended: scalars.stores_suspended,
        active_promotions: scalars.active_promotions,
        live_flash_sale: sqlx::query_as(
            "SELECT id, title, ends_at FROM flash_sales \
             WHERE is_active AND starts_at <= now() AND ends_at > now() \
             ORDER BY ends_at ASC LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("admin metrics live flash sale", e))?,
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
