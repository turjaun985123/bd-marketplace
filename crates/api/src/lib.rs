//! BD Marketplace API — thin Axum handlers over individually callable
//! service functions.
//!
//! Wave 1: stateless domain endpoints (delivery charge, cart split,
//! commission). Wave 2: Postgres persistence (SQLx), phone + OTP + JWT
//! auth, stores and products. Wave 3: carts, split orders and COD
//! checkout (`cart` and `orders` modules). Wave 4: weight-based
//! delivery, buyer order cancellation with restock, and receipts.
//! Wave 5: the online payments foundation (`payments` module) —
//! method availability, dev-simulated gateway checkout, signed
//! webhook callbacks and payment retry.
//! Wave 6: courier shipments (`shipments` module) — the real
//! Steadfast adapter behind merchant credentials (or the DEV courier
//! simulator behind its flag), one shipment per sub-order, tracking
//! refresh that syncs sub-order status, and courier availability.
//! Wave 7: the Pathao and RedX adapters (same credential-gated
//! pattern), per-courier delivery-charge estimates, and smart
//! routing — booking with courier `auto` picks the cheapest
//! available courier via the domain `choose_courier` function.
//! Wave 8: reviews and ratings (`reviews` module) — a buyer may
//! review a product only after a delivered sub-order proves they
//! received it; one review per buyer per product; the store owner
//! may post one reply per review; and the product view gains a live
//! rating summary from the single aggregate join in `products`.
//! Wave 9: seller dashboard, earnings and payout requests
//! (`payouts` module) — a store earns only from DELIVERED
//! sub-orders (goods minus the commission snapshot); one earnings
//! aggregate feeds the dashboard, the balance endpoint and payout
//! creation; sellers save one payout account and request payouts
//! against their available balance (platform minimum 500 taka).
//! Wave 10: the platform admin (`admin` module) — every admin
//! endpoint sits behind the one `require_admin` guard (the
//! `is_admin` flag on the user row; the first admin is promoted
//! by SQL, never by an endpoint); admins get overview metrics,
//! the payout processing queue (approve / reject / mark-paid over
//! the Wave 9 transition table), store suspension (enforced in
//! the shared product queries and at checkout), user admin flags
//! (never their own), and platform settings whose rows override
//! the startup commission rate and minimum payout at read time.
//! Wave 11: the eCourier and Paperfly adapters (same credential-
//! gated pattern) complete all five API couriers; availability,
//! estimates and smart routing cover them through the one shared
//! per-courier credential rule, with no new endpoints.
//! Wave 12: Facebook Page linking and post-to-product import
//! (`facebook` module) — a seller links their Page (identity
//! only, no tokens stored) and pastes post texts; the pure
//! domain parser drafts products from the posts, and the import
//! creates the approved drafts through the one product insert
//! path, idempotently by the post's `source_ref`. Fetching posts
//! through the Meta Graph API stays honestly unconfigured (503)
//! or unimplemented (501) until a Meta app and the page-token
//! OAuth flow exist — nothing fakes a fetched post.
//! Wave 13: buyer <-> store messaging and the SIDRA bridge
//! (`messaging` module) — conversations with per-side read
//! markers and unread counts, a best-effort signed notification
//! to the external SIDRA service when a buyer writes, and a
//! signature-verified callback through which SIDRA returns an
//! AI reply: posted directly when the store enabled auto-reply,
//! held as a seller-approved suggestion otherwise. SIDRA is an
//! external service and is never reimplemented here; with no
//! SIDRA configuration the inbox is a complete human chat.
//! Wave 14: coupons and promotions (`coupons` module) —
//! platform coupons (the platform bears the discount, commission
//! stays on full goods) and store coupons (the seller bears it,
//! commission moves to the discounted goods), allocated across
//! sub-orders by the pure domain functions and applied inside
//! the checkout transaction under a coupon row lock; a stateless
//! preview shares the one rejection rule with checkout, and
//! seller earnings now sum the payout snapshot so borne
//! discounts are never paid out.
//! Wave 16: public tracking and returns (`tracking` and
//! `returns` modules) — a login-free tracking view gated on the
//! order's delivery phone (every failure is the same 404), buyer
//! return requests for delivered sub-orders with the seller's
//! approve / reject / received flow (receipt snapshots the
//! refund — goods minus discount, never delivery — restocks the
//! items and opens the refund as pending), the admin refund
//! queue (marking a refund done records an off-platform refund;
//! no gateway refund API is wired), and the RTO fix: a
//! courier-driven return applied by shipment refresh now
//! restocks the sub-order's items in the same transaction.
//! Wave 17: partial advance for COD + the COD fraud shield
//! (`risk` module, checkout in `orders`, a branch in the shared
//! payment callback) — a COD order at/above the platform
//! threshold (default 2,000 taka) or from a buyer whose return
//! history trips the pure domain shield must collect a
//! percentage advance (default 20%) online before any seller
//! may ship; the advance settles through the same signed
//! callback as a full payment, moving the order to
//! `advance_paid` (the balance stays COD). When NO gateway can
//! collect an advance (none configured, dev mode off), checkout
//! deliberately degrades: the order proceeds as plain COD with
//! the required advance recorded on it as a visible risk flag
//! — never a claim that money was collected.
//! Wave 19: manual couriers (`shipments` module) — Sundarban,
//! SA Paribahan, Karatoa and Carrybee have no public APIs, so
//! they join as a seller-driven workflow behind the same
//! shipment system: booking with `courier = "manual"` writes
//! the shipment row directly (no adapter, no network call),
//! the seller records the counter receipt number and every
//! status move by hand, and the moves are checked against the
//! same domain transition table and synced to the sub-order by
//! the same shared sync function as courier-reported ones.
//! Wave 20: deployment readiness — the permissive CORS layer
//! is replaced by an explicit origin list (`CORS_ORIGINS`,
//! defaulting to the local Next.js dev origin), `/health/ready`
//! is a real readiness probe (it pings the database; `/health`
//! stays the liveness check), the server shuts down gracefully
//! on Ctrl-C / SIGTERM, and startup logs one non-secret config
//! summary line. The deployable stack itself (Dockerfiles,
//! root `docker-compose.yml`, the CI `docker` job) lives at
//! the repository root — see the README Deployment section.
//! Wave 22: invoices (`invoices` module) — the per-seller
//! legal document for one sub-order, distinct from the Wave 4
//! buyer receipt: numbered sequentially PER STORE from a
//! counter row locked inside the issuing transaction, issued
//! lazily on the first entitled request for a delivered (or
//! later returned) sub-order and idempotently thereafter,
//! with the money block scoped to the sub-order's own stored
//! snapshots and any collected COD advance allocated by the
//! sub-order's share of the order grand total.
//! Wave 23: inventory & the stock audit trail (`inventory`
//! module) — from migration 0024 on, every change to a
//! product's stock writes a `stock_adjustments` row in the
//! same transaction: checkout decrements (system / `order`),
//! the shared restock helper under a caller-threaded reason
//! (`cancel_restock`, `return_restock`, `rto_restock`), and
//! the seller's own adjust-stock endpoint (seller actor, the
//! domain reason vocabulary). The product PATCH no longer
//! accepts a stock field, so no unaudited write path remains.
//! Sellers also get per-product low-stock thresholds (null =
//! the domain default), a stock-history view, an inventory
//! overview, and low-stock alerts on the dashboard.
//! Wave 24: wishlist + recommendations (`wishlist` and
//! `recommend` modules) — the signed-in user's server-side
//! saved-for-later list (idempotent add/remove; adding
//! requires the product to be publicly visible), and three
//! SQL-ranked discovery rails with documented rules and no
//! ML anywhere: related products (category +4, same store
//! +2, well-reviewed +1), also-bought (order-level
//! co-purchase across the split checkout's sub-orders,
//! counted in distinct orders, honestly empty when nobody
//! has bought the product), and a personalised rail fed by
//! the categories of the caller's delivered purchases and
//! wishlist items — with a labelled top-rated fallback
//! (`personalized: false`) for callers without signals.
//! Wave 25: product Q&A (`qna` module) — any signed-in user
//! may ask a question on a publicly visible product (never
//! their own store's), the store owner answers publicly
//! (re-answering corrects the answer), and both the owner
//! and admins can hide a question: hidden questions vanish
//! from the public list and from the dashboard's unanswered
//! count, but stay visible in the seller inbox so the hide
//! can be undone. The asker is shown only as a masked phone.
//! Wave 26: sponsored promotions (`promotions` module) — a
//! seller prepays whole days (1..=30) at the effective
//! per-day price from their EARNINGS BALANCE: the purchase
//! locks the payout-store row, reads the one shared earnings
//! aggregate inside its transaction, and writes the
//! promotion plus its `seller_charges` row together, so the
//! charge then gates payouts through the same available
//! balance. While the window is live the product view
//! carries `is_promoted` (an EXISTS in the shared product
//! columns implementing the domain activity rule), the
//! default and relevance orderings put it first — always
//! labelled স্পন্সরড, with explicit price/rating sorts kept
//! pure — and cancelling stops placement immediately with
//! NO refund: the charge stands.
//! Wave 27: analytics (`analytics` module) — seller and
//! platform numbers computed per request from the ledger
//! tables by single GROUP BY aggregates, over a window of
//! the last N days (domain-clamped to 7..=90) bucketed by
//! ASIA/DHAKA calendar days. Every metric's population and
//! money definition is written down in the module doc and
//! the README: seller goods exclude cancelled slices,
//! earnings are the delivered payout snapshots (the Wave 9
//! rule), platform GMV is goods − discount + delivery over
//! non-cancelled slices, and commission counts delivered
//! slices only. The seller daily series also exports as
//! CSV through the domain CSV builder.
//! Wave 28: security hardening + the admin audit log
//! (`audit` module) — OTP requests are throttled per phone
//! against the domain hourly cap (counted from the
//! append-only `otp_request_log`, since `otp_codes` keeps
//! only one row per phone); every response carries the
//! security header set and request bodies are capped at
//! 1 MiB in `build_app`; the process refuses to start
//! outside dev mode on a short or well-known JWT secret
//! (`validate_jwt_secret`, enforced in `run`); and every
//! consequential admin action — payout transitions, store
//! suspension, settings updates, verification decisions,
//! admin Q&A moderation, refund completion — writes an
//! audit row inside its own transaction via the one
//! writer in the `audit` module.
//! Wave 29: the notification outbox + a real worker (the
//! `notify` crate, the `notices` helpers, the
//! `notifications` admin view) — every state-changing
//! funnel (order placed, sub-order shipped / delivered /
//! cancelled / returned, payout paid, return updates)
//! enqueues an SMS row inside its own transaction via
//! `notify::enqueue`; the worker claims pending rows
//! `FOR UPDATE SKIP LOCKED`, sends them through the one
//! shared sender selection (`notify::sender_from_env` —
//! the log sender in dev mode, the honestly-unconfigured
//! sender otherwise), and retries each row up to 3
//! attempts before it stays `failed` in the admin view.
//! The `SmsSender` trait and its senders moved from
//! `auth` into the `notify` crate, so OTP codes and
//! notifications can never disagree about delivery.
//! Wave 30: automatic courier status polling (`shipments`
//! module) — the single-shipment refresh core is factored
//! into `refresh_shipment_by_id`, shared by the participant
//! refresh endpoint and by `refresh_stale_shipments`, which
//! the worker process runs on its poll cycle against
//! API-courier shipments that are stale and not in a
//! terminal status (manual shipments are never polled).
//! There is deliberately no HTTP endpoint for the batch
//! poll: the worker depends on this crate as a library and
//! calls the service in-process.
//! Wave 32: the performance pass (migration 0030, the
//! `CompressionLayer` in `build_app`, handler-level cache
//! headers) — four indexes for the query shapes that lacked
//! one (order_items by product, shipments by status +
//! updated_at, sub_orders by store + status, product
//! questions by product + hidden), gzip response compression
//! as the outermost layer, and `Cache-Control: public,
//! max-age=30` on the anonymous public catalogue reads
//! (product list, product detail, search). No payload
//! changed; authenticated responses never carry the public
//! cache header, because the catalogue payload itself is
//! viewer-dependent (owners see their own drafts).
//! Wave 33: product variants (`variants` module, migration
//! 0031) — a product gains purchasable variants (size,
//! colour, …), each priced as base + delta through the pure
//! domain rule (the final price must stay positive at every
//! write that could break it) and stocked independently:
//! while a product has an active variant, availability
//! lives on the variants — the product view's stock is
//! their sum, cart lines name the variant they buy, and
//! checkout decrements (and restocks restore) the variant
//! row, with the stock audit trail carrying the variant id.
//! Variant-less products behave exactly as before.
//! Wave 34: multi-store access + staff roles (`access`
//! module, migration 0032) — a store's owner seats managers
//! and staff as members, and every seller surface resolves
//! the caller's role through the one access helper and the
//! domain permission matrix (`store_role_can`): managers run
//! the shop but cannot seat members or move money out; staff
//! work catalogue, orders and questions only. Owners resolve
//! to `owner` and pass every gate, so owner behaviour is
//! unchanged everywhere; the payout store stays owner-based
//! by design (money settles to the owner alone).
//! Wave 35: buyer loyalty points (`loyalty` module,
//! migration 0033) — an append-only points ledger: a
//! delivered sub-order earns the buyer 1 point per 100 taka
//! of net goods (through the same two delivery funnels as
//! the Wave 29 notices), checkout redeems points at 1 point
//! = 100 poisha capped at half the post-coupon goods, and
//! cancels and received returns reverse their slice. The
//! platform funds redemptions: seller payout snapshots and
//! the commission basis never see the discount, and the
//! stored grand total is the reduced payable every money
//! surface reads.
//! Wave 36: seller subscription plans (`subscriptions`
//! module, migration 0034) — a store's plan (free / pro /
//! business) is prepaid from the seller's earnings balance
//! for 30-day periods through the same `seller_charges`
//! ledger as promotions; a live paid plan discounts that
//! store's checkout commission (resolved per seller group,
//! so a split order can charge each store its own rate) and
//! raises its product cap. Renewals and expiry are the
//! worker's sweep (`process_subscriptions`), not an
//! endpoint: auto-renew charges extend from the old expiry,
//! and a period the balance cannot cover lapses the store
//! back to free.
//! Wave 37: third-party / mobile API keys (`api_keys`
//! module, migration 0035) — a user mints long-lived
//! `bdm_live_...` bearer keys for external apps; only the
//! SHA-256 hash is stored and the full key is shown exactly
//! once. The `AuthUser` extractor routes a key token to the
//! one central key check: hash lookup (revoked / expired →
//! 401), the JWT-only route exclusions (admin, payout
//! account + payout requests, subscription spend, key
//! management itself, data exports → 403), the domain scope
//! rule (read-only keys cannot write → 403), and a per-key
//! fixed-window rate limit held in `AppState` (default 600
//! requests/minute, `API_KEY_RATE_LIMIT_PER_MIN` overrides;
//! JWT traffic is never limited by it).
//! Wave 51: product media (`media` and `product_images`
//! modules, migration 0040) — the media system the
//! storefront parity program stands on: a `MediaStore`
//! trait with a local-disk driver (default; this API
//! serves the bytes at `/media/*`) and an S3-compatible
//! SigV4 driver (MinIO-first; honestly `media_not_configured`
//! without its credential set), one selection rule from
//! the environment, and one processing pipeline (domain
//! sniff + size rules, decode, metadata-stripping
//! re-encode, derived WebP widths that never upscale).
//! Products carry up to 12 images — uploads or external
//! hotlinks — with exactly one primary; every product
//! view gains the primary's `image_url`, the detail gains
//! the full gallery, variants gain their own `image_url`,
//! and cart lines carry the product thumbnail. Sellers
//! manage galleries (upload, attach-by-URL, batch reorder,
//! delete-with-promotion), a cross-store media library
//! with bulk delete, and store branding uploads; buyers
//! attach one photo to their own reviews (migration
//! 0041); the Facebook import attaches each post's photo
//! URL best-effort; admin metrics report the library's
//! footprint.
//! Wave 43: observability (`observability` and `metrics`
//! modules) — the outermost middleware honours or mints an
//! `X-Request-Id` per request (the pure validation rule is
//! the domain's `validate_request_id`), echoes it on the
//! response, and writes ONE structured log line per request
//! (method, matched route pattern, status, latency — never
//! query strings, headers or bodies: the tracking endpoint's
//! query carries phone numbers). `GET /v1/admin/metrics`
//! answers "is the platform healthy right now?" with
//! point-in-time aggregates: today's orders and GMV (the
//! Wave 27 formula), orders by status, the pending payout
//! queue, open returns, the notification outbox backlog,
//! stale shipments, and catalogue totals.
//! Handlers only parse/validate,
//! call ONE service function, and return its result — business rules
//! live in the `domain` crate (pure functions) or in the service
//! functions in the per-resource modules, never inside handlers.
//!
//! Database behaviour: when `DATABASE_URL` is set at startup the API
//! connects and runs the workspace migrations automatically. When it is
//! NOT set the API still starts and serves the Wave 1 stateless
//! endpoints; DB-backed endpoints return a clear 503 instead of panicking.

