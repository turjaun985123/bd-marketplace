//! Admin payouts remake, part I: the migration 0072
//! backfill logic, pinned against seeded legacy rows.
//!
//! The standard test harness runs migrations against an
//! EMPTY database, so a backfill UPDATE is never exercised
//! by "migrations run" alone. This test seeds the legacy
//! shape by hand — payouts decided before the remake
//! (per-stage stamps NULL, `processed_at` holding the last
//! transition) plus their Wave 28 audit rows — and then
//! executes the migration's backfill statements VERBATIM.
//! If the migration's SQL changes, this copy must change
//! with it; that duplication is the point of the pin.
//!
//! The asymmetric case is the one that matters: a payout
//! approved BEFORE Wave 28 (no audit row) and paid after
//! it (audit row exists) must end with `paid_at` +
//! `paid_by` set from the audit row, and BOTH approval
//! fields NULL — step A stamps `approved_at` only for rows
//! still `approved`, and the approver is never guessed
//! from the payer.
//!
//! Gated on `DATABASE_URL`, like every integration test
//! here. No HTTP: the backfill is pure SQL.

use api::run_migrations;
use sqlx::PgPool;
use uuid::Uuid;

/// The backfill statements, copied verbatim from
/// migrations/0072_payout_decision_metadata.sql.
const BACKFILL: &[&str] = &[
    "UPDATE payouts SET approved_at = processed_at WHERE status = 'approved'",
    "UPDATE payouts SET rejected_at = processed_at WHERE status = 'rejected'",
    "UPDATE payouts SET paid_at = processed_at WHERE status = 'paid'",
    "UPDATE payouts p
     SET approved_by_user_id = a.admin_user_id,
         approved_at = a.created_at
     FROM (SELECT DISTINCT ON (target_id) target_id, admin_user_id, created_at
           FROM admin_audit_log
           WHERE target_type = 'payout' AND action = 'payout.approve'
           ORDER BY target_id, created_at ASC) a
     WHERE a.target_id = p.id::text AND p.status IN ('approved', 'paid')",
    "UPDATE payouts p
     SET rejected_by_user_id = a.admin_user_id,
         rejected_at = a.created_at
     FROM (SELECT DISTINCT ON (target_id) target_id, admin_user_id, created_at
           FROM admin_audit_log
           WHERE target_type = 'payout' AND action = 'payout.reject'
           ORDER BY target_id, created_at ASC) a
     WHERE a.target_id = p.id::text AND p.status = 'rejected'",
    "UPDATE payouts p
     SET paid_by_user_id = a.admin_user_id,
         paid_at = a.created_at
     FROM (SELECT DISTINCT ON (target_id) target_id, admin_user_id, created_at
           FROM admin_audit_log
           WHERE target_type = 'payout' AND action = 'payout.mark_paid'
           ORDER BY target_id, created_at ASC) a
     WHERE a.target_id = p.id::text AND p.status = 'paid'",
];

