//! Gift cards (Wave 60): store-scoped stored value.
//!
//! A gift card is a BEARER instrument — the CODE is the
//! credential, the Wave 37 API-key shape: the full code
//! (`bdgc_` + 24 uppercase-alphanumeric characters) is
//! shown exactly once at issuance, and only the SHA-256
//! hash of its normalised form is stored. Lists and detail
//! views carry the display prefix + last4, never the code
//! and never the hash.
//!
//! The funding model, stated plainly because everything
//! else follows from it: issuance here is PROMOTIONAL /
//! MANUAL — a seller (or an admin) creates cards, no money
//! is collected, and there is no purchase flow in this
//! wave. Redemption is therefore SELLER-FUNDED: at
//! checkout it behaves exactly like a store coupon on that
//! store's sub-order — commission is computed on goods −
//! redemption and the seller payout snapshot nets it out
//! (the domain `build_order_totals` Wave 60 rules). The
//! platform never funds a redemption.
//!
//! Scope: STORE-scoped cards only. A card redeems only
//! against its own store's sub-order; a card whose store
//! is not in the cart is a bilingual 400 at checkout, never
//! a silent ignore. Platform-wide cards are a documented
//! future wave.
//!
//! Redemption ordering at checkout (pinned by the wave60
//! integration tests): the coupon allocates first, the
//! gift card applies to the target sub-order's POST-COUPON
//! goods, and loyalty runs last — loyalty's 50% cap base
//! is goods net of coupon AND gift card, and the COD
//! advance base is the grand total net of all three. The
//! courier's COD collection reads the sub-order's
//! `gift_card_discount_poisha` snapshot, so the courier
//! never collects the card-funded slice in cash.
//!
//! The ledger is append-only and every balance movement
//! writes its row in the SAME transaction as the cached
//! `balance_poisha` update — the Wave 35 loyalty shape.
//! The ledger sums to the cached balance at every step
//! (pinned by tests). Expiry is time-derived at read; no
//! status is ever stored. Disabling is a stored, reversible
//! fact (`disabled_at`).
//!
//! Restore rules (pinned by tests):
//!
//! - Cancelling an unpaid order (buyer whole-order cancel
//!   or seller sub-order cancel) restores the FULL
//!   redemption to the card, in the cancel transaction.
//! - A return received on the funded sub-order restores
//!   the proportional share
//!   (`domain::gift_card_restore_for_return`) in the
//!   return transaction, and the stored cash refund is
//!   reduced by the restored amount — the buyer recovers
//!   the rest as card balance, so the total recovery is
//!   unchanged and nobody is paid twice.
//! - Restoring NEVER extends a card's life: value restored
//!   to an expired card returns to the balance, but the
//!   expiry stands and the card stays unredeemable.
//!   Restoring to a disabled card likewise returns the
//!   balance; the freeze stands until someone re-enables.
//!
//! Access rules (deliberate): issuing and disable/enable
//! require the owner-level `manage_payouts` capability —
//! issuing a card spends the store's future earnings
//! exactly like requesting a payout, so managers (who can
//! see finances but not move money) cannot do it. Reading
//! the list, a card and its ledger requires
//! `view_finances` (owner + manager). Admin disable /
//! enable / adjust are audited in the same transaction.
//!
//! The public balance check is unauthenticated BY DESIGN
//! — checking a balance is what a bearer instrument is
//! for. Unknown or malformed codes get ONE uniform 404 so
//! the endpoint cannot be used to enumerate; a found code
//! reports its true state, because a holder checking their
//! own card deserves a true answer. The real protection is
//! the code's ≈124-bit entropy plus hash-only storage.
//!
//! The worker's expiry sweep
//! ([`sweep_gift_card_expiry_reminders`]) reminds the
//! ISSUING seller once per card when a balance-holding
//! card's expiry is a week or less away — the stamp and
//! the outbox row commit together, so the reminder is
//! exactly-once.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use domain::{
    gift_card_applicable_poisha, gift_card_display_prefix, gift_card_last4, gift_card_status,
    normalize_gift_card_code, validate_gift_card_adjust, validate_gift_card_bulk_count,
    validate_gift_card_issue, GIFT_CARD_CODE_PREFIX, GIFT_CARD_CODE_RANDOM_CHARS,
    GIFT_CARD_DEFAULT_VALIDITY_DAYS, GIFT_CARD_LEDGER_ADMIN_ADJUST, GIFT_CARD_LEDGER_CANCEL_RESTORE,
    GIFT_CARD_LEDGER_ISSUE, GIFT_CARD_LEDGER_REDEEM, GIFT_CARD_LEDGER_RETURN_RESTORE,
    GIFT_CARD_EXPIRY_REMINDER_WINDOW_DAYS, GIFT_CARD_MAX_VALIDITY_DAYS,
    GIFT_CARD_VALIDITY_SETTING_KEY, NOTIFICATION_GIFT_CARD_EXPIRING, STORE_ACTION_MANAGE_PAYOUTS,
    STORE_ACTION_VIEW_FINANCES,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::access::{require_role, resolve_scoped_store, seller_access};
use crate::admin::{platform_setting_i64, require_admin};
use crate::audit::record_admin_action;
use crate::auth::AuthUser;
use crate::products::clamp_pagination;
use crate::{ApiError, AppState};

// ---------------------------------------------------------------------
// Codes
// ---------------------------------------------------------------------

/// The code alphabet (Wave 60): uppercase letters + digits.
/// Lowercase is excluded so a code read aloud or typed on a
/// phone keyboard has exactly one canonical form.
const CODE_ALPHABET: &[u8; 36] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Generate one full gift-card code (Wave 60). The result
/// is shown to the issuer exactly once; it is never stored.
pub fn generate_gift_card_code() -> String {
    let mut rng = rand::thread_rng();
    let mut code = String::with_capacity(domain::GIFT_CARD_CODE_LENGTH);
    code.push_str(GIFT_CARD_CODE_PREFIX);
    for _ in 0..GIFT_CARD_CODE_RANDOM_CHARS {
        let idx = rng.gen_range(0..CODE_ALPHABET.len());
        code.push(CODE_ALPHABET[idx] as char);
    }
    code
}

