# Launch runbook — BD Marketplace

**Audience:** the operator taking this platform live. This document
consolidates every launch blocker, owner decision, onboarding-confirm
item and honest limit recorded across the README's 48 wave sections,
`.env.example`, and `docs/security-audit-2026-11-27.md`, so going live
does not require reading all of them. Where this runbook and a wave
section disagree, the wave section (and the code) wins — this is a map,
not a second source of truth.

## 1. What this is / current state

BD Marketplace is one multi-tenant system giving every seller both a
per-seller branded storefront (the Shopify side) and a listing in one
central marketplace with a unified cart split per seller (the Amazon
side), built Bangladesh-first: Bangla-first UI, BDT only, phone + OTP
login, COD primary with bKash / Nagad / SSLCommerz / ShurjoPay, nine
couriers behind one adapter (five API, four manual), and the SIDRA AI
salesperson bridged per store. Waves 1–48 are built and green in CI
(Rust tests + clippy, web build, Docker image builds, and the gating
cargo-audit job).

**The code is launch-ready; going live is now a credentials-and-decisions
exercise, not a development one.** Everything below is accounts to open,
values to set, and decisions to ratify — plus one integration (a real SMS
gateway) that is deliberately a trait implementation away.

## 2. Owner decisions owed before launch

Each decision lists the default in force today. None of them blocks
*starting* the launch sequence, but each should be ratified consciously
rather than inherited silently.

### 2.1 CR-15 — the COD advance degradation policy (audit decision)

Wave 17 requires a partial advance (default 20% of the grand total) on
COD orders at/above ৳2,000, and on any order from a buyer the fraud
shield flags. When a gateway **can** collect, the advance is collected
online before shipping, as designed. The open question is the third
case, when **no gateway can collect it** (no merchant credentials
configured, gateway down):