#[tokio::test]
async fn admin_payouts_backfill_logic() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => {
            eprintln!("DATABASE_URL not set — skipping admin payouts remake test I");
            return;
        }
    };

    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to test database");
    run_migrations(&pool).await.expect("migrations run");

    sqlx::query(
        "TRUNCATE notifications, admin_audit_log, otp_request_log, \
         promotions, seller_charges, product_questions, wishlist_items, \
         stock_adjustments, invoices, store_invoice_counters, store_verifications, \
         return_requests, sidra_suggestions, messages, conversations, \
         coupon_redemptions, coupons, platform_settings, payouts, payout_accounts, \
         review_replies, reviews, shipment_events, shipments, payment_events, \
         payments, order_items, sub_orders, orders, cart_items, carts, products, \
         stores, otp_codes, users CASCADE",
    )
    .execute(&pool)
    .await
    .expect("truncate tables");

    // Two admins and one store owner.
    let admin_a = Uuid::new_v4();
    let admin_b = Uuid::new_v4();
    let owner = Uuid::new_v4();
    for (id, phone) in [
        (admin_a, "+8801710000011"),
        (admin_b, "+8801710000012"),
        (owner, "+8801710000013"),
    ] {
        sqlx::query("INSERT INTO users (id, phone) VALUES ($1, $2)")
            .bind(id)
            .bind(phone)
            .execute(&pool)
            .await
            .expect("insert user");
    }
    let store = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO stores (id, owner_user_id, name, slug) VALUES ($1, $2, 'Backfill Store', 'backfill-store')",
    )
    .bind(store)
    .bind(owner)
    .execute(&pool)
    .await
    .expect("insert store");

    // Legacy payouts: decided rows carry ONLY processed_at.
    // (approved, processed 2026-01-10), (paid, processed
    // 2026-02-01), (rejected, processed 2026-01-20), and a
    // still-requested row with no processed_at at all.
    let approved = Uuid::new_v4();
    let paid = Uuid::new_v4();
    let rejected = Uuid::new_v4();
    let requested = Uuid::new_v4();
    for (id, status, processed) in [
        (approved, "approved", Some("2026-01-10T10:00:00Z")),
        (paid, "paid", Some("2026-02-01T10:00:00Z")),
        (rejected, "rejected", Some("2026-01-20T10:00:00Z")),
        (requested, "requested", None),
    ] {
        sqlx::query(
            "INSERT INTO payouts (id, store_id, amount_poisha, method, account_name, \
             account_number, status, processed_at) \
             VALUES ($1, $2, 60000, 'bkash', 'Legacy Seller', '01710000013', $3, \
                     $4::timestamptz)",
        )
        .bind(id)
        .bind(store)
        .bind(status)
        .bind(processed)
        .execute(&pool)
        .await
        .expect("insert legacy payout");
    }

    // Audit rows: the approved payout was approved by A at
    // its processed_at; the paid payout was approved by A
    // on 2026-01-15 (BEFORE its processed_at — the pivot
    // must find THIS row for approved_at, not the mark-paid
    // row) and paid by B at its processed_at; the rejected
    // payout was rejected by B.
    for (admin, action, target, at) in [
        (admin_a, "payout.approve", approved, "2026-01-10T10:00:00Z"),
        (admin_a, "payout.approve", paid, "2026-01-15T09:00:00Z"),
        (admin_b, "payout.mark_paid", paid, "2026-02-01T10:00:00Z"),
        (admin_b, "payout.reject", rejected, "2026-01-20T10:00:00Z"),
    ] {
        sqlx::query(
            "INSERT INTO admin_audit_log (admin_user_id, action, target_type, target_id, created_at) \
             VALUES ($1, $2, 'payout', $3, $4::timestamptz)",
        )
        .bind(admin)
        .bind(action)
        .bind(target.to_string())
        .bind(at)
        .execute(&pool)
        .await
        .expect("insert audit row");
    }
    // The asymmetric case: a payout approved pre-W28 (no
    // audit row for the approval) and paid post-W28. Its
    // processed_at is the PAYMENT time (2026-03-01); the
    // approval moment is simply not in the data, so both
    // approval fields must stay NULL.
    let asymmetric = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO payouts (id, store_id, amount_poisha, method, account_name, \
         account_number, status, processed_at) \
         VALUES ($1, $2, 60000, 'bkash', 'Legacy Seller', '01710000013', 'paid', \
                 '2026-03-01T10:00:00Z'::timestamptz)",
    )
    .bind(asymmetric)
    .bind(store)
    .execute(&pool)
    .await
    .expect("insert asymmetric payout");
    sqlx::query(
        "INSERT INTO admin_audit_log (admin_user_id, action, target_type, target_id, created_at) \
         VALUES ($1, 'payout.mark_paid', 'payout', $2, '2026-03-01T10:00:00Z'::timestamptz)",
    )
    .bind(admin_b)
    .bind(asymmetric.to_string())
    .execute(&pool)
    .await
    .expect("insert asymmetric audit row");

    for statement in BACKFILL {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .expect("run backfill statement");
    }

    #[derive(sqlx::FromRow)]
    struct StampRow {
        approved_at: Option<chrono::DateTime<chrono::Utc>>,
        approved_by_user_id: Option<Uuid>,
        paid_at: Option<chrono::DateTime<chrono::Utc>>,
        paid_by_user_id: Option<Uuid>,
        rejected_at: Option<chrono::DateTime<chrono::Utc>>,
        rejected_by_user_id: Option<Uuid>,
    }
    async fn stamps(pool: &PgPool, id: Uuid) -> StampRow {
        sqlx::query_as(
            "SELECT approved_at, approved_by_user_id, paid_at, paid_by_user_id, \
             rejected_at, rejected_by_user_id FROM payouts WHERE id = $1",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("load payout stamps")
    }
    fn at(text: &str) -> chrono::DateTime<chrono::Utc> {
        text.parse().expect("timestamp parses")
    }

    let row = stamps(&pool, approved).await;
    assert_eq!(row.approved_at, Some(at("2026-01-10T10:00:00Z")), "approved_at");
    assert_eq!(row.approved_by_user_id, Some(admin_a), "approved_by");
    assert_eq!(row.paid_at, None);
    assert_eq!(row.paid_by_user_id, None);
    assert_eq!(row.rejected_at, None);
    assert_eq!(row.rejected_by_user_id, None);

    // The pivot case: approved_at comes from the APPROVE
    // audit row (2026-01-15), NOT from processed_at and NOT
    // from the mark-paid row.
    let row = stamps(&pool, paid).await;
    assert_eq!(row.approved_at, Some(at("2026-01-15T09:00:00Z")), "paid row approved_at");
    assert_eq!(row.approved_by_user_id, Some(admin_a), "paid row approved_by");
    assert_eq!(row.paid_at, Some(at("2026-02-01T10:00:00Z")), "paid_at");
    assert_eq!(row.paid_by_user_id, Some(admin_b), "paid_by");

    let row = stamps(&pool, rejected).await;
    assert_eq!(row.rejected_at, Some(at("2026-01-20T10:00:00Z")), "rejected_at");
    assert_eq!(row.rejected_by_user_id, Some(admin_b), "rejected_by");

    let row = stamps(&pool, requested).await;
    assert_eq!(row.approved_at, None);
    assert_eq!(row.approved_by_user_id, None);
    assert_eq!(row.paid_at, None);
    assert_eq!(row.paid_by_user_id, None);
    assert_eq!(row.rejected_at, None);
    assert_eq!(row.rejected_by_user_id, None);

    // Asymmetric: paid stamps exact from the audit row;
    // the approval is honestly absent — never inferred
    // from the payment.
    let row = stamps(&pool, asymmetric).await;
    assert_eq!(row.paid_at, Some(at("2026-03-01T10:00:00Z")), "asym paid_at");
    assert_eq!(row.paid_by_user_id, Some(admin_b), "asym paid_by");
    assert_eq!(row.approved_at, None, "no audit row, no approval stamp");
    assert_eq!(row.approved_by_user_id, None, "the approver is never guessed");
}