/// Hash a NORMALISED code for storage / lookup (Wave 60):
/// SHA-256 hex, the Wave 37 `hash_api_key` construction.
pub fn hash_gift_card_code(normalised_code: &str) -> String {
    let digest = Sha256::digest(normalised_code.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------
// Rows and views
// ---------------------------------------------------------------------

/// One card row, with its store's display fields joined in.
#[derive(Debug, FromRow)]
struct GiftCardRow {
    id: Uuid,
    store_id: Uuid,
    store_slug: String,
    store_name: String,
    code_prefix: String,
    last4: String,
    initial_amount_poisha: i64,
    balance_poisha: i64,
    note: Option<String>,
    recipient_name: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    disabled_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

const CARD_COLUMNS: &str = "gc.id, gc.store_id, s.slug AS store_slug, \
    s.name AS store_name, gc.code_prefix, gc.last4, \
    gc.initial_amount_poisha, gc.balance_poisha, gc.note, \
    gc.recipient_name, gc.expires_at, gc.disabled_at, gc.created_at";

/// A card as clients see it (Wave 60): display fragments
/// only — never the code, never the hash. `status` is
/// derived at read (active | disabled | expired |
/// depleted); `redeemed_net_poisha` is initial − balance,
/// which nets redemptions against restores and admin
/// adjustments — the ledger carries the gross movements.
#[derive(Debug, Serialize)]
pub struct GiftCardView {
    pub id: Uuid,
    pub store_id: Uuid,
    pub store_slug: String,
    pub store_name: String,
    pub code_prefix: String,
    pub last4: String,
    pub initial_amount_poisha: i64,
    pub balance_poisha: i64,
    pub redeemed_net_poisha: i64,
    pub status: String,
    pub note: Option<String>,
    pub recipient_name: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub disabled: bool,
    pub created_at: DateTime<Utc>,
}

fn card_view(row: &GiftCardRow, now: DateTime<Utc>) -> GiftCardView {
    GiftCardView {
        id: row.id,
        store_id: row.store_id,
        store_slug: row.store_slug.clone(),
        store_name: row.store_name.clone(),
        code_prefix: row.code_prefix.clone(),
        last4: row.last4.clone(),
        initial_amount_poisha: row.initial_amount_poisha,
        balance_poisha: row.balance_poisha,
        redeemed_net_poisha: row.initial_amount_poisha - row.balance_poisha,
        status: gift_card_status(
            row.disabled_at.is_some(),
            row.balance_poisha,
            row.expires_at.map(|at| at.timestamp()),
            now.timestamp(),
        )
        .to_string(),
        note: row.note.clone(),
        recipient_name: row.recipient_name.clone(),
        expires_at: row.expires_at,
        disabled: row.disabled_at.is_some(),
        created_at: row.created_at,
    }
}

/// One ledger movement as clients see it (Wave 60).
#[derive(Debug, Serialize, FromRow)]
pub struct GiftCardLedgerEntry {
    pub id: Uuid,
    pub kind: String,
    /// Signed: positive into the card, negative out.
    pub delta_poisha: i64,
    pub balance_after_poisha: i64,
    pub order_id: Option<Uuid>,
    pub sub_order_id: Option<Uuid>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A card plus its recent ledger (Wave 60).
#[derive(Debug, Serialize)]
pub struct GiftCardDetail {
    #[serde(flatten)]
    pub card: GiftCardView,
    pub ledger: Vec<GiftCardLedgerEntry>,
}

/// A freshly issued card WITH its full code (Wave 60).
/// This shape exists only in the issue response — the one
/// moment the code is ever visible.
#[derive(Debug, Serialize)]
pub struct CreatedGiftCard {
    #[serde(flatten)]
    pub card: GiftCardView,
    pub code: String,
}

/// The issue response (Wave 60): one card for a plain
/// issue, many for a bulk issue (`count` > 1).
#[derive(Debug, Serialize)]
pub struct IssuedGiftCards {
    pub cards: Vec<CreatedGiftCard>,
}

/// A page of cards (Wave 60).
#[derive(Debug, Serialize)]
pub struct GiftCardList {
    pub items: Vec<GiftCardView>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
}

/// Aggregate figures over one store's cards, or the whole
/// platform's for the admin metrics (Wave 60). Assembled
/// from two aggregate queries (card rows, then ledger
/// rows) — one query cannot SUM both tables without
/// multiplying rows.
#[derive(Debug, Serialize)]
pub struct GiftCardStats {
    pub cards_total: i64,
    pub active_cards: i64,
    pub disabled_cards: i64,
    pub expired_cards: i64,
    pub depleted_cards: i64,
    /// Sum of every card's initial value.
    pub initial_total_poisha: i64,
    /// Balances on cards a holder can still spend (not
    /// disabled, not expired, balance above zero) — the live
    /// stored-value liability. Before the overview remake
    /// this summed EVERY balance, counting value frozen on
    /// disabled cards and value stranded on expired cards as
    /// live liability; those two buckets are reported
    /// separately below so the figures reconcile.
    pub outstanding_poisha: i64,
    /// Balances frozen on admin-disabled cards: unspendable
    /// until re-enabled, so excluded from the live liability.
    pub frozen_disabled_poisha: i64,
    /// Balances stranded on expired (not disabled) cards:
    /// unspendable by expiry, so excluded from the live
    /// liability.
    pub expired_unredeemed_poisha: i64,
    /// Gross redeemed value (ledger redeem rows), all time.
    pub redeemed_total_poisha: i64,
    /// Gross restored value (cancel + return restores).
    pub restored_total_poisha: i64,
    /// Net of admin balance adjustments (ledger
    /// `admin_adjust` deltas, signed): the one flow that
    /// moves balances without an issue/redeem/restore, so
    /// the reconciliation initial − redeemed + restored +
    /// adjusted = total balances holds.
    pub adjusted_net_poisha: i64,
}

/// The card-row half of [`GiftCardStats`].
#[derive(Debug, FromRow)]
struct CardAggregates {
    cards_total: i64,
    active_cards: i64,
    disabled_cards: i64,
    expired_cards: i64,
    depleted_cards: i64,
    initial_total_poisha: i64,
    outstanding_poisha: i64,
    frozen_disabled_poisha: i64,
    expired_unredeemed_poisha: i64,
}

/// The ledger half of [`GiftCardStats`].
#[derive(Debug, FromRow)]
struct LedgerAggregates {
    redeemed_total_poisha: i64,
    restored_total_poisha: i64,
    adjusted_net_poisha: i64,
}

/// Compute the stats over one store's cards, or the whole
/// platform when `store_id` is None (Wave 60). The status
/// buckets use the same derivation as
/// `domain::gift_card_status`, precedence included.
pub async fn gift_card_stats(
    pool: &PgPool,
    store_id: Option<Uuid>,
) -> Result<GiftCardStats, ApiError> {
    let cards = sqlx::query_as::<_, CardAggregates>(
        "SELECT COUNT(*)::bigint AS cards_total, \
         COUNT(*) FILTER (WHERE gc.disabled_at IS NULL \
             AND (gc.expires_at IS NULL OR gc.expires_at > now()) \
             AND gc.balance_poisha > 0)::bigint AS active_cards, \
         COUNT(*) FILTER (WHERE gc.disabled_at IS NOT NULL)::bigint AS disabled_cards, \
         COUNT(*) FILTER (WHERE gc.disabled_at IS NULL \
             AND gc.expires_at IS NOT NULL AND gc.expires_at <= now())::bigint \
             AS expired_cards, \
         COUNT(*) FILTER (WHERE gc.disabled_at IS NULL \
             AND (gc.expires_at IS NULL OR gc.expires_at > now()) \
             AND gc.balance_poisha = 0)::bigint AS depleted_cards, \
         COALESCE(SUM(gc.initial_amount_poisha), 0)::bigint AS initial_total_poisha, \
         COALESCE(SUM(gc.balance_poisha) FILTER (WHERE gc.disabled_at IS NULL \
             AND (gc.expires_at IS NULL OR gc.expires_at > now()) \
             AND gc.balance_poisha > 0), 0)::bigint AS outstanding_poisha, \
         COALESCE(SUM(gc.balance_poisha) FILTER \
             (WHERE gc.disabled_at IS NOT NULL), 0)::bigint \
             AS frozen_disabled_poisha, \
         COALESCE(SUM(gc.balance_poisha) FILTER (WHERE gc.disabled_at IS NULL \
             AND gc.expires_at IS NOT NULL AND gc.expires_at <= now()), 0)::bigint \
             AS expired_unredeemed_poisha \
         FROM gift_cards gc WHERE ($1::uuid IS NULL OR gc.store_id = $1)",
    )
    .bind(store_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("gift card stats (cards)", e))?;
    let ledger = sqlx::query_as::<_, LedgerAggregates>(
        "SELECT COALESCE(SUM(-l.delta_poisha) FILTER (WHERE l.kind = 'redeem'), 0)::bigint \
             AS redeemed_total_poisha, \
         COALESCE(SUM(l.delta_poisha) FILTER \
             (WHERE l.kind IN ('cancel_restore', 'return_restore')), 0)::bigint \
             AS restored_total_poisha, \
         COALESCE(SUM(l.delta_poisha) FILTER \
             (WHERE l.kind = 'admin_adjust'), 0)::bigint \
             AS adjusted_net_poisha \
         FROM gift_card_ledger l JOIN gift_cards gc ON gc.id = l.gift_card_id \
         WHERE ($1::uuid IS NULL OR gc.store_id = $1)",
    )
    .bind(store_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("gift card stats (ledger)", e))?;
    Ok(GiftCardStats {
        cards_total: cards.cards_total,
        active_cards: cards.active_cards,
        disabled_cards: cards.disabled_cards,
        expired_cards: cards.expired_cards,
        depleted_cards: cards.depleted_cards,
        initial_total_poisha: cards.initial_total_poisha,
        outstanding_poisha: cards.outstanding_poisha,
        frozen_disabled_poisha: cards.frozen_disabled_poisha,
        expired_unredeemed_poisha: cards.expired_unredeemed_poisha,
        redeemed_total_poisha: ledger.redeemed_total_poisha,
        restored_total_poisha: ledger.restored_total_poisha,
        adjusted_net_poisha: ledger.adjusted_net_poisha,
    })
}

// ---------------------------------------------------------------------
// Shared internals
// ---------------------------------------------------------------------

/// Append one ledger row (Wave 60). Always called inside
/// the same transaction as the balance update it records.
#[allow(clippy::too_many_arguments)]
async fn insert_ledger_row(
    conn: &mut PgConnection,
    card_id: Uuid,
    kind: &str,
    delta_poisha: i64,
    balance_after_poisha: i64,
    order_id: Option<Uuid>,
    sub_order_id: Option<Uuid>,
    actor_user_id: Option<Uuid>,
    note: Option<&str>,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO gift_card_ledger (gift_card_id, kind, delta_poisha, \
         balance_after_poisha, order_id, sub_order_id, actor_user_id, note) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(card_id)
    .bind(kind)
    .bind(delta_poisha)
    .bind(balance_after_poisha)
    .bind(order_id)
    .bind(sub_order_id)
    .bind(actor_user_id)
    .bind(note)
    .execute(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("insert gift card ledger row", e))?;
    Ok(())
}

/// Load one card view row by id (Wave 60).
async fn load_card_row(pool: &PgPool, card_id: Uuid) -> Result<Option<GiftCardRow>, ApiError> {
    let sql = format!(
        "SELECT {CARD_COLUMNS} FROM gift_cards gc \
         JOIN stores s ON s.id = gc.store_id WHERE gc.id = $1"
    );
    sqlx::query_as::<_, GiftCardRow>(&sql)
        .bind(card_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::internal("load gift card", e))
}

/// Load a card's recent ledger, newest first (Wave 60).
async fn load_card_ledger(
    pool: &PgPool,
    card_id: Uuid,
) -> Result<Vec<GiftCardLedgerEntry>, ApiError> {
    sqlx::query_as::<_, GiftCardLedgerEntry>(
        "SELECT id, kind, delta_poisha, balance_after_poisha, order_id, \
         sub_order_id, note, created_at FROM gift_card_ledger \
         WHERE gift_card_id = $1 ORDER BY created_at DESC, id DESC LIMIT 50",
    )
    .bind(card_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("load gift card ledger", e))
}

/// The status WHERE fragment shared by the list endpoints
/// (Wave 60): a derived status expressed in SQL so lists
/// can filter by it. `$1` is the requested status (NULL =
/// no filter). The derivation mirrors
/// `domain::gift_card_status` exactly, precedence included.
const STATUS_FILTER: &str = "($1::text IS NULL \
    OR ($1 = 'disabled' AND gc.disabled_at IS NOT NULL) \
    OR ($1 = 'expired' AND gc.disabled_at IS NULL \
        AND gc.expires_at IS NOT NULL AND gc.expires_at <= now()) \
    OR ($1 = 'depleted' AND gc.disabled_at IS NULL \
        AND (gc.expires_at IS NULL OR gc.expires_at > now()) \
        AND gc.balance_poisha = 0) \
    OR ($1 = 'active' AND gc.disabled_at IS NULL \
        AND (gc.expires_at IS NULL OR gc.expires_at > now()) \
        AND gc.balance_poisha > 0))";

/// Reject a status filter outside the vocabulary (Wave
/// 60) instead of silently returning an empty list.
fn validate_status_filter(status: Option<&str>) -> Result<(), ApiError> {
    match status {
        None => Ok(()),
        Some("active" | "disabled" | "expired" | "depleted") => Ok(()),
        Some(_) => Err(ApiError::bad_request(
            "কার্ডের অবস্থা সঠিক নয় — unknown gift card status filter",
        )),
    }
}

/// The effective default validity in days (Wave 60): the
/// platform setting when it parses to a valid day count,
/// else the domain constant — the DB > const precedent.
async fn effective_default_validity_days(pool: &PgPool) -> i64 {
    match platform_setting_i64(pool, GIFT_CARD_VALIDITY_SETTING_KEY).await {
        Some(days) if (1..=GIFT_CARD_MAX_VALIDITY_DAYS).contains(&days) => days,
        _ => GIFT_CARD_DEFAULT_VALIDITY_DAYS,
    }
}

/// Clean an optional free-text input (Wave 60): trim, and
/// treat blank as absent.
fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

// ---------------------------------------------------------------------
// Seller endpoints
// ---------------------------------------------------------------------

/// Issue input (Wave 60). `count` > 1 is a bulk issue:
/// that many identical cards, each with its own code, in
/// one transaction. `no_expiry: true` issues cards that
/// never expire; otherwise `validity_days` wins when
/// present, else the platform default.
#[derive(Debug, Deserialize)]
pub struct IssueGiftCardInput {
    pub amount_poisha: i64,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub recipient_name: Option<String>,
    #[serde(default)]
    pub validity_days: Option<i64>,
    #[serde(default)]
    pub no_expiry: bool,
    #[serde(default)]
    pub count: Option<i64>,
}

/// POST /v1/seller/gift-cards (Wave 60): issue one card —
/// or a bulk batch when `count` > 1 — for the scoped
/// store. Owner-level (`manage_payouts`): issuing value
/// spends the store's future earnings. Each response card
/// carries its full code; that response is the ONLY place
/// the code ever appears.
pub async fn issue_gift_card_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<crate::access::StoreScopeQuery>,
    Json(input): Json<IssueGiftCardInput>,
) -> Result<(StatusCode, Json<IssuedGiftCards>), ApiError> {
    let pool = state.require_pool()?;
    let (store, _role) =
        resolve_scoped_store(pool, user.user_id, scope.store_id, STORE_ACTION_MANAGE_PAYOUTS)
            .await?;
    let count = input.count.unwrap_or(1);
    validate_gift_card_bulk_count(count).map_err(|e| {
        ApiError::bad_request(format!("গিফট কার্ডের সংখ্যা সঠিক নয় — {e}"))
    })?;
    let note = clean_optional(input.note);
    let recipient_name = clean_optional(input.recipient_name);
    if input.no_expiry && input.validity_days.is_some() {
        return Err(ApiError::bad_request(
            "মেয়াদহীন কার্ডে মেয়াদের দিন দেওয়া যাবে না — a no-expiry card cannot also carry validity days",
        ));
    }
    validate_gift_card_issue(
        input.amount_poisha,
        note.as_deref(),
        recipient_name.as_deref(),
        input.validity_days,
    )
    .map_err(|e| ApiError::bad_request(format!("গিফট কার্ডের তথ্য সঠিক নয় — {e}")))?;
    let validity_days = if input.no_expiry {
        None
    } else {
        Some(input.validity_days.unwrap_or(effective_default_validity_days(pool).await))
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin gift card issue transaction", e))?;
    let mut created: Vec<(Uuid, String)> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        // Generate + insert, retrying on a hash collision.
        // With ≈124 bits of entropy a collision is a
        // never-event; the retry exists so it stays one
        // instead of becoming a 500.
        let mut placed: Option<(Uuid, String)> = None;
        for _attempt in 0..3 {
            let code = generate_gift_card_code();
            let normalised = normalize_gift_card_code(&code).ok_or_else(|| {
                ApiError::internal_any("generated gift card code failed normalisation", "shape")
            })?;
            let card_id: Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO gift_cards (store_id, code_hash, code_prefix, last4, \
                 initial_amount_poisha, balance_poisha, note, recipient_name, \
                 issued_by_user_id, expires_at) \
                 VALUES ($1, $2, $3, $4, $5, $5, $6, $7, $8, \
                     CASE WHEN $9::bigint IS NULL THEN NULL \
                          ELSE now() + make_interval(days => $9::int) END) \
                 ON CONFLICT (code_hash) DO NOTHING RETURNING id",
            )
            .bind(store.id)
            .bind(hash_gift_card_code(&normalised))
            .bind(gift_card_display_prefix(&code))
            .bind(gift_card_last4(&code))
            .bind(input.amount_poisha)
            .bind(note.as_deref())
            .bind(recipient_name.as_deref())
            .bind(user.user_id)
            .bind(validity_days)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("insert gift card", e))?;
            if let Some(id) = card_id {
                placed = Some((id, code));
                break;
            }
        }
        let (card_id, code) = placed.ok_or_else(|| {
            ApiError::internal_any("issue gift card", "code collision persisted across retries")
        })?;
        insert_ledger_row(
            &mut tx,
            card_id,
            GIFT_CARD_LEDGER_ISSUE,
            input.amount_poisha,
            input.amount_poisha,
            None,
            None,
            Some(user.user_id),
            note.as_deref(),
        )
        .await?;
        created.push((card_id, code));
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit gift card issue", e))?;

    let now = Utc::now();
    let mut cards = Vec::with_capacity(created.len());
    for (card_id, code) in created {
        let row = load_card_row(pool, card_id)
            .await?
            .ok_or_else(|| ApiError::internal_any("reload issued gift card", "row missing"))?;
        cards.push(CreatedGiftCard {
            card: card_view(&row, now),
            code,
        });
    }
    Ok((StatusCode::CREATED, Json(IssuedGiftCards { cards })))
}

/// The seller list query (Wave 60).
#[derive(Debug, Deserialize)]
pub struct SellerGiftCardListQuery {
    pub store_id: Option<Uuid>,
    pub status: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

/// GET /v1/seller/gift-cards (Wave 60): the scoped store's
/// cards, newest first, masked. `view_finances` — owners
/// and managers may look; issuing stays owner-level.
pub async fn list_gift_cards_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<SellerGiftCardListQuery>,
) -> Result<Json<GiftCardList>, ApiError> {
    let pool = state.require_pool()?;
    let (store, _role) =
        resolve_scoped_store(pool, user.user_id, query.store_id, STORE_ACTION_VIEW_FINANCES)
            .await?;
    validate_status_filter(query.status.as_deref())?;
    let (page, limit) = clamp_pagination(query.page, query.limit);
    let offset = (page - 1) * limit;
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*)::bigint FROM gift_cards gc \
         WHERE gc.store_id = $2 AND {STATUS_FILTER}"
    ))
    .bind(query.status.as_deref())
    .bind(store.id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count gift cards", e))?;
    let rows = sqlx::query_as::<_, GiftCardRow>(&format!(
        "SELECT {CARD_COLUMNS} FROM gift_cards gc \
         JOIN stores s ON s.id = gc.store_id \
         WHERE gc.store_id = $2 AND {STATUS_FILTER} \
         ORDER BY gc.created_at DESC, gc.id DESC LIMIT $3 OFFSET $4"
    ))
    .bind(query.status.as_deref())
    .bind(store.id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list gift cards", e))?;
    let now = Utc::now();
    Ok(Json(GiftCardList {
        items: rows.iter().map(|row| card_view(row, now)).collect(),
        total,
        page,
        limit,
    }))
}

/// Load a card the caller may READ (Wave 60): the card's
/// store must be one the caller holds `view_finances` in —
/// strangers get the uniform 404, under-privileged members
/// the bilingual 403 (the access module's conventions).
async fn load_card_for_read(
    pool: &PgPool,
    user_id: Uuid,
    card_id: Uuid,
) -> Result<GiftCardRow, ApiError> {
    let row = load_card_row(pool, card_id)
        .await?
        .ok_or_else(|| ApiError::not_found("গিফট কার্ড পাওয়া যায়নি — gift card not found"))?;
    let role = seller_access(pool, user_id, row.store_id).await?;
    require_role(&role, STORE_ACTION_VIEW_FINANCES)?;
    Ok(row)
}

/// Load a card the caller may ACT on (Wave 60):
/// `manage_payouts` in the card's store.
async fn load_card_for_action(
    pool: &PgPool,
    user_id: Uuid,
    card_id: Uuid,
) -> Result<GiftCardRow, ApiError> {
    let row = load_card_row(pool, card_id)
        .await?
        .ok_or_else(|| ApiError::not_found("গিফট কার্ড পাওয়া যায়নি — gift card not found"))?;
    let role = seller_access(pool, user_id, row.store_id).await?;
    require_role(&role, STORE_ACTION_MANAGE_PAYOUTS)?;
    Ok(row)
}

/// GET /v1/seller/gift-cards/:id (Wave 60): one card plus
/// its recent ledger.
pub async fn get_gift_card_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(card_id): Path<Uuid>,
) -> Result<Json<GiftCardDetail>, ApiError> {
    let pool = state.require_pool()?;
    let row = load_card_for_read(pool, user.user_id, card_id).await?;
    let ledger = load_card_ledger(pool, card_id).await?;
    Ok(Json(GiftCardDetail {
        card: card_view(&row, Utc::now()),
        ledger,
    }))
}

