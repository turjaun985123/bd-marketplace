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
crates/payments/        PaymentAdapter trait + COD (records intent) + stubs
                        (Bkash, Nagad, SslCommerz, ShurjoPay)
crates/api/             Axum API — thin handlers over service functions
  src/auth.rs           phone + OTP + JWT service functions + SmsSender trait
  src/stores.rs         store create/get service functions
  src/products.rs       product CRUD + the ONE shared listing function
  src/cart.rs           one cart per buyer: add/set/remove/clear + cart view
  src/orders.rs         COD checkout (one transaction), buyer orders,
                        seller sub-orders + status transitions
  tests/wave2.rs        end-to-end integration test (needs DATABASE_URL)
  tests/wave3.rs        cart + split checkout integration test (same gate)
crates/worker/          Tokio worker binary (jobs listed, implementation TODO)
migrations/             SQLx migrations (users, stores, otp_codes, products,
                        carts, orders, sub_orders, order_items)
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
# /cart, /checkout, /orders (buyer), /orders/{id} (printable receipt),
# /seller, /seller/products (seller catalogue + create form, with a
# weight-in-grams field) and /seller/orders (sub-orders)
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

**Stubbed (typed `NotConfigured` errors — never fake success, no network
calls, no credentials):** Steadfast / Pathao / RedX courier adapters;
bKash / Nagad / SSLCommerz / ShurjoPay payment adapters. Tier 1b couriers
(eCourier, Paperfly) and manual Phase 2 couriers (Sundarban, SA Paribahan,
Karatoa, Carrybee) join behind the same traits later. SMS sending has
only the dev-mode log implementation (see the dev OTP flow above).

**TODO / not wired:** Redis, online payments (bKash/Nagad/SSLCommerz —
Wave 5), courier booking, payouts, a real SMS gateway, SIDRA calls,
worker jobs (the worker logs its planned jobs and exits cleanly).

## Verification status

- **Next.js: verified on this machine** — `npm ci && npm run build`
  passes (Wave 4 route `/orders/[id]` included).
- **Rust: NOT compiled locally** — this development VM has no `cargo`
  installed, by design. Rust compilation, `cargo test --workspace`
  (including the Wave 2 + Wave 3 + Wave 4 integration tests against
  CI's Postgres service) and clippy `-D warnings` are verified via
  **GitHub Actions CI** (`.github/workflows/ci.yml`) on push. Waves 1–3
  are green there; Wave 4 awaits its first CI run.