pub mod access;
pub mod admin;
pub mod analytics;
pub mod api_keys;
pub mod audit;
pub mod auth;
pub mod cart;
pub mod coupons;
pub mod exports;
pub mod facebook;
pub mod fulfillment;
pub mod inventory;
pub mod invoices;
pub mod loyalty;
pub mod media;
pub mod merch;
pub mod messaging;
pub mod metrics;
pub mod notices;
pub mod notifications;
pub mod observability;
pub mod orders;
pub mod payments;
pub mod payouts;
pub mod product_images;
pub mod products;
pub mod promotions;
pub mod qna;
pub mod recommend;
pub mod returns;
pub mod reviews;
pub mod risk;
pub mod search;
pub mod shipments;
pub mod stores;
pub mod subscriptions;
pub mod tracking;
pub mod variants;
pub mod verification;
pub mod wishlist;

use axum::{
    extract::{DefaultBodyLimit, Request},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use ::courier::{
    CourierConfig, CouriersConfig, EcourierConfig, PaperflyConfig, PathaoConfig, RedxConfig,
};
use domain::{
    calculate_delivery_charge, compute_commission, split_cart_by_seller, CartItem,
    CommissionQuote, DeliveryZone, SellerSubOrder,
};
use ::payments::{GatewayConfig, PaymentMethod, PaymentsConfig};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};

/// Shared application state. `pool` is `None` when the API was started
/// without `DATABASE_URL` — DB endpoints must check via `require_pool`.
#[derive(Clone)]
pub struct AppState {
    pub pool: Option<PgPool>,
    /// HMAC secret for signing JWTs.
    pub jwt_secret: String,
    /// True only in explicit dev mode (`APP_ENV=dev` or `SMS_DEV_MODE=true`).
    /// Gates the log SMS sender and the `dev_otp` response field.
    pub sms_dev_mode: bool,
    /// Platform commission rate in basis points (800 = 8%). Read once at
    /// startup from `COMMISSION_RATE_BPS` (see `from_env`). This is
    /// the FALLBACK rate: checkout asks `admin::effective_commission_bps`
    /// for the rate to pass to the domain `build_order_totals`
    /// function, and an admin-set platform setting (Wave 10) wins
    /// over this value.
    pub commission_rate_bps: u32,
    /// COD advance rate in basis points (2,000 = 20%), read once
    /// at startup from `ADVANCE_PERCENT_BPS` (see `from_env`).
    /// This is the FALLBACK rate: checkout asks
    /// `admin::effective_advance_bps` for the rate to use, and an
    /// admin-set platform setting (Wave 17) wins over this value.
    pub advance_percent_bps: u32,
    /// COD advance threshold in poisha (200,000 = 2,000 taka),
    /// read once at startup from `ADVANCE_THRESHOLD_POISHA`. A
    /// COD order at or above this grand total must collect an
    /// advance before shipping; 0 disables threshold-based
    /// advances (risk-forced advances still apply). The FALLBACK
    /// value: `admin::effective_advance_threshold` wins over it.
    pub advance_threshold_poisha: i64,
    /// Sponsored-promotion price per day in poisha (10,000 =
    /// 100 taka), read once at startup from
    /// `PROMOTION_PRICE_PER_DAY_POISHA` (Wave 26). The FALLBACK
    /// value: `admin::effective_promotion_price_per_day` wins
    /// over it, and it prices NEW promotions only.
    pub promotion_price_per_day_poisha: i64,
    /// Pro subscription price per 30-day period in poisha
    /// (49,900 = 499 taka), read once at startup from
    /// `SUBSCRIPTION_PRICE_PRO_POISHA` (Wave 36). The FALLBACK
    /// value: `admin::effective_subscription_price` wins over
    /// it, and it prices NEW subscriptions and renewals only.
    pub subscription_price_pro_poisha: i64,
    /// Business subscription price per 30-day period in poisha
    /// (149,900 = 1,499 taka), read once at startup from
    /// `SUBSCRIPTION_PRICE_BUSINESS_POISHA` (Wave 36). The
    /// FALLBACK value: see
    /// [`AppState::subscription_price_pro_poisha`].
    pub subscription_price_business_poisha: i64,
    /// The free plan's product cap, read once at startup from
    /// `PLAN_FREE_PRODUCT_CAP` (Wave 36). The FALLBACK value:
    /// `admin::effective_plan_free_product_cap` wins over it.
    /// Only the free cap is tunable; the paid caps are domain
    /// constants (part of what the plans sell).
    pub plan_free_product_cap: i64,
    /// Payment configuration (Wave 5): the DEV simulator switch and
    /// secret, plus one merchant credential set per gateway. Built
    /// from the environment in `from_env`; tests set it explicitly
    /// via `with_payments_config`.
    pub payments_config: PaymentsConfig,
    /// Courier configuration (Wave 6, extended Waves 7 and 11):
    /// the DEV simulator switch, one merchant credential set per
    /// courier (Steadfast, Pathao, RedX, eCourier, Paperfly), and
    /// the default courier. Built from the environment in
    /// `from_env`; tests set it explicitly via
    /// `with_couriers_config`.
    pub couriers_config: CouriersConfig,
    /// Facebook configuration (Wave 12): only the PRESENCE of the
    /// platform's Meta app credentials. The credential values
    /// themselves are never stored on state, logged or returned by
    /// any endpoint; presence alone drives the `graph_api` field
    /// of the seller Facebook status.
    pub facebook_config: FacebookConfig,
    /// SIDRA bridge configuration (Wave 13): the external SIDRA
    /// service's notify URL and the shared webhook secret. The
    /// secret is held in memory only — never stored in the
    /// database, logged or returned by any endpoint. The bridge
    /// is `configured` only when BOTH values are present; without
    /// them messaging works as a plain human inbox and the SIDRA
    /// callback endpoint answers 503.
    pub sidra_config: SidraConfig,
    /// Browser origins allowed to call the API (Wave 20). Read
    /// from `CORS_ORIGINS` (comma-separated) in `from_env`; when
    /// the variable is unset the list is just the local Next.js
    /// dev origin ([`DEFAULT_CORS_ORIGIN`]). `build_app` turns
    /// the list into the router's CORS layer — preflight answers
    /// name exactly these origins, no wildcard.
    pub cors_origins: Vec<String>,
    /// Per-API-key rate limit, in requests per minute
    /// (Wave 37). Read once at startup from
    /// `API_KEY_RATE_LIMIT_PER_MIN`; invalid or zero values
    /// keep the default
    /// ([`api_keys::DEFAULT_API_KEY_RATE_LIMIT_PER_MIN`]).
    /// Applies ONLY to key-authenticated traffic — JWT
    /// sessions are never limited by it.
    pub api_key_rate_limit_per_min: u32,
    /// The rate limiter itself (Wave 37): per-key fixed
    /// windows, in memory, behind an `Arc` so every clone of
    /// the state shares the one counter set. See the
    /// `api_keys` module for the honest limits of an
    /// in-memory limiter.
    pub api_key_rate_limiter: std::sync::Arc<api_keys::ApiKeyRateLimiter>,
    /// Fulfillment SLA for confirming an order, in hours
    /// (Wave 40), read once at startup from
    /// `FULFILLMENT_CONFIRM_SLA_HOURS`. The FALLBACK value:
    /// `fulfillment::effective_fulfillment_sla` wins over it
    /// with a stored platform setting.
    pub fulfillment_confirm_sla_hours: i64,
    /// Fulfillment SLA for booking a courier after
    /// confirmation, in hours (Wave 40), read once at startup
    /// from `FULFILLMENT_SHIP_SLA_HOURS`. The FALLBACK value:
    /// see [`AppState::fulfillment_confirm_sla_hours`].
    pub fulfillment_ship_sla_hours: i64,
    /// The media store (Wave 51): where product / review /
    /// branding image objects live. Selected ONCE from the
    /// environment by `media::store_from_env` (the local
    /// disk driver unless `MEDIA_STORE=s3`), behind an `Arc`
    /// so every clone of the state shares the one store.
    pub media_store: std::sync::Arc<dyn media::MediaStore>,
}