/// GET /v1/seller/gift-card-stats (Wave 60): the scoped
/// store's card aggregates — counts by derived status,
/// issued / outstanding / redeemed / restored totals.
pub async fn gift_card_stats_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(scope): Query<crate::access::StoreScopeQuery>,
) -> Result<Json<GiftCardStats>, ApiError> {
    let pool = state.require_pool()?;
    let (store, _role) =
        resolve_scoped_store(pool, user.user_id, scope.store_id, STORE_ACTION_VIEW_FINANCES)
            .await?;
    Ok(Json(gift_card_stats(pool, Some(store.id)).await?))
}

/// Shared disable/enable write (Wave 60): set or clear
/// `disabled_at`. Idempotent — freezing a frozen card (or
/// unfreezing a live one) returns the current view, since
/// the caller's intent is a state, not an event.
async fn set_card_disabled(
    pool: &PgPool,
    card_id: Uuid,
    disabled: bool,
) -> Result<GiftCardView, ApiError> {
    sqlx::query(
        "UPDATE gift_cards SET disabled_at = CASE WHEN $2 THEN now() ELSE NULL END, \
         updated_at = now() WHERE id = $1",
    )
    .bind(card_id)
    .bind(disabled)
    .execute(pool)
    .await
    .map_err(|e| ApiError::internal("set gift card disabled state", e))?;
    let row = load_card_row(pool, card_id)
        .await?
        .ok_or_else(|| ApiError::internal_any("reload gift card", "row missing"))?;
    Ok(card_view(&row, Utc::now()))
}

