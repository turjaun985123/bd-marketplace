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
  components/           ProductCard, PriceTag, DeliveryChargeWidget
  hooks/                useDeliveryCharge (only caller of /v1/delivery-charge)
  lib/                  formatBdt and shared helpers
crates/domain/          pure, unit-tested domain functions (no DB, no network)
crates/courier/         CourierAdapter trait + Tier 1 stubs (Steadfast, Pathao, RedX)
crates/payments/        PaymentAdapter trait + COD (records intent) + stubs
                        (Bkash, Nagad, SslCommerz, ShurjoPay)
crates/api/             Axum API — thin handlers over the domain crate
crates/worker/          Tokio worker binary (jobs listed, implementation TODO)
infra/docker-compose.yml  Postgres + Redis + MinIO for later waves
.env.example            every environment variable, no real values
.github/workflows/ci.yml  Rust tests/clippy + Next.js build
```

## API endpoints (Wave 1)

| Method | Path | Domain function |
| --- | --- | --- |
| GET | `/health` | — |
| POST | `/v1/delivery-charge` | `calculate_delivery_charge(zone, weight_kg, cod_amount_poisha)` |
| POST | `/v1/cart/split` | `split_cart_by_seller(items)` |
| POST | `/v1/commission/quote` | `compute_commission(subtotal_poisha, rate_bps)` |

OpenAPI: Wave 1 keeps dependencies minimal instead of pulling in a spec
generator; this table is the endpoint listing for now. A generated OpenAPI
spec (and the typed TypeScript client generated from it) is a Wave 2 task.

Worked example (unit-tested in `crates/domain`): goods ৳4,450 + delivery
৳240 = ৳4,690 payable; 8% commission on the goods subtotal only = ৳356;
sellers receive ৳4,094.

## How to run

Rust API:

```bash
cargo run -p api            # listens on API_PORT (default 8080)
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
```

Infra (not needed by Wave 1 code; for later waves):

```bash
docker compose -f infra/docker-compose.yml up -d
cp .env.example .env
```

## Wave 1 scope — what is real, stubbed and TODO

**Real:** domain functions + unit tests (delivery charge, cart split,
commission, worked example); the four API endpoints; COD adapter recording
payment intent; the Next.js route groups, shared components/hook, and the
delivery-charge widget (shows an honest "API offline" message in Bangla
when the Rust API is not running).

**Stubbed (typed `NotConfigured` errors — never fake success, no network
calls, no credentials):** Steadfast / Pathao / RedX courier adapters;
bKash / Nagad / SSLCommerz / ShurjoPay payment adapters. Tier 1b couriers
(eCourier, Paperfly) and manual Phase 2 couriers (Sundarban, SA Paribahan,
Karatoa, Carrybee) join behind the same traits later.

**TODO / not wired:** SQLx/Postgres and Redis (a marked placeholder in
`crates/api/src/main.rs` — deliberately no fake DB code), OTP/SMS login,
product/order persistence, courier booking, payouts, SIDRA calls, worker
jobs (the worker logs its planned jobs and exits cleanly).

## Verification status

- **Next.js: verified on this machine** — `npm ci && npm run build` passes;
  routes `/`, `/[store]`, `/admin`, `/seller` all build.
- **Rust: NOT compiled locally** — this development VM has no `cargo`
  installed, by design. Rust compilation, `cargo test --workspace` and
  clippy are verified via **GitHub Actions CI** (`.github/workflows/ci.yml`)
  on the first push. The code is kept deliberately simple and mainstream
  (axum, tokio, serde, async-trait, thiserror) to compile cleanly there.
