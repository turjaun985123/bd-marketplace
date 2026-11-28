//! Notification enqueue helpers (Wave 29): the API-side half
//! of the outbox. Each helper loads the recipient and the
//! template variables for one event INSIDE the caller's
//! transaction and queues the row through the ONE writer,
//! `notify::enqueue` — so the notification commits or rolls
//! back together with the state change it announces.
//!
//! The funnel → kind map (each event is enqueued at exactly
//! one place; see the README Wave 29 section):
//!
//! - `order_placed` — `orders::place_order`, once per order.
//! - `sub_order_shipped` — whichever single path performs a
//!   sub-order's confirmed → shipped move: the seller status
//!   PATCH (`orders::update_sub_order_status`) or a shipment
//!   booking (`shipments::create_shipment_for_seller` /
//!   `create_manual_shipment_for_seller`). The moves are
//!   mutually exclusive — a shipped sub-order cannot be
//!   booked, a booked one cannot be PATCHed to shipped — so
//!   exactly one fires per sub-order.
//! - `sub_order_delivered` — the seller PATCH, or the shared
//!   shipment sync (`shipments::sync_sub_order_with_shipment_status`)
//!   when a courier/manual status delivers the parcel. A
//!   sub-order reaches `delivered` exactly once (the domain
//!   transition table has no move out of it), so the two
//!   paths can never both fire.
//! - `sub_order_cancelled` — the seller PATCH (the only
//!   seller-driven cancel of a single sub-order).
//! - `sub_order_returned` — the shared shipment sync (RTO:
//!   courier-reported or seller-entered return).
//! - `payout_paid` — `admin::transition_payout` on the move
//!   to `paid`, to the seller.
//! - `payout_approved` / `payout_rejected` (admin payouts
//!   remake) — the same transition function on the moves to
//!   `approved` / `rejected`, to the seller. All three
//!   payout kinds share the one funnel
//!   ([`enqueue_payout_notice`]); a rejected notice carries
//!   the admin's reason, a paid notice the recorded payment
//!   reference.
//! - `verification_approved` / `verification_rejected`
//!   (admin verifications remake) —
//!   `verification::approve_verification` /
//!   `reject_verification`, to the store owner. Both kinds
//!   share the one funnel ([`enqueue_verification_notice`]);
//!   an approved notice carries the granted tier, a rejected
//!   notice the reviewer's note. The wording claims manual
//!   platform review only — no government NID check exists.
//! - `store_suspended` / `store_reactivated` (admin stores
//!   remake) — `admin::set_store_status`, to the store
//!   owner, inside the transition's own transaction. Both
//!   kinds share the one funnel
//!   ([`enqueue_store_status_notice`]); a suspended notice
//!   carries the admin's required note, a reactivated
//!   notice carries none. The wording says TEMPORARY —
//!   suspension deletes nothing and cancels no payout.
//!   The follower funnels above (`store_new_product`,
//!   `store_new_post`, `store_flash_sale`) enqueue NOTHING
//!   while their store is suspended: a dark store's
//!   publishes do not ping followers about pages they
//!   cannot open, and nothing retro-fires on reactivation.
//! - `product_taken_down` / `product_restored` (admin
//!   products remake) — `admin::set_product_takedown`, to
//!   the store owner, inside the transition's own
//!   transaction. Both kinds share the one funnel
//!   ([`enqueue_product_status_notice`]); a taken-down
//!   notice carries the admin's required reason, a
//!   restored notice carries none. The wording says
//!   TEMPORARY — a takedown deletes nothing and leaves
//!   the seller's own status untouched. The follower
//!   funnel above (`store_new_product`) also enqueues
//!   NOTHING for a taken-down product: followers must
//!   not be pinged about a listing the platform removed.
//! - `coupon_deactivated` / `coupon_reactivated` (admin
//!   coupons remake) — `coupons::set_admin_coupon_active`,
//!   to the store owner, inside the transition's own
//!   transaction. Both kinds share the one funnel
//!   ([`enqueue_coupon_status_notice`]); a deactivated
//!   notice carries the admin's required reason, a
//!   reactivated notice carries none. Store coupons only:
//!   a platform coupon has no seller to notify.
//! - `return_update` — `returns::decide_return_request`
//!   (approved / rejected) and `returns::mark_return_received`
//!   (received), to the buyer.
//! - `store_new_product` (Wave 54) — the three product
//!   publish funnels: `products::create_product_for_owner`
//!   (created already `active`), `products::
//!   update_product_for_owner` (a PATCH that moves a product
//!   INTO `active` from any other status) and the Facebook
//!   import (`facebook::import_posts`, whose rows are born
//!   `active`). One row per follower of the store, enqueued
//!   by [`enqueue_store_new_product_notices`] inside the
//!   same transaction as the publish itself. A product that
//!   was already active and stays active never re-fires:
//!   the update funnel compares the stored status before
//!   the write, so an edit to a live product is silent.
//!   The flash-sale sibling kind
//!   ([`enqueue_store_flash_sale_notices`]) fires from the
//!   admin item-set write when a product NEWLY joins a
//!   sale that is live at that moment.
//! - `store_new_post` (Wave 59) — a followed store's blog
//!   post becoming live: `store_content::create_post`
//!   (born published with a live moment) and
//!   `store_content::set_post_status` (a move INTO live
//!   from draft, archived, or a future schedule). One row
//!   per follower, enqueued by
//!   [`enqueue_store_new_post_notices`] inside the publish
//!   transaction. A post that was already live and stays
//!   live never re-fires, and a scheduled post fires
//!   nothing — when its moment passes there is no write
//!   to hook (liveness is read-time), which the Wave 59
//!   README section states plainly.