/// SIDRA bridge configuration (Wave 13). See [`AppState::sidra_config`].
#[derive(Debug, Clone, Default)]
pub struct SidraConfig {
    /// Where buyer-message notifications are POSTed.
    pub api_url: Option<String>,
    /// Shared secret for the bridge's HMAC signatures (both the
    /// outbound notify header and the inbound callback body).
    pub webhook_secret: Option<String>,
}

impl SidraConfig {
    /// The bridge is live only with a URL to notify AND a secret
    /// to sign with; half a configuration is no configuration.
    pub fn configured(&self) -> bool {
        self.api_url.is_some() && self.webhook_secret.is_some()
    }
}

/// Facebook app credential presence (Wave 12). The Graph API
/// reports `configured` only when BOTH the app id and the app
/// secret are present; the page-token OAuth flow that would make
/// the credentials usable lands in a later wave.
#[derive(Debug, Clone, Copy, Default)]
pub struct FacebookConfig {
    pub app_id_present: bool,
    pub app_secret_present: bool,
}

impl FacebookConfig {
    pub fn graph_api_configured(&self) -> bool {
        self.app_id_present && self.app_secret_present
    }
}

/// Default commission rate: 8% (matches the MVP plan's worked example).
pub const DEFAULT_COMMISSION_RATE_BPS: u32 = 800;

/// Default COD advance rate: 20% of the grand total (Wave 17).
pub const DEFAULT_ADVANCE_PERCENT_BPS: u32 = 2_000;

/// Default COD advance threshold: 2,000 taka (Wave 17). COD orders
/// at or above this grand total collect an advance before shipping.
pub const DEFAULT_ADVANCE_THRESHOLD_POISHA: i64 = 200_000;

/// Default sponsored-promotion price: 100 taka per day
/// (Wave 26) — the domain constant, re-exported here so the
/// startup defaults read in one place.
pub const DEFAULT_PROMOTION_PRICE_PER_DAY_POISHA: i64 =
    domain::DEFAULT_PROMOTION_PRICE_PER_DAY_POISHA;

/// Default pro subscription price: 499 taka per 30-day period
/// (Wave 36) — the domain constant, re-exported like the
/// promotion default above.
pub const DEFAULT_SUBSCRIPTION_PRICE_PRO_POISHA: i64 =
    domain::DEFAULT_SUBSCRIPTION_PRICE_PRO_POISHA;

/// Default business subscription price: 1,499 taka per 30-day
/// period (Wave 36). See [`DEFAULT_SUBSCRIPTION_PRICE_PRO_POISHA`].
pub const DEFAULT_SUBSCRIPTION_PRICE_BUSINESS_POISHA: i64 =
    domain::DEFAULT_SUBSCRIPTION_PRICE_BUSINESS_POISHA;

/// Default free-plan product cap: 50 products (Wave 36) — the
/// domain constant, re-exported like the defaults above.
pub const DEFAULT_PLAN_FREE_PRODUCT_CAP: i64 = domain::DEFAULT_FREE_PLAN_PRODUCT_CAP;

/// Default fulfillment confirm SLA: 24 hours (Wave 40) — the
/// domain constant, re-exported like the defaults above.
pub const DEFAULT_FULFILLMENT_CONFIRM_SLA_HOURS: i64 =
    domain::DEFAULT_FULFILLMENT_CONFIRM_SLA_HOURS;

/// Default fulfillment ship SLA: 48 hours (Wave 40). See
/// [`DEFAULT_FULFILLMENT_CONFIRM_SLA_HOURS`].
pub const DEFAULT_FULFILLMENT_SHIP_SLA_HOURS: i64 = domain::DEFAULT_FULFILLMENT_SHIP_SLA_HOURS;

/// The one origin allowed by CORS when `CORS_ORIGINS` is not set
/// (Wave 20): the local Next.js dev server. A real deployment
/// sets `CORS_ORIGINS` to its deployed web origin(s).
pub const DEFAULT_CORS_ORIGIN: &str = "http://localhost:3000";

/// The well-known DEV-ONLY JWT secret `AppState::from_env`
/// falls back to when `JWT_SECRET` is unset. It is public by
/// definition — it is printed in this source file — so anyone
/// can forge tokens signed with it; [`validate_jwt_secret`]
/// refuses it outside dev mode.
pub const DEV_JWT_SECRET: &str = "dev-only-insecure-jwt-secret-change-me";

/// Minimum length of a production JWT secret (Wave 28).
pub const MIN_JWT_SECRET_CHARS: usize = 32;

/// Maximum request body the router accepts (Wave 28): 1 MiB.
/// Every current payload is far below this (the largest, the
/// Facebook post import, caps its own post count well under
/// it); the cap exists so a client cannot stream unbounded
/// bodies at the JSON extractors.
pub const MAX_REQUEST_BODY_BYTES: usize = 1_048_576;

/// May the API start with this JWT secret (Wave 28)? In dev
/// mode anything goes — local development must not need a
/// real secret, and dev mode already gates the log SMS sender
/// and the `dev_otp` field. Outside dev mode the secret must
/// be at least [`MIN_JWT_SECRET_CHARS`] characters and must
/// not be the well-known [`DEV_JWT_SECRET`] fallback: starting
/// anyway would mean every token the API issues can be forged
/// by anyone who has read this source file. The error names
/// the problem and the required length; it never echoes the
/// secret itself.
pub fn validate_jwt_secret(secret: &str, dev_mode: bool) -> Result<(), String> {
    if dev_mode {
        return Ok(());
    }
    if secret == DEV_JWT_SECRET {
        return Err(
            "JWT_SECRET is not set (the dev-only default secret is in use) — \
             set a real JWT_SECRET before running outside dev mode"
                .to_string(),
        );
    }
    if secret.chars().count() < MIN_JWT_SECRET_CHARS {
        return Err(format!(
            "JWT_SECRET is too short ({} characters) — use at least \
             {MIN_JWT_SECRET_CHARS} characters outside dev mode",
            secret.chars().count()
        ));
    }
    Ok(())
}

impl AppState {
    pub fn new(pool: Option<PgPool>, jwt_secret: String, sms_dev_mode: bool) -> Self {
        AppState {
            pool,
            jwt_secret,
            sms_dev_mode,
            commission_rate_bps: DEFAULT_COMMISSION_RATE_BPS,
            advance_percent_bps: DEFAULT_ADVANCE_PERCENT_BPS,
            advance_threshold_poisha: DEFAULT_ADVANCE_THRESHOLD_POISHA,
            promotion_price_per_day_poisha: DEFAULT_PROMOTION_PRICE_PER_DAY_POISHA,
            subscription_price_pro_poisha: DEFAULT_SUBSCRIPTION_PRICE_PRO_POISHA,
            subscription_price_business_poisha: DEFAULT_SUBSCRIPTION_PRICE_BUSINESS_POISHA,
            plan_free_product_cap: DEFAULT_PLAN_FREE_PRODUCT_CAP,
            payments_config: PaymentsConfig::default(),
            couriers_config: CouriersConfig::default(),
            facebook_config: FacebookConfig::default(),
            sidra_config: SidraConfig::default(),
            cors_origins: vec![DEFAULT_CORS_ORIGIN.to_string()],
            api_key_rate_limit_per_min: api_keys::DEFAULT_API_KEY_RATE_LIMIT_PER_MIN,
            api_key_rate_limiter: std::sync::Arc::new(api_keys::ApiKeyRateLimiter::new()),
            fulfillment_confirm_sla_hours: DEFAULT_FULFILLMENT_CONFIRM_SLA_HOURS,
            fulfillment_ship_sla_hours: DEFAULT_FULFILLMENT_SHIP_SLA_HOURS,
            media_store: media::store_from_env(),
        }
    }

    /// Replace the media store (Wave 51; used by tests that
    /// need a specific driver — an unconfigured S3 store, or
    /// a local store rooted in a scratch directory).
    pub fn with_media_store(
        mut self,
        media_store: std::sync::Arc<dyn media::MediaStore>,
    ) -> Self {
        self.media_store = media_store;
        self
    }

    /// Replace the payments configuration (used by `from_env` and by
    /// tests that need the DEV payment simulator on or off).
    pub fn with_payments_config(mut self, payments_config: PaymentsConfig) -> Self {
        self.payments_config = payments_config;
        self
    }

    /// Replace the couriers configuration (used by `from_env` and by
    /// tests that need the DEV courier simulator on or off).
    pub fn with_couriers_config(mut self, couriers_config: CouriersConfig) -> Self {
        self.couriers_config = couriers_config;
        self
    }

