# BD Marketplace — Shopify + Amazon hybrid, Bangladesh-first

One multi-tenant system where every seller gets **both**:

- a **per-seller storefront** (the Shopify side: own shop page, own branding), and
- listing in **one central marketplace** (the Amazon side: one search, one
  unified cart, one checkout, orders auto-split per seller).

Positioned against **Daraz and Facebook F-commerce**, not abstract global
platforms. Built for Bangladesh first:

- **Bangla-first UI** (`bn` default, `en` secondary), **BDT (৳) only**
- **Phone number + OTP** as the primary login concept (Wave 2)
- **COD primary**, then bKash / Nagad / SSLCommerz / ShurjoPay (no Stripe)
- Delivery zones: **Inside Dhaka / Dhaka suburb / Outside Dhaka**
- Sellers onboard from Facebook Pages; couriers via one unified adapter
- **SIDRA** AI salesperson per store, called as an external service (not
  rewritten here)

## Architecture principle: Atomic, Individually Callable Functions

Every capability is a single-responsibility, reusable function:

- Rust domain operations are pure public functions with typed inputs/outputs
  (`calculate_delivery_charge`, `split_cart_by_seller`, `compute_commission`,
  `reconcile_cod` in later waves). Money is integer **poisha** (`i64`,
  1 BDT = 100 poisha) — never floats.
- Axum handlers are thin: parse → call **one** domain function → return.
- **One operation = one API endpoint.** Marketplace, storefront, seller,
  admin, SIDRA and a future mobile app all call the same functions.
- Couriers and payments sit behind traits with one method per operation, so
  a new courier/payment method is added without touching existing code.
- Next.js shares components/hooks (`ProductCard`, `PriceTag`,
  `useDeliveryCharge`) across all route groups — no copy-pasted page logic,
  and no money/order maths duplicated in TypeScript (the Rust API is the
  single source of truth).

## Monorepo layout

```
apps/web/               Next.js (App Router, TypeScript)
  app/(marketplace)/    central marketplace home  ->  /
  app/(storefront)/     per-seller storefront     ->  /[store]
  app/(seller)/         seller dashboard          ->  /seller
  app/(admin)/          platform admin            ->  /admin
  components/           ProductCard, PriceTag, DeliveryChargeWidget, ProductGrid
  hooks/                useAuth, useCart (only cart caller),
                        useDeliveryCharge (only caller of /v1/delivery-charge)
  lib/                  api.ts typed client (one function per endpoint),
                        formatBdt and shared helpers
crates/domain/          pure, unit-tested domain functions (no DB, no network)
crates/courier/         CourierAdapter trait + real credential-gated
                        adapters for all five API couriers (Steadfast,
                        Pathao, RedX, eCourier, Paperfly), the DEV
                        courier simulator, and the courier_adapter_for
                        factory
crates/payments/        PaymentAdapter trait + COD (records intent),
                        credential-configured gateway adapters (Bkash,
                        Nagad, SslCommerz, ShurjoPay), the DEV gateway
                        simulator + HMAC callback signing, and the
                        payment_adapter_for factory
crates/notify/          notification outbox + the SmsSender trait
                        (dev log sender + unconfigured sender;
                        sender_from_env is the one selection rule)
crates/api/             Axum API — thin handlers over service functions
  src/auth.rs           phone + OTP + JWT service functions (OTPs are
                        sent through the notify crate's SmsSender)
  src/stores.rs         store create/get service functions
  src/products.rs       product CRUD + the ONE shared listing function
  src/cart.rs           one cart per buyer: add/set/remove/clear + cart view
  src/orders.rs         checkout (one transaction, COD + online
                        methods), buyer orders, seller sub-orders +
                        status transitions, receipts
  src/payments.rs       method availability, webhook callback
                        application (the one apply path), retry,
                        dev-simulate helper
  src/reviews.rs        reviews + ratings + seller replies
  src/shipments.rs      courier availability, shipment booking,
                        tracking refresh
  src/payouts.rs        seller dashboard, the one earnings
                        aggregate, payout account + payout requests
  tests/wave2.rs        end-to-end integration test (needs DATABASE_URL)
  tests/wave3.rs        cart + split checkout integration test (same gate)
  tests/wave4.rs        weight / cancel / receipt integration test
  tests/wave5.rs        online payments integration test (same gate)
  tests/wave9.rs        dashboard / earnings / payouts test (same gate)
crates/worker/          Tokio worker binary (drains the Wave 29 notification outbox; polls courier statuses since Wave 30 via the api library)
migrations/             SQLx migrations (users, stores, otp_codes, products,
                        carts, orders, sub_orders, order_items, payments,
                        payment_events, shipments, reviews, payout_accounts,
                        payouts)
infra/docker-compose.yml  Postgres + Redis + MinIO
.env.example            every environment variable, no real values
.github/workflows/ci.yml  Rust tests/clippy (with a Postgres service),
                        Next.js build, Docker image builds, gating
                        cargo audit (since Wave 45)
```

## API endpoints

Wave 1 (stateless, no database needed):

| Method | Path | Domain function |
| --- | --- | --- |
| GET | `/health` | — (also reports whether a database is configured) |
| POST | `/v1/delivery-charge` | `calculate_delivery_charge(zone, weight_kg, cod_amount_poisha)` |
| POST | `/v1/cart/split` | `split_cart_by_seller(items)` |
| POST | `/v1/commission/quote` | `compute_commission(subtotal_poisha, rate_bps)` |

Wave 2 (Postgres-backed; return 503 when `DATABASE_URL` is not set):

| Method | Path | Auth | What it does |
| --- | --- | --- | --- |
| POST | `/v1/auth/request-otp` | — | Validate + normalize a BD phone, store a HASHED 6-digit OTP (5 min, max 5 attempts), deliver it via the SMS sender |
| POST | `/v1/auth/verify-otp` | — | Verify the OTP, create the user if new (role `buyer`), return a 7-day JWT |
| GET | `/v1/auth/me` | Bearer | The caller's own user row, re-read from the database (Wave 10: carries `is_admin`, so clients can refresh what login stored) |
| POST | `/v1/stores` | Bearer | Create a store (slug validated, duplicate slug → 409); first store upgrades the user to `seller` |
| GET | `/v1/stores/{slug}` | — | Public store profile |
| POST | `/v1/products` | Bearer (owner) | Create a product in your own store (Wave 4: accepts `weight_grams`, default 1000) |
| GET | `/v1/products/{id}` | — | Product detail (non-active products: owner only) |
| PATCH | `/v1/products/{id}` | Bearer (owner) | Update title/price/stock/status/`weight_grams` |
| GET | `/v1/products?q=&store_slug=&page=&limit=` | — | Marketplace listing: active products, title/title_bn search (ILIKE + pg_trgm), pagination |
| GET | `/v1/stores/{slug}/products` | — | Storefront listing — the SAME service function as the marketplace listing, with the store filter set |

Wave 3 (Postgres-backed, Bearer auth on everything):

| Method | Path | What it does |
| --- | --- | --- |
| POST | `/v1/cart/items` | Add a product to your cart (must be `active`; quantities accumulate, result must stay within 1..=99) |
| GET | `/v1/cart` | Your cart: lines with title/price/store + goods total |
| PATCH | `/v1/cart/items/{product_id}` | Set a line's quantity (0 removes the line) |
| DELETE | `/v1/cart/items/{product_id}` | Remove one line |
| DELETE | `/v1/cart` | Empty the cart |
| POST | `/v1/orders` | Checkout: split the cart per seller and place one order (see below) |
| GET | `/v1/orders?page=&limit=` | Your own orders, newest first, with sub-orders + items |
| GET | `/v1/orders/{id}` | One of your own orders (another buyer's order is a 404) |
| GET | `/v1/seller/sub-orders?page=&limit=` | Sub-orders for stores you own, with items + delivery details |
| PATCH | `/v1/seller/sub-orders/{id}/status` | Move a sub-order's status (owner only; the domain transition table decides legality) |

Checkout (`POST /v1/orders`) runs in ONE transaction: it locks the
cart's product rows (`FOR UPDATE`), rejects an empty cart (400),
non-active products (400) and insufficient stock (409, naming the
product), then splits by seller (`split_cart_by_seller`) and computes
every figure with `build_order_totals` — per sub-order: goods subtotal,
delivery charge (Wave 1 function), commission and seller payout (Wave 1
function, on the goods subtotal only). It writes the order, sub_orders
and order_items (title/price snapshots), decrements stock and clears
the cart — all-or-nothing. **Payment: COD only in Wave 3** — any other
`payment_method` is a 400 ("not yet available", online payments are
Wave 5). Commission rate comes from `COMMISSION_RATE_BPS` (default 800 =
8%), read once at startup — since Wave 10 an admin platform setting
overrides it at checkout time (see the Wave 10 section).

Sub-order status flow (pure domain function `can_transition_sub_order`,
unit-tested): `placed → confirmed → shipped → delivered`, plus
`placed/confirmed → cancelled` and `shipped → returned`. Everything
else is a 400.

Wave 4 (Postgres-backed, Bearer auth):

| Method | Path | What it does |
| --- | --- | --- |
| POST | `/v1/orders/{id}/cancel` | Buyer cancels their own order — only while EVERY sub-order is still `placed` (pure domain rule `can_buyer_cancel`); otherwise 409. One transaction: order + all sub-orders become `cancelled` and every item's stock is restored |
| GET | `/v1/orders/{id}/receipt` | Buyer receipt view-model (own order only): platform name, delivery details, per-store items with snapshots, totals, and the COD amount due. Commission and seller payout are deliberately NOT in this view |

**Delivery is weight-based from Wave 4** (the Wave 3 "1 kg per item
unit" assumption is gone): every product carries `weight_grams`
(migration 0008, default 1000, validated to 1..=50,000 by the domain
function `validate_weight_grams`). At checkout a seller group's parcel
weight is the sum of unit weight × quantity; `build_order_totals`
converts grams to kilograms with `weight_kg_from_grams` and bills per
started kilogram, so 2 × 2,500 g = 5 kg exactly, and 3,500 g bills as
4 kg.

**Cancellation restores stock.** The buyer cancel above restocks all
items. A seller cancelling their own sub-order (the status endpoint,
`placed/confirmed → cancelled`) now also restocks that sub-order's
items in the same transaction — previously the status changed but the
stock stayed decremented. Both paths share one restock helper inside
their transaction, so a cancel and its restock commit or roll back
together.

Wave 5 (online payments foundation):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/payments/methods` | Per-method availability (public): COD always available; bkash / nagad / sslcommerz / shurjopay available only with merchant credentials present (mode `live`) or the DEV simulator on (mode `dev`); otherwise `available: false, reason: "not_configured"`. The checkout page renders exactly this answer |
| POST | `/v1/orders` (extended) | `payment_method` may now be `cod`, `bkash`, `nagad`, `sslcommerz` or `shurjopay`. Online methods are initiated via the payments factory inside the checkout transaction: success creates a `pending` payment row and the order's `payment_status` is `pending`; an unconfigured gateway fails the whole checkout with 503 and the transaction rolls back (cart and stock untouched) |
| POST | `/v1/payments/webhook/{method}` | Gateway callback (public, signature-verified — contract below). Applies the result to the payment and the order's `payment_status` |
| POST | `/v1/payments/{id}/retry` | Buyer retries their own failed payment: `failed` → `pending`, reusing the same gateway reference |
| POST | `/v1/payments/{id}/dev-simulate-success` | DEV ONLY: signs and applies a success callback for one of the caller's dev payments, through the SAME apply function as the webhook. Returns 404 whenever dev mode is off |

**Payment state machine** (pure domain function `can_transition_payment`,
unit-tested): `pending → paid`, `pending → failed`, `paid → refunded`,
`failed → pending` (retry). The order carries the same lifecycle in
`orders.payment_status`, plus `pay_on_delivery` for COD. **COD
semantics are unchanged and explicit:** a COD order has
`payment_status = 'pay_on_delivery'` and NO row in `payments` — there
is no gateway attempt to record; cash is collected by the courier.

**Method availability rule:** a gateway method is usable only when
its merchant credentials are all present in the environment (bKash:
`BKASH_APP_KEY` + `BKASH_APP_SECRET`; Nagad: `NAGAD_MERCHANT_ID` +
`NAGAD_API_KEY`; SSLCommerz: `SSLCOMMERZ_STORE_ID` +
`SSLCOMMERZ_STORE_PASSWORD`; ShurjoPay: `SHURJOPAY_USERNAME` +
`SHURJOPAY_PASSWORD`) or when the DEV simulator is on. Adapters are
structured and credential-configured, but live gateway calls are NOT
implemented yet: with credentials present an adapter honestly reports
`NotImplemented`, and without them `NotConfigured` — nothing ever
fakes a successful payment.

**DEV gateway simulator** (`PAYMENTS_DEV_MODE=true`, local/CI only —
NEVER in production): checkout with an online method mints a
`DEV-<method>-<order_id>` gateway reference and a `/dev-pay/<ref>`
redirect path, and creates the pending payment. The checkout success
screen labels it as a dev simulation and offers a "Simulate payment
success" button, which calls the dev-simulate endpoint above; the API
signs a success callback and applies it through the same
`apply_payment_callback` service function the webhook uses — one code
path, no browser-side signing, no second apply implementation.

**Webhook contract** (`POST /v1/payments/webhook/{method}`), JSON body:

```json
{
  "payment_ref": "DEV-bkash-<order-uuid>",
  "amount_poisha": 106000,
  "status": "success",
  "signature": "<hex hmac-sha256>"
}
```

`status` is `success` or `failed`. For DEV payments the signature is
HMAC-SHA256 (hex) over `"<payment_ref>:<amount_poisha>:<status>"`
keyed with `PAYMENTS_DEV_SECRET`, computed by the single
`sign_dev_callback` / `verify_dev_callback` implementation in the
payments crate. Every callback received is written to
`payment_events` with its raw body and signature verdict BEFORE any
rejection, so rejected callbacks are auditable too. A bad signature
is a 401 with no state change; an amount that differs from the
payment amount (exact poisha equality, domain function
`validate_callback_amount`) is a 400 with no state change; a
duplicate delivery of an already-applied result returns 200 without
double effects. Real-gateway signature verification is a deliberate
TODO: it cannot be implemented without each gateway's credentials
and documented signature scheme, so callbacks for non-dev payments
are logged and rejected (401) rather than trusted.

Wave 6 (courier shipments):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/couriers` | Per-courier availability (public): Steadfast available with mode `live` when its merchant credentials are set, any known courier available with mode `dev` when the DEV courier simulator is on; otherwise `available: false, reason: "not_configured"`. The seller page renders exactly this answer |
| POST | `/v1/seller/sub-orders/{id}/shipment` | Book a courier shipment for one of your CONFIRMED sub-orders (409 while `placed`, 409 if already booked, 503 when the courier is unavailable). Body `{ "courier": "steadfast" }` is optional — the configured default (`DEFAULT_COURIER`) is used otherwise. On success the shipment row, its `create` event and the sub-order's move to `shipped` commit in one transaction |
| POST | `/v1/shipments/{id}/refresh` | Poll the courier for the shipment's current status (the sub-order's seller or the order's buyer only; anyone else gets a 404). A legal change is applied, logged in `shipment_events` with the courier's raw status, and syncs the sub-order on `delivered` / `returned` |

**Courier availability rule:** Steadfast is bookable only when its
platform merchant credentials are set (`STEADFAST_API_KEY` +
`STEADFAST_SECRET_KEY`; `STEADFAST_BASE_URL` overrides the API base
URL, default `https://portal.packzy.com/api/v1`) or when the DEV
courier simulator is on. Platform master accounts are the model:
sellers never need a courier account of their own.

**Steadfast adapter** (the first real courier adapter): booking is
`POST /create_order` with `Api-Key` / `Secret-Key` headers and an
invoice, recipient name/phone/address, a note, and the COD amount —
Steadfast's API takes taka, so integer poisha is converted at that
boundary only (`poisha_to_taka`, 4,690 poisha = 46.90); money stays
integer poisha everywhere else. Tracking is
`GET /status_by_cid/{consignment_id}`; the raw `delivery_status` is
mapped by the single domain function `map_steadfast_status`
(`pending` → `created`, `hold` → `in_transit`,
`partial_delivered` keeps its own status, anything unrecognised →
`unknown`). Two honest limits: Steadfast's basic API has **no cancel
endpoint** for a created consignment, so the adapter's `cancel`
returns `Unsupported` rather than pretending; and `price_estimate`
is a purely local estimate from the domain delivery-charge function
under our own tariff — no network call, and not a quote from
Steadfast. Pathao and RedX became real adapters in Wave 7 (below),
and eCourier and Paperfly in Wave 11 (further below).

**DEV courier simulator** (`COURIER_DEV_MODE=true`, local/CI only —
NEVER in production): booking mints a
`DEV-<courier>-<sub_order_id>` consignment id, and each tracking
poll advances the parcel one step — `picked_up`, `in_transit`,
`out_for_delivery`, `delivered`. The poll counts are in-memory,
process-wide state (adapters are built per call), so progression
resets when the API restarts. Simulated shipments are not real
bookings; the seller UI labels dev couriers as such.

**Shipment state machine** (pure domain function
`can_transition_shipment`, unit-tested): `created → picked_up →
in_transit → out_for_delivery → delivered`, plus
`out_for_delivery → partial_delivered → delivered | returned`, the
return path `delivered → return_in_transit → returned`, and
cancellation from any non-final state (`delivered` itself is not
cancellable — the return path is the only way back). `unknown`
never transitions automatically: a refresh that reports an
unrecognised or backwards status leaves the stored shipment
untouched and writes no event. **One shipment per sub-order** is a
hard rule (UNIQUE on `shipments.sub_order_id`): the COD amount the
courier collects is the sub-order's goods + delivery share for
pay-on-delivery orders, and 0 for online-paid orders. Order and
seller sub-order views now embed a `shipment` summary (`id`,
`courier`, `consignment_id`, `status`) per sub-order.

Wave 7 (Pathao + RedX adapters, smart routing):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/couriers/estimate?zone=inside_dhaka&weight_grams=1000` | Per-courier delivery-charge estimates for one zone + weight (public): each available courier's local-tariff estimate in poisha, plus `recommended` — the smart-routing pick over the same numbers (`null` when no courier is available). An unavailable courier has NO price at all |
| POST | `/v1/seller/sub-orders/{id}/shipment` | The Wave 6 booking endpoint, extended: the body may be `{ "courier": "auto" }` for smart routing (the shipment stores the CHOSEN courier's name, never the literal `auto`), and may carry `{ "area_id": 123 }` — the recipient's numeric area id that RedX requires |

**Pathao adapter** (real, credential-gated): authentication is an
OAuth-style client-credentials exchange —
`POST /aladdin/api/v1/issue-token` with `PATHAO_CLIENT_ID` +
`PATHAO_CLIENT_SECRET` returns a bearer token, which the adapter
caches and reuses until the token response's own `expires_in` says
otherwise (one-hour fallback; Pathao's docs describe ~30-day
tokens, and we deliberately do not hard-code that lifetime).
Booking is `POST /aladdin/api/v1/orders` with the bearer token: the
merchant's numeric store id (`PATHAO_STORE_ID`), our invoice as
`merchant_order_id`, the recipient phone converted to the 11-digit
local form Pathao requires (the platform stores `+880` canonical;
the single domain conversion `to_local_phone` does it), the parcel
weight in kg, `delivery_type` 48, and the COD amount — Pathao
collects WHOLE taka, so poisha are rounded half-up at that boundary
only (`poisha_to_whole_taka`: 150 poisha → 2 taka). Tracking is
`GET /aladdin/api/v1/orders/{consignment_id}`; the raw
`order_status` is mapped by the single domain function
`map_pathao_status` (`FailedDelivery` → `out_for_delivery` — a
failed attempt on a delivery run, which Pathao retries; `OnHold` →
`in_transit`, the same paused-in-the-pipeline choice as Steadfast's
hold). Pathao's merchant API has **no simple cancel endpoint**, so
`cancel` returns `Unsupported` honestly. `price_estimate` is the
local domain estimate, NOT Pathao's price-plan endpoint: that
endpoint is an authenticated POST, and calling it per routing
decision would make routing network-dependent and slow.

**RedX adapter** (real, credential-gated): booking is
`POST /parcel` — customer name/phone/address (phone in the local
11-digit form), the cash collection amount in taka, the parcel
weight in GRAMS, our invoice as `merchant_invoice_id`, and a
numeric `area_id`. RedX area ids are RedX's own and there is no
honest default: a booking without the recipient's `area_id` fails
loudly with a gateway error before any network call, rather than
guessing an area. Tracking parses the status defensively from the
known field names (`status`, `parcel_status`, `current_status` —
top level first, then under `data`) and maps it with the single
domain function `map_redx_status` (RedX reports human strings like
`out-for-delivery`; only confidently known strings are mapped, the
rest are `unknown`). RedX's open API documents **no cancel call**,
so `cancel` returns `Unsupported`. **Two uncertainties to confirm
at merchant onboarding:** the exact `API-ACCESS-TOKEN` header
format (the adapter sends the raw API key as the header value, per
RedX's docs style; the live API might expect a `Bearer <key>`
form) and the exact tracking endpoint path (the adapter uses
`GET /tracking/{tracking_id}`). Each is isolated to one header
line / one URL line when the answer is known.

**Smart routing rule:** among the couriers AVAILABLE for the
parcel (credentials complete, or the DEV simulator on), the lowest
local-tariff estimate wins; ties break by platform priority —
steadfast, then pathao, then redx, then ecourier, then paperfly.
The rule is the pure domain function `choose_courier`
(unit-tested), and the estimate endpoint and booking's `auto`
resolution read the SAME quotes through the one shared
`resolve_booking_courier` function, so they can never disagree.
Honest limit: the prices are our own tariff's estimates (the domain
delivery-charge function), not live courier quotes — couriers
confirm their real charge on the consignment after booking. When
nothing is available, an `auto` booking is a 503, not a silent
fallback. Availability itself is now one shared per-courier rule:
`GET /v1/couriers` reports Pathao `live` when `PATHAO_CLIENT_ID` +
`PATHAO_CLIENT_SECRET` + `PATHAO_STORE_ID` are all set, and RedX
`live` when `REDX_API_KEY` is set.

**Adapter verification status (honesty):** the Pathao and RedX
adapters are coded to the documented API shapes and their response
parsers are fixture-tested in the courier crate, but they CANNOT
be verified against the live services until platform merchant
accounts exist. Without complete credentials every network
operation returns `NotConfigured` and nothing is sent — the same
rule as Steadfast and the payment gateways.

Wave 11 (eCourier + Paperfly adapters — all five API couriers complete):

No new endpoints: eCourier and Paperfly appear in
`GET /v1/couriers`, `GET /v1/couriers/estimate` and smart routing
automatically, because availability and estimates iterate the known
couriers and read each one's credential rule from the courier crate
— the Wave 7 generalization needed only the two new match arms in
the one availability helper. Bookings name them exactly like the
other couriers (`{ "courier": "ecourier" }` /
`{ "courier": "paperfly" }`, or let `auto` pick them).

**eCourier adapter** (real, credential-gated): authentication is
two headers — the API auth token (`ECOURIER_API_KEY`) and the
merchant user id (`ECOURIER_USER_ID`); both must be set before
eCourier reports `live`. Booking is `POST /order` — recipient name,
the mobile converted to the 11-digit local form (the platform
stores `+880` canonical), the address, the collection amount as
`product_price` in WHOLE taka (poisha rounded half-up at that
boundary only), the weight in kg, our invoice as
`merchant_order_id`, the note as `comments`, and a numeric
`area_id`. eCourier area ids are eCourier's own and there is no
honest default: a booking without the recipient's `area_id` (the
same booking-body field RedX uses) fails loudly with a gateway
error before any network call. Tracking is
`GET /track?tracking={consignment_id}`; the raw status is mapped by
the single domain function `map_ecourier_status`. eCourier's basic
API documents **no cancel call**, so `cancel` returns
`Unsupported`. `price_estimate` is the local domain estimate, not
an eCourier quote.

**Paperfly adapter** (real, credential-gated): authentication is
HTTP Basic (`PAPERFLY_USERNAME` + `PAPERFLY_PASSWORD`) PLUS the
merchant key (`PAPERFLY_KEY`) in a header; all three must be set
before Paperfly reports `live`. Booking is
`POST /OrderPlacement` — our invoice as `merInvoiceNo`, the
customer name/phone (local form)/address, the collection amount in
WHOLE taka, a generic `productName` naming the order (a sub-order
can hold several products, so there is no single honest title),
quantity 1, the weight as a kg string, and the pickup point from
platform config: `PAPERFLY_PICKUP_NAME` / `PAPERFLY_PICKUP_PHONE` /
`PAPERFLY_PICKUP_ADDRESS`. A master account collects from one
place, so the pickup point is config, never invented per order: a
booking with any pickup field missing fails loudly BEFORE any
network call, naming every missing variable. Tracking POSTs the
tracking id as JSON to `/API-Order-Tracking`; the raw status is
mapped by the single domain function `map_paperfly_status`.
Paperfly's API documents **no cancel call**, so `cancel` returns
`Unsupported`. `price_estimate` is the local domain estimate, not
a Paperfly quote.

**Onboarding-confirm items (flagged, not assumed):** eCourier's
exact auth header NAMES — the adapter sends the token as
`API-AUTH-TOKEN` and the user id as `USER-ID`, and eCourier doc
versions differ on the names; Paperfly's merchant-key header name
— the adapter sends `paperflykey` — and its tracking call's verb
and path (the adapter POSTs JSON to `/API-Order-Tracking`). The
create-response parsers are defensive for the same reason:
eCourier's booked-order id is accepted as `order_id`, `tracking`,
`consignment_id` or `e_id` (top level or under `data`, string or
number), Paperfly's tracking id as `trackingId`, `tracking_id` or
`refNo`, and both tracking parsers accept the status under
`status`, `order_status` or `current_status`. Each uncertainty is
isolated to one header line / one URL line / one field list when
the answer is known at onboarding.

**Adapter verification status (honesty):** with Wave 11, all five
API couriers — Tier 1 Steadfast, Pathao, RedX and Tier 1b eCourier,
Paperfly — have real adapters behind the one trait, coded to their
documented API shapes with fixture-tested parsers. NONE of the five
can be verified against its live service until platform merchant
accounts exist; without complete credentials every network
operation returns `NotConfigured` and nothing is sent. The manual
couriers — Sundarban, SA Paribahan, Karatoa, Carrybee — have no
merchant APIs to adapt; since Wave 19 they run as a seller-driven
manual workflow behind the same shipment system (see the Wave 19
section), not as adapters and never as a faked API.

Wave 12 (Facebook Page linking + post-to-product import):

Bangladeshi sellers run their shops as Facebook Pages: the
catalogue already exists as Page posts. This wave builds the
whole import pipeline around that fact — a seller links their
Page, pastes post texts, reviews the parsed drafts, and imports
them as real products.

| Method | Path | What it does |
| --- | --- | --- |
| PUT | `/v1/seller/store/facebook` | Bearer, own store. Link a Facebook Page: `{page_id, page_name}` (page id 1–64 letters/digits, name 1–120 chars). Returns the store view |
| DELETE | `/v1/seller/store/facebook` | Bearer, own store. Unlink (both fields nulled) |
| GET | `/v1/seller/facebook/status` | Bearer. `{linked, page_id, page_name, graph_api}` — the link state and the Graph API state are reported independently |
| POST | `/v1/seller/products/import-preview` | Bearer. Parse up to 50 pasted posts into editable drafts. Writes NOTHING |
| POST | `/v1/seller/products/import` | Bearer. Create products from the reviewed drafts; invalid or already-imported drafts come back in `skipped` with reasons |
| POST | `/v1/seller/products/import-from-page` | Bearer. The Graph API source — honestly a 503 / 501 stub (see below) |

**The parser** (pure domain functions `bn_to_en_digits`,
`extract_price_poisha`, `draft_from_fb_post`, all unit-tested):
Bengali digits are converted to ASCII first. A price is a number
carrying a currency marker — `৳`, `টাকা`, `tk` or `BDT` adjacent
to the number on either side — or the first number on a line
that opens with দাম / "price". Numbers may group thousands with
commas and carry a decimal part. When several labelled prices
exist, the FIRST one wins. A bare number with no marker is
NEVER a price: phone numbers, stock counts and dates must not
turn into prices, and a labelled candidate above a sanity
ceiling (100,000,000 taka) is skipped as implausible. The draft
title is the post's first non-empty line (max 120 chars); the
description is the whole post (max 2000 chars). A draft is valid
when it passes the shared product validator AND names a real
price — a missing price, or a price of zero, is an error the
seller fixes in the preview, never a guessed number.

**The import** validates every draft before writing anything,
then inserts the survivors in ONE transaction through the same
`insert_product` service function the create endpoint uses,
stamped `source = 'facebook'` and created `active` (importing
is an explicit publish action — the seller reviewed each draft
first). Products carry provenance from this wave on:
`source` (`manual` / `facebook`) and `source_ref` (for imports,
the post id the seller supplied). **Re-import is idempotent:**
a draft whose `source_ref` the store already has is skipped as
"already imported", and a partial UNIQUE index on
`(store_id, source_ref)` (migration 0014) backstops the rule in
the database.

**Graph API status (honesty):** linking stores only the Page's
identity — no tokens exist anywhere in this wave. Fetching a
seller's posts directly needs a registered Meta app (with Meta's
app review) plus a page access token from an OAuth flow; neither
exists yet. `GET /v1/seller/facebook/status` reports
`graph_api: "configured"` only when both `FACEBOOK_APP_ID` and
`FACEBOOK_APP_SECRET` are present in the API's environment
(presence only — the values are never stored or returned), and
`import-from-page` answers 503 while they are absent and 501
while the page-token OAuth flow is still missing. It never
returns posts it did not fetch; the paste pipeline above is the
real import source until that later wave lands.

Wave 13 (buyer <-> store messaging + the SIDRA bridge):

Buyers ask questions before they buy — on Facebook, that is a
Page inbox; here it is a real in-platform inbox, plus the bridge
that lets the external SIDRA AI salesperson answer inside it.
SIDRA itself is a separate service and is NOT reimplemented
here; this wave is the platform side of the contract only.

| Method | Path | What it does |
| --- | --- | --- |
| POST | `/v1/conversations` | Bearer. Find-or-create the caller's OPEN conversation with `{store_id, product_id?}`: 201 when created, 200 when the open one is reused. A suspended store is a 404 |
| GET | `/v1/conversations` | Bearer. Every conversation the caller takes part in — as buyer and as seller — merged, newest activity first; each carries the caller's `role`, an 80-char preview of the newest message, and the caller's unread count |
| GET | `/v1/conversations/{id}` | Bearer, participants only (anyone else: 404). The thread (newest 200 messages, oldest first). Reading stamps the caller's read marker |
| POST | `/v1/conversations/{id}/messages` | Bearer, participants only. Post as your own side (the sender type comes from your role, never the request); body 1–2000 chars. A closed conversation is a 409 |
| POST | `/v1/conversations/{id}/close` | Bearer, participants only. Close the conversation (idempotent; history stays readable) |
| POST | `/v1/sidra/reply` | Public, signature-verified (see the bridge contract). 503 while the bridge is unconfigured, 401 on a bad signature |
| GET | `/v1/seller/sidra/suggestions` | Bearer, own store. The pending SIDRA suggestions, oldest first |
| POST | `/v1/seller/sidra/suggestions/{id}/send` | Bearer, own store. Approve: the suggestion's words enter the thread as an `ai` message. A decided suggestion is a 409 |
| POST | `/v1/seller/sidra/suggestions/{id}/dismiss` | Bearer, own store. Reject a pending suggestion (409 once decided) |
| PUT | `/v1/seller/store/sidra` | Bearer, own store. `{auto_reply: bool}` — set the store's SIDRA mode; returns the store view |
| GET | `/v1/seller/sidra/status` | Bearer. `{configured, auto_reply}` — the platform bridge state and the store's mode, reported independently |

**The messaging model:** one OPEN conversation per (store,
buyer, product) triple, enforced by the find-or-create service
function (the nullable `product_id` keeps a database unique
index from expressing the rule — see migration 0015). Closing a
conversation ends the triple's reuse: the next find-or-create
starts a fresh one. Read state is two markers on the
conversation (`buyer_last_read_at` / `seller_last_read_at`);
a side's unread count is the other side's messages newer than
its marker. AI messages count as unread for the buyer and never
for the seller (the seller approved them, or opted into
auto-reply). Buyer display follows the reviews rule: name when
set, else the masked phone.

**The SIDRA bridge contract:** both directions are signed with
HMAC-SHA256 (hex) over ONE canonical payload defined in the
domain crate (`sidra_signature_payload`):
`<conversation_id>:<message_id>:<text>`.

Outbound: when a buyer posts a message and the bridge is
configured, the API POSTs `{conversation_id, store_id,
buyer_message_id, body, product_id}` to `SIDRA_API_URL` with an
`X-Sidra-Signature` header (the message id and body in the
payload's slots). The notification is **best effort**: it fires
in a detached task, a failure is logged, and the buyer's
request never fails because SIDRA is down.

Inbound: SIDRA POSTs `{conversation_id,
in_reply_to_message_id, reply_text, signature}` to
`/v1/sidra/reply`, the signature covering the reply text and
the buyer message it answers. A bad signature is a 401 and
nothing is stored; an unknown conversation or answered message
is a 404; a closed conversation is a 409. A valid reply lands
by the store's mode: `sidra_auto_reply` on → it is inserted
directly as an `ai` message (`{mode: "sent"}`); off → it waits
as a pending suggestion (`{mode: "suggested"}`) until the
seller sends or dismisses it from the inbox. Even a
seller-approved suggestion enters the thread labelled `ai` —
the words are SIDRA's. Every callback outcome, accepted or
rejected, is logged to stderr; there is deliberately no audit
table, because no money moves on a chat reply — the stored
message or suggestion is itself the record.

**Honest limits:** the bridge needs BOTH `SIDRA_API_URL` and
`SIDRA_WEBHOOK_SECRET`; a half-configuration is treated as
unconfigured (with a startup warning), the callback answers
503, and the inbox works completely as a human chat — SIDRA is
an enhancement, never a dependency. The web UI polls the
thread every 10 seconds; there is no websocket or push yet.
Outbound delivery is not retried: if SIDRA misses a
notification it can still answer later through the callback
only for messages it knows about, so a missed notification can
mean a missed AI reply — the seller's own inbox is unaffected.

Wave 14 (coupons and promotions):

Two coupon scopes, one code namespace. A **platform coupon**
(admin-created, `store_id` NULL) discounts the whole order's
goods; a **store coupon** (seller-created) discounts only that
store's goods. Codes are case-insensitive (stored upper-case)
and unique across BOTH scopes — a buyer types one code at
checkout and it must mean exactly one coupon. A coupon is a
percent (1–90, optionally capped at `max_discount_poisha`) or a
fixed poisha amount, with an optional minimum eligible subtotal,
an optional active window (`starts_at` / `ends_at`), an optional
global `usage_limit`, and an `active` flag.

| Method | Path | What it does |
| --- | --- | --- |
| POST | `/v1/coupons/preview` | Bearer. Stateless check: `{code, goods_total_poisha, store_id?, store_goods_poisha?}` → `{valid, code, scope, store_id, discount_poisha, reason?}`. Writes nothing, reserves nothing |
| POST | `/v1/seller/coupons` | Bearer, own store. Create a store coupon; a code that exists in any scope is a 409, invalid fields a 400 |
| GET | `/v1/seller/coupons` | Bearer, own store. The store's coupons, newest first, with `used_count` |
| PATCH | `/v1/seller/coupons/{id}` | Bearer, own store (another store's coupon is a 404). `{active}` toggle |
| DELETE | `/v1/seller/coupons/{id}` | Bearer, own store. Delete an unused coupon; one with redemptions is a 409 — deactivate it instead |
| POST | `/v1/admin/coupons` | Admin. Create a platform coupon (same validation; 409 on a duplicate code) |
| GET | `/v1/admin/coupons` | Admin. Every coupon on the platform, newest first |
| PATCH | `/v1/admin/coupons/{id}` | Admin. `{active}` toggle for any coupon |
| POST | `/v1/orders` | Checkout accepts an optional `coupon_code`; see the checkout rule below |

**The money design (decided once, applied everywhere through
the pure domain functions):**

- **Who bears the discount decides the commission basis.** A
  store coupon is the seller's own promotion, so the seller
  bears it: that sub-order's commission is computed on
  (goods − discount). A platform coupon is the platform's
  promotion, so the platform bears it: commission stays on the
  FULL goods, and the platform's loss is exactly the discount.
  In domain terms each order group carries its allocated
  `discount_poisha` plus a `commission_on_discounted` flag
  (set exactly when the coupon is a store coupon), and
  `build_order_totals` stays the one pure totals function.
- **Allocation:** the order-level discount is split across the
  in-scope sub-orders proportionally to their goods subtotals
  (`allocate_discount`: integer-floor shares, the rounding
  remainder goes one poisha at a time to the largest subtotal
  first), so the shares sum EXACTLY to the discount — no poisha
  is created or lost. Each sub-order snapshots its share in
  `discount_poisha`.
- **Snapshots:** every sub-order's `seller_payout_poisha` is
  goods − discount − commission, whoever bore the discount —
  the payout snapshot always nets the discount out, because the
  buyer really paid less. The buyer pays goods − discount +
  delivery; the COD due and any online payment amount derive
  from that one discounted grand total.
- **Earnings switch (Wave 9 → 14):** seller earnings originally
  recomputed `SUM(goods − commission)` over delivered
  sub-orders. They now sum the `seller_payout_poisha` SNAPSHOT
  instead. For pre-coupon orders the two are identical; after
  coupons exist, a recompute would silently pay a seller for a
  discount they bore. One aggregate still feeds the dashboard,
  the balance and payout creation.

**The preview contract:** the preview is honest about what it
can know before checkout. A platform coupon needs only the
cart's whole goods total. A store coupon's discount depends on
THAT store's goods in the cart, which a bare total cannot
express — so the first preview answers with `scope: "store"`
and the coupon's `store_id`, and the client re-previews passing
`store_goods_poisha` (the checkout page does exactly this). The
preview and checkout share ONE rejection rule
(`coupon_rejection_reason`: inactive / not started / expired /
usage exhausted / not applicable / below the minimum, in that
order), so they cannot disagree about validity — time and usage
races aside.

**The checkout rule:** with a `coupon_code`, checkout loads the
coupon row `FOR UPDATE` inside the order transaction and
re-validates it. Any rejection fails the WHOLE checkout with a
400 and the cart stays intact — there is no silent
"coupon ignored" order. On success the same transaction writes
the order's `coupon_code` / `discount_total_poisha`, each
sub-order's allocated share, the `coupon_redemptions` row, and
`used_count + 1` — so a rolled-back checkout never consumes a
use, and the usage limit cannot be raced.

**Lifecycle limits (stated, not hidden):** there is NO
per-buyer usage limit in this wave — the optional global
`usage_limit` is the only cap, so one buyer may redeem the same
coupon on many orders while it lasts. Coupons never discount
delivery, never stack (one code per order), and a coupon whose
redemptions exist cannot be deleted, only deactivated.

Wave 15 (product search — Postgres-native):

Search runs entirely on Postgres: **pg_trgm similarity + ILIKE**
over the product text, backed by the trigram GIN indexes
(migration 0004 for the per-column title indexes, migration 0017
for the combined title index and the description index). There
is deliberately **no external search service** — no extra
process to deploy, secure and keep in sync. The two endpoints
below are the whole search contract, so if the catalogue ever
outgrows Postgres, a dedicated engine (Meilisearch or similar)
can be put behind the same endpoints without clients changing.

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/search` | Public. Query params: `q`, `store_slug`, `category`, `min_price_poisha`, `max_price_poisha`, `sort`, `page`, `per_page` → `{items, total, page, per_page}`; `items` are the shared product view (Wave 8 rating summary included). Invalid parameters are a 400 |
| GET | `/v1/search/suggest` | Public. `?q=` → `{products: [{id, title}], stores: [{slug, name}]}` — up to 8 distinct product titles and 4 stores; below 2 characters both lists are empty |

**Matching and ranking.** With `q` present, a product matches
when the text appears in its title, Bangla title or description
(ILIKE contains, metacharacters escaped by the domain crate's
`search_like_pattern`), or when the combined title text is
trigram-similar to it (similarity ≥ 0.2 — the typo tolerance
this wave offers). Relevance order comes from ONE rank
expression in the items query: exact title match (English or
Bangla) first, then title prefix, then title substring, then a
small boost for description-only matches, with the pg_trgm
similarity of `title || ' ' || title_bn` breaking remaining
ties. Sorts: `relevance` (the default with text), `newest`
(the default without), `price_asc`, `price_desc`, `rating`
(highest rated first, unreviewed last). All parameter rules —
text ≤ 100 chars, non-negative bounds, min ≤ max, page ≥ 1,
per_page 1..=48 — live in the domain crate's
`validate_search_params`, so they are unit-tested pure rules.

**Filters compose with AND:** `category` (case-insensitive
exact match on the label), the inclusive price range, and
`store_slug` (the storefront's within-store search rides on
this). Visibility is the strict public rule — only `active`
products of `active` stores, with NO owner exception: search
is a discovery surface, and drafts or suspended-store products
never appear (sellers manage drafts through the listing
endpoints). The Wave 2 `GET /v1/products` listing is unchanged;
search is additive.

**Categories (migration 0017):** `products.category` is a
free-text seller label — optional, at most 60 characters
(domain `validate_category`), set on product create/PATCH and
in the seller product form. It is deliberately NOT a taxonomy
table yet; the filter matches the label exactly
(case-insensitively), so sellers should reuse spellings.

**Limits (stated, not hidden):** typo tolerance is only what
trigram similarity gives — no stemming, no synonyms, no
Bangla transliteration matching (searching "shari" does not
find "শাড়ি"). Faceting is exactly the filters listed above —
no per-category counts, no brand/size attributes. Ranking is
the one fixed expression, not personalised.

Wave 16 (public tracking + returns / RTO):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/track?order_id=&phone=` | PUBLIC, no login. The tracking timeline: per-store sub-order status + items, and each shipment with its full event history. The delivery phone given at checkout is the credential — an unknown id, a malformed id, a missing phone and a wrong phone all answer the SAME 404 |
| POST | `/v1/buyer/sub-orders/{id}/return-request` | Bearer, the order's buyer only (anyone else: 404). Body `{reason}` (1–500 chars). Only for a `delivered` sub-order (else 409); one request per sub-order (a second is 409). 201 with the request |
| GET | `/v1/buyer/return-requests` | Bearer. The buyer's own requests with statuses + refund fields. The buyer order views also summarise each sub-order's `return_status` |
| GET | `/v1/seller/return-requests` | Bearer, own stores only. Newest first, with the buyer's MASKED phone and the sub-order's items |
| POST | `/v1/seller/return-requests/{id}/approve` | Bearer, store owner. `requested` → `approved`; optional `{note}` for the buyer. Illegal moves are 409 |
| POST | `/v1/seller/return-requests/{id}/reject` | Bearer, store owner. `requested` → `rejected`; optional `{note}` |
| POST | `/v1/seller/return-requests/{id}/received` | Bearer, store owner. `approved` → `received` in ONE transaction: refund snapshot + refund `pending` + sub-order `returned` + items restocked |
| GET | `/v1/admin/return-requests?refund_status=` | Admin. Every request; the refund queue filters `refund_status=pending` |
| POST | `/v1/admin/return-requests/{id}/refund-done` | Admin. Records that a `pending` refund was paid off-platform; from any other state it is a 409 |

**The tracking contract:** the endpoint is public because
tracking links get shared (family, SMS) where no session
exists. The order id is only an identifier; the delivery
phone is the knowledge factor, compared through the pure
domain rule `phones_match_for_tracking` (both sides
normalized, so `01...`, `880...` and `+880...` all match).
Every failure is the same 404 with the same body — the
endpoint never confirms that an order exists, so it cannot
be used as an oracle for enumerating order ids. The response
is a purpose-built view: no delivery address, no commission,
no payout split.

**The return state machine:** `requested` → `approved` /
`rejected` (the seller decides), `approved` → `received`
(the goods came back) — the pure domain table
`can_transition_return`; skips like `requested` → `received`
are 409, and `rejected` / `received` are terminal for the
request. The refund has its OWN lifecycle on the same row
(`none` → `pending` → `done`), so a received return whose
refund is still outstanding is representable without fake
request states.

**Refund math:** the refund snapshotted at receipt time is
`compute_refund_poisha(goods, discount)` — what the buyer
paid for the goods, minus any coupon discount allocated to
the sub-order (never paid, so never refunded). The DELIVERY
charge is never refunded: the courier trip happened and was
paid for, whichever way the parcel travelled.

**RTO restock (fixed in this wave):** when a shipment
refresh syncs a sub-order to `returned` (the courier brought
the parcel back), the refresh transaction now restocks the
sub-order's items through the same shared restock helper the
cancels and received returns use. Before this fix the
sub-order was marked `returned` and the stock was silently
lost. A courier return creates NO return request and NO
refund: a COD parcel was never paid for, and a prepaid one
is refunded by manual platform decision, never as an
automatic side effect of a courier status.

**Limits (stated, not hidden):** a refund marked `done`
records that the platform refunded the buyer OFF-platform
(bKash / cash) — no gateway refund API is wired (the Wave 5
TODO stands), and nothing here claims a gateway moved
money. One return per sub-order and the whole sub-order is
the unit: partial-item returns are not supported yet.
Buyers cannot cancel into a return — undelivered parcels go
through cancellation or the courier's RTO path, never a
return request (`can_request_return` demands `delivered`).

Wave 17 (partial advance for COD + COD fraud shield):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/payments/advance-policy` | PUBLIC. The effective advance policy: `{percent_bps, threshold_poisha}` (admin setting over `ADVANCE_PERCENT_BPS` / `ADVANCE_THRESHOLD_POISHA` over the defaults 2000 / 200000). Checkout shows it as an early warning; a buyer's personal risk-forced advance is never visible here |
| GET | `/v1/seller/buyer-risk?phone=` | Bearer, store owners only (403 otherwise). The fraud shield's verdict for one phone: `{found, total_orders, delivered, returned_rto, buyer_cancelled, risk_level, requires_advance}`. An unknown phone answers `found: false` with zeroed counts |
| PUT | `/v1/admin/settings` | Admin. Now also accepts `advance_percent_bps` (0–10000) and `advance_threshold_poisha` (≥ 0), stored as `platform_settings` rows that beat the startup values at read time |
| POST | `/v1/orders` | Checkout gains `advance_method` (bkash / nagad / sslcommerz / shurjopay): the online method for a COD advance, when one is required (see the decision below) |
| POST | `/v1/seller/sub-orders/{id}/shipment` | Booking gains a guard: a sub-order whose parent order is `advance_pending` answers 409 "advance payment pending" until the advance settles |

**The advance decision (checkout, COD only):** the buyer's
risk verdict comes from the ONE aggregate in the `risk`
module (`buyer_risk_for_user` — checkout, the buyer-risk
endpoint and the seller sub-order list all read it), and the
pure domain rule `compute_advance_poisha` turns the effective
policy into an amount: the rate's share of the grand total,
rounded up, owed only when the total reaches the threshold —
or at ANY size when the shield forces it. When an advance is
required, exactly one of three things happens:

- **A gateway can collect it** (merchant credentials present,
  or the DEV simulator) **and the buyer named a method:** the
  order is stored `advance_pending`, and a `pending` payment
  row for the ADVANCE amount is initiated through the same
  payments factory as a full online checkout. A method whose
  adapter cannot take it fails the whole checkout with a 503
  (cart intact), the same honesty rule as online checkout.
- **A gateway can collect it but NO method was named:** 400,
  telling the buyer to choose an advance payment method. The
  checkout page turns that 400 into the method picker and a
  re-place.
- **NO gateway can collect it** (the platform today, without
  credentials): the order proceeds as plain COD with
  `advance_required_poisha` recorded on it — a visible risk
  flag the seller surfaces show ("অগ্রিম নেওয়া হয়নি"). This
  is a deliberate safety valve, NOT a fake success: no payment
  row exists, `advance_paid_poisha` stays 0, and no surface
  claims money was collected. It is also what keeps checkouts
  working in deployments (and test suites) where gateways are
  not configured yet.

**Settlement:** the advance settles through the ONE shared
callback (`apply_payment_callback`) — a branch recognises an
advance payment (COD order, payment amount exactly the
required advance, order still `advance_pending`) and moves
the order to `advance_paid` with `advance_paid_poisha` set —
never to `paid`, because the balance is still COD. A failed
advance attempt leaves the order `advance_pending` (the
advance is still owed), and retrying the payment restores
`advance_pending` rather than the generic `pending`. The
receipt's cash-due is `grand_total − advance_paid`, and a
shipment booked after settlement tells the courier to
collect the sub-order's share net of its slice of the
advance. Cancelling an `advance_pending` order is a plain
buyer cancel; its payment row stays `pending` as the audit
record (no money ever moved).

**The fraud shield (pure domain, `assess_buyer_risk`):**

| Signal | Rule |
| --- | --- |
| `requires_advance` | ≥ 2 returned sub-orders AND returns ≥ half of completed (delivered + returned) |
| `high` | `requires_advance`, OR ≥ 3 orders with 0 delivered and ≥ 2 buyer-cancellations/returns |
| `medium` | any return or buyer cancellation at all |
| `low` | otherwise (including a brand-new buyer) |

`returned_rto` counts every sub-order in the `returned`
state (courier RTO and received buyer returns alike);
`buyer_cancelled` counts only sub-orders of orders the buyer
cancelled wholesale — a seller cancelling one sub-order of a
live order never counts against the buyer.

**Limits (stated, not hidden):** the advance is collected
through the same credential-gated gateways as full payments,
so until merchant credentials exist the only ways to actually
collect one are the DEV simulator or the degradation path
above. Risk stats count this platform's history only — a
buyer who burned other platforms starts here with a clean
record. The shield never blocks an order outright; it only
prices the risk into an advance.

Wave 18 (storefront themes & store branding):

| Method | Path | What it does |
| --- | --- | --- |
| PUT | `/v1/seller/store/branding` | Bearer, store owner (a user without a store gets the usual 404). Sets the caller's own store branding; returns the shared store view. The public `GET /v1/stores/{slug}` view carries the same fields, so the storefront renders from the one shape |

**The branding fields (migration 0020):** `theme_key`
(`classic` until the seller picks one), `brand_color`,
`logo_url`, `banner_url` and `announcement` are new columns
on `stores`; the store `description` from migration 0002 is
reused, not re-added. Every value is written through the
pure domain validators (`validate_theme_key`,
`validate_hex_color`, `validate_branding_url`,
`validate_announcement`, `validate_store_description`): a
theme is one of the three keys exactly; a colour is `#RRGGBB`
and is stored normalized to UPPERCASE (`#ff5722` comes back
`#FF5722`); an image URL must be an absolute `http(s)` URL of
at most 500 characters with no spaces; an announcement is at
most 120 characters and a description at most 1,000, both
counted in Unicode characters, not bytes, so Bangla text is
measured by what a visitor reads. An invalid write is a 400
and changes nothing.

**The write convention** is the codebase's PATCH convention
(the one product updates use): a field absent from the body —
or sent as JSON `null` — keeps its stored value; a field
sent as a string is validated and stored; for the nullable
fields an empty string CLEARS the value back to null (the
validators' blank-to-`None` normalization). `theme_key` is
NOT NULL: it can be changed but never cleared.

**The three themes** are layout variants of ONE shared
storefront view (`StorefrontView`), not three pages:
`classic` is the pre-Wave-18 look (banner image when set,
logo beside the store name, the standard product grid);
`minimal` drops the banner, shrinks and mutes the header,
and lists products as compact rows; `bold` enlarges the
banner and the store name and renders products as oversized
cards in a two-column grid. The brand colour is an accent
only — it paints the announcement bar and, through a
`--store-accent` CSS variable scoped to the storefront, the
button borders inside it.

**Limits (stated, not hidden):** image URLs are HOTLINKED —
there is no image upload or storage yet, so a logo or banner
lives wherever the seller hosts it and disappears if that
host drops it. The colour is a single accent, not a theme
editor: no seller CSS, HTML or layout code is accepted
anywhere, by design — branding values are data the platform
renders, never markup a seller injects. The marketplace
home has no public store listing (products only), so logos
appear on the storefront itself, not on marketplace cards.

Wave 19 (manual couriers — Sundarban, SA Paribahan, Karatoa,
Carrybee):

| Method | Path | What it does |
| --- | --- | --- |
| POST | `/v1/seller/sub-orders/{id}/shipment` | The Wave 6 booking endpoint, extended: with `{ "courier": "manual", "manual_courier": "<key>", "manual_tracking_id": "<receipt>" }` (receipt optional) it books a MANUAL courier — no adapter, no network call; the shipment row is written directly. Every Wave 6/17 guard applies identically (own store, confirmed sub-order, advance settled, one shipment per sub-order) and the COD collection amount is the shared booking computation |
| POST | `/v1/seller/shipments/{id}/manual-status` | Bearer, owner of the shipment's store. Record one seller-entered status for a manual shipment: a non-status is a 400, a move the domain transition table forbids is a 409, and a non-manual shipment is a 409. The move, its event and the sub-order sync commit in one transaction |
| POST | `/v1/seller/shipments/{id}/manual-tracking` | Bearer, same ownership. Record or correct the counter receipt number after booking; it also becomes the consignment id. A blank number is a 400 — a recorded receipt is an audit reference and is never cleared |

**Why manual at all:** these four couriers have NO public
merchant APIs — there is nothing to write an adapter against,
and this platform never fakes one. The operating model is the
one Bangladeshi sellers already use: the platform holds a
master account at each courier, the seller drops the parcel
at the counter, and the counter hands back a paper receipt.
Wave 19 puts that workflow behind the SAME shipment system as
the five API couriers: one `shipments` table (migration 0021
adds `manual_courier_name` / `manual_tracking_id`; the row's
`courier` is the literal `manual`), the same unified status
set, the same transition table, the same sub-order sync —
delivery earns and a return restocks through the identical
shared code paths, including the Wave 17 advance netting in
the collection amount. Refresh on a manual shipment is an
honest no-op: there is no API to poll, so the stored
seller-entered status is returned unchanged.

**The keys and the mapping:** the domain crate owns
`MANUAL_COURIERS` — `sundarban` (Sundarban Courier),
`sa_paribahan` (SA Paribahan), `karatoa` (Karatoa Courier),
`carrybee` (Carrybee) — and the pure validators
`validate_manual_courier` (trimmed, case-insensitive on the
KEY; display names are not accepted) and
`validate_manual_tracking_id` (optional; when present, 4–64
characters, no whitespace). `GET /v1/couriers` appends one
entry per manual courier — `courier` is the key, `available`
is always true (no credentials exist to lack), `mode` is
`manual`, and `display_name` carries the human name. Booking
posts `courier: "manual"` plus `manual_courier: <key>` —
never the key as the courier name. `GET /v1/couriers/estimate`
lists the manual couriers too, always with `price_poisha:
null` and a note: the counter sets the charge, so the
platform tariff does not invent one. Smart routing is
untouched: `choose_courier` only ever sees adapter quotes,
so `auto` can never resolve to a manual courier.

**The trust model:** for an API courier the platform learns
statuses from the courier; for a manual courier the SELLER is
the tracking feed, and the system is honest about that —
every manual event is stored with raw status `manual`, the
buyer's public tracking view names the courier as a manual
counter booking, and the counter RECEIPT NUMBER is the
verifiable reference both sides can quote (it is what the
tracking view shows, and what support checks against the
paper receipt). A booking made before the seller has the
receipt stores a generated `MANUAL-…` placeholder in
`consignment_id` (the column is NOT NULL), visibly different
from a real number until the seller records one.

**Limits (stated, not hidden):** no label printing and no
bulk booking — each manual booking is one parcel at one
counter, recorded one at a time. Statuses are only as timely
as the seller entering them. A recorded receipt number can
be corrected but never cleared. Partial-item and split
manual shipments do not exist: one sub-order, one parcel,
one receipt.

Wave 21 (seller KYC & verification tiers):

| Method | Path | What it does |
| --- | --- | --- |
| PUT | `/v1/seller/store/verification` | Bearer, store owner. Submit (or resubmit) the verification application: `nid_number` (required), `trade_license_number` and `document_url` (optional). Always lands as `pending`; a still-pending application is REPLACED in place; an approved store gets a 409 |
| GET | `/v1/seller/store/verification` | Bearer, store owner. The seller's own view: status (`none` until the first application), tier, masked NID, reviewer note, timestamps |
| GET | `/v1/admin/verifications?status=pending` | Admin. The review queue (`status`: `pending` default, `approved`, `rejected`, `all`): store slug/name, owner phone MASKED, NID masked, `has_trade_license`, document URL, timestamps |
| GET | `/v1/admin/verifications/{store_id}/full` | Admin. The full NID for one application — the single endpoint that ever returns it (see NID handling below) |
| POST | `/v1/admin/verifications/{store_id}/approve` | Admin. Body `{tier: "basic" \| "verified"}`. Approves the pending application and sets the store's `verified_tier`; `verified` without a trade license on the application is a 400; no pending application is a 409 |
| POST | `/v1/admin/verifications/{store_id}/reject` | Admin. Body `{note}` — required, 1–500 chars; the seller sees it. Sets the application rejected; the store tier does not move; no pending application is a 409 |

**The tiers:** every store starts `unverified`. `basic`
means the platform reviewed and approved the seller's NID —
the storefront and product pages show a "যাচাইকৃত বিক্রেতা ✓"
badge next to the store name. `verified` means NID plus a
trade license were approved — the badge reads "যাচাইকৃত
ব্যবসা ✓✓". An unverified store shows NO badge; the tier is
granted by review, never chosen by the seller, and the
public/seller `Store` views carry it as `verified_tier`
(migration 0022 also adds the `store_verifications` table).

**What each tier requires:** `basic` — a valid NID number
(the domain validator accepts the three issued lengths: 10,
13 or 17 digits). `verified` — the same NID plus a trade
license number on the application; the approve endpoint
enforces it, so a license-less application cannot be
approved at `verified` even by mistake.

**The review flow:** the seller submits from
`/seller/verification` (NID, optional trade license,
optional hotlinked document image). An admin works the
queue at `/admin/verifications`: open the document, reveal
the full NID only when needed to compare it against the
document, then approve at a tier or reject with a note. A
rejected seller sees the note on their verification page,
fixes the problem, and resubmits — resubmission from
`rejected` is a normal transition, and the old note is
cleared. `approved` is terminal for applications: changing
an approved store's tier would be a fresh admin action, not
a new application — and, honestly, that action does NOT
exist yet (there is no revoke/demote endpoint; it is an
owner policy decision still owed, recorded in the admin
verifications remake note below). A mistaken approval
currently stands until that ruling lands.

**NID handling (the sensitive rule of this wave):** the full
NID number is stored exactly once, in
`store_verifications.nid_number`, and returned by exactly
ONE endpoint — the admin full view above, which exists
because a reviewer cannot check a number against a document
image without seeing it. Every other surface carries only
the masked form (`domain::mask_nid`: all but the last four
digits starred; the stored `nid_last4` backs it): the
seller's own view, the admin queue list, and logs. The
public store view carries the tier and nothing about the
NID at all.

**The payout gate:** the `kyc_payout_gate` platform setting
(admin settings, stored as `on` / `off`) controls whether
payout creation demands verification. When ON,
`POST /v1/seller/payouts` rejects an `unverified` store
with a 400 that names verification (in Bangla and English)
and points the seller at the verification page; `basic` and
`verified` stores pass to the ordinary Wave 9 rules. Since
the admin payouts remake the gate is ALSO re-checked at
approval (same 400 class): a payout requested while the
gate was off cannot be approved after it comes on while
the store is still unverified. When
OFF — the default — payout behaviour is exactly Wave 9's:
existing sellers are never surprised by a rule that did not
exist when they joined, and the platform enables the gate
when it is ready to enforce KYC for money movement.

**Honest limits:** document review is MANUAL — a human
compares the number against the document image; there is
**no NID authenticity check against any government database,
and none is claimed**. A plausible-looking number with the
right length passes the format validator; only the human
review stands behind the badge. `document_url` is a hotlink
exactly like the Wave 18 branding images (no upload storage
exists yet), and the tier is trust metadata — it does not
change prices, commissions or search ranking.

Wave 22 (invoices with per-store sequential numbers):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/buyer/sub-orders/{id}/invoice` | Bearer, the order's buyer. The invoice for one sub-order — issued on this first read when the sub-order is `delivered` or `returned`; 409 before delivery; anyone else 404 |
| GET | `/v1/seller/sub-orders/{id}/invoice` | Bearer, the store's owner. The same document through the seller's door |
| GET | `/v1/admin/sub-orders/{id}/invoice` | Admin. The same document for support/disputes |

**Numbering rule:** every store numbers its own invoices
sequentially — `INV-000001`, `INV-000002`, … — from a
per-store counter (migration 0023). The number is assigned
on the FIRST entitled read of a delivered (or later
returned) sub-order, inside one transaction that locks the
store's counter row, so concurrent issues for one store
never draw the same number. A number is **never reused**:
the counter only moves forward, re-reading a sub-order's
invoice returns the same row (the `invoices` table is
UNIQUE on `sub_order_id`, and on `(store_id, seq)`), and a
sub-order that has not been delivered has no invoice at
all (409 — there is no completed sale to document). Two
stores can both have an `INV-000001`; an invoice is
identified by the pair (store, number), and the printed
document carries the store's name and slug next to the
number.

**Invoice vs receipt:** the Wave 4 receipt
(`/v1/orders/{id}/receipt`) is the BUYER's whole-order
summary — every seller's slice on one page, with no
commission or payout figures. The invoice is the SELLER's
legal document for ONE sub-order: that store's number, its
items (the checkout snapshots), and the money for that
slice alone — goods minus its allocated coupon discount
plus its delivery charge, built from the sub-order's
stored snapshot columns through the same shared order
loader the receipt uses. A collected COD advance (Wave
17) is an order-level figure, so the invoice allocates
it across the order's sub-orders in proportion to each
sub-order's share of the order grand total (exact for
single-seller orders; integer division can leave a poisha
or two unallocated across a multi-seller order's invoices
— the whole-order receipt remains the authority for the
order-level cash due).

**Printing / PDF:** the web invoice page
(`/invoice/{subOrderId}`, linked from the buyer's orders
and the seller's orders for delivered/returned sub-orders)
renders a clean printable document — store header with the
Wave 18 brand accent and the Wave 21 verification badge,
buyer details, the item table, the money summary with the
cash-due line highlighted — and the প্রিন্ট / PDF button
uses the browser's print dialog (`window.print()`); the
document title is the invoice filename stem
(`invoice-<slug>-<seq>`), which browsers propose as the
saved PDF's name. There is **no server-side PDF
generator**.

**What is NOT included:** no e-invoice / NBR integration
and no VAT or tax fields — whether invoices must carry a
Mushak challan reference or a VAT breakdown is a business
and regulatory decision for the operator, and the schema
deliberately does not invent one. Invoice corrections are
not modelled either: a wrong invoice is a support matter
today, not a credit-note flow.

Wave 23 (inventory management & the stock audit trail):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/seller/inventory` | Bearer. Every product of the caller's stores with its stock, thresholds and health, plus summary counts |
| POST | `/v1/seller/products/{id}/adjust-stock` | Bearer, owner. One manual stock move `{delta, reason, note?}`; 400 on an invalid delta/reason or a move below zero (nothing written); the response carries the product and the audit row |
| GET | `/v1/seller/products/{id}/stock-history?page=&per_page=` | Bearer, owner. The product's audit trail, newest first |
| PUT | `/v1/seller/products/{id}/low-stock-threshold` | Bearer, owner. Set the product's warning level, or clear it with `{"threshold": null}` |

**The audit rule:** from migration 0024 on, every change
to a product's stock writes a `stock_adjustments` row in
the SAME transaction as the stock update itself — the two
commit or roll back together, so the trail can never claim
a move the stock did not make (or miss one it did). Each
row records who moved it (`actor`: `system` for platform
movements, `seller` for manual ones, with the seller's user
id), the signed `delta`, a `new_stock` snapshot of the
on-hand count right after the move, and a `reason`. System
reasons are `order` (checkout), `cancel_restock` (buyer
and seller cancels, through the one shared restock helper),
`return_restock` (a received buyer return) and
`rto_restock` (a courier-driven return). Seller reasons
are a fixed vocabulary — `restock`, `correction`,
`damage`, `count_fix`, `other` — validated by the domain,
and a seller can never write a system reason. Product
CREATION writes no row: the history starts at the creation
stock.

**The product-update stock decision:** the product PATCH
endpoint used to accept a `stock` field and write it
silently. Wave 23 REMOVES `stock` from the PATCH payload —
adjust-stock is now the only way a seller changes stock,
so no unaudited write path remains. (No client used the
PATCH field: the web app has no product-edit call, and
stock was set at creation or not at all.)

**Thresholds:** a product's low-stock warning level is its
own `low_stock_threshold` when set, else the platform
default of 5 (domain `LOW_STOCK_DEFAULT`). Health is the
domain rule: stock ≤ 0 is `out_of_stock`, stock at or
below the threshold is `low`, anything above is
`in_stock`. The product view carries both the threshold
and the computed `stock_status`; the seller dashboard
gains `low_stock_items` (up to ten, out-of-stock first)
behind its alert strip, and the seller inventory page
(/seller/inventory) shows summary counts (মোট পণ্য /
স্টক শেষ / কম স্টক — the low count excludes out-of-stock,
which is its own number), per-product adjust forms and
threshold editors, and expandable per-product history.

**Honest limits:** there are no warehouses or
multi-location stock and no SKU variants — stock is one
number per product row, shared by every channel that sells
it. There is no reservation system either: a buyer's cart
does not hold stock, and the checkout transaction's row
lock (plus its in-transaction audit row) is the whole
concurrency story. Movements from before migration 0024
are not backfilled — the trail honestly starts with this
wave.

Wave 24 (wishlist & recommendations):

| Method | Path | What it does |
| --- | --- | --- |
| POST | `/v1/wishlist/{product_id}` | Bearer. Save a product for later. The product must be publicly visible (active product, active store) — anything else is a 404. Idempotent: re-adding is a 200 with `{wishlisted: true, product_id}` |
| DELETE | `/v1/wishlist/{product_id}` | Bearer. Unsave. Idempotent: removing an absent product is a 200 with `{wishlisted: false, product_id}` |
| GET | `/v1/wishlist` | Bearer. The caller's saved products as full product views (rating summary + stock health included), newest first |
| GET | `/v1/wishlist/ids` | Bearer. Only the saved product ids — the lightweight read the heart toggles share |
| GET | `/v1/products/{id}/related?limit=` | Public. Products related to this one, ranked by the scoring rule below; 404 when the base product is not publicly visible |
| GET | `/v1/products/{id}/also-bought?limit=` | Public. Cross-store co-purchase companions with their `co_count`; 404 when the base product is not publicly visible |
| GET | `/v1/recommendations?limit=` | Bearer. The caller's personalised rail, or the labelled top-rated fallback (`{personalized, items}`) |

**Wishlist semantics:** the list is account data and lives
server-side (migration 0025, one row per user + product),
so it follows the user to any device they sign in on. Add
requires public visibility — a wishlist must not become a
way to hold on to a draft or a suspended store's product —
while the list itself returns what the user saved without
re-filtering (the product page enforces visibility when
they click through). Add and remove always answer with
the resulting truth, so clients never guess state.

**The related scoring rule** (one SQL query; the weights
are far enough apart that a higher tier always wins):

| Signal | Points |
| --- | --- |
| Same category (both non-null, case-insensitive) | +4 |
| Same store | +2 |
| Well reviewed (rating average ≥ 4.0) | +1 |

Ties break by rating average (unreviewed last), review
count, then newest first. Only publicly visible products
are candidates, and the base product never appears.

**Also-bought** is ORDER-LEVEL co-purchase: a product
counts when it sits in a DIFFERENT sub-order of an order
that also contains the base product — the split checkout
puts each store's goods in its own sub-order under one
order, so this is exactly the cross-store "bought
together" signal. `co_count` is the number of DISTINCT
orders the pair shares. Only publicly visible companions
are returned, ordered by `co_count`, then rating. A
product nobody has ordered yet returns an empty list:
co-purchases are counted, never invented.

**Recommendations** are category signals, not a model.
The signals are the categories of (a) products the caller
received in a delivered sub-order and (b) products on
their wishlist, each category weighted by its most
recent signal. Candidates are publicly visible products
in a signal category that the caller has NOT bought (any
non-cancelled sub-order) and has NOT wishlisted —
recommendations surface discoveries, not saved or owned
items — ordered by signal recency, then rating. A caller
with no signals gets the documented fallback (top-rated
publicly visible products) and the response says so:
`personalized: false`. A caller WITH signals whose
candidates are all excluded gets `personalized: true`
with an empty list, never the fallback dressed up as
personalisation. Every rail's page size goes through
the one domain rule (`recommendation_limit`: default 8,
clamped to 1..=24).

**Recently viewed** is the deliberate opposite of the
wishlist: the viewed product ids live in the browser's
localStorage on ONE device and are never sent to the
server as tracking data. The home page resolves them
through the ordinary public product endpoint (ids that
no longer resolve are skipped silently), caps the rail
at 12, and clearing the browser's site data clears the
history — that is the privacy model, stated plainly.

**Honest limits:** there is no ML, no click/view event
pipeline and no A/B testing behind any rail — the
rankings are the documented rules above, computed from
orders, wishlist rows and review aggregates the platform
already stores. Same-sub-order companions do not count
toward also-bought (the definition is the cross-store
pair), and the personalised rail is only as good as its
category signals: a catalogue with sparse categories
gives sparse personalisation, and the fallback carries
new users until they buy or save something.

Wave 25 (product Q&A):

| Method | Path | What it does |
| --- | --- | --- |
| POST | `/v1/products/{id}/questions` | Bearer. Ask a question on a product (3–500 chars). The product must be publicly visible (else 404); asking about your OWN store's product is a 400. 201 with the question |
| GET | `/v1/products/{id}/questions?page=&limit=` | Public question list, newest first, paginated like every other list; hidden questions excluded; 404 when the product is not publicly visible |
| POST | `/v1/seller/questions/{id}/answer` | Bearer, owner of the product's store only (anyone else: 404). Answer (1–1000 chars); answering again CORRECTS the answer on the same row |
| POST | `/v1/seller/questions/{id}/hide` · `/unhide` | Bearer, same ownership rule. Set/clear the question's hidden flag |
| POST | `/v1/admin/questions/{id}/hide` · `/unhide` | Admin. The same moderation write, behind the admin guard |
| GET | `/v1/seller/questions?answered=false\|true\|all` | Bearer. The seller inbox across all their stores, newest first; default `false` (the work queue). Hidden questions are included and marked `is_hidden` |

**The rules:** anyone signed in may ask — except about
their own store's products, because sellers answer
questions rather than seed them. Only the owner of the
product's store may answer, and re-answering is a
correction, not a second answer: there is exactly one
answer per question, always the seller's latest word.
Seller hide/unhide and admin hide/unhide are the same
single write (`set_question_hidden`) behind two guards.
A hidden question disappears from the public list AND
from the dashboard's unanswered count (hiding is how a
seller deals with spam — a dealt-with question must not
keep nagging), but it stays in the seller inbox so the
hide can be undone. The dashboard's
`unanswered_questions` counter therefore counts questions
that are unanswered AND not hidden. Asker identity is
public only as a masked phone — the full number never
leaves the API.

**Honest limits:** there are no notifications in this
wave. Sellers discover questions by opening the Q&A
inbox or glancing at the dashboard counter; buyers get
no ping when their question is answered — they see the
answer by revisiting the product page. And there are no
AI answers here by design: SIDRA answers store chat
(Wave 13), but Q&A answers are public, permanent and
signed by the store, so they stay human-written.

Wave 26 (sponsored product promotions):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/promotions/policy` | Public. The effective per-day price, the 30-day cap and the currency note |
| POST | `/v1/seller/products/{id}/promote` | Bearer, own product only (anyone else's: 404). Buy `{days}` (1–30) of sponsorship for an ACTIVE product (draft/archived: 400), prepaid from the earnings balance (insufficient: 400; another live promotion for the product: 409). 201 with the promotion, its cost and the balance after |
| GET | `/v1/seller/promotions` | Bearer. The caller's promotions, newest first, each with product title and derived `display_status` (`active` / `expired` / `cancelled`) |
| POST | `/v1/seller/promotions/{id}/cancel` | Bearer, own promotion only (anyone else's: 404). Stop a live promotion now; already cancelled/expired: 409. **No refund — the charge stands** |
| PUT | `/v1/admin/settings` | Admin. Now also accepts `promotion_price_per_day_poisha` (validated 1..=10,000,000) — reprices NEW promotions only |

**The model:** a promotion is prepaid sponsorship of ONE
product for a whole number of days, bought ONLY from the
seller's earnings balance — there is no card charge and no
cash top-up, so a brand-new seller cannot promote until
they have actually sold something. The per-day price is
an effective setting: the admin's `platform_settings` row
beats the startup `PROMOTION_PRICE_PER_DAY_POISHA`, which
beats the built-in 10,000 poisha (৳100/day); the maximum
single purchase is 30 days.

**The balance equation** (the one aggregate in the
`payouts` module — dashboard, balance endpoint, payout
creation and promotion purchase all read it):

```
available = earned (delivered sub-order snapshots)
          − paid out (approved + paid payouts)
          − pending (requested payouts)
          − charges (seller_charges: promotion purchases)
```

The purchase runs in ONE transaction: it locks the
seller's payout-store row, reads that aggregate inside
the transaction (through the shared executor-generic
`seller_earnings_on`), refuses when the balance cannot
cover the cost, and writes the promotion row plus its
`seller_charges` row together — so the money is spent
exactly once and the same available figure then gates
payout requests. Charges belong to the seller (user),
not to one store, because a promotion may target any of
the seller's stores while money settles through the one
payout store.

**Ranking rule:** while a promotion window is live
(status `active`, now inside [starts_at, ends_at) — the
domain's `promotion_is_active`), the product view
carries `is_promoted`, computed as an EXISTS in the
shared product columns so cards, detail, search and
wishlist all agree. Promoted products sort FIRST in the
default marketplace/storefront listing and in search's
relevance and newest orderings — and always render the
স্পন্সরড badge. Explicit buyer sorts stay pure: price
ascending/descending and rating order exactly by price
or rating, promoted or not.

**No-refund policy, stated plainly:** cancelling a
promotion stops the placement immediately and refunds
NOTHING. The charge stands. The seller UI says this
before purchase (in the page copy and on the cancel
button itself), not after.

**Honest limits:** there is no external ad network, no
per-click or per-impression billing, and no impression /
click statistics — a promotion is a fixed prepaid window,
nothing is metered. There is no cash top-up: promotions
are bought only from earned balance. One live window per
product (no stacking, no scheduling a future start —
every promotion starts now). Sponsored ranking applies
to the default/relevance/newest orderings only, by the
documented rule above.

Wave 27 (analytics for sellers and the platform):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/seller/analytics?days=` | Bearer, own stores only (no store: 404, like the dashboard). Totals, zero-filled daily series, top products, status and courier breakdowns for the window |
| GET | `/v1/seller/analytics/export.csv?days=` | Bearer. The same daily series as a `text/csv` download (`seller-analytics-<days>d.csv`) |
| GET | `/v1/admin/analytics?days=` | Admin (403 otherwise). Platform totals, top stores, courier breakdown and daily GMV series for the window |

**The window:** the last N days INCLUDING today —
`days` absent means 30, and any value clamps into
7..=90 (the pure domain `analytics_days`; a caller is
never errored for asking outside the range). Day
boundaries are **Asia/Dhaka calendar days**, computed by
Postgres (`created_at AT TIME ZONE 'Asia/Dhaka'`): a sale
at 11pm in Dhaka counts on that Dhaka day, not the next
UTC day. Sub-orders (seller slices) are the unit of
almost every metric, because fulfilment and money move
per slice.

**Metric definitions — seller** (scope: the sub-orders
of ALL stores the caller owns, created in the window):

| Metric | Definition |
| --- | --- |
| `totals.sub_orders` | Sub-orders NOT in `cancelled` — the sales population the money figures share |
| `totals.goods_poisha` | SUM(`goods_subtotal_poisha`) over that non-cancelled population (before coupon discounts) |
| `totals.earnings_poisha` | SUM(`seller_payout_poisha`) over DELIVERED sub-orders only — the Wave 9 earnings rule; the snapshot already nets out borne discounts |
| `totals.delivered` / `returned` / `cancelled` | Counts per current sub-order status |
| `totals.return_rate_bps` | returned ÷ (delivered + returned) × 10,000, integer-truncated; 0 when the denominator is 0 |
| `daily[]` | One row per Dhaka day, ZERO-FILLED via `generate_series` (quiet days appear with zeros, so the series length is always the window length): non-cancelled sub-order count, their goods, the delivered slices' earnings |
| `top_products[]` | Order items of non-cancelled sub-orders grouped by product: units = SUM(quantity), goods = SUM(line_total_poisha); top 10 by goods |
| `status_breakdown[]` | Counts per status over ALL sub-orders in the window (cancelled included — the operations view) |
| `courier_breakdown[]` | Shipments of the window's sub-orders per courier; manual bookings count under their counter name, not the `manual` marker |

**Metric definitions — admin** (platform-wide, same
window):

| Metric | Definition |
| --- | --- |
| `totals.gmv_poisha` | SUM(goods − discount + delivery) over sub-orders NOT cancelled — what buyers committed to pay for goods and carriage |
| `totals.orders` | Parent orders CREATED in the window (the orders table, any state) |
| `totals.sub_orders` | ALL sub-orders created in the window, cancelled included (operations volume) |
| `totals.commission_poisha` | SUM(goods − discount − seller_payout) over DELIVERED sub-orders — equals their stored `commission_poisha` snapshots |
| `totals.new_users` / `new_stores` | User / store rows created in the window |
| `totals.payouts` | `requested_count` + `requested_poisha`: payouts still in `requested` created in the window; `paid_poisha`: payouts in `paid` created in the window |
| `totals.charges_poisha` | SUM of `seller_charges` (Wave 26 promotion purchases) created in the window |
| `totals.coupons` | `redemptions`: coupon_redemptions rows created in the window; `discount_poisha`: their summed discount |
| `top_stores[]` | Per store over non-cancelled sub-orders: GMV by the platform formula + the non-cancelled sub-order count; top 10 |
| `daily[]` | Per Dhaka day, zero-filled: `orders` = DISTINCT parent orders among that day's non-cancelled sub-orders; `gmv_poisha` by the GMV formula |

**The CSV:** header `date,orders,goods_poisha,earnings_poisha`,
then one line per day of the seller series, built by the
domain CSV builder (RFC 4180 escaping). Note the
`orders` column counts SUB-ORDERS — the same value as
the JSON daily rows' `sub_orders` field; the file is a
seller's own sales ledger, where a slice is the unit of
work.

**How it is computed:** every block is a single GROUP BY
aggregate (or one scalar-subselect row) over the ledger
tables — no N+1, no rows loaded into Rust to sum, no
rollup tables and no cached counters, so the numbers can
never drift from the money tables they describe.

**Honest limits:** there is no cohort, funnel or
retention analysis, and nothing is realtime — figures
are computed per request from the ledger tables, so a
window always reflects the current state of orders
(a slice cancelled tomorrow leaves yesterday's goods
when the window is re-read). At very large volumes the
per-request aggregates would move to rollup tables
behind these same definitions; the 90-day cap keeps a
single request bounded until then.

Wave 28 (security hardening + the admin audit log):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/admin/audit-log?page=&per_page=&action=` | Admin (403 otherwise). The audit entries, newest first; `action` filters to one action; `limit` is accepted as an alias of `per_page` |

Everything else in this wave is behaviour of existing
surfaces, not new endpoints:

**OTP request throttle.** `POST /v1/auth/request-otp`
counts the phone's requests in the last hour and answers
**429** once the domain cap
(`OTP_REQUEST_LIMIT_PER_HOUR`) is reached; no code is
stored or sent for a refused request, and refused requests
are not themselves counted. The count comes from the
append-only `otp_request_log` table (migration 0028) —
`otp_codes` cannot answer it, because it holds at most
one row per phone (a new request deletes the previous
rows, a successful verify deletes them too). The cap is
**20 per hour**, set from evidence: the design target
was 5, but the integration suites share ONE Postgres
database across 25+ test binaries in a single CI run,
and the busiest fixture phone legitimately logs in 9
times across those suites within the run — a cap of 5
(or 10) would throttle the test suite itself. 20/hour
still bounds SMS cost to at most 480 messages per number
per day, and brute force stays impractical because every
issued code still expires in 5 minutes and allows only
5 verify attempts against a 6-digit space. The count
check and the log insert are two statements in one
request path, so two concurrent requests can both pass
the check — an accepted race for a cost cap, not exact
accounting.

**Security headers (API).** Every API response —
including errors and CORS preflights — carries:

| Header | Value |
| --- | --- |
| `X-Content-Type-Options` | `nosniff` |
| `X-Frame-Options` | `DENY` |
| `Referrer-Policy` | `no-referrer` |
| `Content-Security-Policy` | `default-src 'none'; frame-ancestors 'none'` |
| `Strict-Transport-Security` | `max-age=31536000` |

The set is strict because the API serves JSON only —
never HTML, frames or assets. The Next.js app serves
its own subset from `next.config.js` (`nosniff`,
`DENY`, `no-referrer`, HSTS): the API's
`default-src 'none'` CSP would break a site that serves
HTML/JS/CSS, so the web app does not send it — a real
web CSP is future work.

**Request body cap.** Request bodies are capped at
1 MiB (`MAX_REQUEST_BODY_BYTES`); an oversized body is
a **413** before any handler runs. Every current
payload is far below this — the largest, the Facebook
post import, caps its own post count well under it.

**JWT secret startup rule.** Outside dev mode the API
refuses to start (stderr reason, exit code 2) when the
JWT secret is the well-known dev-only fallback
(`JWT_SECRET` unset) or shorter than 32 characters
(`validate_jwt_secret` in the API crate, enforced in
`run`). In dev mode anything goes — local development
must not need a real secret. The error never echoes the
secret itself. Integration tests build state directly
(`AppState::new`), never through the env path, so the
rule cannot fire inside a test.

**Admin audit log.** Every consequential admin action
writes one row to `admin_audit_log` (migration 0028)
through the ONE writer (`audit::record_admin_action`)
inside the SAME transaction as the action itself — the
log can never claim an action that rolled back, or
miss one that committed:

| Action | Target | Written by |
| --- | --- | --- |
| `payout.approve` / `payout.reject` / `payout.mark_paid` | payout id | the payout transition (detail: `{from, to}`, plus `note` on a rejection — required since the admin payouts remake — and `payment_reference` on a mark-paid; account numbers and phones are never written to the log) |
| `store.suspend` / `store.reactivate` | store id | the store status change |
| `settings.update` | — | the settings patch (detail: `{changed_keys}` — key NAMES only, never values; a no-op patch writes no row) |
| `verification.approve` / `verification.reject` | store id | the KYC decision (detail: the granted tier, resp. the reviewer note the seller reads; since the admin verifications remake also `document_present`, `no_document_override` and the decided `version`) |
| `verification.view_full_nid` | store id | the audited full-NID reveal (detail: `{reason, nid_last4, status}` — the admin's stated reason; the full number is NEVER a detail value) |
| `qna.hide` / `qna.unhide` | question id | the ADMIN Q&A moderation endpoints only — a seller hiding a question on their own product is not audited |
| `return.refund_done` | return request id | marking an off-platform refund done |

The read side identifies the acting admin by MASKED
phone only. What is never logged: any secret value
(JWT secret, gateway credentials), OTP codes, full NID
numbers, and setting values.

**Honest limits:** the throttle is per PHONE, not
per IP — an attacker rotating through many numbers is
bounded only by the SMS provider's own caps and the
platform's SMS budget, which is a provider-account
setting, not something this API can enforce. Headers
are set by the API and (subset) by the Next.js config;
anything deployed in front of them (a reverse proxy
that strips or overrides headers) can silently undo
them — verify with `curl -I` after deploying. The
audit log covers the six admin action families above;
seller-side and buyer-side actions are not in it (their
own histories — stock adjustments, payment events —
already exist from earlier waves).

**Real (Wave 29):** the notification outbox + a real
worker. A new workspace crate (`crates/notify`) owns the
`SmsSender` trait (moved out of `auth.rs`; the OTP path
uses it unchanged), the sender selection rule and the
outbox itself. Selection lives in ONE function,
`sender_from_env(dev_mode)`: dev mode → the log sender
(prints `[dev SMS] to <phone>: <message>` to stderr);
anything else → the unconfigured sender, whose every send
fails with `sms_not_configured` until a real gateway is
integrated. Message bodies are rendered by a pure,
unit-tested domain function (`render_body` over seven
kinds, taka amounts formatted by `format_taka` — whole
taka when evenly divisible, else two decimals) **at
enqueue time** and stored rendered, so a queued message
can never change wording after the fact. Enqueue happens
in the SAME transaction as the state change (migration
0029: `notifications` with status
pending/sending/sent/failed, `attempts`, `last_error`,
rendered `body`, JSON payload, an index on
`(status, created_at)`) at exactly one funnel per event:
checkout success → `order_placed` (once per order, to the
buyer at the order's DELIVERY phone, body carries the
grand total); the seller status PATCH →
`sub_order_shipped` / `sub_order_delivered` /
`sub_order_cancelled`; an API-courier or manual booking
(which itself moves the sub-order to shipped) →
`sub_order_shipped`; the shared shipment sync (courier
refresh + manual status updates) → `sub_order_delivered`
/ `sub_order_returned` (RTO) — each sub-order reaches
delivered exactly once, so no funnel pair can
double-notify; the payout transitions → `payout_approved`
/ `payout_rejected` / `payout_paid` (to the seller at
their ACCOUNT phone, with the amount; the rejected notice
carries the reason, the paid notice the recorded payment
reference — admin payouts remake; before it only the paid
move notified); return approve / reject / received →
`return_update` (buyer, account phone; a platform return
is not an RTO and never produces `sub_order_returned`).
The **worker is real now**: it connects to `DATABASE_URL`
(absent → clear stderr error, exit 2), runs the same
embedded migrations as the API (idempotent), then loops
every `WORKER_POLL_SECONDS` (default 10): claim a batch
of 20 pending rows in one transaction
(`FOR UPDATE SKIP LOCKED`, flipped to `sending`,
committed BEFORE any send, so no row lock is held across
the network), send each, and write the outcome back —
success → `sent` + `sent_at`; failure → `attempts + 1`
and `last_error`, back to `pending` while attempts
remain, `failed` after 3 attempts (`MAX_ATTEMPTS`).
Batches are logged only when something was processed;
Ctrl-C / SIGTERM stops the loop gracefully. Admins watch
all of it at `GET /v1/admin/notifications` (counts per
status + recent rows with masked phones) and
`/admin/notifications`.

**Honest limits:** SMS is STILL dev-only — there is no
real SMS gateway, so in production without one every
notification is attempted 3 times and lands in `failed`
with `sms_not_configured`, VISIBLE in the admin page,
never silently dropped; OTP requests outside dev mode
still 503 before anything is stored. The biggest launch
blocker remains a real SMS gateway. A worker that dies
mid-batch leaves its claimed rows in `sending` — counted
in the admin view, but there is no automatic reaper yet.
(Closed in Wave 48: the worker reaps stale `sending`
rows at the top of every cycle — see the Wave 48
section.) No email or push channels exist yet; payout jobs and COD
reconciliation remain planned worker jobs, printed at
worker startup (courier polling landed in Wave 30).

**Real (Wave 30):** automatic courier status polling. The
single-shipment refresh core is factored out of the
participant endpoint into ONE service function,
`refresh_shipment_by_id` (returns the shipment plus an
`Advanced` / `Unchanged` outcome), and the endpoint is now
a participant check in front of it — its behaviour is
unchanged. On top of it sits `refresh_stale_shipments`:
candidates are shipments whose courier is not `manual`,
whose status is not terminal, and whose `updated_at` is
older than the staleness threshold, taken oldest-first up
to a batch limit. The terminal set for polling is
`delivered` / `returned` / `cancelled`
(`POLL_TERMINAL_STATUSES`): the domain table does allow
`delivered` → `return_in_transit`, but that journey is
driven by an explicit refresh or a return flow, not by
background polling — for the poller, delivery is final.
`partial_delivered` and `return_in_transit` are NOT
terminal (both still have somewhere to go) and keep being
polled. There is deliberately NO HTTP endpoint for the
batch poll: the worker binary now depends on the `api`
crate as a library and calls the service in-process on
every poll cycle, with a state built by
`AppState::from_env` around the worker's own pool — the
same courier credentials and DEV simulator flag the API
server reads, and no JWT startup gate (that gate lives in
`api::run`, which the worker never calls; the worker
serves no HTTP and issues no tokens). Configuration:
`SHIPMENT_POLL_STALE_MINUTES` (default 15, invalid or
negative → default with a warning, zero allowed) and a
fixed batch of 50 per cycle; the cycle logs one line only
when it actually checked something. A shipment whose
refresh errors (courier down, credentials revoked) is
counted in the run's `PollReport` (`checked` /
`advanced` / `unchanged` / `failed`) and logged — never
fatal to the batch — and because a failed refresh writes
nothing, its `updated_at` stays stale so the next cycle
retries it. An integration test
(`crates/api/tests/wave30.rs`, same `DATABASE_URL` gate)
drives the poller in courier dev mode: a baseline poll
first (shared-database discipline), then a booked shipment
walks picked_up → in_transit → out_for_delivery →
delivered one poll at a time with the sub-order sync and
the delivered notification firing, the staleness window
excludes a fresh shipment and includes a backdated one,
and a final exact-count poll proves a backdated delivered
shipment and a backdated manual shipment are both left
alone. No web changes this wave.

**Honest limits:** freshness is bounded by the poll
cycle — a status can lag the courier by up to
`WORKER_POLL_SECONDS` plus the staleness threshold, and
every poll is a tracking call against the platform's
SHARED courier merchant accounts, so the staleness
threshold and the 50-per-cycle batch are also a rate
budget: sellers who need an answer sooner still have the
manual refresh endpoint. No courier offers this platform
a webhook receiver yet, so polling is the mechanism, not
a fallback. The Wave 29 caveat stands: notification rows
a worker claims and then dies on stay in `sending` with
no automatic reaper yet. (Closed in Wave 48 — the
worker now reaps stale claims every cycle; see the
Wave 48 section.)

Wave 31 (backups + data portability):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/seller/export` | Bearer, seller with a store (404 without one). The seller's whole record as ONE downloadable JSON document (see below) |
| GET | `/v1/buyer/export` | Bearer. The buyer's whole record as ONE downloadable JSON document (see below) |

**Backups.** `scripts/backup.sh` writes a gzipped
`pg_dump` of the database plus a `.sha256` sidecar and
prunes to the newest 14 files. It runs two ways: directly
(`DATABASE_URL=... ./scripts/backup.sh [outdir]`, needs
`pg_dump` on PATH) or against the compose stack
(`./scripts/backup.sh --compose [outdir]`, which runs
pg_dump inside the `postgres` container with the
container's own `POSTGRES_USER` / `POSTGRES_DB`). The
script header carries a cron example. `scripts/restore.sh
[--yes] <backup.sql.gz>` restores: it verifies the
checksum sidecar when present, asks for confirmation
unless `--yes` is given, DROPS and recreates the target's
`public` schema (a restore replaces, never merges), loads
with psql `ON_ERROR_STOP` so a bad dump fails loudly
instead of half-loading, and prints the restored table
count as a smoke check. CI proves the round trip on every
run: after the test step, the rust job dumps the test
database, restores it into a scratch database, and
asserts the schema survived (more than 20 public tables,
`orders` among them) — a migration whose schema pg_dump
cannot round-trip fails in CI, not during a recovery.

**Exports.** Both endpoints answer with
`Content-Disposition: attachment` and the domain
`export_filename` (`bd-marketplace-<kind>-<slug-or-phone>-
<yyyymmdd>.json`, sanitized to `[a-z0-9-]`). An export
MIRRORS STORED FIELDS — rows as they sit in the tables,
items nested under their sub-orders — rather than
re-shaping the public views, because the owner is
entitled to the record as kept. The seller document
holds the primary store (verification tier included),
ALL products of ALL the seller's stores (drafts and
archived included — an export that dropped drafts would
not be the catalogue), the newest 1_000 sub-orders with
items, payouts, coupons, promotions, and the lifetime
`seller_charges` total. The buyer document holds the
profile — the phone IN FULL, because it is the buyer's
own data shown to themselves; the masking on public
surfaces protects users from other people, never from
themselves — the newest 1_000 orders with sub-orders and
items (delivery details live on the orders, so addresses
travel with them; there is no separate address book),
reviews, wishlist (ids + titles) and asked questions.
The seller dashboard and the buyer orders page both carry
a ডেটা এক্সপোর্ট download button (fetch-blob, the Wave 27
pattern). The intent is portability: nobody is locked in
— a seller can leave with their catalogue, sales history
and money ledger, a buyer with their history.

**Honest limits:** file backups are point-in-time dumps,
not point-in-time recovery — there is no WAL archiving,
so a restore lands on the last backup, not on any moment
between backups. Nothing copies backups off-site: the
script writes files, and shipping them somewhere safe is
the operator's job (the script header says so). And an
export is a JSON snapshot for the owner's records — not
a migration tool: nothing imports it back, here or on
another platform.

Wave 32 (performance pass — indexes, gzip, cache headers):

No endpoint changed shape in this wave; three costs were
attacked instead. **Indexes (migration 0030).** The full
0001–0029 history was inventoried first — the hot paths
that already had indexes (orders by buyer, sub_orders by
order and by store+created, order_items by sub_order,
reviews by product, messages by conversation, wishlist
by product, stock_adjustments by product, notifications
by status) are NOT duplicated. Four gaps were filled,
one per query shape that lacked one:

| Index | Query it serves |
| --- | --- |
| `idx_order_items_product` on `order_items (product_id)` | Seller top-products analytics (Wave 27) groups order items by product; the also-bought rail (Wave 24) probes them by product inside an EXISTS |
| `idx_shipments_status_updated` on `shipments (status, updated_at)` | The Wave 30 poller sweeps non-terminal shipments oldest-first by staleness, every worker cycle |
| `idx_sub_orders_store_status` on `sub_orders (store_id, status)` | Seller earnings and dashboard counts filter one store's sub-orders by status (earned = store + `delivered`) |
| `idx_product_questions_product_hidden` on `product_questions (product_id, is_hidden, created_at DESC)` | The public Q&A list reads one product's visible questions newest first |

The migration uses plain `CREATE INDEX` inside the
migration transaction — a sub-second lock at the current
catalogue size. On a large production table these would
be built with `CREATE INDEX CONCURRENTLY` as an ops step
instead (it cannot run inside a transaction); the
migration header says so.

**Gzip.** tower-http's `CompressionLayer` (the pinned
tower-http 0.5.2, `compression-gzip` feature) is the
OUTERMOST middleware layer: the response flows out
through CORS and the Wave 28 security headers first, and
compression applies to the finished body only when the
client sends `Accept-Encoding: gzip`. Clients that do
not ask get byte-identical responses, which is why every
earlier test suite is unaffected. Enabling the feature
added only new transitive crates (async-compression,
flate2 and friends) to the lockfile — no existing crate
changed version.

**Cache headers.** The anonymous public catalogue reads
— `GET /v1/products`, `GET /v1/products/{id}`,
`GET /v1/search` — now answer `Cache-Control: public,
max-age=30`, set at the handler (never by path-matching
middleware). Thirty seconds is the whole policy: prices
and stock change without invalidation, so a shared cache
may serve a price at most half a minute stale — accepted
for the catalogue, never for carts, orders or balances.
The subtlety: the catalogue payload is viewer-dependent
(a signed-in owner also sees their own drafts), so the
header is attached ONLY when the request carries no
token. An authenticated catalogue read gets no cache
header at all, and no authenticated endpoint anywhere
gets a public one — both rules are asserted in the
integration test.

**Web images.** `next.config.js` now allows the
next/image optimiser to fetch any http/https host
(`images.remotePatterns`), matching the Wave 18 hotlink
model for seller imagery. Nothing was converted this
wave, honestly: products carry no image field yet, so
the card and the product page render no product image
to convert, and the only hotlinked images in the app —
store logos and banners — size themselves from CSS with
unknown intrinsic dimensions, so swapping them to
next/image is a layout change, not a one-line swap; it
waits until product imagery lands and gives the
optimiser real call sites.

**Honest limits:** there is still no Redis and no CDN —
the cache header only pays off once a cache (browser,
proxy, CDN) sits in front, and the worker/Redis compose
profile remains future work. The indexes serve the
current query shapes only; a new access pattern earns
its own index when it exists, not before. And nothing
in this wave was benchmarked: no load test was run, so
no throughput or latency numbers are claimed — the
indexes, compression and cache policy are shaped from
the queries and payloads in the code, and a load test
remains future work.

Wave 33 (product variants — size / colour, own price delta, own stock):

| Method | Path | What it does |
| --- | --- | --- |
| POST | `/v1/seller/products/{id}/variants` | Bearer, owner only (anyone else: 404). Create a variant `{name, sku?, price_delta_poisha?, stock?, sort?}`. 201 with the variant (incl. computed `final_price_poisha`); 400 on a bad name/delta or a non-positive final price |
| GET | `/v1/seller/products/{id}/variants` | Bearer, owner only. All variants, inactive included, in sort/name order |
| PATCH | `/v1/seller/products/{id}/variants/{variant_id}` | Bearer, owner only. Edit name / sku / delta / is_active / sort; the merged delta is re-validated against the current base price |
| DELETE | `/v1/seller/products/{id}/variants/{variant_id}` | Bearer, owner only. SOFT delete: flips `is_active` to false and returns the row; reactivation is a PATCH. The row is never destroyed — order history references it |
| POST | `/v1/seller/products/{id}/variants/{variant_id}/adjust-stock` | Bearer, owner only. One manual, audited stock move on the VARIANT (same reason vocabulary and below-zero refusal as product adjust-stock) |

**The model.** A variant's price is never stored: it is
the product's base price plus the variant's
`price_delta_poisha`, computed by the pure domain rule
`variant_final_price`. The final price must stay positive
at EVERY write that could break it — variant create,
variant reprice, checkout itself, and a base-price PATCH
that would strand an existing delta (refused with a 400
naming how many variant prices it would break). The
delta itself is sanity-bounded (±100_000_000 poisha) by
the domain, and names are trimmed 1..=80 characters.

**The stock semantics switch.** While a product has at
least one ACTIVE variant, availability lives on the
variants: the shared product view's `stock` is the SUM
of the active variants' stocks (computed inside
`PRODUCT_COLUMNS`, so listing, search, detail, wishlist
and recommendations all agree, and `stock_status`
derives from the sum), checkout locks and decrements the
VARIANT rows, and `products.stock` is touched by no
write path for that product. The seller inventory
overview and low-stock alerts read the same effective
stock. With no active variants, `products.stock` rules
exactly as before — the Wave 9 two-store money flow is
asserted unchanged in the integration test.

**Cart and checkout.** A cart line's identity is now
(product, variant): `cart_items` gains `variant_id`, and
its uniqueness becomes a `NULLS NOT DISTINCT` unique
index over (cart_id, product_id, variant_id) (migration
0031) so two variant lines of one product coexist while
variant-less lines still merge. Adding a product that
has active variants without choosing one is a 400
("আকার/রং বেছে নিন"); choosing a variant that is not an
active variant of the product is a 400. Cart lines show
the variant name and price at the FINAL price. Checkout
locks the variant rows alongside the product rows,
re-validates activity and stock per line (insufficient
variant stock is the same 409, naming product AND
variant), enters the totals at the final price, and
snapshots `variant_id` + `variant_name` onto the order
item. A variant-less line for a product that has since
gained active variants is refused at checkout — a stale
cart can never draw on stock the seller moved into
variants. Set-quantity and remove address a line by
(product, variant) too.

**Audit fidelity.** `stock_adjustments` gains
`variant_id`: every movement that touches a variant's
stock — checkout, cancel/return/RTO restocks (the shared
restock helper restores to the variant the line bought,
even if it was deactivated since), and per-variant
adjust-stock — writes its row through the one Wave 23
writer with the variant id set and `new_stock`
snapshotting the VARIANT's count; the per-product
history shows the movements in place. A variant's
creation stock writes no row (the product-creation
rule), and product-level adjust-stock REFUSES a product
with active variants (400) rather than writing a pool
nothing reads. The product detail gains `variants`
(active only, final prices, own stock); list views gain
only `has_variants` — variant rows never ride the list
payload. The web app gains the product page's variant
picker (price and stock follow the pick), variant names
on cart and order lines, and the seller variant manager
on the products page.

**Honest limits:** variants carry no weight of their own
(delivery still bills the product's `weight_grams` per
unit, variant or not) and no images (products have no
image field yet — see Wave 32). Promotions, coupons and
search ranking treat the product as one unit — there is
no variant-level promotion or per-variant coupon.
Deactivating every variant returns the product to
product-stock semantics with whatever `products.stock`
still holds; sellers moving stock into variants should
set each variant's stock explicitly (creation stock or
adjust-stock), because the product's old count is NOT
auto-distributed across variants.

Wave 34 (multi-store access + staff roles — a store is a team):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/seller/stores` | Bearer. Every store the caller owns or is a team member of, each with `my_role` (owner \| manager \| staff), slug, name, status and verified tier. Owned stores first |
| GET | `/v1/seller/stores/{id}/members` | Bearer. The store's team list (masked phones + roles). Owner and manager may view; staff get 403; a caller with no relationship to the store gets 404 |
| POST | `/v1/seller/stores/{id}/members` | Bearer, OWNER only. Seat an EXISTING account by phone `{phone, role}` — 201 with the member view; 404 when no account has that number (no silent account creation, no invite SMS); 400 for the owner themself or a bad role/phone; 409 on a duplicate seat |
| PATCH | `/v1/seller/stores/{id}/members/{user_id}` | Bearer, owner only. Change a member's role `{role}` (manager \| staff); a non-member is a 404 |
| DELETE | `/v1/seller/stores/{id}/members/{user_id}` | Bearer, owner only. Remove a member — 204; access ends with the row. A non-member is a 404 |

**The permission matrix** is one pure domain function —
`store_role_can(role, action)` — over eight named actions;
it fails closed (an unknown role or action can do nothing):

| Action | Owner | Manager | Staff |
| --- | --- | --- | --- |
| `manage_members` (seat / re-role / remove) | ✓ | — | — |
| `manage_payouts` (payout account, payout requests) | ✓ | — | — |
| `view_finances` (balance, payout list, analytics, export) | ✓ | ✓ | — |
| `manage_catalog` (products, variants, inventory) | ✓ | ✓ | ✓ |
| `manage_orders` (sub-orders, shipments, return decisions) | ✓ | ✓ | ✓ |
| `manage_marketing` (coupons, promotions, branding) | ✓ | ✓ | — |
| `answer_questions` (product Q&A) | ✓ | ✓ | ✓ |
| `manage_store_settings` (verification submit, SIDRA toggle) | ✓ | ✓ | — |

**The access helper** (`crates/api/src/access.rs`) is the
one place role resolution lives: `store_role` answers the
caller's role in a store in ONE query (owner when
`stores.owner_user_id` matches, else the member row's
role, else none), `seller_access` turns "none" into a
404 (a stranger must not learn the store exists), and
`require_role` turns a matrix denial into a bilingual
403. Two scoping shapes cover the whole seller surface:

- **Store-in-hand surfaces** (the store comes from a
  product, sub-order, shipment, question, return or
  promotion row) keep each module's historical no-access
  answer — the 403 modules (orders, shipments, returns)
  still answer 403 to a caller with no relationship, the
  404 modules (catalogue, inventory, variants, Q&A,
  promotions) still answer 404 — and then apply the
  matrix. Owner behaviour is byte-for-byte what it was.
- **Caller-store surfaces** gained an optional
  `?store_id=`: absent, they resolve the caller's own
  earliest store exactly as before; present, they resolve
  through `seller_access` + the matrix. That covers the
  dashboard (any role may VIEW a served store's
  dashboard), balance, payout list, payout account,
  payout requests, seller analytics + CSV, the seller
  export, store coupons, branding, verification and the
  SIDRA toggle. Cross-store LISTS (seller sub-orders,
  return requests, the Q&A inbox, the inventory overview,
  the promotions list) scope to every store the caller
  holds the relevant action in — for owners that is the
  same set as before.

**Per-family decisions.** Catalogue: members create and
edit products in the store (creation keeps naming the
store in the body); a non-active product is now visible
to the store's whole team, not only its owner. Orders: a
member confirms, books (API or manual), refreshes and
decides returns; shipment refresh keeps its buyer-or-team
participant rule. Finances: a manager reads a store's
balance, payout history, analytics and export — the
scoped export's document is that one store, and its
charges total counts only that store's promotions.
Payouts: the account and requests are owner-only in the
matrix, so a manager naming the store is refused 403
before any money rule runs, and the payout store
(`get_store_by_owner`, the owner's earliest store) never
resolves for a member at all. Promotions: sponsoring is
gated by the PRODUCT's store role, but the money still
moves from the CALLER's own earnings and payout store —
a member's purchase is paid from the member's own
balance, never silently from the owner's (a member with
no earnings of their own cannot complete one); the list
and cancel are scoped to the promotion's store.
Verification review stays admin work; the seller side is
store-settings work.

**Web.** The seller dashboard gains a store switcher
(shown only when the seller serves more than one store;
the selection persists in localStorage as
`seller-store-id`), a role badge next to the store name,
and a টিম — Team link (hidden for staff). In a scoped
store view the payout account/request, buyer-risk and
export sections hide — they belong to the caller's own
store — with a note that the store's owner manages its
payouts. The new `/seller/members` page lists the team
with masked phones; the owner gets the add-by-phone
form, role selects and remove buttons, managers see the
list read-only, and the page explains that membership
requires an existing account. Earlier wave tables in
this README that say "owner only" predate team access —
the matrix above is the current rule.

**Honest limits:** membership requires an existing
account — there is no invite flow (no SMS invite, no
pending-invite state). The payout store remains the
owner's earliest store; a member can never receive or
request a payout for a store they serve. Staff activity
is not separately attributed in order history: actions
record the acting user only where the underlying feature
already did (stock adjustments do; sub-order status
changes do not carry an actor column). Several surfaces
remain owner-scoped this wave, deliberately: Facebook
link/import, the SIDRA suggestion queue and conversation
inbox, review replies, the seller invoice read, the
buyer-risk lookup, and the SIDRA status read — each
still resolves the caller's own store. A member's
promotion purchase charges the member (see above), which
most members cannot fund; owner-funded member marketing
is not modelled. Roles are per store and there are
exactly two of them; finer-grained or custom roles are
not modelled.

Wave 35 (buyer loyalty points — earn on delivery, spend at checkout):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/loyalty` | Bearer. The caller's points position: `available_points`, `lifetime_earned`, `lifetime_redeemed`, the programme rules (`earn_poisha_per_point`, `point_value_poisha`, `max_redeem_percent_of_goods`, `expiry_days`) and the last 20 ledger movements, newest first |

**The rates** are pure domain constants and functions:
earn **1 point per ৳100** (10_000 poisha) of NET goods —
goods subtotal minus the sub-order's coupon discount
share, floored per sub-order (`loyalty_points_for_goods`);
redeem at **1 point = ৳1** (100 poisha,
`loyalty_discount_poisha`); a redemption may not exceed
**half the order's post-coupon goods total**
(`loyalty_max_redeemable_points`); earned points **expire
365 days** after they are earned.

**The ledger** (migration 0033, `loyalty_points`) is
append-only and IS the balance — there is no stored
counter to drift. Rows are `earn` (positive, per delivered
sub-order, carrying `expires_at`), `redeem` (negative,
one per checkout that spent points, ref = the order) and
`adjust` (refunds and clawbacks, ref = the sub-order).
The one balance query (`loyalty_balance_on`) computes
available points as the UNEXPIRED earn sum plus every
redeem/adjust movement, floored at zero. Deliberate
simplification: movements net against the unexpired earn
pool as a whole — there is no per-point FIFO tracking of
which earn lot a redemption consumed.

**Earn funnels.** Points land exactly where the Wave 29
delivered notification lands — the two transactions that
move a sub-order to `delivered`: the seller status PATCH
(`orders::update_sub_order_status`) and the shared
shipment sync (`shipments::sync_sub_order_with_shipment_status`,
courier refresh and manual statuses alike) — via the one
hook `loyalty::earn_for_sub_order`, inside the same
transaction. A partial UNIQUE index (one earn row per
sub-order) plus `ON CONFLICT DO NOTHING` makes the credit
exactly-once even if a funnel were ever reached twice.
Checkout is authenticated, so every order's buyer is an
account the earn can credit.

**Redeem at checkout.** `POST /v1/orders` accepts
`loyalty_points`. Inside the checkout transaction, after
the coupon is applied: the buyer's user row is locked
`FOR UPDATE` (concurrent checkouts serialize; the same
points cannot be spent twice), the balance is read, and
the request is CLAMPED to the redeemable maximum — never
rejected for being too large; a negative request is a
400. The discount is allocated across the seller groups
proportional to post-coupon goods by the same
`allocate_discount` the coupon uses, and each sub-order
snapshots its share (`sub_orders.loyalty_discount_poisha`).
The stored `grand_total_poisha` is the REDUCED payable:
the Wave 17 advance decision, the online payment amount,
the receipt's cash-due and the courier's collection
amount (`compute_booking_cod_amount` nets each parcel's
share) all read reduced figures. **The platform funds
the redemption**: `seller_payout_poisha` and the
commission basis are computed from the pre-loyalty totals
and never change — a seller is paid exactly as if the
buyer had paid cash. The order and receipt views carry
`loyalty_points_used` + `loyalty_discount_poisha`.

**Reversals** ride the one-way transitions, which is
their once-only guard (no unique index — see migration
0033's note). A cancelled sub-order (buyer whole-order
cancel or seller single cancel) refunds the points its
allocated share bought — share / 100, floored; the
redemption total was a multiple of 100 poisha but its
per-sub-order shares need not be, and the remainder
poisha stay with the platform. A return marked RECEIVED
claws back the sub-order's earn (the mirror `adjust`
row); if the buyer already spent those points, the
balance floors at zero and the clawback consumes future
earns first. An RTO parcel was never delivered, so it
never earned and needs no clawback.

**Web.** Checkout shows a লয়্যালটি পয়েন্ট block when
the buyer has points: the balance, the most this cart may
redeem (the server's rule previewed with the same
arithmetic), an opt-in and an amount; the placed order
shows the loyalty discount line. The buyer orders page
header carries the loyalty card (balance, lifetime
totals, recent ledger with বাংলা movement labels) and
each order shows its loyalty discount line; the receipt
page shows it too.

**Honest limits:** points cannot be transferred or
gifted, there are no tier levels, and expiry is computed
at read time — no sweeper job deletes or rewrites
expired rows, and an expired lot's points simply stop
counting toward the balance. A partial (single-sub-order)
cancel refunds by that sub-order's allocated share, not
by re-running the order's redemption; the allocation
remainder stays with the platform (above). Points are
worth money only inside checkout — there is no cash-out.

Wave 36 (seller subscription plans — prepaid from the earnings balance):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/seller/subscription/plans` | Bearer. The plan catalogue with EFFECTIVE prices, commission discounts and product caps |
| GET | `/v1/seller/subscription` | Bearer, `view_finances` role (owner or manager). The scoped store's plan view — effective plan (free unless a live paid row exists), stored status, expiry, auto-renew, discount, cap and price |
| POST | `/v1/seller/subscription` | Bearer, OWNER only (`manage_payouts`). Body `{ "plan": "pro" }`. Subscribe or switch; 201 with the charge and the balance after. Paid plans are charged in ONE transaction; `plan: "free"` cancels a paid plan at once |
| POST | `/v1/seller/subscription/cancel` | Bearer, OWNER only. Stops auto-renewal; the plan stays active until its expiry. 400 when there is no live paid plan |

**The plans** (pure domain rules; prices and the free cap
are platform settings, DB > env > const, in the usual
`effective_*` style):

| Plan | Price / 30 days | Commission discount | Product cap |
| --- | --- | --- | --- |
| free | ৳0 | 0 bps | 50 (tunable: `plan_free_product_cap`) |
| pro | ৳499 (`subscription_price_pro_poisha`) | 100 bps (1%) | 500 |
| business | ৳1,499 (`subscription_price_business_poisha`) | 200 bps (2%) | unlimited |

**The billing model is prepaid from the earnings
balance** — the same honesty rules as Wave 26
promotions: there is NO external payment rail for
platform billing. Subscribing locks the seller's payout
store row `FOR UPDATE`, computes the available balance
through the one shared `available_balance_poisha`
(earned − paid out − pending − charges), refuses with a
400 and no writes when the price exceeds it, and on
success writes a `seller_charges` row (kind
`subscription`, ref = the subscription id — migration
0034 widens the kind CHECK) and upserts the store's one
subscription row (`store_subscriptions`, UNIQUE per
store) as active for 30 days from now with auto-renew
on. Switching plans charges the new plan's FULL price
and starts a fresh 30-day period from now — **no
proration, no refunds**, stated in the UI before
purchase. Switching to free takes effect immediately:
the row is marked cancelled and the store's benefits
stop at once (also no refund). Cancelling renewal is
different: `auto_renew` goes false and the plan keeps
its benefits until `expires_at`.

**Per-store commission.** A plan is ACTIVE for benefits
only while its row says `active` AND `expires_at` is in
the future — one rule (`live_plan`) behind the view, the
discount and the cap. Checkout resolves the commission
rate PER STORE GROUP: `effective_commission_bps_for_store`
is the base effective rate minus the store's live plan
discount, floored at zero, and `build_order_totals`
takes it through the additive per-group
`commission_rate_bps` override (absent = the order-wide
rate, so every pre-W36 caller and test is unchanged).
Each sub-order's commission and `seller_payout_poisha`
snapshots therefore use its own store's rate — a pro
store in a mixed cart pays 700 bps while a free store
beside it pays the full 800.

**The product cap.** Creating a product counts the
store's product ROWS — products are never hard-deleted
(the lifecycle is draft/active/archived), so every row
counts and archiving does not free a slot — against the
store's effective cap; at the cap the create is a 403
naming the cap in English and Bangla. The count is taken
on the pool just before the insert, so two creates in
flight at once can overshoot by the number in flight;
the cap is a plan boundary, not a money rule.

**Renewal is worker-driven.** Each worker cycle also
runs `process_subscriptions` (the Wave 30 poller shape:
`{ checked, renewed, expired }`, per-store transactions,
errors logged and counted, never fatal). A lapsed active
paid subscription renews when auto-renew is on and the
owner's available balance covers the CURRENT effective
price: the charge lands and `expires_at` extends by 30
days FROM THE OLD EXPIRY — no paid days are lost to a
late sweep. Otherwise the row becomes `expired` and the
store falls back to the free plan (full commission, free
cap) until the owner subscribes again. Free-plan rows
never expire.

**Web.** `/seller/subscription` shows the current plan
(expiry, auto-renew state, discount, cap, the store's
available balance), the three plan cards with
subscribe / switch / return-to-free buttons and the
cancel-renewal button; subscribing is owner-only —
managers and staff see the plan but the buttons are
disabled with an explanation. The page states the model
in plain Bangla before any button: ব্যালেন্স থেকে কাটা
হবে — আলাদা পেমেন্ট লাগবে না (charged from the balance —
no separate payment needed), switching is full price
with no proration, and switching to free is immediate
with no refund. The seller dashboard carries a plan
badge linking to the page, and the seller nav links it.

**Honest limits:** subscription charges produce no
invoice beyond the charges ledger and the Wave 31 seller
export's charges total; there are no free trials, no
per-seat pricing (a plan covers the whole store,
including its Wave 34 team), and no way to pay for a
plan except the earnings balance — a brand-new seller
with no delivered orders cannot subscribe until they
have earned at least the plan price. Renewal depends on
the worker running; a stopped worker means plans lapse
to `expired` at the next sweep rather than renewing,
never a silent extension.

Wave 37 (third-party / mobile API keys — long-lived keys
for external apps):

| Method | Path | What it does |
| --- | --- | --- |
| POST | `/v1/api-keys` | Bearer (JWT only). Body `{ "name": "Mobile app", "scopes": ["read"], "expires_in_days": 90 }` — scopes default to `["read"]`, expiry is optional (1..=365 days or null). 201 with the key metadata PLUS the full key — the one and only time it is ever shown |
| GET | `/v1/api-keys` | Bearer (JWT only). The caller's keys: name, display prefix, scopes, derived status, last use, expiry. NEVER the full key, never the hash |
| DELETE | `/v1/api-keys/{id}` | Bearer (JWT only). Revoke (a timestamp, never a delete). Own keys only — a stranger's key is a 404; re-revoking is a 200 with the same view |

**The credential.** A full key is `bdm_live_` + 40
alphanumeric characters from the same `rand` CSPRNG that
generates OTP codes (~238 bits of entropy; no new
dependency). Only its SHA-256 hash is stored
(`api_keys.key_hash`, UNIQUE — the auth lookup is by hash)
— the hash-only discipline OTP codes already use — plus
the first 12 characters as the display prefix, so lists
can tell keys apart without exposing anything usable.
The pure rules are the domain's: name (trimmed,
1..=60 characters), scope vocabulary and normalisation
(`read`, `write`; write implies read; unknown scopes are
refused, never stored), the scope permission rule, and
the expiry bound.

**Authentication is one central path.** The `AuthUser`
extractor recognises the `bdm_live_` marker and routes
the token to `api_keys::authenticate_request`, which
applies, in order: the hash lookup (unknown, revoked or
expired → 401; the owner's phone and role are re-read
from the user row, so account changes apply immediately),
the JWT-only route exclusions (→ 403), the scope rule (a
read-only key gets 403 on any non-GET call), and the
per-key rate limit (→ 429). The request then acts as the
key's user everywhere — cart, orders, seller surfaces —
with no handler aware of how it authenticated. JWT
traffic takes the old path untouched and is never rate
limited by this. On the public catalogue reads (the
optional-auth product endpoints) a key caller is treated
as a guest: those endpoints stay JWT-optional by design.

**The JWT-only exclusions** (no scope overrides them —
they are route rules, not scope rules): all of
`/v1/admin/*`; `/v1/api-keys*` itself, so a leaked key
cannot mint or revoke keys to persist itself; the payout
account (every method) and payout requests
(`POST /v1/seller/payouts`) — money out needs a login
session, though the payout LIST stays readable;
subscription subscribe/cancel (`POST
/v1/seller/subscription*`) — spending the earnings
balance, same reasoning, while reading the plan and
catalogue stays allowed; and both data exports
(`/v1/seller/export`, `/v1/buyer/export`) — a full account
dump is a session-only action.

**Rate limiting** is a per-key fixed-window counter in
`AppState` (a `Mutex<HashMap>` behind an `Arc`): 600
requests per key per 60-second window by default,
overridable at startup with `API_KEY_RATE_LIMIT_PER_MIN`
(invalid or zero keeps the default, with a warning — the
usual env discipline). Windows reset wholesale; stale
entries are swept opportunistically when the map grows,
with no background task. `last_used_at` is stamped
best-effort after a request is admitted — a failed stamp
is logged, never a failed request.

**Web.** `/seller/api-keys` (linked from the seller
dashboard nav) lists the account's keys — name, prefix,
scopes, status, created / last-used / expiry — with a
create form (name, read/write scope checkboxes, optional
expiry days) whose response opens a one-time reveal
panel ("এই কীটি আর দেখানো হবে না — এখনই কপি করুন" —
this key will not be shown again, copy it now) with a
copy button, and a per-key revoke button behind a
confirm. Keys belong to the account, not a store, so
there is no store switcher on the page.

**Honest limits:** the rate limiter is in-memory and
per-process — a restart resets every window, and
multiple API instances would each keep their own count,
so the effective fleet-wide limit is the per-instance
limit times the instance count; a shared store (Redis)
is future work, like every other cross-instance concern
here. Keys are bearer secrets: anyone holding one acts
as its owner within its scopes and exclusions, so a
leaked key is a leaked account for those routes until it
is revoked — there is no per-key IP allowlist and no
OAuth2 (no client credentials flow, no consent screen);
rotation is manual by design: create the replacement,
move the app, revoke the old key. A key with no expiry
stays valid until revoked — the 365-day cap binds only
keys created WITH an expiry.

Wave 38 (PWA — installable, offline-tolerant web app):

**Manifest + icons.** `app/manifest.ts` (Next 14's
MetadataRoute.Manifest, served as /manifest.webmanifest
and linked automatically): Bangla name, `lang: "bn"`,
`display: "standalone"`, start_url `/`, theme_color the
brand green from globals.css (#1b5e20 — the
verification-badge green; globals.css defines no --brand
variable, so the hex is shared by copy between the
manifest, the layout's viewport export, and the icon
script). The icons are REAL generated PNGs, drawn
pixel-by-pixel by `apps/web/scripts/make-icons.mjs` — a
dependency-free Node script (built-in `zlib` only) that
renders a white shopping-bag outline on the brand green
with signed-distance math and 3×3 supersampling:
icon-192 + icon-512 (rounded tile, transparent corners,
purpose "any"), icon-maskable-512 (full-bleed, motif
shrunk to the centre ~54% so it stays inside the 80%
safe-zone circle), and apple-touch-icon.png (180×180,
full-bleed — iOS masks it itself). Regenerate with
`node scripts/make-icons.mjs`; never hand-edit the PNGs.

**Service worker** (`public/sw.js`, hand-written, no
Workbox): registered by `components/PwaRegister` in
production only (a worker in `next dev` would cache
stale dev bundles), versioned caches (`w38-v1` — bump
the version on any rule change; activate deletes older
caches and claims clients, and a `SKIP_WAITING` message
lets an update apply). It handles ONLY same-origin GET
requests; everything else passes through untouched. (In
the deployed shape the API is same-origin — the web
bundle bakes `NEXT_PUBLIC_API_BASE_URL=""` and a proxy
routes /v1 — so the rules below cover the real API
traffic.) The rules:

| Traffic | Rule | Why |
| --- | --- | --- |
| `/_next/static/*`, `/icons/*`, `/manifest.webmanifest` | Cache-first | Content-hashed or versioned assets; immutable in practice |
| Navigations | Network-first, cached page as fallback, then the precached `/offline` page | Fresh pages online; a visited page — or an honest offline page — when the network drops |
| GET `/v1/products*`, `/v1/search*`, `/v1/stores*` (public catalogue, storefront profile + products, reviews) | Network-first, cache fallback | Public, carries no per-user data; on flaky BD connections a slightly stale catalogue beats a dead page |
| `/v1/auth/*`, `/v1/cart/*`, `/v1/orders*`, `/v1/seller/*`, `/v1/admin/*`, `/v1/payments/*`, `/v1/loyalty`, `/v1/wishlist*`, `/v1/conversations*`, `/v1/api-keys*` | NEVER cached — network only, nothing stored | Authenticated or mutating surfaces: a cached copy could be stale, personal, or both |
| Any non-GET request, any cross-origin request | Untouched | Mutations are never replayed or stored by the worker |

The app shell (`/`, `/offline`, the manifest, the two
"any" icons) is precached at install. `/offline` is a
static Bangla page (no API data) with a reload button.

**Install UX.** One shared component,
`components/InstallAppButton`, mounted in the home-page
nav and the buyer orders header: on Chromium it
captures `beforeinstallprompt` and shows "📲 অ্যাপ
ইনস্টল করুন — Install app" only when the browser
actually offers an install (and never when already
running standalone); iOS Safari has no such event, so a
conservative iOS-Safari detection shows the manual hint
instead (শেয়ার বোতাম → 'Add to Home Screen').
`app/layout.tsx` also carries the Next 14 split the
platform wants: `metadata.icons` + `appleWebApp` in the
metadata export, `themeColor` in the separate
`viewport` export.

**CI guard.** `apps/web/scripts/check-pwa.mjs`
(dependency-free; `npm run check-pwa`, and a web-job
step after the build) asserts sw.js exists and still
carries the never-cache deny markers for
`/v1/auth/`, `/v1/cart/`, `/v1/orders`, that
`app/manifest.ts` exists, and that all four icon files
are real PNGs whose IHDR dimensions match their names —
a simplified worker or a wrong-sized icon fails CI
before it reaches a phone.

**Honest limits:** the worker is hand-written — no
Workbox, no background sync, no push notifications
(nothing queues an offline action for later; a failed
mutation is simply failed). Offline means a READ-ONLY
catalogue shell: pages and public product/search/store
responses seen before can render, but cart, checkout,
orders, login and everything authenticated need the
network — the never-cache list above is a privacy rule,
not a missing feature. Cached catalogue data can be
stale (network-first bounds that to one failed fetch,
but a page served offline is as old as the last visit).
The icons are a simple generated geometric mark, not a
designed brand icon — real brand artwork is future
design work. iOS install is manual via the Share menu
(Apple offers no prompt API), and iOS evicts caches
under storage pressure like every web app there.
Wave 39 (security fixes — the Critical money-path and
credential findings of the 2026-11-27 security audit,
PR #1; audit ids below are the audit's own, cited from
its full list with the executive-list number where the
two differ):

**Forged DEV payment callbacks (CR-01 executive /
CR-09 full).** The DEV gateway's HMAC secret had a
public default, so anyone could sign a "paid" webhook.
No default exists anywhere now: the secret comes from
`PAYMENTS_DEV_SECRET`, and when dev mode is on without
it the API generates an EPHEMERAL random secret at
startup and logs a warning saying so (dev payments then
cannot settle across restarts — set the variable for a
stable local secret). Related (HI-50): DEV references
now verify ONLY while dev mode is on — with dev mode
off a DEV callback is unverifiable (401) instead of
being checked against a possibly-empty secret.

**jsonwebtoken CVE (CR-02 executive / CR-04 full).**
`jsonwebtoken` 9.3.1 → 10.4.0, fixing CVE-2026-25537
(type confusion via a crafted JWK header). v10 splits
its crypto backend behind features; the API crate uses
`rust_crypto` (the provider matching the previous
HS256 usage) with default features off. JWT shape is
unchanged: HS256, 7-day expiry, same claims.

**Known compose passwords (CR-03).** The root
`docker-compose.yml` no longer ships a default database
password: `POSTGRES_PASSWORD` is a REQUIRED variable
(`${POSTGRES_PASSWORD:?...}`) for the postgres service
and for the api/worker `DATABASE_URL`s built from it —
compose fails fast until a real one is set (generate:
`openssl rand -hex 24`). The local-dev
`infra/docker-compose.yml` lost its hardcoded Postgres
password and MinIO root credentials the same way
(HI-12). `.env.example` ships no usable password, and
its dev-mode defaults are now the safe ones
(`APP_ENV=production`, `SMS_DEV_MODE=false` — local
development opts in explicitly).

**Payout double-spend race (CR-04 executive / CR-10
full).** Payout creation is one transaction: the store
row is locked `FOR UPDATE` first (the pattern the
promotion/subscription charges already used), the
payout account and the earnings are re-read inside,
then the insert — two concurrent requests can no
longer both pass the balance check. The audit's
approve-time half also landed: admin approval re-checks
the payout against the store's CURRENT (unclamped)
earnings inside the transition transaction and refuses
with a 409 when earnings dropped after the request.
The audit's third element for this finding, an
`Idempotency-Key` on payout creation, is deferred
(below). (Follow-up: it landed in Wave 44 — see that
section.)

**Non-atomic payment settlement (CR-05 executive /
CR-02 full).** `apply_payment_callback` now settles in
ONE transaction: the audit event is written first (on
the pool, as before, so rejected callbacks stay
auditable), then the order row and payment row are
locked `FOR UPDATE`, the payment status is re-read
under the lock, and the payment update, the order
`payment_status` update and the Wave 17 advance branch
commit together or not at all. Idempotency is
unchanged (duplicate callback → 200, no double
effects). Payment retry got the same treatment (HI-07:
one transaction, both rows locked, status re-checked
under the lock — a retry can no longer overwrite a
settled `paid` back to `pending`).

**OTP attempt-cap race (CR-06 executive / CR-01
full).** Verify counted attempts read-check-write, so
parallel guesses could pass the cap. The count is now
a single conditional `UPDATE ... WHERE attempts < 5
RETURNING`, which increments atomically or matches no
row; the cap, the 5-minute expiry and every error
shape (no code 400, cap 400, expired 400, wrong code
401) are unchanged.

**Paid-order cancel without refund (CR-07).** Buyer
whole-order cancel and seller single sub-order cancel
now refuse with a bilingual 409 when the order's money
was actually collected online — `payment_status =
'paid'`, or a COD advance already paid — naming the
refund path (the admin returns flow), and restock
nothing. The audit allowed either a refund record or
"at minimum block with 409"; the 409 block is what
landed (real gateway refunds stay unimplemented —
HI-52 below). Cancels where nothing was collected
(COD pay-on-delivery, advance still pending) are
unchanged.

**Late webhook settles a cancelled order (CR-08).**
The settlement transaction locks and reads the order's
status: a callback for a cancelled order is
audit-logged and returns the payment UNCHANGED — no
money state moves. Buyer cancel additionally voids the
order's pending payment row (`failed` — the payment
lifecycle has no `cancelled`) in the cancel
transaction, so nothing settleable survives; payment
retry refuses cancelled orders the same way.

**Courier over-collects on coupon orders (CR-06
full).** The booking computation never subtracted the
coupon discount: a coupon COD order's courier was told
to collect the discount a second time in cash. The
booking row now carries `discount_poisha` and every
collection share — the plain share and the
advance-branch allocation total — subtracts it, so a
booked parcel collects exactly the receipt's
`cod_amount_due_poisha` (the wave39 test pins the two
equal).

**Domain money safety (CR-11 + CR-12, with HI-18 /
HI-19).** The money boundary no longer panics or wraps:
`build_order_totals` returns a `Result` and rejects
negative unit prices, negative unit weights and
negative group subtotals at the boundary;
`calculate_delivery_charge` returns its negative-COD
error instead of asserting; `line_total_poisha`,
`split_cart_by_seller` and every totals sum use
checked arithmetic (overflow is an error, never a
wrapped figure); `compute_commission` computes in
i128, rejects a negative base and a rate above 100%.
All callers (checkout, coupon allocation, the courier
price estimates, the Wave 1 demo endpoints) map the
error to a 400 / gateway error. Defence in depth:
`[profile.release] overflow-checks = true` in the
workspace manifest. Domain unit tests pin the new
behaviour (negative → Err, near-`i64::MAX` → Err,
normal totals — including the wave9 anchor numbers —
unchanged).

**Courier create parsers accept fake success (CR-05
full).** The RedX, eCourier and Paperfly create
parsers accepted any body carrying a tracking/order
id, including HTTP-200 error bodies. All three now
reject a body carrying an explicit failure verdict —
`success: false`, a non-empty `error` field, a numeric
`status`/`code` ≥ 400, or a failed/failure/error
status string, at the top level or under `data` —
before any id is trusted (Steadfast and Pathao already
required their positive verdicts). Fixture tests pin
error-plus-id bodies as errors for all three. What did
NOT change: an id with no failure signal is still
accepted, because none of the three couriers'
documented success envelopes carries a positive
success flag to demand — their exact envelopes remain
a merchant-onboarding confirm item (see Waves 7/11),
and demanding one blindly would break every real
booking.

**Silent test skips (CR-14, the cheap half).**
`crates/api/tests/ci_gate.rs` is one always-run test
that panics when `CI` is set but `DATABASE_URL` is
missing/empty, so a misconfigured CI can never report
green with zero integration assertions. Locally (no
`CI`) it passes quietly.

**Deferred — named, not dropped.** The rest of the
audit's 363 findings stay in the backlog (PR #1 holds
the full register); the ones nearest this wave, with
their reasons:
- CR-13 (all integration tests share one database and
  truncate each other's tables): CI history is green;
  per-test isolation is a harness redesign and gets
  its own wave.
- CR-15 (Wave 17's degradation policy — when no gateway
  can collect a required COD advance, checkout proceeds
  as flagged plain COD): behaviour deliberately NOT
  changed. Policy decision owed by the owner — fail
  checkout vs flag-and-proceed.
- The payout `Idempotency-Key` (the third element of
  CR-10's prescription): the race itself is closed by
  the lock; key-based dedup is API surface that needs
  a client contract first. (Closed in Wave 44.)
- HI-51 (callback amount is checked against the
  payment row, not re-derived from the order total):
  needs the order-total contract decided alongside
  gateway onboarding.
- HI-52 (no gateway refund is wired — refunds are
  manual/admin today): blocked on real merchant
  accounts, like every live gateway call.
- HI-53 (`gateway_ref` has no UNIQUE constraint —
  duplicate refs resolve newest-first): a migration
  plus a duplicate-handling contract; its own wave.
- Everything else in the audit (the remaining High,
  Medium and Low findings — session/transport
  hardening, upload and export surfaces, the frontend,
  which the audit did not cover): future waves, in the
  audit's priority order.

Wave 40 (seller fulfillment center + SLA dashboard):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/seller/fulfillment?store_id=` | Bearer, `manage_orders` in the store (owner or any member role). The store's work queue: sub-orders classified into buckets (below), oldest first, at most 200, with per-bucket counts, breached count, and the SLA pair the ages were judged against |
| POST | `/v1/seller/fulfillment/bulk-book` | Bearer, same scope. Book up to 20 of the store's confirmed sub-orders in one call, with the single-booking endpoint's courier fields (`courier` name / `auto` / `manual` + its fields, `area_id`). Per-item results; see the bulk rules |
| GET | `/v1/seller/fulfillment/labels?store_id=&ids=` | Bearer, same scope. Label DATA for booked sub-orders (sender, recipient, courier, consignment, COD due); every id without a label is named in `skipped` with its reason. The web print view renders the cards — there is no server-side PDF |

**Queue model:** the queue is a VIEW, not a new workflow —
no status was added anywhere. The pure domain function
`fulfillment_bucket` classifies each sub-order from its own
status, whether a shipment exists, and the shipment's
unified status, in this precedence: a shipment that is
`returned`, `cancelled` or `return_in_transit` → `attention`
(the unified shipment vocabulary has no `failed` status —
`cancelled` is its failure-terminal state); a sub-order
`returned` or `cancelled` → `attention`; `delivered` →
`done`; `placed` → `to_confirm`; `confirmed` without a
shipment → `to_ship` (with one → `in_transit`, a defensive
reading of a raced row); `shipped` → `in_transit`; anything
unrecognised → `attention`, failing visible rather than
hiding in done. Done items leave the queue after 7 days
(the list filter and the counts share one SQL fragment that
mirrors the Done case, so they cannot drift). The seller
dashboard carries the same counts (its `fulfillment`
block, computed by the same function over the dashboard's
scope) as a summary card linking here.

**SLA rules:** two platform SLAs, in hours — confirm
(default 24) and ship (default 48) — resolved
DB `platform_settings` (`fulfillment_confirm_sla_hours` /
`fulfillment_ship_sla_hours`) > startup env
(`FULFILLMENT_CONFIRM_SLA_HOURS` /
`FULFILLMENT_SHIP_SLA_HOURS`) > domain constants, through
one helper pair (`effective_fulfillment_sla`), with stored
or startup values outside 1..=720 ignored on read. Only
the two actionable buckets can breach (`sla_breached`):
a `to_confirm` item breaches when its age REACHES the
confirm SLA, a `to_ship` item at the ship SLA. **Age is
measured from placement for every bucket:** `sub_orders`
carries `created_at` and no `updated_at`, so there is no
honest confirm timestamp to start the ship clock from —
the ship SLA is therefore the stricter reading (confirm
late and less of the 48 hours remains). That is a
documented consequence of the schema, not an oversight.

**Money figures:** an item's `cod_due_poisha` is the
booked shipment's stored collection amount, or — while
unbooked — the preview from the booking path itself
(`preview_cod_amount_for_sub_order` runs the shared field
loader and the one `compute_booking_cod_amount`, with no
guards and no writes). There is no second COD formula in
the fulfillment module.

**Bulk booking semantics:** scope errors (no relationship
with the named store), an empty or >20 id list, and
invalid manual fields fail the WHOLE request before
anything books — they are request-shape errors, exactly
like the single endpoint's 400s. Every id after that is
independent: it must belong to the scoped store (a foreign
or unknown id is a per-item error), then it runs
`book_shipment_for_seller` — the dispatch the Wave 40
refactor factored out of the single-booking handler — with
that path's own guards (confirmed-only, advance-pending,
one-shipment) and its own transaction per item. The
response is per-item truth: the created shipment, or the
booking path's own error message. One failure never rolls
back the others, and nothing is reported booked that was
not.

**Labels:** data only. A label carries the store's name,
slug and the owner's phone as the sender block (the schema
has no store street address, so none is invented), the
delivery name/phone/address/zone as stored at checkout,
the courier's display name, the consignment id, and the
shipment's stored COD amount. Only booked sub-orders get
labels — a label without a consignment id would be a lie.

**Limits:** the queue list is FIFO-capped at 200 rows
(counts stay exact over the full actionable set); Done
retention in the queue is 7 days; bulk batches cap at 20,
label runs at 100; there are no courier-native label PDFs
(per-courier label APIs are merchant-onboarding items,
like the RedX/eCourier/Paperfly field confirmations), no
pick-list or packing workflow, and no per-item SLA clocks
beyond the one placement clock described above.

Wave 41 (security hardening II — the audit's
highest-value High findings, picked up from Wave 39's
deferred list; audit ids are the audit's own):

**Callback amount vs the ORDER's expectation (HI-51).**
Settlement used to trust the payment ROW as the
definition of the price: Wave 5 proved the callback
amount equals the row, but nothing proved the row still
equals what the order expects. Inside the settlement
transaction (under the Wave 39 locks), the expectation
is now re-derived from the locked order: an online
payment must equal the order's `grand_total_poisha`
(checkout snapshots exactly that into both), and a COD
order can only ever settle its recognised advance —
any other COD settlement (a drifted advance amount, an
order no longer `advance_pending`) is rejected instead
of falling through to a generic `paid` the courier
would collect ৳0 against. A mismatch is a 400, the
callback stays in `payment_events`, and no money state
moves. The wave41 test tampers a row's amount by SQL
between checkout and callback — the correctly-signed
callback for the tampered amount is rejected, the
honest one settles.

**`gateway_ref` uniqueness (HI-53, migration 0036).**
Two partial unique indexes close the ambiguity the
settlement lookup used to resolve newest-first:
`payments_gateway_ref_unique` on `gateway_ref` (where
present — references stored today are platform-minted
and unique across the whole table, not just within a
method) and `payments_one_pending_per_order` (at most
one `pending` attempt per order; checkout creates
exactly one, retry flips the same row). The webhook
lookup is now exact — no ORDER BY choosing between
duplicates that can no longer exist — and
`insert_pending_payment` maps a uniqueness collision
to a typed 409 instead of a 500 or a silent second
settleable row. OPERATOR NOTE: on a database that
already holds duplicate references or double-pending
orders, the migration fails loudly until they are
resolved — picking a winner silently is how the
ambiguity started. The reference FORMAT is unchanged
(`DEV-<method>-<order id>`): its predictability was
the compounding half of the old CR-01 forgery, which
Wave 39 closed at the secret, not the string.

**OTP request throttle atomicity (HI-04/F04 +
HI-06/F06).** The request path admitted its own race:
the hourly count check, the log insert and the
code replacement were separate statements, so
concurrent requests for one phone could burst past
the 20/hour cap and interleave DELETE/INSERT into two
live codes. All of it now runs in ONE transaction
serialized per phone by a transaction-scoped advisory
lock keyed on the phone (no per-phone row exists to
lock before the first request — the advisory lock is
the anchor, released at commit), and the SMS is sent
only after the commit, so a delivered code is always
the committed one. A refused request still writes
nothing. The wave41 test races six requests against
three remaining slots: exactly 3 succeed, 3 are
refused, the log holds exactly 20 rows, and exactly
one live code survives.

**Dependency advisory visibility (A14-15).** CI gains
a `security-audit` job running `cargo audit` (via the
official `rustsec/audit-check` action, pinned) against
Cargo.lock on every run — the jsonwebtoken CVE class
becomes visible the day it is published, not at the
next manual audit. The job is deliberately NON-GATING
this wave: separate from the rust/web/docker jobs and
marked continue-on-error, so it reports without
blocking. Making it gating is a follow-up once the
first reports are triaged; no unrelated dependency
advisories were fixed in this wave. (Follow-up: the
first report was triaged in Wave 42, and the gating
flip landed in Wave 45 — see those sections.)

**Refunds reach the payment record (HI-52, migration
0037).** `refunded` existed in the schema and the
transition table since Wave 5, but no code ever moved
a payment there — a return refund recorded `done`
off-platform left the payment and the order saying
`paid` forever. The audit's gateway half (an admin
endpoint calling the adapter's `refund()`) still
cannot exist honestly: every adapter's refund errors
today (real gateways NotConfigured/NotImplemented,
the DEV simulator and COD Unsupported), so it would
only ever fail or pretend. What landed is the audit's
"until the live gateway refund exists" rule: the
admin refund-done handler now folds the refund into
the payment tables IN THE SAME TRANSACTION (its
status read also takes the row lock, so two admins
cannot double-mark and double-count). For an order
collected online (`payment_status = 'paid'`), the
refund accumulates onto the payment's new
`refund_amount_poisha` — partial refunds stay visible
as partial amounts on a still-`paid` payment — and
when EVERY sub-order of the order has its refund
done, the payment flips `paid -> refunded` with
`refunded_at` stamped and `orders.payment_status`
follows. The accumulated figure is the exact recorded
refund (goods minus coupon discount per return); it
can differ from the payment amount because the
delivery charge is never refunded (Wave 16). COD and
advance-paid orders are untouched: their money never
sat in a gateway payment this record describes, and
their return refund record stays the whole story.
Admin/buyer visibility rides the existing surfaces —
the payment object now carries `refund_amount_poisha`
and `refunded_at`, and order views show the flipped
status.

**Backlog after Wave 41.** The remaining audit
findings (the rest of the High list — cart-locking,
ship-before-payment gates, payout clawback and
holdback, loyalty expiry fidelity, session revocation,
per-IP throttling beyond the per-phone OTP cap — plus
the Medium/Low registers and CR-13's test-isolation
redesign and CR-15's owner policy decision) stay
queued in the audit's priority order for future
waves. PR #1 holds the full register; nothing has
been silently dropped.

Wave 42 (dependency hygiene — acting on the first
cargo-audit report):

**The first report.** With the Wave 41 fix resolving
the lockfile in-job first, the `security-audit` job
now reports for real: over 291 dependencies it found
**3 vulnerabilities and 1 informational warning**.
Each is dispositioned below. Nothing was ignored away
silently — the two accepted risks live in
`.cargo/audit.toml` with written justifications and
removal triggers, in the open, where cargo audit
still prints them on every run.

**RUSTSEC-2026-0459 (libsqlite3-sys) — REMOVED, not
ignored.** The bundled SQLite (CVE-2026-119375,
patched in libsqlite3-sys ≥ 0.39) was in the graph
because every sqlx declaration in the workspace rode
the umbrella crate's default features even though
each crate declares Postgres-only drivers — the
defaults' `any` member is what kept the MySQL and
SQLite drivers in the resolved graph for a platform
that never opens either database. All four sqlx
declarations (api, worker, notify, and notify's
migrate-only dev-dependency) now set
`default-features = false`. What the defaults were
silently providing is restored explicitly only where
the code provably uses it: `macros` (the FromRow
derives throughout, and the `migrate!` calls in api
and worker) — `migrate` itself was already explicit
in api/worker. Two defaults are deliberately NOT
restored: `json`, which this codebase never exercises
(jsonb is bound and read as text with `::jsonb` /
`::text` casts — see audit.rs and the notify payload),
and `any`, dropped outright (no `sqlx::Any` types
exist in the tree). In sqlx 0.8.6's published feature
graph the MySQL/SQLite driver crates are reachable
only through the `mysql` / `sqlite` / `all-databases`
features, none of which this workspace enables. CI
lock forensics then corrected the first reading of
that graph: the generated lock's parent records show
`sqlx-sqlite` is ALSO a dependency of
`sqlx-macros-core` — sqlx 0.8's migration-macro
support welds all three drivers into the macros
crate, so the drivers stay in the resolved graph
while `sqlx::migrate!()` is used, on the
compile-time side only (never linked into the
shipped binaries, never opened at runtime). The
sqlite advisory is therefore an accepted-risk ignore
in the audit.toml with that mechanism and its
removal triggers written next to it (the audit.toml
says so, in writing, next to the
ignores).

**RUSTSEC-2023-0071 (rsa) + RUSTSEC-2026-0387
(crypto-bigint) — accepted-risk ignores, HS256-only.**
Both crates enter solely through jsonwebtoken 10.4's
`rust_crypto` feature: `rsa` directly (Marvin Attack
timing sidechannel, patched in rsa ≥ 0.10) and
`crypto-bigint` 0.5.5 via p256/p384 → elliptic-curve
(timing variability, patched ≥ 0.7.6) — in other
words, jsonwebtoken's RSA/ECDSA support. Neither can
be bumped away from here: jsonwebtoken 10.4.0's
manifest declares `rsa = "^0.9.6"`, which excludes
the patched 0.10 line, and elliptic-curve 0.13 pins
crypto-bigint to the 0.5.x line. The ignores in
`.cargo/audit.toml` rest on one verifiable fact: this
platform signs and verifies JWTs with HS256 (HMAC)
ONLY — the only jsonwebtoken key operations in the
tree are `EncodingKey::from_secret` /
`DecodingKey::from_secret` with
`Validation::new(Algorithm::HS256)` in
crates/api/src/auth.rs, and a tree-wide grep finds no
`from_rsa` / `from_ec` constructors and no RS*/ES*
algorithm identifiers. The vulnerable operations
(RSA private-key decryption/signing timing, ECDSA
constant-time arithmetic on secret material)
therefore never execute. Each ignore names its
removal trigger: the first jsonwebtoken release
whose tree resolves rsa ≥ 0.10 / crypto-bigint ≥
0.7.6 — the audit job keeps printing both advisories
until then, so a stale ignore cannot hide.
(Follow-up: Wave 47's jsonwebtoken 12 bump fired
the crypto-bigint trigger in full — 0.7.11 is the
only resolved version and that ignore is retired.
The rsa trigger fired only halfway: jsonwebtoken
now resolves rsa 0.10.0, but rsa 0.9.10 remains in
the resolved graph via sqlx-mysql, so that ignore
stays with a rewritten justification — see the
Wave 47 section.)

**RUSTSEC-2026-0543 (chrono unmaintained) — noted,
not acted on.** Informational-warning class only,
not a vulnerability. chrono is load-bearing across
the API's timestamps today; replacing it with `time`
or `jiff` is a deliberate refactor wave of its own,
queued as such rather than smuggled into a hygiene
wave.

**The rule going forward.** New advisories get
triaged per wave: bump or remove where the graph
allows it; ignore only with a written justification
and a removal trigger in `.cargo/audit.toml`. The
audit job stayed NON-GATING while the accepted-risk
ignores were new; once the tree was clean, making it
gating was a one-line CI change (drop the job's
`continue-on-error`). (Follow-up: that flip landed
in Wave 45 — see that section.)

Wave 43 (observability — request ids, request logs
and an admin metrics snapshot):

**Request ids.** Every request the API answers now
carries an `X-Request-Id`. A client-supplied id is
honoured when it passes the pure domain rule
(`domain::validate_request_id`: 1–64 characters,
printable ASCII only, no leading/trailing padding —
the value lands in a response header and a log line,
so its shape is a safety property, not a courtesy);
anything else (absent, oversized, control bytes,
non-ASCII) is replaced with a freshly minted UUID in
simple form, never with an error. The id is echoed
on the response, so a client report, a proxy log and
the API's own log can be correlated by one string.

**Request logs.** The outermost middleware
(`observability::request_logging` in the api crate)
writes exactly ONE line per request after the
response completes — `request request_id=… method=…
path=… status=… latency_ms=…` — to stdout, or to
stderr when the status is 5xx. `path` is the matched
route PATTERN (`/v1/orders/:id`), not the raw URL:
low-cardinality, and it can never contain a value;
unmatched requests fall back to the raw path.
Deliberately NEVER logged: query strings (the public
tracking endpoint takes a phone number in its query —
that is PII), request/response headers, and bodies.
The line is printed, not emitted through a logging
framework: neither binary carries a tracing
subscriber (every log line in this codebase is
stdio), so the structured key=value fields are
written straight to the stdio handles — with
`writeln!`, so a broken log pipe can never fail a
request.

**Admin metrics.** `GET /v1/admin/metrics` (admin
only, behind the one `require_admin`; API keys stay
locked out by the Wave 37 admin route exclusion)
returns one point-in-time snapshot, computed per
request by a set of aggregate queries
(`metrics::get_admin_metrics`):

- `orders_today` — parent orders created on the
  current Asia/Dhaka day, and the day's GMV: the
  Wave 27 formula, SUM(goods − discount + delivery)
  over the day's non-cancelled sub-orders.
- `orders_by_status` — parent-order counts per
  status (`placed` / `confirmed` / `delivered` /
  `cancelled`; only statuses present appear).
- `pending_payouts` — the payout queue an admin
  still has to decide: status `requested`, count +
  total poisha (the Wave 10 overview's definition).
- `open_returns` — return requests in `requested`
  or `approved`, the returns lifecycle's
  non-terminal states (`rejected` and `received`
  are terminal for a request; a received return's
  refund continues on its own field).
- `notification_backlog` — the Wave 29 outbox
  counted per status (`pending` / `sending` /
  `sent` / `failed`). A `sending` count that never
  drains is the stuck-batch signal the Wave 29
  design warned about: a worker that dies mid-batch
  leaves its claimed rows there.
- `stale_shipments` — non-manual shipments in a
  non-terminal status (the Wave 30
  `POLL_TERMINAL_STATUSES` set, reused rather than
  re-listed) untouched for over 15 minutes — work
  the poller owes.
- `users_total` / `stores_total` /
  `products_active` — catalogue totals (active =
  `status = 'active'`).

The admin overview page shows the snapshot as its
সিস্টেম অবস্থা — System status card.

**Admin overview remake.** The overview page was
rebuilt after a ~100-agent analysis of the surface,
and two long-standing definition bugs were fixed at
the source:

- The overview's GMV now uses the canonical Wave 27
  formula (goods − discount + delivery over
  non-cancelled sub-orders) — it previously summed
  goods only, so the overview and the metrics
  snapshot printed two different figures both
  labelled "GMV". The field is renamed
  `gmv_goods_poisha` → `gmv_poisha` in
  `GET /v1/admin/overview`.
- The overview's `commission_earned_poisha` now sums
  the commission snapshot over DELIVERED sub-orders
  only (the seller-earnings definition) — it
  previously counted placed/confirmed/shipped/
  returned sub-orders as earned.
- Gift-card `outstanding_poisha` (shared by the
  seller stats endpoint and the metrics snapshot)
  now counts only balances on spendable cards
  (active: not disabled, not expired, balance > 0).
  Two new fields carry the rest —
  `frozen_disabled_poisha` (balances on disabled
  cards) and `expired_unredeemed_poisha` (balances
  stranded on expired cards) — and
  `adjusted_net_poisha` totals the ledger's
  `admin_adjust` deltas, so the books reconcile:
  initial − redeemed + restored + adjusted = the sum
  of all current balances. Gift cards are
  seller-funded; the outstanding figure is the
  sellers' aggregate liability, not cash the
  platform holds.
- The metrics snapshot gained operator blocks, all
  reusing existing definitions: `orders_yesterday`
  (today's two definitions one Dhaka day back, for
  deltas), `sub_orders_by_status` and
  `orders_by_payment_status` (sparse groupings;
  `advance_pending` is a payment status, never an
  order status), `advance_pending` (advances still
  owed: count, outstanding, blocked order value,
  >24h / >72h counts, oldest age), `advance_unfulfilled`
  (advances required but never collected — the
  degraded Wave 17 population, a different set),
  `refunds_pending` (received returns whose refund
  is not marked done), `verifications_pending`
  (KYC applications awaiting review, with the oldest
  age), `stock_alerts` (active products out of / low
  on the variant-aware effective stock),
  `growth_today`, `stores_suspended`,
  `active_promotions`, `live_flash_sale`, an
  `oldest_age_secs` on `pending_payouts`, and age
  fields on `notification_backlog` (oldest pending
  age, oldest sending-claim age by the Wave 48
  `claimed_at` stamp, stale-claim count by the
  notify crate's own threshold, sends in the last
  24h, last-send age).
- The page itself is now an operator home: KPI tiles
  with yesterday deltas, a "needs attention" queue
  whose rows deep-link to the page that clears them,
  the sub-order pipeline with canonical Bangla status
  labels, a 7-day orders chart and top stores from
  the Wave 27 analytics, geography bars, compact
  platform blocks, the latest search analytics and
  merchandising events, and the head of the audit
  log. Every section has its own loading skeleton,
  empty state, and error state with retry — a failed
  section no longer vanishes silently.

Honest limits of the remake: there is deliberately
no "failed notifications in the last 24h" (the
outbox records no failure timestamp, so it cannot
be computed); stock-alert counts are shown unlinked
because no admin inventory page exists yet; queue
deep links target whole pages, not pre-filtered
views, because no admin page reads filter query
parameters yet; and the parent-order status remains
write-mostly (it only ever becomes `placed` or
`cancelled` in practice) — the pipeline therefore
leads with sub-order statuses, which are the real
fulfilment axis.

**Worker cycle line.** Each worker cycle now ends
with ONE summary line — notifications
sent/retried/failed, shipments
checked/advanced/unchanged/failed, subscriptions
checked/renewed/expired — printed every cycle. The
previous per-job lines appeared only when something
moved, so an idle worker and a dead one looked the
same in the log; a job that errors still reports on
stderr as it happens and counts as zeroes in that
cycle's summary.

**Honest limits.** Logs are stdout/stderr text —
there is no Prometheus or OpenTelemetry exporter,
no log aggregation and no alerting. The metrics are
point-in-time database aggregates recomputed on
each request, not a time series, and nothing is
retained beyond the rows themselves. Request ids
correlate one process's lines; there is no
cross-service tracing — the SIDRA bridge and the
courier calls do not propagate the id.

Wave 44 (payout idempotency — the `Idempotency-Key`
contract):

Payout creation (`POST /v1/seller/payouts`) now
accepts an optional `Idempotency-Key` header — the
third element of the audit's payout prescription
(CR-10), deferred by Wave 39 until a client contract
existed. This closes that deferral: the race itself
was closed in Wave 39 (one transaction, the store row
locked `FOR UPDATE`, the approve-time re-check); what
remained was the double-submit problem — a seller
whose browser retries the POST, or who double-clicks,
created a second `requested` row an admin had to
reject by hand.

**The contract.** The key is validated before any
database work by the pure domain rule
(`domain::validate_idempotency_key`: 1–128
characters, visible ASCII only — no spaces anywhere,
unlike the Wave 43 request-id rule, because the key
is stored and matched byte-for-byte and a space is an
invisible difference). A present-but-invalid key is a
400, never silently ignored — silence would recreate
the duplicate the key was sent to prevent. Inside the
Wave 39 transaction, after the store lock, the key is
looked up for the store:

- **Same key, same amount** → the ORIGINAL payout is
  returned with **200** (not 201), and nothing else
  happens: no second row, no balance movement. The
  request body carries only the amount — the account
  fields are a server-side snapshot, not part of the
  request's identity — so amount equality is the whole
  identical/different comparison.
- **Same key, different amount** → **409**: reusing
  a key for a different request is a client bug, and
  the seller hears about it instead of silently
  getting either payout.
- **No key** → exactly the pre-Wave-44 behaviour,
  byte for byte. Keyless rows stay outside every rule
  below.

**Storage and the race backstop.** Migration 0038
adds `payouts.idempotency_key` (with a CHECK mirroring
the domain rule) and a partial UNIQUE index on
`(store_id, idempotency_key)` WHERE the key is
present. The scope is per store, not per user column
and not global: the payouts table has no seller-user
column — a payout belongs to a store, and the request
path is owner-only (`manage_payouts`, Wave 34),
resolving exactly the owner's payout store, so
(store, key) is precisely "this seller's request with
this key". Because the lookup runs inside the
transaction under the store lock, a concurrent
same-key request through the API resolves as a replay
at the lookup; the unique index is the backstop for
any path that ever reaches the insert without the
lock — an insert that hits it (SQLSTATE 23505) rolls
its transaction back and retries the WHOLE operation
once, so the retry's lookup finds the winner's row
and applies the same replay/conflict rules. The key
is echoed on the payout view (seller list, admin
list, export), so a "was I charged twice?" support
question can be answered from the rows themselves.
Payout requests remain JWT-only — the Wave 37 route
exclusions already refuse API-key auth for
`POST /v1/seller/payouts`, unchanged.

The web seller page plays its part: the payout form
mints ONE key (`crypto.randomUUID()`) per form
session, sends it on every submit attempt — double
clicks and retries included — and rotates to a fresh
key only after a success, so a genuinely new second
request is never swallowed as a replay.

**Honest limits.** Keys are scoped per store, not
global. A key has no expiry or TTL: it lives exactly
as long as its payout row, so a client that reuses a
key weeks later still gets the replay/conflict
answer for that store. Only payout requests are
covered — other mutating endpoints keep their
existing duplicate semantics (checkout has its own
guards; a general idempotency layer is a design of
its own, not a drive-by).

Wave 45 (supply-chain & CI maturity — the audit job
gates, and Dependabot keeps the tree current):

**The audit job is GATING now.** The `security-audit`
job drops its `continue-on-error` and its
"(non-gating)" name: a new unignored RustSec advisory
now fails the CI run's conclusion. Why now: the tree
has been clean since Wave 42 — 0 unignored
vulnerabilities, with the three accepted-risk ignores
documented in `.cargo/audit.toml` (two HS256-only JWT
subtree advisories, one compile-time-only sqlite) —
and the job has run fully green since, including its
check-run reporting once Wave 42 gave it
`checks: write`. Nothing else about the job changed:
it still resolves a fresh lockfile in-job, still
prints the lock-parent diagnostic, and still runs in
parallel — no job `needs` it, so a red audit fails
the run without serialising the pipeline. This is
the one-line flip the Wave 41 and Wave 42 sections
promised, landed once its precondition (a clean,
triaged tree) actually held.

**Dependabot.** `.github/dependabot.yml` opens the
update PRs that keep the tree current: cargo (repo
root) and npm (`apps/web`) weekly — Monday 09:00 /
09:30 Asia/Dhaka — with minor and patch updates
grouped into one PR per ecosystem and majors arriving
individually; github-actions monthly. The contract,
also written at the top of that file: a Dependabot PR
is a proposal, never an auto-merge — it merges only
when CI is fully green, the now-gating audit
included, and a human has looked at it; a cargo major
bump of a security-relevant crate (jsonwebtoken,
sqlx, axum) gets a human read of the changelog first,
not just a green run.

**Honest limits.** The gate covers Rust advisories
via cargo-audit only: `npm audit` is not wired as a
gate, though Dependabot still surfaces npm security
updates as PRs like any other update. The
accepted-risk ignores remain the documented
exception path, under the Wave 42 discipline: an
ignore is acceptable only with a written
justification and a removal trigger next to it in
`.cargo/audit.toml` — a bare ignore is not. And
gating means exactly what it says: a freshly
published RUSTSEC advisory can turn main red until
it is triaged. That pressure is intended. The triage
order of preference is: fix it, bump past it, or —
only when neither is honestly possible — a
documented ignore.

Wave 46 (load smoke — a regression smoke over the
public read paths):

`crates/api/tests/wave46.rs` adds a bounded,
deterministic burst to the integration suite. It
seeds one store with twelve active, categorised
products through the public API, then fires 200
anonymous GET requests — 8 concurrent tokio workers
x 25 requests, round-robin across the four hot
public reads (product list, product detail rotating
over the seeded ids, search, and the storefront
read `GET /v1/stores/{slug}`), so each route
receives exactly 50. It asserts exactly three
things: every response is 200 (any 4xx/5xx fails,
naming route and status); each route's count is
exactly 50 (no silently skipped workers); and the
whole burst finishes under a generous 120-second
pathology ceiling whose only job is to catch
accidental N+1 storms or missing-index table scans
at seed scale. Per-route min/median/max latencies
are printed to the CI log as observational data and
are never asserted.

**This is a regression smoke, not a benchmark.**
In-process oneshot requests on a shared CI runner
prove the read paths survive concurrency without
errors and without pathological slowdowns at seed
scale; it does NOT measure production capacity,
network behaviour, or Postgres under real load, and
no capacity numbers are claimed from it. A real
load test needs a deployed environment and remains
future work.

Wave 47 (jsonwebtoken 12 — landing Dependabot PR
#13's bump as a wave, and settling the Wave 42
audit ignores):

**The bump.** `crates/api` moves jsonwebtoken
10.4.0 → 12.0.0, the exact bump Dependabot proposed
in PR #13, landed here as a proper wave on main
instead of merging the bot PR (PR #13 is closed as
superseded once this lands — the parent handles the
PR). The committed `Cargo.lock` entry was
hand-updated (version + checksum, taken from PR
#13's lock diff); as in Waves 39/41, cargo completes
the re-resolution in CI, which does not run
`--locked`.

**Feature discipline — verified against the v12
manifest, unchanged.** v12 keeps the layout Wave 39
chose: `default = ["use_pem"]`, and `rust_crypto`
is the RustCrypto provider set (ed25519-dalek 3,
hmac 0.13, p256/p384 0.14, rsa 0.10, sha2 0.11).
The declaration is therefore byte-identical apart
from the version: `default-features = false,
features = ["rust_crypto"]` — exactly one crypto
backend, which v12's new `CryptoProvider` machinery
auto-installs as the process default, and PEM key
parsing stays off because this crate only ever
constructs `from_secret` HMAC keys. (v12's MSRV is
1.88, well under the CI toolchain.)

**The one breaking change that reaches us.**
v12's crypto decoupling made
`EncodingKey::from_secret` and
`DecodingKey::from_secret` return `Result` — they
fail only on an empty secret
(`ErrorKind::InvalidKeyFormat`). Those are the
only two call sites in the tree (both in
`crates/api/src/auth.rs`; a tree-wide grep finds no
other jsonwebtoken use), and they adapt in place:
`issue_token` maps the key error into the same 500
"failed to sign token" as an encode failure (an
empty secret is a server misconfiguration, and
Wave 28's `validate_jwt_secret` keeps a real
deployment from reaching it), while `authenticate`
folds it into the same 401 as every other
verification failure, keeping that function's
single error shape. Everything else this platform
uses is unchanged in v12 — `encode` / `decode`
signatures, `Header::default()` (alg HS256),
`Validation::new(Algorithm::HS256)` with required
`exp` and 60-second leeway, `TokenData.claims`, and
the `Claims` struct itself. JWT semantics are
identical: HS256 only, same claims fields, 7-day
expiry, secret handling untouched — and because
HS256 is the standardised JWS HMAC construction,
tokens issued under 10.4 verify under 12 and vice
versa. No tests changed: the OTP→JWT flows across
the wave suites exercise issue/verify as before.

**The audit ignores — one retired, one rewritten.**
The bump was expected to settle both Wave 42
ignores; the fully-resolved graph (PR #13's lock)
shows it settles one and a half:

- **RUSTSEC-2026-0387 (crypto-bigint) — RETIRED.**
  Its removal trigger fired exactly as written:
  p256/p384 0.14 → elliptic-curve 0.14.3 resolves
  crypto-bigint 0.7.11 as the ONLY version in the
  graph (patched line is ≥ 0.7.6); the vulnerable
  0.5.5 is gone entirely. The ignore is removed
  from `.cargo/audit.toml`, with the retirement
  recorded in that file's footer.
- **RUSTSEC-2023-0071 (rsa) — KEPT, justification
  rewritten.** jsonwebtoken's half of the trigger
  fired: v12 declares `rsa = "0.10"` and resolves
  rsa 0.10.0 (patched line ≥ 0.10.0). But a fresh
  resolution still contains rsa 0.9.10, whose only
  remaining parent is `sqlx-mysql` 0.8.6 — the
  Wave 42 welding mechanism (sqlx-macros-core pulls
  all three drivers into the resolved graph while
  `sqlx::migrate!()` is used; sqlx-mysql's manifest
  pins the old rsa). The advisory therefore still
  matches the resolved graph, and since the audit
  job has been GATING since Wave 45, deleting the
  ignore would turn CI red over a crate that is
  compile-time-only (the shipped binaries enable
  sqlx's postgres features only — sqlx-mysql is
  never linked) and whose vulnerable operations no
  code path can reach (HS256-only, as before). The
  audit.toml entry now says exactly that, with a
  new removal trigger: a sqlx release resolving
  rsa ≥ 0.10 for its MySQL driver, sqlx unwelding
  the drivers, or this platform dropping
  `sqlx::migrate!()`. One honest correction to the
  Wave 42 record: the fully-resolved graph shows
  rsa 0.9.10 had TWO parents all along
  (jsonwebtoken 10.4 AND sqlx-mysql) — the Wave 42
  note, written from the committed lock's parent
  records, named only jsonwebtoken. The HS256-only
  safety argument was never affected.

Wave 48 (notification reaper — closing the Wave 29
stuck-`sending` edge):

**The edge.** The Wave 29 outbox claim flips a batch of
pending rows to `sending` and commits BEFORE any send
happens — deliberately, so no row lock is held across a
network call. A worker that dies after claiming (crash,
deploy, OOM) therefore strands its batch in `sending`
forever: nothing else ever looks at that status, and the
only signal was the `sending` count in the Wave 43 admin
metrics, waiting for a human to notice it never drains.

**A correction the code forced.** The reaper needs to
know when a row was CLAIMED, and the `notifications`
table could not say: migration 0029 has no `updated_at`,
and the claim UPDATE stamped nothing at all — only
`created_at` (the event's age, not the claim's; judging
by it would reap rows a live worker claimed seconds ago
out of a long-queued backlog). **Migration 0039** adds
`claimed_at timestamptz`: the claim UPDATE stamps it,
both outcome write-backs clear it, so it is non-NULL
exactly while a claim is live, and a `(status,
claimed_at)` index backs the reaper's scan. A `sending`
row with a NULL stamp is reaped as stale by definition —
the stamp has existed since this wave, so an unstamped
claim can only be a pre-Wave-48 one, i.e. precisely the
stuck population the reaper exists to rescue.

**The fix.** `notify::reap_stale_sending(pool,
stale_minutes) -> ReapReport { returned_to_pending,
marked_failed }` — one UPDATE (a single statement, so a
single transaction) over every `sending` row whose claim
is older than the threshold. A recovered row goes back
to `pending` — unless the lost claim spends its last
attempt, in which case it goes to `failed`. That clause
needs one honest explanation: `attempts` counts RECORDED
SEND FAILURES (the claim itself does not increment it —
verified in the claim SQL), so a row whose worker keeps
dying before any send completes would otherwise be
reaped and re-claimed forever at attempts 0. The reap
therefore consumes one attempt — the claim produced no
delivery, so it counts against the same `MAX_ATTEMPTS`
(3) budget as a failed send — and writes a `reaped: …`
note into `last_error`, so the admin view explains why
the row moved. The worker runs the reaper at the TOP of
every cycle, before `process_pending`, so a recovered
row can be claimed and sent in the same cycle it is
reaped; its counts ride the Wave 43 summary line as
`reaped_pending` / `reaped_failed` inside the
notifications group.

**The tunable.** Staleness defaults to
`notify::DEFAULT_SENDING_STALE_MINUTES` = 10 minutes,
overridable via `NOTIFICATION_SENDING_STALE_MINUTES`
(the worker reads it with the usual pattern: unparsable
or zero → warning + default; zero is NOT a valid setting
here, unlike the shipment staleness — zero would reap
rows another worker claimed seconds ago). The notify
crate reads no platform settings (it never has), so this
stays env > const, resolved once in the worker like
`WORKER_POLL_SECONDS`; there is no DB layer to add.

**Safety.** A legitimate in-flight send is one SMS HTTP
call measured in seconds, against a 10-minute floor — a
false reap is practically impossible, and even one could
only cause a duplicate: the outbox is at-least-once by
design (a row counts as delivered only when its send
outcome is written back), so the worst case is a second
SMS, never a lost one. Covered by
`crates/api/tests/wave48.rs`, which drives the notify
crate on the pool exactly as the worker does: a fresh
claim is never touched, a stale claim with budget left
returns to `pending` (attempt consumed, note recorded)
and is then delivered by the same cycle's drain, a stale
claim with its budget spent lands in `failed` and is
never claimed again, and an aged-out claim recovers
through the same path.

Wave 49 (launch runbook — `docs/LAUNCH.md`):

**Docs only — no code, no config.** Forty-eight waves
of honest limits, onboarding-confirm items and owner
decisions were scattered across this README's wave
sections, `.env.example` and the security-audit doc.
`docs/LAUNCH.md` consolidates them into one
operator-facing runbook that takes a fresh operator
from "repo cloned" to "live and selling": the owner
decisions owed before launch (CR-15's
advance-degradation policy — flag-and-proceed today,
the owner's call to change; the 8% commission and
৳500 payout-minimum defaults to ratify; the
placeholder product name), the credentials & accounts
checklist (the SMS gateway as THE launch blocker —
one `SmsSender` implementation in the `notify` crate
away; the four payment gateways with the
signature-verification caveat; the five API couriers
with each one's written-confirm items; Meta app
review for Page import; the SIDRA bridge), the
production configuration checklist (the must-set env,
the three dev simulators that must be OFF, SQL-only
first-admin promotion, the platform_settings knobs
worth reviewing), the ordered go-live sequence with
the end-to-end smoke path, the known-limits shortlist
(the Wave 46 smoke is not a load test, CR-13's
test-isolation redesign is still owed, the audit
register beyond Waves 39/41/44's named closures, the
never-audited frontend, the remaining Dependabot
majors, the two accepted-risk audit ignores), and
the where-the-signals-live guide for when something
breaks. Everything in it is derived from the repo's
own sources; where this README and the runbook
disagree, this README (and the code) wins.

Wave 50 (program complete — final consolidation):

**Docs only — no code, no config.** This is the final
wave of the 50-wave program the project was built
under, and it closes the record rather than adding a
feature.

**The arc.** Waves 1–10 built the core commerce: the
poisha-exact domain, phone+OTP auth, stores and
products, split-cart checkout with COD, the online
payments foundation, the first three courier adapters
behind one trait with smart routing, reviews, seller
earnings and payouts, and the platform admin. Waves
11–20 were the Bangladesh growth layer: the remaining
API couriers (eCourier, Paperfly), Facebook Page
import with its Bangla price parser, buyer↔store
messaging with the SIDRA bridge, coupons, Postgres
search, public tracking and returns, the COD partial
advance and fraud shield, storefront themes, the four
manual couriers, and deployment readiness. Waves
21–36 were scale and trust: seller KYC tiers,
per-store invoices, inventory with an audited stock
ledger, wishlist and recommendations, product Q&A,
sponsored promotions, analytics, security hardening
with the admin audit log, the notification outbox and
the real worker, automatic courier polling, backups
and data export, the performance pass, product
variants, multi-store team access, loyalty points and
seller subscription plans. Waves 37–45 opened the
platform up and answered the security audit:
third-party API keys with rate limiting, the PWA
shell, the two security-fix waves (39 and 41) that
closed the audit's named money-path Criticals and
Highs (payout idempotency followed in Wave 44), the
seller fulfillment center, dependency hygiene,
observability (request IDs, structured request logs,
admin metrics), payout idempotency, and supply-chain
maturity — the cargo-audit job made gating and
Dependabot switched on. Waves 46–49 finished the
maturity list: the CI load smoke, the jsonwebtoken 12
migration, the notification reaper, and the launch
runbook.

**Final state — counted, not remembered** (counted
from this repo at this wave): **39 migrations**
(0001–0039), **6 Rust workspace crates** (`api`,
`courier`, `domain`, `notify`, `payments`, `worker`)
plus the Next.js web app, and **44 integration test
files** in `crates/api/tests/` (43 wave-numbered files
plus `ci_gate.rs`), alongside the crates' unit tests.
CI is 4 jobs: `rust` (workspace tests + clippy
`-D warnings` against a Postgres 16 service, plus the
backup/restore proof), `web` (Next.js build + the PWA
wiring check), `docker` (the three image builds) and
`security-audit` (cargo audit — **gating** since
Wave 45, with the two documented accepted-risk ignores
in `.cargo/audit.toml`). Per the project's CI-only
rule, nothing in this program was built or tested on
a development machine: every wave's verdict was
verified from GitHub Actions at job/step level,
twice, before the wave was called closed.

**What "done" means — and what it does not.** The
platform is feature-complete per the 50-wave program
and launch-ready per `docs/LAUNCH.md`. Going live is
not a development task any more, but it is not done:
it still needs the credentials and the owner
decisions listed in the runbook — a real SMS gateway
first among them (without one, OTP login and every
notification stay in dev mode), then the payment and
courier merchant accounts with their written-confirm
items, the Meta app review for Page import, the
SIDRA configuration, and the owner's calls on CR-15,
the commission defaults and the product name.
Post-50 work is named here as future work, NOT
silently queued: the remaining Dependabot majors
(Next 16, rand 0.10, tower 0.5, TypeScript 7, React —
PRs #2–#14, minus #13, which landed as Wave 47), the
CR-13 test-isolation redesign, a real load test in a
deployed environment (Wave 46 is a smoke, not a
benchmark), the remaining audit backlog in the
audit's priority order, and a frontend audit (the
Wave 39/41 audit covered the backend only).

**Record repairs in this wave.** Verifying the counts
above surfaced one gap in this README itself: the
Scope summary was missing its **Real (Wave 29)**
entry — the Wave 29 section always existed, its
summary line did not. The entry has been restored
(distilled from the section, nothing invented), and
three framing areas that present themselves as
current state were corrected to match the record:
the Monorepo layout (the `notify` crate was missing
and the `SmsSender` trait still showed its pre-Wave-29
home in `auth.rs`), the Deployment Honest limits (the
worker bullet still claimed the worker does no jobs),
and Verification status (below).

Wave 51 (product media system — the Shopify-parity program opens):

**The first wave of the post-50 Shopify-parity program
(branch `shopify-parity`): products get real images, end
to end.** Until this wave the data model had no image
field at all — every card, rail and detail page rendered
text only. The wave adds the complete media system:

- **Model (migration 0040):** `product_images` — one row
  per image, either an UPLOAD (`storage_key`) or an
  EXTERNAL hotlink (`external_url`), never both (a CHECK
  enforces exactly one); width/height/size facts, dense
  `position`, `is_primary` under a partial UNIQUE index
  (at most one primary per product), optional
  `variant_id` (SET NULL on variant delete) and alt text.
  Cap: 12 images per product. Migration 0041 adds the
  review photo columns (`image_storage_key` + dims).
- **Domain rules (pure, unit-tested):** the 10 MiB file
  cap, magic-byte sniffing for JPEG/PNG/WebP with
  declared-type agreement, dimension caps, alt-text and
  external-URL validation (http/https only), the derived
  size planner (1200/600/300, never upscale, aspect kept),
  the best-size picker every view uses, the primary/
  position normalisation that the reorder, delete and
  bulk paths all run through, and the media-key rule
  that makes path traversal unrepresentable.
- **Storage + processing (`media` module):** a
  `MediaStore` trait with two drivers — `LocalDiskStore`
  (default; files under `MEDIA_DIR`, served by the API
  itself at `GET /media/*key` with sniffed content type
  and `Cache-Control: public, max-age=31536000,
  immutable`) and `S3Store` (a hand-rolled SigV4
  PUT/GET/DELETE for MinIO or any S3-compatible
  endpoint; selected via `MEDIA_STORE=s3`). Uploads are
  decoded with the `image` crate and re-encoded — the
  original with metadata stripped, plus one WebP per
  planned derived width (Lanczos3). Object keys are
  deterministic (`products/<product>/<image>-orig`,
  `-<width>.webp`), so views, deletes and serving never
  need a key table. An S3 selection with an incomplete
  `MEDIA_S3_*` set fails every operation with the typed
  `media_not_configured` 503 — never a fake success.
- **Seller endpoints** (behind `manage_catalog`; a
  product the caller has no relationship with is the
  same 404 as a missing one): multipart upload
  (`POST /v1/seller/products/:id/images`, 12 MiB body
  limit layered on this route only — the global 1 MiB
  cap stands elsewhere; first image auto-primary),
  attach-by-URL (`.../images/from-url`), the management
  list, ONE-transaction batch update (`PATCH
  .../images`: reorder + primary + alt + variant
  assignment, normalised by the domain rule; primary
  flagging is opt-in — a batch that flags nothing, like
  an alt-text edit, preserves the stored primary), and
  delete (`DELETE .../images/:image_id` — the row goes,
  the survivors renormalise so the next image promotes,
  the objects are removed after commit). Plus the
  extras: the cross-store media library
  (`GET /v1/seller/media` + `POST
  /v1/seller/media/bulk-delete`, per-id skip semantics),
  store branding uploads (`POST
  /v1/seller/store/branding-image?kind=logo|banner` —
  real files behind the Wave 18 `logo_url`/`banner_url`
  fields), and one photo per review (`POST`/`DELETE
  /v1/reviews/:id/image`, author-only) shown in review
  lists.
- **Every product surface carries imagery:** product
  JSON everywhere (list, detail, search, storefront,
  related, recommendations, wishlist) gains `image_url`
  (the primary image's gallery-size URL, or null) from
  the shared `PRODUCT_COLUMNS`; cart lines carry it
  too; the detail gains the full ordered `images`
  gallery (card/gallery/zoom URLs per image); variant
  views (public picker + seller list) gain their
  assigned image's `image_url`. Facebook import drafts
  accept an optional `image_url`, attached through the
  from-url path after insert — a bad photo URL is
  logged and skipped, never fails the import. Admin
  metrics gain a `media` block (image totals, stored
  vs external, stored bytes).
- **Web:** `ProductImage` (srcset over the derived
  sizes, lazy, initial-letter placeholder when null),
  `ProductGallery` (stage, thumbnail strip, prev/next,
  keyboard navigation, lightbox at the zoom size),
  `ImageUploader` (multi-file + drag-and-drop with
  per-file XHR progress, URL attach, star-primary,
  move left/right, alt editing, variant assignment,
  delete with confirm) embedded per product on the
  seller products page; cards render the primary image
  across the grid, rails, search and wishlist; the
  detail page uses the gallery and shows review photos
  with author attach/remove; a new seller media-library
  page with bulk delete; the admin system-status card
  shows the media footprint; the PWA worker treats
  `/media/` as cache-first immutable (VERSION `w51-v1`).

- **Wave 51 extension (same wave, second half):** the
  pipeline grew the production edges — EXIF orientation
  is read from JPEG uploads and baked into the pixels
  before the metadata-stripping re-encode (a phone photo
  can no longer land sideways); a SHA-256 `content_hash`
  (migration 0042) powers the duplicate guard (the same
  bytes twice on one product are a bilingual 400); a
  per-store media quota (500 MiB of uploaded originals,
  a domain rule) gates every upload path; images can be
  REPLACED in place (`PUT .../images/:image_id` — slot,
  primary, alt and variant survive), uploaded in BULK
  (`POST .../images/bulk` — per-file outcomes by name),
  or FETCHED from a URL (`POST .../images/from-web` —
  the server downloads and stores the photo; the fetch
  is SSRF-guarded by domain rules: scheme/credential/
  loopback-name checks plus a blocked-IP check over
  every resolved address, with a documented
  `MEDIA_FETCH_ALLOW_PRIVATE_HOSTS` hatch that exists
  for development/tests and must stay off in
  production); the worker reaps orphaned media objects
  every cycle (objects no `product_images` / `reviews` /
  `stores` row names — `MediaStore::list` enumerates the
  local tree or S3 ListObjectsV2); and admins get a
  moderation surface (`GET /v1/admin/media` +
  audited `DELETE /v1/admin/media/:image_id`, with an
  admin web page). The web grew with it: fetch-and-store
  and replace controls in the uploader, a storage-usage
  meter in the media library, branding-file uploads on
  the branding page (replacing its "no upload yet"
  note), cart-line thumbnails, and variant-focus in the
  gallery (picking a variant shows its photo).
- **Wave 51 extension, third pass (same wave):** order
  lines (buyer receipt, seller orders, invoices) carry
  the product's CURRENT primary image as a live
  thumbnail — deliberately not a snapshot, unlike the
  title/price beside it; the media limits are platform
  SETTINGS (`media_max_images_per_product` 1..=50,
  `media_store_quota_mb` 10..=102400, edited on the
  admin settings page, defaults 12 / 500); uploads
  remember the file's original name (migration 0043),
  shown in the uploader, the library and admin media,
  and a blank alt falls back to a humanised suggestion
  from that name ("red-running-shoe_front.jpg" → "Red
  running shoe front"); bulk URL attach
  (`POST .../images/from-urls`, per-URL outcomes); S3
  private-bucket mode — `MEDIA_S3_PRESIGN_SECONDS` > 0
  makes public URLs presigned GETs (query-string SigV4,
  clock-pinned unit tests); cards cross-fade to the
  product's SECOND image on hover (`hover_image_url`
  on every product view); the gallery gained hover-zoom
  pan, touch swipe, fullscreen lightbox, an all-photos
  grid, and variant-first ordering; the library filters
  by source and product and sorts newest/oldest/largest;
  admin media filters by store slug and the metrics
  snapshot names the five heaviest stores by media
  bytes; the import preview lifts a photo link out of
  the post text into the draft; stored images carry
  quality warnings (`low_resolution`, `extreme_aspect`,
  `very_large`) computed by a domain rule and badged in
  the seller surfaces; the serve route answers
  `?download=1` with an attachment disposition (key name
  + sniffed extension); the worker's cycle line reports
  the media library's image/byte totals; and the product
  page emits schema.org Product JSON-LD with the full
  gallery, price and availability.

**Honest limits.** The S3 driver is implemented and its
signing is unit-pinned (fixed-clock header shape +
determinism), and its put/get/delete/list round-trip is
exercised against an in-process mock endpoint in unit
tests — but it has never run against a real bucket: no
MinIO/S3 credentials exist yet; the local driver is the
default and the one CI exercises end to end. The
from-web fetch checks resolved IPs before connecting,
but reqwest re-resolves at connect time, so a narrow
DNS-rebinding window remains (documented at the fetch
function). JPEG coverage in the integration suite is
PNG fixtures plus one EXIF-tagged JPEG built in-test.
Search suggestions carry no thumbnail (the suggestion
query dedupes by title, so a per-title image is
ill-defined). Order lines keep their title/price
snapshots; their thumbnails are the product's live
primary image (documented on the field — a replaced
photo updates old receipts' pictures, never their
prices).

Wave 52 (design foundation + merchandising home — the
Shopify-parity program continues):

**The storefront stops looking like a form demo, and the
home page becomes a merchandised storefront.**
`apps/web/app/globals.css` gains a real design-token
layer — CSS custom properties for the brand palette
(built on the brand green `#1b5e20`), an amber accent,
surface / text / border tones, a type scale, radius and
shadow tokens, and `prefers-color-scheme: dark`
overrides — and `apps/web/components/ui.tsx` the shared
primitives: `Button` (primary / ghost / danger, sizes,
loading), `Badge`, `SectionHeader`, `Skeleton` +
product/rail skeletons, `EmptyState`, the horizontal
scroll-snap `Rail`, and a live-ticking `Countdown`.
`ProductCard` v2 uses the Wave 51 hover image swap, a
2-line title clamp, rating + sold-count meta, promotion
and flash badges, and a price block that shows the sale
price with the base struck through and a −% badge.

**Merchandising data (migrations 0044–0047):**
`home_banners` — title, subtitle, link, ONE image source
(an upload through the Wave 51 pipeline under a
`site/banners/` storage prefix, or a hotlink; exactly
one, like product images), position, an optional
visibility window, an active flag. `collections` +
`collection_products` — curated sets whose membership
rows carry the display position (cascade on both
sides). `flash_sales` + `flash_sale_items` — a sale
window plus per-product sale price and optional stock
cap. `merch_events` — an append-only engagement counter
log (banner clicks, collection views; no user or
session is stored). Admin API (all behind
`require_admin`; API keys stay refused on the whole
`/v1/admin/*` prefix): full CRUD for banners including
`POST /v1/admin/banners/:id/image` (multipart, through
the media pipeline) and its `DELETE`; collections CRUD
plus `PUT /v1/admin/collections/:id/products` replacing
the ordered membership; flash sales CRUD plus
`PUT /v1/admin/flash-sales/:id/items` replacing items
with per-item price + cap; `GET /v1/admin/home-preview?at=`
rendering the home feed as of any instant (the feed
builder takes `now` as a parameter from the start, so
previewing a scheduled campaign is the same code path);
and merchandising totals inside `GET /v1/admin/metrics`.
Domain rules, pure and unit-tested: banner visibility =
`is_active AND (starts_at IS NULL OR starts_at <= now)
AND (ends_at IS NULL OR now < ends_at)`; banner links
must be a site-relative path or an http(s) URL;
collection slugs reuse the store-slug rule; a flash
window must end after it starts; **a sale price must be
> 0 and below the product's current base price**,
validated against the product row at write time; and
**at most one ACTIVE flash sale may cover any instant**
— overlapping active windows are refused bilingually
(409) inside the create/update transaction, since no
plain CHECK can express it; back-to-back sales that
merely touch at a boundary are allowed.

**Flash price resolution — the money rule.** ONE
service function (`active_flash_price` in
`crates/api/src/merch.rs`, plus a batch variant for
lists) resolves the price, and every surface honours
it: product views gain `flash_price_poisha` +
`flash_ends_at` while a sale resolves; the cart view
prices lines at the sale price (a variant's price delta
still composes on top of the sale base, via the same
`variant_final_price` rule); and checkout resolves the
price **inside its own transaction, under the
product-row locks, on the transaction's own
`SELECT now()` clock** — so the order-item snapshot
carries the sale price, and goods subtotal, commission,
the seller payout snapshot, coupon allocation bases and
COD collection all derive from the price actually paid,
exactly as they already derived from the snapshot. A
sale that ends mid-checkout therefore resolves at the
price the placement transaction sees, never a torn mix.
**Cap accounting (the deliberate, auditable
definition):** units sold at the sale price =
`SUM(quantity)` over order items that sit in
non-cancelled sub-orders, whose order was placed inside
the sale window, and whose snapshot unit price equals
the item's sale price plus the variant's price delta
(0 for variant-less lines). The admin item editor shows
exactly this aggregate as sold / remaining; when it
reaches the cap, the price reverts to base everywhere,
immediately — the resolution and the accounting are the
same query family, so they cannot drift apart. The cap
is measured on units already sold at the moment an order
is placed, so a single multi-unit order placed just
under the cap prices all of its units at the sale price
(the count can land above the cap; the remaining count
clamps at 0) and only the next buyer sees base price —
the Wave 52 tests pin this behaviour exactly.

**Sold counts (real numbers only).** Every product view
gains `sold_count` = `SUM(quantity)` over order items
whose sub-order status is `delivered` — the platform's
one definition of a completed sale — computed as a
single aggregate inside the shared product SELECT, so
list, detail, search, storefront, related, also-bought,
wishlist, recommendations, seller export and the home
rails all agree. A product with no delivered sales
shows 0, and the card hides the sold line at 0 rather
than printing a hollow "0 sold" (a design call, stated
here).

**`GET /v1/home`** returns the whole merchandised home
in one response: the visible banners; category tiles
(product counts, the tile image being the primary image
of the category's best-selling product — newest with an
image when nothing has sold); the live flash sale with
per-item sale prices and cap progress; up to 4
collection sections with their first products; the new
arrivals, top rated, best sellers (only products with
at least one delivered sale — the rail hides rather
than padding with unsold products) and promoted rails;
and featured stores (stores on a verified KYC tier,
with branding and product counts). Section sizes are
named constants in `merch.rs`. The home page renders
the feed as a hero carousel (auto-advance, dots,
arrows, touch swipe; a click beacons a `banner_click`
event, fire-and-forget), category tiles, the flash
section with a live countdown and cap progress bars,
collection sections, rails and the featured-stores
strip — skeleton placeholders while loading, empty
sections hidden, and the plain catalogue grid remains
below as content and as the fallback if the feed fails.
Public collection landing pages live at
`/collections/<slug>` — products in membership order, a
`collection_view` beacon per visit, and an honest 404
for unpublished collections. Admin web pages:
`/admin/banners` (form + image upload per row),
`/admin/collections` (CRUD + the ordered product
picker), `/admin/flash-sales` (CRUD + the item editor
with taka price entry and the live sold/remaining
accounting), `/admin/home-preview` (the feed as of any
chosen instant, summarised section by section for
verification), nav links in the admin shell, and the
merchandising totals on the admin overview. The wave's
surfaces extend past the home page: the product detail
page prices through the flash rule too (sale banner with
the countdown, struck base, −% badge, sold line, and
variant prices composed over the SALE base — the API's
per-variant finals are base-derived, so the page composes
them itself through the same add-the-delta rule the cart
uses, and the JSON-LD offer price is the price actually
charged); a site-wide flash strip rides above every
marketplace page while a sale is live (the marketplace
route group's layout — never the back offices);
storefronts gain on-sale / best-sellers / new-arrivals
sections scoped to the store above their full grid; the
search page renders results as design-system cards in a
grid with skeleton and empty states, its cards carrying
the same sold/flash fields; and sellers get their own
seat on the event — `GET /v1/seller/flash-sale` and
`/seller/flash-sale` show a seller exactly which of their
products the live sale carries, at what price, and how
much of each cap is already sold (read-only: flash sales
are a platform event, and the seller's levers stay their
base price and their stock).

**Honest limits.** The feed is identical for every
visitor — no per-user personalisation (the per-visitor
discovery strips from Wave 24 still render below it).
Category tiles derive from the free-text product
category strings: there is still no categories table,
so a category rename is a data edit, not a migration.
Banner PATCH is merge-style, like the platform's other
PATCH endpoints: provided fields override, absent fields
keep their stored values, and the merged row is
re-validated whole (a patch moving only `ends_at` still
cannot invert the window). Text fields clear by sending
an empty string (validation normalises blank to NULL);
a stored window bound cannot be cleared back to NULL
through the patch. The image source has its own rule:
uploading replaces a hotlink and setting a hotlink
deletes the stored upload, so the one-image-source rule
holds from every direction. The engagement beacon is
best-effort by design: a lost count is acceptable, a
blocked click is not, so counts inform and never gate
anything. Cap accounting keys on the snapshot price by
design — if a seller edits a product's base price
mid-sale so it collides with the sale price, units sold
at that price inside the window count toward the cap;
the sale price itself is never re-validated against a
later base-price edit (the write-time rule is the
guarantee).

Wave 53 (discovery & facets — the Shopify-parity program
continues):

**Browsing becomes a real shopping surface: faceted,
priced by the effective price everywhere, and honest
about what it counts.** `GET /v1/search` is reworked
into a faceted browse. Alongside the Wave 15 trigram
text match it takes repeatable `category` and `store`
parameters (OR within a group; the comma form works
too, and the legacy `category=` / `store_slug=`
spellings keep working), `min_price_poisha` /
`max_price_poisha`, `min_rating` (0..=5), `in_stock` and
`on_sale` flags, the sorts (`relevance`, `newest`,
`price_asc`, `price_desc`, `rating`, and new `sold` —
best sellers first by the delivered-units definition),
and `page` + `limit` (default 24, max 60; the legacy
`per_page` spelling still works and both names are
echoed). The response keeps the shared product view and
gains `facets`. **The facet convention** (the standard
one, stated precisely): `total` and `items` apply EVERY
filter; each facet group counts the set filtered by
every OTHER group — the categories facet does not
narrow when a category is picked (it is how the shopper
widens), while the stores facet does; the price facet
is the span over the set filtered by everything except
the price bounds; the ratings facet counts products at
or above the 4 / 3 / 2 thresholds with unrated products
counting as 0 stars. **The effective-price rule:**
every price in discovery — the price filter, the price
sorts, the price facet, the suggestion price — is the
price the buyer would pay: the live flash price when
one resolves, else the base. The SQL side is a lateral
join (`FLASH_PRICE_JOIN` in merch.rs) that mirrors the
Rust resolution exactly: the same live-sale pick
(active, half-open window, earliest-ending first), the
same cap rule (the FLASH_UNITS_SOLD_SQL aggregate in
per-product scalar form, variant deltas included), so
the band that admits a product and the
`flash_price_poisha` on its card can never disagree;
the wave's tests pin both directions (a flash price
pulls a product INTO a band its base is outside of, and
pushes it OUT of a band its base is inside of). All
filter validation lives in ONE domain function
(`validate_browse_filters`) shared by the handler and
the saved-searches service, with bilingual 400s.
`GET /v1/search/suggest` v2: product suggestions gain
`price_poisha` (effective), `image_url` and `category`
— additive, the title-dedup behaviour is unchanged —
and the payload gains a `categories` list (the labels
the matching products live in, with counts) for the
dropdown's "in category" rows. `GET /v1/categories`
serves the full category list from the home feed's
aggregate, extracted into one shared function both
callers use. `GET /v1/products` gains the same `sort`
(its envelope already existed); existing consumers are
untouched. The price facet also carries a `histogram`:
the span split into bands by a pure domain planner,
counted on effective prices over the same exclude-price
set — the filter panel renders it as clickable bars
(clicking the active band again clears it).
`GET /v1/search/popular` serves the most-run search
texts of the trailing window from the analytics log —
public aggregate data (texts + counts; the log holds
nothing else about anyone) for the search box's popular
list.

**Web.** A new `/browse` page renders the shared
`BrowseResults` component — desktop filter sidebar and
mobile drawer over ONE `FilterPanel` (category and
store checkboxes with facet counts, taka price inputs,
rating radios, in-stock / on-sale toggles), the result
count line, active-filter chips, grid AND list views
(the list row prices by the same honesty rules as the
card), windowed pagination, loading skeletons, an empty
state whose reset is the "সব ফিল্টার মুছে ফেলুন"
button, and a JSON-LD ItemList of the loaded page. All
state lives in the URL (`lib/browse.ts` is the single
translator), so browses are shareable and the category
tiles (home + /categories, which now reads
`/v1/categories`) and the header search deep-link into
them. `/search` renders the same component — one
discovery path; storefront within-store searches still
land there scoped. The header SearchBar's dropdown
renders suggest v2 (thumbnail, category, effective
price) with full keyboard navigation and click-outside
close; submit still works without it. The product page
gains the "একই ক্যাটাগরিতে আরও" rail driven by the
faceted endpoint (same category, sort `sold`, self
excluded) and the recently-viewed rail. **Recently
viewed, privacy note:** the history is DEVICE-LOCAL —
localStorage only, capped at 12, now carrying a minimal
card snapshot (title, store, prices, image) so the rail
renders without a fetch; it is never sent to the
server, and the rail's "মুছে ফেলুন — Clear" button (and
clearing site data) forgets it. Wave 24's id-only
entries migrate on read.

The same discovery path serves two more surfaces: a
store's **shop page** (`/{store}/shop`) — BrowseResults
pinned to the store by the route, its categories and
price span scoped by the same facets — and **category
landings** (`/categories/{category}`), the browse pinned
to one category with per-category metadata. The home
rails, the flash section and the storefront sections
all deep-link into these surfaces with their matching
sort or filter. The browse toolbar adds a page-size
control (24 / 48 / 60) and a copy-link share button —
the URL is the state, so sharing is copying.

**Extras that shipped with the wave.** (a) *Saved
searches* for signed-in buyers: migration 0048, CRUD at
`/v1/buyer/saved-searches` (owner-scoped, a per-user
cap of 20), storing the validator's canonical filter
form (lists deduplicated AND sorted, so equivalent
spellings converge — the round trip save → list →
render → browse is test-pinned); the `/saved-searches`
page lists them with a plain-words summary, one-click
apply-to-browse and delete, and the browse toolbar
carries the save action. (b) *Search analytics-lite*:
migration 0049's append-only `search_queries` log —
query text, result count and a hash of the canonical
filters, and NO user id or any other identifier, by
design — read by `GET /v1/admin/search-analytics` (top
searches with mean result counts + zero-result
searches over a window the caller may set, 1..=90 days,
default 30) and shown as a card on the admin overview
plus a full `/admin/search-analytics` page. Migration
0050 adds the canonical filters as JSON beside the
hash, so the read also reports the top filter
*combinations* in readable form (rows logged before
0050 carry no shape and are excluded from that list,
never guessed at). The log write is best-effort: a
failed insert never fails the search. (c) *Recent
searches* in the search box: the visitor's own
submitted queries, device-local like the viewed
history (localStorage, capped at 8, clearable from the
dropdown) — never sent to the server.

**Honest limits.** Categories are still the products'
free-text labels: the facet groups by the raw label
while the filter matches case-insensitively, so two
labels differing only in case count as two facet rows
(the category-vocabulary cleanup is a later wave's
work). Search is still the Wave 15 trigram match —
there is no full-text engine, no typo tolerance beyond
the similarity threshold, and no personalised ranking:
`relevance` is text similarity plus the Wave 26
promoted-first rule under the default sorts only.
Analytics counts every browse the API serves (including
empty filter-only browses in the total; only text
searches appear in the text lists), and the
recently-viewed snapshot is the price as it was when
viewed — the product page it links to always shows the
live price.

Wave 8 (reviews, ratings and seller replies):

| Method | Path | What it does |
| --- | --- | --- |
| POST | `/v1/products/{id}/reviews` | Bearer. Review a product you RECEIVED (see the eligibility rule); rating 1–5, body optional (max 1000 chars). 201 with the review; 400 invalid input, 403 not eligible, 409 already reviewed |
| GET | `/v1/products/{id}/reviews?page=&limit=` | Public review list, newest first, paginated like every other list; each review carries its seller reply when one exists |
| PATCH | `/v1/reviews/{id}` | Bearer, author only (anyone else: 404). Edit rating and/or body |
| DELETE | `/v1/reviews/{id}` | Bearer, author only (anyone else: 404). 204; the reply goes with it |
| POST | `/v1/seller/reviews/{id}/reply` | Bearer, owner of the product's store only (anyone else: 404). One reply per review (a second is a 409); body required, max 500 chars |
| PATCH | `/v1/seller/reviews/{id}/reply` | Bearer, same ownership rule. Edit the store's reply |

**Eligibility rule:** a buyer may review a product only when one of
their orders contains it in a sub-order whose status is
`delivered` — shipped is not enough, and a returned sub-order does
not qualify. The single query behind the rule
(`find_delivered_order_for_review`) returns the newest qualifying
order id, which is stored on the review as its proof; ineligible
creates are a 403 with the message "You can review only products
you received".

**One review per buyer per product** (database UNIQUE on
`(product_id, buyer_id)`, migration 0011): a second create is a
409; changing your mind goes through PATCH on the same row.
Deleting a review deletes its seller reply too (foreign-key
cascade).

**Buyer masking:** public review views never expose a buyer's full
phone number. The display string is the buyer's name when they
have one set, otherwise their masked phone — first three and last
three digits of the local form, middle hidden (`017****678`), via
the pure domain function `mask_phone`.

**Rating summary source:** the product view's `rating_avg` (one
decimal, null when unreviewed) and `rating_count` are computed
LIVE on every product read by one aggregate — a lateral join in
the shared product SELECT in `crates/api/src/products.rs`
(`PRODUCT_RATING_JOIN`) — so the marketplace list, the storefront
list and the product detail always agree. There is NO cached
counter yet; if read volume ever makes the aggregate expensive,
the fix is a maintained summary column updated by the review
service functions, not a second formula.

**Seller reply rules:** only the user who owns the store selling
the reviewed product may reply (other sellers get a 404, the same
as a missing review — a review's existence is not leaked to
competitors). One reply per review; the reply records its store,
and editing it goes through PATCH. On the web side, replies are
written from the product detail page itself when the signed-in
user owns the product's store — deliberately no separate seller
reviews page, so a reply is always written next to the review it
answers.

Wave 9 (seller dashboard, earnings and payout requests):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/seller/dashboard` | Bearer, store required (no store: 404). The seller home in one response: store, product count, sub-order counts per status, earnings, and the 5 most recent sub-orders (the shared seller list view) |
| GET | `/v1/seller/balance` | Bearer. The earnings summary alone: earned / paid out / pending / available |
| GET | `/v1/seller/payout-account` | Bearer. Your payout account, or `null` when none is saved |
| PUT | `/v1/seller/payout-account` | Bearer. Create or replace the payout account (domain-validated; invalid input is a 400) |
| POST | `/v1/seller/payouts` | Bearer. Request a payout of `amount_poisha` against the available balance (201; the account fields are snapshotted onto the payout) |
| GET | `/v1/seller/payouts?page=&limit=` | Bearer. Your store's payout requests, newest first |

**Earning rule:** a store earns only from sub-orders whose status
is `delivered`, and what it earns per sub-order is the goods
subtotal minus the commission snapshot taken at checkout
(`goods_subtotal_poisha − commission_poisha` — the same number
checkout stores as `seller_payout_poisha`). The delivery charge
share is courier/platform money and NEVER earns. A sub-order whose
shipment is only `partial_delivered` is not `delivered`, so it
earns nothing until it is fully delivered — deliberately
conservative: money is counted only when it is certainly the
seller's. One aggregate (`seller_earnings` in
`crates/api/src/payouts.rs`) computes earned / paid out / pending;
the dashboard, the balance endpoint and payout creation all read
it, so the numbers cannot disagree.

**Balance formula:** available = earned − paid out − pending
− charges, where paid out sums payouts in `approved` (committed)
or `paid` (gone), pending sums payouts still `requested`, and
charges are the seller's `seller_charges` (promotion and
subscription charges, Wave 26/34 — keyed to the seller as a
USER, so they subtract from every store the seller owns; this
line was missing from this section's formula until the admin
payouts remake audit). The pure
domain function `compute_available_balance` returns the raw
arithmetic; the API clamps the reported value at zero (with
consistent inputs it can never be negative, because a payout may
only be requested against the available balance).

**Minimum payout:** 500 taka (`MIN_PAYOUT_POISHA = 50_000` in the
domain crate) — a platform rule constant, enforced by
`validate_payout_amount` together with the balance fit.

**Payout lifecycle — plainly:** in this wave sellers can only
CREATE requests; a payout stays `requested` until the platform
acts on it. Approval (`requested → approved`), rejection
(`requested → rejected`) and payment (`approved → paid`) are
platform operations whose endpoints land with the ADMIN wave; the
transition table already exists as the pure domain function
`can_transition_payout`, so the rule lives in exactly one place
from the start.

**Account validation rules** (pure domain
`validate_payout_account`): the method must be `bkash`, `nagad`
or `bank`. For bKash/Nagad the account number must be a valid
Bangladeshi mobile number (the wallet account is the phone
number; local and `+880…` forms both accepted). For bank, the
account holder name must be non-empty and the account number must
be 6–24 ASCII letters or digits. One account per store; saving
again replaces it, and existing payout rows keep the snapshot
they were created with.

**Scoping:** the payout store is the seller's earliest-created
store. Counts and recent orders cover the seller's whole selling
activity (the same scope as the seller orders page), while
earnings and payouts are strictly per payout store.

Wave 10 (platform admin):

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/admin/overview` | Bearer, admin. Platform metrics: user/store/product/order counts, GMV, commission earned, pending payouts (count + amount), pending online payments |
| GET | `/v1/admin/stores?q=&status=&tier=&sort=&page=&limit=` | Bearer, admin. The store queue: MASKED owner phone, tier + effective plan, decision aggregates (products, followers, orders, delivered GMV, pending-payout exposure, open returns), suspension note + stamp, and a whole-queue summary; `q` matches name or slug (LIKE-escaped); unknown filter/sort values: 400 |
| GET | `/v1/admin/stores/{id}` | Bearer, admin. One store in full: the queue row plus subscription view, canonical earnings, returns exposure, verification summary, the owner's stores (with statuses) and the store's audit history; owner phone stays masked |
| PATCH | `/v1/admin/stores/{id}/status` | Bearer, admin. Set a store `active` or `suspended` (anything else: 400). Suspending REQUIRES a `note` (missing/blank: 400) — stored on the store, audited, seller-visible, notified. A transition to the current status is a 409 and writes nothing. Returns the store view |
| GET | `/v1/admin/orders?status=&page=&limit=` | Bearer, admin. Every order (the shared buyer order view), newest first, optional order-status filter |
| GET | `/v1/admin/payouts?status=&q=&sort=&page=&limit=` | Bearer, admin. The payout queue: store name, MASKED owner phone and account number, per-row coverage for in-flight payouts, a possible-duplicate flag, and a whole-queue `summary` by status. `status` is validated (unknown: 400); `q` searches store / phone / account and idempotency keys; `sort` is `newest` (default) / `oldest` / `amount_desc` / `amount_asc`. Full identifiers live only in the detail below (admin payouts remake) |
| GET | `/v1/admin/payouts/{id}` | Bearer, admin. The payout detail: the queue row plus the FULL destination and owner phone, the store's earnings, returns exposure, recent seller charges, verification state, the store's current account beside the payout's snapshot (with a divergence flag), store payout history, duplicate candidates, and the payout's audit entries (admin payouts remake) |
| POST | `/v1/admin/payouts/{id}/approve` | Bearer, admin. `requested → approved`; illegal moves are a 409. Coverage, suspension and the KYC gate are re-checked inside the transition (see the remake note) |
| POST | `/v1/admin/payouts/{id}/reject` | Bearer, admin. `requested → rejected` and `approved → rejected`; the JSON `note` (the reason) is REQUIRED — missing or blank is a 400. The seller sees it and is notified |
| POST | `/v1/admin/payouts/{id}/mark-paid` | Bearer, admin. `approved → paid`; the JSON `payment_reference` is REQUIRED (4–128 printable ASCII) and is recorded on the payout and sent to the seller |
| GET | `/v1/admin/users?q=&page=&limit=` | Bearer, admin. Every user with role, admin flag and store count; `q` matches a phone substring |
| PATCH | `/v1/admin/users/{id}` | Bearer, admin. Set or clear ANOTHER user's `is_admin` (your own flag: 400) |
| GET | `/v1/admin/settings` | Bearer, admin. The EFFECTIVE settings: `commission_bps`, `min_payout_poisha`, `platform_name` |
| PUT | `/v1/admin/settings` | Bearer, admin. Store setting overrides (validated: commission 0–3000 bps, minimum payout 0–10,000,000 poisha) |

Every admin handler starts from the ONE guard
(`require_admin` in `crates/api/src/admin.rs`), which re-reads
the caller's user row on every request — a revoked flag stops
working immediately, not when the JWT expires. Non-admins get a
403 on all of the above.

**Admin model:** admin is a boolean flag on the user row
(`users.is_admin`, migration 0013), NOT a role — an admin keeps
their buyer/seller role, and the flag alone unlocks the admin
endpoints. There is deliberately **no endpoint that creates the
first admin** (an endpoint that grants admin is an attack
target). Promote by SQL instead; the account must have logged
in at least once so its user row exists:

```sql
UPDATE users SET is_admin = true WHERE phone = '+8801XXXXXXXXX';
```

After that, admins promote and demote OTHER users from the
admin panel. An admin can never change their OWN flag through
the API (400, "you cannot change your own admin flag") — the
platform cannot lock itself out of its last admin by accident.

**Suspension semantics:** suspending a store hides its products
from the marketplace listing, the storefront listing and the
public product detail (404 for everyone but the owner — the
enforcement lives in the shared product queries, so no client
can route around it), and checkout refuses any cart containing
a suspended store's product with a 409 naming the product. The
seller can still sign in, see and manage their catalogue, and
keep their payout history; existing orders are untouched.
Reactivation restores visibility and checkout exactly — nothing
is ever deleted.

**Payout processing (lifecycle completed):** the three
processing endpoints are thin handlers over ONE service
function driven by the Wave 9 domain transition table
(`requested → approved → paid`, `requested → rejected`; the
admin payouts remake added `approved → rejected`). A legal
move stamps `processed_at` — the MOST RECENT transition
time; the remake's per-stage stamps (`approved_at` /
`paid_at` / `rejected_at`, migration 0072) are the ones
that are never overwritten. Rejection stores the admin's
note as the reason (required since the remake; it is on
the seller's payout record and in the `payout_rejected`
notice). Rejecting a request releases its
amount back into the seller's available balance (it stops
counting against the balance — for a `requested` payout it
leaves `pending`, for an `approved` one it leaves the
paid-out bucket; nothing was ever sent). Approving
keeps the amount committed (it counts as paid out from
approval on, so it can never be requested twice).

**Admin payouts remake (2026-11-28, the second surface of
the admin remake program; ~66 analysis agents):** what
changed, beyond the endpoint table above —

- **Mark-paid coverage re-check.** The Wave 39 re-check
  ran only at approve; earnings can fall after approval
  (a return is received) and `paid` is final, so the same
  unclamped-balance check now runs at mark-paid too. The
  409 carries the earnings picture as structured
  `details` (`kind: "payout_coverage"`, the shortfall, and
  the four aggregate components, computed inside the
  transition transaction).
- **Decision gates, in order:** input validation (400) →
  existence (404) → store-then-payout row locks → legality
  (409, bilingual, naming the current status) → suspension
  freeze (409: a suspended store's payouts cannot be
  approved or marked paid; reject stays allowed) → KYC
  gate re-check at approve only (400, when the gate is on
  and the store is unverified NOW) → coverage (409).
- **Decision metadata** (migration 0072): per-stage
  timestamps and actor ids on the payout row, plus the
  `payment_reference` (own column, never the note).
  Backfilled from `processed_at` and the Wave 28 audit log
  (one pivot per action); pre-audit-log actors stay NULL —
  unknown is recorded as unknown. Migration 0073 adds the
  queue and audit-target indexes.
- **Seller notified of every decision:** `payout_approved`
  (never claims a send), `payout_rejected` (carries the
  reason), `payout_paid` (carries the reference when one
  was recorded) — all through the one in-transaction
  outbox funnel.
- **The web queue** is rebuilt to the overview's standard:
  age-ordered decision cards with coverage per row, the
  whole-queue summary (its `requested` bucket is the same
  count the overview's `pending_payouts` links here with),
  search/sort/pagination, confirm dialogs that restate
  amount + destination (no more `window.prompt` — whose
  Cancel used to fire the rejection anyway), an in-card
  detail with the earnings behind the amount, and a
  printable payment instruction/record.
- **Honest limits:** the detail is an advisory snapshot —
  the transition's in-transaction re-checks remain the
  authority; a return-received between the last check and
  the external send is still possible (returns take no
  payout lock), which is why the detail shows the returns
  exposure instead of hiding it; there are no bulk actions
  (per-payout decisions only) and no paid-payouts CSV
  export yet (month-end reconciliation is a named
  follow-up); the seller payouts page does not yet render
  the rejection reason, the recorded reference, or the
  per-stage stamps (the API record and the notices carry
  them — a seller-surface follow-up).

**Admin verifications remake (2026-11-28, the third surface
of the admin remake program; 94 analysis agents):** the KYC
review queue was rebuilt around one fact the audit had
already flagged (HI-82): the full-NID reveal was NOT
audited, despite the page claiming it was. What changed —

- **The reveal is now an audited POST.** `POST
  /v1/admin/verifications/:store_id/full` takes a mandatory
  `reason` (domain-validated: meaningful length, and it may
  not contain a long digit run — a pasted NID in the audit
  detail would defeat the design), locks the application
  row, writes `verification.view_full_nid` (detail:
  `{reason, nid_last4, status}`) and commits BEFORE the
  number is served, with `Cache-Control: no-store`. The web
  reveal lives only inside a dialog, beside the document,
  for 30 seconds, and is then deleted from state.
- **Decisions are locked and version-bound** (migration
  0074): store-then-application `FOR UPDATE`, the transition
  re-checked under the lock, approval on a suspended store
  is a 409, and approve/reject must present the
  `expected_version` they reviewed — a seller edit after the
  review makes the decision a 409 instead of landing on
  unseen documents. Submissions take the same locks: a
  payload-changing pending edit bumps the version but keeps
  the original `submitted_at`; an identical resubmit is a
  no-op. Approving with no document image is a 400 unless
  the reviewer passes the explicit, audited
  `no_document_override`.
- **History exists now.** `store_verification_events`
  records submissions only (decisions and reveals live in
  the audit log — one canonical record, not two); events
  carry `nid_last4`, never the number and never a hash of
  it (a hash of a 10/13/17-digit number is a brute-forceable
  second copy). Masked read paths no longer SELECT the full
  number at all — they rebuild the mask from `nid_last4` +
  length, and 0074 enforces `nid_last4 = right(nid_number,
  4)` in the database.
- **Sellers hear the outcome.** `verification_approved` /
  `verification_rejected` notices (owner's account phone,
  inside the decision transaction): approval names the
  granted tier, rejection carries the reviewer's note
  (truncated in the SMS body; the full note stands on the
  seller's page). The wording claims manual platform review
  only — no government NID check exists.
- **KYC is owner-only.** Submission AND reading moved from
  the shared store-settings action to a new
  `manage_verification` domain action (owners only — a
  manager can no longer file or open the owner's identity
  file), the seller now supplies the applicant name as on
  the NID, and the verification PUT joined the API-key
  JWT-only denylist. Bengali-digit NIDs are transliterated
  and stored as canonical ASCII (a deliberate reversal of
  Wave 21's reject-never-guess rule: the map is a
  deterministic 1:1 transliteration and the stored form is
  always ASCII).
- **The queue is decision-ready:** envelope
  `{items, total, page, limit, summary}` (whole-queue
  summary, payouts rule), search over store/applicant/phone
  (NEVER the NID or license — no probing oracle), a detail
  endpoint with store context (canonical aggregates),
  different-owner duplicate flags (counts only), prior
  rejections, and the owner's other stores. The unscoped
  owner export gained a masked per-store `verification`
  section; the manager-scoped export never carries it.
- **Honest limits:** review remains MANUAL (restated —
  nothing here authenticates an NID); there is still NO
  tier revoke/demote action (an owner policy decision is
  owed — a mistaken approval stands until then); the full
  NID of a REJECTED application is retained indefinitely
  (a rejected-NID scrub window is a second owed ruling;
  approved NIDs are the evidence behind a live badge and
  are not scrub candidates); `nid_number` is not encrypted
  at rest (no KMS exists in the stack — database backups
  are the compensating control and a named launch concern);
  prior-rejection counts and decision history reach back
  only to the Wave 28 audit log; the seller's document is
  still a seller-hosted hotlink, so the reviewed evidence
  can change after approval (the URL at decision time is
  preserved in the submission events and audit trail).

**Admin stores remake (2026-11-28, the fourth surface of the
admin remake program; 25 analysis agents):** the store
moderation queue was rebuilt around the decision it exists
for — suspending a business — after the analysis found the
surface both leaking and blind:

- **Privacy fix:** the admin store list carried the owner's
  FULL phone (the same leak class the payouts remake removed
  from its list). The list — and the new detail endpoint —
  now carry `owner_phone_masked` only; no admin store
  response contains the full number, because no store
  decision is executed by phone (a deliberate divergence
  from the payout detail, which sends money to its number).
- **Suspension requires a reason.** `PATCH
  /v1/admin/stores/{id}/status` with `{"status":
  "suspended"}` now requires a `note`
  (`domain::validate_suspension_note`, ≤500 chars; missing
  or blank is a 400 that writes nothing). Migration 0075
  adds `stores.suspension_note`, `status_changed_at` and
  `status_changed_by_user_id` (the stamp backfilled from the
  audit log's latest pivot only where it still matches the
  current status; the note is never backfilled). The note
  is stored on the store, written into the audit detail,
  shown to the seller on their dashboard banner, and quoted
  in the new `store_suspended` notice (the `store_reactivated`
  kind announces a restore); both kinds are enqueued inside
  the transition's own transaction through
  `notices::enqueue_store_status_notice`. Reactivation
  clears the note and takes none.
- **No-op transitions are a 409.** The old code updated
  unconditionally, so a repeated suspend appended a phantom
  `store.suspend` audit row every time. The transition now
  locks the store row (`FOR UPDATE`), and a request for the
  status the store already holds is refused with a bilingual
  409 naming the current status — no write, no audit row, no
  notice, the stored note untouched (security audit LO-93
  prescribes exactly this).
- **Decision-ready queue.** The list gains status and
  verified-tier filters (fail-closed 400s), sorts
  (`newest`/`oldest`/`name_asc`/`gmv_desc`/`followers_desc`,
  domain-validated), LIKE-escaped search (the old query
  interpolated the raw text — `%` matched everything), a
  whole-queue `summary` (counts by status and tier), and
  per-row aggregates: effective subscription plan (live-plan
  rule), product/follower/order counts, delivered GMV
  (goods − discount + delivery over delivered sub-orders),
  pending-payout count + amount (`requested` only), and open
  returns. The count query now runs the same join as the
  list (it previously did not, so totals could exceed the
  items shown). Migration 0076 adds the queue index. New
  `GET /v1/admin/stores/{id}` composes the canonical readers
  (subscription view, `seller_earnings_on`, the payout
  detail's returns-exposure block, the verification summary,
  the audit log) plus an owner rollup that — unlike the
  verification detail's — includes each store's status.
- **Blast-radius holes closed.** Suspension already sealed
  discovery and checkout; three paths still leaked. Cart add
  read only the product's status, so a suspended store's
  product could be carted — it now refuses with the cart
  module's bilingual 400. Reorder gains a dedicated domain
  skip reason (`store_suspended`, checked in
  `reorder_decision`) instead of misreporting the line, and
  the buy-again rail filters suspended stores. All three
  follower-notice funnels (new product, new post, flash
  sale) ignored the store's status, pinging followers about
  pages that publicly 404 — they now enqueue nothing while
  the store is suspended (publishing itself still works;
  nothing retro-fires on reactivation).
- **Web:** `/admin/stores` rebuilt to the remake standard
  (summary chips, status/tier filters, sort, debounced
  search, pagination, expandable detail, one Dialog —
  danger variant for suspend with the required note and the
  honest consequence list, including that subscription
  billing CONTINUES and payouts are frozen, not cancelled);
  the followers page brought to the same standard
  (unchanged API — it was already masked and paginated);
  a suspension banner on the seller dashboard; Bangla
  labels for the two new notice kinds (plus the previously
  unlabelled `store_new_post` / `gift_card_expiring`).
- **Honest limits:** suspension does NOT pause subscription
  billing (the worker charges suspended stores identically —
  documented asymmetry, unchanged); the public store PROFILE
  of a suspended store still resolves (identity and stats
  only — its products are already hidden everywhere; making
  it 404 is a named follow-up, not done here); the
  recently-viewed rail is a client-side snapshot and does
  not revalidate store status; there is deliberately no
  bulk suspend/restore (one click would freeze many stores'
  payouts at once, and no workload evidence justifies it)
  and no store deletion anywhere (suspension is the kill
  switch by design); stores suspended before this remake
  carry no note and possibly no stamp — the UI says so
  instead of inventing either.

**Admin orders remake (2026-11-28, the fifth surface of
the admin remake program; 96 analysis agents):** the
orders surface was a read-only endpoint with no page —
`GET /v1/admin/orders` returned the buyer's FULL order
view per row (full delivery name, phone, address and
note, every sub-order's items, timeline and steps),
assembled by running the shared order loader once per
row (~350 queries for a 25-row page), and the web client
for it had no caller. The remake keeps the surface
strictly observational — order and sub-order transitions
belong to the buyer/seller flows, refunds to the returns
surface, and no admin order-mutating power is added:

- **Privacy fix:** the list is now a purpose-built triage
  row (`AdminOrderRow`): snapshot money under the buyer
  view's field names, per-store sub-order chips (store,
  pipeline status, goods), and the delivery phone MASKED
  (`delivery_phone_masked`); the address, note and items
  are absent from the list entirely. New `GET
  /v1/admin/orders/{id}` returns the buyer's own view
  (the full delivery block lives there only — the
  payouts detail precedent) plus `buyer_phone_masked`
  (the account phone, masked even in the detail) and
  `cod_amount_due_poisha` on the receipt's own formula
  (grand − advance paid, COD only). The read is not
  reason-gated: a delivery phone is operational data the
  seller already sees, not a national credential like
  the verification NID (whose reveal stays gated).
- **Set-based reads:** one page query, one grouped
  sub-order query for the page's ids, one count and the
  summary — the per-row loader loop is gone from this
  path (the buyer loader itself is untouched). Migration
  0077 indexes the queue's walks (`created_at, id` and
  `payment_status, created_at, id`); the orders table
  previously had no secondary index on these paths.
- **Queue filters, all fail-closed 400s:** parent
  `status` (placed/cancelled — the parent status is
  write-once), `payment_status` (the seven-value
  vocabulary), `sub_status` (orders having a sub-order
  in that pipeline status), `store_id`, and
  `advance_state` — `pending` / `unfulfilled`, the
  overview metrics' two advance populations, so the
  overview's Needs-attention rows now deep-link into the
  exact queue they count. Sorts are domain-validated
  (`newest`/`oldest`/`amount_desc`/`amount_asc`).
  Search follows the phone discipline: a BD phone in any
  typed form (Bengali digits transliterated) matches the
  delivery or account phone EXACTLY — phones are never
  substring-searched; an order-id prefix matches by
  prefix; other text matches the delivery name or a
  store name/slug, LIKE-escaped.
- **Whole-queue `summary`:** counts by parent and
  payment status, the advance-pending count + outstanding
  and the CR-15 unfulfilled count + amount, and today's
  order count + GMV — all on the overview metrics' own
  populations and formulas, pinned against the metrics
  by test (`adminordersb`), so the queue header and the
  overview cannot disagree. Note the deliberate
  asymmetry, inherited from the metrics: the
  per-payment-status counts include cancelled orders;
  the advance blocks exclude them.
- **Web:** new `/admin/orders` page to the remake
  standard (summary chips, status/payment/pipeline/
  advance filter chips, debounced search, sorts,
  pagination, expandable detail with the money
  breakdown, per-sub-order timelines via the shared
  `OrderTimeline`, shipment lines, return links, and
  per-store invoices fetched through the ADMIN invoice
  endpoint — the print page's buyer/seller invoice
  calls 403 for admins); an অর্ডার entry in the admin
  nav; the overview's two advance rows deep-link with
  `?advance_state=`.
- **Honest limits:** deliberately no date-range or
  buyer filters (no caller needs them yet), no CSV
  export (an orders export is a full-PII bulk channel;
  the payouts remake cut its CSV on the same reasoning),
  no audit-history block on the detail (no admin action
  targets orders, so it would always be empty), and no
  stale-advance sweeping — stale `advance_pending`
  orders are never expired by the worker; this queue is
  their only visibility, and acting on them stays with
  the buyer/seller flows.

**Admin products remake (2026-11-28, the sixth surface of
the admin remake program; 22 analysis agents):** the
products surface did not exist — there was no
`/admin/products` page and no admin products API, so an
admin could not see the catalogue cross-store at all,
and the only lever on a bad listing was suspending the
seller's WHOLE store. The remake builds the queue, the
detail, and a per-product takedown power:

- **Model — a flag, not a status (migration 0078):**
  `products.admin_hidden_at` (the flag itself),
  `admin_hidden_by_user_id` and `admin_hidden_reason`
  (CHECK 1..=500), plus a consistency CHECK coupling the
  timestamp and the reason. A new status value was
  rejected: it would corrupt the seller-owned lifecycle
  (`draft`/`active`/`archived`) and restore could not
  know the prior state. The flag is orthogonal, so
  restore returns the product to EXACTLY its
  seller-chosen state, and no seller write path (PATCH,
  bulk edit, variant writes) can set or clear it.
  Takedown is allowed on any status and on a suspended
  store's product — deliberately, so it outlives a store
  reactivation. No-op transitions are 409s.
- **Reason + note discipline:** takedown REQUIRES a
  reason (`domain::validate_product_takedown_reason`)
  and restore REQUIRES a note
  (`validate_product_restore_note`) — the restore-note
  requirement consciously diverges from store
  reactivation, which takes none: a takedown is a
  content determination, and the restore note, stored
  in the restore audit detail together with the
  OUTGOING reason and hider (copied before the row is
  cleared), is the only record of why the decision was
  reversed. Both transitions run the remake canon:
  validation → store-then-product row locks → legality
  re-read under the lock → one transaction writes the
  flag, ONE audit row (`product.takedown` /
  `product.restore`, target `product`) and the seller
  notice (`product_taken_down` / `product_restored`
  kinds, the reason copied into the payload at takedown
  time; the restored body claims only the lift, never
  that the product is selling again).
- **Blast radius — every public read path filters
  `admin_hidden_at IS NULL`:** the shared list predicate
  (with the owner exception, so the seller still manages
  the listing and sees the reason on it), the strict
  public-product gate (detail-adjacent reads: wishlist
  add, recommendations bases, Q&A), the product detail
  handler (insiders pass), faceted browse + facets +
  suggestions, home rails / collections / flash block /
  category tiles + cover images / featured-store counts,
  the category tree counts + covers, store public stats,
  follows (counts, newest picker, feed, suggestions),
  recommendations candidates, storefront sections,
  store collections, buy-again, the overview metrics'
  live-catalogue counts, and the per-product review
  list + rating summary (uniform 404) and store review
  list + summary (excluded). The follower new-product
  notice funnel also refuses a taken-down product, so a
  publish-while-hidden can never ping followers.
- **Cart / checkout decision — block, honestly:** cart
  add refuses a taken-down product with a bilingual 400;
  a product carted BEFORE its takedown is refused in
  checkout's locked validation loop with a distinct
  bilingual 400 naming the product (not the
  seller-withdrawal wording — a different act with a
  different remedy). The whole checkout aborts and the
  cart survives, so a restore makes the same cart check
  out. Reorder gains the domain skip reason
  `product_taken_down`; the cart READ deliberately does
  not re-validate (checkout is the validator, the
  platform's standing split).
- **Seller-side integrity:** the seller keeps editing,
  restocking and exporting a taken-down product (their
  records are not the platform's to hide); the seller
  product list shows the takedown banner with the full
  reason. Bulk import refuses a taken-down SKU per-row
  and writes nothing — the export → edit → re-import
  round-trip (and the demo seed, which imports through
  the same path) cannot resurrect or rewrite a removed
  listing. Creating a promotion for a taken-down product
  is refused (its placement could never render).
- **Queue + detail:** `GET /v1/admin/products`
  (fail-closed filters: `status`, `store_id`, `source`,
  `visibility`, `stock_state` over the effective-stock
  CASE, `has_images`; LIKE-escaped search over title /
  Bangla title / store; domain-validated sorts incl.
  `sold_desc` / `rating_desc`; whole-queue summary) and
  `GET /v1/admin/products/{id}` (shared product view +
  store summary + full gallery + ALL variants incl.
  inactive + a sales block — line-total goods over
  non-cancelled sub-orders, labelled as ordered value,
  NOT earnings — + the product's audit history, which is
  real here because takedowns target products). New
  `/admin/products` page to the remake standard, a পণ্য
  nav entry, and the two notification kind labels.
- **Honest limits:** there is NO product report/flag
  signal anywhere in the platform (no report table, no
  endpoint, no view events), so the queue is
  inspection-based by design — no reports inbox is
  fabricated. A running promotion is NOT paused by a
  takedown and never refunded (the store-suspension
  precedent for platform actions: suspension does not
  pause subscription billing either); its placement
  simply stops rendering — the fairness counter-argument
  (prepaid placement burning while platform-hidden) is
  recorded here as open policy, not silently decided.
  Product IMAGES keep serving by URL: order history
  thumbnails are live by design, the media handler has
  no caller identity to gate on, and a gate would leak
  anyway (external hotlinks, immutable caching) — when
  the image itself is the violation, pair the takedown
  with the media moderation surface's audited delete.
  The takedown is per-row: the same listing re-created
  as a new product (manually or under a new Facebook
  post id) is a new row — repeat abuse is a store-level
  decision, and the queue's store filter + the store
  detail are the escalation path. Anonymous catalogue
  reads are cached `public, max-age=30`, so a takedown
  is effective within ~30 seconds, not instantly.
  Granularity is whole-product only (no per-variant
  takedown). The wider pre-existing finding that
  draft/archived products' reviews are publicly readable
  is NOT changed here — this remake seals the
  platform-removal case only.

**Admin shipments remake (2026-11-28, the seventh surface
of the admin remake program; 20 analysis agents):** the
shipments surface did not exist for admins at all — no
`/admin/shipments` page, no admin shipments API — while
the remade overview's Needs-attention queue linked to
`/admin/shipments` (a dead link since the overview
remake; it now carries `?queue=unrefreshed`). The remake
builds the cross-store, cross-courier oversight queue,
the detail, and one action:

- **Two stuck queues, kept apart.** UNREFRESHED = no
  status change in 15 minutes — by construction the
  exact population `/v1/admin/metrics` reports as
  `stale_shipments` (pinned equal in tests). POLL
  OVERDUE = no poll ATTEMPTED in 15 minutes, which
  `updated_at` could never express (unchanged and failed
  polls write nothing), so migration 0079 adds
  `shipments.last_polled_at`, stamped by the ONE refresh
  service on every adapter attempt: an advance inside
  its transaction, an unchanged answer or a failed
  attempt as a best-effort standalone update (a failed
  stamp never masks the poll's real outcome). Manual
  shipments are never stamped — no attempt happens.
  STATUS AGED = in one non-terminal status past its
  threshold (`created` 24h, `picked_up` 48h, `in_transit`
  72h, `out_for_delivery` 24h, `partial_delivered` 48h,
  `return_in_transit` 72h, `unknown` 24h), overridable
  via the `shipment_aged_<status>_hours` platform
  settings (DB row gated 1..=720, then the domain
  default; no env layer — the delivery-estimates
  precedent). Aged-queue membership is computed on
  `updated_at`; the displayed per-row age is exact,
  from the newest event in the current status — a manual
  shipment whose receipt number was edited after its
  last status change can surface in the aged queue
  slightly late, never with a wrong displayed age.
  Editing the thresholds in the settings UI is a named
  follow-up for the settings remake (the fulfillment
  SLA pair is in the same state). Migration 0080 adds
  the queue's indexes.
- **The poller rotates now.** `refresh_stale_shipments`
  selects on `COALESCE(last_polled_at, updated_at)`:
  before the stamp, the same oldest ≤50 shipments were
  re-asked every cycle and rows past the batch limit
  could starve indefinitely. A just-polled shipment now
  waits its full staleness window however the poll
  ended. Pre-stamp rows fall back to `updated_at` —
  exactly the old rule.
- **The one action: single-parcel refresh.**
  `POST /v1/admin/shipments/{id}/refresh` delegates to
  the shared refresh core (the same function the
  participant endpoint and the worker run) with the
  admin's id threaded through; when the refresh
  ADVANCES the shipment, `shipment.refresh` is recorded
  in the refresh's own transaction (detail `{from, to,
  raw_status, courier, sub_order_id}`). Unchanged
  refreshes and courier errors write no audit row;
  errors propagate as errors (503 not configured, 502
  gateway), never folded into "unchanged". A manual
  shipment answers an honest `unchanged`. There is no
  bulk refresh and no status editing — transitions
  belong to courier polling and seller/buyer flows.
  Known inherited limit, not worsened: two CONCURRENT
  refreshes of one shipment can both apply the same
  transition (the shared core takes no row lock — the
  security audit's HI-49 pattern); admin clicks are
  human-rate and downstream syncs are the guard, and
  the fix belongs in the shared core, not this surface.
- **PII, following the orders precedent.** The list row
  carries the delivery name and a MASKED phone (triage),
  zone and district; never the address, the order note
  or the buyer's account phone. The detail carries the
  full delivery block — a stuck delivery cannot be
  worked with the courier without it, and the seller
  and courier already hold all three — while the
  buyer's ACCOUNT phone stays masked even there. The
  detail read is not reason-gated or audited: delivery
  data is ephemeral operational data, not a national
  credential like the verifications NID.
- **COD is shown as what it is.** Every figure is the
  stored `cod_amount_poisha` snapshot from booking,
  never recomputed. "Delivered COD held" is labelled an
  UNRECONCILED UPPER BOUND: no remittance/settlement
  tracking exists anywhere in the platform (the
  worker's `cod_reconciliation` job is still planned),
  so nothing here claims a courier has settled, and the
  figure grows monotonically until such a ledger
  exists. Partially delivered COD is shown separately
  (its stored figure is also an upper bound — no
  collected-so-far amount is recorded), and `unknown`
  shipments carry their COD as a named blind spot. The
  per-courier rollup merges the SQL groups onto the
  availability list, so zero-volume couriers appear;
  DEV-simulator bookings are flagged per row
  (`DEV-` consignments) and by the summary's
  `couriers_dev_mode`, because availability ranks
  credentials above dev mode while the adapter factory
  checks dev mode first.
- **Advance-paid RTO: surfaced, not solved.** A buyer
  who paid a COD advance (or prepaid in full) whose
  parcel returns by courier RTO has NO refund path
  anywhere: the sub-order is `returned`, so a buyer
  return request (which requires `delivered`) is
  unreachable, and the cancel paths refuse
  advance-paid orders and point at "the admin returns
  flow", which cannot accept an RTO sub-order either.
  The summary's `advance_stranded` block and a per-row
  detail flag expose the population (returned shipments
  on orders with `advance_paid_poisha > 0` and no
  return request; the poisha sums distinct orders).
  Creating the refund obligation is a returns/payments
  decision — this surface invents neither an amount
  beyond the recorded advance nor an action.

**Settings override precedence:** a row in `platform_settings`
(migration 0013) beats the startup value, which beats the
built-in default — for commission: setting > `COMMISSION_RATE_BPS`
env > 800; for the minimum payout: setting > the domain
constant `MIN_PAYOUT_POISHA` (50,000). Checkout and payout
creation read through the two effective-value helpers
(`effective_commission_bps`, `effective_min_payout`), never the
raw values. Stored values are parsed by the pure domain
function `parse_setting_i64` and range-checked on read, so a
bad row is ignored instead of breaking checkout. Overrides
apply to NEW operations only: an order keeps the commission
snapshot it was placed with, and a payout request keeps the
rules it was validated under.

**Overview metric definitions:** counts are plain table counts.
GMV (`gmv_poisha`, renamed from `gmv_goods_poisha` by the
overview remake) is the canonical Wave 27 formula — the sum of
`goods_subtotal_poisha − discount_poisha + delivery_charge_poisha`
over sub-orders whose status is NOT `cancelled` — and
`commission_earned_poisha` is the sum of the `commission_poisha`
snapshot over sub-orders whose status IS `delivered` (the
seller-earnings definition; a cancelled sub-order never moved
money, and an undelivered one has not earned yet).
`payouts_pending` covers payouts still `requested`;
`payments_pending_count` covers online payments still `pending`
settlement.

Auth rules that matter: only a SHA-256 hash of an OTP is ever stored;
ownership is checked on every seller mutation (403 for non-owners);
every product query is scoped by store. Listing visibility: public
callers see only `active` products of `active` stores; a signed-in
owner also sees their own store's drafts/archived items (and their
products while the store is suspended, Wave 10).

OpenAPI: dependencies stay minimal instead of pulling in a spec
generator; this table is the endpoint listing for now. The typed
TypeScript client is hand-maintained in `apps/web/lib/api.ts` (one
function per endpoint) until a generated spec lands.

Worked example (unit-tested in `crates/domain`): goods ৳4,450 + delivery
৳240 = ৳4,690 payable; 8% commission on the goods subtotal only = ৳356;
sellers receive ৳4,094.

Wave 54 (product page v2 + storefront v2 — the
Shopify-parity program continues):

**Buyers can follow stores, and the two pages a buyer
actually shops from — the product page and the
storefront — grow their missing halves.** Following is
one table (migration 0051, `store_follows`, PK
`(user_id, store_id)` — the primary key IS the
idempotency guard) behind `POST`/`DELETE
/v1/stores/{slug}/follow`: following your own store is a
bilingual 400 (the domain rule `can_follow_store`), a
signed-in buyer may follow at most 200 stores (a cap on
notification fan-out cost, enforced by the domain), and
both verbs return the store's fresh `FollowState`
(`following` + `follower_count`) so clients render the
server's answer, never an optimistic guess. The public
store view (`GET /v1/stores/{slug}`) gains
`follower_count` + `is_following` for the viewer, plus
the trust aggregates the storefront header prints:
`rating_avg`/`rating_count` over ALL the store's
products' reviews, `total_sold` (delivered units),
`product_count`. `GET /v1/follows` is the buyer's shelf:
their followed stores (suspended stores hidden — the
follow survives a suspension and returns on
reinstatement), each with its four newest active
products attached by one window-function query.
`GET /v1/seller/followers` lists the seller's own
followers with masked displays (a follow is a
subscription, not a contact list); the admin store list
gains `follower_count`; the buyer export gains a
`followed_stores` section.

**A publish notifies the followers.** When a product
BECOMES active — created active, a draft published, or a
Facebook import (imports are born active) — one funnel
(`enqueue_store_new_product_notices` in `notices.rs`,
called inside the publishing transaction) enqueues one
Wave 29 outbox row per follower under the new kind
`store_new_product`; the rendered body names the store
and the product. Editing an already-live product never
re-notifies. The worker needs no change: bodies render
at enqueue time and the outbox drains kinds generically.

**Storefront v2.** The store home and shop header gain
the trust line (rating, sold, followers, products,
joined) and the follow button; the store's own category
tiles are read from the faceted browse's category facet
scoped to the store, so a tile can never advertise an
empty shelf, and each deep-links into the shop page with
the category applied. Sellers write two long-form texts
— `about` and `return_policy` (migration 0052, nullable
columns on `stores`, 2,000-character caps validated in
the domain, edited in the branding page under the
established convention: absent keeps, blank clears) —
and the storefront prints them as sections when set.
`GET /v1/stores/{slug}/reviews` is the store's
all-reviews page data (every review of every product,
product titles attached, seller replies inline; reviews
of since-retired products stay listed — they were real
purchases), and `GET /v1/stores/{slug}/rating-summary`
the store-wide distribution.

**Product page v2.** Breadcrumbs (Home / Category /
Store / Title) with matching BreadcrumbList JSON-LD
alongside the extended Product JSON-LD. The delivery
block: the buyer picks a zone (persisted on the device
in localStorage — a browsing preference, not account
data) and `GET /v1/delivery-estimate?zone=&weight_grams=`
answers with the Wave 1 charge rule for one parcel of
that weight plus the zone's delivery-time range. The
ranges are platform settings (`delivery_days_inside_dhaka`
etc., stored as `min-max` text) read DB > built-in
default (Dhaka 1–2, suburb 2–3, outside 3–5) — the
Wave 21 KYC-gate precedent: there is deliberately no env
layer — and the admin settings page edits all three,
validated by the domain rule (whole days, 0–30, earliest
not after latest). The ranges are display promises only;
they never move a charge. The stock urgency line follows
the API's own `stock_status` (the seller's low-stock
threshold included) for the base product and the
small-number rule for a picked variant; the number shown
is always the real stock. Rating summaries come from
`GET /v1/products/{id}/rating-summary` (and the store
twin): counts per star with FLOORED percents — bars never
overstate — plus the photographed-review count. The
review list takes `?sort=newest|highest|lowest|photos`
(validated in the domain; anything else is a 400); the
page adds a photo strip of the listed reviews' photos,
the variant picker shows each variant's own photo and
its price delta, the share row uses the device share
sheet or copies the link (nothing is posted or counted
by the platform), buy-now adds the item and goes to the
cart (the platform's one purchase path — no parallel
checkout), and a sticky buy bar pins the same
flash-aware price and the same actions once the buyer
scrolls past the buy block. "More from this store"
(best sellers, this product excluded) joins the related
and also-bought rails.

**The extension scope.** Sellers curate their own
collections (migration 0053 — distinct from the Wave 52
platform collections): slug/title/description under the
domain's validators, a 20-per-store cap, and an ordered
membership the seller replaces wholesale (duplicates
collapse, every product must be the store's own, the
swap is one transaction; public reads show active
members in the seller's order, drafts stay members but
invisible). The storefront renders the shelves as tiles
and each shelf gets a public page under the store. The
follow graph gains its reading surfaces: `GET
/v1/follows/feed` (the newest products across followed
stores — the home page's personal rail), `GET
/v1/follows/suggested` (active stores ranked by real
follower counts and ratings, excluding the viewer's own
and already-followed — no paid placement), and `GET
/v1/seller/followers/stats` (the total, 7/30-day gains
and a zero-filled 30-day daily series in Asia/Dhaka
dates, graphed on the seller followers page). And the
outbox gains its owner-side read: the buyer's
notification inbox (`GET /v1/notifications`, migration
0054's `read_at` — the user's state on their copy,
never a delivery input) with mark-one-read and
mark-all-read, at `/notifications`, with an unread bell
in the marketplace frame.

**The second extension round.** Reviews gain helpful
votes (migration 0055): one vote per signed-in user per
review, the author cannot vote on their own review,
cast/retract are idempotent and return the fresh state,
the count rides every review view, and `helpful` joins
the review sorts on the product page and the store
reviews page. Follows gain a per-follow mute (migration
0056's `notify_new_products`, PATCH on the follow
endpoint): muting keeps the follow — the shelf and the
feed — and stops only the notices; the toggle lives on
the /following cards, and sellers and admins see who
muted in their follower lists. The same flag gates the
new `store_flash_sale` notice: when the admin item-set
of a sale that is LIVE at that moment gains a product,
the unmuted followers of its store are alerted once —
re-saving the same set is silent, and items staged into
a future sale alert nobody at staging time — they alert
only if the item set is saved again while the sale is
live; the honest limit is stated plainly rather than
hidden.
Questions gain an answered-first sort and the list
envelope reports how many visible questions still await
the seller. Admin gains the per-store follower list
(`/v1/admin/stores/:id/followers`, linked from the
stores table) and the analytics totals gain a community
block (total/new/muted follows, store collections). The
cart page reuses the delivery estimate block over the
cart's total parcel weight.

Honest limits: the follow cap (200) is enforced on the
follow write only; the delivery day ranges are the
platform's published promises, not per-courier tracking
estimates (courier ETAs remain the couriers' own data);
review photos in the strip come from the currently
listed page of reviews, not a separate all-photos index;
the sticky bar appears after a fixed scroll threshold,
not by observing the buy block; store suggestions rank
by followers then ratings only — there is no
personalisation model behind them, and the label says
exactly what they are.

Wave 55 (cart / checkout / tracking polish — the
Shopify-parity program continues):

**The buyer gains an address book, checkout learns
districts, the cart gains a save-for-later shelf, past
orders can be bought again, and tracking grows a real
timeline.** The address book (migration 0057,
`buyer_addresses`) sits behind `GET`/`POST
/v1/addresses`, `PATCH`/`DELETE /v1/addresses/{id}` and
`POST /v1/addresses/{id}/default` — own rows only,
another buyer's id is a 404 on every verb. Every write
passes the domain's one validation
(`validate_address_input`): label ≤ 40 chars, recipient
≤ 80, area ≤ 80, address line ≤ 200, the phone through
the existing BD normaliser (stored canonical), and the
district against the domain's district table. A buyer
keeps at most 10 addresses (the domain cap, counted
inside a transaction that locks the buyer's user row);
exactly one may be the default (a partial unique index
makes it a database fact, and writes switch it inside
one transaction). The FIRST address saved becomes the
default automatically; deleting the default leaves NO
default — the platform does not silently promote an
address the buyer did not choose. The list orders
default-first, then most recently used at checkout,
then newest.

**Districts are platform policy, stated here.** The
domain carries all 64 districts of Bangladesh with
English + Bangla names, each mapped to one of the three
delivery zones (`GET /v1/districts`, public): Dhaka
district → Inside Dhaka; the five districts that BORDER
Dhaka district — Gazipur, Narayanganj, Munshiganj,
Narsingdi, Manikganj — → Dhaka Suburb; every other
district → Outside Dhaka. Tangail is deliberately
Outside Dhaka: it does not border Dhaka district.
Granularity is the district — thana-level refinement
(Keraniganj-style splits inside a district) is a known
limit, not a silent approximation: a Dhaka-district
address in a far thana is priced Inside Dhaka until a
thana table exists. The zone is NEVER stored on an
address and never accepted from a client; it derives
from the district through the table at read time, so a
mapping change moves every saved address at once.

**Checkout gains three additive inputs; the money canon
(advance Wave 17, coupons Wave 14, loyalty Wave 35,
flash Wave 52) is untouched.** `address_id`: the server
loads the buyer's own address (another buyer's id is a
404 and no order is created) and the order's delivery
name/phone/address/zone/district come from THAT row —
the client-sent delivery fields are ignored on this
path — and the address's `last_used_at` is stamped
inside the checkout transaction (migration 0057's
column; a rolled-back checkout stamps nothing).
`delivery_district` on the manual path: the district is
validated against the table (unknown → bilingual 400)
and the zone derives from it, so a district and a zone
that disagree can never both reach an order. With
neither input, the pre-Wave-55 body-fields path runs
byte-identically. `order_note` (migration 0058, with
`orders.delivery_district`): the buyer's note to the
sellers, domain-capped at 500 chars (over → 400 before
anything writes), stored on the order and shown on the
buyer's order + receipt, the seller's sub-order list,
the fulfillment queue AND the packing-label data — the
two surfaces a seller works from before booking.
Checkout's page mirrors all of it: an address picker
(radio cards; picking one locks the manual form), a
district select that sets the zone, the note field, and
the Wave 54 delivery estimate for the chosen zone. The
admin metrics snapshot gains `orders_by_district` —
only orders that NAMED a district are counted; a
zone-only order has no district to attribute and
inventing one from the zone would fabricate geography.

**Cart v2.** Migration 0059 adds `cart_items.saved`.
`POST /v1/cart/items/{product_id}/save` and `/unsave`
(body carries `variant_id`, like the quantity write)
move a line between the active cart and the shelf; the
cart response gains `saved_items` as its own array.
Saved lines are out of the goods total, invisible to
checkout (the lock scan and the post-checkout clear
both filter them — a cart of only saved lines is
"empty"), and survive DELETE /v1/cart. Adding a saved
product again revives its line into the active cart
(the add upsert clears the flag). The quantity PATCH
now respects real stock: past the line's stock — the
variant's stock for a variant line, the product's
otherwise — it answers the bilingual 400 "স্টকে মাত্র
{n}টি আছে"; exactly-at-stock is accepted. The cart page
renders the shelf with move-back/remove, and per-line
"পরে কিনুন — Save for later".

**Reorder ("buy again").** `POST /v1/orders/{id}/reorder`
re-adds a past order's lines at CURRENT prices through
the cart module's one add path — old prices are never
consulted; the order contributes only (product,
variant, quantity). The domain's `reorder_decision`
names every skip: `product_unavailable` (gone or not
active), `variant_unavailable` (the bought variant was
deactivated or removed, or the product has since gained
variants so the old variant-less line no longer names a
buyable thing), `out_of_stock`, `cart_limit` (the cart
line is already at the 99 cap). Partial adds clamp to
min(ordered, today's stock, cart headroom). The report
is honest partial success — `{added, skipped: [{title,
reason, message}], cart}` — and the buyer orders page
renders it under the order, next to a "deliver to"
summary line. A cancelled order is a 409; another
buyer's order is a 404.

**Tracking v2.** The public tracking response and the
buyer order/receipt views gain TIMELINES, built by one
shared builder (the API's `timeline` module): the
order-level list (placement, then per payment row the
initiation/settlement events — named `advance_*` on COD
orders, where a payment row is by construction the
Wave 17 advance) and a per-sub-order list (placed,
confirmed, shipment booked with courier + consignment,
one event per `shipment_events` row, delivered,
returned/cancelled, and the return request + decision
when they exist). The web renders a stepper whose
derivation mirrors the domain's
`sub_order_tracking_steps` branch for branch.
**Timestamp honesty:** an event carries a time only
when a real column recorded the moment. Placement,
booking, every courier event, payment settlement and
the return rows have one; a seller's confirm (or a
manual delivered/cancel flip) has NONE — orders and
sub-orders carry no status-change timestamps — so
those events ship `at: null` in their logical position.
No surface invents a time.

**Wave 55 extensions.** The stepper is also computed
SERVER-side: every sub-order view (buyer order, receipt,
seller list, public track) carries `steps` from the
domain derivation itself, and the web prefers them,
keeping its local mirror only as a fallback. The seller
sub-order list carries the same timeline + steps, plus
the order's district; so do the fulfillment queue item
and the packing label. The shelf gains bulk operations
(`POST /v1/cart/saved/move-all`, `DELETE
/v1/cart/saved`). A buy-again rail (`GET
/v1/buyer/buy-again`) lists products from the buyer's
past orders that can be bought again TODAY — active and
in stock, flash-resolved current prices, `times_ordered`
and an `in_cart` flag; archived or sold-out products
simply leave the rail rather than failing at add time.
The address book gains a single-address `GET
/v1/addresses/{id}` and a checkout-prefill endpoint
`GET /v1/buyer/default-address` (404 when the book has
no default; it lives under `/v1/buyer/` because axum
0.7 cannot register a static `default` segment beside
`/v1/addresses/:id`). `GET /v1/districts?q=` narrows
the table through the domain's `search_districts`
(English prefix, then substring, then Bangla name). The
per-store invoice shows the order's district and note;
the buyer data export (Wave 31) gains an `addresses`
section — its doc comment previously claimed no address
book existed, which this wave made false; the admin
metrics gain `orders_by_zone` (complete — every order
has a zone) and an `address_book` adoption block
(total saved, buyers holding any, defaults in force).
Checkout's success screen offers to save a manually
typed address into the book (asking only for the area,
the one split the single checkout field cannot know),
and the address form + checkout share one
`DistrictSelect` component so their district lists can
never drift.

Honest limits: the district mapping is district-level
policy (above); deleting the default address leaves
the book without one until the buyer picks another;
reorder prices are whatever the product costs NOW —
including a live flash sale, since reorder adds through
the same cart path as a manual add; timeline
granularity is bounded by the recorded columns (the
confirm/manual-flip moments are unknowable from the
schema, stated above rather than papered over); the
district statistics count only district-known orders.

Wave 56 (category taxonomy v2 — the Shopify-parity
program continues):

**Categories stop being free-text strings and become a
managed taxonomy — without breaking one existing
surface.** Migration 0060 adds `categories` (id,
`parent_id` self-FK, `slug` UNIQUE across the whole
tree, `name_en` + `name_bn`, `icon_key`, `sort_order`,
`is_active`, `is_featured`, timestamps) and
`products.category_id` (nullable FK). The legacy
`products.category` text column STAYS as the
denormalised display label, kept in sync by the service
layer, so search facets, exports and every text-based
consumer keep working byte-identically (below).

**The model.** Depth is capped at 3 levels (root, child,
grandchild) by the domain's one validation
(`validate_category_parent`): reparenting rejects
cycles and any move that would push a subtree past the
cap; slugs follow `is_valid_slug` and are unique
tree-wide (the database UNIQUE answers with a friendly
bilingual 409); names cap at 80 chars; `icon_key` is a
KEY into a small built-in set (`tag`, `shirt`, `phone`,
`cart`, …) — an icon is a glyph the web renders, never
an uploaded image. Display order is `sort_order`, then
English name, then slug — one rule, shared by the
admin list, both trees and the home strip. A category
with children or with products CANNOT be deleted (409
naming the counts); the merge endpoint is the bulk
alternative (below). Orphan rule: a node whose parent
is missing (retired) surfaces at the top of the public
tree rather than vanishing with its products.

**The seed (migration 0061) is reference data in a
migration** — a deliberate new convention: no earlier
migration carries data, but migrations are the repo's
only run-exactly-once mechanism, so the taxonomy ships
there, idempotently (`ON CONFLICT (slug) DO NOTHING`).
Eleven roots — Fashion/ফ্যাশন, Electronics/ইলেকট্রনিক্স,
Grocery/মুদি বাজার, Home & Living, Beauty & Health,
Baby & Kids, Sports & Fitness, Motors, Agriculture,
Books & Stationery, Others/অন্যান্য — with 36 children
and 6 grandchildren (Panjabi under Men's Fashion under
Fashion, and so on), real Bangla + English names.
Fashion and Electronics are `is_featured`. Migration
0062 backfills `category_id` for existing products
whose label matches a taxonomy slug or name EXACTLY
(case-insensitive on slug/English, exact on Bangla) and
unambiguously (exactly one candidate node); the label
itself is never rewritten by the backfill, and
unmatched products stay NULL with their text intact.

**Linking products — the resolution rule, stated
once.** Create and patch resolve through ONE service
function. An explicit `category_id` wins: it must exist
and be active (400 otherwise), and the text label
SYNCS to the node's English name. Without an id, a
non-blank text label resolves by exact match — slug,
then English name (case-insensitive), then Bangla name
— against ACTIVE nodes, and the label stays exactly as
typed; no match keeps the product free-text with
`category_id` NULL. A patch carrying a label re-resolves
(and unlinks when the new label matches nothing); a
patch carrying neither keeps the stored link; a blank
label in a patch still means "no change". A product may
sit on an INTERIOR node — leaf-ness is not required;
interior products are exactly what subtree browse rolls
up. Product views gain `category_id` and
`category_path` (the root→node breadcrumb) additively.

**Admin (audited, one transaction per operation):**
`GET`/`POST /v1/admin/categories`, `GET
/v1/admin/categories/tree`, `GET`/`PATCH`/`DELETE
/v1/admin/categories/{id}`, `PUT
/v1/admin/categories/reorder` (bulk sort orders, all or
nothing), `POST /v1/admin/categories/{id}/merge`
(products re-link — a product whose label was the
source's synced name re-syncs to the target's — children
re-root, the source is deleted, one report),
`GET /v1/admin/categories/{id}/products` (the subtree's
products in ANY status — the delete guard's count,
inspectable), and `GET /v1/admin/categories/export` +
`POST /v1/admin/categories/import` (a slug-keyed JSON
round-trip: parents travel by slug, never ids; import
upserts by slug and validates the merged tree BEFORE
writing, so a cyclic or dangling document changes
nothing). Audit actions `category.create/update/
delete/reorder/merge/import` join the Wave 28 funnel.
The web admin gains /admin/categories: tree-ordered
list, create/edit form (parent picker, icon picker),
sibling reorder, drill-in, delete, merge and the
import/export panel.

**Public surfaces — additive by design.**
`GET /v1/categories` still returns the Wave 53
free-text aggregate in `categories` (the home feed's
tiles and the aggregate are pinned byte-identical by
the Wave 52/53 tests, so the taxonomy did NOT replace
them) and ADDS the tree as a `tree` field; `?flat=1`
returns exactly the pre-Wave-56 shape. `GET
/v1/categories/tree` serves the tree alone; `GET
/v1/categories/{slug}` serves one node + its path (404
for unknown or retired slugs). Tree counts are subtree
ROLLUPS over visible products (active product, active
store) — deliberately unlike the legacy aggregate,
which counts direct labels only. The home feed gains a
`taxonomy` field (top-level nodes, featured first) and
keeps `categories` untouched; the web home renders the
taxonomy strip above the legacy tiles, /categories
renders taxonomy sections first and the labels no node
covers under "আরও লেবেল", and /categories/[slug] is a
taxonomy landing (breadcrumb, children tiles, subtree
browse) that falls back to the legacy text landing for
non-taxonomy segments. Product-page breadcrumbs follow
`category_path` when the product is linked.
`GET /v1/categories/{slug}/products` serves the node's
subtree as a plain product list (direct addressing:
unknown or retired slugs 404, unlike the list filter).
A category navigation bar (quick roots + the full-tree
mega panel) rides in the marketplace frame on every
marketplace page, and a store's specialisation tiles
land on IN-STORE category pages
(`/<store>/categories/<slug>` — the same landing,
scoped to the store) rather than leaving the store.

**Browse.** `GET /v1/search` and the product lists gain
a repeatable `category_slug` parameter resolving to
subtree id sets. The Wave 53 facet convention is
UNCHANGED: the slug filter belongs to the categories
facet group — items and total apply it, the categories
facet is computed without it, the other facets with it —
and the facet itself still groups by the text column.
The slug travels OUTSIDE `BrowseFilters`, so the
analytics fingerprint and saved searches keep their
Wave 53 meaning; a slug-scoped search logs under the
plain fingerprint (stated, not hidden). Suggestions
gain `category_nodes` (labels resolved into taxonomy
nodes with paths) beside the unchanged text fields.
`GET /v1/stores/{slug}/categories` reports a store's top
linked categories with paths — its specialisation,
computed from its real active products — rendered on
the storefront above the legacy label tiles.

**Seller UX.** The product form's free-text category
input becomes a tree PICKER (`CategoryPicker`, fed by
the public tree) with an explicit "use a custom label"
escape — a taxonomy can lag a seller's vocabulary, and
forcing a wrong node would corrupt the data the picker
exists to clean; the seeded Others/অন্যান্য node is the
catch-all for products that fit the taxonomy's shape
but no specific branch. The seller product list filters
by category subtree (the same `category_slug`
parameter).

Honest limits: free-text products remain possible via
the API whenever a label matches no taxonomy node —
the taxonomy organises the catalogue, it does not gate
it; renaming a category does not rewrite labels already
synced onto its products (a label re-syncs on the
product's own next write) and resolution always uses
CURRENT names, so a renamed node's old name stops
linking new products immediately; `category_path` is filled on list, detail, browse
and drill-in reads, and stays empty on the card rails
(home rails, recommendations) to keep those payloads
lean; a retired (inactive) node keeps its products'
links — they stay searchable by text and reappear if
the node is reactivated — but the node leaves the
public tree and its slug stops resolving for browse;
slug-scoped searches are not distinguished in the
search analytics (above); icons are keys rendered as
glyphs, not category images (a node borrows its cover
from its best-selling imaged product, the Wave 53
pattern).

Wave 57 (seller bulk catalogue + demo catalogue seed —
the Shopify-parity program continues):

**The spreadsheet door.** Real sellers keep catalogues
in sheets, not forms. The domain gains a
dependency-free CSV parser/writer (quoted fields,
embedded commas/quotes/newlines, CRLF, BOM) and the
bulk row rules: one column contract (`sku`, `title`,
`title_bn`, `description`, `price_poisha`,
`compare_at_poisha`, `stock`, `weight_grams`, `status`,
`category_slug`, `category_label`, `image_url` — prices
in poisha, like everywhere), per-row validation reusing
the SAME product validators the single-create form
passes, per-row error accumulation (spreadsheet row
number + field + bilingual message), and the batch caps
(500 rows per import, 200 products per bulk edit). A
blank `status` cell defaults to `draft` — a sheet that
does not say otherwise never publishes by accident —
and an ACTIVE row requires a price above zero.

**Bulk import** (`POST /v1/seller/products/bulk-import?store_id=&dry_run=`,
raw CSV body, route body limit 4 MiB; the Wave 12
Facebook import keeps its own `/import` route). Store
scoping is the Wave 34 rule (`manage_catalog` — owners,
managers and staff may import; strangers get the access
module's 404). Idempotency mirrors Wave 12: the sheet's
`sku` is stored as the product's `source_ref` with
`source = 'bulk'`, so re-importing a sheet UPDATES
price/stock instead of duplicating; a sku already
claimed by another source is a per-row error, never a
hijack. The three invariants the wave was built around:
the **plan-cap gate** is the shared
`product_cap_for_store` — the batch spends the store's
remaining budget on net-new rows in file order, rows
past the budget fail individually and the rest still
applies (partial apply), and updates never consume
budget; the **follower-notice funnel** is the shared
`enqueue_store_new_product_notices`, fired only for
rows born active AND newly created, inside the row's
own transaction (updates never notify — a re-import is
a sync, not a publish); the **single stock path** holds
— a re-import that changes stock writes the new value
and its `stock_adjustments` row in the same transaction
through the Wave 23 writer (reason `count_fix`), and a
product whose stock lives on its variants keeps that
stock with a note on its outcome instead of an error.
Category resolution is per row through the Wave 56
taxonomy (slug wins over label; a label links on the
exact-match rule and is stored as typed; a non-matching
label stays free-text with a note). A row's `image_url`
attaches afterwards through the Wave 51 from-url
service — best-effort, its failure rides the outcome's
`image_error` and never fails the row. Dry runs compute
the identical report (`would_create` / `would_update`)
and write no products — but they ARE recorded, like
every completed run, in the import history (migration
0063, `bulk_imports`): `GET …/import-history` lists a
store's runs with counts, `GET …/import-history/{id}`
re-opens the full stored report. Fatally malformed
files (unparseable CSV, unknown/missing/duplicated
header column, over the row cap) refuse the whole
request with a 400, write nothing and record nothing.
`GET …/import-template.csv` downloads an example sheet
generated from the domain column spec, so it cannot
drift from the parser's contract.

**Bulk edit** (`POST /v1/seller/products/bulk-edit`):
price (set / percent / fixed poisha delta — the
domain's checked math: percent rounds half up to the
poisha and floors at 1 poisha, so a bulk cut can never
make a product free), stock (set / adjust — computed as
a delta and applied ONLY through the Wave 23 audited
adjust-stock service), status flips and category
assignment (slug / id / label, resolved once per
request). Price, status and category ride the shared
single-product update path, so its merged validation,
variant-stranding guard and draft→active follower
notice all apply unchanged. Per-item outcomes: a
foreign or missing product id is an error entry, never
silently skipped; request-shape problems (no ids, no
action, bad percent) are whole-request 400s. The batch
is deliberately not one transaction (the Wave 40
bulk-book precedent) — the report names exactly what
landed.

**Catalogue export** (`GET /v1/seller/products/export.csv?store_id=`):
the store's whole catalogue in the import's own column
format — export, edit, re-import is the round-trip
(pinned by test, including the category links). Base
product rows only: variants are managed in their own
module and are deliberately not flattened into the
sheet. The `sku` column is the product's `source_ref`;
products created outside an import carry none and
export with a blank sku — assign one in the sheet
before re-importing those rows (the import requires a
sku and will say so per row, never silently duplicate).
The exported stock is the product's stored pool; for a
variant product the live stock is its variants' sum and
a re-import reports stock changes as variant-managed.

**The demo catalogue seed.** `worker seed-demo` (a
one-shot subcommand of the worker binary) refuses to
run unless `DEMO_SEED=1` is set, then builds the demo
catalogue THROUGH the same service functions — eight
demo stores across the taxonomy (fashion, electronics,
grocery, home, beauty, agriculture, sports, baby &
kids) with 161 products defined as compact data tables in
`crates/api/src/demo_catalogue.rs` (realistic BD
products with Bangla + English titles, poisha prices,
weights, Wave 56 category slugs). Each store's sheet
runs through the bulk import itself, so re-running the
seed updates in place (keyed on sku) instead of
duplicating. Each newly created product receives ONE
generated placeholder image through the real Wave 51
upload pipeline — a deterministic gradient rendered
in-process from the product's sku by the `image` crate,
no network — honestly labelled as a placeholder in its
alt text; it demonstrates the media system, it is not
product photography. The seller web gains a
`/seller/bulk` page: CSV upload with dry-run preview
(the report as a table), the template + export
downloads, the import history with re-openable reports,
and a bulk-edit panel over the store's product list.

Honest limits: the import is CSV only (no XLSX — a
seller saves-as CSV, the universal exchange format);
import updates never fire follower notices and never
touch variants (variant stock and variant prices stay
in their own module); a blank optional cell on an
update row leaves the stored value standing (title_bn,
description, compare_at), while data columns
(title, price, stock, weight, status, category) always
overwrite — the sheet is the truth for those; the seed's
demo seller accounts use sequential 0171-range numbers
created only behind the `DEMO_SEED=1` guard, and the
seed's placeholder images are generated art, stated
above.

Wave 58 (storefront section editor — the Shopify-parity
program continues): until now a seller picked one of
three themes plus branding, and the storefront's layout
was fixed code. This wave makes the layout itself the
seller's: a real sections system — an ordered,
configurable composition per store — with a domain
catalogue, a resolved public render model, a full
seller editor with live preview, per-section scheduling,
and aggregate-only section analytics.

**The catalogue** (domain crate; the caps are the
domain's, enforced on every write): nine section types —
`hero` (headline ≤120 chars, subtext ≤300, image, link +
label ≤60; every field optional, unset fields fall back
to the store's branding at resolution), `rich_text`
(heading ≤120, body required ≤2,000), `product_rail`
(source `newest` | `best_sellers` | `on_sale` |
`manual`; title ≤80; limit 4–24, default 10; a manual
rail holds 1–24 of the store's OWN product ids, in the
seller's order, duplicates collapsed), `collection_shelf`
(an optional Wave 54 collection id — absent means all
collections as tiles; limit 4–24, default 8),
`category_grid` (mode `taxonomy` | `labels`; limit 4–24,
default 12), `product_grid` (the full catalogue; title
≤80), `image_banner` (image required, optional link +
caption ≤120), `store_info` (no config — its texts are
the branding-owned about / return-policy, never copied
into the layout), `faq` (1–10 pairs; question ≤200,
answer ≤1,000). A layout holds at most **20 sections**;
section ids are unique slug-shaped handles ≤40 chars
(they key the analytics). Validation errors are
bilingual and name the offending section's 1-based
position ("section 2 (faq): …"), and a failed PUT
changes nothing.

**Versioning & tolerance:** the document is
`{ "version": 1, "sections": [...] }` stored as one
JSONB row per store (migration 0065, `store_layouts`).
Reads are forward-tolerant (unknown fields ignored,
missing `enabled` defaults to true). On WRITE the
version must be current — a future document is refused,
never silently rewritten. A section whose type is
well-formed but UNKNOWN to this build is a future
section: it is preserved verbatim (config, flags,
window) on write, shown as a locked block in the
editor, and skipped at render; a malformed type string
is refused as garbage. (The brief's "unknown type
rejected" and "unknown types skipped but preserved"
read together this way: garbage is rejected, the
future is preserved.)

**The default layout** is CODE, not data:
`domain::default_storefront_layout()` composes hero →
on-sale rail → best-sellers rail → newest rail →
collection shelf → taxonomy grid → label grid →
catalogue grid → store info, with the rails' historical
titles — exactly the pre-Wave-58 storefront. A store
with no row renders it; **reset = DELETE the row**.
Three presets (`default`, `minimal`, `merchandising`)
ship in the catalogue as one-click starting points;
every preset passes the same validation as a seller
document.

**Endpoints:**

| Method | Path | What it does |
| --- | --- | --- |
| GET | `/v1/stores/{slug}/layout` | Public. The RESOLVED render model: sections in order, each with its data loaded server-side (rails carry real product cards through the shared product view, flash prices attached; shelves carry collections/products; grids carry counts; the hero carries branding fallbacks + an `is_customised` flag). Non-active store → 404 |
| GET | `/v1/seller/store/layout` | Bearer; the branding access rule (owner, or `manage_marketing` via `?store_id=` — manager yes, staff 403, stranger 404). The effective document (stored or default) with `is_custom`, windows as RFC 3339 |
| PUT | `/v1/seller/store/layout` | Same access. Full replace: domain validation first, then reference checks (manual rails name only the store's own products; shelves only its own collections — a foreign id is a 400 naming the section). Last write wins — no conflict guard, by design; `updated_at`/`updated_by` keep provenance |
| POST | `/v1/seller/store/layout/reset` | Same access. Deletes the row; answers the default document |
| GET | `/v1/seller/store/layout/catalogue` | Bearer. Editor metadata from the domain: types with Bangla+English labels/descriptions, caps, rail sources, grid modes, presets |
| POST | `/v1/stores/{slug}/section-events` | Public, anonymous. Body `{section_id, kind}` with kind `section_view` \| `section_click`; the section must exist in the store's current effective layout (404 otherwise). Aggregate-only: no user, session, IP or agent is recorded (migration 0066, `storefront_events`) |
| GET | `/v1/seller/store/layout/stats` | Same access as the layout. One row per section of the current layout (zeros included): total views/clicks, today's (Asia/Dhaka), and the trailing 30 days |

**Resolution rules** (the public GET): disabled
sections, sections outside their scheduling window
(per-section `starts_at`/`ends_at`, the Wave 52 banner
rule — half-open [start, end), via the domain's one
`storefront_section_is_live`), unknown types, and
sections whose data resolves EMPTY are dropped
server-side: an on-sale rail with nothing on sale, a
best-sellers rail with no delivered units (the
platform's one "sold"), a shelf whose collection was
deleted or whose members all drafted, a category grid
with no categories, store info with neither text
written, an FAQ with no pairs. The ONE exception is
`product_grid`, which always resolves (it carries the
store's active-product total; an empty storefront must
show its empty state where the seller placed it).
Resolution always follows the CURRENT catalogue: a
product drafted after the layout was saved drops out
of its rail/shelf quietly; the stored document is
never rewritten by reads. Taxonomy grids read through
the Wave 56 specialties service (which caps at 8);
label grids use the faceted browse's category facet
scoped to the store, so the grid and the old tiles can
never disagree.

**Rendering (web):** the storefront page fetches the
layout and renders one component per type
(`StorefrontSectionViews`) reusing ProductRail /
ProductGrid / the Wave 54–56 tile markup. Placement
rule: hero sections render ABOVE the identity frame
(header, trust line, follow/message/search — who the
store IS, not layout), everything else below it in
layout order; with the default layout the page is the
pre-Wave-58 page. The theme still frames everything
(accent, banner rule, grid card layout) — sections are
layout, theme is skin. The shop / categories /
collections / reviews routes are separate surfaces,
untouched: sections compose the storefront HOME only.
Each rendered section fires one `section_view` beacon
per page load; hero/banner link clicks fire
`section_click` (fire-and-forget). If the layout read
fails, the page falls back to the legacy fixed
composition (the Wave 52–56 components) — a storefront
never blanks over a sections outage.

**The editor** (`/seller/storefront`, linked from the
dashboard and the branding page): section cards with
bilingual type labels, id editing (the analytics
handle), enable/disable, up/down reorder, duplicate,
delete, per-type config forms (including a product
multi-select for manual rails that shows the pick
order, a collection select, an FAQ pair editor, and
datetime scheduling), an add-section picker driven by
the catalogue endpoint, preset loading, save + reset
with confirm, a stats panel, and a LIVE PREVIEW through
the same section components. **Preview fidelity
limit:** the preview resolves the draft CLIENT-side
from the seller's own reads (search/store products,
collections, facets, the store view); the live page
resolves server-side at request time, so edge cases
can differ (a flash sale starting mid-edit, a
collection changed in another tab) — the saved layout
read back from the API is the truth.

**Honest limits:** the editor acts on the seller's own
store (a manager's `?store_id=` layout edit works at
the API, like branding's, but the editor page itself
does not yet offer a store switcher — the same limit
the branding page has); section analytics count page
loads that render a section, not viewability (no
intersection observation — a view means "served in the
layout", stated here so the numbers are never
oversold); store_info's trust numbers render in the
identity frame's stats line (the section carries the
texts; the API payload includes the aggregates for
clients that want them).

## Wave 59 — store content: custom pages + store blog

Shopify stores have two kinds of writing the product
catalogue is not: standing documents (About, Shipping
policy, FAQ, Size guide) and a blog. This platform's
stores had neither — Waves 18/54 gave a store an
about text and a return policy rendered inside the
storefront frame, but no standalone pages and no
posts. Wave 59 builds the content system
(`crates/api/src/store_content.rs`, migrations
0067–0069, domain rules in the Wave 59 section of
`crates/domain`).

**The model.** `store_pages` (slug, title, body,
status, `show_in_nav`, `position`) and `store_posts`
(the same plus a hand-written excerpt, normalised
tags, a cover image, a `published_at` moment, and a
word count maintained at write time), each unique per
(store, slug) — the same slug in another store is a
different document, and a page and a post may share a
slug inside one store. Both kinds share one status
vocabulary — draft / published / archived — and one
domain state machine: any move between distinct
states is allowed, staying put is not a transition
and is refused. Caps are domain constants, enforced
at the API: 25 pages and 500 posts per store, page
titles ≤ 120 chars, post titles ≤ 160, page bodies
≤ 20,000 chars, post bodies ≤ 40,000, excerpts
≤ 300 chars, at most 8 tags of ≤ 30 chars.

**Plain text only — the XSS stance.** Bodies are
stored and rendered as PLAIN TEXT with paragraph
breaks (split on blank lines; single line breaks are
preserved as line breaks). No HTML or Markdown is
accepted, stored, or rendered anywhere in the
content system, so seller input can never become
markup on a public page — the exclusion is by
design, and the editors say so next to the body
field.

**Scheduling.** A post is public only when it is
published AND its moment has arrived
(`published_at <= now`). Publishing names the
moment: none supplied means now; a future moment
schedules the post, and it goes live on its own when
the moment passes — no job flips anything, liveness
is computed at read time (the same discipline as the
Wave 52 banner windows and Wave 58 section windows).
The stamp rules are part of the contract: publishing
sets the stamp (supplied, else the existing one, else
now); unpublishing back to draft CLEARS it, so a
later publish starts a fresh moment; archiving keeps
it, so restoring an archived post restores its
original moment; and re-publishing an
already-published post WITH a new moment is the one
same-status write the machine allows — a reschedule,
which hides the post until the new moment arrives.
Pages have no scheduling (their status is the whole
story).

**Seller surface.** Pages and posts are managed
under `/v1/seller/store/pages` and
`/v1/seller/store/posts`, behind the SAME access
rule as store branding (Wave 34's manage-marketing
capability: the owner and managers write, staff are
refused 403, strangers get the scoped 404, anonymous
callers 401). Page reordering is a PUT on the pages
collection naming the store's EXACT current page set
— a partial list, a duplicate, or another store's
page id is refused and nothing moves (a `/reorder`
sibling route is impossible under the router's
static-beside-param rule, the Wave 54 notifications
precedent). The post editor's list carries a status
filter and a title/slug search. Post covers work two
ways, both first-class: a hotlink field validated by
the same external-image rule as product images, and
an upload through the Wave 51 media pipeline under
the post's own key (`stores/<store>/posts/<post>/
cover`), with the derived sizes and object cleanup
that pipeline provides.

**Public surface.** `GET /v1/stores/:slug/pages`
(the nav list — published + nav-flagged, in the
seller's order — with the store's live `blog_post_count`
riding along so the storefront frame can render its
Blog link in one round trip), `GET
/v1/stores/:slug/pages/:page_slug` (published only,
else the uniform 404), `GET /v1/stores/:slug/posts`
(live posts, newest first, paginated, with an
optional tag filter — the filter value is normalised
by the same domain rule that normalises tags at
write time, and a tag that can never exist answers
an honest empty list), and `GET
/v1/stores/:slug/posts/:post_slug` with the previous
(older) and next (newer) live neighbours and a
related rail ranked by shared-tag count then
newest — a tagless post gets NO related rail rather
than unrelated filler. Tags are normalised at write
time (Unicode-lowercased, interior whitespace runs
become one hyphen, duplicates collapse first-seen;
anything outside letters/digits/hyphens is refused
by name). The excerpt on every card is the
hand-written one when present, else DERIVED from the
body at read time — whitespace collapsed, cut at the
last word boundary inside 240 chars, NO ellipsis
appended (a truncation that admits itself would be
fakery of a small kind; the rule is a real cut).
Reading time is the stored word count at 200 words
per minute, ceiling, minimum one minute. Suspended
stores answer the uniform 404 on every content
surface, exactly like their products, while the
seller keeps full editor access.

**The feed.** `GET /v1/stores/:slug/feed` serves an
Atom feed of the store's latest 20 live posts —
hand-built XML, every seller-written string passed
through the domain XML escaper, content type
`application/atom+xml`. (The route is `/feed` on the
store, not `/posts/feed`: the router cannot place a
static segment beside `:post_slug` — the Wave 54
rule.) Entry links are path-absolute (`/<slug>/blog/
<post>`) and ids are URNs, because the platform has
no configured public base URL anywhere — inventing
one for the feed would put wrong absolute URLs into
every reader; when a deployment gains a canonical
base URL, the feed is where it lands.

**Discovery + view counts.** `GET /v1/posts/recent`
lists live posts across active stores newest-first
(the platform blog page), and `GET /v1/follows/posts`
the same for the signed-in buyer's followed stores
(the Wave 54 follow graph). When a post becomes live
— born published with a live moment, or moved into
live by a status write — the store's followers get a
`store_new_post` notice through the Wave 54 outbox
(one funnel in `notices`, the follow's news mute
honoured, fired inside the publish transaction);
a scheduled post fires nothing, and nothing fires
when its moment later passes, because liveness is
computed at read time and there is no later write to
hook — the one gap in the notice story, stated
plainly. Post reads are counted
the Wave 58 way — aggregate-only: the public detail
page fires `POST /v1/stores/:slug/posts/:post_slug/
view`, which records one timestamped row for a LIVE
post only (anything else is the uniform 404 and
records nothing; no reader, session, or address is
ever stored), and the seller's stats
(`GET /v1/seller/store/post-stats`) report all-time
and trailing-30-day counts per post and in total.

**Moderation + export.** Platform admins get the
cross-store queue at `GET /v1/admin/content/posts`
and `/pages` (status filter + search) and audited
status writes that run the SAME state machine and
stamp rules as the seller's own actions — a takedown
is an archive, a restore is a draft, and the audit
log records every move (`/admin/content` in the web
admin). The Wave 31 seller data export now carries
the store's pages and posts in full.

**Web.** The storefront frame gains a store nav
(Home / Shop / each nav page / Blog — the Blog link
renders only when the store has at least one live
post); public pages at `/[store]/pages/[pageSlug]`,
`/[store]/blog` (tag-filterable cards with cover,
excerpt, tags, reading time) and
`/[store]/blog/[postSlug]` (full post, tag chips,
prev/next, related rail, BlogPosting JSON-LD, the
view beacon); a platform blog at `/blog` (recent
across stores, with a followed-stores view when
signed in); and the seller editors `/seller/pages`
(create/edit, nav toggle, up/down reorder, status
actions) and `/seller/blog` (editor with draft /
publish / schedule, tag input, excerpt override,
cover hotlink + upload, read statistics), linked
from the seller dashboard and the branding page.

**Honest limits:** no HTML/Markdown rich text (the
plain-text rule above is the limit, deliberately);
no comments on posts; no full-text post search on
the public surfaces (the blog filters by tag; the
seller editor searches their own titles/slugs);
scheduled posts go live by read-time computation,
so a post scheduled for later does not notify
followers when its moment arrives (publishes that
are live at write time do — see Discovery above);
the stats window is
a rolling 30 days, not a calendar month; feed entry
links are path-absolute until the platform has a
configured public base URL (above); deleting a post
deletes its view rows with it (the counts are the
post's own, not a platform archive).

## Wave 60 — gift cards: store-scoped stored value

Shopify's stored-value instrument, in the shape this
platform's money rules allow. A gift card
(`crates/api/src/gift_cards.rs`, migrations
0070–0071, domain rules in the Wave 60 section of
`crates/domain`) is a **bearer instrument**: the code
is the credential. Codes are `bdgc_` + 24
uppercase-alphanumeric characters (≈124 bits of
entropy, the Wave 37 API-key construction); only the
SHA-256 hash of the normalised code is stored, and
the full code is shown **exactly once**, in the issue
response. Lists, details, exports and receipts carry
the display prefix + last4, never the code and never
the hash. A public balance check
(`POST /v1/gift-cards/balance`) answers anyone holding
a code — that is what a bearer instrument is for —
with ONE uniform 404 for unknown or malformed codes
so it cannot enumerate; a found card reports its true
state, because its holder deserves a true answer. The
tradeoff is deliberate and is the standard gift-card
shape: the protection is the code's entropy plus
hash-only storage, not secrecy of the endpoint.

**Scope: store-scoped cards only.** A card is issued
by one store and redeems only against that store's
sub-order in a checkout. Platform-wide cards are a
documented future — store scope is what keeps the
money math exact: a redemption touches exactly one
sub-order, one commission base, one payout snapshot.

**The funding model — read this before the money.**
Issuance in this wave is **promotional / manual**: a
seller (or an admin) creates cards, and NO money is
collected at issuance. There is no purchase flow (a
buyable gift-card product is a documented future
wave). Redemption is therefore **SELLER-FUNDED**: it
behaves exactly like a store coupon on that sub-order
— commission is computed on goods − redemption and
the seller payout snapshot nets it out. The platform
never funds a redemption and never pretends purchase
money exists.

**Issuance.** `POST /v1/seller/gift-cards` (one card,
or a bulk batch with `count` 1–50 — each card its own
code, one transaction), amounts ৳10–৳50,000 (domain
caps), an optional note (≤ 200 chars) and recipient
name (≤ 80), and an expiry: explicit validity days
(1–3,650), a no-expiry flag, or the platform default —
the `gift_card_default_validity_days` platform setting
when valid, else 365 days (DB > const, the standing
precedent). Issuing is gated on the owner-level
`manage_payouts` capability: minting a card spends the
store's future earnings exactly like requesting a
payout, so managers cannot do it. Reads (list, detail
+ ledger, per-store stats at
`GET /v1/seller/gift-card-stats`) are `view_finances`
(owner + manager). Sellers disable/re-enable their own
cards; the freeze is a stored, reversible fact.
Wave 37 API keys may READ cards but issuance and
freeze/unfreeze are JWT-only, like payout requests.

**The ledger.** Every balance movement is an
append-only `gift_card_ledger` row — `issue`,
`redeem`, `cancel_restore`, `return_restore`,
`admin_adjust` — written in the SAME transaction as
the cached `balance_poisha` update (the Wave 35
loyalty shape). Tests pin that the ledger sums to the
cached balance at every step. Expiry is NOT a ledger
kind and NOT a stored status: like loyalty expiry it
is time-derived at read. The derived status vocabulary
is `active | disabled | expired | depleted`, in that
precedence (a disabled card reads disabled even past
its expiry; an expired card with money left reads
expired, not depleted).

**Checkout.** The checkout body gains an optional
`gift_card_code`. Inside the checkout transaction the
card row is locked FOR UPDATE (two concurrent
checkouts presenting the same code serialize and can
never spend one balance twice) and validated: unknown
code → the one uniform bilingual 400 (checkout is not
an enumeration oracle either); disabled / expired /
depleted → a 400 naming the reason class (the holder
is the one asking; the balance endpoint tells them the
same); a card whose store is not in the order → a
bilingual 400, never a silent ignore. The redemption
ordering is **coupon → gift card → loyalty**: the
coupon allocates first, the card applies to its
sub-order's POST-COUPON goods
(`gift_card_applicable_poisha` = min of balance and
eligible goods), and loyalty then prices against goods
net of coupon AND card — its 50% cap base and the COD
advance base both treat the card-reduced goods
consistently (pinned exactly by the wave60i test: 170
points usable where a post-coupon-only base would
have allowed 270). The funded sub-order snapshots
`gift_card_discount_poisha` (+ the card id), the order
snapshots the total and the card's last4, the receipt
shows the gift-card line, the per-seller invoice nets
the redemption out of its money block the same way
(it is a sub-order snapshot, unlike order-level
loyalty), and the courier's booking COD computation
subtracts the redemption — the courier never
collects the card-funded slice in cash. The COD
advance base is the payable net of the card too
(pinned: a card that pulls an order under the
advance threshold removes the advance entirely).

**Restores.** Cancelling an unpaid order (buyer
whole-order cancel or seller sub-order cancel)
restores the FULL redemption to the card inside the
cancel transaction, mirroring the loyalty cancel hook.
A return received on the funded sub-order restores
the proportional share —
`gift_card_restore_for_return` =
floor(redemption × refund ÷ refundable goods), never
more than was redeemed; whole-sub-order returns
restore the full redemption — and the stored CASH
refund is reduced by the restored amount: the buyer
recovers the card-funded slice as card balance, the
total recovery is unchanged, and the Wave 41 payment
settlement folds in the cash figure only, which is
all any gateway ever collected. **Restoring never
extends a card's life**: value restored to an expired
card returns to its balance but the expiry stands and
the card stays unredeemable; restoring to a disabled
card likewise returns the balance under the freeze.
Both rules are pinned by tests.

**Admin.** `GET /v1/admin/gift-cards` searches every
store's cards by code prefix, last4 or store slug;
detail carries the ledger; disable/enable and a
balance **adjust** endpoint (signed delta, never
below zero, note REQUIRED) are each audited in the
same transaction (`gift_card.disable` /
`gift_card.enable` / `gift_card.adjust`). Admin
metrics gain the platform gift-card block (counts by
derived status; issued / outstanding / redeemed /
restored totals — the outstanding figure is the live
stored-value liability). Gift cards also ride the
data exports: the seller export gains a masked
`gift_cards` section, and buyer/seller order rows
carry the redemption + last4.

**Web.** A seller page at `/seller/gift-cards`
(issue form in taka with one-time code reveal + copy,
stats strip, status-filtered list, per-card ledger
panel, disable/enable), an admin page at
`/admin/gift-cards` (search, ledger, freeze/unfreeze,
audited adjust form) plus a gift-card block on the
admin overview, a gift-card field on the checkout
page (checked through the public balance endpoint,
mirroring the coupon flow; the placed order on the
success screen shows the server's redemption), the
gift-card line on the buyer orders list and receipt,
and a public balance-check page at `/gift-cards`.

**Expiry reminders.** The worker's fourth sweep
(`sweep_gift_card_expiry_reminders`, migration 0071)
watches for the moment a card's value is about to
lapse: a card that is not disabled, still holds a
balance, and expires within 7 days earns its ISSUING
seller exactly one outbox reminder
(`gift_card_expiring` — store, masked last4, balance
at risk). The card row's `expiry_reminded_at` stamp
is written in the same transaction as the enqueue,
so the reminder is exactly-once across cycles and
racing workers; already-expired cards are not
reminded about (the moment to act has passed). The
seller gift-cards page shows the same window as a
warning strip over the unfiltered list.

**Honest limits:** no purchase flow (a buyable
gift-card product, with real money collected at
issuance and platform-held liability, is a future
wave — until then every card is seller-funded
promotion); store-scoped only (no platform-wide
cards, no cross-store redemption); one card per
checkout (Shopify allows stacking; here a second
card means a second order — the one-card shape is
what keeps the snapshot columns singular and the
restore rules exact); a lost code is unrecoverable
by design (hash-only storage — the remedy is an
audited admin adjustment onto a new card, never a
reveal); reminders go to the issuing SELLER, not the
card's holder — the platform usually does not know
who holds a bearer card, and pretending otherwise
would mean inventing a recipient.

## How to run

With a database (Wave 2 features):

```bash
export POSTGRES_PASSWORD=$(openssl rand -hex 24)   # Wave 39: no default exists
docker compose -f infra/docker-compose.yml up -d   # Postgres on :5432
cp .env.example .env                               # safe defaults; set APP_ENV=dev for local dev
DATABASE_URL=postgres://marketplace:$POSTGRES_PASSWORD@localhost:5432/marketplace \
JWT_SECRET=any-long-random-string \
APP_ENV=dev \
  cargo run -p api            # connects, runs migrations/, listens on 8080
```

Without `DATABASE_URL` the API still starts and serves the Wave 1
stateless endpoints; DB endpoints return `503 database not configured`.

Dev OTP flow (no real SMS gateway yet): with `APP_ENV=dev` (or
`SMS_DEV_MODE=true`) the API uses a log sender and the request-otp
response includes a `dev_otp` field for local testing:

```bash
curl -X POST localhost:8080/v1/auth/request-otp \
  -H 'Content-Type: application/json' -d '{"phone":"01712345678"}'
# -> {"phone":"+8801712345678","dev_otp":"123456"}   (dev mode only)
curl -X POST localhost:8080/v1/auth/verify-otp \
  -H 'Content-Type: application/json' \
  -d '{"phone":"01712345678","code":"123456"}'
# -> {"token":"<JWT>","user":{...,"role":"buyer"}}
```

Outside dev mode there is no SMS gateway configured yet, so
request-otp honestly returns 503 instead of pretending an SMS was sent.
`JWT_SECRET` must be set for any real deployment: if it is missing the
API falls back to a public dev-only secret and logs a loud warning —
and since Wave 28 the API refuses to start outside dev mode on that
fallback (or on any secret under 32 characters), so a production
process that is running at all has a real secret.

Stateless check (Wave 1):

```bash
curl localhost:8080/health
curl -X POST localhost:8080/v1/delivery-charge \
  -H 'Content-Type: application/json' \
  -d '{"zone":"OutsideDhaka","weight_kg":1,"cod_amount_poisha":0}'
```

Web:

```bash
cd apps/web
npm install
NEXT_PUBLIC_API_BASE_URL=http://localhost:8080 npm run dev   # http://localhost:3000
# Pages: / (marketplace, real product grid), /login (phone + OTP),
# /products/{id} (product detail + reviews, Wave 8), /{store}
# (storefront, same shared grid), /cart, /checkout, /orders (buyer),
# /orders/{id} (printable receipt), /seller, /seller/products
# (seller catalogue + create form, with a weight-in-grams field),
# /seller/orders (sub-orders) and /seller/import (Facebook Page
# link + post import, Wave 12); /search (search results with
# filters + pagination, Wave 15, with the shared SearchBar on
# the home page and storefronts)
```

## Deployment (Wave 20)

The deployable stack lives at the repository root:

```bash
cp .env.example .env        # then work through the checklist below
docker compose up --build   # Postgres 16 + API + worker + web
```

`docker-compose.yml` runs four services on the compose
network: **postgres** (postgres:16, a named volume, a
`pg_isready` healthcheck, and NO published port — only the
app services reach it), **api** (built from `Dockerfile.api`,
waits for a healthy database, publishes `${API_PORT:-8080}`),
**worker** (built from `Dockerfile.worker`) and **web**
(built from `apps/web/Dockerfile`, publishes 3000). The API
and worker read the same `.env` the local flow uses;
`DATABASE_URL` is the one deliberate override, rewritten by
compose to reach the `postgres` service (the `POSTGRES_USER`
/ `POSTGRES_PASSWORD` / `POSTGRES_DB` values, which can also
be set in `.env`, default to the dev database in
`.env.example`). Redis is present only as an opt-in
`future`-profile service — nothing connects to it yet, and a
plain `docker compose up` never starts it. The older
`infra/docker-compose.yml` remains the lightweight local-dev
services file (Postgres/Redis/MinIO for running cargo and
next on the host); it is not the deployment stack.

The images: `Dockerfile.api` and `Dockerfile.worker` are
multi-stage Rust builds (builder `rust:1.100-bookworm`, the
toolchain the workspace is developed with; runtime
`debian:bookworm-slim`, non-root user; the API image adds
`ca-certificates` for its outbound HTTPS calls). Migrations
need no runtime copy — the API embeds them at compile time
via `sqlx::migrate!` and applies them on startup.
`apps/web/Dockerfile` builds Next.js with
`output: "standalone"` (set in `next.config.js`) and runs the
traced server as a non-root user on node:20-alpine.
`NEXT_PUBLIC_API_BASE_URL` is a **build argument**: Next.js
inlines `NEXT_PUBLIC_*` values into the browser bundle, so it
must be the origin browsers use to reach the API, and
changing it means rebuilding the web image. CI has a
`docker` job that builds all three images on every push
(build only — nothing is pushed to a registry).

**API hardening in this wave:** `GET /health/ready` is a real
readiness probe — it runs `SELECT 1` against the pool and
answers 200 `{"status": "ready"}` only when the query
succeeds, 503 otherwise (including "no database configured");
`GET /health` stays the liveness check. Gate load balancers
and compose healthchecks on readiness, not liveness. The
Wave 1–19 permissive CORS layer is replaced by an explicit
origin list: `CORS_ORIGINS` (comma-separated) names exactly
the origins whose preflights are answered; unset, only
`http://localhost:3000` is allowed. The server shuts down
gracefully on SIGTERM/SIGINT (in-flight requests drain), and
startup logs one `config summary:` line — dev-mode flags,
how many of the 4 payment gateways and 5 couriers carry
complete credentials, SIDRA/Facebook configuration, CORS
origin count, money-policy fallbacks — with no secret values.

### Environment variables

Every variable from `.env.example`, plus the two deployment
ones. "Required in production" means the deployment is
broken or unsafe without it.

| Variable | Purpose | Required in production |
| --- | --- | --- |
| `API_PORT` | Port the API listens on (default 8080) | No (default works) |
| `CORS_ORIGINS` | Comma-separated browser origins allowed to call the API (Wave 20) | **Yes** — set to the deployed web origin; unset allows only `http://localhost:3000` |
| `DATABASE_URL` | Postgres connection string; migrations auto-apply on startup | **Yes** (compose rewrites it to the `postgres` service) |
| `REDIS_URL` | Placeholder for the worker's planned queue | No — nothing connects yet |
| `JWT_SECRET` | HMAC secret for auth tokens | **Yes** — unset falls back to a public DEV-ONLY secret (loud warning) and anyone can forge tokens; since Wave 28 the API refuses to start outside dev mode on the fallback or a <32-char secret |
| `APP_ENV` / `SMS_DEV_MODE` | Dev mode: log SMS sender + `dev_otp` in responses | **Must be off** — `APP_ENV` must not be `dev` and `SMS_DEV_MODE` must not be `true`, or OTP codes are public |
| `COMMISSION_RATE_BPS` | Startup commission fallback (admin settings override it) | No |
| `ADVANCE_PERCENT_BPS` / `ADVANCE_THRESHOLD_POISHA` | Startup COD-advance fallback (admin settings override) | No |
| `PROMOTION_PRICE_PER_DAY_POISHA` | Startup per-day sponsored-promotion price fallback (admin settings override; Wave 26) | No |
| `NEXT_PUBLIC_API_BASE_URL` | API origin baked into the browser bundle at web build time | **Yes** for the web image — the public API origin |
| `POSTGRES_USER` / `POSTGRES_PASSWORD` / `POSTGRES_DB` | Compose Postgres credentials. Wave 39 (audit CR-03): `POSTGRES_PASSWORD` is REQUIRED — there is no default, and compose refuses to start without it | **Yes** — set a real one (`openssl rand -hex 24`) |
| `S3_ENDPOINT` / `S3_BUCKET` / `S3_ACCESS_KEY` / `S3_SECRET_KEY` | Object storage placeholders | No — not wired to any code yet |
| `SMS_API_KEY` / `SMS_SENDER_ID` | Real SMS gateway credentials | Not yet — no real gateway exists (see the checklist) |
| `STEADFAST_API_KEY` / `STEADFAST_SECRET_KEY` / `STEADFAST_BASE_URL` | Steadfast merchant credentials + optional sandbox base URL | For live Steadfast bookings |
| `DEFAULT_COURIER` | Courier sellers get by default | No |
| `PATHAO_CLIENT_ID` / `PATHAO_CLIENT_SECRET` / `PATHAO_STORE_ID` / `PATHAO_BASE_URL` | Pathao merchant credentials + optional base URL | For live Pathao bookings |
| `REDX_API_KEY` / `REDX_BASE_URL` | RedX access token + optional base URL | For live RedX bookings |
| `ECOURIER_API_KEY` / `ECOURIER_USER_ID` / `ECOURIER_BASE_URL` | eCourier credentials + optional base URL | For live eCourier bookings |
| `PAPERFLY_USERNAME` / `PAPERFLY_PASSWORD` / `PAPERFLY_KEY` / `PAPERFLY_PICKUP_NAME` / `PAPERFLY_PICKUP_PHONE` / `PAPERFLY_PICKUP_ADDRESS` / `PAPERFLY_BASE_URL` | Paperfly credentials, platform pickup point + optional base URL | For live Paperfly bookings |
| `BKASH_APP_KEY` / `BKASH_APP_SECRET` | bKash merchant credentials | For bKash availability at checkout |
| `NAGAD_MERCHANT_ID` / `NAGAD_API_KEY` | Nagad merchant credentials | For Nagad availability |
| `SSLCOMMERZ_STORE_ID` / `SSLCOMMERZ_STORE_PASSWORD` | SSLCommerz store credentials | For SSLCommerz availability |
| `SHURJOPAY_USERNAME` / `SHURJOPAY_PASSWORD` | ShurjoPay credentials | For ShurjoPay availability |
| `PAYMENTS_DEV_MODE` / `PAYMENTS_DEV_SECRET` | DEV payment simulator (local/CI only). Wave 39 (audit CR-01): the secret has NO default — unset with dev mode on, an ephemeral random secret is generated per process (warned at startup) | **Must be off** — simulated payments are not real money |
| `COURIER_DEV_MODE` | DEV courier simulator (local/CI only) | **Must be off** — simulated shipments are not real bookings |
| `FACEBOOK_APP_ID` / `FACEBOOK_APP_SECRET` | Meta app credentials (presence only is read) | For the Graph API status to report configured |
| `SIDRA_API_URL` / `SIDRA_WEBHOOK_SECRET` | SIDRA bridge endpoint + shared HMAC secret (both or neither) | For the AI salesperson bridge |
| `FULFILLMENT_CONFIRM_SLA_HOURS` / `FULFILLMENT_SHIP_SLA_HOURS` | Fulfillment SLA fallbacks in hours (Wave 40; defaults 24 / 48, legal range 1–720). Admin settings `fulfillment_confirm_sla_hours` / `fulfillment_ship_sla_hours` override at read time | No — defaults are sane |

### Production checklist

- [ ] `JWT_SECRET` set to a long random value (never the
  dev fallback).
- [ ] Dev modes OFF: `APP_ENV` is not `dev`,
  `SMS_DEV_MODE=false`, `PAYMENTS_DEV_MODE=false`,
  `COURIER_DEV_MODE=false`. Verify from the startup
  `config summary:` line, which prints all three flags.
- [ ] `POSTGRES_PASSWORD` changed from the dev default.
- [ ] `CORS_ORIGINS` set to the deployed web origin, and the
  web image rebuilt with `NEXT_PUBLIC_API_BASE_URL` pointing
  at the public API origin.
- [ ] Merchant credentials filled for every courier and
  payment gateway the launch actually offers (the config
  summary counts them: x/4 gateways, x/5 couriers). Methods
  without credentials stay honestly unavailable.
- [ ] `SIDRA_API_URL` + `SIDRA_WEBHOOK_SECRET` set if the AI
  salesperson is part of the launch.
- [ ] **SMS is still dev-only — this is the biggest launch
  blocker.** There is no real SMS gateway yet: outside dev
  mode, request-otp returns a typed 503 instead of sending a
  code, so phone login cannot work in production until a
  Bangladeshi SMS gateway is wired into the `SmsSender`
  trait. Do not "fix" this by enabling dev mode — that
  publishes the OTP codes.
- [ ] TLS termination is deployment-side: neither the API
  nor the Next.js server terminates HTTPS. Put a reverse
  proxy in front, e.g. Caddy on the host:

  ```caddyfile
  marketplace.example.com {
      reverse_proxy localhost:3000
  }
  api.marketplace.example.com {
      reverse_proxy localhost:8080
  }
  ```

  (and then `CORS_ORIGINS=https://marketplace.example.com`
  and the web image built with
  `NEXT_PUBLIC_API_BASE_URL=https://api.marketplace.example.com`).

### Ops: backups & data portability (Wave 31)

- **Backups:** run `scripts/backup.sh` on a schedule (the
  script header has a cron example) — directly with
  `DATABASE_URL`, or with `--compose` against this stack.
  It keeps the newest 14 dumps in `./backups` (gitignored).
  `scripts/restore.sh` is the restore drill; CI re-proves
  the dump/restore round trip on every run. Copy the files
  OFF this host — a backup that lives only beside the
  database is not a backup. See the Wave 31 section under
  API endpoints for the full story and its honest limits.
- **Data exports:** sellers and buyers download their own
  record as JSON from `GET /v1/seller/export` /
  `GET /v1/buyer/export` (buttons on the seller dashboard
  and the buyer orders page) — the portability promise;
  exports are snapshots for the owner, not a migration
  tool.

### Honest limits

- **Backups are scripted, not managed.** Wave 31 ships
  `scripts/backup.sh` / `scripts/restore.sh` and a CI
  restore proof, but there is no managed backup service,
  no WAL archiving (no point-in-time recovery) and no
  off-site copy automation — scheduling the script and
  shipping the files somewhere safe stays on the operator.
- **One host, one replica.** No horizontal-scaling story:
  the API keeps no in-memory session state (JWT + Postgres),
  so it *could* be replicated behind a load balancer, but
  that topology is untested and the web/worker services are
  single-instance here.
- **The worker runs most, not all, of its planned jobs.**
  Every poll cycle it drains the notification outbox
  (Wave 29), polls courier statuses (Wave 30), renews
  subscriptions (Wave 36) and reaps stale notification
  claims (Wave 48). Two planned jobs are still printed at
  startup as TODO, not implemented — payout jobs and COD
  reconciliation — so payouts stay an admin-driven flow
  and COD stays reconciled by the order/payment records.
- **Live gateway calls are still not implemented** (see the
  Scope section): configured payment adapters report
  NotImplemented honestly, so "credentials set" does not yet
  mean "can charge".

## Scope — what is real, stubbed and TODO

**Real (Wave 1):** domain functions + unit tests (delivery charge, cart
split, commission, worked example); the four stateless endpoints; COD
adapter recording payment intent; the Next.js route groups, shared
components/hooks, delivery-charge widget (honest Bangla "API offline"
message when the Rust API is not running).

**Real (Wave 2):** Postgres via SQLx with migrations auto-applied on
startup (users, stores, otp_codes, products — runtime-checked queries
only, so CI compiles with no live DB); phone + OTP auth (BD phone
validation + normalization in the domain crate, hashed OTPs, JWT);
stores (creation implies seller); products CRUD + one shared listing
function for marketplace and storefront; ownership checks on every
seller mutation; an end-to-end integration test
(`crates/api/tests/wave2.rs`) that CI runs against a Postgres 16
service; Next.js typed API client (`lib/api.ts`), `useAuth` hook, real
marketplace product grid, `/login` (phone → OTP) and
`/seller/products` pages.

**Real (Wave 3):** carts (one per buyer; add/set/remove/clear, domain
quantity bounds 1..=99); split checkout in a single transaction with
stock locking, snapshots and stock decrement; orders + sub_orders +
order_items migrations; all checkout money computed by the new pure
domain function `build_order_totals` composing the Wave 1 delivery /
commission functions (unit tests include the two-seller ৳4,690 worked
example and the sub-order transition table); buyer order history;
seller sub-order list + status updates (owner-only, domain-checked
transitions); an end-to-end integration test
(`crates/api/tests/wave3.rs`, same `DATABASE_URL` gate) covering a
two-seller COD checkout outside Dhaka (grand total ৳4,690), stock
decrement, cart clearing, the insufficient-stock 409 and legal/illegal
status transitions; Next.js `useCart` hook (the only cart caller),
add-to-cart in the product grid, `/cart`, `/checkout` (per-seller
delivery preview via the shared `useDeliveryCharge` hook — the
server's totals at order time are final), `/orders` and
`/seller/orders` pages.

**Real (Wave 4):** product `weight_grams` (migration 0008; create and
PATCH accept it, default 1000; the seller product form has a grams
field); weight-based delivery everywhere — the domain sums unit weight
× quantity per seller group and bills per started kilogram (new pure
functions `validate_weight_grams` and `weight_kg_from_grams`, unit
tests include a mixed-weight group: 2 × 500 g + 1 × 2,500 g bills as
4 kg), and the checkout page preview uses the cart's real weights;
buyer order cancellation (`POST /v1/orders/{id}/cancel`, pure rule
`can_buyer_cancel`) with full stock restore in one transaction;
seller sub-order cancellation now restocks that sub-order's items in
the same transaction (shared restock helper); the buyer receipt
(`GET /v1/orders/{id}/receipt`, built by the single `build_receipt`
service function from the shared order loader, with NO commission or
payout fields) and a printable `/orders/{id}` receipt page linked from
the orders list and the checkout success screen, plus a Cancel button
on the orders page shown only while every sub-order is `placed`; an
end-to-end integration test (`crates/api/tests/wave4.rs`, same
`DATABASE_URL` gate) covering a 2 × 2,500 g checkout outside Dhaka
(delivery = the domain charge for 5 kg), receipt totals with no
commission field, buyer-cancel restock, the cancel-after-confirm 409,
and seller-cancel restocking only that sub-order's items.

**Real (Wave 5):** the online payments foundation — migration 0009
(`orders.payment_status`, `payments`, `payment_events`); the pure
domain payment transition table and exact callback-amount validation;
credential-configured gateway adapters behind the
`payment_adapter_for` factory (complete credentials →
`NotImplemented` for live calls, incomplete → `NotConfigured`); the
DEV gateway simulator with HMAC-signed callbacks (dev flag only,
`DEV-` references, labelled in API responses, UI and this README);
checkout with online methods (pending payment row inside the checkout
transaction, 503 with full rollback when a gateway is unavailable);
the signed webhook with its auditable event log, idempotent
duplicates and the one shared `apply_payment_callback` path also used
by dev-simulate; payment retry (`failed` → `pending`); order and
receipt views carrying `payment_status` and the latest payment
summary; an end-to-end integration test (`crates/api/tests/wave5.rs`,
same `DATABASE_URL` gate) covering method availability on and off,
dev bKash checkout, wrong-signature 401 with the audit row,
amount-mismatch 400, success + duplicate webhooks, failure + retry,
dev-simulate, COD staying row-free, and the production-mode 503 with
the cart left intact; the checkout page rendering the methods
endpoint (unavailable methods disabled, dev methods labelled) and
the orders/receipt pages showing payment status in Bangla.

**Real (Wave 6):** courier shipments — migration 0010 (`shipments`
with a UNIQUE one-per-sub-order rule, `shipment_events` audit
trail); the pure domain shipment transition table and the single
Steadfast raw-status mapping; the real Steadfast adapter behind the
`courier_adapter_for` factory (credential-gated `create_order` /
`status_by_cid` calls with defensive response parsing, honest
`Unsupported` cancel, local domain price estimate); the DEV courier
simulator (dev flag only, `DEV-` consignment ids, poll-by-poll
status progression, labelled in API responses, UI and this README);
seller booking for confirmed sub-orders with the sub-order moving
to `shipped` in the same transaction; tracking refresh for the
seller or the buyer that applies only legal unified-status moves
and syncs the sub-order on delivery/return; courier availability
(`GET /v1/couriers`) as the single source the seller page renders;
shipment summaries embedded in the buyer order and seller sub-order
views; an end-to-end integration test (`crates/api/tests/wave6.rs`,
same `DATABASE_URL` gate) covering availability on and off, booking
while placed (409), non-owner booking (403), the DEV booking with
its COD share, duplicate booking (409), the four-refresh progression
to delivered with the sub-order sync, stranger refresh (404), and
the production-mode 503; the seller orders page (courier select +
"কুরিয়ার বুক করুন" booking, shipment status + refresh) and the
buyer orders page (per-sub-order shipment block with Bangla status
labels + refresh).

**Real (Wave 7):** the Pathao and RedX adapters behind the same
`courier_adapter_for` factory (Pathao: client-credentials token
flow with caching, order create/track with fixture-tested parsers,
local-format recipient phone, whole-taka COD rounding; RedX:
parcel create/track with defensive parsers, weight in grams, a
required numeric `area_id` with no invented default); the pure
domain Pathao/RedX raw-status mappings on the one shared status
normalization, and the pure `choose_courier` smart-routing rule
(cheapest available, platform-priority tie-break); the estimate
endpoint (`GET /v1/couriers/estimate`) and booking with
`courier: "auto"` resolving through the one shared
`resolve_booking_courier` function, with per-courier availability
generalized to one rule; an integration test
(`crates/api/tests/wave7.rs`, same `DATABASE_URL` gate) covering
dev-mode estimates with the priority recommendation, `auto` and
named-Pathao DEV bookings, and the production-mode estimates +
`auto` 503; the seller orders page (Auto option with the routing
recommendation, per-courier আনুমানিক estimate texts priced from
the estimate endpoint).

**Real (Wave 8):** reviews and ratings — migration 0011 (`reviews`
with the one-per-buyer-per-product UNIQUE, `review_replies` with
one reply per review); the pure domain validators `validate_rating`,
`validate_review_body`, `validate_reply_body` and `mask_phone`;
delivered-only eligibility enforced by the single
`find_delivered_order_for_review` query (403 before delivery);
author-only edit/delete (404 for everyone else); store-owner-only
replies (404 for other sellers, 409 on a duplicate); the live
rating summary (`rating_avg` / `rating_count`) on the shared
product view via the one aggregate join, so list, storefront and
detail agree; an end-to-end integration test
(`crates/api/tests/wave8.rs`, same `DATABASE_URL` gate) that drives
a real Wave 6 delivery, then covers the 403, 201, 409, invalid
rating 400, summary changes across create/edit/delete, cross-buyer
edit 404, reply ownership and the masked public list; the product
detail page (`/products/{id}`) with the shared `Stars` component,
review form (the API's 403 message shown as-is), own-review
edit/delete and the owner's reply box, plus rating lines on the
shared product card and the storefront page rendering the shared
grid.

**Real (Wave 9):** seller dashboard, earnings and payout
requests — migration 0012 (`payout_accounts`, one per store;
`payouts` with the account snapshot and the
requested/approved/paid/rejected lifecycle); the pure domain
payout rules (`MIN_PAYOUT_POISHA`, `compute_available_balance`,
`is_valid_payout_status`, `can_transition_payout`,
`validate_payout_account`, `validate_payout_amount`); the single
`seller_earnings` aggregate (delivered sub-orders only, goods
minus commission) behind the dashboard, balance and payout
creation endpoints; an integration test
(`crates/api/tests/wave9.rs`, same `DATABASE_URL` gate) driving a
real delivery, then covering the no-store 404, delivered-only
earnings against a merely-confirmed order, account-first and
bad-number 400s, the below-minimum 400, a valid request moving
money from available to pending, the exceeds-balance 400, and
per-seller isolation; the seller home page (`/seller`) with money
stat cards, status counts, recent orders, the payout account
form, the payout request form (taka input converted by the shared
`parseTakaToPoisha`) and the payout history with Bangla status
labels. Payout APPROVAL and PAYMENT are not in this wave — they
are platform operations and land with the admin wave (Wave 10,
below); until then payouts stay `requested`.

**Real (Wave 10):** the platform admin — migration 0013
(`users.is_admin`, `stores.status`, `platform_settings`); the
pure domain settings rules (`validate_commission_bps`,
`parse_setting_i64`, the setting key constants, and
`validate_payout_amount_with_min`, which the Wave 9 validator
now delegates to); the ONE `require_admin` guard behind every
`/v1/admin/*` endpoint; overview metrics (GMV and commission
exclude cancelled sub-orders); store suspension enforced in the
shared product listing/detail paths and at checkout (409), with
owner access preserved; the payout processing queue completing
the Wave 9 lifecycle (approve / reject-with-note / mark-paid
over the one transition function, rejection restoring the
seller's available balance); user admin flags with the
self-change lockout; platform settings whose stored values
override the startup commission rate and minimum payout through
the two effective-value helpers checkout and payout creation
call; `GET /v1/auth/me` and `is_admin` on every user view; an
integration test (`crates/api/tests/wave10.rs`, same
`DATABASE_URL` gate) covering the 403 wall, SQL promotion,
overview numbers, the full payout lifecycle, suspension and
reactivation, the commission override changing a real checkout,
and flag promotion/demotion; the Next.js admin area
(`/admin` overview, stores, payouts, users, settings) behind
the shared `AdminShell` guard, plus an admins-only Admin link
in the marketplace nav.

**Real (Wave 11):** the eCourier and Paperfly adapters behind the
same `courier_adapter_for` factory, completing all five API
couriers (eCourier: two-header auth, order create/track with
fixture-tested defensive parsers, a required numeric `area_id`
with no invented default; Paperfly: HTTP Basic plus merchant-key
header, order placement/track with fixture-tested defensive
parsers, a required platform pickup point in config that bookings
refuse to invent); the pure domain eCourier/Paperfly raw-status
mappings on the one shared status normalization; availability,
estimates and smart routing covering both new couriers with no new
handler logic (the Wave 7 generalization — only the credential-rule
match gained two arms); an integration test
(`crates/api/tests/wave11.rs`, same `DATABASE_URL` gate) covering
an eCourier-only config reporting eCourier `live` and Paperfly
`not_configured`, the estimate endpoint pricing and recommending
the only available courier, and DEV bookings under both new names.

**Real (Wave 12):** Facebook Page linking and the post-to-product
import pipeline — migration 0014 (`stores.facebook_page_id` /
`facebook_page_name`, product provenance `source` / `source_ref`
with the partial UNIQUE index that makes re-import idempotent);
the pure domain post parser (Bengali-digit conversion, labelled
price extraction where a bare number is never a price, draft
building); the link/unlink/status endpoints (Graph API state
reported from credential PRESENCE only); preview with no writes;
the import validating every draft up front and creating the
survivors in one transaction through the shared `insert_product`
path extracted from the create endpoint; an integration test
(`crates/api/tests/wave12.rs`, same `DATABASE_URL` gate) covering
link validation, preview correctness with an unchanged product
count, the 2-created / 1-skipped import, idempotent re-import,
and unlink; the Next.js seller import page (`/seller/import`)
linked from the seller dashboard and products page.

**Real (Wave 13):** buyer <-> store messaging and the SIDRA
bridge — migration 0015 (`conversations` with per-side read
markers, `messages`, `sidra_suggestions`, and the store's
`sidra_auto_reply` mode flag); the pure domain message-body
rule and the ONE canonical SIDRA signature payload; the
find-or-create conversation service (open conversations only,
suspended stores hidden); per-side unread counts computed from
the read markers; the HMAC-verified SIDRA callback applying by
store mode (direct `ai` message, or a pending suggestion the
seller sends/dismisses); the best-effort outbound notification
on buyer messages; an integration test
(`crates/api/tests/wave13.rs`, same `DATABASE_URL` gate)
covering find-or-create idempotence, the unread lifecycle,
participant-only access, wrong-signature 401 with nothing
stored, suggestion then seller-send, dismissal, direct insert
under auto-reply, close semantics, and the unconfigured app
answering 503 while human chat keeps working; the Next.js
buyer messages pages (`/messages`, `/messages/[id]`), the
seller inbox (`/seller/inbox` with the suggestion queue and
SIDRA settings card), the shared `MessageThread` component
(10-second polling), and "মেসেজ করুন" buttons on the storefront
and product pages.

**Real (Wave 14):** coupons and promotions — migration 0016
(`coupons` with the platform/store scope split, `coupon_redemptions`,
the discount snapshot columns on `orders` and `sub_orders`); the
pure domain coupon rules (code normalization, input validation,
discount computation with the percent cap, and `allocate_discount`
with exact-sum proportional allocation); the shared rejection
rule serving both the stateless preview and checkout; checkout
applying the coupon inside its transaction under a coupon row
lock (rejection = 400, cart intact; redemption + `used_count`
committing with the order); the per-group discount and
commission-basis fields on `build_order_totals` (store coupons
move the commission basis, platform coupons do not); seller
earnings switched to the `seller_payout_poisha` snapshot sum;
an integration test (`crates/api/tests/wave14.rs`, same
`DATABASE_URL` gate) covering the capped platform percent
coupon (allocation sums, commission unchanged, redemption,
receipt), the store fixed coupon (commission on discounted
goods, earnings after delivery), min-order / expired /
usage-limit / inactive rejections with the cart intact, and
preview agreement; the Next.js checkout coupon field with the
two-step store-scope preview, the receipt discount line, and
the shared `CouponManager` component behind the seller
(`/seller/coupons`) and admin (`/admin/coupons`) pages.

**Real (Wave 15):** product search on Postgres — migration 0017
(`products.category` plus the combined-title and description
trigram indexes); the pure domain search rules
(`validate_search_params`, `validate_category`, the LIKE-pattern
escaping in `search_like_pattern` / `search_prefix_pattern`);
`GET /v1/search` (trigram + ILIKE matching, the one-expression
relevance rank, category / price / store filters composing with
AND, five sorts, strict public visibility) returning the shared
product view; `GET /v1/search/suggest` (distinct product titles
+ stores, silent below 2 characters); an integration test
(`crates/api/tests/wave15.rs`, same `DATABASE_URL` gate)
covering English + Bangla title matches, description-only
ranking below title matches, every filter, price sorts,
pagination totals and slices, draft + suspended-store
invisibility, suggestions, and the 400s; the Next.js shared
`SearchBar` (debounced suggestions) on the marketplace home and
on storefronts (store-scoped), the `/search` results page with
its filter form and pagination, and a category field on the
seller product form.

**Real (Wave 16):** public tracking and returns — migration
0018 (`return_requests`: one per sub-order, request status and
refund status as separate lifecycles, the refund snapshot
column); the pure domain return rules (`can_request_return`,
`can_transition_return`, `compute_refund_poisha`,
`validate_return_reason`, `phones_match_for_tracking`);
`GET /v1/track` (login-free, delivery phone as the credential,
every failure the same 404, purpose-built timeline view over
`shipment_events`); the buyer return create/list endpoints
plus the `return_status` summary on the buyer order view; the
seller decision endpoints (approve / reject / received — the
receipt transaction snapshots the refund, returns the
sub-order and restocks through the one shared helper); the
admin refund queue (list with the refund-status filter,
refund-done recording an off-platform refund, never twice);
the RTO fix in shipment refresh (a courier-driven `returned`
sync now restocks in the same transaction); an integration
test (`crates/api/tests/wave16.rs`, same `DATABASE_URL` gate)
covering the indistinguishable tracking 404s, the delivered
timeline with events, the 409s (before delivery, second
request, illegal moves, double refund-done), the stranger
404, the full approve → received flow with stock restored and
refund = goods, and the reject path; the Next.js public
`/track` page, the return actions + refund display on the
buyer orders page, the seller `/seller/returns` inbox and the
admin `/admin/returns` refund queue, with track/returns nav
links on the marketplace home, the checkout success screen,
the seller dashboard and the admin shell.

**Real (Wave 17):** partial advance for COD + the COD fraud
shield — migration 0019 (`orders.advance_required_poisha` /
`advance_paid_poisha` snapshots, the `advance_pending` /
`advance_paid` payment states); the pure domain rules
(`compute_advance_poisha` with the threshold/force/ceiling
rules, `assess_buyer_risk` with the risk table, the advance
setting validators and keys); the effective advance readers
in `admin` under the same DB > env > default precedence as
commission, with the admin settings view extended; the ONE
risk aggregate (`risk::buyer_risk_for_user`) feeding
checkout's force flag, the seller buyer-risk endpoint and
the seller sub-order list's `buyer_risk_level`; checkout's
three-way advance decision (enforce / 400 / flagged-proceed
degradation — see the Wave 17 section); the advance branch
inside the ONE payment callback (settlement to
`advance_paid`, failure leaves `advance_pending`, retry
restores it); the shipment booking guard (409 while
`advance_pending`) and the courier collection netting after
settlement; the receipt's advance lines and netted cash-due;
an integration test (`crates/api/tests/wave17.rs`, same
`DATABASE_URL` gate) covering below-threshold COD untouched,
the 400 without a method, the enforced advance end-to-end
(41,200 of 206,000; booking 409 then 201 collecting
164,800), the seeded high-risk buyer's forced advance on a
small order, the buyer-risk endpoint's agreement with the
seed, the admin threshold switch (with settings cleanup),
and the no-gateway degradation on a bare app; the Next.js
checkout advance warning + method picker + 400 retry flow
and success-screen advance walkthrough, the receipt advance
lines, the seller orders risk badge + advance warnings, the
seller dashboard buyer-risk lookup, and the two new admin
settings fields.

**Real (Wave 18):** storefront themes & store branding —
migration 0020 (the `theme_key` / `brand_color` / `logo_url`
/ `banner_url` / `announcement` columns on `stores`, the
existing `description` reused); the pure domain branding
validators (exact theme keys, `#RRGGBB` normalized to
uppercase, absolute http(s) image URLs ≤ 500 chars,
announcement ≤ 120 and description ≤ 1,000 Unicode
characters); `PUT /v1/seller/store/branding` on the caller's
own store under the codebase PATCH convention (absent keeps,
blank clears a nullable field, invalid is a 400 that changes
nothing), with the shared store view — public storefront
included — carrying the branding fields; an integration
test (`crates/api/tests/wave18.rs`, same `DATABASE_URL`
gate) covering the defaults, the full write + public read,
the uppercase normalization, every 400 shape, the
partial-write and blank-clear semantics, cross-seller
isolation, and the storeless 404 / anonymous 401; the
Next.js shared `StorefrontView` rendering all three themes
(classic / minimal / bold) from the one component with the
brand colour as a scoped accent, the variant-aware shared
`ProductGrid` / `ProductCard`, the seller `/seller/branding`
page (theme cards, colour picker + hex, URL previews,
char-counted announcement and description), and a branding
link on the seller dashboard.

**Real (Wave 19):** manual couriers — migration 0021 (the
`manual_courier_name` / `manual_tracking_id` columns on
`shipments`); the pure domain manual-courier validators
(canonical keys + display names, receipt-number shape);
booking with `courier = "manual"` on the Wave 6 route
through the SAME guard path and COD computation as API
bookings (the booking internals were factored so both call
one guard loader and one amount function), writing the
shipment row directly — no adapter, no network call; the
seller-entered status and receipt-number endpoints, with
the sub-order sync extracted into ONE function shared by
courier refresh and manual entry (delivery earns, return
restocks, identically); refresh on a manual shipment as an
honest no-op; the courier list and estimates carrying the
manual entries (always available, never priced, never
routed to); an integration test (`crates/api/tests/wave19.rs`,
same `DATABASE_URL` gate) covering the list/estimate shape,
the full manual walk to delivered + earnings, the return
walk + restock, every 400/403/409, the placeholder
consignment and its later replacement, public tracking, and
`auto` never choosing manual; the Next.js seller orders
page manual booking group, status buttons and receipt edit,
and the public tracking view's manual courier line.

**Real (Wave 20):** deployment readiness — `Dockerfile.api`
and `Dockerfile.worker` (multi-stage Rust 1.100 builds, slim
non-root runtimes; migrations are embedded at compile time
by `sqlx::migrate!`, so no runtime copy exists or is
needed), `apps/web/Dockerfile` (Next.js standalone output —
`output: "standalone"` in `next.config.js` — with
`NEXT_PUBLIC_API_BASE_URL` as a build argument), the root
`docker-compose.yml` deployment stack (Postgres 16 with a
healthcheck and no published port, API, worker, web, and a
profile-gated unused Redis), `.dockerignore` files, and a CI
`docker` job that builds all three images on every push;
the API hardening — `GET /health/ready` (a real `SELECT 1`
readiness probe next to the `/health` liveness check), the
permissive CORS layer replaced by the `CORS_ORIGINS` origin
list on `AppState` (default: the local Next.js dev origin),
graceful shutdown on SIGTERM/SIGINT, and the non-secret
startup `config summary:` line; an integration test
(`crates/api/tests/wave20.rs`, same `DATABASE_URL` gate)
covering readiness with and without a pool and the CORS
default/replaced origin behaviour; and the Deployment
section above (stack architecture, the full environment
table, the production checklist, the Caddy TLS example, and
the honest limits). Docker is not installed on the
development machine, so the image builds are verified by
the CI `docker` job, not locally.

**Real (Wave 21):** seller KYC & verification tiers —
migration 0022 (`stores.verified_tier` plus the
`store_verifications` application table); the domain rules
(the tier list and ranking, the BD NID validator accepting
the 10/13/17-digit issued lengths, NID masking, the
optional trade-license validator, the application
transition table — `none → pending`, `pending →
approved/rejected`, `rejected → pending`, `approved`
terminal — and the reviewer-note validator); the
`verification` API module (seller submit/own-view, the
admin review queue with masked phone and NID, the single
admin full-NID view, approve/reject in one transaction
with the tier landing on the store); the settings-gated
KYC payout gate (`kyc_payout_gate`, default off — when on,
an unverified store's payout request fails with a
verification-specific 400 before the account and balance
rules; when off, payout creation is exactly Wave 9's); an
integration test (`crates/api/tests/wave21.rs`, same
`DATABASE_URL` gate) covering the masked-everywhere rule
(the full NID asserted ABSENT from the seller view and the
admin queue JSON), the license requirement for the
`verified` tier, both tier landings on the public store
view, the reject → note → resubmit loop, the 409 on a
non-pending decision, and the gate's on/off behaviour with
its settings-row cleanup; the Next.js seller verification
page, the admin verification queue (approve basic/verified
with `verified` disabled without a trade license, reject
with a required note, full-NID reveal behind an explicit
button), the admin settings KYC gate toggle, and the
storefront/product verification badges (an unverified
store shows nothing).

**Real (Wave 22):** per-store sequential invoices —
migration 0023 (the `store_invoice_counters` table and
the `invoices` table, UNIQUE on `sub_order_id` and on
`(store_id, seq)`); the domain formatters
(`format_invoice_number` — `INV-` plus the sequence
zero-padded to six digits, growing wider past 999_999,
never truncated — and `invoice_filename`); the
`invoices` API module with ONE service function
(`get_or_issue_invoice`) behind the buyer, seller and
admin endpoints: ownership-or-404 access per actor, a 409
until the sub-order is delivered, lazy idempotent issuing
in one transaction (counter row locked `FOR UPDATE`, an
in-transaction re-check, and the unique violation as the
race backstop — a sequence is never reused and never
burned twice for one sub-order); the view assembled from
the shared order loader and store loader (no forked item
or money queries), with the money block scoped to the
sub-order's stored snapshots and a collected COD advance
allocated by the sub-order's share of the order grand
total; an integration test (`crates/api/tests/wave22.rs`,
same `DATABASE_URL` gate) covering the pre-delivery 409,
`INV-000001` with matching items/money, idempotent
re-reads (same row id), the same store's `INV-000002`, a
different store's own `INV-000001`, stranger/other-seller
404s, the admin door (403 for a non-admin), and a returned
sub-order keeping its invoice; and the Next.js printable
invoice page at /invoice/[subOrderId] (brand accent +
verification badge header, item table, money summary,
highlighted cash-due, browser print-to-PDF with the
invoice filename as the document title), linked from the
buyer orders page and the seller orders page for
delivered/returned sub-orders.

**Real (Wave 23):** inventory management & the stock
audit trail — migration 0024 (`products.low_stock_threshold`
plus the `stock_adjustments` table with a
(product, created_at) index); the domain inventory rules
(`stock_status` over an own-or-default threshold,
`validate_stock_adjustment` — non-zero delta capped at
±100_000 and the five-reason seller vocabulary,
`validate_low_stock_threshold`); the `inventory` API
module with ONE audit writer (`record_stock_adjustment`)
that checkout, the shared restock helper (now threaded
with `cancel_restock` / `return_restock` / `rto_restock`
by its callers) and the adjust-stock endpoint all record
through, each in the movement's own transaction; the
seller endpoints for the inventory overview, manual
adjustments (row-locked, below-zero refused with nothing
written), per-product history and thresholds — another
seller's product is a 404 throughout; the product view's
additive `low_stock_threshold` + computed `stock_status`;
the dashboard's `low_stock_items`; the product PATCH's
`stock` field removed so no unaudited write path remains;
an integration test (`crates/api/tests/wave23.rs`, same
`DATABASE_URL` gate) covering manual adjustments, the
refused below-zero move, checkout/cancel system rows,
threshold default/override/clear across the product view,
inventory summary and dashboard alerts, and foreign-seller
404s; and the Next.js seller inventory page at
/seller/inventory (summary cards, health badges, adjust
forms, threshold editors, expandable history) plus the
dashboard's low-stock alert strip and nav link.

**Real (Wave 24):** wishlist & recommendations —
migration 0025 (`wishlist_items`, composite primary key,
product index); the domain page-size rule
(`recommendation_limit` — default 8, clamped to 1..=24);
the `wishlist` API module (idempotent add/remove
answering with the resulting state, add gated on the one
public-visibility rule now shared as
`products::get_public_product`, the full-view list and
the lightweight ids read); the `recommend` API module
with the three documented SQL rails (related by the
+4/+2/+1 scoring rule, also-bought as order-level
co-purchase counted in distinct orders, and the
personalised category-signal rail with bought/wishlisted
exclusions and the labelled top-rated fallback); an
integration test (`crates/api/tests/wave24.rs`, same
`DATABASE_URL` gate) covering wishlist idempotency,
per-user isolation and the draft 404, the related
scoring order, symmetric cross-store also-bought with
`co_count`, and all three recommendation cases
(delivered signal, wishlist-only signal, fallback); and
the Next.js wishlist page at /wishlist, the shared
WishlistProvider + heart toggle on product cards and the
product page, the related + also-bought rails on the
product page, and the home page's personalised and
device-local recently-viewed strips (localStorage only,
never sent to the server).

**Real (Wave 25):** product Q&A — migration 0026
(`product_questions`: asker FK, answer fields on the same
row, `is_hidden` moderation flag, product index); the
domain body rules (`validate_question_body` — 3..=500
chars returning the trimmed body; `validate_answer_body`
— 1..=1000 chars); the `qna` API module (ask gated on the
shared public-visibility rule plus the not-your-own-store
400, the public list with masked asker display, owner-only
answering where re-answering corrects, the ONE
`set_question_hidden` write behind the seller-ownership
and admin guards, the seller inbox with its
answered/unanswered/all queues, and the
`count_unanswered_questions` aggregate — unanswered AND
not hidden — feeding the dashboard's new
`unanswered_questions` field); an integration test
(`crates/api/tests/wave25.rs`, same `DATABASE_URL` gate)
covering the 400/404 ask rules, the masked-asker
no-full-phone assertion, stranger-answer 404, answer +
correction, inbox splits, seller and admin hide/unhide,
and the dashboard count at every step; and the Next.js
Q&A section on the product page (ask form for signed-in
non-owners, an inbox pointer for the owner), the seller
Q&A inbox at /seller/questions (filter queues, answer /
correct forms, hide/unhide), and the dashboard's
unanswered-questions strip and nav link.

**Real (Wave 26):** sponsored product promotions —
migration 0027 (`promotions`: stored window, cost
snapshot, `active`/`cancelled` status; `seller_charges`:
the balance deduction ledger, kind `promotion`); the
domain rules (`validate_promotion_days` 1..=30,
`promotion_cost_poisha` as a checked multiply that
refuses non-positive prices, `validate_promotion_price_per_day`,
and `promotion_is_active` — the one activity rule the
SQL flag and the display status both implement); the
balance unification (the earnings aggregate is now
executor-generic `seller_earnings_on`, gains the
seller's charges term, and `available_balance_poisha`
resolves a seller's spendable balance through it —
dashboard, balance, payout creation and promotion
purchase all read the same number); the `promotions`
API module (public policy, the one-transaction purchase
under the payout-store row lock, the seller list with
derived display status, cancel-with-no-refund); the
admin setting + startup env for the per-day price; the
product view's `is_promoted` (an EXISTS in the shared
product columns) with promoted-first default/relevance
ordering in listings and search (explicit price/rating
sorts stay pure); an integration test
(`crates/api/tests/wave26.rs`, same `DATABASE_URL` gate)
covering the insufficient-balance 400, the exact 30,000
charge at the default price, overlap 409, the search
ordering pair (promoted first by default, pure by
price), the admin reprice to 5,000 for the next purchase
only, cancel-without-refund, the payout boundary at the
exact remaining balance, cross-seller 404s, and the
settings-row cleanup; and the Next.js seller promotions
page at /seller/promotions (policy + balance, per-product
promote forms with live cost preview and the no-refund
copy stated before purchase, the promotion list with
cancel), the স্পন্সরড badge on the shared ProductCard,
promote links on the seller products page, the dashboard
nav link and charges line, and the admin settings price
field.

**Real (Wave 27):** analytics — no migration (the wave
reads the existing ledger tables); the domain rules
(`analytics_days` absent = 30, clamped 7..=90;
`csv_escape` RFC 4180 quoting; `build_daily_csv` for the
export); the `analytics` API module (seller totals /
zero-filled daily series / top products / status +
courier breakdowns over Asia/Dhaka day buckets, the CSV
export of the same series, and the admin platform view
behind `require_admin` — GMV = goods − discount +
delivery over non-cancelled slices, commission over
delivered slices only — every metric defined in the
module doc and the Wave 27 section above); an
integration test (`crates/api/tests/wave27.rs`, same
`DATABASE_URL` gate) covering seller totals against
known delivered/cancelled flows, the zero-filled series
and its Dhaka-today bucket, top-product order, seller
isolation, the window clamps, the CSV header + today's
row against the JSON series, admin GMV / commission /
top stores, and the non-admin 403; and the Next.js
seller analytics page at /seller/analytics (days
selector, totals cards, CSS daily bars, top products,
breakdowns, CSV download) and admin analytics page at
/admin/analytics (platform cards, daily GMV bars, top
stores, courier breakdown), with nav links in the
seller dashboard and the admin shell, and the shared
DailyBars component (no chart library).

**Real (Wave 28):** security hardening + the admin
audit log — migration 0028 (`otp_request_log`, the
append-only count source for the throttle, and
`admin_audit_log`); the domain OTP cap
(`OTP_REQUEST_LIMIT_PER_HOUR` = 20, evidence-based —
see the Wave 28 section — with `otp_request_allowed`);
the per-phone 429 throttle inside the request-otp
service; the security-header middleware and 1 MiB
body cap in `build_app`; `validate_jwt_secret`
(api-local, unit-tested) enforced as a hard startup
stop in `run` outside dev mode; the `audit` module
(one writer, executor-generic so audited actions pass
their transaction) wired into payout transitions,
store status, settings updates, verification
decisions, admin Q&A moderation and refund
completion, plus the admin read endpoint; an
integration test (`crates/api/tests/wave28.rs`, same
`DATABASE_URL` gate) covering the header set on
/health, the 413, the throttle boundary on fresh
numbers (cap successes, then 429, another number
unaffected), the non-admin 403, and the payout /
store / settings audit rows (masked admin phone,
changed-keys-only detail, settings-row cleanup); and
the Next.js audit page at /admin/audit (action
filter, pagination, detail summaries) with the
AdminShell nav link, the api.ts client, and the web
app's own header subset in `next.config.js`.

**Real (Wave 29):** the notification outbox + a real
worker — a new workspace crate (`crates/notify`) owns
the `SmsSender` trait (moved out of `auth.rs`; the OTP
path uses it unchanged), the ONE selection rule
(`sender_from_env`: dev mode → the log sender; anything
else → the unconfigured sender, whose sends fail with
`sms_not_configured`) and the outbox (migration 0029:
`notifications` — pending/sending/sent/failed,
`attempts`, `last_error`, the body rendered at enqueue
time by the pure domain `render_body` over seven kinds
and stored rendered). Enqueue happens in the SAME
transaction as the state change at exactly one funnel
per event: checkout → `order_placed`; the seller status
PATCH, bookings and the shared shipment sync →
`sub_order_shipped` / `sub_order_delivered` /
`sub_order_cancelled` / `sub_order_returned`; the
payout transition to paid → `payout_paid`; return
approve / reject / received → `return_update`. The
worker binary connects to `DATABASE_URL` (absent →
exit 2), runs the same embedded migrations as the API,
and every `WORKER_POLL_SECONDS` (default 10) claims a
batch of 20 pending rows (`FOR UPDATE SKIP LOCKED`,
committed BEFORE any send), sends each and writes the
outcome back — `sent` + `sent_at`, or `attempts + 1`
back to `pending`, `failed` after `MAX_ATTEMPTS` (3).
Admins watch it at `GET /v1/admin/notifications` and
`/admin/notifications`. SMS is still dev-only — a real
SMS gateway remains the biggest launch blocker (the
stuck-`sending` edge this design left open was closed
in Wave 48). *(This entry was restored in Wave 50: the
Wave 29 section always existed, but its Scope summary
line was missing.)*

**Real (Wave 30):** automatic courier status polling —
the refresh core factored into `refresh_shipment_by_id`
(shared by the participant endpoint and the new
`refresh_stale_shipments` batch service), and the worker
binary calling that service in-process through an `api`
library dependency: stale, non-terminal, API-courier
shipments are refreshed oldest-first, 50 per cycle, with
per-shipment failures counted in a `PollReport` and
never fatal. Manual and terminal (`delivered` /
`returned` / `cancelled`) shipments are never polled.
An integration test (`crates/api/tests/wave30.rs`, same
`DATABASE_URL` gate) covers the dev-courier progression
to delivered with its notification, the staleness
window, and the terminal/manual exclusions at exact
counts. No new endpoints and no web changes.

**Real (Wave 31):** backups + data portability —
`scripts/backup.sh` (direct via `DATABASE_URL` or
`--compose` through the stack's postgres container;
gzipped dump + SHA-256 sidecar, newest-14 retention) and
`scripts/restore.sh` (checksum verify, confirmation,
schema-replacing load with `ON_ERROR_STOP`, table-count
smoke check); a CI step in the rust job that dumps the
test database and restores it into a scratch database on
every run, asserting the schema round-trips; and the two
export endpoints (`GET /v1/seller/export`,
`GET /v1/buyer/export`) returning the caller's own record
as one downloadable JSON document (domain
`export_filename`; seller: store + full catalogue +
newest 1_000 sub-orders with items + payouts + coupons +
promotions + charges total; buyer: profile with the own
phone in full + newest 1_000 orders nested + reviews +
wishlist + questions), with download buttons on the
seller dashboard and buyer orders page. An integration
test (`crates/api/tests/wave31.rs`, same `DATABASE_URL`
gate) covers both documents' contents, cross-seller
isolation, the storeless 404 and the anonymous 401s.

**Real (Wave 32):** the performance pass — migration
0030 adds the four indexes the query inventory lacked
(order_items by product, shipments by status +
updated_at, sub_orders by store + status, product
questions by product + hidden; the full pre-existing
inventory is in the Wave 32 section and the gaps only
were filled), gzip response compression via
tower-http's `CompressionLayer` on the pinned 0.5.2
(new transitive crates only, no version changes),
`Cache-Control: public, max-age=30` on the anonymous
catalogue reads (product list, product detail, search)
set at the handler — never on authenticated responses,
whose catalogue payload is viewer-dependent — and the
next/image remote allowlist for seller hotlinks in
`next.config.js` (no component converted: products have
no image field yet). No payload changed. An integration
test (`crates/api/tests/wave32.rs`; the gzip half needs
no database, the index/cache half keeps the
`DATABASE_URL` gate) asserts the four indexes in
`pg_indexes`, the cache header on anonymous reads and
its absence on authenticated ones, and gzip negotiation
with the gzip magic bytes on the body. Nothing was
benchmarked — no load-test numbers are claimed.

**Real (Wave 33):** product variants — migration 0031
(`product_variants`, `cart_items.variant_id` with the
NULLS NOT DISTINCT line-uniqueness index,
`order_items.variant_id` + `variant_name` snapshots,
`stock_adjustments.variant_id`); the domain price rule
(base + delta, final price must stay positive at create,
reprice, base-price PATCH and checkout) and the stock
semantics switch (a product with active variants reads,
sells and restocks from variant stock; the product
view's stock is the active-variant sum computed in
`PRODUCT_COLUMNS`); seller variant CRUD + per-variant
audited adjust-stock (soft delete only); cart lines
identified by (product, variant) and priced at the
final price; checkout decrementing variant rows with
variant-labelled audit rows, and the shared restock
helper restoring to the bought variant. Web: the
product page variant picker, variant names on cart and
order lines, and the seller variant manager. An
integration test (`crates/api/tests/wave33.rs`, same
`DATABASE_URL` gate) covers the validation refusals,
the detail/list shapes, the cart rules, the checkout
stock split, the 409, cancel restock, adjust-stock
fidelity, and the variant-less Wave 9 totals
(445_000 + 12_000 = 457_000) unchanged.

**Real (Wave 34):** multi-store team access — migration
0032 (`store_members`); the pure domain permission matrix
(`store_role_can` over eight actions × owner / manager /
staff, failing closed) and member-role validation; the
one access helper (`crates/api/src/access.rs`) resolving
a caller's role per store and scoping the seller surface
by family (catalogue, orders, Q&A, marketing, store
settings, finances) with each module's historical
no-access answer preserved for owners and strangers;
member management by phone (existing accounts only);
the optional `?store_id=` scope on caller-store surfaces
including the any-role store dashboard; the payout store
staying owner-only. Web: the dashboard store switcher +
role badge and the /seller/members team page. An
integration test (`crates/api/tests/wave34.rs`, same
`DATABASE_URL` gate) covers seating (201 / duplicate 409
/ ghost 404 / self 400), the manager's catalogue + order
powers and payout/member refusals (403), staff shipment
booking against coupon + balance refusals (403), stranger
invisibility, role change and removal ending access, and
the Wave 9 owner money flow (184_000 earned) unchanged.

**Real (Wave 35):** buyer loyalty points — migration
0033 (the append-only `loyalty_points` ledger with its
earn-once partial unique index; loyalty snapshots on
orders and sub_orders); the pure domain rules (earn 1
point per 100 taka of net goods, redeem 1 point = 100
poisha capped at half the post-coupon goods, 365-day
expiry computed at read); the one balance aggregate and
the earn / cancel-refund / return-clawback hooks in
`crates/api/src/loyalty.rs`, wired into the two delivery
funnels, both cancel paths and the return-received path;
checkout redemption clamped under the buyer's user-row
lock with the stored grand total as the reduced payable
(advance, online amount, receipt and courier collection
all read it) and seller payouts untouched (platform
funded). Web: the checkout points block, the orders-page
loyalty card, and loyalty lines on the order and receipt
views. An integration test (`crates/api/tests/wave35.rs`,
same `DATABASE_URL` gate) covers both earn funnels,
no-double-earn, the redemption maths + cap, the
platform-funded payout proof, the advance reading the
reduced payable, the cancel refund, the return clawback,
the courier collection netting and the full ledger order.

**Real (Wave 36):** seller subscription plans —
migration 0034 (`store_subscriptions`, one row per
store; the `seller_charges` kind CHECK widened to
`('promotion','subscription')`); the pure domain plan
rules (keys, commission discounts 0/100/200 bps,
product caps 50/500/unlimited, 30-day period) with
prices and the free cap as platform settings (DB > env
> const); subscribing prepaid from the earnings balance
through the one shared balance function in
`crates/api/src/subscriptions.rs` (owner-only,
payout-store lock, charge + upsert in one transaction,
switch = full price no proration, free switch =
immediate cancel, cancel = renewal off only); per-store
commission resolution at checkout via the additive
per-group rate in `build_order_totals`; the product cap
enforced at create (403, all product rows counted); the
worker-driven renewal sweep extending from the old
expiry or expiring to free. Web: /seller/subscription,
the dashboard plan badge and the seller nav link. An
integration test (`crates/api/tests/wave36.rs`, same
`DATABASE_URL` gate) covers the catalogue + free
default, subscribe-from-balance (184_000 -> 134_100
after the 49_900 charge), the 700 bps delivered earning
(186_000), the tuned free cap (2 -> third create 403,
pro unaffected), the manager's subscribe refusal (403),
cancel-then-resubscribe, and the sweep renewing from
the old expiry (a third charge) then expiring the store
back to full commission when the price outruns the
balance.

**Real (Wave 37):** third-party / mobile API keys —
migration 0035 (`api_keys`: owner, name, display prefix,
UNIQUE key hash, comma-joined canonical scopes,
last-used / expiry / revocation timestamps; only the
SHA-256 hash of a key is ever stored); the pure domain
rules (name, scope vocabulary + normalisation, the
scope permission rule with write implying read, the
365-day expiry bound); the one central key
authentication in `crates/api/src/api_keys.rs`, wired
into the `AuthUser` extractor (hash lookup → JWT-only
route exclusions → scope rule → per-key fixed-window
rate limit, default 600/min from
`API_KEY_RATE_LIMIT_PER_MIN`, JWT traffic unlimited);
management endpoints that are JWT-only by that same
central rule, with the full key shown exactly once at
creation. Web: /seller/api-keys with the one-time
reveal panel, and the seller nav link. An integration
test (`crates/api/tests/wave37.rs`, same
`DATABASE_URL` gate) covers the one-time reveal and
prefix, key auth acting as the owner, the read-only
write refusal, a write key writing, all five exclusion
families, unknown / revoked / expired 401s, the
stranger-revoke 404 and idempotent re-revoke, the list
leaking neither key nor hash, and the per-key 429 with
JWT traffic unaffected; the limiter and route rules
also have unit tests in the module.

**Real (Wave 38):** the PWA shell — `app/manifest.ts`
(Bangla name, standalone display, brand-green theme);
four real PNG icons generated pixel-by-pixel by
`apps/web/scripts/make-icons.mjs` (dependency-free,
`zlib` only: 192 + 512 "any", 512 maskable inside the
safe zone, 180 apple-touch); the hand-written
`public/sw.js` (versioned caches; cache-first statics,
network-first navigations + public catalogue GETs,
an explicit never-cache deny list for every
authenticated/mutating prefix, /offline fallback,
production-only registration via
`components/PwaRegister`); `/offline`, a static Bangla
page; the shared `components/InstallAppButton`
(beforeinstallprompt on Chromium, Share-menu hint on
iOS Safari, hidden when installed) in the home nav and
the orders header; layout metadata (icons, appleWebApp,
viewport themeColor); and the dependency-free
`scripts/check-pwa.mjs` guard wired into the CI web
job. No API change, no migration. Offline is a
read-only catalogue shell by design — see the Wave 38
section for the caching table and the honest limits.
**Real (Wave 39):** the security-fix wave — no new
endpoints and no migration; behaviour changes only
where the 2026-11-27 audit (PR #1) proved a defect.
jsonwebtoken 10.4.0 (CVE-2026-25537); no public DEV
payment secret anywhere (env or ephemeral, never a
default) and DEV callbacks unverifiable with dev mode
off; compose files with required (defaultless)
database/MinIO credentials and safe `.env.example`
dev-mode defaults; payout creation serialized per
store in one transaction plus an approve-time balance
re-check; payment settlement and retry atomic in one
locked transaction each, with a cancelled-order guard
and the buyer cancel voiding pending payments; the
OTP attempt cap as one conditional UPDATE; paid /
advance-paid cancels blocked with a bilingual 409
(nothing restocked); courier booking amounts net of
the coupon discount (booking == receipt); the domain
money boundary returning errors instead of panicking
or wrapping (checked arithmetic throughout, release
overflow checks on); RedX/eCourier/Paperfly create
parsers rejecting failure-verdict bodies that carry
ids; and the `ci_gate` test that makes a CI run
without `DATABASE_URL` fail instead of silently
skipping. An integration test
(`crates/api/tests/wave39.rs`, same `DATABASE_URL`
gate) covers the forged-secret 401, the dev-mode-off
401, the OTP cap, the concurrent payout race and the
approve re-check, all three paid-cancel blocks plus
the COD regression, the late-webhook no-op, and the
coupon booking == receipt equality. The audit items
NOT addressed are listed as deferred, with reasons,
in the Wave 39 section above — the backlog stays
visible, never silently dropped.
**Real (Wave 40):** the seller fulfillment center — the
work queue (`GET /v1/seller/fulfillment`, domain bucket
classification + SLA ages + exact counts, FIFO 200,
7-day Done retention), bulk booking
(`POST /v1/seller/fulfillment/bulk-book`, per-item
outcomes through the factored single-booking dispatch),
label data (`GET /v1/seller/fulfillment/labels`) with the
printable `/seller/fulfillment/labels` view, the
`/seller/fulfillment` queue page (bucket tabs, breach
badges, bulk-book panel), and the dashboard fulfillment
card fed by the same counts function. No migration: the
queue is a view over existing statuses, and the SLA pair
rides the existing settings precedence. See the Wave 40
section for the queue model, the one-clock age rule and
the limits.
**Real (Wave 41):** security hardening tranche 2 — no
new endpoints; behaviour changes only where the audit
proved a defect, plus two migrations. Settlement now
re-derives the expected amount from the locked order
(online = grand total; COD = only its recognised
advance) and rejects a drifted payment row 400 with
nothing moved (HI-51); `gateway_ref` is unique and at
most one `pending` payment exists per order (0036),
the webhook lookup is exact, and payment creation
answers a collision with a typed 409 (HI-53); the OTP
request path is one per-phone-locked transaction —
count check, throttle log, code replacement — with
the SMS sent after commit (HI-04/F04 + HI-06/F06);
refunds reach the payment record: refund-done
accumulates `refund_amount_poisha` on the order's
payment and flips payment + order to `refunded`
(stamping `refunded_at`, 0037) when every sub-order's
refund is done (HI-52 — the record layer only; live
gateway refund calls stay unwired, every adapter's
refund still errors honestly); and CI gains a
NON-GATING `security-audit` job (cargo audit on every
run, A14-15). An integration test
(`crates/api/tests/wave41.rs`, same `DATABASE_URL`
gate) covers the tampered-row rejections (full +
advance), the uniqueness conflicts, the six-way OTP
race at the cap boundary, and the partial-then-complete
refund flip with its COD regression.
**Real (Wave 42):** dependency hygiene — no new
endpoints and no behaviour changes; declarations,
audit config and docs only. Every sqlx dependency
(api, worker, notify + notify's dev-dependency) now
sets `default-features = false` with `macros`
restored explicitly (the defaults' `any` member kept
the MySQL/SQLite drivers — and libsqlite3-sys's
bundled-SQLite CVE, RUSTSEC-2026-0459 — in the graph;
`json` provably unexercised, the jsonb text-cast
pattern; `any` unused), so the SQLite/MySQL driver
crates leave the umbrella's feature graph. What CI
then proved, across three diagnostic runs: the
advisory survived both the diet and a from-scratch
lock resolve, and `cargo tree -i` misleadingly
printed "nothing to print" — the decisive evidence
was the generated lock's own parent records, now
printed by the audit job on every run:
`libsqlite3-sys` ← `sqlx-sqlite` ← `sqlx` AND
`sqlx-macros-core`. sqlx 0.8's migration-macro
support welds all three database drivers into the
macros crate, so the drivers remain in the resolved
graph for as long as `sqlx::migrate!()` is used —
on the compile-time (proc-macro) side only: the
shipped api/worker binaries enable postgres features
only, SQLite is not linked into them, and no code
path ever opens a SQLite database (the CVE requires
opening an attacker-crafted DB + WAL pair). The
disposition is therefore the third accepted-risk
ignore in `.cargo/audit.toml`, with the mechanism
and removal triggers (a sqlx release unwelding the
drivers, dropping `migrate!`, or any real SQLite
use) written next to it. The audit job also resolves
its lock from scratch (`rm -f Cargo.lock` first), so
it always scans a current resolution; a fresh
resolve may pick newer semver-compatible versions
than the committed lock for some crates, acceptable
for this non-gating advisory job. Final audit
state: 0 unignored vulnerabilities (2 HS256-only
ignores + this compile-time-only one), chrono
unmaintained remains an informational warning.
`.cargo/audit.toml` records exactly two accepted-risk
ignores — RUSTSEC-2023-0071 (rsa, via jsonwebtoken's
rust_crypto; unbumpable, its manifest pins ^0.9.6)
and RUSTSEC-2026-0387 (crypto-bigint, via p256/p384)
— each justified by the platform's HS256-only JWT use
(only `from_secret` keys exist in the tree) and each
carrying its removal trigger. chrono's unmaintained
warning (RUSTSEC-2026-0543) is noted, with the
time/jiff migration queued as its own refactor wave.
See the Wave 42 section for the full triage.

**Real (Wave 43):** observability — an `X-Request-Id`
on every response (a valid inbound id honoured under
the domain's printable-ASCII rule, anything else
replaced by a minted UUID), one structured request
log line per request (route pattern, status, latency;
never query strings, headers or bodies), a
point-in-time admin health snapshot at
`GET /v1/admin/metrics` (today's orders + Wave 27
GMV, orders by status, the pending payout queue,
open returns, the outbox backlog, stale shipments,
catalogue totals) shown as the admin page's
system-status card, and a per-cycle summary line in
the worker. See the Wave 43 section for the
definitions and the honest limits (stdio only — no
exporter, no aggregation, no time series).

**Real (Wave 44):** payout idempotency — an optional
`Idempotency-Key` on `POST /v1/seller/payouts`
(validated by the domain rule, stored on the row,
unique per store via a partial index): same key +
same amount returns the original payout with 200,
same key + different amount is a 409, no key is the
unchanged pre-Wave-44 behaviour; the seller form
mints one key per session and rotates it only after
a success. This closes the deferred third element
of the audit's payout finding (CR-10). See the
Wave 44 section for the contract and its limits
(per-store scope, no key TTL, payouts only).

**Real (Wave 45):** supply-chain & CI maturity — no
new endpoints and no behaviour changes; CI config
and docs only. The `security-audit` job is GATING:
its `continue-on-error` is dropped and the
"(non-gating)" name with it, so a new unignored
RustSec advisory fails the run — the flip the
Wave 41/42 sections promised, landed on the tree
Wave 42 cleaned (0 unignored vulnerabilities, the
three accepted-risk ignores on record in
`.cargo/audit.toml`). The job still runs in parallel
(no `needs`), still resolves a fresh lock in-job and
still prints the lock-parent diagnostic. And
`.github/dependabot.yml` automates the update
proposals that keep the tree current: cargo and npm
weekly with minor/patch grouped and majors
individual, github-actions monthly — proposals only,
merged solely on a fully green CI (gating audit
included) after a human look, with changelog reads
for security-relevant cargo majors. See the Wave 45
section for the contract and the honest limits
(Rust-only gate, ignore discipline, red-main
pressure by design).

**Real (Wave 46):** a load smoke, not a benchmark —
one new integration test (`wave46.rs`) and no
production changes. It seeds a store with twelve
active products and fires a fixed burst of 200
anonymous reads (8 workers x 25, round-robin, 50
each) at the product list, product detail, search
and storefront routes, asserting all-200 responses,
exact per-route counts, and a generous 120-second
pathology ceiling; per-route latencies are printed
to the CI log, never asserted. It proves the public
read paths survive concurrency without errors or
pathological slowdowns at seed scale — production
capacity and real load behaviour remain unmeasured
and need a deployed environment. See the Wave 46
section.

**Real (Wave 47):** jsonwebtoken 12 — the auth
crate moves from 10.4.0 to 12.0.0 (Dependabot PR
#13's bump, landed as a wave; the PR is closed as
superseded). The Wave 39 feature discipline is
unchanged (`default-features = false`,
`rust_crypto` only — verified against the v12
manifest), and the only call-site adaptation is
v12's Result-returning `from_secret` constructors
at the two sites in auth.rs; JWT semantics
(HS256, same claims, 7-day expiry) are identical.
The crypto-bigint audit ignore is retired (0.7.11
is the only resolved version, past the patched
line); the rsa ignore stays with a rewritten
justification — jsonwebtoken now resolves the
patched rsa 0.10.0, but rsa 0.9.10 remains in the
resolved graph via sqlx-mysql's welded driver,
compile-time only. See the Wave 47 section.

**Real (Wave 48):** the notification reaper — the
Wave 29 outbox's stuck-`sending` edge is closed.
Migration 0039 adds `claimed_at` (the claim stamps
it, outcome write-backs clear it; the table had no
claim timestamp at all, and `created_at` is the
event's age, not the claim's). Each worker cycle
first runs `notify::reap_stale_sending`: a
`sending` row whose claim is older than
`NOTIFICATION_SENDING_STALE_MINUTES` (default 10,
env > const in the worker — the notify crate reads
no platform settings) returns to `pending`, or to
`failed` when the lost claim spends its last
attempt (a reap consumes one attempt, since the
claim path itself does not increment `attempts` —
only recorded send failures do — and a
crash-looping row must not cycle forever). Reaped
rows carry a `reaped: …` note in `last_error`, and
the counts ride the worker's summary line
(`reaped_pending` / `reaped_failed`). At-least-once
is unchanged: a false reap could only duplicate an
SMS, never lose one. See the Wave 48 section.

**Real (Wave 49):** the launch runbook —
`docs/LAUNCH.md` consolidates 48 waves of honest
limits, onboarding-confirm items and owner decisions
into one operator document: the decisions owed
(CR-15, commission/minimum-payout defaults, the
placeholder name), the credentials checklist (SMS
gateway first — the one launch-blocking integration,
a single `SmsSender` implementation in the `notify`
crate), the production configuration checklist, the
go-live sequence with its smoke path, the
known-limits shortlist, and the signals guide.
Docs only; every claim in it traces to this README,
`.env.example`, the audit doc or the code. See the
Wave 49 section.

**Real (Wave 50):** program complete — docs only. The
closing consolidation: the 50-wave arc (1–10 core
commerce, 11–20 Bangladesh growth, 21–36 scale &
trust, 37–45 third-party API + the security-audit
response + supply-chain maturity, 46–49 the load
smoke, jsonwebtoken 12, the notification reaper and
the launch runbook) and the final counted state —
39 migrations, 6 Rust crates + the Next.js app, 44
API integration test files, 4 CI jobs with cargo
audit gating since Wave 45 — every wave's verdict
verified from GitHub Actions at job/step level,
twice, under the project's CI-only rule. "Done"
means feature-complete per the program and
launch-ready per `docs/LAUNCH.md`; it does not mean
live — going live still needs the runbook's
credentials and owner decisions, the SMS gateway
first. Post-50 work is named as future, not queued:
the remaining Dependabot majors, CR-13 test
isolation, a deployed-environment load test, the
remaining audit backlog, a frontend audit. This wave
also restores the summary's missing **Real (Wave 29)**
entry above and corrects the stale current-state
framing in the Monorepo layout, the Deployment
Honest limits and Verification status. See the
Wave 50 section.

**Real (Wave 51):** the complete product media system —
migration 0040 `product_images` (+ 0041 review photo
columns), pure domain image rules (sniff, caps, derived
planner, order normalisation, key safety), the
`MediaStore` trait with the local-disk driver (default,
served at `/media/*` immutable) and the hand-rolled
SigV4 S3 driver (honest `media_not_configured` until
credentials exist), seller upload/attach/list/batch/
delete endpoints behind `manage_catalog`, `image_url`
on every product JSON + cart lines, the detail gallery,
variant images, Facebook import photo wiring that never
fails an import, the media library + bulk delete,
branding uploads, review photos, admin media metrics,
and the web gallery/uploader/card wiring with the PWA
cache-first `/media/` rule — plus the same-wave
extensions: EXIF orientation, the duplicate-hash guard,
the per-store quota, replace/bulk/from-web (SSRF-
guarded) uploads, the orphan reaper, admin moderation,
order-line thumbnails, settings-driven media limits,
original filenames + alt suggestions, bulk URL attach,
S3 presigned URLs, hover second-image, quality
warnings, and Product JSON-LD. See the Wave 51 section.

**Real (Wave 53):** the faceted browse — `GET /v1/search`
reworked (repeatable category/store filters,
effective-price bounds via the SQL twin of the flash
resolution, rating floor, stock/sale flags, the `sold`
sort, `limit` paging, and the exclude-own-group facet
block), suggest v2 (effective price + image + category),
`GET /v1/categories` from the extracted shared
aggregate, the products list's sorts, saved searches
(migration 0048 + buyer CRUD + page), the identifier-free
search analytics log (migration 0049 + admin read +
overview card), the /browse page with the shared
FilterPanel/BrowseResults, the suggest-v2 SearchBar
dropdown, and the snapshot-based recently-viewed rails.
See the Wave 53 section.

**Real (Wave 54):** store follows (migration 0051,
follow/unfollow + public counts + `/v1/follows` +
seller followers + admin follower counts + buyer-export
section), the `store_new_product` outbox notification on
every publish funnel (create-active, draft-to-active,
Facebook import), store profile texts (migration 0052
`about`/`return_policy` + branding UI + storefront
sections), the public store aggregates (rating / sold /
products), rating summaries with floored distributions
(product + store), review sorts, the store all-reviews
list, the delivery estimate endpoint (Wave 1 charges +
settings-driven day ranges, admin-editable), and the
product page v2 (breadcrumbs + JSON-LD, delivery block,
variant thumbnails + deltas, photo strip, share row,
buy-now, sticky buy bar, same-store rail) with the
/following page — plus the extension scope: seller
store collections (migration 0053 + seller editor +
storefront shelves + public shelf pages), the follows
feed + store suggestions + seller follower statistics,
and the buyer's notification inbox (migration 0054
read state) — plus the second extension round: review
helpful votes (migration 0055 + `helpful` sort), the
per-follow notice mute (migration 0056), `store_flash_sale`
follower alerts on live-sale item changes, the Q&A
answered-first sort + unanswered count, the admin
per-store follower list, the analytics community totals,
the header notification bell, and the cart-page delivery
estimate. See the Wave 54 section.

**Real (Wave 55):** the buyer address book (migration
0057, CRUD + one-default rules + 10-address cap +
ownership 404s + `last_used_at` stamped by checkout),
the 64-district table with its zone policy (Dhaka
inside; the five Dhaka-bordering districts suburb;
Tangail outside — stated in the Wave 55 section) behind
public `GET /v1/districts`, checkout's address path
(server-side fields + zone from the saved address) and
district path (zone derived), the order note (migration
0058, ≤ 500 chars) on the buyer, seller, fulfillment
and label surfaces, save-for-later (migration 0059,
save/unsave endpoints, checkout- and clear-proof) with
the stock-capped quantity write, reorder at current
prices with the per-line skip report, order + sub-order
timelines on the buyer order, receipt and public
tracking views (timestamps only where recorded), the
/districts-driven checkout form with address picker and
delivery estimate, the /addresses page, and the admin
orders-by-district aggregate. Extensions: API-computed
tracking `steps` on every sub-order view, the seller
timeline, shelf bulk move-all/clear, the buy-again rail
(`GET /v1/buyer/buy-again`), single-address GET +
`GET /v1/buyer/default-address`, district search
(`?q=`), invoice district + note, the export's
`addresses` section, admin orders-by-zone + address-book
stats, and the shared DistrictSelect. See the Wave 55
section.

**Real (Wave 56):** the managed category taxonomy —
migration 0060 (`categories` tree, depth ≤ 3, tree-wide
unique slugs, icon keys, featured flag) +
`products.category_id`, the seeded Bangladesh taxonomy
(migration 0061, 11 roots / 36 children / 6
grandchildren, Bangla + English) and the exact-match
backfill (migration 0062); domain tree rules (cycle +
depth guards, display order, orphan promotion, delete
guard, link resolution); the audited admin API (CRUD,
bulk reorder, merge with label re-sync, subtree
drill-in, slug-keyed import/export validated before
writing) and /admin/categories; product linking through
one resolution rule (explicit id syncs the label, text
resolves exactly, no match stays free-text) with
`category_id` + `category_path` on product views; the
additive public contract (`GET /v1/categories` keeps
the Wave 53 aggregate and adds `tree`, `?flat=1` is the
old shape, `/v1/categories/tree`, `/v1/categories/{slug}`,
home feed `taxonomy` beside the untouched tiles);
subtree browse via `category_slug` on search + product
lists with the Wave 53 facet convention intact;
taxonomy suggestion nodes; store specialisation
(`GET /v1/stores/{slug}/categories`); the seller
CategoryPicker + subtree filter, taxonomy landings,
taxonomy-first /categories, the subtree product list
endpoint, the marketplace category nav bar with its
mega panel, and in-store category landings. See the
Wave 56 section.

**Real (Wave 57):** seller bulk catalogue tools —
a dependency-free domain CSV parser/writer plus the
bulk row rules (one column contract, the shared product
validators, per-row error accumulation, caps 500/200);
the bulk import (`POST /v1/seller/products/bulk-import`,
dry-run mode, sku→source_ref idempotency with
update-on-reimport, the shared plan-cap gate in
partial-apply form, the shared follower-notice funnel
for born-active creations only, stock moves through
the Wave 23 writer, per-row Wave 56 category
resolution, best-effort Wave 51 image-URL attach);
import history (migration 0063) with re-openable
reports and a generated example template; the bulk
edit (price set/percent/delta with checked poisha
math floored at 1, stock only through the audited
adjust path, status + category through the shared
update path, per-item outcomes); the round-trip
catalogue export in the import's own format; the
`worker seed-demo` subcommand (DEMO_SEED=1-guarded)
building an 8-store / 161-product demo catalogue
through the same services with generated placeholder
images; and the /seller/bulk page. See the Wave 57
section.

**Real (Wave 58):** the storefront section editor —
a nine-type section catalogue in the domain crate
(hero, rich text, product rails incl. hand-picked,
collection shelves, category grids, the catalogue
grid, image banners, store info, FAQ) with per-type
config caps and bilingual, position-naming validation;
one JSONB layout document per store (migration 0065)
with a code-level default layout equivalent to the
pre-Wave-58 storefront (reset = delete the row);
a public resolved-layout endpoint that loads every
section's data server-side (flash-priced product
cards, shelves, counts, hero branding fallbacks) and
drops disabled / out-of-window / unknown / empty
sections; seller layout GET/PUT/reset under the
branding access rule with ownership checks on rail
and shelf references; an editor catalogue endpoint
(types, caps, presets); per-section scheduling on
the banner window rule; aggregate-only section
analytics (migration 0066) with a seller stats view
(total / Dhaka-today / trailing-30-days); the
storefront rendering sections in order through
shared components with a legacy-composition fallback;
and the /seller/storefront editor with catalogue-
driven forms and a live preview (client-resolved —
the fidelity limit is documented in the Wave 58
section). See the Wave 58 section.

**Real (Wave 59):** store content — custom pages and
a store blog (migrations 0067–0069): per-store pages
(≤25) and posts (≤500) with slugs unique per store
and kind, one draft/published/archived state machine
for both, PLAIN-TEXT bodies only (no HTML rendering
of seller input anywhere — the XSS stance is in the
Wave 59 section); scheduled publishing computed at
read time from the published_at stamp (publish sets
it, unpublish clears it, archive keeps it, and a
re-publish with a new moment is a reschedule);
normalised post tags (lowercase, hyphenated,
deduplicated, ≤8) with a public tag filter; excerpts
hand-written or derived on a word boundary with no
ellipsis appended; reading time from a write-time
word count; post covers as a validated hotlink or a
Wave 51 pipeline upload under the post's own key;
seller CRUD + exact-set page reorder under the
branding access rule (manage-marketing); public
nav/page/post reads with the uniform 404 for
anything not live, prev/next neighbours and a
shared-tag related rail; an Atom feed at the store's
/feed URL (hand-built escaped XML, path-absolute
links — the honest limit is documented); platform
blog discovery (recent across stores + followed
stores); a store_new_post follower notice fired
inside the publish transaction (scheduled posts
fire nothing — documented); aggregate-only post
view counts with a seller stats view; admin content
moderation (cross-store lists + audited status
writes on the same state machine) and pages/posts
in the seller data export; the storefront store
nav, the public page/blog surfaces with BlogPosting
JSON-LD, and the /seller/pages + /seller/blog
editors. See the Wave 59 section.

**Real (Wave 60):** gift cards — store-scoped bearer
stored value (migrations 0070–0071): `bdgc_` codes hashed
with SHA-256 and shown exactly once at issuance,
an append-only ledger behind a cached balance
(pinned to sum at every step), promotional
SELLER-FUNDED issuance with no purchase flow (the
funding model is stated wherever it matters);
seller issue (single + bulk) / list / detail /
stats / disable / enable under the deliberate
access split (issue + freeze are owner-level
manage_payouts, reads are view_finances), a public
balance check with a uniform unknown-code 404, and
audited admin search / disable / enable / adjust;
checkout redemption ordered coupon → gift card →
loyalty with the card locked FOR UPDATE in the
checkout transaction, the sub-order + order
snapshots, the receipt line, the invoice money
block, the courier COD and the COD advance base all
netting the redemption out; full restore on cancel
(both funnels) and proportional restore on
return-received with the cash refund netted by the
restored amount; restoring never extends a card's
life; and a worker expiry-reminder sweep
(exactly-once, stamped on the card row) warning the
issuing seller a week before a balance lapses.
Seller + admin gift-card pages, the checkout
gift-card field, the public /gift-cards balance
page, gift-card lines on buyer order views and the
invoice page, the admin metrics gift-card block,
and gift cards in the data exports. See the Wave 60
section.

**Stubbed (typed errors — never fake success, no network calls):**
none among the couriers — all five API couriers have real
credential-gated adapters, and the four manual couriers run
the Wave 19 seller-driven workflow (they have no APIs to
adapt, and none is faked). SMS sending has only the
dev-mode log implementation in the `notify` crate since
Wave 29 (the unconfigured sender errors `sms_not_configured`
outside dev mode — see the Wave 29 section).
Fetching Facebook posts through the Meta Graph API is stubbed
honestly (503 without app credentials, 501 without the
page-token OAuth flow — see the Wave 12 section); the paste
import pipeline is the real source.

**TODO / not wired:** live gateway calls and real-gateway webhook
signature verification (need merchant credentials + each gateway's
integration docs), gateway refund APIs (Wave 16 refunds are
recorded off-platform obligations instead — see the Wave 16
section), Redis, a real SMS gateway,
the REMAINING worker jobs — payout jobs and COD
reconciliation; the notification outbox drain IS wired
since Wave 29 and automatic courier status polling
since Wave 30 (both call the same service functions the
API exposes).
(The SIDRA bridge contract IS wired in Wave 13 — SIDRA itself is
the external service at the other end of it, configured by
`SIDRA_API_URL` / `SIDRA_WEBHOOK_SECRET`.)

## Verification status

- **Next.js: verified in CI** — the CI `web` job runs
  `npm ci && npm run build` on every push (Wave 10 admin pages included: /admin, /admin/stores,
  /admin/payouts, /admin/users, /admin/settings; Wave 12 adds
  /seller/import; Wave 13 adds /messages, /messages/[id] and
  /seller/inbox; Wave 14 adds /seller/coupons and /admin/coupons;
  Wave 15 adds /search; Wave 16 adds /track, /seller/returns and
  /admin/returns; Wave 18 adds /seller/branding; Wave 19 extends
  /seller/orders and /track with the manual-courier workflow;
  Wave 21 adds /seller/verification and /admin/verifications;
  Wave 22 adds /invoice/[subOrderId]; Wave 23 adds
  /seller/inventory and the dashboard low-stock strip;
  Wave 24 adds /wishlist, the product page's related +
  also-bought rails, and the home page's personalised +
  recently-viewed strips; Wave 25 adds /seller/questions,
  the product page's Q&A section, and the dashboard's
  unanswered-questions strip; Wave 26 adds
  /seller/promotions, the স্পন্সরড badge on the shared
  ProductCard, and the admin settings promotion price;
  Wave 27 adds /seller/analytics and /admin/analytics;
  Wave 28 adds /admin/audit; Wave 29 adds
  /admin/notifications; Wave 31 adds no page — it adds the
  ডেটা এক্সপোর্ট download buttons on the seller dashboard
  and the buyer orders page; Wave 32 adds no page — it
  adds the next/image remote allowlist in next.config.js;
  Wave 33 adds the product page's variant picker, variant
  names on the cart and buyer-order lines, and the seller
  variant manager on /seller/products; Wave 34 adds
  /seller/members, the dashboard store switcher and the
  role badge next to the store name; Wave 35 adds no page —
  it adds the checkout loyalty block, the orders-page
  loyalty card, and loyalty discount lines on the order
  and receipt views; Wave 36 adds /seller/subscription,
  the dashboard plan badge and the seller nav link;
  Wave 37 adds /seller/api-keys and the seller nav link;
  Wave 38 adds /offline and the PWA shell — the manifest
  route, the generated icons, and the service worker —
  plus the install button on the home page and the
  buyer orders page; Wave 40 adds /seller/fulfillment
  and /seller/fulfillment/labels, the dashboard
  fulfillment card and the seller nav link).
- **Rust unit tests: verified in CI** — the CI `rust` job runs
  `cargo test --workspace` on every push (the domain, courier,
  payments and notify crates' unit tests, plus the API crate's
  in-module tests; the `DATABASE_URL`-gated integration tests
  run against the job's Postgres 16 service and skip cleanly
  without a database). Per the project's CI-only rule, nothing
  is built or tested on a development machine — GitHub Actions
  is the only verifier.
- **Rust integration + clippy: CI-only** — the Wave 2–49
  integration tests (including the Wave 29 outbox test in the
  `notify` crate) against a real Postgres 16 service and
  clippy `-D warnings` are verified via **GitHub Actions CI**
  (`.github/workflows/ci.yml`) on push. Waves 1–49 are green
  there, every verdict checked at job/step level twice;
  Wave 50's own run is the program's closing gate.
- **Docker images: CI-only** — Docker is not installed on the
  development machine, so `Dockerfile.api`, `Dockerfile.worker`
  and `apps/web/Dockerfile` are verified by the CI `docker` job
  (build only, no registry push), never locally.