- **Today — flag-and-proceed.** The order proceeds as plain COD with
  `advance_required_poisha` recorded on it and `advance_paid_poisha`
  0 — a visible risk flag the seller surfaces show ("অগ্রিম নেওয়া
  হয়নি"). No payment row exists and no surface claims money was
  collected; the flag is a safety valve, not a fake success (README,
  Wave 17 section).
- **The alternative — fail the checkout.** Such orders are refused
  (400/503) until a gateway can collect the advance. The fraud shield
  then has no bypass, but a gateway outage also stops every
  above-threshold COD order.

The security audit records this as CR-15 and the Wave 39 section
defers it explicitly: *"Policy decision owed by the owner — fail
checkout vs flag-and-proceed."* Note the practical interaction: until
at least one payment gateway is live (§3), flag-and-proceed is the
only setting under which above-threshold COD orders can be placed at
all.

### 2.2 Commission 8% and payout minimum ৳500 — ratify or change

The defaults in force: platform commission **8%** of the goods
subtotal (`COMMISSION_RATE_BPS=800` startup fallback, Wave 3) and a
minimum payout request of **৳500** (`MIN_PAYOUT_POISHA` = 50,000
poisha, Wave 9). Both are runtime-changeable without a deploy: an
admin settings row (`platform_settings`) beats the startup value —
commission via the admin settings page, the payout minimum via the
`min_payout_poisha` setting (see §4). Subscription-plan discounts
reduce a store's effective commission rate (Wave 36). Ratify these
numbers, or change them in admin settings before the first sellers
onboard — changing them later changes seller economics mid-flight.

### 2.3 Product name

The brand is a placeholder. The code constant `PLATFORM_NAME` is
`"BD Marketplace"` (`crates/api/src/admin.rs`, `orders.rs`), and the
PWA manifest ships `name: "বাংলা মার্কেটপ্লেস — BD Marketplace"`,
`short_name: "BD Marketplace"` (`apps/web/app/manifest.ts`, Wave 38).
Picking the real name is the owner's call; renaming means updating
those constants, the manifest, and the generated PWA icons (brand
colour `#1b5e20` today) — a small change, but a visible one, so make
it before sellers print the name anywhere.

## 3. Credentials & accounts checklist

One row per external relationship. Until a row is complete, the
platform degrades honestly — typed `NotConfigured` / 503 answers,
never a fake success — and COD plus the manual couriers carry the
shop.

| Service | What to obtain | Env vars | What stays limited until then |
| --- | --- | --- | --- |
| **SMS gateway — THE launch blocker** | A Bangladeshi SMS gateway account (API key + sender id) | `SMS_API_KEY`, `SMS_SENDER_ID` (placeholders in `.env.example` today) | OTP login codes and every notification (order placed/confirmed/shipped/delivered, payout, OTP) are **never sent**. Outside dev mode the sender is `UnconfiguredSmsSender` and every send fails `sms_not_configured` (Wave 29); in dev mode codes are only logged / shown as `dev_otp`. Nobody can log in for real until this lands. |
| bKash | Merchant account | `BKASH_APP_KEY`, `BKASH_APP_SECRET` | bKash is unavailable at checkout and for COD advances; its adapter answers `NotConfigured`. |
| Nagad | Merchant account | `NAGAD_MERCHANT_ID`, `NAGAD_API_KEY` | Same — Nagad unavailable at checkout / for advances. |
| SSLCommerz | Merchant (store) account | `SSLCOMMERZ_STORE_ID`, `SSLCOMMERZ_STORE_PASSWORD` | Same — SSLCommerz unavailable at checkout / for advances. |
| ShurjoPay | Merchant account | `SHURJOPAY_USERNAME`, `SHURJOPAY_PASSWORD` | Same — ShurjoPay unavailable at checkout / for advances. |
| Steadfast | Platform merchant (master) account | `STEADFAST_API_KEY`, `STEADFAST_SECRET_KEY` (`STEADFAST_BASE_URL` optional) | Steadfast not bookable (sellers fall back to other live couriers, the DEV simulator in dev only, or manual couriers). |
| Pathao | Platform merchant account + numeric store id | `PATHAO_CLIENT_ID`, `PATHAO_CLIENT_SECRET`, `PATHAO_STORE_ID` (all three required before Pathao reports `live`) | Pathao not bookable. |
| RedX | Platform merchant account (single access token) | `REDX_API_KEY` | RedX not bookable. Bookings additionally need the recipient's numeric RedX `area_id` per booking (API body, never env). |
| eCourier | Platform merchant account (auth token + user id) | `ECOURIER_API_KEY`, `ECOURIER_USER_ID` | eCourier not bookable. Bookings additionally need the recipient's numeric eCourier `area_id` per booking. |
| Paperfly | Platform merchant account (username/password + merchant key) + the platform pickup point | `PAPERFLY_USERNAME`, `PAPERFLY_PASSWORD`, `PAPERFLY_KEY`, `PAPERFLY_PICKUP_NAME`, `PAPERFLY_PICKUP_PHONE`, `PAPERFLY_PICKUP_ADDRESS` | Paperfly not bookable; a booking with any pickup field missing fails loudly before any network call. |
| Meta / Facebook | A Meta app, **app review**, and Page access tokens (the page-token OAuth flow) | `FACEBOOK_APP_ID`, `FACEBOOK_APP_SECRET` (presence only is read) | Sellers link their Page and import by **pasting post texts** (that pipeline is fully real, Wave 12). Fetching posts directly from a Page answers 503 (unconfigured) or 501 (configured but OAuth still missing) — never fake posts. |
| SIDRA (external AI salesperson) | The SIDRA service endpoint + shared webhook secret from the SIDRA operator | `SIDRA_API_URL`, `SIDRA_WEBHOOK_SECRET` (both required) | The bridge is unconfigured: the SIDRA reply callback answers 503 and the inbox works as a plain human chat (Wave 13). SIDRA itself is an external service — this platform only holds the bridge. |

**SMS integration shape (the one piece of code a launch requires):**
implement the `SmsSender` trait in the **`notify` crate**
(`crates/notify/src/lib.rs` — `LogSmsSender` and
`UnconfiguredSmsSender` are the existing implementations, and
`sender_from_env` is the single selection rule) against the chosen
gateway's API, and select it where `sender_from_env` selects today.
OTP sending and the notification outbox both ride that one trait, so
nothing else changes.

**Payment caveat (documented, Wave 5 / README "TODO / not wired"):**
setting a gateway's credentials makes the method *available* at
checkout, but live gateway calls and **real-gateway webhook signature
verification are not wired** — they need each gateway's integration
docs confirmed against live credentials. The HMAC scheme that exists
today signs **DEV-simulator** callbacks only (`PAYMENTS_DEV_SECRET`,
Wave 39); it is not any real gateway's production signature scheme,
and must not be treated as one.

**Courier written-confirm items (get these answered by each courier,
in writing, during merchant onboarding — the adapters are coded to
documented shapes with defensive, fixture-tested parsers, and none of
the five has been verified against its live service):**

- **RedX** (Wave 7): the exact `API-ACCESS-TOKEN` header format — the
  adapter sends the raw API key as the header value; the live API
  might expect a `Bearer <key>` form — and the exact tracking
  endpoint path (the adapter uses `GET /tracking/{tracking_id}`).
  Each is isolated to one header line / one URL line. Plus the
  `area_id` requirement above.
- **eCourier** (Wave 11): the exact auth header **names** — the
  adapter sends the token as `API-AUTH-TOKEN` and the user id as
  `USER-ID`, and eCourier doc versions differ on the names — and the
  create-response field carrying the booked-order id (the parser
  accepts `order_id`, `tracking`, `consignment_id` or `e_id`).
- **Paperfly** (Wave 11): the merchant-key header name — the adapter
  sends `paperflykey` — and the tracking call's verb and path (the
  adapter POSTs JSON to `/API-Order-Tracking`).