    /// Replace the Facebook configuration (used by `from_env` and
    /// by tests that need the Graph API to report `configured`).
    pub fn with_facebook_config(mut self, facebook_config: FacebookConfig) -> Self {
        self.facebook_config = facebook_config;
        self
    }

    /// Replace the SIDRA bridge configuration (used by `from_env`
    /// and by tests that need the bridge configured with a known
    /// secret, or deliberately unconfigured).
    pub fn with_sidra_config(mut self, sidra_config: SidraConfig) -> Self {
        self.sidra_config = sidra_config;
        self
    }

    /// Replace the CORS origin list (used by `from_env` and by
    /// tests that need a specific deployed origin). The list
    /// REPLACES the default; it is never merged with it.
    pub fn with_cors_origins(mut self, cors_origins: Vec<String>) -> Self {
        self.cors_origins = cors_origins;
        self
    }

    /// Replace the per-key rate limit (used by `from_env` and
    /// by tests that need a tiny limit to exercise the 429
    /// path). The limiter itself is NOT replaced — a fresh
    /// state already carries a fresh limiter.
    pub fn with_api_key_rate_limit_per_min(mut self, limit: u32) -> Self {
        self.api_key_rate_limit_per_min = limit;
        self
    }

    /// Build state from environment variables around an optional pool.
    ///
    /// WARNING: if `JWT_SECRET` is not set we fall back to a well-known
    /// DEV-ONLY secret and log a loud warning. Never run production with
    /// the fallback — anyone can forge tokens signed with a public secret.
    /// Since Wave 28 this is also enforced: `run` validates the
    /// resulting secret with [`validate_jwt_secret`] and refuses
    /// to start outside dev mode when it fails, so the fallback
    /// can only ever serve local development.
    pub fn from_env(pool: Option<PgPool>) -> Self {
        let jwt_secret = match std::env::var("JWT_SECRET") {
            Ok(secret) if !secret.is_empty() => secret,
            _ => {
                eprintln!(
                    "WARNING: JWT_SECRET is not set — using the DEV-ONLY default secret. \
                     Tokens can be forged; set JWT_SECRET before any real deployment."
                );
                DEV_JWT_SECRET.to_string()
            }
        };
        let sms_dev_mode = std::env::var("APP_ENV").map(|v| v == "dev").unwrap_or(false)
            || std::env::var("SMS_DEV_MODE").map(|v| v == "true").unwrap_or(false);
        let mut state = AppState::new(pool, jwt_secret, sms_dev_mode);
        // COMMISSION_RATE_BPS is read once, here at startup; invalid or
        // out-of-range values keep the default rather than breaking money
        // maths later.
        if let Ok(raw) = std::env::var("COMMISSION_RATE_BPS") {
            match raw.parse::<u32>() {
                Ok(rate) if rate <= 10_000 => state.commission_rate_bps = rate,
                _ => eprintln!(
                    "WARNING: COMMISSION_RATE_BPS='{raw}' is invalid — \
                     using the default {DEFAULT_COMMISSION_RATE_BPS} bps"
                ),
            }
        }
        // ADVANCE_PERCENT_BPS / ADVANCE_THRESHOLD_POISHA are read
        // once, here at startup, under the same discipline as the
        // commission rate: invalid or out-of-range values keep the
        // default rather than breaking checkout maths later. Admin
        // platform settings (Wave 17) override both at read time.
        if let Ok(raw) = std::env::var("ADVANCE_PERCENT_BPS") {
            match raw.parse::<i64>() {
                Ok(rate) if domain::validate_advance_bps(rate) => {
                    state.advance_percent_bps = rate as u32
                }
                _ => eprintln!(
                    "WARNING: ADVANCE_PERCENT_BPS='{raw}' is invalid — \
                     using the default {DEFAULT_ADVANCE_PERCENT_BPS} bps"
                ),
            }
        }
        if let Ok(raw) = std::env::var("ADVANCE_THRESHOLD_POISHA") {
            match raw.parse::<i64>() {
                Ok(threshold) if domain::validate_advance_threshold(threshold) => {
                    state.advance_threshold_poisha = threshold
                }
                _ => eprintln!(
                    "WARNING: ADVANCE_THRESHOLD_POISHA='{raw}' is invalid — \
                     using the default {DEFAULT_ADVANCE_THRESHOLD_POISHA} poisha"
                ),
            }
        }
        // PROMOTION_PRICE_PER_DAY_POISHA is read once, here at
        // startup, under the same discipline: an invalid or
        // out-of-range value keeps the default rather than
        // breaking the promotion price later. An admin platform
        // setting (Wave 26) overrides it at read time.
        if let Ok(raw) = std::env::var("PROMOTION_PRICE_PER_DAY_POISHA") {
            match raw.parse::<i64>() {
                Ok(price) if domain::validate_promotion_price_per_day(price) => {
                    state.promotion_price_per_day_poisha = price
                }
                _ => eprintln!(
                    "WARNING: PROMOTION_PRICE_PER_DAY_POISHA='{raw}' is invalid — \
                     using the default {DEFAULT_PROMOTION_PRICE_PER_DAY_POISHA} poisha"
                ),
            }
        }
        // SUBSCRIPTION_PRICE_PRO_POISHA /
        // SUBSCRIPTION_PRICE_BUSINESS_POISHA /
        // PLAN_FREE_PRODUCT_CAP are read once, here at startup,
        // under the same discipline as the promotion price: an
        // invalid or out-of-range value keeps the default
        // rather than breaking plan pricing later. Admin
        // platform settings (Wave 36) override all three at
        // read time.
        if let Ok(raw) = std::env::var("SUBSCRIPTION_PRICE_PRO_POISHA") {
            match raw.parse::<i64>() {
                Ok(price) if domain::validate_subscription_price(price) => {
                    state.subscription_price_pro_poisha = price
                }
                _ => eprintln!(
                    "WARNING: SUBSCRIPTION_PRICE_PRO_POISHA='{raw}' is invalid — \
                     using the default {DEFAULT_SUBSCRIPTION_PRICE_PRO_POISHA} poisha"
                ),
            }
        }
        if let Ok(raw) = std::env::var("SUBSCRIPTION_PRICE_BUSINESS_POISHA") {
            match raw.parse::<i64>() {
                Ok(price) if domain::validate_subscription_price(price) => {
                    state.subscription_price_business_poisha = price
                }
                _ => eprintln!(
                    "WARNING: SUBSCRIPTION_PRICE_BUSINESS_POISHA='{raw}' is invalid — \
                     using the default {DEFAULT_SUBSCRIPTION_PRICE_BUSINESS_POISHA} poisha"
                ),
            }
        }
        if let Ok(raw) = std::env::var("PLAN_FREE_PRODUCT_CAP") {
            match raw.parse::<i64>() {
                Ok(cap) if domain::validate_plan_free_product_cap(cap) => {
                    state.plan_free_product_cap = cap
                }
                _ => eprintln!(
                    "WARNING: PLAN_FREE_PRODUCT_CAP='{raw}' is invalid — \
                     using the default {DEFAULT_PLAN_FREE_PRODUCT_CAP}"
                ),
            }
        }
        // API_KEY_RATE_LIMIT_PER_MIN is read once, here at
        // startup, under the same discipline (Wave 37): a
        // positive integer wins; anything else (zero,
        // negative, non-numeric) keeps the default rather
        // than silently disabling or zeroing the limit.
        if let Ok(raw) = std::env::var("API_KEY_RATE_LIMIT_PER_MIN") {
            match raw.parse::<u32>() {
                Ok(limit) if limit >= 1 => state.api_key_rate_limit_per_min = limit,
                _ => eprintln!(
                    "WARNING: API_KEY_RATE_LIMIT_PER_MIN='{raw}' is invalid — \
                     using the default {} requests/minute",
                    api_keys::DEFAULT_API_KEY_RATE_LIMIT_PER_MIN
                ),
            }
        }
        // FULFILLMENT_CONFIRM_SLA_HOURS / FULFILLMENT_SHIP_SLA_HOURS
        // are read once, here at startup, under the same
        // discipline (Wave 40): a value inside the domain's
        // legal range wins; anything else keeps the default
        // rather than flagging every queued order breached (or
        // none). Admin platform settings override both at read
        // time.
        if let Ok(raw) = std::env::var("FULFILLMENT_CONFIRM_SLA_HOURS") {
            match raw.parse::<i64>() {
                Ok(hours) if domain::validate_fulfillment_sla_hours(hours) => {
                    state.fulfillment_confirm_sla_hours = hours
                }
                _ => eprintln!(
                    "WARNING: FULFILLMENT_CONFIRM_SLA_HOURS='{raw}' is invalid — \
                     using the default {DEFAULT_FULFILLMENT_CONFIRM_SLA_HOURS} hours"
                ),
            }
        }
        if let Ok(raw) = std::env::var("FULFILLMENT_SHIP_SLA_HOURS") {
            match raw.parse::<i64>() {
                Ok(hours) if domain::validate_fulfillment_sla_hours(hours) => {
                    state.fulfillment_ship_sla_hours = hours
                }
                _ => eprintln!(
                    "WARNING: FULFILLMENT_SHIP_SLA_HOURS='{raw}' is invalid — \
                     using the default {DEFAULT_FULFILLMENT_SHIP_SLA_HOURS} hours"
                ),
            }
        }
        state.payments_config = payments_config_from_env();
        state.couriers_config = couriers_config_from_env();
        state.facebook_config = facebook_config_from_env();
        state.sidra_config = sidra_config_from_env();
        state.cors_origins = cors_origins_from_env();
        state
    }

    /// The pool, or a clear 503 when the API runs without a database.
    pub fn require_pool(&self) -> Result<&PgPool, ApiError> {
        self.pool
            .as_ref()
            .ok_or_else(|| ApiError::service_unavailable("database not configured"))
    }
}

/// One error type for the whole API: an HTTP status + a message, rendered
/// as `{"error": "..."}`. Service functions return this; handlers never
/// build ad-hoc error responses.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        ApiError {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::UNAUTHORIZED, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::FORBIDDEN, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::CONFLICT, message)
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }

    /// Map a SQLx error to a 500 without leaking internals to clients.
    pub fn internal(context: &str, err: sqlx::Error) -> Self {
        eprintln!("database error in {context}: {err}");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("database error ({context})"),
        )
    }

    /// Map any other internal failure (IO, HTTP client setup, …) to a
    /// 500 without leaking internals to clients (Wave 51).
    pub fn internal_any(context: &str, err: impl std::fmt::Display) -> Self {
        eprintln!("internal error in {context}: {err}");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("internal error ({context})"),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({ "error": self.message }));
        (self.status, body).into_response()
    }
}