/// POST /v1/seller/gift-cards/:id/disable (Wave 60):
/// freeze one of the store's cards. Owner-level, like
/// issuing — a frozen card cannot be spent.
pub async fn disable_gift_card_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(card_id): Path<Uuid>,
) -> Result<Json<GiftCardView>, ApiError> {
    let pool = state.require_pool()?;
    load_card_for_action(pool, user.user_id, card_id).await?;
    Ok(Json(set_card_disabled(pool, card_id, true).await?))
}

/// POST /v1/seller/gift-cards/:id/enable (Wave 60):
/// unfreeze. An expired card stays expired — enabling
/// clears the freeze, never the clock.
pub async fn enable_gift_card_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(card_id): Path<Uuid>,
) -> Result<Json<GiftCardView>, ApiError> {
    let pool = state.require_pool()?;
    load_card_for_action(pool, user.user_id, card_id).await?;
    Ok(Json(set_card_disabled(pool, card_id, false).await?))
}

// ---------------------------------------------------------------------
// Admin endpoints
// ---------------------------------------------------------------------

/// The admin list query (Wave 60).
#[derive(Debug, Deserialize)]
pub struct AdminGiftCardListQuery {
    pub q: Option<String>,
    pub store_id: Option<Uuid>,
    pub status: Option<String>,
    pub page: Option<i64>,
    pub limit: Option<i64>,
}