- **Steadfast** (Wave 6) and **Pathao** (Wave 7): their real
  **charges**. Every adapter's `price_estimate` is the platform's own
  local tariff from the domain delivery-charge function — explicitly
  **not a quote** from the courier; couriers confirm their real
  charge on the consignment after booking. Confirm each courier's
  delivery / COD / return charges in the merchant agreement so the
  platform tariff and the seller-facing numbers can be reconciled
  before the first real booking.
- **Manual couriers** (Sundarban, SA Paribahan, Karatoa, Carrybee)
  need **no accounts and no credentials**: they have no APIs and run
  the seller-driven manual workflow (book with
  `{ "courier": "manual", "manual_courier": "<key>", … }`, Wave 19).

## 4. Production configuration checklist

Work through `.env.example` → `.env` (never commit a real `.env`).
The README's Deployment section (Wave 20) carries the full
per-variable table; this is the launch-critical subset.

**Must set:**

- `APP_ENV=production` — the shipped `.env.example` default is
  already the safe one (Wave 39); dev mode is opt-in only.
- `JWT_SECRET` — a real secret of **at least 32 characters**. Since
  Wave 28 the API **refuses to start** outside dev mode when the
  secret is the well-known dev fallback or shorter than 32 characters
  (`validate_jwt_secret`, enforced in `run`; stderr + exit). Unset
  falls back to a public dev-only secret with a loud warning — tokens
  signed with it can be forged by anyone.
- `POSTGRES_PASSWORD` — compose requires it: `docker-compose.yml`
  uses `${POSTGRES_PASSWORD:?...}` with **no default** (Wave 39,
  audit CR-03), and the same value goes into `DATABASE_URL`.
  Generate one: `openssl rand -hex 24`.
- `DATABASE_URL` — the Postgres connection string (compose rewrites
  it to reach the `postgres` service). Migrations are embedded in the
  API binary (`sqlx::migrate!`) and apply on startup.
- `CORS_ORIGINS` — the real web origin(s), comma-separated exact
  origins. Unset, only `http://localhost:3000` is allowed.
- `NEXT_PUBLIC_API_BASE_URL` — the public API origin, set **at web
  build time** (Next.js inlines `NEXT_PUBLIC_*` into the browser
  bundle; changing it means rebuilding the web image).

**Must be OFF (all three are dev simulators — local/CI only):**

- `PAYMENTS_DEV_MODE` — DEV payment simulator (fake paid callbacks).
- `COURIER_DEV_MODE` — DEV courier simulator (fake bookings/tracking).
- SMS dev mode — `APP_ENV` must not be `dev` and `SMS_DEV_MODE` must
  not be `true`, or OTP codes are logged and returned in API
  responses (`dev_otp`) — public codes.

**First admin (SQL-only, by design — there is no admin signup):**

