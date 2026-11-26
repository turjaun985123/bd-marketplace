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
crates/api/             Axum API — thin handlers over service functions
  src/auth.rs           phone + OTP + JWT service functions + SmsSender trait
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
crates/worker/          Tokio worker binary (jobs listed, implementation TODO)
migrations/             SQLx migrations (users, stores, otp_codes, products,
                        carts, orders, sub_orders, order_items, payments,
                        payment_events, shipments, reviews, payout_accounts,
                        payouts)
infra/docker-compose.yml  Postgres + Redis + MinIO
.env.example            every environment variable, no real values
.github/workflows/ci.yml  Rust tests/clippy (with a Postgres service) + Next.js build
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
an approved store's tier is a fresh admin action, not a new
application.

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
`verified` stores pass to the ordinary Wave 9 rules. When
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

**Balance formula:** available = earned − paid out − pending,
where paid out sums payouts in `approved` (committed) or `paid`
(gone), and pending sums payouts still `requested`. The pure
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
| GET | `/v1/admin/stores?q=&page=&limit=` | Bearer, admin. Every store with owner phone, product count and status; `q` matches name or slug |
| PATCH | `/v1/admin/stores/{id}/status` | Bearer, admin. Set a store `active` or `suspended` (anything else: 400); returns the store view |
| GET | `/v1/admin/orders?status=&page=&limit=` | Bearer, admin. Every order (the shared buyer order view), newest first, optional order-status filter |
| GET | `/v1/admin/payouts?status=&page=&limit=` | Bearer, admin. The payout queue with store name + owner phone, optional status filter |
| POST | `/v1/admin/payouts/{id}/approve` | Bearer, admin. `requested → approved`; illegal moves are a 409 |
| POST | `/v1/admin/payouts/{id}/reject` | Bearer, admin. `requested → rejected`; an optional JSON `note` records the reason |
| POST | `/v1/admin/payouts/{id}/mark-paid` | Bearer, admin. `approved → paid` |
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
(`requested → approved → paid`, `requested → rejected`). A legal
move stamps `processed_at`; rejection stores the admin's note
as the reason the seller sees. Rejecting a request releases its
amount back into the seller's available balance (it stops
counting as pending; nothing was ever paid out). Approving
keeps the amount committed (it counts as paid out from
approval on, so it can never be requested twice).

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
GMV (`gmv_goods_poisha`) is the sum of `goods_subtotal_poisha`
over sub-orders whose status is NOT `cancelled`, and
`commission_earned_poisha` is the same filter over
`commission_poisha` — a cancelled sub-order never moved money,
so it counts towards neither. `payouts_pending` covers payouts
still `requested`; `payments_pending_count` covers online
payments still `pending` settlement.

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

## How to run

With a database (Wave 2 features):