/// GET /v1/admin/gift-cards (Wave 60): every store's
/// cards, searchable by code prefix, last4 or store slug —
/// the support view. Codes and hashes never appear.
pub async fn admin_list_gift_cards_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<AdminGiftCardListQuery>,
) -> Result<Json<GiftCardList>, ApiError> {
    let pool = state.require_pool()?;
    require_admin(&state, &user).await?;
    validate_status_filter(query.status.as_deref())?;
    let (page, limit) = clamp_pagination(query.page, query.limit);
    let offset = (page - 1) * limit;
    let q = clean_optional(query.q);
    let pattern = q.as_ref().map(|text| format!("%{}%", text));
    let last4 = q.as_ref().map(|text| text.trim().to_uppercase());
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*)::bigint FROM gift_cards gc \
         JOIN stores s ON s.id = gc.store_id \
         WHERE ($2::uuid IS NULL OR gc.store_id = $2) \
           AND ($3::text IS NULL OR gc.code_prefix ILIKE $3 \
                OR gc.last4 = $4 OR s.slug ILIKE $3) \
           AND {STATUS_FILTER}"
    ))
    .bind(query.status.as_deref())
    .bind(query.store_id)
    .bind(pattern.as_deref())
    .bind(last4.as_deref())
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::internal("count admin gift cards", e))?;
    let rows = sqlx::query_as::<_, GiftCardRow>(&format!(
        "SELECT {CARD_COLUMNS} FROM gift_cards gc \
         JOIN stores s ON s.id = gc.store_id \
         WHERE ($2::uuid IS NULL OR gc.store_id = $2) \
           AND ($3::text IS NULL OR gc.code_prefix ILIKE $3 \
                OR gc.last4 = $4 OR s.slug ILIKE $3) \
           AND {STATUS_FILTER} \
         ORDER BY gc.created_at DESC, gc.id DESC LIMIT $5 OFFSET $6"
    ))
    .bind(query.status.as_deref())
    .bind(query.store_id)
    .bind(pattern.as_deref())
    .bind(last4.as_deref())
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("list admin gift cards", e))?;
    let now = Utc::now();
    Ok(Json(GiftCardList {
        items: rows.iter().map(|row| card_view(row, now)).collect(),
        total,
        page,
        limit,
    }))
}

/// GET /v1/admin/gift-cards/:id (Wave 60): one card plus
/// its ledger, for support.
pub async fn admin_get_gift_card_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(card_id): Path<Uuid>,
) -> Result<Json<GiftCardDetail>, ApiError> {
    let pool = state.require_pool()?;
    require_admin(&state, &user).await?;
    let row = load_card_row(pool, card_id)
        .await?
        .ok_or_else(|| ApiError::not_found("গিফট কার্ড পাওয়া যায়নি — gift card not found"))?;
    let ledger = load_card_ledger(pool, card_id).await?;
    Ok(Json(GiftCardDetail {
        card: card_view(&row, Utc::now()),
        ledger,
    }))
}