1. Register the operator's account normally (phone + OTP).
2. Promote it directly in the database:

   ```sql
   UPDATE users SET is_admin = true WHERE phone = '+8801XXXXXXXXX';
   ```

   (the phone in the canonical `+880…` form the platform stores).
   Admin handlers re-read `is_admin` from the DB on every request
   (Wave 10), so the promotion takes effect immediately.

**Platform settings worth reviewing at launch** (admin settings page
/ `PUT /v1/admin/settings`; a `platform_settings` row beats the
startup env, which beats the built-in default — the full list is
`AdminSettings` in `crates/api/src/admin.rs`):

- `commission_bps` — §2.2 (fallback `COMMISSION_RATE_BPS`, 800).
- `min_payout_poisha` — §2.2 (domain default 50,000 = ৳500).
- `advance_percent_bps` / `advance_threshold_poisha` — the COD
  advance policy (fallbacks 2000 = 20% / 200,000 = ৳2,000; threshold
  0 disables threshold-based advances, risk-forced ones still apply).
- `fulfillment_confirm_sla_hours` / `fulfillment_ship_sla_hours` —
  seller SLA dashboard thresholds (fallbacks
  `FULFILLMENT_CONFIRM_SLA_HOURS` 24 / `FULFILLMENT_SHIP_SLA_HOURS`
  48, Wave 40).
- `kyc_payout_gate` — when `on`, a store must hold at least the
  `basic` verification tier to request a payout (Wave 21; default
  off).
- Also present: `promotion_price_per_day_poisha`,
  `subscription_price_pro_poisha`,
  `subscription_price_business_poisha`, `plan_free_product_cap`
  (Waves 26/36) — review before enabling promotions/subscriptions
  commercially.

## 5. Go-live sequence

1. **Provision Postgres 16** (the compose `postgres` service, or a
   managed instance) and complete §4's `.env`.
2. **Start the stack**: `docker compose up --build` — Postgres +
   API + worker + web (Wave 20). The API applies all migrations on
   startup; the worker runs the same embedded migrations and exits
   with an error if `DATABASE_URL` is missing.
3. **Wait for readiness**: `GET /health/ready` answers 200
   `{"status": "ready"}` only when its `SELECT 1` succeeds — gate
   traffic on it, not on the `/health` liveness check.
4. **Promote the first admin** (§4).
5. **Create the platform's own test store** with the operator's
   seller account.
6. **Run the smoke path end to end** (every step is a real user
   flow; do it before any seller is invited):
   register via OTP → create store → add a product → a second
   (buyer) account orders it with **COD** → seller confirms the
   sub-order → book a **manual** shipment (Sundarban) → mark it
   delivered → check the seller balance shows the earning
   (goods − commission) → request a payout → admin approves and
   marks it paid. Then repeat the order with an API courier and an
   online payment **once those credentials exist** (§3).
7. **Wire backups before real orders exist**: `scripts/backup.sh`
   produces a gzipped `pg_dump` + SHA-256 sidecar and keeps the
   newest 14; `scripts/restore.sh` restores with sidecar
   verification (Wave 31; CI verifies dump+restore on every run).
   **What exists today is the script — scheduling it and copying
   dumps OFF the box is the operator's job** (the script's own
   header says so: "A backup file is only useful once it leaves
   this host… This script deliberately does NOT upload anywhere").
   File backups are not point-in-time recovery, and there is no
   off-site automation — decide both before launch, not after the
   first incident.
8. **Open registrations** — point the real domain at the web
   service, confirm `CORS_ORIGINS` and `NEXT_PUBLIC_API_BASE_URL`
   name it, and invite the first sellers.

## 6. Known limits at launch

The honest shortlist. Each item is a limit that is **known and
accepted for launch**, not a surprise waiting to be discovered.

- **No real load test has been run.** Wave 46 is an in-process
  regression smoke (200 concurrent requests over the public reads,
  all-200 + a pathology ceiling), explicitly **not a benchmark** —
  no capacity numbers exist. A real load test needs a deployed
  environment and remains future work.
- **Test isolation (audit CR-13) is not redesigned.** All
  integration tests share one database and truncate each other's
  tables; CI is green because test binaries run sequentially
  against it. The redesign "is a harness redesign and gets its own
  wave" (Wave 39 section) — it has not had that wave yet.