```bash
docker compose -f infra/docker-compose.yml up -d   # Postgres on :5432
cp .env.example .env                               # APP_ENV=dev by default
DATABASE_URL=postgres://marketplace:marketplace_dev_only@localhost:5432/marketplace \
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
API falls back to a public dev-only secret and logs a loud warning.

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
| `JWT_SECRET` | HMAC secret for auth tokens | **Yes** — unset falls back to a public DEV-ONLY secret (loud warning) and anyone can forge tokens |
| `APP_ENV` / `SMS_DEV_MODE` | Dev mode: log SMS sender + `dev_otp` in responses | **Must be off** — `APP_ENV` must not be `dev` and `SMS_DEV_MODE` must not be `true`, or OTP codes are public |
| `COMMISSION_RATE_BPS` | Startup commission fallback (admin settings override it) | No |
| `ADVANCE_PERCENT_BPS` / `ADVANCE_THRESHOLD_POISHA` | Startup COD-advance fallback (admin settings override) | No |
| `NEXT_PUBLIC_API_BASE_URL` | API origin baked into the browser bundle at web build time | **Yes** for the web image — the public API origin |
| `POSTGRES_USER` / `POSTGRES_PASSWORD` / `POSTGRES_DB` | Compose Postgres credentials (defaults match the dev database) | **Yes** — change the password from the dev default |
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
| `PAYMENTS_DEV_MODE` / `PAYMENTS_DEV_SECRET` | DEV payment simulator (local/CI only) | **Must be off** — simulated payments are not real money |
| `COURIER_DEV_MODE` | DEV courier simulator (local/CI only) | **Must be off** — simulated shipments are not real bookings |
| `FACEBOOK_APP_ID` / `FACEBOOK_APP_SECRET` | Meta app credentials (presence only is read) | For the Graph API status to report configured |
| `SIDRA_API_URL` / `SIDRA_WEBHOOK_SECRET` | SIDRA bridge endpoint + shared HMAC secret (both or neither) | For the AI salesperson bridge |

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

### Honest limits

- **Backups are on you.** The compose stack keeps data in a
  named Docker volume; there is no managed backup, snapshot
  schedule or restore drill in this repository.
- **One host, one replica.** No horizontal-scaling story:
  the API keeps no in-memory session state (JWT + Postgres),
  so it *could* be replicated behind a load balancer, but
  that topology is untested and the web/worker services are
  single-instance here.
- **The worker still does no jobs.** It logs its planned
  jobs (courier status polling, payout jobs, notifications,
  COD reconciliation) and exits cleanly; automatic courier
  polling in particular does not exist yet — sellers refresh
  tracking through the API.
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

**Stubbed (typed errors — never fake success, no network calls):**
none among the couriers — all five API couriers have real
credential-gated adapters, and the four manual couriers run
the Wave 19 seller-driven workflow (they have no APIs to
adapt, and none is faked). SMS sending has only the
dev-mode log implementation (see the dev OTP flow above).
Fetching Facebook posts through the Meta Graph API is stubbed
honestly (503 without app credentials, 501 without the
page-token OAuth flow — see the Wave 12 section); the paste
import pipeline is the real source.

**TODO / not wired:** live gateway calls and real-gateway webhook
signature verification (need merchant credentials + each gateway's
integration docs), gateway refund APIs (Wave 16 refunds are
recorded off-platform obligations instead — see the Wave 16
section), Redis, a real SMS gateway,
worker jobs — including automatic courier status
polling, which will call the same refresh service function the API
exposes (the worker still logs its planned jobs and exits cleanly).
(The SIDRA bridge contract IS wired in Wave 13 — SIDRA itself is
the external service at the other end of it, configured by
`SIDRA_API_URL` / `SIDRA_WEBHOOK_SECRET`.)

## Verification status

- **Next.js: verified on this machine** — `npm ci && npm run build`
  passes (Wave 10 admin pages included: /admin, /admin/stores,
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
  recently-viewed strips).
- **Rust unit tests: verified on this machine** — cargo 1.100.0 is
  installed here after all; `cargo test --workspace` passes locally
  (domain 92, courier 31, payments 8 unit tests; the `DATABASE_URL`-
  gated integration tests skip cleanly without a database). The API
  binary was also smoke-tested locally without a database: routes
  build, `/v1/couriers` answers, auth rejects unauthenticated calls
  (including the Wave 13 messaging endpoints), and the SIDRA
  callback answers 503 without a database.
- **Rust integration + clippy: CI-only** — the Wave 2–24 integration
  tests against a real Postgres 16 service and clippy `-D warnings`
  are verified via **GitHub Actions CI** (`.github/workflows/ci.yml`)
  on push. Waves 1–23 are green there; Wave 24 awaits its first CI run.
- **Docker images: CI-only** — Docker is not installed on the
  development machine, so `Dockerfile.api`, `Dockerfile.worker`
  and `apps/web/Dockerfile` are verified by the CI `docker` job
  (build only, no registry push), never locally.