/// Shared admin disable/enable (Wave 60): the state write
/// and its audit row commit together.
async fn admin_set_card_disabled(
    state: &AppState,
    user: &AuthUser,
    card_id: Uuid,
    disabled: bool,
) -> Result<GiftCardView, ApiError> {
    let pool = state.require_pool()?;
    require_admin(state, user).await?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin admin gift card transaction", e))?;
    let exists: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM gift_cards WHERE id = $1 FOR UPDATE")
            .bind(card_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("lock gift card for admin action", e))?;
    if exists.is_none() {
        return Err(ApiError::not_found("গিফট কার্ড পাওয়া যায়নি — gift card not found"));
    }
    sqlx::query(
        "UPDATE gift_cards SET disabled_at = CASE WHEN $2 THEN now() ELSE NULL END, \
         updated_at = now() WHERE id = $1",
    )
    .bind(card_id)
    .bind(disabled)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::internal("admin set gift card disabled state", e))?;
    record_admin_action(
        &mut *tx,
        user.user_id,
        if disabled { "gift_card.disable" } else { "gift_card.enable" },
        "gift_card",
        Some(&card_id.to_string()),
        None,
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit admin gift card action", e))?;
    let row = load_card_row(pool, card_id)
        .await?
        .ok_or_else(|| ApiError::internal_any("reload gift card", "row missing"))?;
    Ok(card_view(&row, Utc::now()))
}

/// POST /v1/admin/gift-cards/:id/disable (Wave 60):
/// freeze any store's card — the fraud / chargeback
/// lever. Audited.
pub async fn admin_disable_gift_card_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(card_id): Path<Uuid>,
) -> Result<Json<GiftCardView>, ApiError> {
    Ok(Json(admin_set_card_disabled(&state, &user, card_id, true).await?))
}

/// POST /v1/admin/gift-cards/:id/enable (Wave 60):
/// unfreeze. Audited.
pub async fn admin_enable_gift_card_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(card_id): Path<Uuid>,
) -> Result<Json<GiftCardView>, ApiError> {
    Ok(Json(admin_set_card_disabled(&state, &user, card_id, false).await?))
}

/// Admin balance-adjust input (Wave 60). The note is
/// REQUIRED — an unexplained balance change is exactly
/// what the ledger exists to prevent.
#[derive(Debug, Deserialize)]
pub struct AdjustGiftCardInput {
    pub delta_poisha: i64,
    pub note: String,
}

/// POST /v1/admin/gift-cards/:id/adjust (Wave 60): correct
/// a card's balance by a signed delta (never below zero),
/// with a ledger row and an audit row in the same
/// transaction. This is the support-recovery lever for a
/// card whose value was lost to a bug or a dispute — it
/// moves no money by itself; it rewrites store liability,
/// which is why it is admin-only and audited.
pub async fn admin_adjust_gift_card_handler(
    State(state): State<AppState>,
    user: AuthUser,
    Path(card_id): Path<Uuid>,
    Json(input): Json<AdjustGiftCardInput>,
) -> Result<Json<GiftCardView>, ApiError> {
    let pool = state.require_pool()?;
    require_admin(&state, &user).await?;
    let note = input.note.trim().to_string();
    if note.is_empty() {
        return Err(ApiError::bad_request(
            "সংশোধনের কারণ লিখুন — an adjustment needs a note explaining it",
        ));
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::internal("begin gift card adjust transaction", e))?;
    let balance: Option<i64> =
        sqlx::query_scalar("SELECT balance_poisha FROM gift_cards WHERE id = $1 FOR UPDATE")
            .bind(card_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ApiError::internal("lock gift card for adjustment", e))?;
    let Some(balance) = balance else {
        return Err(ApiError::not_found("গিফট কার্ড পাওয়া যায়নি — gift card not found"));
    };
    let new_balance = validate_gift_card_adjust(balance, input.delta_poisha).map_err(|e| {
        ApiError::bad_request(format!("ব্যালেন্স সংশোধন করা যাচ্ছে না — {e}"))
    })?;
    sqlx::query("UPDATE gift_cards SET balance_poisha = $2, updated_at = now() WHERE id = $1")
        .bind(card_id)
        .bind(new_balance)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("apply gift card adjustment", e))?;
    insert_ledger_row(
        &mut tx,
        card_id,
        GIFT_CARD_LEDGER_ADMIN_ADJUST,
        input.delta_poisha,
        new_balance,
        None,
        None,
        Some(user.user_id),
        Some(&note),
    )
    .await?;
    record_admin_action(
        &mut *tx,
        user.user_id,
        "gift_card.adjust",
        "gift_card",
        Some(&card_id.to_string()),
        Some(serde_json::json!({
            "delta_poisha": input.delta_poisha,
            "balance_before_poisha": balance,
            "balance_after_poisha": new_balance,
        })),
    )
    .await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::internal("commit gift card adjustment", e))?;
    let row = load_card_row(pool, card_id)
        .await?
        .ok_or_else(|| ApiError::internal_any("reload gift card", "row missing"))?;
    Ok(Json(card_view(&row, Utc::now())))
}

// ---------------------------------------------------------------------
// Public balance check
// ---------------------------------------------------------------------

/// Balance-check input (Wave 60).
#[derive(Debug, Deserialize)]
pub struct GiftCardBalanceInput {
    pub code: String,
}

/// The balance-check answer (Wave 60): what a holder
/// learns about their own card. No code, no hash.
#[derive(Debug, Serialize)]
pub struct GiftCardBalanceView {
    pub balance_poisha: i64,
    pub initial_amount_poisha: i64,
    pub status: String,
    /// The card's store — an id, not a secret (store ids
    /// ride every product view); the checkout page uses it
    /// to preview the applicable amount against the cart.
    pub store_id: Uuid,
    pub store_slug: String,
    pub store_name: String,
    pub last4: String,
    pub expires_at: Option<DateTime<Utc>>,
}

/// The ONE uniform balance-check failure (Wave 60): an
/// unknown code and a malformed code are
/// indistinguishable, so the endpoint cannot enumerate.
fn balance_not_found() -> ApiError {
    ApiError::not_found("গিফট কার্ড পাওয়া যায়নি — gift card not found")
}

/// POST /v1/gift-cards/balance (Wave 60): the public,
/// unauthenticated balance check — the standard
/// bearer-instrument shape (see the module doc for the
/// tradeoff). A found card reports its TRUE status,
/// including disabled and expired: the holder of a frozen
/// card deserves to know it is frozen.
pub async fn check_gift_card_balance_handler(
    State(state): State<AppState>,
    Json(input): Json<GiftCardBalanceInput>,
) -> Result<Json<GiftCardBalanceView>, ApiError> {
    let pool = state.require_pool()?;
    let normalised = normalize_gift_card_code(&input.code).ok_or_else(balance_not_found)?;
    let row = sqlx::query_as::<_, GiftCardRow>(&format!(
        "SELECT {CARD_COLUMNS} FROM gift_cards gc \
         JOIN stores s ON s.id = gc.store_id WHERE gc.code_hash = $1"
    ))
    .bind(hash_gift_card_code(&normalised))
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal("gift card balance lookup", e))?
    .ok_or_else(balance_not_found)?;
    let view = card_view(&row, Utc::now());
    Ok(Json(GiftCardBalanceView {
        balance_poisha: view.balance_poisha,
        initial_amount_poisha: view.initial_amount_poisha,
        status: view.status,
        store_id: view.store_id,
        store_slug: view.store_slug,
        store_name: view.store_name,
        last4: view.last4,
        expires_at: view.expires_at,
    }))
}