use serde_json::json;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use crate::ApiError;

/// Queue the notice for one sub-order event: the recipient
/// is the order's buyer, reached at the order's DELIVERY
/// phone (the number the buyer gave for this purchase —
/// not necessarily their account phone).
pub async fn enqueue_sub_order_notice(
    conn: &mut PgConnection,
    sub_order_id: Uuid,
    kind: &str,
) -> Result<(), ApiError> {
    #[derive(FromRow)]
    struct NoticeRow {
        buyer_user_id: Uuid,
        delivery_phone: String,
        order_id: Uuid,
        store_name: String,
    }
    let row: NoticeRow = sqlx::query_as(
        "SELECT o.buyer_user_id, o.delivery_phone, o.id AS order_id, \
         s.name AS store_name \
         FROM sub_orders so \
         JOIN orders o ON o.id = so.order_id \
         JOIN stores s ON s.id = so.store_id \
         WHERE so.id = $1",
    )
    .bind(sub_order_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load sub-order notice recipient", e))?;
    let payload = json!({
        "order_id": row.order_id.to_string(),
        "store_name": row.store_name,
        "sub_order_id": sub_order_id.to_string(),
    });
    notify::enqueue(
        &mut *conn,
        row.buyer_user_id,
        &row.delivery_phone,
        kind,
        &payload,
    )
    .await
    .map_err(|e| ApiError::internal("enqueue sub-order notification", e))?;
    Ok(())
}

/// Queue the `return_update` notice for a return request,
/// reading its CURRENT state — call this AFTER the decision
/// or receipt update, inside the same transaction, so the
/// body reports the status that actually committed. The
/// recipient is the buyer at their account phone (a return
/// is arranged with the account holder, and the returns
/// module already treats that number as the contact).
pub async fn enqueue_return_notice(
    conn: &mut PgConnection,
    return_id: Uuid,
) -> Result<(), ApiError> {
    #[derive(FromRow)]
    struct ReturnNoticeRow {
        buyer_id: Uuid,
        buyer_phone: String,
        order_id: Uuid,
        store_name: String,
        status: String,
        refund_poisha: Option<i64>,
    }
    let row: ReturnNoticeRow = sqlx::query_as(
        "SELECT rr.buyer_id, u.phone AS buyer_phone, rr.order_id, \
         s.name AS store_name, rr.status, rr.refund_poisha \
         FROM return_requests rr \
         JOIN users u ON u.id = rr.buyer_id \
         JOIN stores s ON s.id = rr.store_id \
         WHERE rr.id = $1",
    )
    .bind(return_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load return notice recipient", e))?;
    let payload = json!({
        "order_id": row.order_id.to_string(),
        "store_name": row.store_name,
        "status": row.status,
        "amount_poisha": row.refund_poisha,
    });
    notify::enqueue(
        &mut *conn,
        row.buyer_id,
        &row.buyer_phone,
        domain::NOTIFICATION_RETURN_UPDATE,
        &payload,
    )
    .await
    .map_err(|e| ApiError::internal("enqueue return notification", e))?;
    Ok(())
}

/// Queue the `store_new_product` notice (Wave 54) for
/// EVERY follower of the product's store: one outbox row
/// per follower, each at the follower's account phone,
/// inside the caller's transaction — so the notices exist
/// exactly when the publish commits, and vanish with it
/// when it rolls back. A store with no followers enqueues
/// nothing (the common case costs one indexed read).
/// Returns how many notices were queued, for the caller's
/// logs/tests.
pub async fn enqueue_store_new_product_notices(
    conn: &mut PgConnection,
    product_id: Uuid,
) -> Result<i64, ApiError> {
    #[derive(FromRow)]
    struct ProductNoticeRow {
        store_id: Uuid,
        store_name: String,
        store_status: String,
        product_title: String,
        product_taken_down: bool,
    }
    let product: ProductNoticeRow = sqlx::query_as(
        "SELECT p.store_id, s.name AS store_name, s.status AS store_status, \
         p.title AS product_title, \
         (p.admin_hidden_at IS NOT NULL) AS product_taken_down \
         FROM products p JOIN stores s ON s.id = p.store_id \
         WHERE p.id = $1",
    )
    .bind(product_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load new-product notice subject", e))?;

    // Admin stores remake: a SUSPENDED store's publishes
    // enqueue nothing. The seller may keep preparing the
    // catalogue while suspended (their management access
    // survives), but followers must not be pinged about
    // products they cannot open — the store's pages are
    // dark. Nothing retro-fires on reactivation either:
    // the funnel runs at publish time only.
    if product.store_status != "active" {
        return Ok(0);
    }

    // Admin products remake: a TAKEN-DOWN product's
    // publishes enqueue nothing either. The seller may
    // keep editing the listing — including flipping it
    // back to `active` while the platform flag stands —
    // but followers must not be pinged about a product
    // the platform removed and they cannot open. This
    // guard is the funnel-level backstop behind every
    // publish path (create, status flip, bulk import,
    // Facebook import): no caller can forget it.
    if product.product_taken_down {
        return Ok(0);
    }

    #[derive(FromRow)]
    struct FollowerRow {
        user_id: Uuid,
        phone: String,
    }
    // Wave 54 extension: a follow whose owner muted
    // new-product notices (notify_new_products = false)
    // is skipped — the follow itself (shelf, feed) is
    // unaffected; only this fan-out honours the flag.
    let followers: Vec<FollowerRow> = sqlx::query_as(
        "SELECT f.user_id, u.phone FROM store_follows f \
         JOIN users u ON u.id = f.user_id \
         WHERE f.store_id = $1 AND f.notify_new_products \
         ORDER BY f.created_at ASC",
    )
    .bind(product.store_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load store followers for notice", e))?;

    let payload = json!({
        "store_id": product.store_id.to_string(),
        "store_name": product.store_name,
        "product_id": product_id.to_string(),
        "product_title": product.product_title,
    });
    let mut queued: i64 = 0;
    for follower in &followers {
        notify::enqueue(
            &mut *conn,
            follower.user_id,
            &follower.phone,
            domain::NOTIFICATION_STORE_NEW_PRODUCT,
            &payload,
        )
        .await
        .map_err(|e| ApiError::internal("enqueue new-product notification", e))?;
        queued += 1;
    }
    Ok(queued)
}

/// Queue the `store_new_post` notice (Wave 59) for the
/// followers of a store whose blog post has JUST become
/// live. One row per follower; a follow whose owner
/// muted store news (`notify_new_products = false` —
/// the one mute covers products, sales, and posts) is
/// skipped. The caller (the content publish write)
/// decides the post is live NOW: a scheduled post does
/// not fire this, and nothing fires later when its
/// moment passes — liveness is computed at read time,
/// so there is no later transition to hook. Runs inside
/// the caller's transaction, like every funnel here.
/// Returns how many notices were queued.
pub async fn enqueue_store_new_post_notices(
    conn: &mut PgConnection,
    post_id: Uuid,
) -> Result<i64, ApiError> {
    #[derive(FromRow)]
    struct PostNoticeRow {
        store_id: Uuid,
        store_name: String,
        store_status: String,
        post_slug: String,
        post_title: String,
    }
    let post: PostNoticeRow = sqlx::query_as(
        "SELECT p.store_id, s.name AS store_name, s.status AS store_status, \
         p.slug AS post_slug, \
         p.title AS post_title \
         FROM store_posts p JOIN stores s ON s.id = p.store_id \
         WHERE p.id = $1",
    )
    .bind(post_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load new-post notice subject", e))?;

    // Admin stores remake: a SUSPENDED store's post
    // publishes enqueue nothing — the same dark-store rule
    // as the new-product funnel above.
    if post.store_status != "active" {
        return Ok(0);
    }

    #[derive(FromRow)]
    struct FollowerRow {
        user_id: Uuid,
        phone: String,
    }
    let followers: Vec<FollowerRow> = sqlx::query_as(
        "SELECT f.user_id, u.phone FROM store_follows f \
         JOIN users u ON u.id = f.user_id \
         WHERE f.store_id = $1 AND f.notify_new_products \
         ORDER BY f.created_at ASC",
    )
    .bind(post.store_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load store followers for post notice", e))?;

    let payload = json!({
        "store_id": post.store_id.to_string(),
        "store_name": post.store_name,
        "post_id": post_id.to_string(),
        "post_slug": post.post_slug,
        "post_title": post.post_title,
    });
    let mut queued: i64 = 0;
    for follower in &followers {
        notify::enqueue(
            &mut *conn,
            follower.user_id,
            &follower.phone,
            domain::NOTIFICATION_STORE_NEW_POST,
            &payload,
        )
        .await
        .map_err(|e| ApiError::internal("enqueue new-post notification", e))?;
        queued += 1;
    }
    Ok(queued)
}

/// Queue the `store_flash_sale` notice for the followers
/// of the stores behind a batch of products that have
/// JUST joined a live flash sale (Wave 54 extension). One
/// row per (follower, product): a follower of two stores
/// in the same sale hears about each store's product,
/// which is the same per-store honesty as the
/// new-product fan-out. Muted follows are skipped by the
/// same flag. The caller (the merch item-set write)
/// decides which products are new and whether the sale is
/// live; this funnel only fans out. Returns how many
/// notices were queued.
pub async fn enqueue_store_flash_sale_notices(
    conn: &mut PgConnection,
    sale_id: Uuid,
    sale_title: &str,
    product_ids: &[Uuid],
) -> Result<i64, ApiError> {
    if product_ids.is_empty() {
        return Ok(0);
    }
    #[derive(FromRow)]
    struct SaleNoticeRow {
        user_id: Uuid,
        phone: String,
        store_id: Uuid,
        store_name: String,
        product_id: Uuid,
        product_title: String,
    }
    let rows: Vec<SaleNoticeRow> = sqlx::query_as(
        "SELECT f.user_id, u.phone, s.id AS store_id, \
                s.name AS store_name, p.id AS product_id, \
                p.title AS product_title \
         FROM products p \
         JOIN stores s ON s.id = p.store_id \
         JOIN store_follows f ON f.store_id = s.id \
         JOIN users u ON u.id = f.user_id \
         WHERE p.id = ANY($1) AND f.notify_new_products \
           AND s.status = 'active' \
         ORDER BY f.created_at ASC",
    )
    .bind(product_ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load flash-sale notice recipients", e))?;

    let mut queued: i64 = 0;
    for row in &rows {
        let payload = json!({
            "store_id": row.store_id.to_string(),
            "store_name": row.store_name,
            "product_id": row.product_id.to_string(),
            "product_title": row.product_title,
            "sale_title": sale_title,
            "flash_sale_id": sale_id.to_string(),
        });
        notify::enqueue(
            &mut *conn,
            row.user_id,
            &row.phone,
            domain::NOTIFICATION_STORE_FLASH_SALE,
            &payload,
        )
        .await
        .map_err(|e| ApiError::internal("enqueue flash-sale notification", e))?;
        queued += 1;
    }
    Ok(queued)
}

/// Queue one payout-decision notice for a store's owner:
/// the recipient is the seller at their ACCOUNT phone (a
/// payout belongs to the account, unlike an order event,
/// which belongs to the delivery details). One funnel for
/// all three payout kinds (`payout_approved`,
/// `payout_rejected`, `payout_paid`) — the caller in
/// `admin::transition_payout` maps the target status to the
/// kind, so each decision is announced exactly once, inside
/// the transition's own transaction.
///
/// `note` (the rejection reason) and `payment_reference`
/// (the recorded proof of the send) enter the payload ONLY
/// when present: the paid payload for a reference-less call
/// stays byte-identical to the pre-remake
/// `{"amount_poisha": N}` shape.
pub async fn enqueue_payout_notice(
    conn: &mut PgConnection,
    store_id: Uuid,
    amount_poisha: i64,
    kind: &str,
    note: Option<&str>,
    payment_reference: Option<&str>,
) -> Result<(), ApiError> {
    let (owner_user_id, phone): (Uuid, String) = sqlx::query_as(
        "SELECT s.owner_user_id, u.phone FROM stores s \
         JOIN users u ON u.id = s.owner_user_id WHERE s.id = $1",
    )
    .bind(store_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load payout notice recipient", e))?;
    let mut payload = json!({ "amount_poisha": amount_poisha });
    if let Some(note) = note {
        payload["note"] = serde_json::Value::String(note.to_string());
    }
    if let Some(reference) = payment_reference {
        payload["payment_reference"] = serde_json::Value::String(reference.to_string());
    }
    notify::enqueue(&mut *conn, owner_user_id, &phone, kind, &payload)
        .await
        .map_err(|e| ApiError::internal("enqueue payout notification", e))?;
    Ok(())
}

/// Queue one verification-decision notice for a store's
/// owner (admin verifications remake): the recipient is the
/// seller at their ACCOUNT phone, exactly like the payout
/// funnel — a KYC decision belongs to the account. One
/// funnel for both verification kinds
/// (`verification_approved`, `verification_rejected`),
/// called from `verification::approve_verification` /
/// `reject_verification` inside the decision's own
/// transaction, so the notice commits or rolls back with
/// the decision it announces.
///
/// The tier is a caller-supplied PARAMETER, never re-read
/// from the store: the caller has just set it inside the
/// same transaction, and a re-read could observe the
/// pre-decision value depending on statement order. The
/// rejected kind ignores the tier (a rejected application
/// grants none) and carries the reviewer's note instead.
/// The payload NEVER carries the NID number or its last4 —
/// a notification row is not a place for identity data.
pub async fn enqueue_verification_notice(
    conn: &mut PgConnection,
    store_id: Uuid,
    kind: &str,
    verification_tier: Option<&str>,
    note: Option<&str>,
) -> Result<(), ApiError> {
    let (owner_user_id, phone): (Uuid, String) = sqlx::query_as(
        "SELECT s.owner_user_id, u.phone FROM stores s \
         JOIN users u ON u.id = s.owner_user_id WHERE s.id = $1",
    )
    .bind(store_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load verification notice recipient", e))?;
    let mut payload = json!({});
    if let Some(tier) = verification_tier {
        payload["verification_tier"] = serde_json::Value::String(tier.to_string());
    }
    if let Some(note) = note {
        payload["note"] = serde_json::Value::String(note.to_string());
    }
    notify::enqueue(&mut *conn, owner_user_id, &phone, kind, &payload)
        .await
        .map_err(|e| ApiError::internal("enqueue verification notification", e))?;
    Ok(())
}

/// Queue one store-status notice for a store's owner
/// (admin stores remake): the recipient is the seller at
/// their ACCOUNT phone, exactly like the payout and
/// verification funnels — a suspension is an account-level
/// event. One funnel for both store kinds
/// (`store_suspended`, `store_reactivated`), called from
/// `admin::set_store_status` inside the transition's own
/// transaction, so the notice commits or rolls back with
/// the status change it announces.
///
/// The store name is a caller-supplied PARAMETER, never
/// re-read from the store: the caller has just written the
/// transition inside the same transaction. The note (the
/// admin's suspension reason) rides ONLY on the suspended
/// kind — reactivation takes no note, and its payload
/// stays note-free.
pub async fn enqueue_store_status_notice(
    conn: &mut PgConnection,
    store_id: Uuid,
    store_name: &str,
    kind: &str,
    note: Option<&str>,
) -> Result<(), ApiError> {
    let (owner_user_id, phone): (Uuid, String) = sqlx::query_as(
        "SELECT s.owner_user_id, u.phone FROM stores s \
         JOIN users u ON u.id = s.owner_user_id WHERE s.id = $1",
    )
    .bind(store_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load store-status notice recipient", e))?;
    let mut payload = json!({
        "store_id": store_id.to_string(),
        "store_name": store_name,
    });
    if let Some(note) = note {
        payload["note"] = serde_json::Value::String(note.to_string());
    }
    notify::enqueue(&mut *conn, owner_user_id, &phone, kind, &payload)
        .await
        .map_err(|e| ApiError::internal("enqueue store-status notification", e))?;
    Ok(())
}

/// Enqueue the ONE seller notification for an admin
/// product takedown or restore (`product_taken_down`,
/// `product_restored`), called from
/// `admin::set_product_takedown` inside the transition's
/// own transaction, so the notice commits or rolls back
/// with the state change it announces.
///
/// The store name and product title are caller-supplied
/// PARAMETERS, never re-read: the caller has just locked
/// and written the product inside the same transaction,
/// and the reason is copied into the payload at takedown
/// time — a restore racing in later can never swap or
/// empty the reason this notice carries (the store
/// funnel's rule). The reason rides ONLY on the
/// taken-down kind — restore takes a note too, but that
/// note is an admin-to-admin record in the audit detail,
/// not broadcast to the seller.
pub async fn enqueue_product_status_notice(
    conn: &mut PgConnection,
    store_id: Uuid,
    store_name: &str,
    product_id: Uuid,
    product_title: &str,
    kind: &str,
    note: Option<&str>,
) -> Result<(), ApiError> {
    let (owner_user_id, phone): (Uuid, String) = sqlx::query_as(
        "SELECT s.owner_user_id, u.phone FROM stores s \
         JOIN users u ON u.id = s.owner_user_id WHERE s.id = $1",
    )
    .bind(store_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load product-status notice recipient", e))?;
    let mut payload = json!({
        "store_id": store_id.to_string(),
        "store_name": store_name,
        "product_id": product_id.to_string(),
        "product_title": product_title,
    });
    if let Some(note) = note {
        payload["note"] = serde_json::Value::String(note.to_string());
    }
    notify::enqueue(&mut *conn, owner_user_id, &phone, kind, &payload)
        .await
        .map_err(|e| ApiError::internal("enqueue product-status notification", e))?;
    Ok(())
}

/// Queue one coupon-status notice for a store's owner
/// (admin coupons remake): the recipient is the seller
/// at their ACCOUNT phone, exactly like the store and
/// product funnels — an admin flipping a store coupon is
/// an account-level event for that store. One funnel for
/// both coupon kinds (`coupon_deactivated`,
/// `coupon_reactivated`), called from the admin toggle
/// in `coupons` inside the transition's own transaction,
/// so the notice commits or rolls back with the state
/// change it announces. Platform coupons have no store
/// and no recipient — the caller invokes this funnel
/// only for store coupons.
///
/// The store name and coupon code are caller-supplied
/// PARAMETERS, never re-read from the database: the
/// caller has just locked and written the coupon inside
/// the same transaction. The reason (the admin's
/// deactivation reason) rides ONLY on the deactivated
/// kind — reactivation takes no reason, and its payload
/// stays reason-free (the store funnel's rule).
pub async fn enqueue_coupon_status_notice(
    conn: &mut PgConnection,
    store_id: Uuid,
    store_name: &str,
    coupon_code: &str,
    kind: &str,
    note: Option<&str>,
) -> Result<(), ApiError> {
    let (owner_user_id, phone): (Uuid, String) = sqlx::query_as(
        "SELECT s.owner_user_id, u.phone FROM stores s \
         JOIN users u ON u.id = s.owner_user_id WHERE s.id = $1",
    )
    .bind(store_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load coupon-status notice recipient", e))?;
    let mut payload = json!({
        "store_id": store_id.to_string(),
        "store_name": store_name,
        "coupon_code": coupon_code,
    });
    if let Some(note) = note {
        payload["note"] = serde_json::Value::String(note.to_string());
    }
    notify::enqueue(&mut *conn, owner_user_id, &phone, kind, &payload)
        .await
        .map_err(|e| ApiError::internal("enqueue coupon-status notification", e))?;
    Ok(())
}