/// A non-empty environment variable, or `None`. Credential values
/// are only ever held in memory inside `PaymentsConfig` — never
/// logged, never returned by any endpoint.
fn env_value(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

/// Generate an ephemeral random secret (48 alphanumeric chars from
/// the OS CSPRNG — the same `rand` sampling the API-key generator
/// uses). Wave 39 (security audit CR-01): the DEV payment
/// simulator's HMAC secret must never be a known value, so when
/// `PAYMENTS_DEV_SECRET` is unset the API invents one per process
/// instead of falling back to a public default.
fn generate_ephemeral_secret() -> String {
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}

/// Build the Wave 5 payments configuration from the environment:
/// `PAYMENTS_DEV_MODE` gates the DEV simulator (with a loud startup
/// warning — it must never be on in production), `PAYMENTS_DEV_SECRET`
/// is its HMAC secret (Wave 39: NO public default exists — when dev
/// mode is on and the variable is unset, an ephemeral random secret
/// is generated for this process and a warning says so), and each
/// gateway's merchant credentials come from its `*_APP_KEY` /
/// `*_MERCHANT_ID` style variables (see `.env.example` for the full
/// list per gateway).
fn payments_config_from_env() -> PaymentsConfig {
    let dev_mode = std::env::var("PAYMENTS_DEV_MODE")
        .map(|v| v == "true")
        .unwrap_or(false);
    let dev_secret = match env_value("PAYMENTS_DEV_SECRET") {
        Some(secret) => secret,
        None if dev_mode => {
            eprintln!(
                "WARNING: PAYMENTS_DEV_SECRET is not set — generated an \
                 EPHEMERAL random DEV callback secret for this process. \
                 It changes on every restart, so dev-simulated payments \
                 cannot be settled across restarts. Set PAYMENTS_DEV_SECRET \
                 to a stable random value for local development."
            );
            generate_ephemeral_secret()
        }
        None => String::new(),
    };
    if dev_mode {
        eprintln!(
            "WARNING: PAYMENTS_DEV_MODE is ON — the DEV payment simulator is \
             enabled. Simulated payments are not real money. NEVER enable \
             this in production."
        );
    }
    PaymentsConfig {
        dev_mode,
        dev_secret,
        bkash: GatewayConfig {
            merchant_id: None,
            api_key: env_value("BKASH_APP_KEY"),
            secret: env_value("BKASH_APP_SECRET"),
        },
        nagad: GatewayConfig {
            merchant_id: env_value("NAGAD_MERCHANT_ID"),
            api_key: env_value("NAGAD_API_KEY"),
            secret: None,
        },
        sslcommerz: GatewayConfig {
            merchant_id: env_value("SSLCOMMERZ_STORE_ID"),
            api_key: None,
            secret: env_value("SSLCOMMERZ_STORE_PASSWORD"),
        },
        shurjopay: GatewayConfig {
            merchant_id: env_value("SHURJOPAY_USERNAME"),
            api_key: None,
            secret: env_value("SHURJOPAY_PASSWORD"),
        },
    }
}

/// Build the couriers configuration from the environment:
/// `COURIER_DEV_MODE` gates the DEV courier simulator (with a loud
/// startup warning — it must never be on in production),
/// `STEADFAST_API_KEY` / `STEADFAST_SECRET_KEY` are the platform's
/// Steadfast merchant credentials, `STEADFAST_BASE_URL` overrides
/// the production API base URL (sandbox/testing), and
/// `DEFAULT_COURIER` names the courier sellers get by default.
/// Wave 7 adds Pathao (`PATHAO_CLIENT_ID` / `PATHAO_CLIENT_SECRET`
/// / `PATHAO_STORE_ID`, `PATHAO_BASE_URL` overrides the Hermes API
/// base URL) and RedX (`REDX_API_KEY`, `REDX_BASE_URL` overrides
/// the open-API base URL). Wave 11 adds eCourier
/// (`ECOURIER_API_KEY` / `ECOURIER_USER_ID`, `ECOURIER_BASE_URL`
/// overrides the backoffice API base URL) and Paperfly
/// (`PAPERFLY_USERNAME` / `PAPERFLY_PASSWORD` / `PAPERFLY_KEY`,
/// plus the platform pickup point `PAPERFLY_PICKUP_NAME` /
/// `PAPERFLY_PICKUP_PHONE` / `PAPERFLY_PICKUP_ADDRESS`;
/// `PAPERFLY_BASE_URL` overrides the production API base URL).
fn couriers_config_from_env() -> CouriersConfig {
    let dev_mode = std::env::var("COURIER_DEV_MODE")
        .map(|v| v == "true")
        .unwrap_or(false);
    if dev_mode {
        eprintln!(
            "WARNING: COURIER_DEV_MODE is ON — the DEV courier simulator is \
             enabled. Simulated shipments are not real bookings. NEVER \
             enable this in production."
        );
    }
    CouriersConfig {
        dev_mode,
        steadfast: CourierConfig {
            api_key: env_value("STEADFAST_API_KEY"),
            secret_key: env_value("STEADFAST_SECRET_KEY"),
            base_url: env_value("STEADFAST_BASE_URL")
                .unwrap_or_else(|| ::courier::DEFAULT_STEADFAST_BASE_URL.to_string()),
        },
        pathao: PathaoConfig {
            client_id: env_value("PATHAO_CLIENT_ID"),
            client_secret: env_value("PATHAO_CLIENT_SECRET"),
            store_id: env_value("PATHAO_STORE_ID"),
            base_url: env_value("PATHAO_BASE_URL")
                .unwrap_or_else(|| ::courier::DEFAULT_PATHAO_BASE_URL.to_string()),
        },
        redx: RedxConfig {
            api_key: env_value("REDX_API_KEY"),
            base_url: env_value("REDX_BASE_URL")
                .unwrap_or_else(|| ::courier::DEFAULT_REDX_BASE_URL.to_string()),
        },
        ecourier: EcourierConfig {
            api_key: env_value("ECOURIER_API_KEY"),
            user_id: env_value("ECOURIER_USER_ID"),
            base_url: env_value("ECOURIER_BASE_URL")
                .unwrap_or_else(|| ::courier::DEFAULT_ECOURIER_BASE_URL.to_string()),
        },
        paperfly: PaperflyConfig {
            username: env_value("PAPERFLY_USERNAME"),
            password: env_value("PAPERFLY_PASSWORD"),
            merchant_id: env_value("PAPERFLY_KEY"),
            pickup_name: env_value("PAPERFLY_PICKUP_NAME"),
            pickup_phone: env_value("PAPERFLY_PICKUP_PHONE"),
            pickup_address: env_value("PAPERFLY_PICKUP_ADDRESS"),
            base_url: env_value("PAPERFLY_BASE_URL")
                .unwrap_or_else(|| ::courier::DEFAULT_PAPERFLY_BASE_URL.to_string()),
        },
        default_courier: env_value("DEFAULT_COURIER")
            .unwrap_or_else(|| "steadfast".to_string()),
    }
}

/// Build the Wave 12 Facebook configuration from the environment:
/// only whether `FACEBOOK_APP_ID` and `FACEBOOK_APP_SECRET` are
/// present. The values are read here solely for that presence
/// check and are dropped immediately — never stored, never logged.
fn facebook_config_from_env() -> FacebookConfig {
    FacebookConfig {
        app_id_present: env_value("FACEBOOK_APP_ID").is_some(),
        app_secret_present: env_value("FACEBOOK_APP_SECRET").is_some(),
    }
}

/// Build the Wave 13 SIDRA bridge configuration from the
/// environment: `SIDRA_API_URL` (where buyer-message
/// notifications go) and `SIDRA_WEBHOOK_SECRET` (the shared HMAC
/// secret). The secret value is held in memory only. A
/// half-configuration (only one of the two) is reported with a
/// startup warning and treated as unconfigured by the bridge.
fn sidra_config_from_env() -> SidraConfig {
    let config = SidraConfig {
        api_url: env_value("SIDRA_API_URL"),
        webhook_secret: env_value("SIDRA_WEBHOOK_SECRET"),
    };
    if config.configured() {
        println!("SIDRA bridge configured (notify URL + webhook secret present)");
    } else if config.api_url.is_some() || config.webhook_secret.is_some() {
        eprintln!(
            "WARNING: SIDRA bridge is half-configured — both SIDRA_API_URL and \
             SIDRA_WEBHOOK_SECRET are required; the bridge is treated as \
             unconfigured and messaging runs as a human-only inbox."
        );
    }
    config
}

/// Build the Wave 20 CORS origin list from the environment:
/// `CORS_ORIGINS` is a comma-separated list of exact origins
/// (scheme + host + port, e.g. `https://shop.example.com`).
/// Unset, empty, or all-blank values fall back to the single
/// local dev origin ([`DEFAULT_CORS_ORIGIN`]) — a deployment
/// must name its real web origin here, because the browser
/// blocks cross-origin calls from anywhere else.
fn cors_origins_from_env() -> Vec<String> {
    let origins: Vec<String> = match std::env::var("CORS_ORIGINS") {
        Ok(raw) => raw
            .split(',')
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    };
    if origins.is_empty() {
        vec![DEFAULT_CORS_ORIGIN.to_string()]
    } else {
        origins
    }
}

/// The router's CORS layer (Wave 20): preflight and response
/// headers for exactly the configured origins — the Wave 1–19
/// permissive layer is gone. Only the methods and headers the
/// Next.js app actually uses are allowed. An entry that is not
/// a valid header value is skipped with a warning rather than
/// taking the whole API down over a typo.
fn cors_layer(origins: &[String]) -> CorsLayer {
    let parsed: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|origin| match origin.parse::<HeaderValue>() {
            Ok(value) => Some(value),
            Err(_) => {
                eprintln!(
                    "WARNING: CORS origin '{origin}' is not a valid origin — ignoring it"
                );
                None
            }
        })
        .collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(parsed))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ])
}

/// Run the embedded workspace migrations (SQLx migrate format, in
/// `migrations/` at the workspace root) against a pool.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("../../migrations").run(pool).await
}

// ---------------------------------------------------------------------------
// Wave 1 stateless endpoints (unchanged behaviour; no database involved).
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    version: &'static str,
    database: &'static str,
}

async fn health(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "bd-marketplace-api",
        version: env!("CARGO_PKG_VERSION"),
        database: if state.pool.is_some() {
            "configured"
        } else {
            "not configured"
        },
    })
}