// ---------------------------------------------------------------------
// Checkout / cancel / return hooks (called by orders.rs and
// returns.rs inside their transactions — the loyalty shape)
// ---------------------------------------------------------------------

/// A card row locked FOR UPDATE inside the checkout
/// transaction (Wave 60).
#[derive(Debug, FromRow)]
pub struct LockedGiftCard {
    pub id: Uuid,
    pub store_id: Uuid,
    pub balance_poisha: i64,
    pub last4: String,
    expires_at: Option<DateTime<Utc>>,
    disabled_at: Option<DateTime<Utc>>,
}

/// Lock a card by its normalised code inside the checkout
/// transaction (Wave 60). `Ok(None)` = no such card; the
/// caller answers the uniform invalid-code 400.
pub async fn lock_card_by_code(
    conn: &mut PgConnection,
    normalised_code: &str,
) -> Result<Option<LockedGiftCard>, ApiError> {
    sqlx::query_as::<_, LockedGiftCard>(
        "SELECT id, store_id, balance_poisha, last4, expires_at, disabled_at \
         FROM gift_cards WHERE code_hash = $1 FOR UPDATE",
    )
    .bind(hash_gift_card_code(normalised_code))
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("lock gift card at checkout", e))
}

/// Why a locked card cannot be spent, when it cannot
/// (Wave 60): the bilingual checkout 400 naming the reason
/// class. A DISABLED or EXPIRED card is a card the buyer
/// is holding — naming its state leaks nothing (the
/// balance endpoint reports the same state to anyone
/// holding the code). An UNKNOWN code never reaches this
/// function; checkout answers it with the one uniform
/// message.
pub fn checkout_block_reason(card: &LockedGiftCard, now: DateTime<Utc>) -> Option<ApiError> {
    if card.disabled_at.is_some() {
        return Some(ApiError::bad_request(
            "গিফট কার্ডটি বন্ধ করা আছে — this gift card has been disabled",
        ));
    }
    if let Some(expires_at) = card.expires_at {
        if expires_at <= now {
            return Some(ApiError::bad_request(
                "গিফট কার্ডের মেয়াদ শেষ — this gift card has expired",
            ));
        }
    }
    if card.balance_poisha <= 0 {
        return Some(ApiError::bad_request(
            "গিফট কার্ডে কোনো ব্যালেন্স নেই — this gift card has no balance left",
        ));
    }
    None
}

/// Spend `applied_poisha` from a locked card (Wave 60):
/// the balance update and its redeem ledger row, inside
/// the caller's checkout transaction. The caller computed
/// `applied_poisha` via `gift_card_applicable_poisha`
/// against the card's store's post-coupon goods.
pub async fn apply_redemption(
    conn: &mut PgConnection,
    card: &LockedGiftCard,
    applied_poisha: i64,
    order_id: Uuid,
    sub_order_id: Uuid,
    buyer_user_id: Uuid,
) -> Result<(), ApiError> {
    let new_balance = card
        .balance_poisha
        .checked_sub(applied_poisha)
        .filter(|balance| *balance >= 0)
        .ok_or_else(|| {
            ApiError::internal_any("gift card redemption", "applied amount exceeds the balance")
        })?;
    sqlx::query("UPDATE gift_cards SET balance_poisha = $2, updated_at = now() WHERE id = $1")
        .bind(card.id)
        .bind(new_balance)
        .execute(&mut *conn)
        .await
        .map_err(|e| ApiError::internal("apply gift card redemption", e))?;
    insert_ledger_row(
        conn,
        card.id,
        GIFT_CARD_LEDGER_REDEEM,
        -applied_poisha,
        new_balance,
        Some(order_id),
        Some(sub_order_id),
        Some(buyer_user_id),
        None,
    )
    .await
}

/// Add value back to a card inside a caller's transaction
/// (Wave 60): the shared restore write for the cancel and
/// return funnels. Restoring never touches `expires_at` or
/// `disabled_at` — the balance returns, the card's state
/// stands (see the module doc).
async fn restore_value(
    conn: &mut PgConnection,
    card_id: Uuid,
    amount_poisha: i64,
    kind: &str,
    order_id: Uuid,
    sub_order_id: Uuid,
) -> Result<(), ApiError> {
    let new_balance: Option<i64> = sqlx::query_scalar(
        "UPDATE gift_cards SET balance_poisha = balance_poisha + $2, updated_at = now() \
         WHERE id = $1 RETURNING balance_poisha",
    )
    .bind(card_id)
    .bind(amount_poisha)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("restore gift card value", e))?;
    let Some(new_balance) = new_balance else {
        // The sub-order references a card that no longer
        // exists — impossible through the FK, but a restore
        // must never take its funnel down with it.
        return Ok(());
    };
    insert_ledger_row(
        conn,
        card_id,
        kind,
        amount_poisha,
        new_balance,
        Some(order_id),
        Some(sub_order_id),
        None,
        None,
    )
    .await
}

/// The sub-order's gift-card snapshot (Wave 60): which
/// card funded it and by how much.
#[derive(Debug, FromRow)]
struct SubOrderGiftSnapshot {
    order_id: Uuid,
    gift_card_id: Option<Uuid>,
    gift_card_discount_poisha: i64,
}

async fn load_sub_order_snapshot(
    conn: &mut PgConnection,
    sub_order_id: Uuid,
) -> Result<Option<SubOrderGiftSnapshot>, ApiError> {
    sqlx::query_as::<_, SubOrderGiftSnapshot>(
        "SELECT order_id, gift_card_id, gift_card_discount_poisha \
         FROM sub_orders WHERE id = $1",
    )
    .bind(sub_order_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| ApiError::internal("load sub-order gift card snapshot", e))
}

/// Restore a cancelled sub-order's FULL redemption (Wave
/// 60), mirroring `loyalty::refund_for_cancelled_sub_order`:
/// called by both cancel paths inside their transactions.
/// A sub-order that used no card is a no-op.
pub async fn restore_for_cancelled_sub_order(
    conn: &mut PgConnection,
    sub_order_id: Uuid,
) -> Result<(), ApiError> {
    let Some(snapshot) = load_sub_order_snapshot(conn, sub_order_id).await? else {
        return Ok(());
    };
    let (Some(card_id), amount) = (snapshot.gift_card_id, snapshot.gift_card_discount_poisha)
    else {
        return Ok(());
    };
    if amount <= 0 {
        return Ok(());
    }
    restore_value(
        conn,
        card_id,
        amount,
        GIFT_CARD_LEDGER_CANCEL_RESTORE,
        snapshot.order_id,
        sub_order_id,
    )
    .await
}

