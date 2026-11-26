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
crates/courier/         CourierAdapter trait + Tier 1 stubs (Steadfast, Pathao, RedX)
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
8%), read once at startup.

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
Steadfast. Pathao and RedX became real adapters in Wave 7 (below);
eCourier and Paperfly remain `NotConfigured` stubs behind the same
trait.

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

Auth rules that matter: only a SHA-256 hash of an OTP is ever stored;
ownership is checked on every seller mutation (403 for non-owners);
every product query is scoped by store. Listing visibility: public
callers see only `active` products; a signed-in owner also sees their
own store's drafts/archived items.

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
# (seller catalogue + create form, with a weight-in-grams field)
# and /seller/orders (sub-orders)
```

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
labels. Payout APPROVAL and PAYMENT are not here yet — they are
platform operations and land with the admin wave; until then
payouts stay `requested`.

**Stubbed (typed errors — never fake success, no network calls):**
eCourier / Paperfly courier adapters. Manual Phase 2 couriers
(Sundarban, SA Paribahan, Karatoa, Carrybee) join behind the same
trait later. SMS sending has only the dev-mode log implementation
(see the dev OTP flow above).

**TODO / not wired:** live gateway calls and real-gateway webhook
signature verification (need merchant credentials + each gateway's
integration docs), refunds, payout approval/payment processing
(admin wave), Redis, a real SMS gateway,
SIDRA calls, worker jobs — including automatic courier status
polling, which will call the same refresh service function the API
exposes (the worker still logs its planned jobs and exits cleanly).

## Verification status

- **Next.js: verified on this machine** — `npm ci && npm run build`
  passes (Wave 9 seller dashboard page included).
- **Rust unit tests: verified on this machine** — cargo 1.100.0 is
  installed here after all; `cargo test --workspace` passes locally
  (domain 41, courier 21, payments 8 unit tests; the `DATABASE_URL`-
  gated integration tests skip cleanly without a database). The API
  binary was also smoke-tested locally without a database: routes
  build, `/v1/couriers` answers, auth rejects unauthenticated calls
  (including the Wave 9 seller endpoints).
- **Rust integration + clippy: CI-only** — the Wave 2–9 integration
  tests against a real Postgres 16 service and clippy `-D warnings`
  are verified via **GitHub Actions CI** (`.github/workflows/ci.yml`)
  on push. Waves 1–8 are green there; Wave 9 awaits its first CI run.