/// Wave 20 readiness probe: unlike `/health` (liveness — the
/// process is up), this answers whether the API can actually
/// serve database-backed traffic right now. It runs a real
/// `SELECT 1` against the pool: 200 `{"status": "ready"}` when
/// the query succeeds, 503 when there is no pool or the query
/// fails. Orchestrators (compose healthchecks, load balancers)
/// should gate traffic on this, not on `/health`.
async fn health_ready(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> (StatusCode, Json<serde_json::Value>) {
    let db_ok = match state.pool.as_ref() {
        Some(pool) => sqlx::query("SELECT 1").execute(pool).await.is_ok(),
        None => false,
    };
    if db_ok {
        (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ready", "database": "ok" })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "status": "not ready", "database": "unavailable" })),
        )
    }
}

#[derive(Deserialize)]
struct DeliveryChargeRequest {
    zone: DeliveryZone,
    weight_kg: u32,
    #[serde(default)]
    cod_amount_poisha: i64,
}

#[derive(Serialize)]
struct DeliveryChargeResponse {
    delivery_charge_poisha: i64,
}

async fn delivery_charge(
    Json(req): Json<DeliveryChargeRequest>,
) -> Result<Json<DeliveryChargeResponse>, ApiError> {
    // Wave 39 (audit CR-11): the domain rule returns an error for
    // a negative COD amount — a clean bilingual 400 here, where
    // the previous code panicked inside the handler.
    let charge = calculate_delivery_charge(req.zone, req.weight_kg, req.cod_amount_poisha)
        .map_err(ApiError::bad_request)?;
    Ok(Json(DeliveryChargeResponse {
        delivery_charge_poisha: charge,
    }))
}

#[derive(Deserialize)]
struct SplitCartRequest {
    items: Vec<CartItem>,
}

#[derive(Serialize)]
struct SplitCartResponse {
    sub_orders: Vec<SellerSubOrder>,
}

async fn split_cart(
    Json(req): Json<SplitCartRequest>,
) -> Result<Json<SplitCartResponse>, ApiError> {
    Ok(Json(SplitCartResponse {
        sub_orders: split_cart_by_seller(&req.items).map_err(ApiError::bad_request)?,
    }))
}

#[derive(Deserialize)]
struct CommissionQuoteRequest {
    subtotal_poisha: i64,
    rate_bps: u32,
}

async fn commission_quote(
    Json(req): Json<CommissionQuoteRequest>,
) -> Result<Json<CommissionQuote>, ApiError> {
    Ok(Json(
        compute_commission(req.subtotal_poisha, req.rate_bps).map_err(ApiError::bad_request)?,
    ))
}

/// Wave 28 security headers, applied to EVERY response the
/// router produces (including errors and CORS preflights).
/// This service is a JSON API — it never serves HTML, frames
/// or assets — so the set is strict: no MIME sniffing, no
/// framing at all (the header and the CSP agree), no referrer
/// leakage, a `default-src 'none'` CSP that would make any
/// accidentally-served markup inert, and HSTS for the TLS
/// terminators deployed in front of it. The Next.js app
/// serves its own header set — see `apps/web/next.config.js`.
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    headers.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000"),
    );
    response
}