/// Restore the return-proportional share of a sub-order's
/// redemption (Wave 60) and return the restored amount:
/// `refund_poisha` is what the buyer is owed back for the
/// returned goods and `refundable_goods_poisha` is the
/// basis it was computed from (goods − coupon discount) —
/// the share is `domain::gift_card_restore_for_return`.
/// The caller subtracts the returned amount from the cash
/// refund it stores, so the buyer recovers the rest as
/// card balance and the total recovery is unchanged.
pub async fn restore_for_returned_sub_order(
    conn: &mut PgConnection,
    sub_order_id: Uuid,
    refund_poisha: i64,
    refundable_goods_poisha: i64,
) -> Result<i64, ApiError> {
    let Some(snapshot) = load_sub_order_snapshot(conn, sub_order_id).await? else {
        return Ok(0);
    };
    let (Some(card_id), redemption) = (snapshot.gift_card_id, snapshot.gift_card_discount_poisha)
    else {
        return Ok(0);
    };
    let restored = domain::gift_card_restore_for_return(
        redemption,
        refund_poisha,
        refundable_goods_poisha,
    );
    if restored <= 0 {
        return Ok(0);
    }
    restore_value(
        conn,
        card_id,
        restored,
        GIFT_CARD_LEDGER_RETURN_RESTORE,
        snapshot.order_id,
        sub_order_id,
    )
    .await?;
    Ok(restored)
}

/// The platform-wide gift-card figures for the admin
/// metrics snapshot (Wave 60).
pub async fn platform_gift_card_stats(pool: &PgPool) -> Result<GiftCardStats, ApiError> {
    gift_card_stats(pool, None).await
}

// ---------------------------------------------------------------------
// Data export
// ---------------------------------------------------------------------

/// One card in the seller data export (Wave 60): display
/// fragments and money only — no code, no hash, ever.
#[derive(Debug, Serialize, FromRow)]
pub struct ExportGiftCard {
    pub id: Uuid,
    pub store_id: Uuid,
    pub code_prefix: String,
    pub last4: String,
    pub initial_amount_poisha: i64,
    pub balance_poisha: i64,
    pub note: Option<String>,
    pub recipient_name: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub disabled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Every card issued by the given stores, for the seller
/// export (Wave 60).
pub async fn export_gift_cards(
    pool: &PgPool,
    store_ids: &[Uuid],
) -> Result<Vec<ExportGiftCard>, ApiError> {
    if store_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, ExportGiftCard>(
        "SELECT id, store_id, code_prefix, last4, initial_amount_poisha, \
         balance_poisha, note, recipient_name, expires_at, disabled_at, created_at \
         FROM gift_cards WHERE store_id = ANY($1) ORDER BY created_at, id",
    )
    .bind(store_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("export gift cards", e))
}

/// Re-export the redemption helper for the checkout
/// module's convenience (Wave 60): the min-of-balance-and-
/// goods rule lives in the domain; this keeps orders.rs
/// importing one gift-card surface.
pub fn applicable_poisha(balance_poisha: i64, eligible_goods_poisha: i64) -> i64 {
    gift_card_applicable_poisha(balance_poisha, eligible_goods_poisha)
}

// ---------------------------------------------------------------------
// Worker sweep: expiry reminders (Wave 60 extension)
// ---------------------------------------------------------------------

/// What one [`sweep_gift_card_expiry_reminders`] call did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GiftCardReminderReport {
    /// Cards whose issuing seller was reminded this call.
    pub reminded: i64,
}

/// One card due a reminder, with everything the notice
/// needs (Wave 60 extension).
#[derive(Debug, FromRow)]
struct ExpiryReminderCandidate {
    id: Uuid,
    last4: String,
    balance_poisha: i64,
    issued_by_user_id: Uuid,
    issuer_phone: String,
    store_name: String,
}

/// Remind issuing sellers about cards whose value is
/// about to lapse (Wave 60 extension): the worker calls
/// this every cycle, in the media-reaper's shape. A card
/// qualifies when it is not disabled, still holds a
/// balance, has an expiry inside
/// [`GIFT_CARD_EXPIRY_REMINDER_WINDOW_DAYS`], and has
/// never been reminded about (`expiry_reminded_at` —
/// migration 0071). Each reminder is one outbox row to
/// the ISSUING seller (the person who can still reach
/// the recipient), enqueued in the same transaction as
/// the stamp that marks the card reminded — a crash
/// between the two is impossible, so a card is reminded
/// exactly once: never zero times by a lost transaction,
/// never twice by a retried sweep. Already-expired cards
/// are NOT reminded about: the moment to act has passed,
/// and a "your card expired" notice to the seller would
/// announce a fact the stats page already shows.
pub async fn sweep_gift_card_expiry_reminders(
    state: &AppState,
) -> Result<GiftCardReminderReport, ApiError> {
    let pool = state.require_pool()?;
    let candidates = sqlx::query_as::<_, ExpiryReminderCandidate>(
        "SELECT c.id, c.last4, c.balance_poisha, c.issued_by_user_id, \
         u.phone AS issuer_phone, s.name AS store_name \
         FROM gift_cards c \
         JOIN users u ON u.id = c.issued_by_user_id \
         JOIN stores s ON s.id = c.store_id \
         WHERE c.disabled_at IS NULL AND c.balance_poisha > 0 \
         AND c.expires_at IS NOT NULL AND c.expiry_reminded_at IS NULL \
         AND c.expires_at > now() \
         AND c.expires_at <= now() + make_interval(days => $1::int) \
         ORDER BY c.expires_at ASC LIMIT 200",
    )
    .bind(GIFT_CARD_EXPIRY_REMINDER_WINDOW_DAYS)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::internal("gift card expiry reminder candidates", e))?;
    let mut reminded: i64 = 0;
    for candidate in candidates {
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| ApiError::internal("gift card reminder tx begin", e))?;
        // Re-check the stamp inside the transaction: two
        // workers racing the same cycle must not both
        // remind about one card.
        let still_due: bool = sqlx::query_scalar(
            "SELECT expiry_reminded_at IS NULL FROM gift_cards WHERE id = $1 FOR UPDATE",
        )
        .bind(candidate.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::internal("gift card reminder recheck", e))?;
        if still_due {
            notify::enqueue(
                &mut *tx,
                candidate.issued_by_user_id,
                &candidate.issuer_phone,
                NOTIFICATION_GIFT_CARD_EXPIRING,
                &serde_json::json!({
                    "store_name": candidate.store_name,
                    "amount_poisha": candidate.balance_poisha,
                    "card_last4": candidate.last4,
                }),
            )
            .await
            .map_err(|e| ApiError::internal("gift card reminder enqueue", e))?;
            sqlx::query("UPDATE gift_cards SET expiry_reminded_at = now() WHERE id = $1")
                .bind(candidate.id)
                .execute(&mut *tx)
                .await
                .map_err(|e| ApiError::internal("gift card reminder stamp", e))?;
            reminded += 1;
        }
        tx.commit()
            .await
            .map_err(|e| ApiError::internal("gift card reminder tx commit", e))?;
    }
    Ok(GiftCardReminderReport { reminded })
}