- **Most of the security audit's register is still open.** The
  audit (`docs/security-audit-2026-11-27.md`) holds **363 findings**
  (15 Critical, 91 High, 158 Medium, 99 Low). Waves 39, 41 and 44
  closed the named Critical/High **money-path** items (dev-secret
  forgery, payout race + idempotency, settlement atomicity, OTP
  caps, paid-order cancel, courier COD over-collection, checked
  money arithmetic, gateway_ref uniqueness, refund recording). The
  rest — the remaining High list (cart-locking, ship-before-payment
  gates, payout clawback/holdback, loyalty expiry fidelity, session
  revocation, per-IP throttling) plus the Medium/Low registers —
  stays queued in the audit's priority order (Wave 41 section's
  backlog paragraph). Nothing has been silently dropped; equally,
  nothing outside the named closures should be assumed fixed.
- **The frontend was never separately audited.** The audit's scope
  states it plainly: *"Frontend (`apps/web/`) — explicitly excluded…
  Any frontend vulnerability… is still unknown."*
- **Remaining Dependabot majors are post-launch upgrade waves, not
  launch blockers**: Next.js 16 (PR #14), rand 0.10 (#11), tower
  0.5 (#8), TypeScript 7 (#5), React / react-dom (#12, #10). Each
  merges only on fully green CI under the Wave 45 contract, and a
  security-relevant cargo major gets a human changelog read first.
  (jsonwebtoken 12, the one that mattered most, already landed as
  Wave 47.)
- **Two audit ignores are accepted risks, on the record.**
  `.cargo/audit.toml` carries written justifications and removal
  triggers: `rsa` 0.9.10 (RUSTSEC-2023-0071 — enters the resolved
  graph only via sqlx-mysql's welded driver; compile-time only,
  never linked into the shipped postgres-only binaries, and no code
  path reaches it — JWT is HS256-only) and `libsqlite3-sys`
  (RUSTSEC-2026-0459 — same welding mechanism; SQLite is never
  linked or opened at runtime). The crypto-bigint ignore was
  retired in Wave 47. A new ignore without a written justification
  + removal trigger is not acceptable (Wave 42 discipline).
- **SMS is dev-only until §3's first row is done** — restated here
  because it is the one limit that blocks real users outright.

## 7. If something breaks

Where the signals live, in the order an operator should look:

- **Worker summary lines.** The worker prints ONE line per cycle:
  `worker cycle: notifications sent=… retried=… failed=…
  reaped_pending=… reaped_failed=… shipments checked=… advanced=…
  unchanged=… failed=… subscriptions checked=… renewed=…
  expired=…` (Waves 29/30/36/43/48). A cycle that stops appearing
  means the worker is down; job errors print to stderr immediately
  and count as zeroes in that cycle's summary.
- **Request logs + `X-Request-Id`.** Every request emits one
  structured stdio line (`request request_id=… method=… path=…
  status=… latency_ms=…`, 5xx to stderr; path is the matched route
  pattern, query strings/headers/bodies are never logged — Wave
  43). The response echoes `X-Request-Id`, so a user's failed
  request can be traced to exactly one log line by the id they (or
  their screenshot) carry.
- **`GET /v1/admin/metrics`** (admin JWT only) — the health
  snapshot: orders today + GMV, orders by status, pending payouts,
  open returns, the **notification backlog by status** (a `sending`
  count that never drains is the stuck-batch signal — since Wave 48
  the reaper recovers stale claims automatically, so a persistent
  backlog means sends are failing, not stuck), **stale shipments**
  (API-courier shipments not polled in 15 minutes), and
  users/stores/active-product totals. The admin overview page
  renders the same snapshot as the System status card.
- **The gating cargo-audit job turning red** means a new RUSTSEC
  advisory matches the dependency tree. It is supposed to turn red
  — that pressure is intended (Wave 45). Triage in this order:
  **fix → bump past it → documented-ignore** (an ignore only with
  the Wave 42 justification + removal-trigger discipline, §6).
- **Startup refusal is a signal, not a bug.** The API exiting at
  boot with a JWT-secret error (§4) or the worker exiting without
  `DATABASE_URL` means a production guard fired. Fix the
  configuration; do not work around the guard.