/// Build the full router around a state. Used by `main` and by the
/// integration tests (which drive it via tower oneshot, no TCP port).
pub fn build_app(state: AppState) -> Router {
    // Wave 20: the CORS layer is built from the state's origin
    // list before the state moves into the router.
    let cors = cors_layer(&state.cors_origins);
    Router::new()
        .route("/health", get(health))
        // Wave 20 — readiness probe (pings the database).
        .route("/health/ready", get(health_ready))
        // Wave 1 — stateless domain calls.
        .route("/v1/delivery-charge", post(delivery_charge))
        .route("/v1/cart/split", post(split_cart))
        .route("/v1/commission/quote", post(commission_quote))
        // Wave 2 — auth (phone + OTP + JWT).
        .route("/v1/auth/request-otp", post(auth::request_otp))
        .route("/v1/auth/verify-otp", post(auth::verify_otp))
        // Wave 10 — the caller's own row, re-read (carries is_admin).
        .route("/v1/auth/me", get(auth::me))
        // Wave 37 — API keys: mint (the full key is shown
        // once), list (metadata + prefixes only) and revoke
        // (idempotent). JWT-only: the auth layer refuses key
        // authentication on these very routes, so a key can
        // never manage keys.
        .route(
            "/v1/api-keys",
            post(api_keys::create_api_key_handler).get(api_keys::list_api_keys_handler),
        )
        .route(
            "/v1/api-keys/:id",
            delete(api_keys::revoke_api_key_handler),
        )
        // Wave 2 — stores.
        .route("/v1/stores", post(stores::create_store))
        .route("/v1/stores/:slug", get(stores::get_store))
        .route(
            "/v1/stores/:slug/products",
            get(products::list_store_products),
        )
        // Wave 34 — multi-store access + staff roles: every
        // store the caller owns or is a member of (with their
        // role in it), and the member management endpoints —
        // seating, re-roling and removing members are the
        // owner's alone (`manage_members` in the domain
        // matrix); the list itself is owner + manager.
        .route("/v1/seller/stores", get(stores::list_seller_stores_handler))
        .route(
            "/v1/seller/stores/:id/members",
            get(stores::list_store_members_handler).post(stores::add_store_member_handler),
        )
        .route(
            "/v1/seller/stores/:id/members/:user_id",
            patch(stores::update_store_member_handler)
                .delete(stores::remove_store_member_handler),
        )
        // Wave 2 — products.
        .route(
            "/v1/products",
            post(products::create_product).get(products::list_products_handler),
        )
        .route(
            "/v1/products/:id",
            get(products::get_product).patch(products::update_product),
        )
        // Wave 15 — Postgres-native product search (pg_trgm +
        // ILIKE) and typeahead suggestions; public, active
        // products of active stores only.
        .route("/v1/search", get(search::search_handler))
        .route("/v1/search/suggest", get(search::suggest_handler))
        // Wave 3 — cart (one cart per signed-in buyer).
        .route("/v1/cart", get(cart::get_cart).delete(cart::clear))
        .route("/v1/cart/items", post(cart::add_item))
        .route(
            "/v1/cart/items/:product_id",
            patch(cart::set_item_quantity).delete(cart::remove_item),
        )
        // Wave 3 — checkout + buyer orders.
        .route(
            "/v1/orders",
            post(orders::create_order).get(orders::list_orders),
        )
        .route("/v1/orders/:id", get(orders::get_order))
        // Wave 4 — buyer cancel (with restock) + receipt view.
        .route("/v1/orders/:id/cancel", post(orders::cancel_order))
        .route("/v1/orders/:id/receipt", get(orders::get_receipt))
        // Wave 35 — loyalty: the caller's points balance,
        // the programme rules and their recent ledger.
        .route("/v1/loyalty", get(loyalty::get_loyalty_handler))
        // Wave 5 — payments: availability, gateway webhook, retry,
        // and the dev simulator helper (404s when dev mode is off).
        .route("/v1/payments/methods", get(payments::list_methods))
        // Wave 17 — the public COD advance policy (effective
        // percent + threshold), so checkout can warn the buyer
        // before the server decides.
        .route(
            "/v1/payments/advance-policy",
            get(payments::advance_policy),
        )
        .route("/v1/payments/webhook/:method", post(payments::webhook))
        .route("/v1/payments/:id/retry", post(payments::retry))
        .route(
            "/v1/payments/:id/dev-simulate-success",
            post(payments::dev_simulate),
        )
        // Wave 3 — seller sub-orders.
        .route(
            "/v1/seller/sub-orders",
            get(orders::list_seller_sub_orders_handler),
        )
        .route(
            "/v1/seller/sub-orders/:id/status",
            patch(orders::update_sub_order_status_handler),
        )
        // Wave 6 — couriers + shipments: availability, booking a
        // shipment for a confirmed sub-order, tracking refresh.
        // Wave 7 — per-courier estimates + smart routing.
        .route("/v1/couriers", get(shipments::list_couriers))
        .route("/v1/couriers/estimate", get(shipments::courier_estimates))
        .route(
            "/v1/seller/sub-orders/:id/shipment",
            post(shipments::create_shipment),
        )
        // Wave 40 — seller fulfillment center: the work queue
        // (buckets + SLA ages over the caller's store), bulk
        // shipment booking through the single-booking path,
        // and courier label data for the web print view.
        .route(
            "/v1/seller/fulfillment",
            get(fulfillment::list_fulfillment_handler),
        )
        .route(
            "/v1/seller/fulfillment/bulk-book",
            post(fulfillment::bulk_book_handler),
        )
        .route(
            "/v1/seller/fulfillment/labels",
            get(fulfillment::labels_handler),
        )
        .route("/v1/shipments/:id/refresh", post(shipments::refresh))
        // Wave 19 — manual couriers: seller-entered statuses and
        // the counter receipt number for manual shipments (the
        // booking itself goes through the Wave 6 booking route
        // with `courier = "manual"`).
        .route(
            "/v1/seller/shipments/:id/manual-status",
            post(shipments::set_manual_status),
        )
        .route(
            "/v1/seller/shipments/:id/manual-tracking",
            post(shipments::set_manual_tracking),
        )
        // Wave 8 — reviews: buyers review products they received,
        // edit/delete their own; store owners reply once per review.
        .route(
            "/v1/products/:id/reviews",
            post(reviews::create_review_handler).get(reviews::list_reviews_handler),
        )
        .route(
            "/v1/reviews/:id",
            patch(reviews::update_review_handler).delete(reviews::delete_review_handler),
        )
        .route(
            "/v1/seller/reviews/:id/reply",
            post(reviews::create_reply_handler).patch(reviews::update_reply_handler),
        )
        // Wave 25 — product Q&A: signed-in users ask on a
        // product page, the store owner answers (re-answering
        // corrects), owners and admins hide spam.
        .route(
            "/v1/products/:id/questions",
            post(qna::ask_question_handler).get(qna::list_questions_handler),
        )
        .route(
            "/v1/seller/questions",
            get(qna::list_seller_questions_handler),
        )
        .route(
            "/v1/seller/questions/:id/answer",
            post(qna::answer_question_handler),
        )
        .route(
            "/v1/seller/questions/:id/hide",
            post(qna::hide_question_handler),
        )
        .route(
            "/v1/seller/questions/:id/unhide",
            post(qna::unhide_question_handler),
        )
        .route(
            "/v1/admin/questions/:id/hide",
            post(qna::admin_hide_question_handler),
        )
        .route(
            "/v1/admin/questions/:id/unhide",
            post(qna::admin_unhide_question_handler),
        )
        // Wave 9 — seller dashboard, balance, payout account and
        // payout requests (approval/payment land with the admin wave).
        .route("/v1/seller/dashboard", get(payouts::dashboard_handler))
        .route("/v1/seller/balance", get(payouts::balance_handler))
        .route(
            "/v1/seller/payout-account",
            get(payouts::get_payout_account_handler)
                .put(payouts::save_payout_account_handler),
        )
        .route(
            "/v1/seller/payouts",
            post(payouts::request_payout_handler).get(payouts::list_payouts_handler),
        )
        // Wave 27 — seller analytics (windowed aggregates over
        // the caller's stores) and its CSV export.
        .route(
            "/v1/seller/analytics",
            get(analytics::seller_analytics_handler),
        )
        .route(
            "/v1/seller/analytics/export.csv",
            get(analytics::seller_analytics_csv_handler),
        )
        // Wave 31 — data portability: the caller's own record as
        // one downloadable JSON document (the seller's catalogue
        // + sales + money ledger; the buyer's orders + reviews +
        // wishlist + questions).
        .route("/v1/seller/export", get(exports::seller_export_handler))
        .route("/v1/buyer/export", get(exports::buyer_export_handler))
        // Wave 12 — Facebook Page link + post import: link/unlink
        // the seller's Page, read the link + Graph API status,
        // preview parsed drafts from pasted posts, import the
        // approved drafts, and the honest Graph source stub.
        .route(
            "/v1/seller/store/facebook",
            put(facebook::link_facebook_page).delete(facebook::unlink_facebook_page),
        )
        // Wave 18 — store branding: theme, brand colour, logo /
        // banner URLs and announcement for the seller's own
        // store (validated values only, never markup).
        .route(
            "/v1/seller/store/branding",
            put(stores::update_store_branding),
        )
        .route("/v1/seller/facebook/status", get(facebook::facebook_status))
        .route(
            "/v1/seller/products/import-preview",
            post(facebook::import_preview_handler),
        )
        .route(
            "/v1/seller/products/import",
            post(facebook::import_products_handler),
        )
        .route(
            "/v1/seller/products/import-from-page",
            post(facebook::import_from_page),
        )
        // Wave 13 — messaging: buyer <-> store conversations
        // (find-or-create, thread with read markers, close), the
        // SIDRA reply callback, and the seller's SIDRA controls
        // (suggestions queue, auto-reply toggle, bridge status).
        .route(
            "/v1/conversations",
            post(messaging::create_conversation_handler)
                .get(messaging::list_conversations_handler),
        )
        .route(
            "/v1/conversations/:id",
            get(messaging::get_conversation_handler),
        )
        .route(
            "/v1/conversations/:id/messages",
            post(messaging::post_message_handler),
        )
        .route(
            "/v1/conversations/:id/close",
            post(messaging::close_conversation_handler),
        )
        .route("/v1/sidra/reply", post(messaging::sidra_reply_handler))
        .route(
            "/v1/seller/sidra/suggestions",
            get(messaging::list_suggestions_handler),
        )
        .route(
            "/v1/seller/sidra/suggestions/:id/send",
            post(messaging::send_suggestion_handler),
        )
        .route(
            "/v1/seller/sidra/suggestions/:id/dismiss",
            post(messaging::dismiss_suggestion_handler),
        )
        .route(
            "/v1/seller/store/sidra",
            put(messaging::set_sidra_auto_reply_handler),
        )
        .route(
            "/v1/seller/sidra/status",
            get(messaging::sidra_status_handler),
        )
        // Wave 10 — platform admin: every route behind require_admin.
        .route("/v1/admin/overview", get(admin::overview_handler))
        // Wave 43 — the observability snapshot: point-in-time
        // platform health (orders today, backlogs, stale work).
        .route("/v1/admin/metrics", get(metrics::admin_metrics_handler))
        .route(
            "/v1/admin/analytics",
            get(analytics::admin_analytics_handler),
        )
        .route("/v1/admin/stores", get(admin::list_stores_handler))
        .route(
            "/v1/admin/stores/:id/status",
            patch(admin::set_store_status_handler),
        )
        .route("/v1/admin/orders", get(admin::list_orders_handler))
        .route("/v1/admin/payouts", get(admin::list_payouts_handler))
        .route(
            "/v1/admin/payouts/:id/approve",
            post(admin::approve_payout_handler),
        )
        .route(
            "/v1/admin/payouts/:id/reject",
            post(admin::reject_payout_handler),
        )
        .route(
            "/v1/admin/payouts/:id/mark-paid",
            post(admin::mark_payout_paid_handler),
        )
        .route("/v1/admin/users", get(admin::list_users_handler))
        .route("/v1/admin/users/:id", patch(admin::set_user_admin_handler))
        .route(
            "/v1/admin/settings",
            get(admin::get_settings_handler).put(admin::update_settings_handler),
        )
        // Wave 28 — the admin audit log: the read side of the
        // rows the audited admin actions write inside their
        // own transactions (see the `audit` module).
        .route("/v1/admin/audit-log", get(audit::list_audit_log_handler))
        // Wave 29 — the admin notifications view: outbox
        // counts (pending / sending / sent / failed) plus the
        // newest rows with masked phones (see the
        // `notifications` module).
        .route(
            "/v1/admin/notifications",
            get(notifications::list_notifications_handler),
        )
        // Coupons (Wave 14): stateless preview for buyers, seller
        // CRUD for store coupons, admin CRUD for platform coupons.
        .route("/v1/coupons/preview", post(coupons::preview_handler))
        .route(
            "/v1/seller/coupons",
            get(coupons::list_seller_coupons_handler)
                .post(coupons::create_seller_coupon_handler),
        )
        .route(
            "/v1/seller/coupons/:id",
            patch(coupons::set_seller_coupon_active_handler)
                .delete(coupons::delete_seller_coupon_handler),
        )
        .route(
            "/v1/admin/coupons",
            get(coupons::list_admin_coupons_handler)
                .post(coupons::create_admin_coupon_handler),
        )
        .route(
            "/v1/admin/coupons/:id",
            patch(coupons::set_admin_coupon_active_handler),
        )
        // Wave 17 — the seller's buyer-risk lookup (the fraud
        // shield's verdict for one phone number).
        .route("/v1/seller/buyer-risk", get(risk::buyer_risk_handler))
        // Wave 16 — public tracking: NO auth by design; the
        // order's delivery phone is the credential and every
        // failure is the same 404 (see the tracking module).
        .route("/v1/track", get(tracking::track_handler))
        // Wave 16 — returns: buyer requests a return for a
        // delivered sub-order and lists their own; the seller
        // decides (approve / reject) and marks the goods
        // received; admins work the refund queue.
        .route(
            "/v1/buyer/sub-orders/:id/return-request",
            post(returns::create_return_request_handler),
        )
        .route(
            "/v1/buyer/return-requests",
            get(returns::list_buyer_return_requests_handler),
        )
        .route(
            "/v1/seller/return-requests",
            get(returns::list_seller_return_requests_handler),
        )
        .route(
            "/v1/seller/return-requests/:id/approve",
            post(returns::approve_return_handler),
        )
        .route(
            "/v1/seller/return-requests/:id/reject",
            post(returns::reject_return_handler),
        )
        .route(
            "/v1/seller/return-requests/:id/received",
            post(returns::received_return_handler),
        )
        .route(
            "/v1/admin/return-requests",
            get(returns::list_admin_return_requests_handler),
        )
        .route(
            "/v1/admin/return-requests/:id/refund-done",
            post(returns::refund_done_handler),
        )
        // Wave 21 — seller KYC: the seller submits and reads
        // their own store's verification application; admins
        // work the review queue, open the single full-NID view,
        // and approve or reject. Every admin route is behind
        // require_admin; the full view is the ONLY response in
        // the API that carries a complete NID number.
        .route(
            "/v1/seller/store/verification",
            put(verification::submit_seller_verification_handler)
                .get(verification::get_seller_verification_handler),
        )
        .route(
            "/v1/admin/verifications",
            get(verification::list_admin_verifications_handler),
        )
        .route(
            "/v1/admin/verifications/:store_id/full",
            get(verification::get_admin_verification_full_handler),
        )
        .route(
            "/v1/admin/verifications/:store_id/approve",
            post(verification::approve_verification_handler),
        )
        .route(
            "/v1/admin/verifications/:store_id/reject",
            post(verification::reject_verification_handler),
        )
        // Wave 22 — invoices: the per-seller document for one
        // sub-order, issued lazily (and idempotently) on the
        // first entitled read once the sub-order is delivered.
        // Buyer = the order's buyer, seller = the store's
        // owner, admin behind require_admin; everyone else
        // gets the same 404 as an unknown sub-order.
        .route(
            "/v1/buyer/sub-orders/:id/invoice",
            get(invoices::get_buyer_invoice),
        )
        .route(
            "/v1/seller/sub-orders/:id/invoice",
            get(invoices::get_seller_invoice),
        )
        .route(
            "/v1/admin/sub-orders/:id/invoice",
            get(invoices::get_admin_invoice),
        )
        // Wave 23 — inventory: the seller's stock overview,
        // manual adjustments (each one audited), the per-product
        // movement history, and low-stock thresholds. All
        // seller-scoped: another seller's product is a 404.
        .route("/v1/seller/inventory", get(inventory::inventory_handler))
        .route(
            "/v1/seller/products/:id/adjust-stock",
            post(inventory::adjust_stock_handler),
        )
        .route(
            "/v1/seller/products/:id/stock-history",
            get(inventory::stock_history_handler),
        )
        .route(
            "/v1/seller/products/:id/low-stock-threshold",
            put(inventory::set_low_stock_threshold_handler),
        )
        // Wave 33 — product variants: seller CRUD over a
        // product's variants (create, list incl. inactive,
        // patch, soft-deactivate via DELETE) and per-variant
        // audited stock adjustments. Owner-only; a stranger's
        // product or variant is a 404. The public side rides
        // the product detail (its `variants` array); list
        // views carry only `has_variants`.
        .route(
            "/v1/seller/products/:id/variants",
            post(variants::create_variant_handler).get(variants::list_variants_handler),
        )
        .route(
            "/v1/seller/products/:id/variants/:variant_id",
            patch(variants::update_variant_handler)
                .delete(variants::deactivate_variant_handler),
        )
        .route(
            "/v1/seller/products/:id/variants/:variant_id/adjust-stock",
            post(variants::adjust_variant_stock_handler),
        )
        // Wave 51 — product media: image upload (multipart;
        // the 12 MiB body limit is layered on THIS route
        // only — the global 1 MiB cap still guards every
        // other route), attach-by-URL, the gallery list and
        // batch layout update, single delete (the primary
        // promotes the next image), the cross-store media
        // library with bulk delete, and store branding
        // uploads. All seller writes sit behind the
        // manage_catalog access rule (404 for strangers).
        .route(
            "/v1/seller/products/:id/images",
            post(product_images::upload_image_handler)
                .layer(DefaultBodyLimit::max(
                    product_images::IMAGE_UPLOAD_ROUTE_LIMIT_BYTES,
                )),
        )
        .route(
            "/v1/seller/products/:id/images",
            get(product_images::list_images_handler)
                .patch(product_images::update_images_handler),
        )
        .route(
            "/v1/seller/products/:id/images/from-url",
            post(product_images::attach_image_url_handler),
        )
        .route(
            "/v1/seller/products/:id/images/from-urls",
            post(product_images::attach_images_from_urls_handler),
        )
        .route(
            "/v1/seller/products/:id/images/from-web",
            post(product_images::attach_image_from_web_handler),
        )
        .route(
            "/v1/seller/products/:id/images/bulk",
            post(product_images::bulk_upload_images_handler).layer(
                DefaultBodyLimit::max(product_images::IMAGE_UPLOAD_ROUTE_LIMIT_BYTES),
            ),
        )
        .route(
            "/v1/seller/products/:id/images/:image_id",
            delete(product_images::delete_image_handler),
        )
        .route(
            "/v1/seller/products/:id/images/:image_id",
            put(product_images::replace_image_handler).layer(
                DefaultBodyLimit::max(product_images::IMAGE_UPLOAD_ROUTE_LIMIT_BYTES),
            ),
        )
        // Wave 51 extension — admin media moderation: the
        // cross-store image list and the audited delete.
        .route(
            "/v1/admin/media",
            get(product_images::list_admin_media_handler),
        )
        .route(
            "/v1/admin/media/:image_id",
            delete(product_images::delete_admin_media_handler),
        )
        .route(
            "/v1/seller/media",
            get(product_images::list_seller_media_handler),
        )
        .route(
            "/v1/seller/media/bulk-delete",
            post(product_images::bulk_delete_media_handler),
        )
        .route(
            "/v1/seller/store/branding-image",
            post(product_images::upload_branding_image_handler).layer(
                DefaultBodyLimit::max(product_images::IMAGE_UPLOAD_ROUTE_LIMIT_BYTES),
            ),
        )
        // Wave 51 — review photos: the author attaches (or
        // replaces) and removes the one photo on their own
        // review; the upload takes the same raised body
        // limit as the product image upload.
        .route(
            "/v1/reviews/:id/image",
            post(reviews::attach_review_image_handler)
                .layer(DefaultBodyLimit::max(
                    product_images::IMAGE_UPLOAD_ROUTE_LIMIT_BYTES,
                ))
                .delete(reviews::delete_review_image_handler),
        )
        // Wave 51 — media serving: the LOCAL driver's bytes.
        // (For the S3 driver the handler answers 404; views
        // carry the endpoint's own URLs in that deployment.)
        .route("/media/*key", get(product_images::serve_media_handler))
        // Wave 26 — sponsored promotions: the public policy
        // (effective per-day price + the 30-day cap), buying a
        // promotion for an own active product prepaid from the
        // earnings balance, the seller's promotion list, and
        // cancellation (placement stops, the charge stands).
        .route("/v1/promotions/policy", get(promotions::policy_handler))
        .route(
            "/v1/seller/products/:id/promote",
            post(promotions::promote_handler),
        )
        .route(
            "/v1/seller/promotions",
            get(promotions::list_promotions_handler),
        )
        .route(
            "/v1/seller/promotions/:id/cancel",
            post(promotions::cancel_promotion_handler),
        )
        // Wave 36 — seller subscription plans: the public
        // catalogue (prices are effective values), the
        // caller's current subscription on their payout store,
        // subscribing / switching (owner only, prepaid from
        // the earnings balance), and cancelling renewal
        // (benefits run to the paid period's end). Renewals
        // and expiry are the worker's sweep over the same
        // service functions — there is no renewal endpoint.
        .route(
            "/v1/seller/subscription/plans",
            get(subscriptions::list_plans_handler),
        )
        .route(
            "/v1/seller/subscription",
            get(subscriptions::get_subscription_handler)
                .post(subscriptions::subscribe_handler),
        )
        .route(
            "/v1/seller/subscription/cancel",
            post(subscriptions::cancel_subscription_handler),
        )
        // Wave 52 — merchandising: the public home feed and
        // collection landing data, the event beacon, and the
        // admin CRUD for banners / collections / flash sales
        // (all behind require_admin; the banner image upload
        // takes the same raised body limit as product images).
        // Flash price resolution itself lives in the merch
        // module and is honoured by the cart and checkout.
        .route("/v1/home", get(merch::home_feed_handler))
        .route(
            "/v1/collections/:slug",
            get(merch::get_collection_public_handler),
        )
        .route("/v1/merch/events", post(merch::record_merch_event_handler))
        .route(
            "/v1/admin/home-preview",
            get(merch::admin_home_preview_handler),
        )
        .route(
            "/v1/admin/merch-events",
            get(merch::list_merch_events_handler),
        )
        .route(
            "/v1/admin/banners",
            get(merch::list_banners_handler).post(merch::create_banner_handler),
        )
        .route(
            "/v1/admin/banners/:id",
            patch(merch::update_banner_handler).delete(merch::delete_banner_handler),
        )
        .route(
            "/v1/admin/banners/:id/image",
            post(merch::upload_banner_image_handler).layer(DefaultBodyLimit::max(
                product_images::IMAGE_UPLOAD_ROUTE_LIMIT_BYTES,
            )),
        )
        .route(
            "/v1/admin/banners/:id/image",
            delete(merch::delete_banner_image_handler),
        )
        .route(
            "/v1/admin/collections",
            get(merch::list_collections_handler).post(merch::create_collection_handler),
        )
        .route(
            "/v1/admin/collections/:id",
            get(merch::get_collection_admin_handler)
                .patch(merch::update_collection_handler)
                .delete(merch::delete_collection_handler),
        )
        .route(
            "/v1/admin/collections/:id/products",
            put(merch::set_collection_products_handler),
        )
        .route(
            "/v1/admin/flash-sales",
            get(merch::list_flash_sales_handler).post(merch::create_flash_sale_handler),
        )
        .route(
            "/v1/admin/flash-sales/:id",
            get(merch::get_flash_sale_handler)
                .patch(merch::update_flash_sale_handler)
                .delete(merch::delete_flash_sale_handler),
        )
        .route(
            "/v1/admin/flash-sales/:id/items",
            put(merch::set_flash_sale_items_handler),
        )
        // Wave 52 — the seller's seat on the same event: their
        // own products inside the live flash sale, with the
        // cap accounting the resolution itself uses.
        .route("/v1/seller/flash-sale", get(merch::seller_flash_sale_handler))
        // Wave 24 — wishlist: the caller's server-side
        // saved-for-later list. Add/remove are idempotent
        // and answer with the resulting state; the ids read
        // is the lightweight one the heart toggles use.
        // `/ids` is registered alongside the `:product_id`
        // routes — the static segment wins for GETs.
        .route("/v1/wishlist", get(wishlist::list_wishlist_handler))
        .route("/v1/wishlist/ids", get(wishlist::list_wishlist_ids_handler))
        .route(
            "/v1/wishlist/:product_id",
            post(wishlist::add_to_wishlist_handler)
                .delete(wishlist::remove_from_wishlist_handler),
        )
        // Wave 24 — recommendations: related and also-bought
        // are public rails on the product; the personalised
        // rail is the caller's own (Bearer).
        .route(
            "/v1/products/:id/related",
            get(recommend::related_handler),
        )
        .route(
            "/v1/products/:id/also-bought",
            get(recommend::also_bought_handler),
        )
        .route(
            "/v1/recommendations",
            get(recommend::recommendations_handler),
        )
        .layer(cors)
        // Wave 28: security headers on every response, and a
        // hard cap on request bodies for the body extractors.
        .layer(middleware::from_fn(security_headers))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        // Wave 32: gzip response compression — the response
        // flows back out through the security-headers and CORS
        // layers first (their headers are set on the plain
        // body), and this layer compresses the finished body
        // only when the client sent `Accept-Encoding: gzip`.
        // Requests without that header are untouched, so the
        // existing test suites (which never send it) see
        // byte-identical responses. (Wave 43 adds one layer
        // outside this one; it only stamps a header and logs,
        // so the compression behaviour is unchanged.)
        .layer(CompressionLayer::new())
        // Wave 43: request ids + one structured log line per
        // request, added LAST so it is the OUTERMOST layer —
        // every request the process answers (health checks,
        // CORS preflights, body-limit rejections, 404s)
        // carries an X-Request-Id and leaves exactly one log
        // line. See the `observability` module.
        .layer(middleware::from_fn(observability::request_logging))
        .with_state(state)
}

/// One startup log line (Wave 20) summarising the EFFECTIVE
/// configuration without printing a single secret value: which
/// dev modes are on, how many payment gateways and couriers
/// carry complete merchant credentials, whether the SIDRA
/// bridge and the Facebook app are configured, the CORS origin
/// count, and the money policy fallbacks. Operators verify a
/// deployment from this line instead of inspecting the process
/// environment.
fn log_config_summary(state: &AppState) {
    let gateways_configured = [
        PaymentMethod::Bkash,
        PaymentMethod::Nagad,
        PaymentMethod::SslCommerz,
        PaymentMethod::ShurjoPay,
    ]
    .iter()
    .filter(|method| state.payments_config.credentials_present(**method))
    .count();
    let couriers_live = ["steadfast", "pathao", "redx", "ecourier", "paperfly"]
        .iter()
        .filter(|name| {
            shipments::courier_availability_entry(name, &state.couriers_config)
                .mode
                .as_deref()
                == Some("live")
        })
        .count();
    println!(
        "config summary: sms_dev_mode={} payments_dev_mode={} couriers_dev_mode={} \
         payment gateways configured={}/4 couriers live={}/5 sidra_configured={} \
         facebook_app_configured={} cors_origins={} commission={}bps \
         advance={}bps at/above {} poisha promotion_price={} poisha/day \
         subscription_pro={} subscription_business={} poisha/period \
         free_plan_cap={} api_key_rate_limit={}/min media_store={}",
        state.sms_dev_mode,
        state.payments_config.dev_mode,
        state.couriers_config.dev_mode,
        gateways_configured,
        couriers_live,
        state.sidra_config.configured(),
        state.facebook_config.graph_api_configured(),
        state.cors_origins.len(),
        state.commission_rate_bps,
        state.advance_percent_bps,
        state.advance_threshold_poisha,
        state.promotion_price_per_day_poisha,
        state.subscription_price_pro_poisha,
        state.subscription_price_business_poisha,
        state.plan_free_product_cap,
        state.api_key_rate_limit_per_min,
        state.media_store.driver(),
    );
}

/// Resolve when the process is asked to stop (Wave 20): Ctrl-C
/// on any platform, plus SIGTERM on Unix — what Docker and
/// systemd send. `axum::serve` then stops accepting new
/// connections and drains the in-flight ones.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install the Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install the SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    println!("shutdown signal received — draining in-flight requests");
}

/// Connect to Postgres (when configured), migrate, and serve. Called by
/// the binary's `main`; kept in the library so behaviour stays testable.
pub async fn run() {
    let pool = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.is_empty() => {
            let pool = PgPool::connect(&url)
                .await
                .expect("failed to connect to Postgres via DATABASE_URL");
            run_migrations(&pool)
                .await
                .expect("failed to run database migrations");
            println!("database connected, migrations applied");
            Some(pool)
        }
        _ => {
            println!(
                "DATABASE_URL not set — starting without a database; \
                 DB endpoints will return 503"
            );
            None
        }
    };

    let state = AppState::from_env(pool);
    // Wave 28: refuse to start on a forgeable JWT secret
    // outside dev mode. `from_env` has already warned loudly
    // about the unset-secret fallback; this turns the warning
    // into a hard stop — the reason on stderr, the
    // conventional configuration-error exit code 2, and no
    // secret value in the message.
    if let Err(reason) = validate_jwt_secret(&state.jwt_secret, state.sms_dev_mode) {
        eprintln!("FATAL: {reason}");
        std::process::exit(2);
    }
    log_config_summary(&state);
    let port: u16 = std::env::var("API_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8080);
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind API listener");
    println!("bd-marketplace api listening on {addr}");
    axum::serve(listener, build_app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("API server error");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_secret_dev_mode_accepts_anything() {
        // Dev mode is the local-development path: even the
        // well-known fallback and a one-character secret pass,
        // because dev mode already gates everything dangerous
        // (log SMS sender, dev_otp) behind the same flag.
        assert!(validate_jwt_secret(DEV_JWT_SECRET, true).is_ok());
        assert!(validate_jwt_secret("x", true).is_ok());
        assert!(validate_jwt_secret("", true).is_ok());
    }

    #[test]
    fn jwt_secret_production_rules() {
        // The well-known fallback is refused even though it is
        // longer than the minimum — length alone is not safety.
        assert!(validate_jwt_secret(DEV_JWT_SECRET, false).is_err());
        // Short secrets are refused; the boundary is exact.
        assert!(validate_jwt_secret("short-secret", false).is_err());
        assert!(validate_jwt_secret(&"a".repeat(31), false).is_err());
        assert!(validate_jwt_secret(&"a".repeat(32), false).is_ok());
        assert!(validate_jwt_secret(
            "a-real-production-secret-with-plenty-of-entropy",
            false
        )
        .is_ok());
    }

    #[test]
    fn jwt_secret_errors_never_echo_the_secret() {
        let secret = "tiny";
        let err = validate_jwt_secret(secret, false).expect_err("short secret refused");
        assert!(!err.contains(secret), "error leaked the secret: {err}");
        let err = validate_jwt_secret(DEV_JWT_SECRET, false).expect_err("fallback refused");
        assert!(
            !err.contains(DEV_JWT_SECRET),
            "error leaked the secret: {err}"
        );
    }
}
