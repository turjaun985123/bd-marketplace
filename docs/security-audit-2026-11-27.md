# MASTER REPORT — bd-marketplace backend security / bug audit

**Project:** `bd-marketplace` (cloned from https://github.com/turjaun985123/bd-marketplace into `~/workspace/SECRET PROJECT/bd-marketplace`)
**Scope:** backend only — `crates/api`, `crates/domain`, `crates/courier`, `crates/payments`, `crates/notify`, `crates/worker`, `migrations/`, `docker-compose.yml`, `infra/`, `Dockerfile.*`, `.env.example`, `.github/workflows/ci.yml`, `scripts/`, `Cargo.toml` / `Cargo.lock`. **Frontend (`apps/web/`) was NOT audited**, per the user's instruction.
**Method:** 20 agents in parallel, one per scope area, each doing 2–3 full line-by-line passes over its files (≈50 passes total). Each agent wrote its own report to `scan/agent-NN-<scope>.md`. This master report deduplicates them: **same file:line + same issue = one entry** (merged entries list every agent that found them, and keep the highest severity any agent assigned). No project code was modified; read-only audit.
**Date:** 2026-11-27 (Asia/Dhaka).

## Executive summary

- **Raw findings across the 20 agent reports: 446** — Critical 17, High 108, Medium 188, Low 133.
- **After deduplication: 363 unique findings** — Critical 15, High 91, Medium 158, Low 99.
- **Do not deploy this backend as-is.** Three independent paths let an attacker take money or accounts without paying: (1) DEV-mode payment forgery with a public default secret, (2) a vulnerable `jsonwebtoken` 9.3.1, and (3) production compose shipping a publicly known database password with dev mode on. On top of that, the money paths are systematically non-atomic: payout creation, payment settlement, OTP attempt caps, and refund/return flows all race, and cancelled paid orders are never refunded while cancelled orders can still be settled as `paid`.
- **The dominant systemic pattern (found by 8+ agents independently): check-then-act without a transaction or row lock.** The same defect class appears in OTP verification, payout creation, payment webhooks, admin payout transitions, verification approval, coupon/promotion handling, loyalty, inventory, and shipment sync. Fixing it piecemeal will not work — every money/state transition needs `SELECT … FOR UPDATE` inside one transaction plus a status-guarded `UPDATE … WHERE status = …`, and the database needs the constraints in migration 13's report as a backstop (unique `gateway_ref`, payout balance cap, coupon `used_count` guard, OTP uniqueness).
- **Second systemic pattern: dev-mode / default-secret leakage.** `SMS_DEV_MODE=true` / `APP_ENV=dev` in `.env.example` disables the JWT-secret hard stop, leaks live OTPs in API responses and logs, and (with the public `dev-secret`) makes DEV payment callbacks forgeable. `.env.example` must ship with dev mode OFF and no usable defaults.
- **Third systemic pattern: no money invariants in the database.** Totals, commissions, payouts, loyalty balances and advance payments can all be invented or driven negative by application bugs, because migrations enforce almost none of them (agent-13).
- **What held up:** no SQL injection was found anywhere in scope (all queries use bound parameters; ORDER BY comes from validated enums); JWT verification is pinned to HS256 with expiry; checkout re-derives prices server-side under row locks; admin handlers re-read `is_admin` from the DB; and the API-key model, while broad, does exclude payout/export/admin/key-management routes. Details are in each agent report's "verified clean" section.

## Counts

| Severity | Raw (20 reports) | Deduplicated (this report) |
|---|---|---|
| Critical | 17 | 15 |
| High | 108 | 91 |
| Medium | 188 | 158 |
| Low | 133 | 99 |
| **Total** | **446** | **363** |

## Top 10 critical findings (fix these first)

### CR-01 — DEV mode + public default secret = unauthenticated forgeable "paid" for any DEV order

- **Severity:** Critical | **Area:** Payments | **File:** `crates/api/src/payments.rs:559-587`
- **Found by:** agent-02 (?)
- **Impact:** With dev mode on (local/CI, staging, or a production misconfiguration — one env var, only a stderr warning stands between it and prod), anyone, without any account or payment, computes `HMAC-SHA256("dev-secret", "DEV-bkash-<order_uuid>:<amount>:success")` in any language and `POST /v1/payments/webhook/bkash …
- **Evidence:** // crates/api/src/payments.rs:559-563 — webhook takes no AuthUser, no API key, no IP check pub async fn webhook( State(state): State<AppState>, Path(method_segment): Path<String>, Json(body): Json<WebhookBody>, ) -> Result<Json<Payment>, ApiError> { // crates/payments/src/lib.rs:391-400 — scheme is public, deterministic, no secret-strength check pub fn sign_dev_callback(secret: &str, payment_ref: &str, amount_poisha: i64, status: &str) -> String { let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length"); mac.update(format!("{payment_ref}:{amount_poisha}:{status}").as_bytes()); hex::encode(mac.finalize().into_bytes()) } // …
- **Fix:** Fail startup, do not fall back, when `PAYMENTS_DEV_MODE=true` and `PAYMENTS_DEV_SECRET` is unset, empty, or shorter than e.g. 32 random bytes (generate with `openssl rand -hex 32`). Never ship a documented default secret. Additionally gate dev mode on `APP_ENV != production` (refuse to start if both are set), and remove the public webhook's ability to settle DEV payments in any non-dev build — or require the same secret to be presented as a bearer header that is never logged. ---

### CR-02 — `jsonwebtoken 9.3.1` — type-confusion claim-validation bypass (CVE-2026-25537), auth bypass

- **Severity:** Critical | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `crates/api/Cargo.toml`
- **Found by:** agent-14 (A14-02)
- **Impact:** This is the crate that signs/verifies every JWT in the API (phone+OTP+JWT auth, Wave 2). An attacker holding *any* validly signed token (e.g. their own low-privilege token) can smuggle a wrong-typed `nbf`/`exp`/`aud`/`iss`/`sub` past time-based and audience/issuer restrictions the server believes it enforces — e.g. use a not-yet-valid token immediately, or a token minted for …
- **Evidence:** `Cargo.lock`: [[package]] name = "jsonwebtoken" version = "9.3.1" Advisory (GHSA-h395-gr6q-cpjc / CVE-2026-25537, published 2026-02-04): prior to **10.3.0**, a standard claim (`exp`/`nbf`/`aud`/`iss`/`sub`) supplied with the wrong JSON type (e.g. `"nbf": "99999999999"` as a string instead of a number) is marked `FailedToParse`, which validation treats identically to `NotPresent` — so if the check is enabled (`validate_nbf = true`) but the claim is not also listed in `required_spec_claims`, the check is silently skipped. -
- **Fix:** Upgrade to `jsonwebtoken >= 10.3.0` (note: v10 splits the crypto backend into a required feature — select `rust_crypto` or `aws_lc_rs` explicitly, cf. A14-08, and re-run the full auth test suite), and in the meantime add every validated claim to `required_spec_claims` so `FailedToParse` fails closed. Add `cargo audit`/`cargo deny` to CI (A14-15) so this class can never sit undetected again. ---

### CR-03 — Production compose stack boots with a publicly known database password and dev-mode defaults if `.env` is missing or copied verbatim

- **Severity:** Critical | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `docker-compose.yml:27`
- **Found by:** agent-01 (F15); agent-14 (A14-01)
- **Impact:** Anyone who has read this public-by-design example (or the repo) knows the DB password `marketplace_dev_only`. If an operator runs `docker compose up` without editing `.env` — or with a partial `.env` — Postgres, the API and the worker all start with that password, the API is published on `${API_PORT:-8080}:8080` (`docker-compose.yml:53-54`, all interfaces, no TLS/reverse proxy …
- **Evidence:** # docker-compose.yml:27 POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:-marketplace_dev_only} # docker-compose.yml:49,65 (api and worker) DATABASE_URL: postgres://${POSTGRES_USER:-marketplace}:${POSTGRES_PASSWORD:-marketplace_dev_only}@postgres:5432/${POSTGRES_DB:-marketplace} # .env.example:16,27-28 DATABASE_URL=postgres://marketplace:marketplace_dev_only@localhost:5432/marketplace APP_ENV=dev SMS_DEV_MODE=true The compose header itself instructs `cp .env.example .env` (`docker-compose.yml:3-6`), and every secret-bearing variable has a fallback, so *no* variable is actually required. -
- **Fix:** Remove every `:-default` fallback for credentials in `docker-compose.yml` (`${POSTGRES_PASSWORD:?set POSTGRES_PASSWORD in .env}`), fail fast if `JWT_SECRET` is unset (already the documented intent in `.env.example:20-23`), change `.env.example` to `APP_ENV=production` / `SMS_DEV_MODE=false`, and add a startup guard in the API that refuses to start with `marketplace_dev_only` / dev mode outside localhost. Generate the DB password with `openssl rand -base64 32` at provisioning time.

### CR-04 — Payout creation is a check-then-insert race: concurrent requests overdraw the balance (duplicate / excess payouts)

- **Severity:** Critical | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/payouts.rs:423-482`
- **Found by:** agent-04 (C1); agent-17 (?); agent-20 (C2)
- **Impact:** Seller with 600,000 poisha available fires two concurrent `POST /v1/seller/payouts` for 600,000 each (trivial with two tabs, a script, or a client retry after a slow response — each retry is a *new* payout, since there is no idempotency key). Both read `available = 600,000`, both pass validation, both insert `requested` rows totalling 1,200,000. Nothing downstream re-checks …
- **Evidence:** - **Severity:** Critical - **File:line:** `crates/api/src/payouts.rs:423-482` (`request_payout`) - **Code evidence:** // payouts.rs:459-469 — plain pool reads, no transaction, no row lock let earnings = seller_earnings(pool, store.id).await?; let min_payout_poisha = crate::admin::effective_min_payout(state).await; validate_payout_amount_with_min(amount_poisha, earnings.available_poisha, min_payout_poisha) .map_err(ApiError::bad_request)?; sqlx::query_as( "INSERT INTO payouts (store_id, amount_poisha, method, account_name, account_number) \ VALUES ($1, $2, $3, $4
- **Fix:** Mirror the subscription pattern exactly: run `request_payout` in one transaction, first `SELECT id FROM stores WHERE id = $1 FOR UPDATE`, re-read `seller_earnings_on(&mut *tx, …)` inside it, validate, then `INSERT` inside the same transaction and commit. Additionally (defence in depth) re-check `amount <= available` in `admin::transition_payout` at approve time, and accept/store an `Idempotency-Key` header with a unique constraint so a retried POST cannot create a second payout. ---

### CR-05 — Payment webhook applies settlement with no transaction, no row lock, and a non-atomic duplicate check — success/failed race can leave payment and order inconsistent, and a paid order can be overwritten back to `pending`

- **Severity:** Critical | **Area:** Concurrency / Transactions / TOCTOU | **File:** `crates/api/src/payments.rs:264-420`
- **Found by:** agent-02 (?); agent-03 (M4); agent-17 (?); agent-20 (C1)
- **Impact:** 1. **Duplicate/concurrent delivery race.** Two deliveries of the same callback (gateway retries are normal) arriving concurrently both read `status = 'pending'`, both pass the `:341` check, both `UPDATE` — the "idempotent" return is unreachable under concurrency because the check and the update are not atomic. The payment/order values happen to be the same in the …
- **Evidence:** // :275 — plain SELECT, no FOR UPDATE, no transaction (every statement runs on `pool`) let payment: Payment = sqlx::query_as(&format!( "SELECT {PAYMENT_COLUMNS} FROM payments WHERE gateway_ref = $1 \ ORDER BY created_at DESC LIMIT 1")) ... // :341 — the ONLY duplicate guard, a check on a stale snapshot if payment.status == target { // Duplicate delivery of an already-applied result: success, no double effects. return Ok(payment); } // :359 — payment update is its own autocommit statement sqlx::query("UPDATE payments SET status = $2, paid_at = COALESCE($3, paid_at), \ updated_at = now() WHERE id = $1") ... .execute(pool) // :396/:410 — order update is a SECOND, separate autocommit statement …
- **Fix:** Wrap the whole function in one transaction: `let mut tx = pool.begin().await?;` then `SELECT ... FROM payments WHERE gateway_ref = $1 ... FOR UPDATE` on `&mut *tx`; make the duplicate check and both `UPDATE`s (payment + order) run on `&mut *tx`; insert the `payment_events` row in the same tx (or keep it out but accept duplicate audit rows); commit once. For defence in depth, make the payment update conditional: `UPDATE payments SET status=$2 ... WHERE id=$1 AND status='pending' RETURNING ...` …

### CR-06 — OTP 5-attempt cap is bypassable in full by parallel requests (check-then-act race, no transaction / row lock)

- **Severity:** Critical | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:337-371`
- **Found by:** agent-01 (F01); agent-15 (M1)
- **Impact:** The cap is the *only* brute-force control on `verify-otp` (there is no IP/global rate limit — see F04). An attacker fires one burst of N concurrent `POST /v1/auth/verify-otp` requests with N different guesses. Every request in the burst runs its `SELECT` before any of the others' `UPDATE attempts = attempts + 1` lands, so all N see `attempts < 5` and all N guesses are tested. …
- **Evidence:** // auth.rs:348-351 — plain SELECT, no transaction, no FOR UPDATE let row: Option<OtpRow> = sqlx::query_as( "SELECT code_hash, expires_at, attempts FROM otp_codes \ WHERE phone = $1 ORDER BY created_at DESC LIMIT 1") // auth.rs:356-358 if row.attempts >= OTP_MAX_ATTEMPTS { return Err(ApiError::bad_request("too many OTP attempts — request a new code")); } // auth.rs:364-370 — the increment happens only AFTER the check, as a separate statement if hash_otp(&phone, code) != row.code_hash { sqlx::query("UPDATE otp_codes SET attempts = attempts + 1 WHERE phone = $1") `OTP_MAX_ATTEMPTS` is 5 (`auth.rs:38`) against a 6-digit (1,000,000-value) code space (`auth.rs:47-50`). -
- **Fix:** Do the check-and-increment atomically in one statement inside a transaction, e.g. `UPDATE otp_codes SET attempts = attempts + 1 WHERE phone = $1 AND attempts < 5 RETURNING code_hash, expires_at, attempts` (or `SELECT ... FOR UPDATE` in an explicit transaction), and decide success/failure from the returned row. Reject when the UPDATE affects 0 rows. Apply the same pattern to the request throttle (F04/F06). ---

### CR-07 — Cancelling a paid order never refunds: buyer (and seller) cancel keeps the money

- **Severity:** Critical | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/orders.rs:1416`
- **Found by:** agent-03 (C1); agent-17 (?)
- **Impact:** Buyer pays online (webhook sets `payment_status='paid'`), then cancels while every sub-order is still `placed` — which `can_buyer_cancel` expressly allows (domain `lib.rs:408`). The order becomes `cancelled`, stock is restored, but `payment_status` stays `paid` and the money stays with the platform/seller: no refund row, no payment status change, no payout reversal. Same for a …
- **Evidence:** - **Severity:** Critical - **File:line:** `crates/api/src/orders.rs:1416–1480` (`cancel_order_for_buyer`), `crates/api/src/orders.rs:1780–1862` (`update_sub_order_status`) - **Code evidence:** // cancel_order_for_buyer — the whole money handling is: flip statuses, restock, refund *loyalty points*. sqlx::query("UPDATE orders SET status = 'cancelled' WHERE id = $1") // orders.rs:1455 sqlx::query("UPDATE sub_orders SET status = 'cancelled' WHERE order_id = $1") // orders.rs:1460 // ... restock_sub_order_items(...) + loyalty::refund_for_cancelled_sub_order(...) // No touch of `payme
- **Fix:** In both cancel paths, inside the same transaction: read the order's `payment_status` / latest payment row; if `paid` (or `advance_paid`), create a refund record (reuse the returns refund shape: `refund_poisha`, `refund_status='pending'`) and move the payment toward `refunded` via the payment transition table, or at minimum block buyer/seller cancel of paid orders with 409 until a refund flow exists. Also update `orders.payment_status` so a cancelled order never still reads `paid`.

### CR-08 — A late payment webhook settles a cancelled order as `paid` (and a retry can revive one)

- **Severity:** Critical | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/payments.rs:264`
- **Found by:** agent-02 (?); agent-03 (C2)
- **Impact:** Buyer places an online order (payment `pending`), cancels it (allowed while sub-orders are `placed`), stock is restored. The buyer then completes the payment (or a delayed gateway webhook arrives, or the buyer hits retry on a failed payment): the callback succeeds, the payment row becomes `paid`, and the **cancelled** order's `payment_status` becomes `paid`. Money is taken for …
- **Evidence:** - **Severity:** Critical - **File:line:** `crates/api/src/payments.rs:264–424` (`apply_payment_callback`), `crates/api/src/payments.rs:439–490` (`retry_payment_for_buyer`) - **Code evidence:** // apply_payment_callback loads the payment and the order facts, but never reads orders.status: let order_facts: (String, String, i64) = sqlx::query_as( "SELECT payment_method, payment_status, advance_required_poisha FROM orders WHERE id = $1") // ... then unconditionally: sqlx::query("UPDATE orders SET payment_status = $2 WHERE id = $1") // payments.rs:410 // with target = "paid" on a
- **Fix:** In `apply_payment_callback` and `retry_payment_for_buyer`, load `orders.status` (lock the order row `FOR UPDATE`) and reject/ignore settlement when the order is `cancelled` (return the payment unchanged, or 409; for a webhook, log the event and do not move money state). Ideally also void/cancel the `pending` payment row inside `cancel_order_for_buyer` so there is nothing left to settle. ---

### CR-09 — Courier collects the FULL goods price on a coupon order: coupon discount never subtracted from COD amount

- **Severity:** Critical | **Area:** Money Math / Currency | **File:** `crates/api/src/shipments.rs:591-621`
- **Found by:** agent-17 (?)
- **Impact:** every COD order placed with a coupon over-collects by exactly the coupon discount (up to 90% of goods, `MAX_COUPON_PERCENT`, `domain/src/lib.rs:1934`). Buyer is shown a receipt/invoice for `goods − discount + delivery` (`orders.rs:1587-1589` receipt; `invoices.rs:357-358` correctly subtracts the discount) but the courier is told to collect `goods + delivery − loyalty`. The …
- **Evidence:** // shipments.rs:598-601 — no discount_poisha field exists on the row at all let sub_share = row.goods_subtotal_poisha + row.delivery_charge_poisha - row.loyalty_discount_poisha; if row.payment_status == "pay_on_delivery" { Ok(sub_share.max(0)) The booking query (`shipments.rs:518-527`) selects `goods_subtotal_poisha, delivery_charge_poisha, loyalty_discount_poisha` — `so.discount_poisha` is never selected. The advance branch (`:603-617`) sums the same discount-free expression over the whole order. -
- **Fix:** add `discount_poisha` to `SubOrderBookingRow` and the booking query, and compute `sub_share = goods_subtotal − discount + delivery_charge − loyalty_discount` (and the same in the `order_collect_total` SUM). Add a regression test: coupon COD order → `compute_booking_cod_amount` equals receipt `cod_amount_due_poisha`.

### CR-10 — RedX / eCourier / Paperfly create-parsers accept *any* HTTP-200 payload containing an id — fake shipment on error body

- **Severity:** Critical | **Area:** Domain / Courier / Payments core | **File:** `crates/courier/src/lib.rs:1122-1137`
- **Found by:** agent-12 (C2)
- **Impact:** These three adapters only check the HTTP status (`is_success()`) before calling the parser. Many courier APIs return HTTP 200 with an in-body error (`{ "status": "failed", "message": "insufficient balance", "tracking_id": "<echoed/stale id>" }`, or a `data.tracking_id` left over from an error/echo payload). The parser returns `Ok(Shipment{status:"created"})`, so the caller …
- **Evidence:** pub fn parse_redx_create_response(courier: &str, payload: &Value) -> Result<Shipment, CourierError> { let tracking_id = payload.get("tracking_id") .or_else(|| payload.get("data").and_then(|d| d.get("tracking_id"))) .and_then(Value::as_str).filter(|s| !s.trim().is_empty()); match tracking_id { Some(id) => Ok(Shipment { courier: courier.to_string(), consignment_id: id.to_string(), tracking_code: Some(id.to_string()), status: "created".to_string() }), None => Err(CourierError::Gateway(message)) } } Contrast Steadfast (`parse_create_order_response`, `:512`) which requires `status == 200` *and* a `consignment` object, and Pathao (`:857`) which requires `code == 200`. -
- **Fix:** Mirror Steadfast/Pathao in all three parsers: require an explicit success signal in the body (success flag / code / status field, per each courier's documented shape) *before* accepting the id, and reject when an error/`message` field is present. Add fixture tests with `200 + error body + tracking_id` expecting `Err`.

## Full deduplicated findings

Grouped by severity, then by area. IDs are stable within this report (CR/HI/ME/LO). "Found by" lists every agent report that independently reported the same issue.

## Critical (15)

### CR-01 — OTP 5-attempt cap is bypassable in full by parallel requests (check-then-act race, no transaction / row lock)

- **Severity:** Critical | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:337-371`
- **Found by:** agent-01 (F01); agent-15 (M1)
- **Impact:** The cap is the *only* brute-force control on `verify-otp` (there is no IP/global rate limit — see F04). An attacker fires one burst of N concurrent `POST /v1/auth/verify-otp` requests with N different guesses. Every request in the burst runs its `SELECT` before any of the others' `UPDATE attempts = attempts + 1` lands, so all N see `attempts < 5` and all N guesses are tested. …
- **Evidence:** // auth.rs:348-351 — plain SELECT, no transaction, no FOR UPDATE let row: Option<OtpRow> = sqlx::query_as( "SELECT code_hash, expires_at, attempts FROM otp_codes \ WHERE phone = $1 ORDER BY created_at DESC LIMIT 1") // auth.rs:356-358 if row.attempts >= OTP_MAX_ATTEMPTS { return Err(ApiError::bad_request("too many OTP attempts — request a new code")); } // auth.rs:364-370 — the increment happens only AFTER the check, as a separate statement if hash_otp(&phone, code) != row.code_hash { sqlx::query("UPDATE otp_codes SET attempts = attempts + 1 WHERE phone = $1") `OTP_MAX_ATTEMPTS` is 5 (`auth.rs:38`) against a 6-digit (1,000,000-value) code space (`auth.rs:47-50`). -
- **Fix:** Do the check-and-increment atomically in one statement inside a transaction, e.g. `UPDATE otp_codes SET attempts = attempts + 1 WHERE phone = $1 AND attempts < 5 RETURNING code_hash, expires_at, attempts` (or `SELECT ... FOR UPDATE` in an explicit transaction), and decide success/failure from the returned row. Reject when the UPDATE affects 0 rows. Apply the same pattern to the request throttle (F04/F06). ---

### CR-02 — Payment webhook applies settlement with no transaction, no row lock, and a non-atomic duplicate check — success/failed race can leave payment and order inconsistent, and a paid order can be overwritten back to `pending`

- **Severity:** Critical | **Area:** Concurrency / Transactions / TOCTOU | **File:** `crates/api/src/payments.rs:264-420`
- **Found by:** agent-02 (?); agent-03 (M4); agent-17 (?); agent-20 (C1)
- **Impact:** 1. **Duplicate/concurrent delivery race.** Two deliveries of the same callback (gateway retries are normal) arriving concurrently both read `status = 'pending'`, both pass the `:341` check, both `UPDATE` — the "idempotent" return is unreachable under concurrency because the check and the update are not atomic. The payment/order values happen to be the same in the …
- **Evidence:** // :275 — plain SELECT, no FOR UPDATE, no transaction (every statement runs on `pool`) let payment: Payment = sqlx::query_as(&format!( "SELECT {PAYMENT_COLUMNS} FROM payments WHERE gateway_ref = $1 \ ORDER BY created_at DESC LIMIT 1")) ... // :341 — the ONLY duplicate guard, a check on a stale snapshot if payment.status == target { // Duplicate delivery of an already-applied result: success, no double effects. return Ok(payment); } // :359 — payment update is its own autocommit statement sqlx::query("UPDATE payments SET status = $2, paid_at = COALESCE($3, paid_at), \ updated_at = now() WHERE id = $1") ... .execute(pool) // :396/:410 — order update is a SECOND, separate autocommit statement …
- **Fix:** Wrap the whole function in one transaction: `let mut tx = pool.begin().await?;` then `SELECT ... FROM payments WHERE gateway_ref = $1 ... FOR UPDATE` on `&mut *tx`; make the duplicate check and both `UPDATE`s (payment + order) run on `&mut *tx`; insert the `payment_events` row in the same tx (or keep it out but accept duplicate audit rows); commit once. For defence in depth, make the payment update conditional: `UPDATE payments SET status=$2 ... WHERE id=$1 AND status='pending' RETURNING ...` …

### CR-03 — Production compose stack boots with a publicly known database password and dev-mode defaults if `.env` is missing or copied verbatim

- **Severity:** Critical | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `docker-compose.yml:27`
- **Found by:** agent-01 (F15); agent-14 (A14-01)
- **Impact:** Anyone who has read this public-by-design example (or the repo) knows the DB password `marketplace_dev_only`. If an operator runs `docker compose up` without editing `.env` — or with a partial `.env` — Postgres, the API and the worker all start with that password, the API is published on `${API_PORT:-8080}:8080` (`docker-compose.yml:53-54`, all interfaces, no TLS/reverse proxy …
- **Evidence:** # docker-compose.yml:27 POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:-marketplace_dev_only} # docker-compose.yml:49,65 (api and worker) DATABASE_URL: postgres://${POSTGRES_USER:-marketplace}:${POSTGRES_PASSWORD:-marketplace_dev_only}@postgres:5432/${POSTGRES_DB:-marketplace} # .env.example:16,27-28 DATABASE_URL=postgres://marketplace:marketplace_dev_only@localhost:5432/marketplace APP_ENV=dev SMS_DEV_MODE=true The compose header itself instructs `cp .env.example .env` (`docker-compose.yml:3-6`), and every secret-bearing variable has a fallback, so *no* variable is actually required. -
- **Fix:** Remove every `:-default` fallback for credentials in `docker-compose.yml` (`${POSTGRES_PASSWORD:?set POSTGRES_PASSWORD in .env}`), fail fast if `JWT_SECRET` is unset (already the documented intent in `.env.example:20-23`), change `.env.example` to `APP_ENV=production` / `SMS_DEV_MODE=false`, and add a startup guard in the API that refuses to start with `marketplace_dev_only` / dev mode outside localhost. Generate the DB password with `openssl rand -base64 32` at provisioning time.

### CR-04 — `jsonwebtoken 9.3.1` — type-confusion claim-validation bypass (CVE-2026-25537), auth bypass

- **Severity:** Critical | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `crates/api/Cargo.toml`
- **Found by:** agent-14 (A14-02)
- **Impact:** This is the crate that signs/verifies every JWT in the API (phone+OTP+JWT auth, Wave 2). An attacker holding *any* validly signed token (e.g. their own low-privilege token) can smuggle a wrong-typed `nbf`/`exp`/`aud`/`iss`/`sub` past time-based and audience/issuer restrictions the server believes it enforces — e.g. use a not-yet-valid token immediately, or a token minted for …
- **Evidence:** `Cargo.lock`: [[package]] name = "jsonwebtoken" version = "9.3.1" Advisory (GHSA-h395-gr6q-cpjc / CVE-2026-25537, published 2026-02-04): prior to **10.3.0**, a standard claim (`exp`/`nbf`/`aud`/`iss`/`sub`) supplied with the wrong JSON type (e.g. `"nbf": "99999999999"` as a string instead of a number) is marked `FailedToParse`, which validation treats identically to `NotPresent` — so if the check is enabled (`validate_nbf = true`) but the claim is not also listed in `required_spec_claims`, the check is silently skipped. -
- **Fix:** Upgrade to `jsonwebtoken >= 10.3.0` (note: v10 splits the crypto backend into a required feature — select `rust_crypto` or `aws_lc_rs` explicitly, cf. A14-08, and re-run the full auth test suite), and in the meantime add every validated claim to `required_spec_claims` so `FailedToParse` fails closed. Add `cargo audit`/`cargo deny` to CI (A14-15) so this class can never sit undetected again. ---

### CR-05 — RedX / eCourier / Paperfly create-parsers accept *any* HTTP-200 payload containing an id — fake shipment on error body

- **Severity:** Critical | **Area:** Domain / Courier / Payments core | **File:** `crates/courier/src/lib.rs:1122-1137`
- **Found by:** agent-12 (C2)
- **Impact:** These three adapters only check the HTTP status (`is_success()`) before calling the parser. Many courier APIs return HTTP 200 with an in-body error (`{ "status": "failed", "message": "insufficient balance", "tracking_id": "<echoed/stale id>" }`, or a `data.tracking_id` left over from an error/echo payload). The parser returns `Ok(Shipment{status:"created"})`, so the caller …
- **Evidence:** pub fn parse_redx_create_response(courier: &str, payload: &Value) -> Result<Shipment, CourierError> { let tracking_id = payload.get("tracking_id") .or_else(|| payload.get("data").and_then(|d| d.get("tracking_id"))) .and_then(Value::as_str).filter(|s| !s.trim().is_empty()); match tracking_id { Some(id) => Ok(Shipment { courier: courier.to_string(), consignment_id: id.to_string(), tracking_code: Some(id.to_string()), status: "created".to_string() }), None => Err(CourierError::Gateway(message)) } } Contrast Steadfast (`parse_create_order_response`, `:512`) which requires `status == 200` *and* a `consignment` object, and Pathao (`:857`) which requires `code == 200`. -
- **Fix:** Mirror Steadfast/Pathao in all three parsers: require an explicit success signal in the body (success flag / code / status field, per each courier's documented shape) *before* accepting the id, and reject when an error/`message` field is present. Add fixture tests with `200 + error body + tracking_id` expecting `Err`.

### CR-06 — Courier collects the FULL goods price on a coupon order: coupon discount never subtracted from COD amount

- **Severity:** Critical | **Area:** Money Math / Currency | **File:** `crates/api/src/shipments.rs:591-621`
- **Found by:** agent-17 (?)
- **Impact:** every COD order placed with a coupon over-collects by exactly the coupon discount (up to 90% of goods, `MAX_COUPON_PERCENT`, `domain/src/lib.rs:1934`). Buyer is shown a receipt/invoice for `goods − discount + delivery` (`orders.rs:1587-1589` receipt; `invoices.rs:357-358` correctly subtracts the discount) but the courier is told to collect `goods + delivery − loyalty`. The …
- **Evidence:** // shipments.rs:598-601 — no discount_poisha field exists on the row at all let sub_share = row.goods_subtotal_poisha + row.delivery_charge_poisha - row.loyalty_discount_poisha; if row.payment_status == "pay_on_delivery" { Ok(sub_share.max(0)) The booking query (`shipments.rs:518-527`) selects `goods_subtotal_poisha, delivery_charge_poisha, loyalty_discount_poisha` — `so.discount_poisha` is never selected. The advance branch (`:603-617`) sums the same discount-free expression over the whole order. -
- **Fix:** add `discount_poisha` to `SubOrderBookingRow` and the booking query, and compute `sub_share = goods_subtotal − discount + delivery_charge − loyalty_discount` (and the same in the `order_collect_total` SUM). Add a regression test: coupon COD order → `compute_booking_cod_amount` equals receipt `cod_amount_due_poisha`.

### CR-07 — Cancelling a paid order never refunds: buyer (and seller) cancel keeps the money

- **Severity:** Critical | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/orders.rs:1416`
- **Found by:** agent-03 (C1); agent-17 (?)
- **Impact:** Buyer pays online (webhook sets `payment_status='paid'`), then cancels while every sub-order is still `placed` — which `can_buyer_cancel` expressly allows (domain `lib.rs:408`). The order becomes `cancelled`, stock is restored, but `payment_status` stays `paid` and the money stays with the platform/seller: no refund row, no payment status change, no payout reversal. Same for a …
- **Evidence:** - **Severity:** Critical - **File:line:** `crates/api/src/orders.rs:1416–1480` (`cancel_order_for_buyer`), `crates/api/src/orders.rs:1780–1862` (`update_sub_order_status`) - **Code evidence:** // cancel_order_for_buyer — the whole money handling is: flip statuses, restock, refund *loyalty points*. sqlx::query("UPDATE orders SET status = 'cancelled' WHERE id = $1") // orders.rs:1455 sqlx::query("UPDATE sub_orders SET status = 'cancelled' WHERE order_id = $1") // orders.rs:1460 // ... restock_sub_order_items(...) + loyalty::refund_for_cancelled_sub_order(...) // No touch of `payme
- **Fix:** In both cancel paths, inside the same transaction: read the order's `payment_status` / latest payment row; if `paid` (or `advance_paid`), create a refund record (reuse the returns refund shape: `refund_poisha`, `refund_status='pending'`) and move the payment toward `refunded` via the payment transition table, or at minimum block buyer/seller cancel of paid orders with 409 until a refund flow exists. Also update `orders.payment_status` so a cancelled order never still reads `paid`.

### CR-08 — A late payment webhook settles a cancelled order as `paid` (and a retry can revive one)

- **Severity:** Critical | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/payments.rs:264`
- **Found by:** agent-02 (?); agent-03 (C2)
- **Impact:** Buyer places an online order (payment `pending`), cancels it (allowed while sub-orders are `placed`), stock is restored. The buyer then completes the payment (or a delayed gateway webhook arrives, or the buyer hits retry on a failed payment): the callback succeeds, the payment row becomes `paid`, and the **cancelled** order's `payment_status` becomes `paid`. Money is taken for …
- **Evidence:** - **Severity:** Critical - **File:line:** `crates/api/src/payments.rs:264–424` (`apply_payment_callback`), `crates/api/src/payments.rs:439–490` (`retry_payment_for_buyer`) - **Code evidence:** // apply_payment_callback loads the payment and the order facts, but never reads orders.status: let order_facts: (String, String, i64) = sqlx::query_as( "SELECT payment_method, payment_status, advance_required_poisha FROM orders WHERE id = $1") // ... then unconditionally: sqlx::query("UPDATE orders SET payment_status = $2 WHERE id = $1") // payments.rs:410 // with target = "paid" on a
- **Fix:** In `apply_payment_callback` and `retry_payment_for_buyer`, load `orders.status` (lock the order row `FOR UPDATE`) and reject/ignore settlement when the order is `cancelled` (return the payment unchanged, or 409; for a webhook, log the event and do not move money state). Ideally also void/cancel the `pending` payment row inside `cancel_order_for_buyer` so there is nothing left to settle. ---

### CR-09 — DEV mode + public default secret = unauthenticated forgeable "paid" for any DEV order

- **Severity:** Critical | **Area:** Payments | **File:** `crates/api/src/payments.rs:559-587`
- **Found by:** agent-02 (?)
- **Impact:** With dev mode on (local/CI, staging, or a production misconfiguration — one env var, only a stderr warning stands between it and prod), anyone, without any account or payment, computes `HMAC-SHA256("dev-secret", "DEV-bkash-<order_uuid>:<amount>:success")` in any language and `POST /v1/payments/webhook/bkash …
- **Evidence:** // crates/api/src/payments.rs:559-563 — webhook takes no AuthUser, no API key, no IP check pub async fn webhook( State(state): State<AppState>, Path(method_segment): Path<String>, Json(body): Json<WebhookBody>, ) -> Result<Json<Payment>, ApiError> { // crates/payments/src/lib.rs:391-400 — scheme is public, deterministic, no secret-strength check pub fn sign_dev_callback(secret: &str, payment_ref: &str, amount_poisha: i64, status: &str) -> String { let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length"); mac.update(format!("{payment_ref}:{amount_poisha}:{status}").as_bytes()); hex::encode(mac.finalize().into_bytes()) } // …
- **Fix:** Fail startup, do not fall back, when `PAYMENTS_DEV_MODE=true` and `PAYMENTS_DEV_SECRET` is unset, empty, or shorter than e.g. 32 random bytes (generate with `openssl rand -hex 32`). Never ship a documented default secret. Additionally gate dev mode on `APP_ENV != production` (refuse to start if both are set), and remove the public webhook's ability to settle DEV payments in any non-dev build — or require the same secret to be presented as a bearer header that is never logged. ---

### CR-10 — Payout creation is a check-then-insert race: concurrent requests overdraw the balance (duplicate / excess payouts)

- **Severity:** Critical | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/payouts.rs:423-482`
- **Found by:** agent-04 (C1); agent-17 (?); agent-20 (C2)
- **Impact:** Seller with 600,000 poisha available fires two concurrent `POST /v1/seller/payouts` for 600,000 each (trivial with two tabs, a script, or a client retry after a slow response — each retry is a *new* payout, since there is no idempotency key). Both read `available = 600,000`, both pass validation, both insert `requested` rows totalling 1,200,000. Nothing downstream re-checks …
- **Evidence:** - **Severity:** Critical - **File:line:** `crates/api/src/payouts.rs:423-482` (`request_payout`) - **Code evidence:** // payouts.rs:459-469 — plain pool reads, no transaction, no row lock let earnings = seller_earnings(pool, store.id).await?; let min_payout_poisha = crate::admin::effective_min_payout(state).await; validate_payout_amount_with_min(amount_poisha, earnings.available_poisha, min_payout_poisha) .map_err(ApiError::bad_request)?; sqlx::query_as( "INSERT INTO payouts (store_id, amount_poisha, method, account_name, account_number) \ VALUES ($1, $2, $3, $4
- **Fix:** Mirror the subscription pattern exactly: run `request_payout` in one transaction, first `SELECT id FROM stores WHERE id = $1 FOR UPDATE`, re-read `seller_earnings_on(&mut *tx, …)` inside it, validate, then `INSERT` inside the same transaction and commit. Additionally (defence in depth) re-check `amount <= available` in `admin::transition_payout` at approve time, and accept/store an `Idempotency-Key` header with a unique constraint so a retried POST cannot create a second payout. ---

### CR-11 — `build_order_totals`: negative goods subtotal panics (`clamp` + `assert`) — deterministic checkout crash

- **Severity:** Critical | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:344`
- **Found by:** agent-12 (C1)
- **Impact:** `CartItem` is a plain deserializable struct (`unit_price_poisha: i64`, `quantity: u32`, no invariant — see H7). A negative `unit_price_poisha` (or a group whose line totals sum negative) makes `goods_subtotal_poisha < 0`. Then `clamp(0, negative)` panics in Rust (`min > max`), and even before that `calculate_delivery_charge` panics on the `assert!`. One crafted cart/checkout …
- **Evidence:** // lib.rs:344 let discount_poisha = group.discount_poisha.clamp(0, goods_subtotal_poisha); // lib.rs:339-343 — called just before, with the same negative subtotal as COD: let delivery_charge_poisha = calculate_delivery_charge(zone, billable_weight_kg, goods_subtotal_poisha); // lib.rs:112 assert!(cod_amount_poisha >= 0, "COD amount cannot be negative"); -
- **Fix:** Validate at the boundary — reject `unit_price_poisha < 0` and `unit_weight_grams < 0` in `CartItem` construction / `validate_product_input` / checkout, *and* make the core total: return `Result<OrderTotals, String>` (or `Option`), replace the `assert!` in `calculate_delivery_charge` with `Err`, and replace `clamp(0, goods)` with `discount.clamp(0, goods.max(0))` only after rejecting negative goods.

### CR-12 — Unchecked `i64` money arithmetic wraps silently in release (no overflow checks configured)

- **Severity:** Critical | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:59`
- **Found by:** agent-03 (M3); agent-12 (C3); agent-17 (?)
- **Impact:** A large-but-plausible input (e.g. `unit_price_poisha` near `i64::MAX / 99`, or a many-line cart whose subtotals sum past `i64::MAX`) silently wraps to a negative/huge total in production instead of erroring — wrong charge, negative payout, or a near-zero grand total for an expensive order. The same file *does* use `checked_mul`/`checked_add`/`i128` in `promotion_cost_poisha` …
- **Evidence:** // lib.rs:59 CartItem::line_total_poisha self.unit_price_poisha * i64::from(self.quantity) // lib.rs:154 compute_commission let commission_poisha = subtotal_poisha * i64::from(rate_bps) / 10_000; // lib.rs:332,356-358,373 build_order_totals — Iterator::sum() and += / - / + on i64 group.items.iter().map(CartItem::line_total_poisha).sum(); goods_total_poisha += goods_subtotal_poisha; grand_total_poisha: goods_total_poisha - discount_total_poisha + delivery_total_poisha, The workspace `Cargo.toml` sets no `overflow-checks`; Rust defaults are panic-in-debug / **wrap-in-release**. -
- **Fix:** Use `checked_mul`/`checked_add`/`checked_sub` (returning `Result`/`Option`) in `line_total_poisha`, `compute_commission`, `split_cart_by_seller`, and `build_order_totals` (or compute in `i128` and range-check back to `i64`, as `compute_advance_poisha` already does). Also set `[profile.release] overflow-checks = true` as defence-in-depth so any missed site panics instead of wrapping. ---

### CR-13 — All suites share one database and `TRUNCATE` it; CI runs them in parallel, so suites corrupt each other — tests can false-pass or false-fail depending on scheduling

- **Severity:** Critical | **Area:** Tests / Coverage | **File:** `.github/workflows/ci.yml:37`
- **Found by:** agent-16 (C1)
- **Impact:** This is not hypothetical flakiness — it is a correctness hole in the safety net. If wave30's baseline poll runs while wave6/wave19 is mid-flow, it advances *that* suite's shipments, breaking its refresh-count assertions; conversely a `TRUNCATE` in wave14 mid-run can delete wave3's order, turning a real regression into a confusing failure (or, worse, deleting the fixture a …
- **Evidence:** - CI: `DATABASE_URL: postgres://marketplace:marketplace@localhost:5432/marketplace_test` (single shared DB) + `cargo test --workspace`. Cargo runs separate test binaries in parallel by default. - wave30 header: `"Shared-database discipline: CI runs every suite sequentially against one Postgres. This suite truncates first … then runs ONE baseline poll at a zero staleness threshold to drain anything a previous suite could have left non-terminal and stale"`. - wave30: `let baseline = refresh_stale_shipments(&poll_state, 0, 50).await…; assert_eq!(baseline.checked, baseline.advanced + baseline.unchanged + baseline.failed, …)` — the suite itself admits foreign rows exist and must be drained. - …
- **Fix:** Make the claim true or the tests isolated: either run `cargo test --workspace -- --test-threads=1` in CI (slow but honest), or give each test binary its own database/schema (e.g. per-binary `DATABASE_URL` with a unique DB name, or `CREATE SCHEMA`/template-DB per file). Delete the "sequential" claim in wave30.rs:14-22 if parallel execution is kept. At minimum, add a CI step that runs the suite twice and fails on order-dependence.

### CR-14 — Every test silently passes with no database: `cargo test` with no `DATABASE_URL` runs 0 assertions and reports success

- **Severity:** Critical | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave2.rs:57-62`
- **Found by:** agent-16 (C2)
- **Impact:** A developer (or a misconfigured CI job, a renamed env var, a dead Postgres service) gets a green `cargo test` while *zero* security assertions executed — including OTP, webhook-signature, payout, and API-key tests. The suite cannot distinguish "all secure" from "nothing ran". A regression that breaks the DB connection string would be invisible. -
- **Evidence:** let database_url = match std::env::var("DATABASE_URL") { Ok(url) if !url.is_empty() => url, _ => { eprintln!("DATABASE_URL not set — skipping Wave 2 integration test"); return; } }; Only wave32's gzip test (`wave32_gzip_compression_is_opt_in_per_request`, wave32.rs:187) runs without a DB. -
- **Fix:** Fail loudly instead of skipping in at least one gate, or add a dedicated always-run test that asserts `DATABASE_URL` is set in CI (e.g. a `#[test]` that panics when the var is missing and `CI=true`). Alternatively keep the skip locally but make the CI job assert the number of tests executed (`cargo test --workspace` output must contain ≥37 integration tests; fail the job otherwise).

### CR-15 — Wave 17 *codifies* the fraud-shield bypass: an above-threshold COD order proceeds as plain COD with the advance recorded but never collected when no gateway is available

- **Severity:** Critical | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave17.rs:565-581`
- **Found by:** agent-16 (C3)
- **Impact:** An attacker (or an ordinary outage) that takes the payment gateway offline converts every high-value / high-risk COD order into an unsecured order: goods ship with ৳412 (41,200 poisha) of required advance unpaid and `payment_status: pay_on_delivery`. The fraud shield's entire purpose — forcing a 20% advance on >৳2,000 or high-risk buyers — is silently void exactly when the …
- **Evidence:** // 8. The degradation path: a second app with NO gateway // available (payments dev mode off, no credentials …). The same above-threshold // COD checkout proceeds as plain COD with the required // advance recorded as a flag: totals unchanged, payment // status pay_on_delivery, no payment row, nothing collected. let bare_state = AppState::new(Some(pool.clone()), "wave17-test-secret".to_string(), true); let bare_app = build_app(bare_state); … assert_eq!(order["payment_status"], "pay_on_delivery"); assert_eq!(order["advance_required_poisha"], 41_200); assert_eq!(order["advance_paid_poisha"], 0); assert!(order["payment"].is_null(), "no payment row: {order}"); The same file proves the shield …
- **Fix:** Decide the intended policy and test it: either (a) fail the checkout (503/400) when an advance is required but no gateway can collect it, or (b) if "flag-and-proceed" is genuinely intended, add tests proving the flag has teeth — courier booking blocked until the advance lands (the wave17.rs:373-385 guard exists for the `advance_pending` case; assert it also applies here), the receipt shows `advance_required_poisha: 41_200` with `advance_paid: 0`, and the seller sub-order list surfaces the …

## High (91)

### HI-01 — No global rate limiting: JWT and anonymous traffic is unlimited

- **Severity:** High | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:1218`
- **Found by:** agent-15 (H1)
- **Impact:** Any unauthenticated endpoint can be hammered without limit: credential/OTP stuffing against `POST /v1/auth/verify-otp` (only the per-code 5-attempt cap slows it — see M1 for its race); scraping of the whole public catalogue via `GET /v1/search`, `GET /v1/products`; and DB-load DoS via `GET /health/ready` (lib.rs:1118 — runs a real `SELECT 1` per request, no auth) and the …
- **Evidence:** // lib.rs:1752-1765 — the entire middleware stack. No rate-limit layer exists. .layer(cors) .layer(middleware::from_fn(security_headers)) .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES)) .layer(CompressionLayer::new()) The only throttles anywhere are (a) the per-phone OTP cap (`crates/domain/src/lib.rs:2927`, `OTP_REQUEST_LIMIT_PER_HOUR = 20`) and (b) the per-API-key limiter (`crates/api/src/api_keys.rs:73`, 600/min, JWT traffic explicitly exempt per the module docs). No per-IP, per-user, or global limiter, no `tower::limit::RateLimitLayer`, no `Governor`/token bucket. -
- **Fix:** Add a router-level rate-limit layer (e.g. `tower_governor` keyed by client IP, or `RateLimitLayer`) in `build_app` before the routes, with a tighter bucket on `/v1/auth/*`, `/v1/track`, and `/health/ready`; put the limiter behind `route_layer` only if 404s should not consume budget (note: `Router::layer` also wraps the fallback, so a global layer will rate-limit 404s too — acceptable). Keep the existing per-phone and per-key limits as second layers.

### HI-02 — OTP codes stored as unsalted, fast SHA-256(phone:code) — trivial offline brute force on any DB leak

- **Severity:** High | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:54-60`
- **Found by:** agent-01 (F02); agent-15 (L5)
- **Impact:** SHA-256 is designed to be fast (~10⁹+ hashes/sec on a GPU). The code space is 10⁶. Anyone who obtains a dump/leak/backup of `otp_codes` (or a single row via SQLi elsewhere, a leaked backup, a compromised read replica) recovers *every live OTP in well under a second* by hashing 000000–999999 for the known phone — no per-code salt, no server-side pepper, no slow KDF. The …
- **Evidence:** pub fn hash_otp(phone: &str, code: &str) -> String { let mut hasher = Sha256::new(); hasher.update(phone.as_bytes()); hasher.update(b":"); hasher.update(code.as_bytes()); format!("{:x}", hasher.finalize()) } The migration comment claims "Only a SHA-256 HASH of the code is stored, never the plaintext code" — true, but the hash is a single fast SHA-256 over a 6-digit code with the phone number (which the attacker already knows) as the only "salt". -
- **Fix:** Hash with a slow, salted KDF (Argon2id or bcrypt) with a random per-code salt stored alongside the hash; or at minimum HMAC-SHA-256 with a server-side pepper from a secret (never in the DB) plus a random per-code salt. Keep the 5-minute expiry and 5-attempt cap as defence in depth, not as the primary protection.

### HI-03 — JWTs are unrevocable, 7-day, bearer-only sessions: no logout, no revocation list, no `jti`/`iat`, and `authenticate()` never checks the user exists

- **Severity:** High | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:40`
- **Found by:** agent-01 (F03); agent-15 (M3)
- **Impact:** A stolen token (XSS, leaked log, stolen device, shoulder-surfed) is valid for up to 7 days and **cannot be killed**: there is no logout endpoint, no revocation list, no per-user session version, and no account-status field to suspend a user. Re-login does not invalidate the old token; the victim has no "log out everywhere". Worse, `authenticate()` trusts `sub` blindly — a …
- **Evidence:** // auth.rs:40 pub const TOKEN_TTL_SECONDS: i64 = 7 * 24 * 60 * 60; // auth.rs:126-134 — Claims carries only sub/phone/role/exp; no jti, no iat pub struct Claims { pub sub: String, pub phone: String, pub role: String, pub exp: usize } // auth.rs:177-200 — authenticate() verifies the signature and returns; no DB lookup at all let data = decode::<Claims>(token, &DecodingKey::from_secret(state.jwt_secret.as_bytes()), &Validation::new(Algorithm::HS256)) .map_err(|_| ApiError::unauthorized("invalid or expired token"))?; let user_id = Uuid::parse_str(&data.claims.sub) .map_err(|_| ApiError::unauthorized("token subject is not a user id"))?; Ok(AuthUser { user_id, phone: data.claims.phone, role: …
- **Fix:** Add a `jti` (random UUID per token) + `iat` to `Claims`; store issued/valid `jti`s or a per-user `token_version` / `sessions` table; add a `POST /v1/auth/logout` (revoke this `jti`) and "revoke all sessions" for the user; add a `users.status` (`active`/`suspended`) column and check it (and user existence) in `authenticate()` or in a middleware that re-validates the user row (cached briefly if needed). Shorten the access-token TTL (e.g. 15–60 min) with a rotating refresh token if 7-day sessions …

### HI-04 — No IP/global rate limiting and no cooldown on the OTP endpoints: SMS toll fraud, victim SMS flooding, and login-DoS by code invalidation

- **Severity:** High | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:258-331`
- **Found by:** agent-01 (F04); agent-15 (H2)
- **Impact:** (a) **SMS pumping / toll fraud:** the per-phone cap bounds cost to 20 SMS/hour *per number*, but there is no per-IP, per-device, or global cap — an attacker iterates over thousands of valid BD numbers (`01[3-9]xxxxxxxx` is a small, enumerable space) and triggers 20 SMS each, running up the gateway bill and getting numbers flagged. (b) **Victim flooding + login DoS:** 20 …
- **Evidence:** // lib.rs:1231-1232 — both endpoints are unauthenticated, with no limiter in the router .route("/v1/auth/request-otp", post(auth::request_otp)) .route("/v1/auth/verify-otp", post(auth::verify_otp)) The only throttle in `request_otp_code` is per-phone, 20/hour (`domain/src/lib.rs:2933-2935`, `OTP_REQUEST_LIMIT_PER_HOUR = 20`), and `verify_otp_code` has no rate limit at all beyond the (race-broken, F01) 5-attempt cap. The code itself admits the throttle race: "two concurrent requests can both pass the check; that small race is accepted" (`auth.rs:270-280`). -
- **Fix:** Add per-IP (and per-device-fingerprint, if available) rate limits on both endpoints (e.g. ≤5 request-otp/hour/IP, ≤10–20 verify attempts/min/IP with exponential backoff), a minimum interval between re-requests for the same phone (e.g. 30–60 s cooldown returning 429 with `Retry-After`), and a global burst cap. Enforce the per-phone throttle atomically (see F01). Add CAPTCHA / proof-of-work on repeated failures.

### HI-05 — Dev-mode chain ships switched ON in `.env.example`: OTPs disclosed in API responses and logs, and JWT-secret validation disabled

- **Severity:** High | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/lib.rs:533`
- **Found by:** agent-01 (F05); agent-10 (H3); agent-14 (A14-09); agent-15 (H3); agent-15 (L8)
- **Impact:** The file's own comments say "NEVER enable in production: OTP codes would be public", yet the shipped template enables both flags by default, so a copy-`env.example`-to-`.env` deployment that only fills in `JWT_SECRET` still runs in dev mode. In that state: (1) anyone can call `request-otp` and read the victim's live OTP straight from the JSON `dev_otp` field — full account …
- **Evidence:** # .env.example:27-28 — the shipped defaults are dev mode ON APP_ENV=dev SMS_DEV_MODE=true // lib.rs:555-558 — any secret (including empty / the public fallback) passes in dev mode pub fn validate_jwt_secret(secret: &str, dev_mode: bool) -> Result<(), String> { if dev_mode { return Ok(()); } // lib.rs:665-666 — dev mode is derived from either flag let sms_dev_mode = std::env::var("APP_ENV").map(|v| v == "dev").unwrap_or(false) || std::env::var("SMS_DEV_MODE").map(|v| v == "true").unwrap_or(false); // lib.rs:533 — the public fallback secret pub const DEV_JWT_SECRET: &str = "dev-only-insecure-jwt-secret-change-me"; // auth.rs:326-330 — the live OTP is returned in the API response in dev mode …
- **Fix:** Ship `.env.example` with `APP_ENV=production` (or blank) and `SMS_DEV_MODE=false`; split the flags — JWT-secret validation must be gated on an explicit production/deployment flag, never on the SMS-dev flag; make `dev_otp` and the log sender require *both* dev mode *and* a non-production build/profile (or an explicit `DEV_OTP_ENABLED=true` that is refused when `APP_ENV != dev`); fail startup if `dev_otp`/log-sender is enabled while a real `DATABASE_URL`/public bind is in use. Rotate any secret …

### HI-06 — OTP replacement is non-atomic and `otp_codes` has no uniqueness: two concurrent requests leave TWO valid codes

- **Severity:** High | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:303-316`
- **Found by:** agent-01 (F06); agent-13 (H1)
- **Impact:** Two concurrent `request-otp` calls for the same phone can both execute their `DELETE` before either `INSERT`s, leaving two live rows (two valid codes, doubling the guess surface). `verify_otp_code` only ever looks at the newest row (`ORDER BY created_at DESC LIMIT 1`, `auth.rs:348-351`), so the older code stays valid-but-unchecked until a successful verify deletes all rows — …
- **Evidence:** // auth.rs:308-316 — DELETE then INSERT as separate statements, no transaction sqlx::query("DELETE FROM otp_codes WHERE phone = $1") ... sqlx::query("INSERT INTO otp_codes (phone, code_hash, expires_at) VALUES ($1, $2, $3)") ... -- 0003: no PRIMARY KEY, no UNIQUE(phone), no FK — nothing enforces "at most one row per phone" CREATE TABLE otp_codes ( phone text NOT NULL, code_hash text NOT NULL, expires_at timestamptz NOT NULL, attempts int NOT NULL DEFAULT 0, created_at timestamptz NOT NULL DEFAULT now()); CREATE INDEX otp_codes_phone_idx ON otp_codes (phone); The migration comment asserts "At most one active row per phone: requesting a new OTP deletes the previous rows" — the schema does not …
- **Fix:** Add `UNIQUE(phone)` (or make `phone` the primary key) on `otp_codes`, and replace DELETE+INSERT with a single atomic upsert (`INSERT ... ON CONFLICT (phone) DO UPDATE SET code_hash=..., expires_at=..., attempts=0, created_at=now()`) inside a transaction that also covers the throttle-log insert; send the SMS only after the upsert commits, and use the code from the committed row. ---

### HI-07 — `retry_payment_for_buyer` is lock-free and multi-statement: a webhook landing between its two updates silently overwrites `paid` back to `pending`

- **Severity:** High | **Area:** Concurrency / Transactions / TOCTOU | **File:** `crates/api/src/payments.rs:447-490`
- **Found by:** agent-20 (H1)
- **Impact:** Buyer retries a failed payment. Step 1 sets the payment `pending`. The gateway (or dev-simulate) callback for that retried attempt arrives and is applied by C1's code *between* statements 1 and 2: payment → `paid`, order → `paid`. Then retry's step 2 runs and overwrites the order to `pending` (or `advance_pending`). Net state: money taken, payment row `paid`, order `pending` — …
- **Evidence:** // :450 — stale read, no FOR UPDATE, no tx let payment = load_payment_for_owner(pool, user, payment_id).await?; if payment.status != "failed" || !can_transition_payment(&payment.status, "pending") { ... } // :458 — statement 1 (autocommit) sqlx::query("UPDATE payments SET status = 'pending', updated_at = now() WHERE id = $1") // :480 — statement 2 (autocommit), after a second stale read of the order sqlx::query("UPDATE orders SET payment_status = $2 WHERE id = $1") -
- **Fix:** One transaction with `SELECT ... FROM payments WHERE id = $1 FOR UPDATE` (and lock the order row too), conditional updates (`UPDATE payments SET status='pending' WHERE id=$1 AND status='failed'` — 0 rows = someone else moved it, return 409), order update in the same tx, commit once. Ideally share one `apply_payment_transition` helper with the webhook path so the two writers cannot disagree.

### HI-08 — [cross-file] The shared restock helper can run twice for one sub-order — the `returned` path never locks the sub-order row, so goods are put back into stock twice

- **Severity:** High | **Area:** Concurrency / Transactions / TOCTOU | **File:** `crates/api/src/orders.rs:1330-1405`
- **Found by:** agent-20 (H3)
- **Impact:** (a) Seller marks a return received while the worker poller (or a manual-status update, or a second refresh) concurrently syncs the same sub-order to `returned`: both pass their own guards (return-request status vs. stale shipment status), both restock → stock inflated by the full ordered quantity, and the phantom units can then be sold to the next buyer (oversell of goods not …
- **Evidence:** // orders.rs — the helper itself takes no lock and applies unconditionally: let items: Vec<RestockItem> = sqlx::query_as( "SELECT product_id, variant_id, quantity FROM order_items WHERE sub_order_id = $1") // ... for each item: "UPDATE product_variants SET stock = stock + $2 ... WHERE id = $1 RETURNING stock" "UPDATE products SET stock = stock + $2 ... WHERE id = $1 RETURNING stock" // returns.rs:443 — mark_return_received locks ONLY the return request: "SELECT rr.status, ... FROM return_requests rr ... WHERE rr.id = $1 FOR UPDATE OF rr" // ...then, with NO lock on sub_orders and NO status guard: sqlx::query("UPDATE sub_orders SET status = 'returned' WHERE id = $1") // :~556 …
- **Fix:** Every path that moves a sub-order must first `SELECT ... FROM sub_orders WHERE id=$1 FOR UPDATE` inside its tx (add it to `mark_return_received` and to `sync_sub_order_with_shipment_status`'s callers — or better, inside the sync helper itself, keyed on the passed `conn`), re-check `can_transition_sub_order(current, target)` under that lock, and make the status update conditional (`UPDATE sub_orders SET status=$2 WHERE id=$1 AND status=$3`, 0 rows = already moved, skip restock/notice/earn). Add …

### HI-09 — No per-buyer usage limit: one buyer/account can reuse any coupon indefinitely and can single-handedly exhaust a limited coupon

- **Severity:** High | **Area:** Coupons / Promotions | **File:** `crates/api/src/coupons.rs:24-25`
- **Found by:** agent-07 (H1)
- **Impact:** A coupon with `usage_limit = NULL` (the default — `usage_limit` is `Option<i32>`) is a permanent discount for whoever knows the code: the same buyer uses `WELCOME20` on every order forever. A coupon with `usage_limit = 100` intended for 100 different customers can be exhausted by one buyer placing 100 small orders (or one buyer + a few sock-puppet accounts — there is no …
- **Evidence:** // coupons.rs:24-25 //! There is deliberately NO per-buyer usage limit in this wave: //! the optional global `usage_limit` is the only cap. // coupons.rs:164-193 — the ONLY rejection rule; it checks active, window, // global used_count >= usage_limit, eligible > 0, min-order. No buyer identity // is even passed in: fn coupon_rejection_reason(coupon, now, eligible_subtotal_poisha) if let Some(limit) = coupon.usage_limit { if coupon.used_count >= limit { return Some("coupon usage limit reached"); } } -- 0016_coupons.sql — the audit table has no per-buyer uniqueness at all: CREATE TABLE coupon_redemptions ( coupon_id uuid NOT NULL REFERENCES coupons (id), order_id uuid NOT NULL UNIQUE …
- **Fix:** Add an explicit `per_buyer_limit int` (default 1, nullable = unlimited only by deliberate admin choice) to `coupons`; enforce it in `coupon_rejection_reason` / checkout by counting `coupon_redemptions WHERE coupon_id=$1 AND buyer_id=$2` under the same `FOR UPDATE` coupon-row lock already held in `apply_coupon_to_groups` (`orders.rs:559`), and add a DB guard — e.g. a partial unique index for `per_buyer_limit = 1` coupons, or at minimum `CREATE UNIQUE INDEX … ON coupon_redemptions (coupon_id, …

### HI-10 — Promotion overlap race: two different users can buy live promotions for the same product at the same time (double window, double charge)

- **Severity:** High | **Area:** Coupons / Promotions | **File:** `crates/api/src/promotions.rs:250-266`
- **Found by:** agent-07 (H2); agent-13 (H7)
- **Impact:** The store owner and a `manage_marketing` member of the same store have *different* payout stores (`get_store_by_owner` is per-user, promotions.rs:229), so their `FOR UPDATE` locks are on different rows and do not serialise against each other. Both fire `promote_product` concurrently for the same product: both `EXISTS` checks see no live promotion, both insert, both insert a …
- **Evidence:** // promotions.rs:250-266 — the ONLY serialisation is the CALLER's payout-store row: sqlx::query("SELECT id FROM stores WHERE id = $1 FOR UPDATE") .bind(payout_store.id) // = get_store_by_owner(user) — the CALLER's earliest store // ... let overlapping: bool = sqlx::query_scalar( "SELECT EXISTS (SELECT 1 FROM promotions \ WHERE product_id = $1 AND status = 'active' AND ends_at > now())", ) // plain SELECT — no FOR UPDATE, no lock on any promotions row -- 0027: the index is non-unique and cannot enforce the invariant: CREATE INDEX promotions_product_window_idx ON promotions (product_id, ends_at); -
- **Fix:** Enforce the invariant in the DB, not just in app code: a partial unique index `CREATE UNIQUE INDEX … ON promotions (product_id) WHERE status='active' AND ends_at > now()` (note: `now()` is not immutable — use an exclusion constraint with `tstzrange`, or a `FOR UPDATE` lock on the overlapping rows / `SELECT … FROM promotions WHERE product_id=$1 AND status='active' FOR UPDATE` before the `EXISTS`, or a per-`product_id` advisory lock `pg_advisory_xact_lock(hashtext(product_id::text))` held for the …

### HI-11 — Fixed-amount coupon bypasses the 90% discount cap: a fixed coupon can be 100% off

- **Severity:** High | **Area:** Coupons / Promotions | **File:** `crates/domain/src/lib.rs:1934-1938`
- **Found by:** agent-07 (M6); agent-07 (H3); agent-17 (?)
- **Impact:** Create a fixed coupon with `discount_value = 1_000_000_000` poisha (10,000,000 taka), `min_order_poisha = 0`. At checkout any order's discount is clamped to the eligible subtotal — 100% off goods, buyer pays delivery only, commission on a store coupon is computed on discounted goods (`commission_on_discounted`, orders.rs) so commission is 0 and seller payout is 0. The 90% cap …
- **Evidence:** // domain/lib.rs:1934-1938 — the cap's stated rationale applies to ANY 100% discount: /// Highest percent a coupon may take off (90%). A 100% coupon /// would zero out goods while delivery and commission rules still apply — /// a configuration error, not a promotion. pub const MAX_COUPON_PERCENT: i64 = 90; // domain/lib.rs:1967-1977 — but fixed coupons get NO upper bound at all: "percent" => { if !(1..=MAX_COUPON_PERCENT).contains(&discount_value) { return Err(...) } } "fixed" => { if discount_value < MIN_FIXED_COUPON_POISHA { return Err(...) } } // >= 100 only // domain/lib.rs:2005-2022 — and checkout clamps the fixed value only to the subtotal: "fixed" => discount_value, // ... …
- **Fix:** Cap fixed coupons the same way: either reject `discount_value` above a sane absolute ceiling in `validate_coupon_input` (e.g. require `discount_value <= 90% of min_order_poisha` when `min_order_poisha > 0`, and an absolute max such as 100_000 poisha / 1,000 taka otherwise), or enforce at compute time `fixed discount <= 90% of eligible subtotal` so the 90% rule in the domain comment holds for both types. Add a matching DB `CHECK`. ---

### HI-12 — `infra/docker-compose.yml` — hardcoded credentials and every data service published unauthenticated on all interfaces

- **Severity:** High | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `infra/docker-compose.yml`
- **Found by:** agent-14 (A14-03)
- **Impact:** This file is labelled "local dev", but it is the only infra file in the repo and nothing prevents `docker compose -f infra/docker-compose.yml up` on a shared/VPS host. Postgres on 5432 with a known password, Redis on 6379 with *no* auth (anyone can `FLUSHALL`, read queued jobs, or use it for SSRF-adjacent abuse), and MinIO on 9000/9001 with root credentials equal to the …
- **Evidence:** postgres: image: postgres:16-alpine environment: POSTGRES_USER: marketplace POSTGRES_PASSWORD: marketplace_dev_only # line 7, hardcoded, no ${} at all ports: ["5432:5432"] redis: image: redis:7-alpine ports: ["6379:6379"] # no password, no --requirepass minio: image: minio/minio:latest environment: MINIO_ROOT_USER: minio_dev_only MINIO_ROOT_PASSWORD: minio_dev_only # lines 23-24 ports: ["9000:9000", "9001:9001"] # S3 API + web console -
- **Fix:** Bind all three to localhost (`"127.0.0.1:5432:5432"` etc.), require `${VAR:?…}` from a gitignored env file, set a strong random MinIO root password, add `redis: command: redis-server --requirepass …`, and add a header comment + a startup check refusing to run if the host is not loopback-only. Never use this file as a deployment template.

### HI-13 — Backups are plaintext, unencrypted, world-readable by default, and explicitly never leave the host

- **Severity:** High | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `scripts/backup.sh:4-5`
- **Found by:** agent-14 (A14-04)
- **Impact:** A `pg_dump` is the whole database: users, phone numbers, password/OTP artefacts, orders, payment references, courier credentials stored in `platform_settings`, seller earnings. Any host compromise, stolen laptop/VM snapshot, mis-shared `backups/` directory, or the build-context leak in A14-13 hands an attacker the entire production dataset in one gzipped file, readable with a …
- **Evidence:** # backup.sh:4-5 — "Produces a gzipped pg_dump … plus a SHA-256 sidecar" # backup.sh:55-56 mkdir -p "$outdir" # default ./backups; no chmod/umask anywhere in the script # backup.sh:82 pg_dump "$DATABASE_URL" | gzip >"$tmp" # gzip only — compression, not encryption The header itself concedes (lines 36-39): *"A backup file is only useful once it leaves this host … This script deliberately does NOT upload anywhere."* No `gpg`/`age`/`openssl enc` step, no `chmod 600`, no `umask 077`. -
- **Fix:** Encrypt at creation (`gzip | age -r <offsite-pubkey>` or `gpg --encrypt`), `umask 077; chmod 600` on `$file`/`$file.sha256`, and implement the off-site copy the header already calls for (e.g. `rclone`/`aws s3 cp --sse` to a versioned bucket with a different credential than the DB). Test a restore from the *encrypted off-site* copy, not just from `./backups`.

### HI-14 — `restore.sh` is a one-command production wipe: `--yes` skips confirmation, no target guard, no mandatory pre-restore backup

- **Severity:** High | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `scripts/restore.sh:32-36`
- **Found by:** agent-14 (A14-05)
- **Impact:** A typo in `DATABASE_URL`, a stale shell variable, or a `--yes` in a cron/runbook restores a stale/attacker-supplied dump over production and irreversibly drops every table, extension object in `public`, and all data written since the backup — with no way back if no current backup was taken. Combined with A14-04 (plaintext backups, unauthenticated checksum, A14-11), an attacker …
- **Evidence:** # restore.sh:32-36 — --yes sets assume_yes=1 and skips the only confirmation # restore.sh:81-82 psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -q \ -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;" # restore.sh:85 gzip -dc "$file" | psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -q The header (lines 28-31) says *"take a backup of the CURRENT database first"* — but that is a comment only; nothing enforces it, and there is no check that `DATABASE_URL` is not the production database. -
- **Fix:** Refuse `--yes` unless `DATABASE_URL` matches an explicit allowlist/pattern for non-production targets (or require a second env var such as `RESTORE_TARGET_CONFIRMED=prod` plus a typed database-name match, not just the word `restore`); always take (and verify) a pre-restore backup of the target first inside the script; log the target host/database and the dump's checksum before dropping anything; require the operator to type the target database name.

### HI-15 — `rsa 0.9.10` — unpatched Marvin timing side-channel (RUSTSEC-2023-0071), no fixed release; plus panic-on-malformed-key (RUSTSEC-2026-0583)

- **Severity:** High | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `Cargo.lock`
- **Found by:** agent-14 (A14-06)
- **Impact:** Marvin needs a private-key operation and a timing oracle. In this tree the exposure is bounded *today* — the API signs JWTs with HMAC (`jsonwebtoken` HS*, `hmac 0.12.1`/`sha2 0.10.9`), so RSA private keys are not used at runtime unless RS256 signing is enabled — but the crate is compiled into the binary, any future RS256 enablement (or a test/dev path that loads an RSA key) …
- **Evidence:** `Cargo.lock`: `name = "rsa"`, `version = "0.9.10"` — the latest stable 0.9.x release. RUSTSEC-2023-0071 (Marvin attack, CVSS 5.9): timing side-channel in RSA private-key operations allowing key recovery; the advisory lists **no patched version** in the 0.9 line. RUSTSEC-2026-0583: `RsaPrivateKey` construction/validation/re-encoding can panic on crafted factors (0, 1, even, repeated) — fixed only in `rsa 0.10.4`. -
- **Fix:** Prefer the `aws_lc_rs` backend for `jsonwebtoken` (v10+, A14-02) to drop `rsa` from the runtime tree entirely; if `rust_crypto` must stay, record a dated `cargo audit` ignore for RUSTSEC-2023-0071 with the exposure analysis above and an owner/expiry, and upgrade `rsa` to `>= 0.10.4` when the `jsonwebtoken` 10/11 line permits it. Never load RSA private keys from user-supplied input in-process — parse them in a separate process or behind `catch_unwind`.

### HI-16 — `libsqlite3-sys 0.30.1` — bundled SQLite < 3.54.0, uninitialised page-cache memory (RUSTSEC-2026-0459 / CVE-2026-119375), fix unreachable on sqlx 0.8

- **Severity:** High | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `crates/api/Cargo.toml`
- **Found by:** agent-14 (A14-07)
- **Impact:** Exploitation requires opening an *untrusted* SQLite file + `-wal` sidecar. This deployment is Postgres-only, so the reachable exposure is low *if* no code path ever opens a SQLite file — but the vulnerable native code is present in the lockfile and in any build that enables the sqlite feature, and a future "local SQLite for tests/dev" shortcut would silently activate it. …
- **Evidence:** `Cargo.lock`: `name = "libsqlite3-sys"`, `version = "0.30.1"`. RUSTSEC-2026-0459: SQLite before 3.54.0 can return uninitialised page-cache memory when opening a database together with a forged `-wal` file of another page size; fixed in `libsqlite3-sys >= 0.39.0`, which `sqlx-sqlite 0.8.6` caps below `0.31` (0.9.x caps below `0.38`) — no resolution of the current graph reaches the fix. -
- **Fix:** Remove the sqlite (and mysql — see A14-19) drivers from the sqlx feature set / dependency graph if unused (`default-features = false` + explicit `postgres` only, and verify with `cargo tree -i libsqlite3-sys -e normal` that it disappears); otherwise upgrade to `sqlx >= 0.9` when it accepts `libsqlite3-sys >= 0.39`, and until then record a dated, owned assessment in `deny.toml`/`audit.toml` stating the Postgres-only reachability argument.

### HI-17 — CI has no supply-chain scanning at all — no `cargo audit`/`cargo deny`, no image scan, no SBOM/provenance

- **Severity:** High | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `.github/workflows/ci.yml:1-134`
- **Found by:** agent-14 (A14-08)
- **Impact:** A14-02 (`jsonwebtoken` CVE-2026-25537), A14-06 (`rsa`), A14-07 (`libsqlite3-sys`) and A14-09 (`chrono` unmaintained) all sat in `Cargo.lock` through green CI runs — the pipeline cannot distinguish a clean tree from a compromised one. A malicious or vulnerable transitive crate, or a compromised base image (A14-12), ships straight into the release binaries that CI itself uploads …
- **Evidence:** The `rust` job runs only `cargo test`, `cargo clippy -D warnings`, and `cargo build --release`; the `docker` job runs only `docker build` (lines 125-132) and pushes nothing; there is no `cargo audit`, `cargo deny`, `trivy`, `grype`, `docker scout`, `cosign`, SBOM generation, or Dependabot/Renovate config anywhere in the repo (checked: no `deny.toml`, `audit.toml`, `.github/dependabot.yml`). -
- **Fix:** Add a `security` job: `cargo audit --deny warnings` (or `cargo deny check advisories bans licenses sources`), `trivy image`/`grype` on all three built images, and fail the run on High/Critical. Add `cargo-deny` config with dated ignores for genuinely unfixable advisories (A14-06/A14-07) so the gate stays green *honestly*. Enable Dependabot for Cargo, Docker and GitHub Actions.

### HI-18 — `calculate_delivery_charge` panics on negative COD — reachable from every courier `price_estimate`

- **Severity:** High | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:107-115`
- **Found by:** agent-12 (H1); agent-17 (?)
- **Impact:** `Parcel.cod_amount_poisha: i64` has no validation (see H7). Any caller that builds a `Parcel` with a negative COD — a crafted checkout/estimate request, a corrupt DB row, a negative refund/adjustment — panics the thread handling the request. `price_estimate` is called during smart routing for every courier, so one bad parcel DoSes routing/estimation. `assert!` in a library …
- **Evidence:** pub fn calculate_delivery_charge(zone: DeliveryZone, weight_kg: u32, cod_amount_poisha: i64) -> i64 { assert!(cod_amount_poisha >= 0, "COD amount cannot be negative"); // ... base_charge_poisha(zone) + extra_kg * extra_kg_charge_poisha(zone) } // every adapter, e.g. Steadfast price_estimate: Ok(domain::calculate_delivery_charge(parcel.zone, parcel.weight_kg, parcel.cod_amount_poisha)) -
- **Fix:** Change the signature to return `Result<i64, String>` (or clamp/validate in `Parcel` construction) and make every `price_estimate` propagate the error instead of panicking. At minimum validate `cod_amount_poisha >= 0` where `Parcel`/`ShipmentRequest` are built.

### HI-19 — `compute_commission` enforces no rate or subtotal bounds — commission > subtotal ⇒ negative seller payout

- **Severity:** High | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:153-158`
- **Found by:** agent-12 (H2)
- **Impact:** If a stored/admin-set rate is ever > 10,000 bps (or a plan-discount subtraction in the API produces a rate the domain never floors — see L8), commission exceeds the subtotal and `seller_payout_poisha` goes negative; a negative subtotal likewise yields a negative commission (seller "earns" by selling). Downstream payout/balance code that trusts a non-negative payout can then …
- **Evidence:** pub fn compute_commission(subtotal_poisha: i64, rate_bps: u32) -> CommissionQuote { let commission_poisha = subtotal_poisha * i64::from(rate_bps) / 10_000; CommissionQuote { commission_poisha, seller_payout_poisha: subtotal_poisha - commission_poisha } } A separate validator exists — `validate_commission_bps` (`:1509`, allows `0..=3000`) — but `compute_commission` never calls it and takes a raw `u32` (up to 4,294,967,295 bps). -
- **Fix:** Validate inside `compute_commission` (or change it to return `Result`): reject `subtotal < 0` and `rate_bps > 10_000` (or > 3,000 to match `validate_commission_bps`), and clamp/saturate the payout at `>= 0`. Make the API's effective-rate computation floor at 0 after plan discounts.

### HI-20 — Courier `track()` interpolates the consignment id into the URL path unencoded — path/query injection

- **Severity:** High | **Area:** Domain / Courier / Payments core | **File:** `crates/courier/src/lib.rs:631`
- **Found by:** agent-12 (H3); agent-19 (M5)
- **Impact:** A consignment id containing `/`, `..`, `?`, `#` or `%2F` changes the request path/query on the courier host while the merchant `Api-Key`/`Secret-Key`/`Authorization: Bearer` headers are still attached — e.g. `track("../../admin/orders?x=1")` or `track("1?admin=true")` reaches a different courier endpoint with live merchant credentials. The id is normally courier-issued, but …
- **Evidence:** let url = format!("{}/status_by_cid/{consignment_id}", self.base_url); // Steadfast :631 let url = format!("{}/aladdin/api/v1/orders/{consignment_id}", self.base_url); // Pathao :1007 let url = format!("{}/tracking/{consignment_id}", self.base_url); // RedX :1252 `track(&self, consignment_id: &str)` performs no validation/encoding of the argument. (eCourier is safe here: it uses `.query(&[("tracking", consignment_id)])` at `:1510`; Paperfly posts the id as JSON.) -
- **Fix:** Percent-encode the id as a single path segment (`urlencoding` / `reqwest::Url::path_segments_mut`) or validate it against `^[A-Za-z0-9_-]+$` and reject anything else before building the URL. Apply to all three adapters.

### HI-21 — Real-gateway `refund()` will silently report success if `check()` is ever fixed — latent false-refund bug

- **Severity:** High | **Area:** Domain / Courier / Payments core | **File:** `crates/payments/src/lib.rs:258-264`
- **Found by:** agent-12 (H4)
- **Impact:** Today `check()` always errors, so `refund()` always errors — safe by accident. But the moment someone implements the live gateway by making `check()` return `Ok(())` (the natural edit), `refund()` starts returning `Ok(())` **without refunding anything**, while the other three methods panic on `unreachable!`. A caller recording "refund succeeded" would mark money returned that …
- **Evidence:** fn check(&self) -> Result<(), PaymentError> { if gateway_credentials_complete($method_variant, &self.config) { Err(PaymentError::NotImplemented($method_name.to_string())) } else { Err(PaymentError::NotConfigured($method_name.to_string())) } } async fn refund(&self, _reference: &str, _amount_poisha: i64) -> Result<(), PaymentError> { self.check()?; Ok(()) // <-- reached iff check() ever returns Ok } // initiate / verify / payout_status instead end with: unreachable!("check() always returns an error") -
- **Fix:** Make `refund()` (and the others) return `Err(PaymentError::NotImplemented(..))` explicitly, or delete the `Ok(())` and use the same `unreachable!` pattern — better, restructure so `check()` is not the sole guard: each method should end in an explicit `Err(NotImplemented)` until its live implementation lands.

### HI-22 — Dev simulators are gated by a single boolean — one misconfiguration fakes shipments and payments in production

- **Severity:** High | **Area:** Domain / Courier / Payments core | **File:** `crates/courier/src/lib.rs:1959-1968`
- **Found by:** agent-12 (H5)
- **Impact:** `dev_mode` is a plain `bool` in config (presumably env-driven in the API crate, out of scope). If it is ever `true` in a real deployment (env typo, shared staging config, `.env.example` copied), every courier booking returns a fake `DEV-<courier>-<order>` consignment, every tracking poll marches to `delivered` for *any* id (including ids that were never booked), every cancel …
- **Evidence:** // courier :1966-1967 if config.dev_mode { return Ok(Box::new(DevCourierAdapter::new(name))); } // payments :458-461 (dev used whenever credentials are incomplete AND dev_mode) } else if config.dev_mode { Box::new(DevGatewayAdapter::new(method, config.dev_secret.clone())) } // DevCourierAdapter::track — any id, no existence check: async fn track(&self, consignment_id: &str) -> Result<TrackingUpdate, CourierError> { let poll = { /* increments a global counter for this id */ }; let status = match poll { 1 => "picked_up", 2 => "in_transit", 3 => "out_for_delivery", _ => "delivered" }; Ok(TrackingUpdate { status: status.to_string(), raw_status: None }) } async fn cancel(&self, _consignment_id: …
- **Fix:** Gate the dev adapters behind `cfg!(debug_assertions)` *and* the config flag, fail startup loudly if `dev_mode` is set in a production profile, and make `DevCourierAdapter::track` return an error for ids it never created (track created ids in the map; unknown id ⇒ `Err`).

### HI-23 — Merchant secrets are `Debug`-printable in every credential struct

- **Severity:** High | **Area:** Domain / Courier / Payments core | **File:** `crates/courier/src/lib.rs:129`
- **Found by:** agent-12 (H6)
- **Impact:** Any `{:?}` logging of a config/adapter — error context, a debug log line, a panic message including a struct, a test failure dump — prints live merchant API keys, secrets and passwords into logs, which are routinely shipped to third-party log aggregators. The `secret()` accessor on `DevGatewayAdapter` (`payments :330`) also makes the dev HMAC secret trivially exfiltratable by …
- **Evidence:** every one carries `#[derive(Debug, Clone)]` (or `Debug, Clone, Default`) with the secret as a plain `String`/`Option<String>` field, e.g. `pub struct CourierConfig { pub api_key: Option<String>, pub secret_key: Option<String>, pub base_url: String }`. -
- **Fix:** Implement `Debug` manually for all credential-bearing structs, redacting secret fields (`api_key: "[redacted]"`, etc.), or wrap secrets in a `SecretString` type with a redacting `Debug`. Remove or restrict the `secret()` accessor. ---

### HI-24 — Buyer-risk endpoint is an unauthenticated-in-practice history oracle for any phone number

- **Severity:** High | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/risk.rs:126-175`
- **Found by:** agent-11 (H1); agent-18 (M2)
- **Impact:** The only gate is "caller owns ≥1 store" — any newly created empty store qualifies; there is **no check that the caller ever transacted with, shipped to, or has any relationship with the queried buyer**, despite the module doc claiming the phone is one "the seller already has (it is on their parcels)". No rate limit, no audit log, no per-seller query cap exists on this route …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/risk.rs:126-175` (`buyer_risk_by_phone`), handler `risk.rs:185-200` - **Code evidence:** // risk.rs:126-145 pub async fn buyer_risk_by_phone(pool: &PgPool, caller: &AuthUser, raw_phone: &str) -> Result<BuyerRiskView, ApiError> { let store_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stores WHERE owner_user_id = $1") .bind(caller.user_id) …; if store_count == 0 { return Err(ApiError::forbidden(…)); } let phone = normalize_bd_phone(raw_phone)…; let buyer_id: Option<Uuid> =
- **Fix:** Require a prior commercial relationship — e.g. at least one sub-order from one of the caller's stores to that buyer — before returning anything; otherwise return the same `found:false` zero shape. Add per-seller rate limiting (e.g. 60/min + daily cap), audit-log every lookup (caller, queried phone hash, result), and consider returning only `risk_level`/`requires_advance`, not exact counts.

### HI-25 — COD risk shield fails open: high-risk buyer still gets a plain-COD order (and shipment) when no gateway can collect the advance

- **Severity:** High | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/orders.rs:908-945`
- **Found by:** agent-11 (H2)
- **Impact:** For a buyer whose `requires_advance` is true (≥2 returns and ≥50% return rate, `domain/src/lib.rs:1653` `assess_buyer_risk`), the shield's entire purpose is to force money-in before dispatch. When no payment gateway is configured/available — the normal state before gateways go live, or during a gateway outage — checkout records `advance_required_poisha` as a "visible risk …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/orders.rs:908-945` (checkout, verified cross-file — risk enforcement lives there); `crates/api/src/shipments.rs:553-566` (ship gate); `crates/api/src/risk.rs:55-90` (aggregate feeding it) - **Code evidence:** // orders.rs:908-916 — risk is computed only for non-online (COD) orders let report = buyer_risk_for_user(pool, user.user_id).await?; let advance = compute_advance_poisha(…, report.risk.requires_advance); if advance > 0 { advance_required_poisha = advance; if advance_gateway_available(&state.payments_config) {
- **Fix:** Fail closed: when `advance > 0` and no gateway can collect it, reject the COD order (400/503 "advance cannot be collected right now") instead of creating a `pay_on_delivery` order — or hold the order in a non-shippable state until the advance is paid. If the business insists on the degradation, make it an explicit, time-boxed admin setting (`allow_cod_without_advance`) defaulting to off, and surface it in admin analytics.

### HI-26 — API-key scope model is a denylist: a leaked `write` key is effectively full account takeover

- **Severity:** High | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/api_keys.rs:140-172`
- **Found by:** agent-11 (H3)
- **Impact:** A single leaked `write` key (long-lived, possibly never-expiring — see M1) can, as the victim user: place/cancel orders (`POST /v1/orders`, `POST /v1/orders/:id/cancel`), change sub-order fulfilment (`PATCH /v1/seller/sub-orders/:id/status`), book courier shipments (`POST /v1/seller/sub-orders/:id/shipment`), approve/reject/mark-received returns (`POST …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/api_keys.rs:140-172` (`api_key_forbidden_route`); `crates/api/src/lib.rs:1241-1246, 1409-1451, 1560-…` (routes not on the list) - **Code evidence:** // api_keys.rs:140-172 — the ENTIRE exclusion set (admin, api-keys, // payout-account, exports, POST payouts, POST subscription) pub fn api_key_forbidden_route(path: &str, method: &Method) -> bool { if path.starts_with("/v1/admin/") || path == "/v1/admin" { return true; } if path.starts_with("/v1/api-keys") { return true; } if path.starts_with("/v1/seller/payout-accou
- **Fix:** Invert to an allowlist: define the small set of routes/scopes a key may use (e.g. `orders:read`, `products:read/write`, `inventory:read`) and deny everything else by default; add fine-grained scopes and require new money-moving / membership / identity routes to opt *in* to key access explicitly. As an immediate patch, add to `api_key_forbidden_route`: `POST /v1/seller/products/:id/promote`, `/v1/seller/stores/:id/members*`, `/v1/seller/store/verification`, `/v1/seller/sub-orders/:id/shipment`, …

### HI-27 — Facebook Page link is an unverified self-asserted claim, then displayed as the store's linked page

- **Severity:** High | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/facebook.rs:62-95`
- **Found by:** agent-11 (H6)
- **Impact:** Any seller can link *any* Page — a competitor's, a celebrity/brand Page (e.g. a well-known Bangladeshi brand or Daraz's page id) — and a plausible display name, with zero proof. The storefront then presents that Page as "linked", and imported products are stamped `source='facebook'` as if they came from that Page, lending counterfeit / impersonating listings a veneer of …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/facebook.rs:62-95` (`link_facebook_page_for_owner`); `crates/api/src/stores.rs:52-53,69-80` (`Store.facebook_page_id/name` in the public store response) - **Code evidence:** // facebook.rs:62-80 — validation is syntactic only pub async fn link_facebook_page_for_owner(pool, user, page_id, page_name) { let store = get_store_by_owner(pool, user.user_id).await?; let page_id = page_id.trim(); if page_id.is_empty() || page_id.chars().count() > 64 || !page_id.chars().all(|c| c.is_ascii_alphanumeric()) { … }
- **Fix:** Do not display a link as verified anywhere public: return/store a `facebook_page_verified: false` flag and render it as "claimed, unverified" (or hide `facebook_page_id/name` from the public store response until verified). When the Graph OAuth flow lands, require a page access token + `GET /{page-id}?fields=id` ownership check before storing, and re-verify periodically. Until then, rate-limit link/unlink and audit-log both (see M9). ---

### HI-28 — Exports are uncapped in most sections: one authenticated request can materialise an unbounded JSON document

- **Severity:** High | **Area:** Input Validation / SSRF / Uploads | **File:** `crates/api/src/exports.rs:204-275`
- **Found by:** agent-09 (M6); agent-19 (H1)
- **Impact:** Any authenticated seller (or buyer, for their own export) repeatedly calls `GET /v1/seller/export` or `GET /v1/buyer/export` (`lib.rs:1423-1424`). Each call runs 6–7 full `fetch_all` scans, builds the whole document in memory (`serde_json::Value`, `exports.rs:262-273`), and serialises it as one response. A large catalogue / long-lived account yields a multi-hundred-MB …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/exports.rs:204-275` (`build_seller_export`), `crates/api/src/exports.rs:374-470` (`buyer_export`), `crates/api/src/exports.rs:137-155` (`load_items_for`) - **Code evidence:** // exports.rs:209-217 — products: NO LIMIT let products: Vec<Product> = sqlx::query_as(&format!( "SELECT {PRODUCT_COLUMNS} FROM products p JOIN stores s ON s.id = p.store_id \ {PRODUCT_RATING_JOIN} \ WHERE p.store_id = ANY($1) ORDER BY p.created_at ASC")) .bind(store_ids).fetch_all(pool).await // exports.rs:236-243 — payouts, coupons, pro
- **Fix:** Apply the existing `EXPORT_ROW_LIMIT` (or a documented larger cap, e.g. 10,000) with `ORDER BY … LIMIT` to **every** section (products, payouts, coupons, promotions, reviews, wishlist, questions, order items), return a `truncated: true` flag / counts when a section hits the cap, and add a per-user export rate limit (e.g. 1/min, or a cooldown) in the handler. Do not rely on "small by nature".

### HI-29 — Unauthenticated search-suggest has no length limit and no rate limit: anonymous trigram/ILIKE DoS

- **Severity:** High | **Area:** Input Validation / SSRF / Uploads | **File:** `crates/api/src/search.rs:286-292`
- **Found by:** agent-19 (H2)
- **Impact:** An anonymous client sends `GET /v1/search/suggest?q=<many-KB of text>` in a loop. No auth, no per-IP/per-user throttle, no max length — each request forces two full-ish trigram scans on `products` and `stores`. This is the cheapest unauthenticated DoS in the API and also a probing oracle (title enumeration via prefix matching). The same absence of anonymous rate limiting …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/search.rs:286-292` (`suggest_handler`), `crates/api/src/search.rs:198-206` (`search_suggestions`), `crates/api/src/lib.rs:1752-1765` (router layers), `crates/api/src/lib.rs:1283-1284` (public routes) - **Code evidence:** // search.rs:286-292 — no validate_search_params call, q passed raw pub async fn suggest_handler(State(state): State<AppState>, Query(query): Query<SuggestQuery>) -> Result<Json<SuggestResults>, ApiError> { let pool = state.require_pool()?; let results = search_suggestions(pool, query.q.as_deref().u
- **Fix:** Route `suggest_handler` through `validate_search_params` (or at least the same 100-char cap) and add a global anonymous rate limit (per-IP fixed window, e.g. via `tower_governor` or a shared in-memory limiter like the existing `ApiKeyRateLimiter`) on `/v1/search` and `/v1/search/suggest` — and, as defence in depth, on all other unauthenticated GETs.

### HI-30 — Pagination `page` has no upper bound: `(page - 1) * limit` overflows `i64`

- **Severity:** High | **Area:** Input Validation / SSRF / Uploads | **File:** `crates/api/src/search.rs:100`
- **Found by:** agent-05 (M6); agent-08 (?); agent-11 (L4); agent-12 (L10); agent-19 (H4)
- **Impact:** `GET /v1/search?page=9223372036854775807&per_page=48`, or any list endpoint with `?page=<i64::MAX>`, computes `(page-1)*per_page` overflowing `i64`. In a debug build the thread panics (500 / worker crash); in a release build it wraps to a negative number, which is bound as `OFFSET $n` — Postgres rejects negative OFFSET, surfacing a 500 `ApiError::internal` and leaking an …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/search.rs:100`, `crates/api/src/products.rs:223-226` + `:242`, `crates/api/src/admin.rs:398,503,577,724`, `crates/api/src/audit.rs:128`, `crates/api/src/coupons.rs:385`, `crates/api/src/orders.rs:1265,1689`, `crates/api/src/payouts.rs:507`, `crates/api/src/qna.rs:262,440`, `crates/api/src/returns.rs:342,388,605`, `crates/api/src/reviews.rs:275`, `crates/api/src/inventory.rs:385`; validator `crates/domain/src/lib.rs:2265-2272` - **Code evidence:** // products.rs:223-226 — clamps the floor only pub(crate) fn clamp_pagination(page: Opti
- **Fix:** Cap `page` (e.g. `page <= 10_000`, or cap the computed offset, e.g. `offset <= 100_000`) in `clamp_pagination` **and** in `validate_search_params`, and compute the offset with `checked_mul` / `saturating_mul` returning 400 on overflow. Prefer keyset pagination for large lists if deep paging is ever a real use case. ---

### HI-31 — Product text fields have no maximum length: ~1 MiB title/description/title_bn stored in `TEXT` columns

- **Severity:** High | **Area:** Input Validation / SSRF / Uploads | **File:** `crates/domain/src/lib.rs:978-995`
- **Found by:** agent-05 (L1); agent-12 (L1); agent-19 (H3)
- **Impact:** An authenticated seller creates/updates a product (or imports 50 drafts, `facebook.rs:290`) with a title/description just under the 1 MiB global body cap (`lib.rs:543`, `MAX_REQUEST_BODY_BYTES = 1_048_576`). The row is stored, then returned verbatim in every list/search/detail response, amplifying one write into unbounded read payloads for every buyer, and bloating the trigram …
- **Evidence:** - **Severity:** High - **File:line:** `crates/domain/src/lib.rs:978-995` (`validate_product_input`), `crates/api/src/products.rs:589-598` (`CreateProductInput`), `crates/api/src/products.rs:616-624` (`UpdateProductInput`), `crates/api/src/facebook.rs:290-360` (`import_products_for_owner`); schema `migrations/0004_create_products.sql:6-18` - **Code evidence:** // domain/src/lib.rs:978-995 — the ONLY product validator; no length check anywhere pub fn validate_product_input(input: &ProductInput) -> Result<(), String> { if input.title.trim().is_empty() { return Err("title must not be
- **Fix:** Add explicit maxima to `validate_product_input` and enforce them in both create/update and the Facebook import (e.g. title 200 chars, `title_bn` 200, description 5,000 — pick values and mirror them in the DB as `CHECK (char_length(title) <= 200)` or `VARCHAR(n)`). Reuse the preview's 120/2,000 truncation-or-reject rule in `import_products_for_owner` instead of trusting the caller-supplied draft text.

### HI-32 — No RLS, no roles, no GRANT/REVOKE anywhere: all tenancy and authorization is application-only

- **Severity:** High | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (H3)
- **Impact:** a single application bug, a leaked/confused service credential, or any future direct-DB access (analytics, admin tool, compromised dependency with DB access) bypasses *every* tenant boundary at once — cross-store product/order/payout reads and writes, full NID and payout-account reads (see H4). There is no second line of defence. -
- **Evidence:** every table is created with no policy, no role, and no privilege restriction. E.g. `0002:5-12` `stores` carries `owner_user_id`, and `0004:8` `products.store_id REFERENCES stores (id)` — but nothing at the DB layer prevents one store's session from reading/updating another store's rows. -
- **Fix:** introduce least-privilege DB roles (app role with only the needed `SELECT/INSERT/UPDATE/DELETE` per table, a separate read-only analytics role, no `DELETE` on audit/financial tables), and enable RLS with `store_id`/`buyer_user_id` policies for tenant-owned tables — or, at minimum, document the explicit decision that the DB trusts the app completely and restrict network/credential access accordingly. Do not rely on the app alone for the tables holding money and NIDs.

### HI-33 — Full national ID number stored in plaintext, no encryption, `nid_last4` can diverge, retained forever

- **Severity:** High | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (H4)
- **Impact:** any DB backup leak, dump, read replica, or credential compromise (compounded by H3 — no RLS/roles) exposes every seller's full NID in cleartext — identity theft / KYC fraud material, with no expiry or retention rule in the schema. A bug can also store a `nid_last4` that does not match `nid_number`, so masked views lie. -
- **Evidence:** CREATE TABLE store_verifications ( store_id uuid PRIMARY KEY REFERENCES stores (id) ON DELETE CASCADE, status text NOT NULL CHECK (status IN ('pending','approved','rejected')), nid_number text NOT NULL, nid_last4 text NOT NULL, trade_license_number text, document_url text, ... ); No encryption (`pgcrypto` `pgp_sym_encrypt` / application-level encryption), no format/length `CHECK`, no `CHECK (nid_last4 = right(nid_number, 4))`. The header comment (lines 21–27) itself calls it "the FULL national ID number exactly once". -
- **Fix:** encrypt `nid_number` at rest (application-level encryption with a KMS-managed key, or `pgcrypto` column encryption — never a key stored beside the data); store only the masked/last-4 form if full-number verification can be done transiently; add `CHECK (nid_last4 = right(nid_number, 4))` if the full number must stay; add a retention rule (delete/anonymise after decision + N days) and an index/audit for access. Do not put the full NID in any other table, log, or backup without the same protection.

### HI-34 — Store `slug` has no format or reserved-name check (route squatting / routing confusion)

- **Severity:** High | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (H5)
- **Impact:** a seller can register `slug = 'admin'`, `'support'`, `'api'`, `'www'`, `'checkout'`, or a slug containing `.`, `/`, uppercase, spaces, or `..` (e.g. `evil.com`, `Admin`). That squats platform routes, enables phishing storefronts (`shop.support.…`, `shop.admin.…`), and can confuse subdomain/path routing that assumes a safe slug. Empty-string-adjacent slugs (`''`, `' '`, `'-'`) …
- **Evidence:** CREATE TABLE stores ( id uuid PRIMARY KEY DEFAULT gen_random_uuid(), owner_user_id uuid NOT NULL REFERENCES users (id), slug text UNIQUE NOT NULL, name text NOT NULL, ... ); No `CHECK (slug ~ '^[a-z0-9-]+$')`, no length check, no reserved-word exclusion. The header (lines 1–2) says `slug` is the "storefront subdomain segment (shop.<slug> style routing uses it too)". -
- **Fix:** `CHECK (slug ~ '^[a-z0-9]([a-z0-9-]{0,62}[a-z0-9])?$')` plus a reserved-slug blocklist enforced in the DB (`CHECK (slug NOT IN ('admin','api','support','www','checkout','login', …))`) or a `reserved_slugs` table with a FK/exclusion — and normalise to lowercase in the app before insert.

### HI-35 — No money arithmetic invariant is enforced anywhere (totals can be invented)

- **Severity:** High | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-02 (?); agent-13 (H6)
- **Impact:** nothing in the DB requires `grand_total = goods + delivery − discount − loyalty`, `line_total = unit_price × quantity`, `goods_subtotal = Σ line_total`, `seller_payout = goods − discount − commission`, or `commission ≤ goods`. A service bug, a race, or a malicious/tampered write can store an order whose grand total is less than its goods, a payout larger than the sale, or a …
- **Evidence:** -- orders: three independent columns, no cross-check goods_total_poisha bigint NOT NULL CHECK (goods_total_poisha >= 0), delivery_total_poisha bigint NOT NULL CHECK (delivery_total_poisha >= 0), grand_total_poisha bigint NOT NULL CHECK (grand_total_poisha >= 0); -- sub_orders: same pattern; order_items: unit_price_poisha bigint NOT NULL CHECK (unit_price_poisha >= 0), quantity int NOT NULL CHECK (quantity > 0), line_total_poisha bigint NOT NULL CHECK (line_total_poisha >= 0); Later migrations add `orders.discount_total_poisha`, `orders.loyalty_discount_poisha`, `sub_orders.discount_poisha`, `sub_orders.loyalty_discount_poisha`, `sub_orders.commission_poisha`, …
- **Fix:** enforce the invariants where Postgres can: `CHECK (line_total_poisha = unit_price_poisha * quantity)` on `order_items`; `CHECK (commission_poisha <= goods_subtotal_poisha)`, `CHECK (seller_payout_poisha <= goods_subtotal_poisha)`, `CHECK (discount_poisha <= goods_subtotal_poisha)` on `sub_orders`; and a trigger (or a DB-level assertion via a trigger, since cross-table sums need one) that reconciles order totals against sub-orders / line items at commit time. At minimum add a nightly …

### HI-36 — Loyalty ledger can go negative; earn/redeem sign is unchecked; the earn-once index is bypassable with NULL `ref_id`

- **Severity:** High | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (H8)
- **Impact:** (a) `delta` has no sign rule, so an `earn` row with negative delta, a `redeem` row with positive delta, or a zero-delta spam row all pass — the balance is `SUM(delta)` with no floor, so a buyer can redeem more points than earned (negative balance) if the service gate is bypassed or races. (b) Postgres treats `NULL`s as distinct in a unique index, so multiple `earn` rows with …
- **Evidence:** CREATE TABLE loyalty_points ( user_id uuid NOT NULL REFERENCES users (id), delta int NOT NULL, -- no CHECK (delta <> 0), no sign-vs-reason check reason text NOT NULL CHECK (reason IN ('earn','redeem','adjust')), ref_id uuid, expires_at timestamptz, ...); CREATE UNIQUE INDEX loyalty_points_earn_once_idx ON loyalty_points (ref_id) WHERE reason = 'earn'; -
- **Fix:** `CHECK ((reason = 'earn' AND delta > 0) OR (reason = 'redeem' AND delta < 0) OR (reason = 'adjust' AND delta <> 0))`; make `ref_id NOT NULL` for `earn` rows (`CHECK (reason <> 'earn' OR ref_id IS NOT NULL)`); enforce non-negative balance with a trigger or a materialised balance table with `CHECK (balance >= 0)`; add `CHECK (expires_at IS NULL OR expires_at > created_at)` and require `expires_at` on earn rows.

### HI-37 — Destructive `ON DELETE CASCADE` silently erases invoices, returns, shipments, and financial audit rows

- **Severity:** High | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (H9)
- **Impact:** deleting one sub-order deletes its shipment history, its return/refund request, and its legal invoice; deleting a product deletes its entire stock audit trail; deleting a seller deletes their promotion history and every charge deduction (so the earnings aggregate silently changes). Financial/legal/audit records must survive the deletion of the thing they describe — otherwise a …
- **Evidence:** shipments.sub_order_id uuid NOT NULL UNIQUE REFERENCES sub_orders (id) ON DELETE CASCADE; return_requests.sub_order_id uuid NOT NULL UNIQUE REFERENCES sub_orders (id) ON DELETE CASCADE; return_requests.order_id uuid NOT NULL REFERENCES orders (id) ON DELETE CASCADE; invoices.sub_order_id uuid NOT NULL UNIQUE REFERENCES sub_orders (id) ON DELETE CASCADE; stock_adjustments.product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE; promotions.product_id/store_id/seller_user_id ... ON DELETE CASCADE; seller_charges.seller_user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE; -
- **Fix:** change audit/financial/legal FKs to `ON DELETE RESTRICT` (invoices, return_requests, payment_events, stock_adjustments, seller_charges) or `ON DELETE SET NULL` where the reference is genuinely optional, and use soft-delete (`deleted_at`) for stores/products/users instead of hard deletes. Keep `CASCADE` only for genuinely owned ephemeral rows (cart_items, wishlist_items, messages).

### HI-38 — Duplicate conversations are possible: "one open conversation per triple" is service-only and racy

- **Severity:** High | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-08 (?); agent-13 (H10)
- **Impact:** lookup-then-insert is a classic TOCTOU race: two concurrent first messages from the same buyer about the same product both look up (find nothing), both insert, and the buyer/seller now have two parallel "open" threads, split unread counts and split SIDRA suggestions. The comment's NULL argument is also incomplete — a partial unique index can express it (see fix). -
- **Evidence:** -- header: "There is deliberately NO unique constraint on (store_id, buyer_id, product_id): -- Postgres treats NULLs as distinct in unique indexes, so the database cannot express -- 'one OPEN conversation per triple' — the messaging service enforces it with a lookup-then-insert" CREATE TABLE conversations ( store_id uuid NOT NULL REFERENCES stores (id), buyer_id uuid NOT NULL REFERENCES users (id), product_id uuid REFERENCES products (id), status text NOT NULL DEFAULT 'open' CHECK (status IN ('open','closed')), ...); -
- **Fix:** `CREATE UNIQUE INDEX conversations_one_open_idx ON conversations (store_id, buyer_id, COALESCE(product_id, '00000000-0000-0000-0000-000000000000'::uuid)) WHERE status = 'open';` (or a generated column holding `COALESCE(product_id, …)` and a partial unique index on it). Keep the service pre-check for a clean error, but let the DB be the backstop. ---

### HI-39 — Buyer self-selects the delivery zone; delivery charge is never validated against the address

- **Severity:** High | **Area:** Money Math / Currency | **File:** `crates/api/src/orders.rs:646-648`
- **Found by:** agent-17 (?)
- **Impact:** any buyer picks `inside_dhaka` for an outside-Dhaka address and underpays delivery by 6,000 poisha (plus 2,000 vs 4,000 per extra kg) on every order. The courier still charges the real (higher) rate, so the platform/seller absorbs the difference on every such order — trivially scalable, no tools needed. -
- **Evidence:** let zone = parse_delivery_zone(&input.delivery_zone).ok_or_else(|| { ApiError::bad_request("delivery_zone must be one of: inside_dhaka, dhaka_suburb, outside_dhaka") })?; `delivery_zone` comes straight from the buyer's request body. No code anywhere cross-checks it against `delivery_address` (grep over `crates/api/src/*.rs` shows the zone is only parsed, stored, and re-read for booking). Inside Dhaka = 6,000 poisha, Outside Dhaka = 12,000 (`domain/src/lib.rs:84-86`). -
- **Fix:** derive the zone server-side from the delivery address (district/area lookup against a zone table) or, at minimum, require the seller/courier booking step to confirm/override the zone and re-charge the difference before shipping; never trust a client-supplied zone for pricing.

### HI-40 — Prepaid RTO (courier-returned) orders have no refund path at all

- **Severity:** High | **Area:** Money Math / Currency | **File:** `crates/api/src/shipments.rs:868-875`
- **Found by:** agent-17 (?)
- **Impact:** a prepaid online order whose parcel is returned-to-origin (buyer absent/refused) moves to `returned` with no return request, no refund snapshot, and no queue entry — and the buyer *cannot* open one afterwards, because `can_request_return` only allows `delivered`. The buyer paid in full, received nothing, and the system has no record that money is owed. The "manual" refund the …
- **Evidence:** // shipments.rs:868-871 — "A courier-driven or seller-entered return creates NO return // request and NO refund … a prepaid one is refunded manually" pub fn can_request_return(sub_order_status: &str) -> bool { sub_order_status == "delivered" } -
- **Fix:** when the shipment sync moves a prepaid sub-order to `returned`, create a refund obligation for `goods − discount − loyalty` (+ delivery if policy says so) and add it to the admin refund queue; or allow `can_request_return` from `returned` for prepaid orders. Same fix as H-02's refund-obligation table covers this.

### HI-41 — Seller earnings (and payouts) accrue on COD delivery before any courier cash is remitted

- **Severity:** High | **Area:** Money Math / Currency | **File:** `crates/api/src/payouts.rs:213-240`
- **Found by:** agent-17 (?)
- **Impact:** a COD sub-order marked `delivered` immediately becomes withdrawable seller balance, even though the courier may not have collected the cash, may remit weeks later, or may never remit (lost parcel marked delivered, courier default). The seller withdraws, the platform is left chasing the courier for money already paid out. For online-paid orders this is fine; for COD it pays out …
- **Evidence:** (SELECT COALESCE(SUM(seller_payout_poisha), 0)::bigint FROM sub_orders WHERE store_id = $1 AND status = 'delivered') AS earned_poisha, No filter on `orders.payment_status` and no join to any cash-remittance record. The payments crate doc concedes COD "reconciliation happens via the courier crate in a later wave" (`crates/payments/src/lib.rs`, COD adapter) — i.e. it does not exist yet. -
- **Fix:** gate COD earnings on cash collection: only count a delivered COD sub-order toward `earned_poisha` once its COD amount is confirmed remitted (courier settlement record / shipment `cod_collected` flag), or hold COD earnings in a pending bucket until remittance. At minimum, separate COD vs prepaid earnings in the dashboard so payouts can be restricted to prepaid + remitted COD.

### HI-42 — Claimed rows strand in `sending` forever: no reaper, no `claimed_at`, no recovery

- **Severity:** High | **Area:** Notifications / Notify / Worker | **File:** `crates/notify/src/lib.rs:242-302`
- **Found by:** agent-10 (H1)
- **Impact:** worker OOM-killed, SIGKILLed by the orchestrator, host crash, DB blip during the outcome `UPDATE`, or a panic in a future gateway's `send_sms` — any of these permanently loses every notification in that batch (order-placed, payout-paid, delivery confirmations). They are never retried, never marked `failed`, and the admin view cannot distinguish "in flight for 2 seconds" from …
- **Evidence:** // notify/src/lib.rs:247-265 — claim flips rows to 'sending' and commits BEFORE any send "UPDATE notifications SET status = 'sending' \ WHERE id IN ( \ SELECT id FROM notifications \ WHERE status = 'pending' \ ORDER BY created_at \ LIMIT $1 \ FOR UPDATE SKIP LOCKED \ ) \ RETURNING id, phone, body, attempts", // notify/src/lib.rs:267-297 — outcome write uses `?`; any DB error aborts the // whole batch mid-loop, and the rows already claimed stay 'sending' .execute(pool) .await?; // <-- inside the per-row loop, both Ok and Err arms The code itself admits it (`lib.rs:236-241`): *"A worker that dies mid-batch leaves its claimed rows in `sending`; they are counted in the admin view (never hidden) …
- **Fix:** add `claimed_at timestamptz` (set in the claim UPDATE) and `next_attempt_at timestamptz` to the table; add a reaper that runs at the start of each worker cycle (or as a periodic job) resetting `sending` rows with `claimed_at < now() - interval '5 minutes'` back to `pending` (incrementing attempts, or leaving attempts alone and counting the reaper reset as the failure). Also make the per-row outcome write failure non-fatal to the rest of the batch: log the error and continue, or collect errors …

### HI-43 — No timeout on any send; one hanging send stalls the entire worker, including money jobs, and blocks graceful shutdown

- **Severity:** High | **Area:** Notifications / Notify / Worker | **File:** `crates/notify/src/lib.rs:266-268`
- **Found by:** agent-10 (H2)
- **Impact:** once a real SMS gateway exists, one slow/hung gateway call (or a gateway that hangs on a malformed number) freezes the worker loop indefinitely: no further SMS, no courier polling, no subscription renewals (sellers keep or lose paid plans late, renewals from earnings balances stall). SIGTERM during the stuck batch is ignored until the batch returns, so the orchestrator …
- **Evidence:** // notify/src/lib.rs:266 match sender.send_sms(&row.phone, &row.body).await { // no timeout, no deadline // worker/src/main.rs:211-249 — all three jobs run sequentially in one loop, // and shutdown is only awaited during the sleep at the end: match notify::process_pending(&pool, sender.as_ref(), notify::DEFAULT_BATCH).await { ... } match api::shipments::refresh_stale_shipments(&state, stale_minutes, SHIPMENT_POLL_BATCH).await { ... } match api::subscriptions::process_subscriptions(&state).await { ... } tokio::select! { _ = shutdown_signal() => { println!("worker: shutdown signal received — exiting"); break; } _ = tokio::time::sleep(Duration::from_secs(seconds)) => {} } `SmsSender::send_sms` …
- **Fix:** wrap each send in `tokio::time::timeout` (e.g. 10–30 s; treat timeout as a failed attempt), add a timeout to the `SmsSender` contract or document it as a requirement, run the three jobs concurrently (or in separate tasks) so a stuck notification batch cannot starve shipment polling and renewals, and select on the shutdown signal during batch processing (abort between rows on shutdown). Add a minimal health endpoint or heartbeat log so a stuck loop is observable.

### HI-44 — SMS can be sent to any valid Bangladeshi number the buyer types: no ownership check, no rate limit on the notification path

- **Severity:** High | **Area:** Notifications / Notify / Worker | **File:** `crates/notify/src/lib.rs:171-207`
- **Found by:** agent-10 (H4)
- **Impact:** an attacker places repeated COD checkouts (each needs only a cart with items; `order_placed` fires at checkout, and shipped/delivered/ cancelled events follow as the seller acts) with a victim's valid BD number as `delivery_phone`. The victim receives a stream of genuine platform SMS about orders they never placed — harassment / smishing-adjacent confusion — and, once a paid …
- **Evidence:** // orders.rs:644-645 — the ONLY check on the delivery phone at checkout let delivery_phone = normalize_bd_phone(&input.delivery_phone) .ok_or_else(|| ApiError::bad_request("invalid Bangladeshi phone number"))?; // notices.rs:63-64 — that number is then used as the SMS recipient for every // sub-order event, with no check it belongs to the buyer "SELECT o.buyer_user_id, o.delivery_phone, o.id AS order_id, \ s.name AS store_name FROM sub_orders so ..." // notify/src/lib.rs:184-199 — enqueue accepts any phone/kind/payload unconditionally pub async fn enqueue(executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>, user_id: Uuid, phone: &str, kind: &str, payload: &Value) -> Result<Uuid, …
- **Fix:** require the delivery phone to be the buyer's verified account phone, or verify ownership of a distinct delivery phone (OTP challenge) before first use; otherwise fall back to the account phone for notifications. Add a per-phone and per-user notification cap (e.g. max N enqueues per phone per hour, counted in the `notifications` table) enforced in `enqueue`, and a per-user checkout rate limit. At minimum, log/monitor enqueues per phone so pumping is visible.

### HI-45 — No retry backoff: failed rows return to `pending` immediately and burn all 3 attempts in ~20 seconds

- **Severity:** High | **Area:** Notifications / Notify / Worker | **File:** `crates/notify/src/lib.rs:278-296`
- **Found by:** agent-10 (H5)
- **Impact:** a transient gateway outage, rate-limit response, or brief network blip becomes permanent loss: three immediate retries all hit the same outage window instead of spreading over minutes/hours. Against a real gateway it also triples the load on the failing dependency at exactly the wrong moment (retry storm), and a gateway that bills per attempt bills 3× for one logical message. -
- **Evidence:** // notify/src/lib.rs:280-291 — failure path: status goes straight back to 'pending' let attempts = row.attempts + 1; let exhausted = attempts >= MAX_ATTEMPTS; let status = if exhausted { "failed" } else { "pending" }; sqlx::query("UPDATE notifications SET status = $2, attempts = $3, last_error = $4 ...") There is no `next_attempt_at` column (migration 0029) and the claim query (`lib.rs:249-260`) selects all `pending` rows regardless of when they last failed. With the default 10 s poll, a row that fails at t=0 is retried at t=10 and t=20 and is permanently `failed` by t≈20 s. Today the only production sender is `UnconfiguredSmsSender`, so *every* notification deterministically dies this way. …
- **Fix:** add `next_attempt_at timestamptz` (default `now()`), set it with exponential backoff on failure (e.g. 1 min → 5 min → 30 min), claim only rows with `next_attempt_at <= now()`, and raise `MAX_ATTEMPTS` or make it configurable once backoff exists. Do not retry `sms_not_configured`-class permanent errors at all — park them as `failed` immediately instead of burning attempts. ---

### HI-46 — Checkout silently deletes cart lines added while it is running (cart is never locked)

- **Severity:** High | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/orders.rs:655`
- **Found by:** agent-03 (H1); agent-20 (M3)
- **Impact:** Buyer starts checkout; while the checkout transaction is open (it does coupon/loyalty/risk/payment work after the lock query), the buyer (or a second tab/device) adds an item or changes a quantity. The new/changed line was never in the locked snapshot, so it is not ordered or paid for — but the unconditional `DELETE FROM cart_items WHERE cart_id = $1` at orders.rs:1220 wipes …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/orders.rs:655–672` (cart lookup + lock query), `crates/api/src/orders.rs:1220` (cart clear); `crates/api/src/cart.rs:177–246` (`add_cart_item`, autocommit, no lock) - **Code evidence:** // place_order: the cart row itself is never locked, and cart_items rows are never locked — // only the PRODUCT rows are: let cart_id: Uuid = sqlx::query_scalar("SELECT id FROM carts WHERE user_id = $1") // orders.rs:655 let locked: Vec<LockedCartItem> = sqlx::query_as( "SELECT ... FROM cart_items ci JOIN products p ... WHERE ci.cart_id = $1
- **Fix:** Lock the cart row at the start of `place_order` (`SELECT id FROM carts WHERE user_id = $1 FOR UPDATE`), which serializes all cart mutations against checkout — and make `add_cart_item` / `set_cart_item_quantity` / `remove_cart_item` / `clear_cart` take the same lock (or run the whole checkout read under a cart-row lock). Alternatively delete only the exact `(product_id, variant_id, quantity)` lines snapshotted, and fail/merge if the cart changed mid-checkout.

### HI-47 — Seller status PATCH ships goods before any payment, bypassing even the COD-advance gate

- **Severity:** High | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/orders.rs:1780`
- **Found by:** agent-03 (H2)
- **Impact:** Seller confirms a sub-order (`placed → confirmed`) and marks it `shipped` (`confirmed → shipped`) directly via `PATCH /v1/seller/sub-orders/:id`, with no shipment booked, no courier, and no payment: online order still `payment_status='pending'`/`'failed'`, or COD order still `advance_pending` with the advance unpaid. Goods leave with no money in. The advance shield (Wave 17) …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/orders.rs:1780–1862` (`update_sub_order_status`) - **Code evidence:** the function checks ownership, then only the domain transition table: if !can_transition_sub_order(&current_status, new_status) { /* 400 */ } // orders.rs:1805 // No read of orders.payment_status / advance state anywhere in the function. sqlx::query("UPDATE sub_orders SET status = $2 WHERE id = $1") // orders.rs:1822 The advance gate exists only in courier booking (`shipments.rs:561`: `if row.advance_required_poisha > 0 && row.payment_status == "advance_pend
- **Fix:** In `update_sub_order_status`, load the parent order's `payment_status`/`advance_required_poisha` (join `orders`) and reject `confirmed`/`shipped` moves while an online payment is not `paid` and while `advance_required_poisha > 0 && payment_status == 'advance_pending'`. Apply the same rule to the manual-shipment path; keep `confirmed` allowed (as booking does) but gate `shipped`.

### HI-48 — Courier booking ships unpaid online orders; the courier is told to collect ৳0

- **Severity:** High | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/shipments.rs:520`
- **Found by:** agent-03 (H3)
- **Impact:** Buyer checks out with bkash/Nagad/etc., payment stays `pending` (never completes) or `failed`. Seller confirms and books a courier — allowed, because booking only requires status `confirmed` plus the advance check. The parcel ships, `compute_booking_cod_amount` returns 0 for a non-COD order, so the courier collects nothing. Seller loses the goods and the platform has no …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/shipments.rs:520–575` (`load_booking_row`), `crates/api/src/shipments.rs:596–620` (`compute_booking_cod_amount`) - **Code evidence:** // load_booking_row: the ONLY payment-related gate is the advance one: if row.advance_required_poisha > 0 && row.payment_status == "advance_pending" { return Err(ApiError::conflict("advance payment pending ...")); } // shipments.rs:561 // No check that an online order's payment_status is 'paid'. And: } else if row.payment_status == "advance_paid" { /* net of advance */ } else { Ok(0) } //
- **Fix:** In `load_booking_row`, reject booking when the parent order is online (`payment_method != 'cod'`) and `payment_status != 'paid'` (409 "payment not completed"). Same for the manual-shipment booking path if it shares this loader (it does — verify at fix time).

### HI-49 — Concurrent shipment refresh / manual status can restock the same items twice (stock inflation → later oversell)

- **Severity:** High | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/shipments.rs:959`
- **Found by:** agent-03 (H4); agent-09 (H7)
- **Impact:** Two concurrent refreshes of one shipment (buyer + seller both hit refresh, or a double-click, or refresh racing a manual status set) both read `sub_status='shipped'`, both see the courier report `returned`, both pass the stale in-memory transition check, and both run the restock: `stock += quantity` twice for goods that came back once. Stock is now inflated above physical …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/shipments.rs:959–1035` (`refresh_shipment_by_id`), `crates/api/src/shipments.rs:1343–1400` (`set_manual_shipment_status`), `crates/api/src/shipments.rs:877–915` (`sync_sub_order_with_shipment_status`), `crates/api/src/orders.rs:1340–1400` (`restock_sub_order_items`) - **Code evidence:** // refresh: sub-order status is read on the POOL with no lock, BEFORE the transaction begins: let sub_status: String = sqlx::query_scalar("SELECT so.status FROM sub_orders so WHERE so.id = $1") .fetch_one(pool) // shipments.rs:975 — stale the mo
- **Fix:** Inside the transaction, lock the sub-order row (`SELECT status FROM sub_orders WHERE id=$1 FOR UPDATE`), re-read its status, and only then decide the transition; make the status update conditional (`UPDATE sub_orders SET status=$2 WHERE id=$1 AND status=$3`, check `rows_affected()==1`). Do the same for the shipment row (`... WHERE id=$1 AND status=$old`). Pass the freshly locked status into the sync instead of a pre-transaction pool read. ---

### HI-50 — DEV callbacks are verified even when dev mode is OFF, with an empty-string HMAC key

- **Severity:** High | **Area:** Payments | **File:** `crates/api/src/payments.rs:290-299`
- **Found by:** agent-02 (?)
- **Impact:** Turning `PAYMENTS_DEV_MODE` off does **not** disable the DEV webhook path. It only disables the adapter factory and `dev_simulate_success` (which does check `config.dev_mode` at `payments.rs:502`). If the production DB ever contains a `DEV-` row, an attacker forges `success` with the empty-key HMAC and marks that order `paid`. The "off" switch is therefore not a kill-switch …
- **Evidence:** // crates/api/src/payments.rs:290-299 — no config.dev_mode check anywhere in this branch let signature_valid = match payment.gateway_ref.as_deref() { Some(reference) if is_dev_reference(reference) => verify_dev_callback( &config.dev_secret, reference, amount_poisha, callback_status, signature, ), _ => false, }; `verify_dev_callback` (`crates/payments/src/lib.rs:406-420`) accepts any key length, including `""`; HMAC with an empty key is computable by anyone. -
- **Fix:** In `apply_payment_callback`, return 401 immediately unless `config.dev_mode` is true for a `DEV-` reference (and unless `dev_secret` is non-empty and of minimum length). Better: delete/purge `DEV-` rows on any non-dev boot, and add a DB check/constraint or startup assertion that no `DEV-` `gateway_ref` exists when dev mode is off.

### HI-51 — Callback never validates the payment amount against the order total; an advance-amount mismatch marks a COD order fully `paid`

- **Severity:** High | **Area:** Payments | **File:** `crates/api/src/payments.rs:336-340`
- **Found by:** agent-02 (?)
- **Impact:** The webhook is the trust boundary for "money arrived", yet it trusts the `payments.amount_poisha` row as the definition of "full price". If that row is ever wrong — a checkout bug, a loyalty/coupon change after checkout, a partial-payment row, or a direct DB/manual error — a callback for the (small) row amount marks the whole order `paid` and the courier collects nothing …
- **Evidence:** // 336-340: the ONLY amount check — payment row vs callback, never vs the order if !validate_callback_amount(payment.amount_poisha, amount_poisha) { return Err(ApiError::bad_request("callback amount does not match the payment amount")); } // 386-389: advance is recognised only by amount equality with advance_required let is_advance_payment = order_facts.0 == "cod" && order_facts.2 > 0 && payment.amount_poisha == order_facts.2 && order_facts.1 == "advance_pending"; // 409-416: anything that is NOT recognised as an advance falls through to a full 'paid' } else { sqlx::query("UPDATE orders SET payment_status = $2 WHERE id = $1") .bind(payment.order_id).bind(target) // target == "paid" …
- **Fix:** In `apply_payment_callback`, load `orders.grand_total_poisha` and enforce: for a non-COD order, `payment.amount_poisha == grand_total_poisha` or reject 400/409; for a COD order, `payment.amount_poisha == advance_required_poisha` and the order must be `advance_pending`, otherwise reject — never fall through to generic `paid` for a COD order. Add a DB `CHECK` / trigger or an application invariant test covering both branches.

### HI-52 — Refund is unreachable: `refunded` exists in the schema and transition table, but no code ever sets it and `refund()` is never called

- **Severity:** High | **Area:** Payments | **File:** `migrations/0009_create_payments.sql:15`
- **Found by:** agent-02 (?)
- **Impact:** After a real refund (bKash/cash, per `returns.rs`), `payments.status` stays `paid` and `orders.payment_status` stays `paid`. Anything downstream that keys off `paid` — seller payout eligibility, revenue analytics (`analytics.rs` counts `status='paid'`), shipment/COD-netting — treats refunded money as still collected. A seller can be paid out for an order the buyer was refunded …
- **Evidence:** `grep -rn "PaymentAdapter::refund|\.refund(" crates/` returns only the trait definition and the three adapter implementations — zero call sites. The webhook's target mapping (`payments.rs:327-335`) accepts only `"success" → "paid"` and `"failed" → "failed"`; `"refunded"` is rejected 400. Meanwhile `returns.rs:640-688` (out of scope, cited as consumer fact) marks a return's `refund_status='done'` as an off-platform act and never touches `payments` or `orders.payment_status`. -
- **Fix:** Wire one refund path: an admin-only endpoint/function that (in a transaction) calls the gateway adapter's `refund(reference, amount)` **and only on gateway success** transitions `payments paid → refunded` and `orders paid → refunded`, storing `refund_amount_poisha` + `refunded_at`. Until the live gateway refund exists, the off-platform `refund-done` handler must update both payment tables in the same transaction. Add `refunded_at timestamptz` and `refund_amount_poisha bigint CHECK (>=0)` …

### HI-53 — Deterministic, predictable `gateway_ref`, no uniqueness, newest-wins lookup shadows older attempts

- **Severity:** High | **Area:** Payments | **File:** `crates/payments/src/lib.rs:341-355`
- **Found by:** agent-02 (?); agent-13 (H2); agent-20 (M6)
- **Impact:** Anyone who knows an order UUID (exposed in order views / URLs) knows its DEV `gateway_ref` — the exact value the webhook keys on and the HMAC signs over (compounds C-01: the attacker needs no DB access to target an order). Separately, because there is no `UNIQUE(gateway_ref)` and no `UNIQUE(order_id)` guard in `insert_pending_payment`, a second pending row for the same order …
- **Evidence:** // lib.rs:346: reference is a pure function of method + order id — no randomness, no nonce let reference = format!("DEV-{}-{}", self.method.as_str(), order_id); // lib.rs:215: same for COD reference: format!("COD-{order_id}"), // 0009:25,32: no UNIQUE, no NOT NULL gateway_ref text, CREATE INDEX payments_gateway_ref_idx ON payments (gateway_ref); // payments.rs:274-276: lookup picks whichever row is newest "SELECT {PAYMENT_COLUMNS} FROM payments WHERE gateway_ref = $1 ORDER BY created_at DESC LIMIT 1" -
- **Fix:** Mint refs with cryptographic randomness, e.g. `DEV-<method>-<order_id>-<32 hex chars from rand>`, store the gateway's own transaction id in a separate `gateway_txn_id` column, add `UNIQUE(gateway_ref)` (partial unique where not null) and a partial unique index preventing two `pending` rows for one `order_id`, and look up by `(gateway_ref, method)` / by payment `id` where possible.

### HI-54 — No real-gateway signature verification, no server-side reconciliation: the webhook body is the sole source of truth

- **Severity:** High | **Area:** Payments | **File:** `crates/api/src/payments.rs:1-11`
- **Found by:** agent-02 (?)
- **Impact:** (a) Today a merchant who configures bKash/Nagad/SSLCommerz/ShurjoPay credentials gets `payment_methods() → mode:"live"` (available) yet checkout fails with 503 (`NotImplemented` from `initiate`) — the API advertises a live method it cannot take money with. (b) When live verification is added, the current webhook shape (`WebhookBody { payment_ref, amount_poisha, status, …
- **Evidence:** // payments.rs:8-11: "Real gateway signature verification is NOT implemented ... so callbacks // for non-dev payments are logged and rejected (401) rather than trusted." // lib.rs gateway_adapter macro: with credentials, every op is NotImplemented fn check(&self) -> Result<(), PaymentError> { if gateway_credentials_complete(...) { Err(PaymentError::NotImplemented(...)) } else { Err(PaymentError::NotConfigured(...)) } } `verify()` is never called after a callback, there is no gateway transaction id stored (`payments` has no such column), no currency, and no reconciliation job. -
- **Fix:** Do not enable any `live` mode until per-gateway verification lands (bKash/Nagad/SSLCommerz/ShurjoPay each with its documented signature/IPN scheme), each verified against that gateway's own credentials, **plus** a server-side `verify(reference)` / transaction-status query to the gateway before marking `paid`, storing the gateway txn id + currency. Keep the fail-closed default until then, and make `payment_methods()` report `live` only when both credentials *and* verification are implemented …

### HI-55 — Earnings vanish when a delivered order is returned, but an already-paid payout is never clawed back (platform pays twice)

- **Severity:** High | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/payouts.rs:196-214`
- **Found by:** agent-04 (H1); agent-17 (?)
- **Impact:** Order delivered → seller requests and is paid a payout covering it → buyer returns the goods and `mark_return_received` (`returns.rs`) flips the sub-order to `returned` and opens a buyer refund (`refund_status = 'pending'`). Earned drops by the sub-order's payout, `paid_out` does not, and the clamp silently reports available = 0 instead of a negative (owed) balance. The …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/payouts.rs:196-214` (`seller_earnings_on`); `migrations/0012_create_payouts.sql:19-21` - **Code evidence:** -- seller_earnings_on: earned only while status = 'delivered' (SELECT COALESCE(SUM(seller_payout_poisha), 0) FROM sub_orders WHERE store_id = $1 AND status = 'delivered') AS earned_poisha, -- paid_out counts approved+paid payouts; rejected payouts count nowhere (SELECT COALESCE(SUM(amount_poisha), 0) FROM payouts WHERE store_id = $1 AND status IN ('approved', 'paid')) AS paid_out_poisha, `available_balance` (`p
- **Fix:** Do not let delivered earnings become immediately payable, or make the ledger able to go negative: e.g. a payout-eligibility holdback (only sub-orders delivered > N days ago, or past the return window, count toward `available`), and/or track a per-seller negative balance / debt that future earnings must repay before new payouts. At minimum remove the silent `.max(0)` clamp from the *gating* path so an overpaid seller cannot request further payouts, and record the shortfall.

### HI-56 — Cancelling an order launders expiring loyalty points into permanent points (expiry bypass)

- **Severity:** High | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/loyalty.rs:200-231`
- **Found by:** agent-04 (H2)
- **Impact:** Buyer holds points expiring tomorrow. They redeem them on an order (checkout writes a `redeem` row), then cancel the order while it is still `placed` (buyer cancel, `orders.rs:1416-1490`) or get the seller to cancel it while `placed`/`confirmed`. The refund returns the points as an `adjust` row with no expiry, so the points are now spendable forever. Repeat near every expiry …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/loyalty.rs:200-231` (`refund_for_cancelled_sub_order`); `crates/api/src/loyalty.rs:75-103` (`loyalty_balance_on`) - **Code evidence:** // refund_for_cancelled_sub_order: refund written as 'adjust' with NO expiry insert_ledger_row(conn, buyer_user_id, refund_points, "adjust", sub_order_id, None).await // insert_ledger_row passes expires_at through; here it is None. // Balance: adjust rows are netted unconditionally, never expiry-filtered: // COALESCE(SUM(delta) FILTER (WHERE reason IN ('redeem', 'adjust')), 0) AS net_moves /
- **Fix:** Give refund `adjust` rows the expiry of the points they refund — at minimum the earliest unexpired earn lot's `expires_at`, or the original redemption's remaining lifetime. Better: track per-lot expiry (FIFO) so a refund restores points with their original expiry instead of minting immortal ones.

### HI-57 — Loyalty earn is calculated on value the buyer paid with points (points-on-points)

- **Severity:** High | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/loyalty.rs:154-180`
- **Found by:** agent-04 (H3); agent-17 (?)
- **Impact:** Goods 200,000 poisha (2,000 taka) with a 50%-cap redemption: buyer redeems 1,000 points for 100,000 poisha off, pays only 100,000 in cash, yet earns `loyalty_points_for_goods(200,000 − 0) = 20` points instead of 10. Every redemption cycle mints points on money the platform, not the buyer, paid. Over repeated buy→redeem→earn cycles the buyer extracts a permanent …
- **Evidence:** - **Severity:** High (Medium-High value leak; rated High because it is systematic and platform-funded) - **File:line:** `crates/api/src/loyalty.rs:154-180` (`earn_for_sub_order`) - **Code evidence:** let row: Option<(Uuid, i64, i64)> = sqlx::query_as( "SELECT o.buyer_user_id, so.goods_subtotal_poisha, so.discount_poisha \ FROM sub_orders so JOIN orders o ON o.id = so.order_id WHERE so.id = $1", ) // ... let points = loyalty_points_for_goods(goods_poisha - discount_poisha); The query selects only `goods_subtotal_poisha` and the *coupon* `discount_poisha`. It never r
- **Fix:** `SELECT ..., so.loyalty_discount_poisha` and compute `loyalty_points_for_goods(goods_poisha - discount_poisha - loyalty_discount_poisha)`.

### HI-58 — Re-subscribing charges full price again with no duplicate / double-click guard (no idempotency)

- **Severity:** High | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/subscriptions.rs:339-444`
- **Found by:** agent-04 (H4)
- **Impact:** A seller already on Pro with 29 days left who clicks Subscribe → Pro again (double-click, impatient retry, or a client retry after a network timeout — the endpoint returns 201 with no idempotency key) is charged the full 49,900 poisha again and their remaining 29 days are discarded, period reset to 30 days from now. The same double-charge happens on any retried request. The …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/subscriptions.rs:339-444` (`subscribe`) - **Code evidence:** // No check of the existing row's plan/status/expiry before charging: let price = effective_subscription_price(state, plan).await; let mut tx = pool.begin().await ... // lock store, check balance // Upsert: fresh 30-day period from now, full price, no proration "INSERT INTO store_subscriptions (store_id, plan, status, started_at, expires_at, auto_renew) VALUES ($1, $2, 'active', now(), now() + make_interval(days => $3), true) ON CONFLICT (store_id) DO UPDATE SET
- **Fix:** If the store already has an `active`, unexpired row on the *same* plan, return the existing subscription (idempotent no-op, 200) instead of charging; for a genuine plan change keep the documented full-price/no-proration rule but require an explicit confirmation flag. Add an `Idempotency-Key` header (unique on a request key) for the subscribe endpoint, as for payouts (C1).

### HI-59 — Admin payout transitions race: two admins can apply an illegal transition (e.g. approved → rejected)

- **Severity:** High | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/admin.rs:622-656`
- **Found by:** agent-04 (H5); agent-06 (?); agent-20 (H2)
- **Impact:** Two admins (or a double-clicked approve/reject) open the same `requested` payout concurrently. Both read `requested`, both pass `can_transition_payout`, both `UPDATE`. The second `UPDATE` blocks on the row lock, then applies its transition on top of the first's committed state without re-validating: `approved → rejected` and `rejected → paid` become possible, and the audit log …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/admin.rs:622-656` (`transition_payout`) — the admin half of the in-scope payout lifecycle (`payouts.rs` module doc: "Approval, rejection and payment are platform operations driven by the admin endpoints … through the same domain transition table") - **Code evidence:** let current: Option<String> = sqlx::query_scalar("SELECT status FROM payouts WHERE id = $1") // NO FOR UPDATE .bind(payout_id).fetch_optional(&mut *tx).await ...; if !can_transition_payout(&current, target_status) { return Err(conflict) } // ... then,
- **Fix:** `SELECT status FROM payouts WHERE id = $1 FOR UPDATE`, and make the update conditional — `UPDATE payouts SET ... WHERE id = $1 AND status = $2` (the pre-checked status) — returning 409 when zero rows are affected. Re-check after acquiring the lock. ---

### HI-60 — Facebook import bypasses the subscription product cap (cap enforced in only one of two write paths)

- **Severity:** High | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/products.rs:422-470`
- **Found by:** agent-05 (H1); agent-11 (H4); agent-18 (M4)
- **Impact:** A free-plan store (cap 50, `domain::plan_product_cap`: free 50 / pro 500 / business `i64::MAX`) that is blocked at `POST /v1/products` can create unlimited products by repeating `POST /v1/seller/products/import` with 50 drafts per call. The cap is a paid-plan boundary, so this is direct revenue/subscription bypass, not just a race (contrast the documented count-then-insert …
- **Evidence:** // products.rs:452-459 — only the manual create path checks the cap let cap = crate::subscriptions::product_cap_for_store(state, input.store_id).await?; let product_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE store_id = $1") /* … */; if product_count >= cap { return Err(ApiError::forbidden(/* upgrade … */)); } // products.rs:365 — the shared insert has no cap, no caller check is enforced here pub async fn insert_product<'e, E>(executor: E, input: &NewProduct) -> Result<Uuid, ApiError> // facebook.rs:295,374 — import caps only the batch size, then inserts in a loop if drafts.len() > MAX_IMPORT_ITEMS { /* at most 50 per import */ } created_ids.push(insert_product(&mut …
- **Fix:** Enforce the cap inside `insert_product` (pass the cap / a `check_cap: bool` flag, or move the count check into a single `create_product_tx` used by both paths), or add the identical `product_cap_for_store` + count check to `import_products_for_owner` before the transaction, counting the batch (`product_count + ready.len() > cap` → reject/partial-skip). Add a regression test: store at cap → import returns 0 created.

### HI-61 — Product listing search (`q`) is NOT LIKE-escaped: `%` / `_` act as wildcards

- **Severity:** High | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/products.rs:241-246`
- **Found by:** agent-05 (H2); agent-19 (M1)
- **Impact:** `GET /v1/products?q=%` matches every title (full catalogue dump one page at a time, bypassing any intended keyword narrowing); `q=_` matches any single character; `q=50% off` / `q=a_b` match far more than the literal text. An unauthenticated caller can enumerate titles and defeat search narrowing/filtering that downstream clients or rate-limiting assume. Same flaw does not …
- **Evidence:** // products.rs:244-246 — raw interpolation into the LIKE pattern, no escaping let pattern: Option<String> = q .map(str::trim).filter(|s| !s.is_empty()) .map(|s| format!("%{s}%")); // products.rs:213 — consumed by ILIKE "($1::text IS NULL OR p.title ILIKE $1 OR p.title_bn ILIKE $1) …" Contrast the correct path in the same codebase, `crates/api/src/search.rs:98` (`params.q.as_deref().map(search_like_pattern)`) where `domain::search_like_pattern` escapes `\`, `%`, `_` (`domain/src/lib.rs:2295-2312`, with tests). -
- **Fix:** Reuse the domain helper in `list_products`: `.map(|s| search_like_pattern(s))` (import it from `domain`), and add a test that `q="%"` and `q="_"` match only literal `%`/`_` titles.

### HI-62 — Also-bought counts cancelled / unpaid / never-delivered orders as co-purchases

- **Severity:** High | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/recommend.rs:170-196`
- **Found by:** agent-05 (H3)
- **Impact:** Place orders pairing product A with product B, then cancel them (or never pay COD) — they still increment `co_count` and push B to the top of A's also-bought rail, manufacturing social proof for any product. A seller can boost their own product, or demote/boost a competitor's pairing, with zero completed sales and zero cost beyond cancelled checkouts. -
- **Evidence:** // recommend.rs:172-184 — no join to orders, no status filter at all WITH co AS ( SELECT oi2.product_id AS product_id, COUNT(DISTINCT so1.order_id) AS co_count FROM order_items oi1 JOIN sub_orders so1 ON so1.id = oi1.sub_order_id JOIN sub_orders so2 ON so2.order_id = so1.order_id AND so2.id <> so1.id JOIN order_items oi2 ON oi2.sub_order_id = so2.id WHERE oi1.product_id = $1 AND oi2.product_id <> $1 GROUP BY oi2.product_id) Contrast `recommend.rs:114-122` (`NOT_BOUGHT_BY_CALLER` excludes `so.status = 'cancelled'`) and `recommend.rs:228,293` (personalised signals require `so.status = 'delivered'`). -
- **Fix:** Join `orders` and require a completed/paid/delivered sub-order status, e.g. `JOIN orders o ON o.id = so1.order_id … AND so1.status IN ('delivered','paid',…)` (use the same delivered/paid vocabulary checkout uses) on both `so1` and `so2`, and count only `DISTINCT` orders meeting it. Backfill/recompute is not needed — the count is computed live.

### HI-63 — Effective-stock `SUM(...)::int` overflows: one seller's variants can 500 the whole catalogue list / search / detail

- **Severity:** High | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/products.rs:148-158`
- **Found by:** agent-05 (H4)
- **Impact:** A seller creates e.g. two active variants with stock `i32::MAX` each (allowed — see M11) on one product. Every `GET /v1/products`, `GET /v1/search`, `GET /v1/products/:id`, related/recommendations query touching that product now fails with a 500 (`ApiError::internal("list products" / "search products" / "load product", …)`), i.e. one store can take down the shared marketplace …
- **Evidence:** // products.rs:152-155 THEN (SELECT COALESCE(SUM(pv2.stock), 0)::int FROM product_variants pv2 WHERE pv2.product_id = p.id AND pv2.is_active) // variants stock column: migration 0031 — stock integer CHECK (stock >= 0), no per-product total cap // variants.rs:289-312 — create allows stock up to i32::MAX per variant, no variant-count cap (see M7) Postgres `SUM(integer)` returns `bigint`; the `::int` cast raises `integer out of range` when the sum exceeds 2,147,483,647. `PRODUCT_COLUMNS` is used by every list/search/detail/related query, so the error aborts the whole query, not just that product's row. -
- **Fix:** Cast to `bigint` and clamp in SQL (`LEAST(COALESCE(SUM(pv2.stock),0), 2147483647)::int`, or change the `Product.stock` / `InventoryItem.stock` type to `i64`), and cap total variant stock per product at create/adjust time (e.g. reject if the sum would exceed `i32::MAX`). Add a test with two `i32::MAX` variants. ---

### HI-64 — Refund over-pays the loyalty-paid slice in cash and never restores redeemed points

- **Severity:** High | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/returns.rs:513-542`
- **Found by:** agent-09 (H2); agent-17 (?)
- **Impact:** Buyer redeems points for a `loyalty_discount_poisha` slice at checkout, so cash actually paid is `goods − coupon − loyalty_discount`. On return the platform records a cash refund of `goods − coupon` — over-paying by exactly `loyalty_discount_poisha` in cash — while the redeemed points are never credited back. Points (expiring, redeemable only under loyalty rules) are silently …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/returns.rs:513-542` (query) and `:542` - **Code evidence:** // mark_return_received loads only goods + coupon discount — loyalty is never read: "SELECT rr.status, s.owner_user_id, rr.store_id, rr.sub_order_id, \ so.goods_subtotal_poisha, so.discount_poisha \ FROM return_requests rr … JOIN sub_orders so ON so.id = rr.sub_order_id …" let refund_poisha = compute_refund_poisha(goods_poisha, discount_poisha); // domain: (goods_poisha - discount_poisha).max(0) — crates/domain/src/lib.rs:2360 Contrast the COD path, which *do
- **Fix:** Select `so.loyalty_discount_poisha` in `mark_return_received`, compute `refund = (goods − coupon_discount − loyalty_discount).max(0)`, and call the loyalty redemption-refund hook (`refund_for_cancelled_sub_order`-equivalent, or generalise it) in the same transaction so redeemed points are restored instead of being paid out in cash. Add a regression test with a loyalty-discounted sub-order.

### HI-65 — Manual shipment status is pure seller self-attestation with no evidence and no audit — fake delivery/return unlocks invoice, loyalty and restock

- **Severity:** High | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/shipments.rs:1343-1399`
- **Found by:** agent-09 (H3)
- **Impact:** A seller (or a `staff` member — see M3) marks a manual parcel `delivered` without shipping anything. That single self-asserted move, via `sync_sub_order_with_shipment_status`, (a) sets the sub-order `delivered`, (b) credits the buyer's loyalty earn (`loyalty::earn_for_sub_order`), (c) sends the `sub_order_delivered` notification, (d) makes the sub-order eligible for a buyer …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/shipments.rs:1343-1399` (`set_manual_shipment_status`) - **Code evidence:** pub async fn set_manual_shipment_status(state, user, shipment_id, target_status) { let (shipment, sub_status) = load_shipment_for_seller(pool, user, shipment_id).await?; require_manual_shipment(&shipment)?; if !is_valid_shipment_status(target_status) { /* 400 */ } if !can_transition_shipment(&shipment.status, target_status) { /* 409 */ } // … UPDATE shipments SET status = $2 …; INSERT INTO shipment_events … 'manual' … sync_su
- **Fix:** Treat manual status moves as *claims*, not facts: record every manual move in an immutable audit log (actor, shipment, from→to, timestamp, IP/user-agent if available); require a receipt/reference or a buyer-delivery confirmation for the `→ delivered` move (or at minimum flag manual-delivered sub-orders for admin review before loyalty earn / invoice issuance); rate-limit and alert on implausibly fast manual journeys (e.g. `created → delivered` in minutes); consider requiring the counter receipt …

### HI-66 — Buyer export leaks platform commission and seller payout per sub-order

- **Severity:** High | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/exports.rs:90-111`
- **Found by:** agent-09 (H4)
- **Impact:** Any signed-in buyer calls `GET /v1/buyer/export` and receives, for every one of their orders, the exact commission the platform took and the exact payout each seller received per sub-order — commercially sensitive platform/seller economics, usable to pressure sellers, undercut pricing, or infer the platform's commission schedule across stores. -
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/exports.rs:90-111` (shared row/columns) and `:374-420` (`buyer_export`) - **Code evidence:** // The ONE row type/column list is shared by BOTH exports: struct ExportSubOrderRow { … commission_poisha: i64, seller_payout_poisha: i64, … } const EXPORT_SUB_ORDER_COLUMNS: &str = "… commission_poisha, seller_payout_poisha, created_at"; // buyer_export nests exactly these rows under the buyer's orders: // let sub_orders = nest_items(sub_order_rows, items); // no field stripping The buyer receipt deliberately excludes these fie
- **Fix:** Give the buyer export its own row type/column list without `commission_poisha` / `seller_payout_poisha` (or strip them in `nest_items` for the buyer path). Add a test asserting the buyer export JSON contains neither key.

### HI-67 — Store-scoped seller export hands a non-owner manager the owner's full payout account numbers

- **Severity:** High | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/exports.rs:191-200`
- **Found by:** agent-09 (H5)
- **Impact:** A store `manager` (a team member, not the owner) calls `GET /v1/seller/export?store_id=<store>` and downloads every payout row for that store with the owner's complete bank / bKash / Nagad account numbers and account names — enough for social-engineering, account-takeover attempts, or exfiltration after a single compromised/low-trust team account. The owner-only export is …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/exports.rs:191-200`, `:238-246`, `:485-506` - **Code evidence:** // payouts are selected verbatim, full account number included: let payouts: Vec<Payout> = sqlx::query_as( "SELECT id, store_id, amount_poisha, method, account_name, account_number, \ status, note, created_at, processed_at FROM payouts WHERE store_id = ANY($1) …"); // Payout struct (payouts.rs:155-167) carries `account_number: String` in full. // The scoped handler resolves the caller under VIEW_FINANCES, not ownership: let (store, _role) = crate::acc
- **Fix:** Mask `account_number` (e.g. last 3–4 chars only, via `mask_phone`-style helper) for any scoped/non-owner export, or exclude `payouts` entirely from the scoped document and keep full payout data in the owner-only export. Pass the caller's role into `build_seller_export` and branch on it; add a test with a manager caller.

### HI-68 — Booking TOCTOU: guards run outside the transaction, then the sub-order is forced to `shipped` unconditionally — a cancelled sub-order can still ship

- **Severity:** High | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/shipments.rs:513-600`
- **Found by:** agent-09 (H6)
- **Impact:** Seller books a courier; while the adapter call is in flight the buyer cancels (or the seller cancels) the sub-order, or it moves out of `confirmed` another way. Booking still commits, the parcel ships, the courier collects the COD amount computed from the now-cancelled order, and the sub-order is forced back to `shipped`. The duplicate-booking 409 also relies only on the …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/shipments.rs:513-600` (`load_booking_row`), `:634-745` (`create_shipment_for_seller`), `:777-855` (`create_manual_shipment_for_seller`) - **Code evidence:** // Guards (confirmed? advance paid? no shipment yet?) run in load_booking_row on the // pool, OUTSIDE any transaction. Then, after a network call to the courier adapter: let booked = adapter.create_shipment(&request).await…; // seconds-long window let mut tx = pool.begin().await…; // … INSERT INTO shipments … (unique constraint is the only backstop) … sqlx::query("UPD
- **Fix:** Inside the booking transaction, `SELECT … FROM sub_orders WHERE id=$1 FOR UPDATE`, re-check `status = 'confirmed'` and no-shipment, and make the status move conditional (`UPDATE sub_orders SET status='shipped' WHERE id=$1 AND status='confirmed'`, abort on 0 rows). Apply identically to the manual booking path.

### HI-69 — `mark_refund_done` race: status check has no lock and the update has no status predicate — two admins can record the same off-platform refund twice

- **Severity:** High | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/returns.rs:644-690`
- **Found by:** agent-09 (H8)
- **Impact:** Two admins (or one admin double-clicking / retrying) open the refund queue for the same `pending` refund concurrently. Both read `pending`, both `UPDATE` to `done`, both write a `return.refund_done` audit row. The platform's record now asserts the off-platform bKash/cash refund happened twice, inviting a double payout to the buyer (or a false double-payment claim), with the …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/returns.rs:644-690` - **Code evidence:** let refund_status: Option<String> = sqlx::query_scalar("SELECT refund_status FROM return_requests WHERE id = $1") .bind(return_id).fetch_optional(&mut *tx).await…; // NO `FOR UPDATE` if refund_status != "pending" { return Err(conflict(…)); } sqlx::query("UPDATE return_requests SET refund_status = 'done' WHERE id = $1") .bind(return_id)…; // NO `AND refund_status = 'pending'` record_admin_action(&mut *tx, admin_id, "return.refund
- **Fix:** `SELECT refund_status … FOR UPDATE` (or `UPDATE return_requests SET refund_status='done' WHERE id=$1 AND refund_status='pending'` and abort when 0 rows are affected). Add a concurrent double-submit regression test. ---

### HI-70 — Deterministic double restock: courier/manual return + buyer-return receipt restock the same items twice

- **Severity:** High | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/returns.rs:553-559`
- **Found by:** agent-09 (H1)
- **Impact:** (1) Buyer requests a return while the sub-order is `delivered`. (2) The parcel also comes back on the courier/manual path — seller marks the manual shipment `delivered → return_in_transit → returned`, or a courier refresh reports `returned` — which restocks the items once and sets the sub-order `returned`. (3) The seller then approves / marks the still-open return request …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/returns.rs:553-559` (and `crates/api/src/shipments.rs:877-930`) - **Code evidence:** // returns.rs mark_return_received — NO check of the sub-order's current status, // NO lock on the sub-order row (only `FOR UPDATE OF rr`, returns.rs:515-521): sqlx::query("UPDATE sub_orders SET status = 'returned' WHERE id = $1") .bind(sub_order_id) /* … */; restock_sub_order_items(&mut tx, sub_order_id, "return_restock").await?; `restock_sub_order_items` (`crates/api/src/orders.rs:1348-1408`) is an unconditional `UPDATE products SE
- **Fix:** In `mark_return_received`, lock the sub-order row (`SELECT … FROM sub_orders … WHERE id=$1 FOR UPDATE`), require its status to be `delivered` (409 otherwise), and make the status move conditional (`UPDATE sub_orders SET status='returned' WHERE id=$1 AND status='delivered'`, abort if 0 rows affected). Add a DB-level guard: a unique partial index / `stock_movements` uniqueness on `(sub_order_id, reason)` or an idempotency key so a second restock of the same sub-order is impossible from any path.

### HI-71 — Seller can 5-star their own product; no self-review, payment, or non-self-purchase check (fake-review bypass)

- **Severity:** High | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/reviews.rs:225-260`
- **Found by:** agent-08 (?); agent-18 (M3)
- **Impact:** A seller buys their own product (second phone/account, or even their own account if the store allows self-purchase), gets the sub-order to `delivered` — note the seller themselves can drive a manual shipment to delivered via `POST /v1/seller/shipments/:id/manual-status` (`shipments.rs:1435`, `set_manual_shipment_status`) — then posts a 5-star review. `PRODUCT_RATING_JOIN` …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/reviews.rs:225-260` (`create_review`), eligibility `crates/api/src/reviews.rs:177-193` (`find_delivered_order_for_review`) - **Code evidence:** // reviews.rs:225-237 — the ONLY checks before INSERT: pub async fn create_review(pool, user, product_id, rating, body) -> Result<Review, ApiError> { if !validate_rating(rating) { return Err(ApiError::bad_request("rating must be between 1 and 5")); } validate_review_body(body.unwrap_or("")).map_err(ApiError::bad_request)?; get_product_by_id(pool, product_id).await?;
- **Fix:** In `create_review`, after loading the product/store, reject `store.owner_user_id == user.user_id` (403) and reject any user with a `store_role` in that store. Add a `is_verified_purchase` / exclude-self rule to the rating aggregate, and require the delivered order to be paid (join payments / order payment status) before it confers eligibility. Consider a cooling-off / minimum-order-value rule for first reviews.

### HI-72 — No rate limiting on any JWT-authenticated write in scope (spam / flood / DB exhaustion)

- **Severity:** High | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/reviews.rs:474-535`
- **Found by:** agent-08 (?)
- **Impact:** One authenticated account (cheap phone+OTP signup) can hammer `POST /v1/products/:id/questions` (unlimited questions per product, qna.rs:221), `POST /v1/conversations/:id/messages` (2000 chars each, messaging.rs:506), `POST /v1/conversations` (new thread per store/product), `PATCH /v1/reviews/:id` (unlimited rating flips), and wishlist add/remove churn, flooding seller …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/reviews.rs:474-535`, `crates/api/src/qna.rs:534-626`, `crates/api/src/wishlist.rs:146-182`, `crates/api/src/messaging.rs:919-1059` (all handlers); `crates/api/src/lib.rs` (router has no per-route limiter); `crates/api/src/api_keys.rs:140-159` (only API keys are limited) - **Code evidence:** A repo-wide grep for `rate_limit|governor|throttle` in `lib.rs` finds only the API-key limiter (`api_key_rate_limit_per_min`, `ApiKeyRateLimiter`) and the OTP throttle in `auth.rs`. No handler in the four in-scope files calls any limiter, cou
- **Fix:** Add a per-user (and per-IP) rate limiter as middleware or in each service function — e.g. max N questions / messages / reviews / conversations per minute/hour, with 429 responses — persisted in Postgres/Redis (not in-memory, so restarts and multiple instances share it; cf. the documented in-memory limitation of `ApiKeyRateLimiter`, api_keys.rs:165-172). Add stricter per-target caps (e.g. 1 question per user per product per hour, 1 message per conversation per few seconds).

### HI-73 — SIDRA callback has no replay protection and no idempotency: one valid signed payload can be replayed forever

- **Severity:** High | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/messaging.rs:620-720`
- **Found by:** agent-08 (?)
- **Impact:** Anyone who observes or obtains one valid callback (compromised/leaked SIDRA credentials, logged webhook, MITM on a non-TLS hop, a former SIDRA operator) can replay the identical JSON — same `conversation_id`, `in_reply_to_message_id`, `reply_text`, `signature` — indefinitely while the conversation stays `open`, each replay inserting another identical `ai` message (or another …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/messaging.rs:620-720` (`apply_sidra_reply`), `crates/domain/src/lib.rs:1709-1715` (`sidra_signature_payload`) - **Code evidence:** // messaging.rs:634-645 — body validated, then signature checked; nothing else: validate_message_body(reply_text).map_err(ApiError::bad_request)?; if !verify_sidra_callback(secret, &conversation_id.to_string(), &in_reply_to_message_id.to_string(), reply_text, signature) { return Err(ApiError::unauthorized("invalid SIDRA signature")); } // ... then unconditionally INSERTs a new `ai`
- **Fix:** Add `timestamp` (reject if older than e.g. 5 min) and a `nonce` / `Idempotency-Key` to the signed payload and verify both; store processed `(conversation_id, in_reply_to, body_hash)` / idempotency keys with a `UNIQUE` constraint and use `INSERT ... ON CONFLICT DO NOTHING` (return the existing message/suggestion on conflict). Reject a callback whose `in_reply_to` message is not a `buyer` message (currently any message in the conversation qualifies, messaging.rs:683-695).

### HI-74 — `send_suggestion` is a check-then-act race with no transaction: double-send duplicates the AI message

- **Severity:** High | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/messaging.rs:829-863`
- **Found by:** agent-08 (?)
- **Impact:** Two concurrent `POST /v1/seller/sidra/suggestions/:id/send` requests (double-click, retry, two staff) both pass the `pending` check, both call `insert_message`, and both mark `sent` — the buyer receives the same AI reply twice. If the `UPDATE` fails after `insert_message` succeeds, the suggestion stays `pending` and the next send duplicates it again. Same pattern can strand a …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/messaging.rs:829-863` (`send_suggestion`) - **Code evidence:** // messaging.rs:829-856 — two separate, non-atomic steps: let suggestion = load_suggestion_for_owner(pool, user, suggestion_id).await?; if suggestion.status != "pending" { return Err(ApiError::conflict("suggestion was already decided")); } let conversation = load_conversation_row(pool, suggestion.conversation_id).await?; if conversation.status != "open" { return Err(ApiError::conflict("conversation is closed")); } insert_message(pool, suggestion.conversation_id, "
- **Fix:** Wrap the whole decision in one transaction: `SELECT ... FOR UPDATE` on the suggestion row (or an atomic `UPDATE sidra_suggestions SET status = 'sent' ... WHERE id = $1 AND status = 'pending' RETURNING ...` and proceed only if it returns a row), then insert the message in the same transaction. Do the same for `insert_message` (message INSERT + conversation bump in one transaction) and for `dismiss_suggestion` (messaging.rs:865-).

### HI-75 — Seller can undo an admin hide, and seller hides/unhides are unaudited (moderation override + censorship)

- **Severity:** High | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/qna.rs:358-379`
- **Found by:** agent-08 (?)
- **Impact:** (a) Admin hides a spam/abusive question; the seller (or any manager/staff with `answer_questions`, see qna.rs:301-327) immediately calls `POST /v1/seller/questions/:id/unhide` and it is public again — admin moderation is advisory only. (b) A seller can hide legitimate critical questions ("Is this original?", "Why was my order late?") to censor their product page, with no audit …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/qna.rs:358-379` (`set_question_hidden`), `crates/api/src/qna.rs:566-586` (seller hide/unhide handlers), `crates/api/src/qna.rs:381-414` (`admin_set_question_hidden`) - **Code evidence:** // qna.rs:566-586 — seller hide/unhide: only require_question_store, then the bare flag flip: pub async fn hide_question_handler(...) { require_question_store(pool, &user, id).await?; Ok(Json(set_question_hidden(pool, id, true).await?)) } pub async fn unhide_question_handler(...) { /* identical, with `false` */ } // qna.rs:358-3
- **Fix:** Add `hidden_by_user_id`, `hidden_by_admin` (or `hidden_reason` / `hidden_at`) columns; make seller-unhide refuse (403) a question hidden by an admin; route *all* hide/unhide writes through the audited transactional path (`record_admin_action` / an equivalent seller-action audit row) so every flip is attributable.

### HI-76 — Reviews have no moderation, reporting, or hiding at all: an abusive/defamatory review is permanent unless its author deletes it

- **Severity:** High | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/reviews.rs`
- **Found by:** agent-08 (?)
- **Impact:** A competitor or extortionist posts a 1-star review with abuse, false claims, phone numbers, or a phishing link on a rival's product. The seller can only reply once (reviews.rs:392); they cannot hide it, report it, or ask an admin to remove it through any endpoint in this codebase. The review stays public and drags the product's public average indefinitely. The only removal …
- **Evidence:** - **Severity:** High - **File:line:** Absence across `crates/api/src/reviews.rs` (no `is_hidden`, `status`, `report`, `flag`, or admin function) and the route table `crates/api/src/lib.rs:1351-1363` (only create/list/update/delete/reply routes) - **Code evidence:** `grep -n "hidden\|report\|flag\|moderate\|admin" reviews.rs` returns nothing except the module doc. The `reviews` table (migration 0011) has no moderation column; `PRODUCT_RATING_JOIN` (products.rs:195-200) averages every row unconditionally; `list_product_reviews` (reviews.rs:267-300) has no filter. Q&A, by contrast, ha
- **Fix:** Add `is_hidden` / `status` (`visible|hidden|under_review`) to `reviews`, an admin hide/unhide endpoint mirroring qna.rs:381-414 (audited, admin-hide wins over seller — see H-05), a buyer/seller report endpoint, and exclude hidden reviews from `PRODUCT_RATING_JOIN` and `list_product_reviews`.

### HI-77 — Public review list exposes `buyer_id` + `order_id` (+ full name) with no authentication

- **Severity:** High | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/reviews.rs:52-65`
- **Found by:** agent-08 (?)
- **Impact:** Anyone can scrape every review of every product and build a buyer→order→product linkage table: `buyer_id` is a stable identifier across all products a person reviewed, `order_id` is the exact order (usable for order-enumeration / social-engineering — "I am calling about order <uuid>"), and `buyer_display` is the buyer's full real name whenever they set one (`buyer_display`, …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/reviews.rs:52-65` (`Review` struct), `crates/api/src/reviews.rs:485-496` (`list_reviews_handler` — no `AuthUser` parameter) - **Code evidence:** // reviews.rs:52-65: pub struct Review { pub id: Uuid, pub product_id: Uuid, pub buyer_id: Uuid, // raw user UUID, public pub buyer_display: String, // full name when set (reviews.rs:144-150) /// The delivered order that made this review eligible. pub order_id: Uuid, // raw order UUID, public pub rating: i16, p
- **Fix:** Remove `buyer_id` and `order_id` from the public `Review` shape (keep them in an internal/admin shape only); expose at most a `verified_purchase: bool` derived from `order_id IS NOT NULL`. Consider masking the display name the same way Q&A does, or at least first-name + masked phone. ---

### HI-78 — Verification approval is a check-then-act race: admin can approve an NID they never reviewed

- **Severity:** High | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/verification.rs:363-433`
- **Found by:** agent-06 (?)
- **Impact:** Seller (or a manager — see H-3) submits application A (NID X, document X). Admin opens it, decides to approve. Between the admin's `load_verification` and the `UPDATE`, the seller resubmits application B (NID Y, different document). The admin's approval lands on B: the store is granted `basic` / `verified` for an identity/document the reviewer never saw. The same window lets …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/verification.rs:363-433` (`approve_verification`), same pattern at `:434-491` (`reject_verification`) - **Code evidence:** // verification.rs:376-387 — row loaded OUTSIDE any transaction let row = load_verification(pool, store_id).await? .ok_or_else(|| ApiError::not_found("this store has no verification application"))?; if !can_transition_verification(&row.status, "approved") { return Err(...); } if tier == "verified" && row.trade_license_number.is_none() { return Err(...); } // verification.rs:390-410 — tx opened AFTER t
- **Fix:** Load the row inside the transaction with `SELECT … FOR UPDATE`, re-check status and licence inside the tx, and make the update conditional: `UPDATE store_verifications SET status='approved', reviewed_at=now() WHERE store_id=$1 AND status='pending'` (and the same for the tier update), failing with 409 when `rows_affected()==0`. Apply the identical fix to `reject_verification`.

### HI-79 — Seller resubmit can overwrite an APPROVED application (check-then-upsert, no transaction)

- **Severity:** High | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/verification.rs:209-291`
- **Found by:** agent-06 (?)
- **Impact:** If the admin approves between the seller's `load_verification` (which saw `pending`) and the upsert, the upsert silently flips the application back to `pending`, clears `reviewed_at` and the reviewer note, and replaces the approved NID — while `stores.verified_tier` stays `basic`/`verified`. Result: the store keeps its trust badge (and passes the `kyc_payout_gate`, which reads …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/verification.rs:209-291` (`submit_verification`) - **Code evidence:** // verification.rs:230-248 — status read, then a bare 409 guard, no lock/tx let existing = load_verification(pool, store.id).await?; let current_status = existing.as_ref().map(|row| row.status.as_str()).unwrap_or("none"); if current_status == "approved" { return Err(ApiError::conflict("this store is already verified; ...")); } // verification.rs:250-276 — unconditional upsert "INSERT INTO store_verifications (...) VALUES ($1, 'pending', $2, $3, $4, $5, NULL
- **Fix:** Do the submit in one transaction: `SELECT … FOR UPDATE` on the verification row, re-check `status != 'approved'` under the lock, and use a conditional upsert (`… WHERE store_verifications.status <> 'approved'` / `ON CONFLICT … DO UPDATE … WHERE store_verifications.status <> 'approved'`) returning 409 when nothing is written.

### HI-80 — A manager (not the owner) can submit and read the store's KYC application, including overwriting the owner's NID

- **Severity:** High | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/verification.rs:510-545`
- **Found by:** agent-06 (?); agent-18 (M5)
- **Impact:** A store owner seats a manager (a common case: hired shop help). That manager can `PUT /v1/seller/store/verification?store_id=<id>` with **their own** NID, trade licence and document, overwriting the owner's application. If an admin then approves it (especially combined with H-1's race), the store's `verified` badge is issued against the manager's identity, not the owner's — …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/verification.rs:510-545` (`get_seller_verification_handler`, `submit_seller_verification_handler`); matrix `crates/domain/src/lib.rs:1140-1165` - **Code evidence:** // verification.rs:520-544 — both handlers gate only on manage_store_settings let (store, _role) = crate::access::resolve_scoped_store( pool, user.user_id, scope.store_id, domain::STORE_ACTION_MANAGE_STORE_SETTINGS).await?; Ok(Json(submit_verification(pool, &store, input).await?)) // domain/src/lib.rs:1150-1153 — manager is granted manage_store_setting
- **Fix:** Split the action: keep `manage_store_settings` for the SIDRA toggle, but require the `owner` role (not just the matrix action) for verification submit/read — e.g. resolve the role in the verification handlers and reject non-owners with 403, or add a dedicated owner-only `manage_verification` action to `STORE_ACTIONS` / `store_role_can`. Do not let a manager's submit overwrite an existing application without owner confirmation.

### HI-81 — Admin promotion / demotion writes no audit row and has no last-admin guard: a compromised admin can become sole admin undetectably

- **Severity:** High | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/admin.rs:763-787`
- **Found by:** agent-06 (?); agent-11 (H5)
- **Impact:** Any single admin can promote **any** user (including a fresh buyer account they control) to admin and demote **every other** admin — the self-check only blocks changing one's *own* flag, so admin A demotes B and C, promotes sockpuppet D, and the platform is left with a single, attacker-controlled admin set. Because no audit row is written, there is no record of who promoted …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/admin.rs:763-787` (`set_user_admin`), handler `:1173-1185` - **Code evidence:** // admin.rs:763-787 — the ONLY privileged write in admin.rs with no record_admin_action call pub async fn set_user_admin(pool: &PgPool, caller: &User, target_user_id: Uuid, is_admin: bool) -> Result<User, ApiError> { if target_user_id == caller.id { return Err(ApiError::bad_request("you cannot change your own admin flag")); } sqlx::query_as("UPDATE users SET is_admin = $2 WHERE id = $1 \ RETURNING id, phone, na
- **Fix:** Write `record_admin_action` (`user.set_admin` / `user.unset_admin`, target `user`, detail `{is_admin}`) in the same transaction as the flag update; add a guard that refuses to demote the last remaining admin (`SELECT COUNT(*) FROM users WHERE is_admin` — refuse when count is 1 and the target is that admin); consider requiring a second admin's confirmation / a distinct break-glass flow for promotion.

### HI-82 — Full NID disclosure endpoint is unaudited and un-rate-limited: bulk NID harvesting by a compromised admin leaves no trace

- **Severity:** High | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/verification.rs:343-355`
- **Found by:** agent-06 (?)
- **Impact:** A compromised (or malicious) admin account can enumerate `store_id`s from the admin store list and call `GET /v1/admin/verifications/:store_id/full` in a loop, exfiltrating every seller's full national ID number — the most sensitive identifier in the system — with nothing in `admin_audit_log` to show it ever happened. The queue list (`list_admin_verifications`) hands out the …
- **Evidence:** - **Severity:** High - **File:line:** `crates/api/src/verification.rs:343-355` (`get_verification_full_nid`), handler `:554-566`; `crates/api/src/admin.rs:98-107` (`require_admin`) - **Code evidence:** // verification.rs:343-355 pub async fn get_verification_full_nid(pool: &PgPool, store_id: Uuid) -> Result<AdminVerificationFull, ApiError> { let row = load_verification(pool, store_id).await? .ok_or_else(|| ApiError::not_found("this store has no verification application"))?; Ok(AdminVerificationFull { store_id: row.store_id, nid_number: row.nid_number }) }
- **Fix:** Audit every full-NID read (`record_admin_action`, e.g. `verification.view_full_nid`, target `store`) in the same request; add per-admin rate limiting / alerting on this endpoint; consider requiring the reviewer to open a specific queue item (and masking the NID except on explicit reveal) rather than a bare enumerable `GET` by store id. ---

### HI-83 — OTP brute-force / expiry / reuse defences exist in code but are never tested: only one wrong code, once

- **Severity:** High | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave2.rs:97-108`
- **Found by:** agent-16 (H1)
- **Impact:** A 6-digit OTP has a 1-in-a-million space per code; with 5 attempts the guessing probability is negligible — *if* the lockout works. If `attempts` counting regresses (see also the unscoped `UPDATE` below), an attacker can brute-force OTPs indefinitely and take over any account by phone number. Expiry (5 min) and single-use consumption are likewise unproven: a leaked/old code …
- **Evidence:** // 2. A wrong code is rejected (pick one that is certainly wrong). let wrong_code = if dev_otp == "000000" { "111111" } else { "000000" }; … assert_eq!(status, StatusCode::UNAUTHORIZED); That is the *entire* negative OTP coverage in 37 files. Wave28 (wave28.rs:344-371) throttles OTP *requests* per phone, but never tests OTP *verification* attempts. -
- **Fix:** Add tests that (a) submit 5 wrong codes then assert the 6th (even the correct code) is rejected with "too many OTP attempts", (b) backdate `otp_codes.expires_at` via SQL then assert verify is a 400 "OTP has expired", (c) verify the same code twice and assert the second is rejected, (d) request a second OTP then assert the first code no longer works, (e) verify with no prior request → 400. Note the implementation detail these tests would also pin: `crates/api/src/auth.rs:365` increments attempts …

### HI-84 — JWT validation is never negatively tested: no expired, tampered, wrong-secret, malformed, or non-Bearer token test

- **Severity:** High | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave2.rs:125-131`
- **Found by:** agent-16 (H2)
- **Impact:** `authenticate()` (auth.rs:185-200) decodes the JWT and trusts `claims.sub/phone/role` with **no database load** — a token for a deleted user still authenticates (only the admin guard re-reads the row, `crates/api/src/admin.rs:100`; `/v1/auth/me` re-loads, auth.rs:450). If signature validation, `exp` enforcement, or algorithm pinning (`Validation::new(Algorithm::HS256)`) ever …
- **Evidence:** // 4. Protected endpoint without a token -> 401. let (status, _) = call(&app, Method::POST, "/v1/stores", …, None).await; assert_eq!(status, StatusCode::UNAUTHORIZED); Across all 37 files, the only other auth-negative is wave37's unknown API key (wave37.rs:295-303). No test sends `Bearer <garbage>`, a token signed with a different secret, a token with `exp` in the past, `Authorization: Token <jwt>`, a `none`-algorithm token, or a token whose `sub` is not a UUID. `grep -rn "Bearer garbage|invalid.*jwt|expired.*token"` in tests → 0 hits. -
- **Fix:** Add a dedicated auth-negative test: hand-craft tokens with the `jsonwebtoken` crate (already a dependency) — expired `exp`, wrong secret, tampered payload, `alg: none`, non-UUID `sub`, missing `Bearer ` prefix — and assert 401 on a protected route for each; assert a token issued before the user row is deleted is rejected (or document that it is intentionally still valid until expiry).

### HI-85 — The `dev_otp` echo (OTP returned in the API response) is never proven absent outside dev mode — and the one "production" app in tests still has dev mode on

- **Severity:** High | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave5.rs:407-408`
- **Found by:** agent-16 (H3)
- **Impact:** If the `dev_otp`-in-response branch is ever keyed to the wrong flag (or the flag is ever defaulted on in production), OTPs leak to anyone who can call `/v1/auth/request-otp` — full account takeover by phone number, with no SMS access needed. The suite cannot catch this because it has never once asserted the *absence* of `dev_otp`. -
- **Evidence:** // 9. Second app, dev mode OFF and no credentials (the production // reality today): … let prod_state = AppState::new(Some(pool.clone()), "wave5-test-secret".to_string(), true); The comment says "dev mode OFF" but the 3rd argument is `sms_dev_mode = true`. `grep -rn "AppState::new.*false"` across all tests → **0 hits**: no test ever builds an app with `sms_dev_mode=false`. Every `login()` helper in every file depends on `body["dev_otp"]` (e.g. wave2.rs:91-93: `.expect("dev mode returns dev_otp")`). -
- **Fix:** Build one app with `AppState::new(pool, secret, false)` and assert `request-otp` returns 200 with `dev_otp` absent/null; fix the wave5.rs:407 comment/code mismatch (either pass `false` for SMS dev mode there or correct the comment to say only *payments* dev mode is off).

### HI-86 — Three public, unauthenticated money-math endpoints have zero tests: `/v1/delivery-charge`, `/v1/cart/split`, `/v1/commission/quote`

- **Severity:** High | **Area:** Tests / Coverage | **File:** `crates/api/src/lib.rs:1151-1183`
- **Found by:** agent-16 (H4)
- **Impact:** These are the exact functions checkout relies on (delivery charge and commission are asserted to the poisha inside checkout tests, e.g. wave3.rs:226-230), but the standalone endpoints — the ones the frontend / third parties call directly — are unpinned. A regression (wrong zone table, negative weight accepted, `rate_bps > 10_000` accepted in the quote, overflow on huge …
- **Evidence:** async fn delivery_charge(Json(req): Json<DeliveryChargeRequest>) -> Json<DeliveryChargeResponse> { … calculate_delivery_charge(req.zone, req.weight_kg, req.cod_amount_poisha) … } async fn split_cart(Json(req): Json<SplitCartRequest>) -> Json<SplitCartResponse> { … split_cart_by_seller(&req.items) … } async fn commission_quote(Json(req): Json<CommissionQuoteRequest>) -> Json<CommissionQuote> { Json(compute_commission(req.subtotal_poisha, req.rate_bps)) } All three are public (no auth extractor) and take caller-supplied inputs — notably `commission_quote` takes `rate_bps` *from the request*, so a caller chooses the commission rate used in the quote. -
- **Fix:** Add tests for each endpoint: known zone/weight/amount vectors (including 0 kg, negative weight, unknown zone → 400), split correctness for multi-seller carts, and commission-quote validation (rate 0, rate > 10,000, negative subtotal → 400 or clamping, matching the domain rule). Assert they require no auth *deliberately* or add auth if they should not be public.

### HI-87 — Cart line mutation is entirely untested: PATCH/DELETE `/v1/cart/items/:product_id` is never called

- **Severity:** High | **Area:** Tests / Coverage | **File:** `crates/api/src/lib.rs:1288-1292`
- **Found by:** agent-16 (H5)
- **Impact:** A buyer-facing mutation endpoint with no tests is where quantity bugs hide: setting quantity 100+ or negative could corrupt cart totals (checkout trusts cart quantities), and an IDOR in `remove_item` (wrong product/user scoping) would let one user delete another's cart lines. The add path's bound (`validate_cart_quantity`, cart.rs:168,226) is tested only implicitly via …
- **Evidence:** /// Set a cart line's quantity. Zero removes the line (the one place a /// quantity of 0 is meaningful); anything else must pass the domain /// validate_cart_quantity(quantity) // 1..=99 Tests only ever `POST /v1/cart/items` (add), `GET /v1/cart`, and `DELETE /v1/cart` (clear, e.g. wave3.rs:287). The distinct semantics — quantity 0 deletes the line, 100 is rejected, a stranger cannot mutate another user's line, variant lines are addressed correctly — are unproven. -
- **Fix:** Add tests: PATCH quantity to 0 (line removed), to 99 (ok), to 100 / -1 / 0-as-add (400), PATCH a product not in the cart (404), PATCH/DELETE as a different user (404/403), PATCH a variant line, and assert the cart totals update after each mutation. Also add add-path negatives: `quantity: 0`, `100`, `-1` → 400 (currently zero such tests anywhere; `grep -rn '"quantity": 0|"quantity": 100|"quantity": -1'` → 0 hits).

### HI-88 — Checkout's own validation branches are never exercised: empty cart, bad zone, bad payment method, bad delivery phone

- **Severity:** High | **Area:** Tests / Coverage | **File:** `crates/api/src/orders.rs:630-680`
- **Found by:** agent-16 (H6)
- **Impact:** Checkout is the money entry point. If validation regresses, an attacker can create orders with an unknown zone (delivery charge computed as 0 or default?), an unknown payment method (payment state machine confusion), or an empty cart (a zero-total order that still creates sub-orders / earns loyalty / triggers notifications). The "failed checkout leaves cart intact" invariant …
- **Evidence:** `parse_payment_method` rejects unknown methods; empty `delivery_name`/`delivery_address` → 400; `parse_delivery_zone` rejects anything but `inside_dhaka|dhaka_suburb|outside_dhaka`; empty cart → `bad_request("cart is empty")` (twice, :660 and :679). Every checkout helper in every wave file sends the same valid payload (e.g. wave3.rs:198-210: `inside_dhaka`/`outside_dhaka`, `cod`, non-empty name/address/phone). -
- **Fix:** Add a checkout-validation test: empty cart → 400 + cart intact; `delivery_zone: "banana"` → 400; `payment_method: "banana"` → 400; empty `delivery_name`/`delivery_address` → 400; malformed `delivery_phone` → 400 (and pin whether phone normalisation matches the OTP normalisation).

### HI-89 — Webhook coverage is bkash-only and dev-only: other gateways, unknown method/ref, and missing signature are never tested

- **Severity:** High | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave5.rs:291-357`
- **Found by:** agent-16 (H7)
- **Impact:** The webhook is the unauthenticated money endpoint — anyone on the internet can POST to it. A gateway-specific verification bug (e.g. Nagad's scheme verifying differently, or the unknown-method branch returning 200 instead of 400) would let an attacker mark orders paid without paying. The duplicate-after-success test proves idempotency for one path; a replay-after-failure that …
- **Evidence:** wave5's webhook helper (wave5.rs:174-197) is called only with `"bkash"`; the suite tests wrong signature (wave5.rs:303), amount mismatch (:311-323), success (:327), duplicate-after-success (:335), and failed (:356). Never tested: `nagad` / `sslcommerz` / `shurjopay` webhooks (each has its own gateway config and signature scheme per `crates/payments/src/lib.rs:142-152`), an unknown method segment (`/v1/payments/webhook/banana`), an unknown `payment_ref` (404 branch), a missing `signature` field, or a duplicate delivery *after a failed* webhook (replay semantics differ from duplicate-after-success). -
- **Fix:** Parameterise the webhook test over all four online methods with correctly-signed dev callbacks; add unknown-method → 400, unknown-ref → 404, missing-signature → 401/400, and duplicate-after-failed → single application (assert `payment_events` count and order state). Note the suite also never tests a *real* (non-dev) signature verification path — that needs a gateway test double (see M19).

### HI-90 — Zero concurrency tests: every race that matters (stock, coupons, loyalty, invoices, sweeps) is tested single-threaded only

- **Severity:** High | **Area:** Tests / Coverage | **File:** `crates/a`
- **Found by:** agent-16 (H8)
- **Impact:** The classic marketplace exploits are all races: two simultaneous checkouts for the last unit of stock (oversell), two simultaneous redemptions of a usage-limit-1 coupon (double discount), two simultaneous loyalty redemptions of the same balance (negative points / double spend), two simultaneous invoice issuances drawing the same sequence number, two concurrent subscription …
- **Evidence:** Every money flow is strictly sequential: wave3's insufficient-stock test (wave3.rs:291-327) adds quantity 2 for stock 1 and checks out *once*; wave14's usage-limit-1 coupon (wave14.rs:536-551) is used sequentially ("first checkout succeeds, second is a 400"); wave35's loyalty redemption (wave35.rs:362-384) redeems 15 points once; wave22's invoice sequence (wave22.rs:354-380) issues INV-000001 then INV-000002 sequentially; wave36's subscription sweep (wave36.rs:490-544) runs `process_subscriptions` sequentially three times. -
- **Fix:** Add concurrency tests with `tokio::join!` over the real router (oneshot requests are cheap): N simultaneous checkouts for stock-1 product → exactly 1 succeeds; N simultaneous uses of a limit-1 coupon → exactly 1 succeeds; N simultaneous loyalty redemptions → balance never goes negative and total redeemed ≤ balance; two concurrent invoice reads → distinct sequence numbers; two concurrent `process_subscriptions` runs → single charge. These are the highest-value tests missing from the suite.

### HI-91 — Notify outbox: batching, ordering, stuck-`sending` rows, fail-then-succeed, and concurrent claiming are all untested

- **Severity:** High | **Area:** Tests / Coverage | **File:** `crates/notify/tests/outbox.rs:105-158`
- **Found by:** agent-16 (H9)
- **Impact:** Double-send (same SMS twice) or silent loss (stuck in `sending` forever, never delivered, never `failed`) directly affects OTP-adjacent notifications and order/payout notices. The admin view counts `sending` rows (wave29.rs:437-444 asserts `counts["sending"] == 0`), but nothing proves a crashed worker's rows ever leave `sending`. If `process_pending`'s claim transaction …
- **Evidence:** let report = process_pending(&pool, &OkSender, 20).await… // lines 105, 116, 128, 138, 146, 158 — always 20 Never tested: batch limit (enqueue 3 rows, `process_pending(pool, sender, 1)` → exactly 1 sent, 2 still pending); ordering (oldest-first claim); a row stuck in `sending` (claimed then worker dies — is it ever retried? the code says no reaper exists, but no test pins either the stuck behaviour or a reaper); fail-then-succeed (row fails once, then succeeds — is `last_error` cleared? do `attempts` stay at 1? is `sent_at` set?); two concurrent `process_pending` calls claiming the same rows (the `FOR UPDATE SKIP LOCKED` claim is the anti-double-send mechanism and is never exercised …
- **Fix:** Add tests for batch=1/0, oldest-first ordering, concurrent double-claim (two `process_pending` tasks over the same rows → each row sent exactly once), fail-then-succeed (assert `last_error IS NULL`, `sent_at` set, `attempts` retained), stuck-`sending` (insert a row in `sending`, assert a later `process_pending` does not claim it — pinning the documented behaviour — or implement and test a reaper), and partial-batch failure. Also extend `load_row` (outbox.rs:43-52) to select `phone, kind, …

## Medium (158)

### ME-01 — A write-scope API key can move money: `POST /v1/seller/products/:id/promote` is not in the JWT-only exclusion list

- **Severity:** Medium | **Area:** API Surface / Routing | **File:** `crates/api/src/api_keys.rs:140`
- **Found by:** agent-15 (M2); agent-18 (M6)
- **Impact:** A leaked write-scope key (long-lived by default, see L6) lets an attacker drain a seller's earnings balance into promotions, place orders as the buyer, and approve returns — money movement the exclusion list's own stated rationale ("spends the earnings balance") says should be session-only. -
- **Evidence:** // api_keys.rs:153-157 — only subscription spend is blocked for this reason: if method == Method::POST && (path == "/v1/seller/payouts" || path.starts_with("/v1/seller/subscription")) { return true; } // with the doc comment just above (api_keys.rs:135-139): // "POST /v1/seller/subscription* — subscribing and cancelling spend the earnings balance, // the same reasoning as payouts." Promotions are bought the same way — prepaid from the earnings balance (`promotions::promote_handler`, lib.rs Wave 26 doc: "a seller prepays whole days … from their EARNINGS BALANCE") — yet `POST /v1/seller/products/:id/promote` is absent from the list. The same gap class applies to `POST /v1/orders` (checkout as …
- **Fix:** Add `path.starts_with("/v1/seller/products/") && path.ends_with("/promote")` (and audit every other money-moving POST: checkout, payment retry, return approve/reject) to `api_key_forbidden_route`, or introduce granular scopes (`promotions:write`, `orders:write`, …) instead of the single `write` scope.

### ME-02 — JWT secret validation is length-only: a 32-character low-entropy secret passes

- **Severity:** Medium | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:555`
- **Found by:** agent-15 (M4)
- **Impact:** A secret like `aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` or a dictionary phrase ≥32 chars is accepted in production; if it is guessable or leaks via config, total account takeover for every user. (Exacerbated by H3, where the check can be switched off entirely.) -
- **Evidence:** pub fn validate_jwt_secret(secret: &str, dev_mode: bool) -> Result<(), String> { if dev_mode { return Ok(()); } if secret == DEV_JWT_SECRET { return Err(...); } if secret.chars().count() < MIN_JWT_SECRET_CHARS { return Err(...); } // MIN = 32 Ok(()) } The unit test even pins the weak behaviour (`lib.rs` tests: `assert!(validate_jwt_secret(&"a".repeat(32), false).is_ok())`). HS256 is a symmetric secret: whoever holds it can forge a token for any `sub` (any user, since `sub` is just the user UUID) — there is no asymmetric separation and no `iss`/`aud` validation (`Validation::new(Algorithm::HS256)` uses jsonwebtoken defaults). -
- **Fix:** Require ≥32 chars *and* reject low-entropy secrets (character-class diversity + a Shannon-entropy floor, or require 256-bit base64/hex); prefer generating the secret with `openssl rand -base64 32` and documenting that; add `iss`/`aud` claims and validate them.

### ME-03 — `CORS_ORIGINS=*` (or any list containing `*`) panics at startup and takes the whole API down

- **Severity:** Medium | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:1050`
- **Found by:** agent-15 (M5)
- **Impact:** One bad env value (`CORS_ORIGINS=*`, or `*,https://shop.example.com`) makes the process crash-loop on every start — total availability loss, fail-closed but self-inflicted. An operator copying a permissive example value gets a down API instead of a warning. -
- **Evidence:** // lib.rs:1050-1073 — only HeaderValue parseability is checked; "*" is a valid HeaderValue: let parsed: Vec<HeaderValue> = origins.iter().filter_map(|origin| match origin.parse::<HeaderValue>() { Ok(value) => Some(value), Err(_) => { eprintln!("WARNING: CORS origin '{origin}' is not a valid origin — ignoring it"); None } }).collect(); CorsLayer::new().allow_origin(AllowOrigin::list(parsed)) tower-http 0.5.2 `AllowOrigin::list` panics when the list contains the wildcard: `"Wildcard origin (`*`) cannot be passed to `AllowOrigin::list`. Use `AllowOrigin::any()` instead" (verified against tower-http source). The code comment claims the opposite intent: "An entry that is not a valid header value …
- **Fix:** Filter out `"*"` (and empty/whitespace entries) in `cors_origins_from_env`/`cors_layer` with a loud warning, or validate at startup and refuse to start with a clean error (exit code 2, like the JWT check) instead of panicking.

### ME-04 — No timeouts anywhere: no request timeout, no DB statement timeout, default pool

- **Severity:** Medium | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:1850`
- **Found by:** agent-15 (M6)
- **Impact:** Slowloris-style slow clients hold connections indefinitely (no request timeout); a single expensive query (unbounded search, analytics aggregates, exports) can hold pool connections for an unbounded time, starving all DB-backed endpoints into 500s for every user. Combined with H1 (no rate limiting), one abusive client can exhaust the 10-connection pool. -
- **Evidence:** // lib.rs:1853 — plain connect, no PgPoolOptions (max connections, timeouts): let pool = PgPool::connect(&url).await.expect("failed to connect to Postgres via DATABASE_URL"); // ... axum::serve(listener, build_app(state)).with_graceful_shutdown(...) — no TimeoutLayer. `crates/api/Cargo.toml` has no `tower` (non-dev) timeout/tracing dependency; there is no `TimeoutLayer`, no `statement_timeout`, no per-request deadline in `build_app`. sqlx defaults apply (10 max connections, 30 s acquire timeout) — shared by every request including unauthenticated ones. -
- **Fix:** Add `tower_http::timeout::TimeoutLayer` (e.g. 30 s) and `RequestBodyTimeoutLayer` in `build_app`; set `PgPoolOptions` with explicit `max_connections`, `acquire_timeout`, and `statement_timeout` (via `sqlx::postgres::PgConnectOptions`, e.g. 10 s); add a slow-query log.

### ME-05 — Auth is per-handler opt-in: one forgotten `AuthUser` parameter silently makes an endpoint public

- **Severity:** Medium | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:1218`
- **Found by:** agent-15 (M7)
- **Impact:** The next wave that adds a handler and forgets the `AuthUser` parameter (or calls a service function directly) ships a silently public endpoint; nothing fails closed and no test catches it, because routing and authorization are decoupled. -
- **Evidence:** Protection depends entirely on each handler declaring `user: AuthUser` as an extractor argument (e.g. `crates/api/src/orders.rs` `create_order(State(state), user: AuthUser, ...)`). There is no router-level auth middleware and no compile-time or test-time assertion that a `/v1/seller/*`, `/v1/admin/*`, or `/v1/buyer/*` route requires auth. The admin guard is likewise a per-handler call (`require_admin(&state, &user).await?` at the top of each admin handler body) rather than a layer. - **Verification performed:** machine sweep of every handler referenced in `build_app` — all sensitive handlers currently take `AuthUser`; all 12 `admin.rs` handlers plus the admin handlers in `coupons.rs`, …
- **Fix:** Group routes into nested routers by audience (`/v1/admin/*`, `/v1/seller/*`, `/v1/buyer/*`) with `route_layer(middleware::from_fn(require_auth))` / `require_admin` layers, leaving the genuinely-public routes in a separate router; add an integration test that enumerates the router's routes and asserts every non-public path requires auth (or use `MatchedPath` in a test harness). ---

### ME-06 — OTP single-use is violable by race: two concurrent correct verifies both mint tokens

- **Severity:** Medium | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:337-380`
- **Found by:** agent-01 (F07)
- **Impact:** Two requests carrying the same correct code, arriving simultaneously, both pass the hash check before either `DELETE` executes; both then mint a JWT for the same user. Single-use is therefore only single-use for sequential attackers. The practical damage is limited (both tokens belong to the same user — the legitimate owner or someone who already knew the code), but it breaks …
- **Evidence:** // auth.rs:364-379 — check, then DELETE, then find_or_create_user: three separate statements, no tx if hash_otp(&phone, code) != row.code_hash { ... } sqlx::query("DELETE FROM otp_codes WHERE phone = $1") ...; find_or_create_user(pool, &phone).await -
- **Fix:** Consume the code atomically: `DELETE FROM otp_codes WHERE phone = $1 AND code_hash = $2 AND expires_at > now() AND attempts < 5 RETURNING ...` in a transaction, and issue the token only if exactly one row was deleted. (This also fixes F01 if the attempts increment is folded into the same statement.)

### ME-07 — `request_otp_code` is not transactional: a failed SMS leaves a valid undelivered code, and a crash can leave no code at all

- **Severity:** Medium | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:258-331`
- **Found by:** agent-01 (F08)
- **Impact:** If `send_sms` fails (real gateway outage, once a gateway exists), the API returns 503 but the code is already stored and valid for 5 minutes — the user was told delivery failed, yet a code exists that they never received; an attacker who can trigger the failure (or who observes the failure mode) gets a live code the victim doesn't know about. Conversely, a crash between the …
- **Evidence:** // auth.rs:297-324 — log insert, DELETE, INSERT, then SMS send: four independent statements sqlx::query("INSERT INTO otp_request_log (phone) VALUES ($1)") ... let code = generate_otp(); ... sqlx::query("DELETE FROM otp_codes WHERE phone = $1") ... sqlx::query("INSERT INTO otp_codes (phone, code_hash, expires_at) VALUES ($1, $2, $3)") ... sender.send_sms(&phone, &format!("Your BD Marketplace login code is {code}")).await .map_err(|e| ApiError::service_unavailable(format!("SMS delivery failed: {e}")))?; -
- **Fix:** Wrap log-insert + upsert in one transaction; send the SMS after commit; on SMS failure, delete/invalidate the just-issued code (or mark it undelivered) and do not count the failed attempt against the throttle — or better, send first via a transactional outbox (as the `notify` crate already does for other messages) and only mark the code live once the send is accepted.

### ME-08 — Token claims are trusted without re-validation: stale `role`/`phone` for up to 7 days, no `iat`/`aud`/`iss`

- **Severity:** Medium | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:121-146`
- **Found by:** agent-01 (F09)
- **Impact:** A token issued as `buyer` still says `buyer` for 7 days after the user becomes a `seller` (and vice versa for any demotion), and a changed phone number is not reflected. Today the blast radius is contained — grep shows `AuthUser.role` is not actually consulted by any authorization check in `crates/api/src` (seller access goes through store ownership/membership in `access.rs`, …
- **Evidence:** // auth.rs:130-134 — role/phone are snapshotted into the token at login let claims = Claims { sub: user.id.to_string(), phone: user.phone.clone(), role: user.role.clone(), exp: exp as usize }; // auth.rs:184-200 — authenticate() returns those claim values verbatim, never re-reading the DB Ok(AuthUser { user_id, phone: data.claims.phone, role: data.claims.role, api_key: None }) // jsonwebtoken 9.3.1, Validation::new(Algorithm::HS256) — defaults: exp required, 60s leeway, // no iss/aud/nbf/iat validation Role changes are real: creating a store flips the DB role — `stores.rs:124`: `UPDATE users SET role = 'seller' WHERE id = $1 AND role = 'buyer'`. -
- **Fix:** Add `iat` (and `aud`/`iss` for the deployment) to `Claims` and validate them; re-read the user row (or a short-lived cache) on authentication the way `authenticate_api_key` already does (`api_keys.rs:433-456`), or at minimum on role-sensitive routes; set leeway to 0–5 s. If role is never used, remove it from the token to avoid a future stale-trust bug.

### ME-09 — Verify/request errors are an oracle for OTP state (distinct messages per failure mode)

- **Severity:** Medium | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:353`
- **Found by:** agent-01 (F10)
- **Impact:** An unauthenticated caller can probe any phone number and learn, with distinct status/message triples (400 "no OTP" / 400 "expired" / 400 "too many attempts" / 401 "incorrect"), whether that number currently has a live OTP, a locked-out OTP, or none — i.e. whether the victim is mid-login right now, and whether a previous guessing campaign already burned the code. Combined with …
- **Evidence:** let row = row.ok_or_else(|| ApiError::bad_request("no OTP was requested for this phone"))?; if row.attempts >= OTP_MAX_ATTEMPTS { return Err(ApiError::bad_request("too many OTP attempts — request a new code")); } if row.expires_at < Utc::now() { return Err(ApiError::bad_request("OTP has expired — request a new code")); } // ... mismatch: return Err(ApiError::unauthorized("incorrect OTP code")); -
- **Fix:** Return one generic error for every verify failure (e.g. 401 "invalid or expired code") regardless of cause, with the same status code; log the specific reason server-side only. Do the same for `request-otp` (identical 200/400 behaviour for valid vs. invalid numbers is already the case for format, but keep the throttle response generic too).

### ME-10 — Single-factor auth only: possession of the phone/SMS is the whole credential, with no step-up for admin or money-moving actions

- **Severity:** Medium | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs`
- **Found by:** agent-01 (F12)
- **Impact:** SIM-swap, SS7/SMS interception, a stolen unlocked phone, or a leaked SMS (F05's log sender) gives full account takeover — including admin accounts — with nothing else to stop it. A single stolen buyer-grade session is also a stolen admin session when the victim is an admin, and stays valid for 7 days (F03). -
- **Evidence:** There is no password anywhere in the schema (correctly noted as design: phone + OTP login), no TOTP/WebAuthn second factor, and no re-authentication challenge before sensitive operations. The admin guard (`admin.rs:98-105`) accepts the same 7-day JWT that a buyer login minted; the same token that browses the catalogue can toggle `is_admin` on other users (`admin.rs:767-779`), mint API keys, approve payouts, and change platform settings. -
- **Fix:** Offer/require a second factor (TOTP or passkey) for admin accounts at minimum; require fresh step-up verification (a new OTP, issued within the last ~10 minutes) before privileged actions — admin-flag changes, API-key minting, payout approval, platform-settings changes — and record those in `admin_audit_log`. ---

### ME-11 — Unbounded, unpruned tables: `otp_request_log` grows forever; expired `otp_codes` rows are never cleaned up

- **Severity:** Medium | **Area:** Auth / OTP / Sessions | **File:** `migrations/0028_security_hardening.sql:12-18`
- **Found by:** agent-01 (F11); agent-13 (L8)
- **Impact:** `otp_request_log` grows by one row per OTP request forever (20/hour/phone × every phone × forever); there is no scheduled prune, no partition, no TTL — a slow storage-exhaustion / table-bloat DoS, and the `COUNT(*) ... WHERE created_at > now() - interval '1 hour'` throttle query (auth.rs:281-287) scans an ever-growing index range. Expired `otp_codes` rows likewise linger …
- **Evidence:** -- 0028: append-only, no PK, no retention, no TTL CREATE TABLE otp_request_log ( phone text NOT NULL, created_at timestamptz NOT NULL DEFAULT now() ); -- comment: "Rows are never deleted by the OTP flow; an operator may prune old rows freely" -- 0003: otp_codes has no cleanup either; expired rows are only removed when a new OTP is -- requested or a verify succeeds — verify_otp_code returns "OTP has expired" (auth.rs:361) -- WITHOUT deleting the row. -
- **Fix:** Add a periodic cleanup (cron/worker or `pg_cron`): delete `otp_request_log` rows older than ~25 hours and `otp_codes` rows past `expires_at` (or delete an expired row inside `verify_otp_code` when expiry is detected); add a PK / retention policy; consider `PARTITION BY RANGE (created_at)` for the log.

### ME-12 — The COD advance / risk decision reads buyer history on the pool, outside the checkout transaction — concurrent checkouts both see pre-order history and can both dodge a required advance

- **Severity:** Medium | **Area:** Concurrency / Transactions / TOCTOU | **File:** `crates/api/src/orders.rs:908`
- **Found by:** agent-20 (M2)
- **Impact:** A buyer with a risk verdict that would force an advance (or an order just over the advance threshold) fires two checkouts at once, or splits one large purchase into two near-simultaneous checkouts: both compute the advance from the same stale history, both conclude no advance is due (each order is individually under the threshold / pre-dates the other's history), both commit …
- **Evidence:** // inside place_order, but on `pool`, not `&mut *tx` — and AFTER the tx began at :651 let report = buyer_risk_for_user(pool, user.user_id).await?; let advance = compute_advance_poisha(payable_grand_total, crate::admin::effective_advance_bps(state).await, // also a pool read, outside tx crate::admin::effective_advance_threshold(state).await, report.risk.requires_advance); `buyer_risk_for_user` (`crates/api/src/risk.rs:55`) counts *committed* history only — the order being placed is not written yet, and neither is any concurrent checkout's order. -
- **Fix:** Evaluate the risk/advance inputs inside the checkout tx after the buyer row is locked (`SELECT ... FROM users WHERE id=$1 FOR UPDATE` at `:845` already serialises concurrent checkouts by the same buyer — move the risk read to after that lock and run it on `&mut *tx`), and include in-flight (same-buyer, uncommitted-in-this-tx) order totals in the advance computation, or track a per-buyer "pending COD exposure" counter. At minimum document that the advance is best-effort per order, not per buyer.

### ME-13 — Loyalty refund and clawback have no idempotency protection — only the earn hook is guarded, and the guard's premise (one-way transitions) is broken by H3

- **Severity:** Medium | **Area:** Concurrency / Transactions / TOCTOU | **File:** `crates/api/src/loyalty.rs:200-270`
- **Found by:** agent-03 (L4); agent-20 (M5)
- **Impact:** Any double execution of `mark_return_received`'s tx or of the shipment-sync-to-`returned` path writes a second `-earned` clawback row — points are taken twice for one return. A double cancel-refund (only reachable today if the sub-order lock discipline is ever bypassed, e.g. via H3's return path) credits the buyer twice. The balance formula floors at zero, so a double clawback …
- **Evidence:** // refund :200 — plain INSERT via insert_ledger_row, reason 'adjust', ref_id = sub_order_id insert_ledger_row(conn, buyer_user_id, refund_points, "adjust", sub_order_id, None).await // clawback :238 — read-then-insert, no lock, no existence check for a prior clawback let earned: Option<i32> = sqlx::query_scalar( "SELECT delta FROM loyalty_points WHERE reason = 'earn' AND ref_id = $1") insert_ledger_row(conn, buyer_user_id, -i64::from(earned), "adjust", sub_order_id, None).await // earn :154-179, by contrast: "INSERT INTO loyalty_points (...) VALUES ($1, $2, 'earn', $3, ...) \ ON CONFLICT (ref_id) WHERE reason = 'earn' DO NOTHING" `migrations/0033_loyalty.sql` states reversal rows …
- **Fix:** Give reversal rows the same treatment as earn: a partial unique index, e.g. `CREATE UNIQUE INDEX loyalty_points_refund_once_idx ON loyalty_points (ref_id) WHERE reason='adjust' AND delta > 0` and a matching one for negative adjustments, with `ON CONFLICT DO NOTHING` on insert — or, if one sub-order can legitimately need two `adjust` rows, add an explicit `kind`/`adjustment_type` column and unique-index on `(ref_id, kind)`. Fixing H3 (sub-order row lock + conditional transition) closes the main …

### ME-14 — Inventory's variant-guard check runs without locking the variant rows — a variant can be activated mid-adjustment and the write lands in the dead product pool

- **Severity:** Medium | **Area:** Concurrency / Transactions / TOCTOU | **File:** `crates/api/src/inventory.rs:315-323`
- **Found by:** agent-20 (M7)
- **Impact:** Seller A adjusts product-level stock while seller B (or the same seller in another tab) activates a variant for that product. The `EXISTS` check passes (no active variants *yet*), the variant activation commits, then the adjustment commits into `products.stock` — a pool checkout never draws from for variant products (checkout reads variant rows; the overview shows the summed …
- **Evidence:** // inside the tx, product row IS locked (:292 FOR UPDATE OF p), but: let has_active_variants: bool = sqlx::query_scalar( "SELECT EXISTS (SELECT 1 FROM product_variants \ WHERE product_id = $1 AND is_active)").bind(product_id) .fetch_one(&mut *tx).await...; if has_active_variants { return Err(ApiError::bad_request(...)); } // ...then UPDATE products SET stock = ... — the pool nothing reads once variants exist -
- **Fix:** Lock the variant set for the check: `SELECT ... FROM product_variants WHERE product_id=$1 FOR UPDATE` (or take a lock on the product row in the variant create/activate path too — `variants.rs` adjust already locks its variant row, `variants.rs:448`, but creation/activation should lock the parent product row), and re-check `EXISTS` immediately before the `UPDATE`. ---

### ME-15 — Checkout has no idempotency key: a retried request after a lost response cannot return the created order, and the buyer is told the order failed

- **Severity:** Medium | **Area:** Concurrency / Transactions / TOCTOU | **File:** `crates/api/src/orders.rs:640-660`
- **Found by:** agent-03 (L1); agent-20 (M1)
- **Impact:** That cart-clear *does* prevent a literal duplicate order from the same cart (a second concurrent checkout blocks on the product `FOR UPDATE` locks, then sees an empty cart — verified, see "Verified sound"). The remaining harm is operational, not duplicative: if the first checkout commits but the response is lost (timeout, dropped connection), the buyer's automatic retry gets …
- **Evidence:** `place_order` takes `PlaceOrderInput { delivery_name, delivery_phone, delivery_address, delivery_zone, payment_method, coupon_code, advance_method, loyalty_points }` — no `Idempotency-Key` header is read, no dedup table exists, and no order lookup by client key is performed. The only thing standing between a double-submit and a duplicate order is the `DELETE FROM cart_items` at `:1220` inside the same tx. -
- **Fix:** Accept an `Idempotency-Key` header on `POST /orders`, store `(key, buyer_user_id, order_id, response_snapshot)` in a table with `UNIQUE(key, buyer_user_id)` inside the checkout tx, and return the stored order on replay. Until then, at minimum make the empty-cart error distinguishable when a very recent order exists for the buyer (return the existing order instead of 400).

### ME-16 — A cancelled / never-paid order permanently burns the coupon use — `used_count` is never decremented and the redemption is never removed

- **Severity:** Medium | **Area:** Coupons / Promotions | **File:** `crates/api/src/orders.rs:1416-1480`
- **Found by:** agent-07 (M1)
- **Impact:** Buyer applies a `usage_limit = 1` coupon, then cancels the (still-`placed`) order, or the online payment fails / is never completed: the coupon is gone — `used_count` stays 1, the redemption row stays, analytics still counts it — while the buyer received nothing. This is a fairness/availability bug that also corrupts `used_count` as a measure of *successful* redemptions. In …
- **Evidence:** // orders.rs:1184-1199 — the ONLY increment, at checkout commit: INSERT INTO coupon_redemptions (coupon_id, order_id, buyer_id, discount_poisha) ... UPDATE coupons SET used_count = used_count + 1 WHERE id = $1 // orders.rs:1468-1474 — buyer cancel refunds LOYALTY points, but not the coupon: for (sub_order_id, _) in &sub_orders { restock_sub_order_items(&mut tx, *sub_order_id, "cancel_restock").await?; crate::loyalty::refund_for_cancelled_sub_order(&mut tx, *sub_order_id).await?; } // <-- no DELETE FROM coupon_redemptions, no used_count = used_count - 1 The same asymmetry holds for returns (`returns.rs` refunds `goods − coupon discount` via `compute_refund_poisha` and never touches the …
- **Fix:** On buyer cancel (and on seller cancel of a sub-order that voids the whole order, and on a payment definitively failing / an unpaid order expiring — add that expiry job if it does not exist), in the same transaction delete the `coupon_redemptions` row for the order and `UPDATE coupons SET used_count = GREATEST(used_count - 1, 0)`. Guard the decrement so it can never go below the live redemption count; reconcile `used_count` from `coupon_redemptions` periodically.

### ME-17 — Coupon preview is an unauthenticated-by-role oracle: any logged-in buyer can enumerate live coupons and learn their exact discount, cap, minimum and store

- **Severity:** Medium | **Area:** Coupons / Promotions | **File:** `crates/api/src/coupons.rs:218-290`
- **Found by:** agent-07 (M3); agent-18 (L3)
- **Impact:** An attacker with any buyer account brute-forces dictionary codes through preview and harvests every live platform/store coupon: its existence, whether it is platform or store-scoped, which store, the exact discount in poisha for a chosen cart total (which also reveals `max_discount_poisha` — send a huge `goods_total_poisha` and the returned discount is the cap), and the …
- **Evidence:** // coupons.rs preview_handler: any authenticated user, no admin/seller role: pub async fn preview_handler(State(state): State<AppState>, _user: AuthUser, Json(input): Json<CouponPreviewInput>) // preview_coupon returns distinguishable reasons and the full economics of a hit: // not found → "coupon not found"; expired → "coupon has expired"; // limit reached → "coupon usage limit reached"; below min → "order is below the coupon minimum" // valid → CouponPreview { valid: true, scope, store_id, discount_poisha, ... } Only the API-key limiter exists in the codebase (`lib.rs:594-595,771`); no per-user rate limit is applied to this route. Codes are human-chosen, 3–32 chars …
- **Fix:** Rate-limit preview per user/IP (e.g. 10/min), return a single generic reason for invalid coupons to unauthenticated-by-ownership callers (or at least collapse "not found" vs "expired/limit" into one message), do not return `discount_poisha` for a coupon the caller has not demonstrated eligibility for beyond the preview itself — or require the preview to be run against a real cart server-side instead of client-supplied totals (see L3). Consider requiring coupon codes to contain a random suffix …

### ME-18 — Promotion purchase reads price and product state *before* the transaction and never re-checks them inside it (price TOCTOU + inactive-product TOCTOU)

- **Severity:** Medium | **Area:** Coupons / Promotions | **File:** `crates/api/src/promotions.rs:186-232`
- **Found by:** agent-07 (M4)
- **Impact:** (a) Admin changes the per-day price between the `effective_promotion_price_per_day` read and `tx.commit()`: the buyer is charged the stale price — lower than the new price (platform loses the difference on every concurrent purchase) or higher than the price shown on the policy page the buyer just saw (buyer overcharged vs. advertised price; the receipt shows the charged cost, …
- **Evidence:** // promotions.rs:186-232 — all gates run on the pool, before pool.begin(): let product = get_product_by_id(pool, product_id).await?; // status checked here if product.status != "active" { return Err(...); } let payout_store = get_store_by_owner(pool, user.user_id).await?; let price_per_day = effective_promotion_price_per_day(state).await; // price read here let cost = promotion_cost_poisha(days, price_per_day)...; let mut tx = pool.begin().await...; // transaction starts AFTER price + status are fixed // promotions.rs:161-163 (products.rs) — is_promoted does not check product status either: EXISTS (SELECT 1 FROM promotions pr WHERE pr.product_id = p.id AND pr.status = 'active' AND …
- **Fix:** Move the product-status check and the price read *inside* the transaction: `SELECT … FROM products WHERE id=$1 FOR UPDATE` (re-check `status='active'`), and read `platform_settings` for the promotion price on the transaction connection (or re-read and compare after `BEGIN`, aborting on mismatch). Reject the purchase if either changed.

### ME-19 — Cancel-promotion is a non-atomic check-then-act with an unconditional UPDATE: double-cancel succeeds and an expired promotion can be "cancelled"

- **Severity:** Medium | **Area:** Coupons / Promotions | **File:** `crates/api/src/promotions.rs:361-405`
- **Found by:** agent-07 (M5)
- **Impact:** Two concurrent cancel requests for the same promotion both load `display_status == "active"`, both pass the guard, both `UPDATE` — both return 200 with a cancelled promotion, where the second should be 409 ("Cancelling a cancelled … promotion is a 409", module doc). Symmetrically, a promotion whose window expires between the load and the `UPDATE` is flipped from `expired` to …
- **Evidence:** // promotions.rs:361-395 — load (no lock, no transaction), check derived status, then: let promotion = row.map(with_display_status).ok_or_else(...)?; if promotion.display_status != "active" { return Err(ApiError::conflict(format!("only a live promotion can be cancelled (this one is {})", ...))); } sqlx::query("UPDATE promotions SET status = 'cancelled' WHERE id = $1") .bind(promotion_id).execute(pool).await...; // no `AND status = 'active'`, no rows-affected check -
- **Fix:** One atomic statement: `UPDATE promotions SET status='cancelled' WHERE id=$1 AND status='active' AND ends_at > now() RETURNING …`; return 409 when `rows_affected() == 0` (re-load to distinguish already-cancelled vs expired vs not-found). Alternatively wrap load+update in a transaction with `SELECT … FOR UPDATE`.

### ME-20 — Promotion money is charged against the wrong store's earnings: balance is gated on the caller's *earliest* store while the charge is global per user

- **Severity:** Medium | **Area:** Coupons / Promotions | **File:** `crates/api/src/promotions.rs:229`
- **Found by:** agent-07 (M7)
- **Impact:** (a) Multi-store sellers cannot spend earnings from a non-earliest store on promotions — the gate reads only the earliest store, so a seller whose sales are in store B is told "insufficient earnings balance" despite a large balance in B (availability bug). (b) A manager can spend their own earnings to promote the owner's product with no per-purchase acknowledgement of whose …
- **Evidence:** // promotions.rs:229 — the money store is the CALLER's earliest store, not the product's store: let payout_store = get_store_by_owner(pool, user.user_id).await?; // payouts.rs:213-250 — earned counts ONLY that store's delivered sub-orders, // but charges count ALL of the user's charges across every store: (SELECT COALESCE(SUM(seller_payout_poisha), 0) FROM sub_orders WHERE store_id = $1 AND status = 'delivered') AS earned_poisha, (SELECT COALESCE(SUM(amount_poisha), 0) FROM seller_charges WHERE seller_user_id = (SELECT owner_user_id FROM stores WHERE id = $1)) AS charges_total_poisha The module doc (promotions.rs) acknowledges the split — "a member's purchase is paid from the member's own …
- **Fix:** Decide and enforce one money owner per promotion: charge the *product's store's* earnings (and lock that store's row), with an explicit authorisation step when the payer is not that store's owner; or keep caller-pays but show the exact debit ("₨X from your store Y balance") in the purchase confirmation and return 403 with a clear message for members with no store instead of a misleading 404. Make `seller_earnings_on` aggregate per-user (all owned stores) or per-promoted-store consistently, and …

### ME-21 — Unchecked `i64` multiplication in percent-discount maths: preview accepts an arbitrary client-supplied subtotal, so any authenticated user can overflow it

- **Severity:** Medium | **Area:** Coupons / Promotions | **File:** `crates/domain/src/lib.rs:2005`
- **Found by:** agent-07 (M2); agent-12 (M7)
- **Impact:** `POST /v1/coupons/preview` with `{"code":"ANY90","goods_total_poisha":9223372036854775807}` makes `eligible * 90` overflow `i64`. In a debug build (tests, dev) Rust panics on overflow — a 500 / thread panic, a trivially repeatable DoS by any logged-in user with no seller involvement. In a release build (default `overflow-checks = off`, no `[profile]` section in the workspace …
- **Evidence:** // domain/lib.rs:2005 — plain i64 multiply, no checked_mul (contrast allocate_discount // at lib.rs:2028+, which deliberately uses i128 for the same reason): let percent_off = eligible_subtotal_poisha * discount_value / 100; // coupons.rs:210-212,241-247 — preview takes the subtotal straight from the client, // with no upper bound (only .max(0)): pub goods_total_poisha: i64, pub store_goods_poisha: Option<i64>, None => input.goods_total_poisha.max(0), _ => input.store_goods_poisha.unwrap_or(0).max(0), // domain/lib.rs:58-60 — checkout's own line total is the same unchecked shape: pub fn line_total_poisha(&self) -> i64 { self.unit_price_poisha * i64::from(self.quantity) } // …
- **Fix:** Use `i128` (as `allocate_discount` already does) or `checked_mul` in `compute_coupon_discount` and in `line_total_poisha`; validate preview inputs with a sane upper bound (e.g. reject `goods_total_poisha` / `store_goods_poisha` > 10^12 poisha) and add an upper bound to product-price validation. Add a regression test with `goods_total_poisha = i64::MAX`.

### ME-22 — Database credentials are exposed in plaintext in the compose environment and in the API/worker process environment

- **Severity:** Medium | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `docker-compose.yml:46-49`
- **Found by:** agent-14 (A14-10)
- **Impact:** The DB password is visible to anyone who can run `docker compose config`, `docker inspect`, or read `/proc/<pid>/environ` in the container, and it is baked into the connection string that can appear in logs/crash dumps if the URL is ever printed. The worker gets every secret the API needs (courier keys, payment keys, `SIDRA_WEBHOOK_SECRET`) although it makes no network calls …
- **Evidence:** env_file: [.env] # api (46) and worker (62): the WHOLE .env, # incl. courier/payment/Meta/SIDRA keys, is # injected into both containers environment: DATABASE_URL: postgres://…:${POSTGRES_PASSWORD:-marketplace_dev_only}@postgres:5432/… -
- **Fix:** Use Docker secrets (`secrets:` + `*_FILE` variables) or at minimum a `postgres` password file; give the worker only the variables it needs (drop `env_file` from `worker`, or use a separate `worker.env`); never interpolate the password into `DATABASE_URL` in compose — let the binary read `PGPASSWORD`/`DATABASE_URL` from a secret file. In CI, scope `PGPASSWORD` to the single step that needs it (already step-scoped, but prefer a `.pgpass` file with `0600`).

### ME-23 — Backup/restore integrity is unauthenticated and fragile: checksum is optional on restore, sidecar travels with the file, and the sidecar's recorded path breaks when the file moves

- **Severity:** Medium | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `scripts/backup.sh:86-88`
- **Found by:** agent-14 (A14-11)
- **Impact:** An attacker who can replace a backup (stolen laptop, shared drive, compromised bucket — see A14-04) can replace the `.sha256` alongside it and the "verification" passes; a restore from a moved file either fails spuriously (operator learns to skip the check) or proceeds unchecked. Either way the check provides no authenticity. -
- **Evidence:** # backup.sh:86-88 sha256sum "$file" >"$file.sha256" # plain SHA-256, no HMAC/signature # restore.sh:57-67 if [[ -f "$file.sha256" ]]; then # OPTIONAL — absent sidecar = no check at all sha256sum --check "$file.sha256" fi `sha256sum "$file"` records the path *as invoked* (e.g. `./backups/backup-….sql.gz`), so `sha256sum --check` fails if the file is restored from a different working directory or after being copied/renamed without its sidecar — the common off-site-restore case. -
- **Fix:** Sign backups (`gpg --detach-sign` / `age` with a signing key, or at least HMAC the checksum with a key kept off the backup host), make verification mandatory (fail if the sidecar/signature is absent), and record only the basename in the sidecar (`cd "$outdir" && sha256sum "$(basename "$file")"`).

### ME-24 — No image is pinned: mutable tags and unpatched base OS accumulate known CVEs

- **Severity:** Medium | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `infra/docker-compose.yml:4`
- **Found by:** agent-14 (A14-12)
- **Impact:** Two builds a month apart can produce different binaries/OS packages with no audit trail; a compromised or CVE-laden base image is pulled silently; `minio:latest` can jump major versions on a rebuild. Debian bookworm is old-stable, so unpatched userspace CVEs accumulate between rebuilds with no signal. -
- **Evidence:** FROM rust:1.100-bookworm AS builder # 1.100 is real (Rust 1.100.0, 2026-11-12) but the tag is mutable FROM debian:bookworm-slim AS runtime # no patch level, no digest image: postgres:16 # compose:24 — any 16.x image: redis:7-alpine # compose:97, infra:15 image: minio/minio:latest # infra:20 — literally :latest image: postgres:16-alpine # infra:4 No `HEALTHCHECK`, no `apt-get upgrade` (api installs `ca-certificates` from whatever bookworm snapshot the tag points at, lines 26-28; worker installs nothing at all). -
- **Fix:** Pin every image by digest (`image: postgres:16@sha256:…`, `FROM debian:bookworm-slim@sha256:…`), record digests in the repo, add a scheduled rebuild + `trivy` scan (A14-08), and prefer `trixie`-based images / `postgres:16.x` explicit patch tags. Keep `rust:1.100-bookworm` (not `stable`) so the builder matches CI's toolchain — see A14-14.

### ME-25 — `.dockerignore` does not exclude `backups/`, `.env.*`, or the rest of the repo — plaintext DB dumps are sent to the Docker daemon as build context

- **Severity:** Medium | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `apps/w`
- **Found by:** agent-14 (A14-13)
- **Impact:** If `backups/` exists locally (the default outdir of `backup.sh`), `docker build`/`docker compose up --build` ships every `backup-*.sql.gz` — the full plaintext production database (A14-04) — to the Docker daemon, and to any remote builder/BuildKit endpoint configured. A `.env.production` or `.env.local` left beside `.env` is likewise sent, even though it is never `COPY`ed into …
- **Evidence:** .git target node_modules apps/web/node_modules apps/web/.next .env Missing: `backups/` (gitignored, but `.dockerignore` is a separate list), `.env.*` (`.gitignore` excludes them from git, `.dockerignore` does not exclude them from the *build context*), `infra/`, `scripts/`, `README.md`, `.github/`. -
- **Fix:** Add `backups/`, `.env.*`, `infra/`, `scripts/`, `.github/`, `*.md` to `.dockerignore`; verify with `docker build --progress=plain` / `docker builder du` that no backup or env file is in the context. (Note: the `COPY` lines themselves are safe — they only copy `Cargo.toml`, `Cargo.lock`, `crates/`, `migrations/` — the leak is the *context*, not the image layers.)

### ME-26 — Builds are not reproducible / not locked: `cargo build` without `--locked`, and CI's toolchain floats on `stable` while Docker pins Rust 1.100

- **Severity:** Medium | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `Dockerfile.api:20`
- **Found by:** agent-14 (A14-14)
- **Impact:** Without `--locked`, Cargo may silently update `Cargo.lock` to satisfy a `Cargo.toml` range (e.g. a yanked/replaced or compromised patch release) and the Docker image and the CI binaries can be built from different dependency sets — the "same" release is not the same code. `stable` in CI vs `1.100` in Docker means a future `stable` (1.101+) can pass CI and then fail (or behave …
- **Evidence:** RUN cargo build --release -p api # no --locked / --frozen # ci.yml:32 - uses: dtolnay/rust-toolchain@stable # floating toolchain # ci.yml:37,69,74 run: cargo test --workspace run: cargo clippy --workspace --all-targets -- -D warnings run: cargo build --release -p api -p worker -
- **Fix:** Use `cargo build --locked --release …` (and `--locked` on `test`/`clippy`) in both Dockerfiles and CI; pin CI to `toolchain: "1.100"` (or a `rust-toolchain.toml` at the repo root, which pins both `cargo` and `rustc` for every developer/CI/Docker invocation that respects it).

### ME-27 — CI actions are pinned only to mutable major tags, with no `permissions:` block and no hash pinning

- **Severity:** Medium | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `(see agent report)`
- **Found by:** agent-14 (A14-15)
- **Impact:** `@v4`/`@v2`/`@stable` are mutable refs: a compromise of any of those action repos (or a force-push to the tag) silently changes what code runs in CI with write access to the repo, able to exfiltrate the uploaded release binaries or push commits. `pull_request` runs from forks get the same default token scope. No `concurrency` means overlapping runs on `main` can interleave …
- **Evidence:** - uses: actions/checkout@v4 - uses: dtolnay/rust-toolchain@stable - uses: Swatinem/rust-cache@v2 - uses: actions/upload-artifact@v4 - uses: actions/setup-node@v4 # on: push: branches: [main]; pull_request: ; workflow_dispatch: # — no `permissions:` block, so jobs run with the default GITHUB_TOKEN # (write) permissions; no `concurrency:` block either. -
- **Fix:** Pin every action to a full commit SHA (`actions/checkout@<sha> # v4.x.y`), add a top-level `permissions: contents: read` (narrow per-job if a job ever needs more), and add `concurrency: group: ci-${{ github.ref }}, cancel-in-progress: true`. (This is defence-in-depth: no `secrets.*` is referenced in this workflow today, which limits the blast radius — but the release binaries *are* the asset.)

### ME-28 — `backup.sh` failure modes leave partial plaintext dumps behind; retention pruning parses `ls` output

- **Severity:** Medium | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `scripts/backup.sh:52-54`
- **Found by:** agent-14 (A14-17)
- **Impact:** A failed/interrupted backup leaves a partial DB dump on disk indefinitely (and, via A14-13, in the next Docker build context); a pre-planted symlink at the predictable `$file.tmp` path lets `gzip >"$tmp"` truncate an arbitrary file the operator can write (classic symlink attack, low likelihood but trivially avoidable); `ls -1tr` on filenames breaks on spaces/newlines in …
- **Evidence:** # backup.sh:52-54 — predictable temp name, no mktemp, no trap tmp="$file.tmp" # backup.sh:69,82 — the pipeline writes straight to $tmp docker compose exec -T postgres sh -c 'pg_dump …' | gzip >"$tmp" pg_dump "$DATABASE_URL" | gzip >"$tmp" # backup.sh:99-107 — retention backups=("$outdir"/backup-*.sql.gz) mapfile -t by_age < <(ls -1tr "${backups[@]}") There is no `trap 'rm -f "$tmp"' EXIT/ERR` anywhere in the script. `set -euo pipefail` does abort on `pg_dump` failure, but the partial `$tmp` (a truncated plaintext dump) stays in `backups/`. -
- **Fix:** `tmp="$(mktemp "$outdir/.backup-XXXXXX")"` + `trap 'rm -f "$tmp"' EXIT`; replace the `ls` pipeline with `stat`-based sorting or `printf '%s\0'` + `sort -z` on mtime; verify `$tmp` is a regular file owned by the operator before writing.

### ME-29 — CI installs an unpinned OS package as root and leaves a plaintext test dump in `/tmp`

- **Severity:** Medium | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `/tmp/ci-backup.sql`
- **Found by:** agent-14 (A14-18)
- **Impact:** Unpinned `apt-get install` on a shared runner image can pull a different (or, in a compromised mirror, malicious) client version on different runs, and installs the full recommended package set. The `/tmp` dump is the test database (seeded/fixture data — lower value than production, but it is still a full schema+data dump on a shared runner filesystem with no …
- **Evidence:** # ci.yml:48-51 if ! command -v pg_dump >/dev/null 2>&1; then sudo apt-get update sudo apt-get install -y postgresql-client # no version pin, no --no-install-recommends fi # ci.yml:53-55 pg_dump -h localhost -U marketplace -d marketplace_test -f /tmp/ci-backup.sql # … dropdb only runs at line 66, after the assertions — any earlier failure # leaves /tmp/ci-backup.sql (and the scratch DB) behind for the rest of the run -
- **Fix:** Pin the package (`postgresql-client=16.x`), add `--no-install-recommends`, prefer the `postgres:16` service image's own client or a pinned `apt-get install -y --allow-downgrades`, and `trap 'rm -f /tmp/ci-backup.sql; dropdb …' EXIT` in the backup-verification step so the dump is removed even when an assertion fails.

### ME-30 — Unused database drivers in the dependency graph widen the attack surface (`sqlx-mysql`, `sqlx-sqlite`)

- **Severity:** Medium | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `crates/api/Cargo.toml`
- **Found by:** agent-14 (A14-19)
- **Impact:** Code that is never called cannot be exploited at runtime, but it *is* compiled/linked into test/dev builds, it is what `cargo audit` flags, and it gives a future "just enable sqlite for local dev" change a pre-built vulnerable path (A14-07). It also bloats build time and the audit surface for no benefit. -
- **Evidence:** The lockfile contains all three sqlx drivers even though every crate requests only `postgres`. This is how `libsqlite3-sys 0.30.1` (A14-07) and the MySQL driver's native code enter the tree at all. -
- **Fix:** Set `default-features = false` on the sqlx dependency and enable only `runtime-tokio, tls-rustls, postgres, uuid, chrono, migrate`; confirm with `cargo tree -e normal -i sqlx-mysql` / `-i sqlx-sqlite` that both disappear, and re-run `cargo audit`. ---

### ME-31 — No hardening in the compose stack: no `restart:`, no resource limits, no `read_only`/`cap_drop`/`no-new-privileges`, no `HEALTHCHECK` on api/worker

- **Severity:** Medium | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `docker-compose.yml:41-99`
- **Found by:** agent-14 (A14-16)
- **Impact:** A crashed/ wedged API is never restarted and never reported unhealthy, so a load balancer (or an operator) keeps routing to a dead container; a memory/CPU hog in any one service can starve Postgres; a container escape or a compromised API process runs with the default Docker capability set and a writable root filesystem, making persistence and lateral movement far easier. The …
- **Evidence:** The only healthcheck in the stack is Postgres (`docker-compose.yml:31-36`); the api has none despite the comment at lines 60-61 saying *"Readiness is observable at GET /health/ready … gate real traffic on it"*. The worker explicitly has *"No restart policy … on purpose"* (lines 71-72). No service sets `deploy.resources.limits`, `read_only: true`, `cap_drop: [ALL]`, `security_opt: [no-new-privileges:true]`, or `tmpfs`. -
- **Fix:** Add `restart: unless-stopped` to api/web (and to worker once it does real work), a `healthcheck: test: ["CMD", "wget", "-q", "-O", "/dev/null", "http://localhost:8080/health/ready"]` (or a tiny statically-linked checker — note the slim image has no `curl`/`wget`, so add one or use the binary's own health endpoint via a `HEALTHCHECK` in the Dockerfile), `deploy.resources.limits: {cpus, memory}`, `read_only: true` + `tmpfs: [/tmp]`, `cap_drop: [ALL]`, `security_opt: [no-new-privileges:true]`, and …

### ME-32 — `validate_hex_color` accepts any 7-character string without `#` (vacuous `all()` on empty string)

- **Severity:** Medium | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:2430-2437`
- **Found by:** agent-12 (M1)
- **Impact:** For input without `#`, `hex` is `""` and `[].all(..)` is `true`, so `"1234567"`, `"abcdefg"`, `"x; color"` (any 7 chars) pass validation and are stored as a "brand colour". Downstream the value is used as CSS; an invalid colour breaks rendering, and a 7-char payload containing CSS-syntax characters is stored unvalidated at the one gate the docs claim rejects everything but …
- **Evidence:** pub fn validate_hex_color(raw: &str) -> Result<String, &'static str> { let hex = raw.strip_prefix('#').unwrap_or(""); if raw.len() == 7 && hex.chars().all(|c| c.is_ascii_hexdigit()) { Ok(raw.to_uppercase()) } else { Err("brand_color must be a hex colour like #FF5722") } } -
- **Fix:** Require the prefix explicitly: `let Some(hex) = raw.strip_prefix('#') else { return Err(..) }; if hex.len() == 6 && hex.chars().all(is_ascii_hexdigit) { Ok(..) }`.

### ME-33 — `validate_payout_amount_with_min` has no non-negativity floor — a negative minimum admits a negative payout

- **Severity:** Medium | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:1260-1272`
- **Found by:** agent-12 (M2)
- **Impact:** The function trusts `min_payout_poisha` (an admin-set platform setting, parsed via `parse_setting_i64`). If that setting is ever negative/zero (a bad admin entry, a corrupt row — the setting reader's validation is in the API crate, out of scope), then `amount = -50, min = -100, available = 0` passes both checks and a *negative* payout request is created — money flowing the …
- **Evidence:** pub fn validate_payout_amount_with_min(amount_poisha: i64, available_poisha: i64, min_payout_poisha: i64) -> Result<(), &'static str> { if amount_poisha < min_payout_poisha { return Err("payout amount is below the minimum payout"); } if amount_poisha > available_poisha { return Err("payout amount exceeds the available balance"); } Ok(()) } -
- **Fix:** Add `if amount_poisha <= 0 { return Err("payout amount must be positive"); }` and `if min_payout_poisha < 0 { return Err(..) }` (or clamp the effective minimum to `>= 0`) inside this function, not only at the call site.

### ME-34 — Shipment transition table has no direct `→ returned` / RTO path — real courier returns get stuck

- **Severity:** Medium | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:475-493`
- **Found by:** agent-12 (M3)
- **Impact:** A courier that reports a parcel as returned (RTO after failed delivery, or return-after-delivery) without first reporting `return_in_transit` produces a transition the table rejects. Callers that consult the table before applying a poll will drop the update, leaving the shipment stuck at `delivered`/`in_transit` forever — buyer never sees the return, refund/return flows that …
- **Evidence:** the table allows `("delivered","return_in_transit")` and `("return_in_transit","returned")`, but *not* `("delivered","returned")`, `("in_transit","returned")`, `("out_for_delivery","returned")`, `("picked_up","returned")`, or `("partial_delivered","return_in_transit")`. Yet the courier mappings *do* produce `returned` directly: RedX `delivered_returned → returned` (`:705`), eCourier/Paperfly `delivered_returned → returned` (`:756`, `:807`). -
- **Fix:** Add `("delivered","returned")`, `("in_transit","returned")`, `("out_for_delivery","returned")`, `("partial_delivered","return_in_transit")` (and consider `("created","in_transit")`/`("picked_up","out_for_delivery")` skip-tolerance) — or document that the worker must synthesise the intermediate `return_in_transit` step when a courier jumps straight to `returned`.

### ME-35 — `choose_courier` trusts any price, and every `price_estimate` returns the *same* platform tariff — routing is not smart

- **Severity:** Medium | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:865-882`
- **Found by:** agent-12 (M4)
- **Impact:** (a) A negative `price_poisha` (corrupt quote, courier error, or a caller bug) always wins — there is no `price >= 0` check. (b) All five real adapters return the identical domain-tariff estimate, so the "lowest price wins" rule is a constant tie and routing *always* picks Steadfast (priority 0) regardless of the couriers' real charges. Sellers are charged the platform tariff …
- **Evidence:** pub fn choose_courier<'a>(candidates: &[CourierQuote<'a>]) -> Option<&'a str> { for candidate in candidates.iter().filter(|c| c.available) { let wins = match best { None => true, Some(current) => candidate.price_poisha < current.price_poisha || (candidate.price_poisha == current.price_poisha && courier_priority(candidate.courier) < courier_priority(current.courier)) }; if wins { best = Some(candidate); } } } // every adapter: Ok(domain::calculate_delivery_charge(parcel.zone, parcel.weight_kg, parcel.cod_amount_poisha)) -
- **Fix:** Reject `price_poisha < 0` candidates in `choose_courier` (treat as unavailable), and either call each courier's real price endpoint or label the estimate honestly in the UI/API as "platform tariff, not a courier quote" and remove the price-based selection until real quotes exist.

### ME-36 — Money crosses the courier boundary as `f64` taka / rounded whole taka — COD collected ≠ COD owed

- **Severity:** Medium | **Area:** Domain / Courier / Payments core | **File:** `crates/courier/src/lib.rs:347-362`
- **Found by:** agent-12 (M5); agent-17 (?)
- **Impact:** (a) `poisha_to_taka` reintroduces floats for money — the one place the crate admits "a float touches money". Above 2^53 poisha the conversion loses integer precision; even below that, `46.90` as `f64` serialises/rounds differently per courier and the courier may collect 46.89/46.90 while the order total is 46.90. (b) Whole-taka rounding is systematic: every COD ending in 50–99 …
- **Evidence:** pub fn poisha_to_taka(poisha: i64) -> f64 { poisha as f64 / 100.0 } pub fn poisha_to_whole_taka(poisha: i64) -> i64 { if poisha <= 0 { return 0; } (poisha + 50) / 100 } -
- **Fix:** Prefer string/decimal taka (`format!("{}.{:02}", poisha/100, poisha%100)`) for Steadfast/RedX where the API accepts a number, and record the rounding delta (`collected_taka*100 - cod_poisha`) on the shipment so reconciliation can account for it; document the half-up rule to buyers/sellers.

### ME-37 — `allocate_discount` is total only for non-negative subtotals; negative shares + `debug_assert` hide a silent mis-allocation in release

- **Severity:** Medium | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:2028-2088`
- **Found by:** agent-12 (M6); agent-17 (?)
- **Impact:** The "sums exactly to the discount" guarantee — which checkout's money maths relies on (doc comment) — is proved only for non-negative subtotals. A negative subtotal (see C1/H7) yields a negative share, the headroom argument fails, and `remainder` can end non-zero. In debug that panics; in release `debug_assert_eq!` is compiled out and the function silently returns shares that …
- **Evidence:** let total: i64 = subtotals.iter().sum(); if discount <= 0 || total <= 0 { return shares; } let discount = discount.min(total); // ... shares[i] = (i128::from(*subtotal) * i128::from(discount) / i128::from(total)) as i64; // leftover loop assumes "whenever a remainder exists every non-zero group still has headroom" debug_assert_eq!(remainder, 0, "allocation must sum to the discount"); -
- **Fix:** Reject/zero-out negative subtotals at the top (`if subtotals.iter().any(|s| *s < 0) { return vec![0; n] }` or return `Err`), and promote the `debug_assert_eq!` to a real invariant check (or a `#[cfg(test)]` test over adversarial inputs including negatives).

### ME-38 — `validate_stock_adjustment`: `delta.abs()` panics on `i64::MIN`

- **Severity:** Medium | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:2766-2790`
- **Found by:** agent-12 (M8)
- **Impact:** A single request with `delta = -9223372036854775808` (`i64::MIN`, expressible in JSON) makes `abs()` overflow — panic in debug, and in release it wraps to `i64::MIN`, which is still `> MAX` so it errors, but the debug panic is a trivial DoS against the inventory endpoint and the inconsistency shows the pattern is not applied uniformly. -
- **Evidence:** pub fn validate_stock_adjustment(delta: i64, reason: &str) -> Result<&'static str, &'static str> { if delta == 0 { return Err("stock adjustment delta must not be zero"); } if delta.abs() > MAX_STOCK_ADJUSTMENT { return Err("stock adjustment delta is too large"); } Contrast `validate_variant_delta` (`:3137-3146`), which correctly uses `delta_poisha.checked_abs().is_none_or(..)`. -
- **Fix:** Use `delta.checked_abs().is_none_or(|abs| abs > MAX_STOCK_ADJUSTMENT)` exactly as `validate_variant_delta` does.

### ME-39 — Courier responses are parsed with unbounded `response.json()`; courier error text is reflected verbatim

- **Severity:** Medium | **Area:** Domain / Courier / Payments core | **File:** `crates/courier/src/lib.rs:614`
- **Found by:** agent-12 (M9); agent-19 (L3)
- **Impact:** `reqwest::Response::json()` has no size limit — a courier (or a MITM, if TLS is ever downgraded/misconfigured) returning a multi-hundred-MB JSON body exhausts worker memory (DoS). `gateway_message` then embeds the courier's `message`/`errors` object verbatim and unbounded into `CourierError::Gateway`, which surfaces to API errors/logs — a vector for log injection / log …
- **Evidence:** let payload: Value = response.json().await.map_err(|e| { CourierError::Gateway(format!("steadfast create_order returned an unreadable response: {e}")) })?; // gateway_message: if let Some(errors) = payload.get("errors") { return format!("{courier} rejected the request: {errors}"); } -
- **Fix:** Read the body with a size cap (e.g. `response.bytes()` with a 1–2 MB limit, or `reqwest` + `serde_json::from_slice` after a length check) and truncate courier-supplied `message`/`errors` text (e.g. to 500 chars) before embedding it.

### ME-40 — `validate_branding_url` allows `http://` and quote/angle-bracket characters in a hotlinked `<img>` URL

- **Severity:** Medium | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:2451-2469`
- **Found by:** agent-06 (?); agent-12 (M10); agent-13 (M9); agent-19 (L2)
- **Impact:** (a) `http://` is accepted, so a seller's logo/banner can be a plaintext URL — mixed-content downgrade and a tracking-pixel / content-injection vector on an otherwise-HTTPS storefront. (b) Only whitespace is rejected: `"`, `<`, `>`, backticks and control characters pass, so if the URL is ever interpolated into HTML without escaping (the doc says "the web app hotlinks it into an …
- **Evidence:** if trimmed.chars().any(char::is_whitespace) { return Err("image URL must not contain spaces"); } if !trimmed.starts_with("https://") && !trimmed.starts_with("http://") { return Err("image URL must start with https:// or http://"); } Ok(Some(trimmed.to_string())) -
- **Fix:** Require `https://` only (or document why `http://` is needed), reject `" ' < > \`` and control characters, and require a non-empty host via `Url::parse` (host present, no username/password).

### ME-41 — `invoice_filename` uses the store slug unsanitized, while `export_filename` sanitizes the same kind of input

- **Severity:** Medium | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:2714-2717`
- **Found by:** agent-12 (M11)
- **Impact:** If `invoice_filename`'s result is ever used as a filesystem path or a `Content-Disposition` filename (its doc says browsers propose it as the PDF filename), a slug containing `/`, `..`, or a newline yields path traversal / header injection. The slug *is* validated elsewhere (`is_valid_slug`, `:943`), but this function itself performs no sanitization, so any future caller that …
- **Evidence:** pub fn invoice_filename(store_slug: &str, store_seq: i64) -> String { format!("invoice-{store_slug}-{store_seq:06}") } // export_filename, by contrast: let sanitized: String = slug_or_phone.chars().map(|c| { let lower = c.to_ascii_lowercase(); if lower.is_ascii_alphanumeric() || lower == '-' { lower } else { '-' } }).collect(); format!("bd-marketplace-{kind}-{sanitized}-{yyyymmdd}.json") `export_filename` also uses `kind` and `yyyymmdd` as-is (unsanitized), but the invoice case is worse because the slug is seller-controlled. -
- **Fix:** Sanitize the slug in `invoice_filename` exactly as `export_filename` does, reject/escape `/`, `..` and control characters in `kind`/`yyyymmdd` in `export_filename`, and reject negative `store_seq`. ---

### ME-42 — API keys: no per-user count limit, never-expiring by default, no creation/revocation audit or notification

- **Severity:** Medium | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/api_keys.rs:311-350`
- **Found by:** agent-11 (M1)
- **Impact:** A stolen session/JWT can mint unlimited long-lived keys (persistence that survives password/OTP rotation and JWT expiry); a leaked never-expiring key is usable forever unless the owner happens to notice and revoke it. With no audit trail or notification, the owner/admin cannot tell a key was minted, when, or by whom. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/api_keys.rs:311-350` (`create_api_key`); `crates/api/src/api_keys.rs:387-414` (`revoke_api_key`); `domain/src/lib.rs:3299-3307` (`validate_api_key_expiry_days`) - **Code evidence:** `create_api_key` validates name/scopes/expiry and inserts — no `SELECT COUNT(*) … WHERE user_id` cap, no maximum-keys check. `validate_api_key_expiry_days(None) => Ok(None)` — a key with no expiry is legal and is the default when `expires_in_days` is omitted. Neither create nor revoke calls `record_admin_action` or any audit/notification (grep confirms no audi
- **Fix:** Cap keys per user (e.g. 10 active), require an expiry by default (e.g. 90 days; never-expire only via an explicit `never_expires: true` flag), audit-log create/revoke (actor, key id, prefix — never the full key), and notify the account owner (SMS/outbox) on creation.

### ME-43 — API-key rate limiter is per-process, fixed-window, and panics on a poisoned lock

- **Severity:** Medium | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/api_keys.rs:159-221`
- **Found by:** agent-11 (M2); agent-15 (L6)
- **Impact:** (a) **Availability/DoS:** if any thread panics while holding the mutex, every subsequent API-key authentication `expect()`-panics — a poisoned lock bricks *all* API-key auth until process restart (the panic propagates out of the `AuthUser` extractor). (b) **Bypass:** the limit is per-process, so N replicas ⇒ N× the cap; a restart resets every window; a fixed window allows a 2× …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/api_keys.rs:159-221` (`ApiKeyRateLimiter::check_and_record`); `api_keys.rs:190-196` - **Code evidence:** let mut windows = self.windows.lock() .expect("api key rate limiter lock poisoned"); // api_keys.rs:193-194 if windows.len() > 1024 { windows.retain(…); } // opportunistic sweep only let entry = windows.entry(key_id).or_insert((now, 0)); let expired = … age >= API_KEY_RATE_WINDOW; // fixed 60 s window if expired { *entry = (now, 0); } if entry.1 >= limit { return false; } entry.1 += 1; The m
- **Fix:** Replace `.expect()` with lock recovery (`lock().unwrap_or_else(|e| e.into_inner())`) or a `RwLock`; move the limiter to a shared store (Redis/Postgres) or at minimum a sliding-window counter; keep the per-process limiter only as a documented dev fallback.

### ME-44 — Facebook import idempotency check races the insert: concurrent duplicate imports fail the whole batch with a 500

- **Severity:** Medium | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/facebook.rs:304-386`
- **Found by:** agent-11 (M3)
- **Impact:** Two concurrent imports of the same `source_ref` (double-click, retry after a slow response, two tabs) both pass the pre-check, both insert, and the loser's transaction fails on the UNIQUE violation — surfaced as a generic 500 `internal("commit import")`, rolling back the *entire* batch including unrelated valid drafts, with no per-draft "already imported" result. Also a …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/facebook.rs:304-386` (`import_products_for_owner`) - **Code evidence:** // facebook.rs:304-313 — existence check runs BEFORE the transaction opens let existing: Vec<String> = sqlx::query_scalar( "SELECT source_ref FROM products WHERE store_id = $1 AND source_ref = ANY($2)") .bind(store.id).bind(&refs).fetch_all(pool).await…; // facebook.rs:368-380 — only the inserts are in the transaction let mut tx = pool.begin().await…; for product in &ready { created_ids.push(insert_product(&mut *tx, product).await?); } tx.co
- **Fix:** Move the existence check inside the transaction with `SELECT … FOR UPDATE` / advisory lock, or use `INSERT … ON CONFLICT (store_id, source_ref) WHERE source_ref IS NOT NULL DO NOTHING` and report conflicts as `skipped: already imported`. Normalise `source_ref` (trim + case-fold for the comparison, or store a canonical form) before the check.

### ME-45 — Facebook import accepts unbounded seller-edited fields and unbounded prices (preview truncates, import does not)

- **Severity:** Medium | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/facebook.rs:290-365`
- **Found by:** agent-11 (M4)
- **Impact:** A seller (or a `write` API key, see H3) can store a multi-megabyte title/description/`title_bn`/`source_ref` per product (bounded only by the HTTP body limit) and a `price_poisha` up to `i64::MAX` — bloating list/detail responses, breaking storefront rendering, and risking overflow in downstream `SUM(goods)` / GMV aggregates that assume sane prices. The preview/import …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/facebook.rs:290-365` (`import_products_for_owner`); `crates/domain/src/lib.rs:978-1000` (`validate_product_input`) - **Code evidence:** `draft_errors()` calls `validate_product_input`, which checks only `title` non-empty, `price_poisha >= 0`, `stock >= 0`, valid status, weight 1–50,000 — **no maximum** on title length, price, or stock. `import_products_for_owner` then stores `draft.title.trim()`, `draft.title_bn`, `draft.description`, `source_ref` verbatim with no length check at all (only `source_ref` non-empty). Contrast `preview_import
- **Fix:** Enforce the same maxima in the import path as the preview: title ≤120 chars, `title_bn` ≤120, description ≤2,000, `source_ref` ≤64–128 chars, price ≤ a sane cap (e.g. 100,000,000 poisha = ৳1,000,000) in `validate_product_input` or in `draft_errors`; reject, don't silently truncate, seller-edited input.

### ME-46 — Audit log is not tamper-evident, not append-only, and has coverage + read-side gaps

- **Severity:** Medium | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/audit.rs:56-80`
- **Found by:** agent-11 (M5); agent-13 (M11)
- **Impact:** (a) **Tampering:** any DB credential / compromised admin with DB write access can `UPDATE`/`DELETE` audit rows with no detection — there is no hash chain, no append-only constraint, no separate audit store. (b) **Silent loss:** if the admin user row is deleted, the `INNER JOIN` drops their audit rows from the list entirely; if `detail` JSON is corrupted, `.ok()` turns it into …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/audit.rs:56-80` (`record_admin_action`); `audit.rs:121-178` (`list_audit_log`); migration `0028_security_hardening.sql:27-38` - **Code evidence:** // audit.rs:56-80 — free-form strings, no validation, no chaining pub async fn record_admin_action(executor, admin_user_id, action: &str, target_type: &str, target_id: Option<&str>, detail: Option<Value>) { let detail_text = detail.map(|value| value.to_string()); sqlx::query("INSERT INTO admin_audit_log (…) VALUES ($1,$2,$3,$4,$5::jsonb)")… } // audit.rs:144,166 — I
- **Fix:** Add a hash chain (`prev_hash`, `row_hash = HMAC(key, prev_hash || canonical row)`) verified on read; `REVOKE UPDATE, DELETE` on the table from the app role and use a separate audit-only role; change the list join to `LEFT JOIN` and surface unparseable `detail` as an explicit error/flag instead of `None`; make `action`/`target_type` typed enums; add admin/target/date filters.

### ME-47 — Risk score never decays and is trivially reset by a new account (Sybil)

- **Severity:** Medium | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/risk.rs:55-90`
- **Found by:** agent-11 (M6)
- **Impact:** Two opposite failures. (a) **Reset:** a buyer flagged High can abandon the phone/account and re-register with a new SIM/OTP (account creation is just phone+OTP, `auth.rs` `find_or_create_user`) and is instantly `Low` with `requires_advance: false` — the shield has no phone-age, device, address, or delivery-phone signal to carry risk across identities. (b) **Never decays:** a …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/risk.rs:55-90` (`buyer_risk_for_user`); `crates/domain/src/lib.rs:1653-1675` (`assess_buyer_risk`) - **Code evidence:** // risk.rs:62-83 — all-time counts, no date filter, no recency/amount weighting "(SELECT COUNT(*) FROM orders WHERE buyer_user_id = $1) AS total_orders, …" // domain: requires_advance = returned_rto >= 2 && return_rate_bps >= 5_000; // High = requires_advance || (total_orders >= 3 && delivered == 0 // && buyer_cancelled + returned_rto >= 2); // Medium = any return or buyer cancellation; Low = otherw
- **Fix:** Add a time window (e.g. trailing 6–12 months) to the aggregate, decay old events, and add a lightweight cross-account signal (delivery phone/address hash, device fingerprint if available) so a fresh account reusing a flagged delivery phone/address inherits scrutiny. Record the decision rationale (counts + window) on the order for dispute handling.

### ME-48 — Analytics CSV export escapes the API-key export ban

- **Severity:** Medium | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/api_keys.rs:140-172`
- **Found by:** agent-11 (M7)
- **Impact:** The export exclusion is trivially circumvented for sales data: a leaked read-only key can repeatedly download the seller's daily revenue/earnings series (up to 90 days per call, unlimited calls, no rate limit beyond the 600/min key cap, no audit). Less severe than the full JSON export (no PII/order detail), but it defeats the stated intent of the ban and leaks commercially …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/api_keys.rs:140-172` (forbidden list); `crates/api/src/analytics.rs:660-690` (`seller_analytics_csv_handler`); `crates/api/src/lib.rs:1416-1417` - **Code evidence:** The forbidden list blocks `GET /v1/seller/export` and `/v1/buyer/export` ("a full data dump of the account is a session-only action"), but `GET /v1/seller/analytics/export.csv` (`lib.rs:1416-1417`, `analytics.rs:660`) is **not** in the list, and it is a `GET`, so a `read`-scope key passes both the route and scope checks. The CSV carries the full daily sales ledger (`build_dai
- **Fix:** Add `"/v1/seller/analytics/export.csv"` (and any future `*.csv` export) to `api_key_forbidden_route`, or — better — treat every `export` path segment as JWT-only.

### ME-49 — API-key auth errors distinguish revoked / expired / never-existed keys (validity oracle)

- **Severity:** Medium | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/api_keys.rs:430-455`
- **Found by:** agent-11 (M8)
- **Impact:** An attacker probing candidate keys learns *why* a key failed, not just that it failed: "revoked"/"expired" confirms the key *existed and was once valid* (useful for confirming a guessed/leaked prefix or a key harvested from logs), while "invalid" means it never existed. With 238-bit keys this does not make brute force feasible, but it leaks lifecycle state and aids validation …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/api_keys.rs:430-455` (`authenticate_api_key`) - **Code evidence:** let row = row.ok_or_else(|| ApiError::unauthorized("invalid api key"))?; if row.revoked_at.is_some() { return Err(ApiError::unauthorized("api key has been revoked")); } if row.expires_at.is_some_and(|expiry| expiry <= Utc::now()) { return Err(ApiError::unauthorized("api key has expired")); } - **Impact:** An attacker probing candidate keys learns *why* a key failed, not just that it failed: "revoked"/"expired" confirms the key *existed and was o
- **Fix:** Return a single generic `401 "invalid api key"` for all three cases; log the specific reason server-side only.

### ME-50 — Facebook link / unlink / import have no audit trail and no rate limit

- **Severity:** Medium | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/facebook.rs:62-95`
- **Found by:** agent-11 (M9)
- **Impact:** Impersonation (H6) and cap-bypass (H4) abuse are invisible after the fact: there is no record of who linked which Page when, who unlinked a legitimate Page (denial-of-identity — unlinking is a single `DELETE` with no confirmation/audit), or who bulk-imported what. Combined with H3, a stolen `write` key can silently re-link a store to an attacker's Page and mass-import …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/facebook.rs:62-95, 290-386` (all service functions); grep-verified: no `record_admin_action`, no logging, no throttle in `facebook.rs` - **Code evidence:** `link_facebook_page_for_owner`, `unlink_facebook_page_for_owner`, and `import_products_for_owner` perform privileged identity and catalogue writes with no audit call, no `last_used`/event row, and no per-store rate limit (only the 50-item batch cap). - **Impact:** Impersonation (H6) and cap-bypass (H4) abuse are invisible after the fact: there is no record of who linked which Page when
- **Fix:** Write an audit/event row for link/unlink/import (actor, store, page_id, item count, timestamp) — a seller-action log table, not necessarily `admin_audit_log` — and rate-limit link/unlink (e.g. 5/hour/store) and imports (e.g. 10/day/store). ---

### ME-51 — Search `store_slug` and `category` filters are completely unvalidated (no length, no charset)

- **Severity:** Medium | **Area:** Input Validation / SSRF / Uploads | **File:** `crates/api/src/search.rs:95-99`
- **Found by:** agent-19 (M2)
- **Impact:** Anonymous caller sends a multi-KB `store_slug` / `category` (bounded only by the HTTP URI length). Values are bound, so no SQLi, but Postgres must hash/compare a huge string per row and `lower()` it per row; combined with the leading-wildcard ILIKE arms this is another anonymous expensive-query vector. It also means a typo'd 5 KB slug is silently accepted as "no match" rather …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/search.rs:95-99` (`search_products`), `crates/api/src/search.rs:258-282` (`search_handler`), `crates/domain/src/lib.rs:2218-2280` (`validate_search_params` — validates only `q`/prices/sort/page/per_page) - **Code evidence:** // search.rs:95-99 — trim-only, then bound straight into the query let store_slug = store_slug.map(str::trim).filter(|s| !s.is_empty()); let category = category.map(str::trim).filter(|s| !s.is_empty()); // SEARCH_WHERE (search.rs:76-89): AND ($3::text IS NULL OR s.slug = $3) //
- **Fix:** Validate both in `validate_search_params` or in `search_handler`: `store_slug` via the existing `domain::is_valid_slug` (3–40 chars, `[a-z0-9-]`), `category` via the existing `domain::validate_category` (≤60 chars). Reject anything longer with 400.

### ME-52 — Facebook import `source_ref` has no length or charset limit, and is stored verbatim as `TEXT`

- **Severity:** Medium | **Area:** Input Validation / SSRF / Uploads | **File:** `crates/api/src/facebook.rs:213-240`
- **Found by:** agent-19 (M3)
- **Impact:** A seller imports drafts with a `source_ref` up to the 1 MiB body cap, containing newlines, control characters, `/`, `..`, quotes, or megabytes of junk. It is stored, echoed back in every draft/product view, used as the idempotency key (so a giant ref poisons the `UNIQUE (store_id, source_ref)` index with a huge entry), and — see L4 — interpolated into error messages. It is …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/facebook.rs:213-240` (`preview_import`), `crates/api/src/facebook.rs:290-330` (`import_products_for_owner`); schema `migrations/0014_facebook_import.sql:19-23` - **Code evidence:** // facebook.rs:213-240 — preview checks only non-empty + post-text length for post in posts { if post.source_ref.trim().is_empty() { return Err(ApiError::bad_request("every post needs a source_ref")); } if post.text.chars().count() > MAX_POST_TEXT_CHARS { ... } // ... source_ref.clone() stored in the draft view verbatim
- **Fix:** Validate `source_ref` in both preview and import: e.g. 1–128 chars, `[A-Za-z0-9_-]` only (or at minimum no whitespace/control characters), rejecting the rest with 400. Add a DB `CHECK (char_length(source_ref) <= 128)`.

### ME-53 — Outbound SSRF surface is operator-configured but unvalidated: `SIDRA_API_URL` and courier `*_BASE_URL` accept any URL, incl. private/internal hosts, with redirects followed

- **Severity:** Medium | **Area:** Input Validation / SSRF / Uploads | **File:** `crates/api/src/lib.rs:1004-1020`
- **Found by:** agent-19 (M6)
- **Impact:** A misconfigured (or, if env is ever set from a less-trusted source, attacker-influenced) `SIDRA_API_URL=http://169.254.169.254/latest/meta-data/` or `http://localhost:2375/` (Docker socket) makes the API POST a signed JSON payload containing conversation/message content to cloud metadata or an internal service on **every buyer message**, with `reqwest`'s default redirect …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/lib.rs:1004-1020` (`sidra_config_from_env`), `crates/api/src/lib.rs:936-989` (`couriers_config_from_env`), `crates/api/src/messaging.rs:575-600` (`notify_sidra`), `crates/courier/src/lib.rs:401-404,721-724,1092-1095,1335-1338,1594-1597` (client builders) - **Code evidence:** // lib.rs:1004-1020 — env value taken verbatim, no scheme/host validation fn sidra_config_from_env() -> SidraConfig { let config = SidraConfig { api_url: env_value("SIDRA_API_URL"), ... }; // lib.rs:951-979 — same for every courier base URL: base_url:
- **Fix:** At startup, validate `SIDRA_API_URL` and every `*_BASE_URL`: require `https://` (allow `http://` only in dev mode), parse with `url::Url`, and reject loopback / RFC-1918 / link-local / metadata hosts (`127.0.0.0/8`, `10/8`, `172.16/12`, `192.168/16`, `169.254.0.0/16`, `::1`) unless an explicit `ALLOW_PRIVATE_UPSTREAMS=true` dev flag is set. Build the `reqwest` clients with `redirect(reqwest::redirect::Policy::none())` (or a custom policy that refuses redirects to private IPs) and a …

### ME-54 — Checkout and store free-text fields have no maximum length (same root cause as H3, different surface)

- **Severity:** Medium | **Area:** Input Validation / SSRF / Uploads | **File:** `crates/api/src/orders.rs:635-639`
- **Found by:** agent-19 (M7)
- **Impact:** A buyer places an order with a ~1 MiB `delivery_name` / `delivery_address`; a seller creates a store with a ~1 MiB name/description. Both are stored, then rendered on every order detail, seller dashboard, receipt, invoice, and (for the store) storefront — the same read-amplification as H3, plus courier payloads that embed the delivery address verbatim (courier APIs have their …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/orders.rs:635-639`, `crates/api/src/stores.rs:87-108` (`create_store_for_user`), `crates/api/src/stores.rs:261-265` (`CreateStoreRequest`) - **Code evidence:** // orders.rs:635-639 — only emptiness is checked if input.delivery_name.trim().is_empty() { return Err(ApiError::bad_request("delivery_name must not be empty")); } if input.delivery_address.trim().is_empty() { return Err(ApiError::bad_request("delivery_address must not be empty")); } // (delivery_phone IS validated via normalize_bd_phone, :644; delivery_zone via // pa
- **Fix:** Add maxima in the domain crate and enforce them at the handlers (e.g. delivery name ≤ 100 chars, delivery address ≤ 500, store name ≤ 100, store description ≤ 1,000 — matching the existing `validate_store_description` limit, which the create path currently does not call). Add matching DB `CHECK (char_length(…) <= …)` constraints. ---

### ME-55 — Admin is two sources of truth: `role = 'admin'` and `is_admin` can disagree

- **Severity:** Medium | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (M1)
- **Impact:** a user can have `role='admin', is_admin=false` or `role='buyer', is_admin=true`. Any code path that checks the wrong one grants or denies admin inconsistently — privilege escalation or lockout depending on which check a given endpoint uses. -
- **Evidence:** `role text NOT NULL DEFAULT 'buyer' CHECK (role IN ('buyer', 'seller', 'admin'))` (0001:9) vs. `ALTER TABLE users ADD COLUMN is_admin boolean NOT NULL DEFAULT false;` (0013:8-9). The 0013 header says "Admin is a FLAG on the user, not a role" — yet the `'admin'` role value was never removed. -
- **Fix:** remove `'admin'` from the role `CHECK` (migrate existing `role='admin'` rows to `role='seller'`/`'buyer'` + `is_admin=true`), and enforce `CHECK (role IN ('buyer','seller'))`. Add a comment/test asserting exactly one admin signal.

### ME-56 — Payment state is incoherent: `paid` without `paid_at`, COD rows allowed, webhook events replayable

- **Severity:** Medium | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (M3)
- **Impact:** a `paid` payment with `paid_at IS NULL` (or a `pending` payment with `paid_at` set) corrupts paid-at reporting and any "paid before X" logic; a `method='cod'` payments row contradicts the stated model; a replayed webhook inserts unlimited duplicate `payment_events` rows (log flooding / double-processing if any consumer counts events). -
- **Evidence:** `status … CHECK (status IN ('pending','paid','failed','refunded'))`, `paid_at timestamptz` (nullable, line 28) with no `CHECK (status <> 'paid' OR paid_at IS NOT NULL)` and no `CHECK (status = 'paid' OR paid_at IS NULL)`; `method CHECK` includes `'cod'` although the header (lines 5–7) says "COD orders deliberately have NO payments row"; `payment_events.event_type text NOT NULL` (line 37) has no `CHECK`/vocabulary and no dedup key, `raw_body text` is unbounded. -
- **Fix:** add the `paid_at` coherence checks; add `CHECK (method <> 'cod')` (or remove `'cod'` from the method list); add `CHECK (event_type IN (…))` and a dedup key such as `UNIQUE (payment_id, event_type, gateway_ref)` / an idempotency key column on `payment_events`; cap `raw_body` length.

### ME-57 — Return requests: refund state/amount/decided-at incoherent, and the (sub-order, order, buyer, store) tuple is unchecked

- **Severity:** Medium | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (M4)
- **Impact:** nothing requires the cited `sub_order` to belong to the cited `order`, `buyer`, or `store` — a return can cite sub-order A, order B, buyer C, store D. `refund_status='done'` with `refund_poisha IS NULL`, `refund_status='none'` with a refund amount set, and `status='approved'`/`'rejected'` with `decided_at IS NULL` are all storable, so refund reporting can claim a refund was …
- **Evidence:** sub_order_id uuid NOT NULL UNIQUE REFERENCES sub_orders (id) ON DELETE CASCADE, order_id uuid NOT NULL REFERENCES orders (id) ON DELETE CASCADE, buyer_id uuid NOT NULL REFERENCES users (id), store_id uuid NOT NULL REFERENCES stores (id), refund_poisha bigint CHECK (refund_poisha IS NULL OR refund_poisha >= 0), refund_status text NOT NULL DEFAULT 'none' CHECK (refund_status IN ('none','pending','done')), decided_at timestamptz -- nullable, no coherence check -
- **Fix:** enforce the tuple in a trigger (or denormalise: store only `sub_order_id` and derive order/buyer/store by join); add `CHECK (refund_status = 'none' OR refund_poisha IS NOT NULL)`, `CHECK (refund_poisha IS NULL OR refund_poisha <= <sub-order goods>)` via trigger, and `CHECK (status IN ('requested') OR decided_at IS NOT NULL)`.

### ME-58 — API keys: `scopes` is free text, hash is unsalted fast SHA-256, expiry/revocation coherence unchecked

- **Severity:** Medium | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (M6)
- **Impact:** a bug or tampered insert can store `scopes = 'admin'`, `'read,write,delete'`, or `''` — scope enforcement is entirely in the auth layer, so a loose parser there escalates the key. SHA-256 is a fast, unsalted hash: if the DB leaks, low-entropy keys are offline-brute-forceable (the 12-char cleartext `key_prefix` also narrows the search). A key can be created already expired or …
- **Evidence:** key_hash text NOT NULL UNIQUE, key_prefix text NOT NULL, scopes text NOT NULL DEFAULT 'read', -- no CHECK; header (lines 18-20) says only 'read' or 'read,write' last_used_at timestamptz, expires_at timestamptz, revoked_at timestamptz, ... No `CHECK (scopes IN ('read','read,write'))`, no salt/pepper column for the hash, no `CHECK (expires_at > created_at)`, no uniqueness on `(user_id, name)`. -
- **Fix:** `CHECK (scopes IN ('read', 'read,write'))`; store keys as a salted slow hash (e.g. store `key_hash` as Argon2/bcrypt of the key, or HMAC-SHA-256 with a server-side pepper kept out of the DB); add `CHECK (expires_at IS NULL OR expires_at > created_at)`; add `UNIQUE (user_id, name)` for manageability; do not `ON DELETE CASCADE` keys silently — retain a revoked-key record for audit (see H9).

### ME-59 — Duplicate sub-order per store per order, and duplicate product lines inside one sub-order

- **Severity:** Medium | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (M8)
- **Impact:** the same store can get two sub-orders for one order (double fulfilment, double commission/payout snapshots for the same goods), and the same product can appear on two lines of one sub-order (double-counting in any `SUM(line_total)` that assumes one line per product). Both are silent data corruption, not an error. -
- **Evidence:** `sub_orders` has indexes on `(order_id)` (line 22) and `(store_id, created_at DESC)` (line 23) but no `UNIQUE (order_id, store_id)`; `order_items` has an index on `(sub_order_id)` (line 35) but no `UNIQUE (sub_order_id, product_id)`. -
- **Fix:** `UNIQUE (order_id, store_id)` on `sub_orders` (or `UNIQUE (order_id, store_id, …)` if a legitimate multi-split per store is ever intended — document it); `UNIQUE (sub_order_id, product_id)` on `order_items` (with `variant_id` in the key once variants exist: `UNIQUE (sub_order_id, product_id, variant_id) NULLS NOT DISTINCT`).

### ME-60 — The stock audit trail is voluntary: no trigger ties `products.stock` writes to `stock_adjustments`

- **Severity:** Medium | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (M10)
- **Impact:** the header (lines 11–19) promises every stock write inserts a row "in the SAME transaction" — the DB does not enforce it. A direct `UPDATE products SET stock = …` (bug, script, admin tool) leaves no trail at all, and a negative `new_stock` snapshot is storable, so the trail can claim −5 on hand. `reason` accepts any string (audit vocabulary unenforced), and `actor='seller'` …
- **Evidence:** ALTER TABLE products ADD COLUMN low_stock_threshold integer NULL CHECK (low_stock_threshold IS NULL OR low_stock_threshold >= 0); CREATE TABLE stock_adjustments ( product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE, store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE, actor text NOT NULL CHECK (actor IN ('seller','system')), actor_user_id uuid NULL REFERENCES users (id) ON DELETE SET NULL, delta integer NOT NULL CHECK (delta <> 0), new_stock integer NOT NULL, -- no CHECK (new_stock >= 0) reason text NOT NULL, -- free text; header says "the validated reason vocabulary" is service-only note text NULL, ...); -
- **Fix:** add a trigger on `products.stock` that inserts the `stock_adjustments` row (or rejects a stock change with no accompanying adjustment row); add `CHECK (new_stock >= 0)`, `CHECK (reason IN ('order','cancel_restock','return_restock','rto_restock', …))`, and `CHECK (actor <> 'seller' OR actor_user_id IS NOT NULL)`.

### ME-61 — Subscriptions: auto-renew defaults ON, periods can be inverted, stale `active` rows keep granting, no history

- **Severity:** Medium | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (M13)
- **Impact:** every paid plan silently auto-renews from the seller's earnings balance unless the seller opts out — an unsafe default for a money charge. `expires_at <= started_at` creates a stillborn `active` row; `status='active'` with `expires_at` in the past stays `active` in the DB (the header admits the API must check both), so any query filtering only on `status` grants expired …
- **Evidence:** store_id uuid NOT NULL UNIQUE REFERENCES stores (id) ON DELETE CASCADE, plan text NOT NULL CHECK (plan IN ('free','pro','business')), status text NOT NULL DEFAULT 'active' CHECK (status IN ('active','cancelled','expired')), started_at timestamptz NOT NULL DEFAULT now(), expires_at timestamptz NOT NULL, -- no CHECK (expires_at > started_at) auto_renew boolean NOT NULL DEFAULT true, ... `store_id UNIQUE` means renewals overwrite the single row (header lines 3–8, 35–39). -
- **Fix:** default `auto_renew` to `false` (opt-in renewal) or require explicit consent at purchase; add `CHECK (expires_at > started_at)`; add a partial index / sweep that flips expired `active` rows to `expired`; keep a `store_subscription_history` table (or never `UPDATE` the row — insert a new period row per renewal).

### ME-62 — Dangling / denormalised references with no FK: coupon code, charge `ref_id`, loyalty `ref_id`, OTP phone, notification phone

- **Severity:** Medium | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-07 (L4); agent-13 (M14)
- **Impact:** an order can cite a coupon code that never existed (or was deleted); a charge / loyalty row can cite a promotion, subscription, order, or sub-order that never existed; a notification can SMS a phone number that no longer belongs to the user (wrong-number disclosure of order/payment information). Deleting a coupon leaves `orders.coupon_code` pointing at nothing, with no error. -
- **Evidence:** otp_codes.phone text NOT NULL; -- no FK to users(phone) orders.coupon_code text; -- no FK to coupons(code) seller_charges.ref_id uuid; -- no FK to anything loyalty_points.ref_id uuid; -- no FK to anything (polymorphic: sub-order OR order) notifications.phone text NOT NULL; -- copy of users.phone, can diverge -
- **Fix:** replace `orders.coupon_code` with `coupon_id uuid REFERENCES coupons (id)` (keep the code as a snapshot only if needed, populated from the joined row); give `seller_charges` typed FK columns (`promotion_id`, `subscription_id`) instead of a polymorphic `ref_id`; split `loyalty_points.ref_id` into `sub_order_id` / `order_id` FKs; derive `notifications.phone` from `users` at send time instead of snapshotting it (or add a trigger keeping it in sync). ---

### ME-63 — Coupons: percent can exceed 100%, `used_count` can exceed `usage_limit`, code uniqueness is case-sensitive, window can be inverted, no per-buyer limit

- **Severity:** Medium | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (M2); agent-20 (L2)
- **Impact:** a `percent` coupon with `discount_value = 500` is a 500% discount at the DB layer; a lower-case `'save10'` and upper-case `'SAVE10'` are two distinct coupons despite the case-insensitive claim; `used_count` can pass `usage_limit` under race/bug; an `ends_at` before `starts_at` is a coupon that can never (or always) apply. `coupon_redemptions` has no `UNIQUE (coupon_id, …
- **Evidence:** code text NOT NULL UNIQUE, discount_type text NOT NULL CHECK (discount_type IN ('percent','fixed')), discount_value bigint NOT NULL CHECK (discount_value > 0), -- no <= 100 for percent starts_at timestamptz, ends_at timestamptz, -- no CHECK (ends_at > starts_at) usage_limit int CHECK (usage_limit > 0), used_count int NOT NULL DEFAULT 0 CHECK (used_count >= 0); -- no CHECK (used_count <= usage_limit) The header (lines 7–8) claims codes are stored "in their normalized (upper-case) form" — the DB has no `CHECK (code = upper(code))`, no `CITEXT`, and no functional unique index on `upper(code)`. -
- **Fix:** `CHECK (discount_type <> 'percent' OR discount_value <= 100)`, `CHECK (ends_at IS NULL OR starts_at IS NULL OR ends_at > starts_at)`, `CHECK (usage_limit IS NULL OR used_count <= usage_limit)`, `CHECK (code = upper(code))` or a `UNIQUE` index on `upper(code)`, and `UNIQUE (coupon_id, buyer_id)` if per-buyer redemption is intended (or document that it is not).

### ME-64 — No tax / VAT anywhere in the backend

- **Severity:** Medium | **Area:** Money Math / Currency | **File:** `crates/api/src/invoices.rs:89-106`
- **Found by:** agent-17 (?)
- **Impact:** if the platform's commission or sales are VAT-able in Bangladesh, every invoice understates the tax due and the platform has no record from which to file or remit it. This is a compliance/finance gap, not an exploitable code bug — but it must be a deliberate, documented decision, not an accident. -
- **Evidence:** no tax rate, no tax computation, no tax field on orders, sub-orders, invoices or receipts; `build_order_totals` is `goods − discount + delivery` with no tax term. -
- **Fix:** confirm with a tax adviser whether VAT applies to marketplace commission and/or sales; if so, add a `tax_poisha` term (rate as a platform setting, computed on the same base as commission, snapshotted per sub-order like commission) and a tax line on invoices/receipts.

### ME-65 — Startup commission rate allows 100% (10,000 bps); the DB setting caps at 30%

- **Severity:** Medium | **Area:** Money Math / Currency | **File:** `crates/api/src/lib.rs:673`
- **Found by:** agent-17 (?)
- **Impact:** `COMMISSION_RATE_BPS=10000` silently takes 100% of every seller's goods as commission (`seller_payout_poisha = 0`). Two paths that are documented as "mirroring" each other enforce different ceilings, so the same rate is legal via env and illegal via the admin UI. A typo (`8000` for `800`) is a 10× commission hike that no validation catches. -
- **Evidence:** // lib.rs:673 — startup fallback Ok(rate) if rate <= 10_000 => state.commission_rate_bps = rate, // domain validate_commission_bps: (0..=3_000).contains(&bps) — admin settings path enforces this -
- **Fix:** route the env parse through `validate_commission_bps` (cap 3,000 bps) like the admin path; log loudly when falling back to the default.

### ME-66 — Admin analytics GMV ignores loyalty discounts and counts returned orders

- **Severity:** Medium | **Area:** Money Math / Currency | **File:** `crates/api/src/analytics.rs:498-510`
- **Found by:** agent-17 (?)
- **Impact:** GMV is overstated by (loyalty discounts + returned-order value) — reporting/finance decisions, seller rankings and any revenue-share math built on it are wrong. (Note the commission line in the same query *is* correct: it uses `goods − discount − seller_payout` over `delivered` only.) -
- **Evidence:** (SELECT COALESCE(SUM(goods_subtotal_poisha - discount_poisha + delivery_charge_poisha), 0) FROM sub_orders WHERE status <> 'cancelled' ...) AS gmv_poisha `status <> 'cancelled'` includes `returned` sub-orders; `loyalty_discount_poisha` is never subtracted, while the buyer actually paid `GMV − loyalty_discount`. -
- **Fix:** GMV = `SUM(goods − discount − loyalty_discount + delivery)` over `status IN ('placed','confirmed','shipped','delivered')` (exclude `returned` as well as `cancelled`); keep a separate returned-goods figure.

### ME-67 — Booked parcel weight is re-read from *current* product weights, not the checkout snapshot; weight conversion goes through `f64`

- **Severity:** Medium | **Area:** Money Math / Currency | **File:** `crates/api/src/shipments.rs:640-660`
- **Found by:** agent-09 (L6); agent-17 (?)
- **Impact:** a seller edits a product's weight after checkout (heavier or lighter); the courier is booked at a different weight than the buyer was charged for, so the courier's bill and the collected delivery fee diverge — the platform eats over-weight parcels and cannot explain under-weight ones. The `f64` division + `.ceil()` is also fragile at exact kilogram boundaries in principle (a …
- **Evidence:** // shipments.rs:655-659 — "Product weights can be edited after checkout; // the booked weight is the products' weight now." SELECT COALESCE(SUM(p.weight_grams * oi.quantity), 0) FROM order_items oi JOIN products p ON p.id = oi.product_id WHERE oi.sub_order_id = $1 let weight_kg = weight_kg_from_grams(total_grams).ceil().max(1.0) as u32; // domain: pub fn weight_kg_from_grams(total_grams: i64) -> f64 { total_grams as f64 / 1_000.0 } -
- **Fix:** snapshot `unit_weight_grams` onto `order_items` at checkout (the title/price already are) and book from the snapshot; replace `weight_kg_from_grams` + `ceil` with integer ceiling division for billing.

### ME-68 — Advance-share allocation floors per sub-order, so a few poisha of the advance are never allocated

- **Severity:** Medium | **Area:** Money Math / Currency | **File:** `crates/api/src/shipments.rs:612-617`
- **Found by:** agent-17 (?)
- **Impact:** on a multi-seller advance order, the per-sub-order advance shares sum to *less* than `advance_paid_poisha` by up to (n−1) poisha. The courier therefore collects those poisha again in cash (booking), and the summed invoice `cod_amount_due` exceeds the receipt's `grand_total − advance_paid` by the same remainder. Small per order, but it is a systematic buyer overcharge and a …
- **Evidence:** // shipments.rs:612-616 let advance_share = if order_collect_total > 0 { row.advance_paid_poisha * sub_share / order_collect_total // floor; no remainder distribution } else { 0 }; // invoices.rs:366 — same floor, same missing remainder order.advance_paid_poisha * grand_total_poisha / order.grand_total_poisha `allocate_discount` (`domain/src/lib.rs:2028`) exists precisely to distribute a remainder exactly — but neither advance-share site uses it. -
- **Fix:** allocate the advance across sub-orders with `allocate_discount`-style exact allocation (floor + remainder to largest shares) in one shared helper used by both booking and invoices.

### ME-69 — COD fee is never priced, and the courier's actual charge is never reconciled against what the buyer was charged

- **Severity:** Medium | **Area:** Money Math / Currency | **File:** `crates/domain/src/lib.rs:101-116`
- **Found by:** agent-17 (?)
- **Impact:** Bangladeshi couriers typically charge a COD handling percentage; the platform collects a flat delivery fee and eats any COD fee plus any difference between its tariff and the courier's real charge, on every order, with no visibility. Combined with H-01 (buyer-picked zone), the delivery line systematically under-recovers. -
- **Evidence:** the delivery function takes `cod_amount_poisha`, asserts it is non-negative, and then ignores it ("no COD percentage is baked in … Wave 1 charges no COD fee rather than inventing one"). Every courier adapter's `price_estimate` just calls that same local function — it is the platform's tariff, never the courier's quote. Shipments store `cod_amount_poisha` but no courier-fee field, so nothing can ever be reconciled. -
- **Fix:** confirm each courier's COD fee in writing (as the code comment itself requires), add it as an explicit priced term, store the courier's confirmed charge on the shipment row at booking, and add a reconciliation report (charged vs courier-confirmed) for admin.

### ME-70 — Seller-controlled store name is interpolated verbatim into buyers' SMS: SMS content injection / spoofing

- **Severity:** Medium | **Area:** Notifications / Notify / Worker | **File:** `crates/api/src/notices.rs:63-73`
- **Found by:** agent-10 (M1)
- **Impact:** a malicious seller names their store with newlines and fake platform text, e.g. `Shop\nBD Marketplace: আপনার OTP 123456 — লিংকে ক্লিক করুন https://evil.example/...`, and every buyer who orders from them receives an SMS that visually contains a second, forged "BD Marketplace" message — phishing / smishing from the platform's own sender ID, which is exactly the trust SMS relies …
- **Evidence:** // stores.rs:95 — the entire validation of a store name if name.trim().is_empty() { return Err(ApiError::bad_request("store name must not be empty")); } // domain render_body, e.g. sub_order_shipped: NOTIFICATION_SUB_ORDER_SHIPPED => format!( "BD Marketplace: {store} থেকে আপনার অর্ডার #{short}-এর পণ্য পাঠানো হয়েছে.") No length cap, no newline/control-character stripping, no truncation anywhere between store creation and `render_body`. `vars_from_payload` copies `store_name` straight from JSON. -
- **Fix:** validate store names at creation (max ~60 chars, reject `\n`, `\r` and other control characters), and defence-in-depth in `render_body`/`vars_from_payload`: strip/replace newlines and control chars in `store_name` and truncate to a fixed length before interpolation.

### ME-71 — No idempotency / dedup: nothing stops the same event from queueing (and sending) twice

- **Severity:** Medium | **Area:** Notifications / Notify / Worker | **File:** `crates/notify/src/lib.rs:171-207`
- **Found by:** agent-10 (M2)
- **Impact:** any caller bug, retried request that re-executes a state transition, or future funnel added without the same care double-queues the SMS: buyers get duplicate "order shipped / payout paid" texts, and each duplicate costs a gateway fee and erodes trust ("did I get paid twice?"). There is no defence in depth at the layer that actually sends the money. -
- **Evidence:** `enqueue` is a bare `INSERT` with no dedup key, no `ON CONFLICT`, and no check for an existing pending/sent row for the same event. The schema's only indexes are the PK and `notifications_pending_idx (status, created_at)`. The "exactly once" guarantee rests entirely on comments in `notices.rs:1-35` asserting that each funnel fires once (mutually exclusive transitions, one-way delivered state) — a property of *callers*, not of the outbox. -
- **Fix:** add an idempotency key to `enqueue` (e.g. `dedup_key` = `kind + order_id / sub_order_id / return_id`, unique-indexed) and `INSERT ... ON CONFLICT (dedup_key) DO NOTHING` (returning the existing id). Keep the caller funnels as the first line of defence, but make double-enqueue structurally impossible.

### ME-72 — No validation at the `enqueue` boundary: unknown kinds render a misleading generic SMS, phones are unvalidated, payload fields silently vanish

- **Severity:** Medium | **Area:** Notifications / Notify / Worker | **File:** `crates/notify/src/lib.rs:143-163`
- **Found by:** agent-10 (M3)
- **Impact:** a typo'd or attacker-influenced kind (via a future caller passing user input through) queues a row whose body is a generic "you have an update" with an empty order number — confusing at best, and a wasted paid SMS. A malformed phone (empty string, garbage) is stored, retried 3 times, and lands in `failed`, polluting the admin view and masking real failures. Silently-dropped …
- **Evidence:** // vars_from_payload: wrong-typed or missing fields silently become empty/None order_id: payload.get("order_id").and_then(Value::as_str).unwrap_or_default().to_string(), amount_poisha: payload.get("amount_poisha").and_then(Value::as_i64), // domain render_body: unknown kind -> generic line, no error _ => format!("BD Marketplace: অর্ডার #{short} সম্পর্কে একটি আপডেট আছে।"), `enqueue` accepts any `kind` string and any `phone` string; the table constrains only `status` (`CHECK (status IN (...))`), not `kind`. All current callers pass constants, but the public function — the "ONE writer" every future funnel must use — enforces nothing. -
- **Fix:** validate in `enqueue`: reject unknown `kind` (allowlist of the 7 domain constants), reject empty/unparseable phones via `domain::normalize_bd_phone`, and require the fields each kind needs (return an error rather than rendering defaults). Add a DB `CHECK (kind IN (...))` as backstop.

### ME-73 — Throughput cap of 2 SMS/second with no priority: a burst of orders delays notifications for hours

- **Severity:** Medium | **Area:** Notifications / Notify / Worker | **File:** `crates/notify/src/lib.rs:40-41`
- **Found by:** agent-10 (M4)
- **Impact:** a sale, festival rush, or an attacker generating many orders (see H4) creates a backlog that drains at 2/s — 1,000 queued messages take 8+ minutes, 10,000 take 1.4 hours. Delivery/payout notifications arrive long after the event, and the admin view shows a growing `pending` count with no alert (see M7). -
- **Evidence:** `pub const DEFAULT_BATCH: i64 = 20;` and the worker calls `notify::process_pending(&pool, sender.as_ref(), notify::DEFAULT_BATCH)` once per `WORKER_POLL_SECONDS` (default 10). That is 20 messages per 10 s = 2/s, sequential, oldest-first, with no priority between kinds (a `payout_paid` waits behind 500 `order_placed` rows) and no way to raise it without a code change + redeploy. -
- **Fix:** make batch size and poll interval env-configurable, send within a batch concurrently with a bounded semaphore (e.g. 5–10 in flight), and prioritise kinds (payout/return updates before bulk order-placed). Monitor queue depth and alert on sustained growth.

### ME-74 — Outcome update is unguarded (`WHERE id = $1` only) and the claim order is nondeterministic for equal timestamps

- **Severity:** Medium | **Area:** Notifications / Notify / Worker | **File:** `crates/notify/src/lib.rs:249-260`
- **Found by:** agent-10 (M5)
- **Impact:** today there is no second writer, so this is latent — but the moment a reaper (H1 fix) or an admin retry action exists, a late outcome write from a stale worker can overwrite the reaper's reset: a message already re-queued and re-sent gets marked `sent` by the first attempt's delayed write, or a `failed` row is silently resurrected to `pending`. Nondeterministic claim order …
- **Evidence:** // claim: ORDER BY created_at only — rows created in the same instant have no tiebreak ORDER BY created_at LIMIT $1 FOR UPDATE SKIP LOCKED // outcome: no status guard, no version check "UPDATE notifications SET status = 'sent', sent_at = now(), last_error = NULL WHERE id = $1" "UPDATE notifications SET status = $2, attempts = $3, last_error = $4 WHERE id = $1" -
- **Fix:** guard outcome writes with `AND status = 'sending'` and check the affected-row count (0 rows = someone else already moved it; log and skip). Add `id` as a secondary `ORDER BY` key (`ORDER BY created_at, id`).

### ME-75 — Notification failure aborts the business transaction: a notify problem can take down checkout, payouts, and returns

- **Severity:** Medium | **Area:** Notifications / Notify / Worker | **File:** `crates/api/src/notices.rs:75-86`
- **Found by:** agent-10 (M6)
- **Impact:** if the `notifications` table is locked, its migration is missing after a bad deploy, or the row violates a future constraint, *all* checkouts fail, no payout can be marked paid, and no return can be decided — a notification-table problem becomes a total marketplace outage, with a misleading "enqueue notification" 500 as the only symptom. -
- **Evidence:** // notices.rs:75-85 — enqueue failure is mapped to ApiError::internal and propagated notify::enqueue(&mut *conn, row.buyer_user_id, &row.delivery_phone, kind, &payload) .await .map_err(|e| ApiError::internal("enqueue sub-order notification", e))?; Every caller runs this inside the order/payout/return transaction and propagates the error with `?`, so a failure to INSERT the notification row rolls back the entire state change. The outbox pattern is meant to couple them — but it couples availability in both directions. -
- **Fix:** keep the transactional enqueue as the default, but distinguish error classes: on a transient DB error, retry the insert inside the transaction; consider a fallback that records the notification intent in a separate minimal table (or logs a high-severity alert) rather than failing the customer's checkout. At minimum, add alerting on `enqueue` failures so they are not discovered via customer complaints.

### ME-76 — No alerting or metrics on notification health: failures accumulate silently except in an admin view nobody watches

- **Severity:** Medium | **Area:** Notifications / Notify / Worker | **File:** `crates/worker/src/main.rs:212-247`
- **Found by:** agent-10 (M7)
- **Impact:** the platform can run for weeks with zero SMS delivery (buyers never told their order shipped, sellers never told they were paid) and nobody is paged; the failure is discovered by an angry seller, not by monitoring. Combined with H5, a gateway outage permanently burns messages before anyone looks. -
- **Evidence:** the worker logs a one-line summary only when a batch processed > 0 rows, and logs errors to stderr; there is no metric, no threshold, and no alert on `failed` count, `pending` depth, stuck `sending` rows, or a sender that fails 100% of sends. In production today the sender is `UnconfiguredSmsSender`, so *every* notification deterministically fails — and the only signal is a growing `failed` count in an admin page. -
- **Fix:** emit structured metrics (pending depth, sent/retried/failed per cycle, oldest pending age, sending-row age) and alert on: failed-rate > threshold, oldest pending age > N minutes, any row stuck in `sending` > 5 minutes, or 100% send failure over a window. Log at `warn`/`error` level, not `println!`.

### ME-77 — Notifications table retains full phone numbers and message bodies forever: no retention / purge

- **Severity:** Medium | **Area:** Notifications / Notify / Worker | **File:** `migrations/0029_create_notifications.sql`
- **Found by:** agent-10 (M8)
- **Impact:** a DB backup leak, a compromised read replica, or a broad `SELECT` by an over-privileged account exposes the entire history of who was contacted about which order and for how much. It also grows the table (and the admin count queries, which scan all rows) without bound. -
- **Evidence:** the table stores `phone text NOT NULL`, `body text NOT NULL`, and `payload jsonb` with no expiry column and no purge job anywhere in the worker (its jobs are outbox, shipment polling, subscription sweep — no cleanup job). The admin view masks phones (`notifications.rs:104`, `domain::mask_phone`), but the underlying rows — and any DB backup — keep every buyer's/seller's full number and every message indefinitely. -
- **Fix:** add a retention policy (e.g. delete or anonymise `sent`/`failed` rows older than 90 days, keeping only aggregate counts), run it as a periodic worker job, and consider storing only the masked phone once a row is `sent`. ---

### ME-78 — Row locks are taken in nondeterministic order: two checkouts can deadlock each other (500, no retry)

- **Severity:** Medium | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/orders.rs:664`
- **Found by:** agent-03 (M2); agent-20 (M4)
- **Impact:** Buyer A checks out cart [X, Y], buyer B checks out cart [Y, X] concurrently. A locks X, B locks Y, each waits on the other's row: Postgres detects the deadlock and aborts one transaction with `40P01`. That buyer gets a 500 "database error" for a checkout that would have succeeded on retry; a client that retries blindly then hits the confusing "cart is empty" 400 (see L1). …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/orders.rs:664–672` (product lock query), `crates/api/src/orders.rs:690–697` (variant lock query) - **Code evidence:** "SELECT ... FROM cart_items ci JOIN products p ... WHERE ci.cart_id = $1 FOR UPDATE OF p" // orders.rs:664-672 — no ORDER BY: lock order = whatever order the planner returns (cart insertion order in practice) "SELECT ... FROM product_variants WHERE id = ANY($1) FOR UPDATE" // orders.rs:693 — no ORDER BY either `ApiError::internal` maps any `sqlx::Error` — including Postgres `40P01 deadlock_detected` — to a ba
- **Fix:** Add deterministic lock ordering: `ORDER BY p.id` on the product lock query and `ORDER BY id` on the variant lock query (lock variants in the same sorted order). Optionally catch `40P01` in `place_order` and retry the transaction once.

### ME-79 — Concurrent cart adds lose units (read-then-upsert, last write wins)

- **Severity:** Medium | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/cart.rs:210`
- **Found by:** agent-03 (M5)
- **Impact:** Two tabs (or a double-tap / retry) add 5 units of the same product concurrently. Both read `existing = 0`, both compute `new_qty = 5`, both upsert 5. Final quantity is 5, not 10 — the buyer is silently short 5 units, and the seller loses the sale. The same shape applies to `add` racing `set_cart_item_quantity`. (The DB unique index `cart_items_cart_product_variant_key`, …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/cart.rs:210–246` (`add_cart_item`) - **Code evidence:** let existing: i32 = sqlx::query_scalar( "SELECT quantity FROM cart_items WHERE cart_id = $1 AND product_id = $2 ...") .fetch_optional(pool) // cart.rs:215 — plain read, no lock, no transaction let new_qty = validate_cart_quantity(i64::from(existing) + i64::from(add_qty))?; sqlx::query( "INSERT INTO cart_items (cart_id, product_id, variant_id, quantity) VALUES ($1, $2, $3, $4) ON CONFLICT (cart_id, product_id, variant_id) DO UPDATE SET quantity = EXCL
- **Fix:** Make the upsert atomic in one statement: `DO UPDATE SET quantity = LEAST(99, cart_items.quantity + EXCLUDED.quantity)` (validating the 1..=99 bound in SQL or re-checking the returned row), or wrap read + write in a transaction with the cart row locked `FOR UPDATE`.

### ME-80 — Seller PATCH `shipped → returned` restocks nothing (and refunds nothing)

- **Severity:** Medium | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/orders.rs:1815`
- **Found by:** agent-03 (M6)
- **Impact:** Seller marks a shipped parcel `returned` by hand (courier brought it back). The goods are physically back in the warehouse, but stock is never restored — the product stays decremented and can show out-of-stock / stop selling even though units are on hand. For a prepaid online order the buyer also gets no refund through this path (the RTO doc notes this is "a separate platform …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/orders.rs:1815–1825` (`update_sub_order_status`) - **Code evidence:** if new_status == "cancelled" && current_status != "cancelled" { restock_sub_order_items(&mut tx, sub_order_id, "cancel_restock").await?; // orders.rs:1819 } sqlx::query("UPDATE sub_orders SET status = $2 WHERE id = $1") // orders.rs:1822 // No restock branch for "returned", no loyalty clawback, no refund — while the same // transition via RTO sync DOES restock (shipments.rs:897-898) and via an approved buyer // return DOES restock (returns.rs:559) w
- **Fix:** In `update_sub_order_status`, when `new_status == "returned"`, call `restock_sub_order_items(&mut tx, sub_order_id, "return_restock")` and `loyalty::clawback_for_returned_sub_order` in the same transaction, exactly like the RTO/return paths — or remove `("shipped","returned")` from the seller PATCH table so `returned` is only ever reached through the restocking funnels. ---

### ME-81 — Stock decrement is unconditional (`stock - qty` with no `WHERE stock >= qty`); the only backstop is the DB CHECK

- **Severity:** Medium | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/orders.rs:1135`
- **Found by:** agent-03 (M1); agent-20 (L1)
- **Impact:** Today the app-level check is protected by the product/variant `FOR UPDATE` locks (verified: every other stock writer — `inventory::adjust_stock_for_seller`, `variants::adjust_variant_stock_for_seller` — also locks its row first), so this is defence-in-depth rather than a live oversell. But the decrement is a bare read-modify-write: any future writer, any missed lock, or any …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/orders.rs:1135–1165` (checkout decrement), `crates/api/src/orders.rs:730–767` (validation it relies on) - **Code evidence:** // Validation (under the FOR UPDATE lock — correct, but it is the ONLY guard): if variant.stock < item.quantity { return Err(ApiError::conflict("insufficient stock ...")); } // orders.rs:742 if item.stock < item.quantity { return Err(ApiError::conflict("insufficient stock ...")); } // orders.rs:758 // ... hundreds of lines later, the decrement itself has no guard: "UPDATE product_variants SET stock =
- **Fix:** Make the decrement atomic and conditional: `UPDATE products SET stock = stock - $2 WHERE id = $1 AND stock >= $2 RETURNING stock`, and treat zero affected rows as `409 insufficient stock`. Do the same for `product_variants`. Keep the app-level check for the friendly error message; let the SQL be the guarantee.

### ME-82 — Audit log is not a faithful record: `raw_body` is re-serialised (gateway fields dropped), the signature is stored in it, and `event_type` comes from unvalidated attacker input

- **Severity:** Medium | **Area:** Payments | **File:** `crates/api/src/payments.rs:568-574`
- **Found by:** agent-02 (?)
- **Impact:** (a) Any extra gateway field (gateway txn id, currency, payer, timestamp) is silently discarded before logging, so a disputed payment cannot be reconstructed or re-verified from `payment_events` — the log's stated purpose ("every callback … with its raw body", `0009:8-10`) is not met. (b) The HMAC `signature` itself is stored in plaintext in `payment_events.raw_body` — anyone …
- **Evidence:** // 568-574: the "raw" body is NOT the raw request — it is a re-serialisation of 4 parsed fields let raw_body = serde_json::json!({ "payment_ref": body.payment_ref, "amount_poisha": body.amount_poisha, "status": body.status, "signature": body.signature, }).to_string(); // 302-306: event_type is built from the raw, unvalidated callback_status BEFORE it is checked let event_type = if method_matches { format!("webhook_{callback_status}") } else { "webhook_method_mismatch".to_string() }; // ... inserted at 307-317 with signature_valid, before the status/amount checks at 327-340 -
- **Fix:** Capture the actual raw request bytes (Axum `Bytes` / `RawJson`) and store those; never store the `signature` field (store only its first 8 chars or a hash if needed for debugging); validate `callback_status ∈ {success, failed}` *before* inserting the event and use a fixed enum for `event_type` plus a separate `rejection_reason` column.

### ME-83 — Unauthenticated webhook: no rate limit, pre-auth DB write, and a 404/401/400 existence oracle

- **Severity:** Medium | **Area:** Payments | **File:** `crates/api/src/payments.rs:559-587`
- **Found by:** agent-02 (?)
- **Impact:** Anyone on the internet can (a) flood `payment_events` with one insert per POST for any known `gateway_ref` (unbounded DB growth / log-spam DoS — each request is a DB write before any authentication), (b) enumerate payment references by response code (404 = does not exist, 401/400 = exists), and (c) brute-force weak HMAC secrets online with no throttling or lockout. The DEV-ref …
- **Evidence:** The handler takes no `AuthUser`/API key; `apply_payment_callback` inserts a `payment_events` row (307-317) *before* the signature check (324-326); an unknown ref returns 404 (282), a known ref with a bad signature returns 401 (325), a known ref with a wrong method returns 400 (319-323). -
- **Fix:** Apply a strict per-IP rate limit + request-size limit to `/v1/payments/webhook/:method` (and ideally a gateway IP allowlist per gateway once live), do not insert an audit row until after the signature verdict for *unknown* refs (or rate-limit the insert path), and collapse the oracle: return the same generic 401 for unknown-ref, bad-signature and method-mismatch cases (log the distinction server-side only).

### ME-84 — Advance recognition is fragile and `advance_paid_poisha` has no DB invariant

- **Severity:** Medium | **Area:** Payments | **File:** `crates/api/src/payments.rs:386-403`
- **Found by:** agent-02 (?); agent-13 (M12)
- **Impact:** The "is this the advance?" decision is pure amount-equality against a mutable snapshot, duplicated in two places that can drift (callback vs retry). If a second advance payment row ever exists (see H-05 — nothing prevents it), the second settlement *overwrites* rather than accumulates `advance_paid_poisha`, so the courier's COD netting (`shipments.rs` computes the collectable …
- **Evidence:** // 396-401: overwrite, not increment — and the same rule is copy-pasted in retry at 476-479 sqlx::query("UPDATE orders SET payment_status = 'advance_paid', advance_paid_poisha = $2 WHERE id = $1") .bind(payment.order_id).bind(payment.amount_poisha) Migration 0019 has no `CHECK (advance_paid_poisha <= advance_required_poisha)`, no `CHECK (advance_paid_poisha = 0 OR advance_required_poisha > 0)`, and no constraint tying the two columns together. -
- **Fix:** Identify the advance by the payment row's identity (e.g. a `payments.is_advance` / `advance_for_order` marker set at checkout), not by amount equality; accumulate with `advance_paid_poisha = advance_paid_poisha + $2` guarded by `AND advance_paid_poisha + $2 <= advance_required_poisha`; deduplicate the recognition rule into one helper used by both callback and retry; add the DB checks `advance_paid_poisha <= advance_required_poisha` and `advance_paid_poisha > 0 ⇒ payment_method='cod'`.

### ME-85 — `payment_methods()` leaks live/dev configuration to unauthenticated callers, and the order view hands out the forge inputs

- **Severity:** Medium | **Area:** Payments | **File:** `crates/api/src/payments.rs:162-200`
- **Found by:** agent-02 (?)
- **Impact:** An unauthenticated `GET /v1/payments/methods` tells an attacker exactly which gateways have merchant credentials and whether the DEV simulator is on — i.e. whether C-01 is exploitable — before they spend any effort. The order view then gives the buyer (or anyone who can view the order) the `gateway_ref` and exact `amount_poisha`, which are two of the three HMAC inputs; with …
- **Evidence:** // 175-187: mode reveals per-gateway credential presence and dev-mode state let availability = if config.credentials_present(method) { mode: Some("live") } else if config.dev_mode { mode: Some("dev") } else { available: false, reason: Some("not_configured") }; // 121-125: order view exposes the ref and the dev path let dev_redirect_url = gateway_ref.as_deref() .filter(|r| is_dev_reference(r)).map(|r| format!("/dev-pay/{r}")); -
- **Fix:** Return only `available: bool` (+ `method`) from the public methods endpoint; keep `mode`/`reason` for an admin endpoint. Do not return `gateway_ref` in the buyer-facing summary — return only the `dev_redirect_url` (or a short opaque token) and keep the ref server-side. ---

### ME-86 — DEV signatures never expire and cover too little: no timestamp/nonce, no method/currency in the HMAC

- **Severity:** Medium | **Area:** Payments | **File:** `crates/payments/src/lib.rs:387-420`
- **Found by:** agent-02 (?); agent-12 (L5)
- **Impact:** A captured valid signature is valid forever and replayable (the only thing stopping a replay after a refund is the transition table rejecting `refunded → paid` — a defence that depends on H-04's refund path existing, which it does not). Because the method is not in the HMAC, a signature minted for one gateway is structurally valid for another if the ref ever collides (DEV refs …
- **Evidence:** The signed payload contains only ref, amount and status. There is no timestamp, expiry, nonce, gateway/method field, currency, or order id separate from the ref. -
- **Fix:** Add `timestamp` (reject if older than e.g. 5 minutes / skewed) and a per-payment nonce / gateway event id to the signed payload and to `payment_events` (unique), include `method` and `currency` in the HMAC input, and store signatures only as a truncated hash. For real gateways, follow each gateway's own replay-protection (txn id dedup) rather than inventing one.

### ME-87 — Zero (and, in the adapters, negative) amounts are accepted: a 0-poisha payment can be marked `paid`

- **Severity:** Medium | **Area:** Payments | **File:** `migrations/0009_create_payments.sql:22`
- **Found by:** agent-02 (?); agent-12 (L9)
- **Impact:** If checkout ever produces a 0 total (full coupon/loyalty redemption, pricing bug — the totals maths is out of scope, so this is conditional, not proven reachable today), checkout inserts a `pending` 0-poisha payment and a signed `success` callback for amount 0 marks the order `paid` for free goods. More generally the payments layer — the layer that should be the last guard on …
- **Evidence:** No `amount_poisha > 0` check exists in any scope file; the DB check is `>= 0`, not `> 0`. -
- **Fix:** Reject `amount_poisha <= 0` in `insert_pending_payment` and in every adapter's `initiate` (return a typed `PaymentError::InvalidAmount`), and tighten the DB check to `amount_poisha > 0` (with a data migration confirming no 0 rows exist). Zero-value orders, if genuinely wanted, should be a separate explicit free-order path, not a `paid` payment.

### ME-88 — Subscription renewal charges for already-elapsed periods with no benefits (late-sweep catch-up)

- **Severity:** Medium | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/subscriptions.rs:539-614`
- **Found by:** agent-04 (M1)
- **Impact:** Normally the sweep runs every 10 s so the gap is trivial — but if the worker is down, redeployed, or its DB is unreachable for >30 days, each sweep after recovery charges another full period for a window in which the store had *no* benefits (`live_plan` requires `status == 'active' AND expires_at > now`, `subscriptions.rs:210-217`). A seller whose balance refills from new …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/subscriptions.rs:539-614` (`settle_lapsed_subscription`); worker cadence `crates/worker/src/main.rs:78-97` (default 10 s) - **Code evidence:** // settle: extends FROM THE OLD EXPIRY, one period per sweep "UPDATE store_subscriptions SET expires_at = expires_at + make_interval(days => $1), ..." Module doc (`subscriptions.rs:46-55`): "a store more than one period behind catches up one period per sweep". - **Impact / exploit scenario:** Normally the sweep runs every 10 s so the gap is trivial — but if the worker is down, redeploye
- **Fix:** Renew from `GREATEST(expires_at, now())`, or expire (don't renew) when the lapse exceeds one period / a grace window and require the seller to re-subscribe. Cap catch-up to one charge per sweep run per store and notify the seller of renewal charges.

### ME-89 — Switching to `free` runs outside any transaction or lock and can race a paid subscribe into charged-but-cancelled

- **Severity:** Medium | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/subscriptions.rs:351-367`
- **Found by:** agent-04 (M2)
- **Impact:** A free-switch racing a paid `subscribe` (which holds the store row lock only for its own transaction, which this branch never takes) can interleave either way: free-switch lands after the paid upsert+charge commits → seller paid full price and is instantly cancelled with no refund; or the paid upsert lands after the free-switch → seller ends on a paid plan they just tried to …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/subscriptions.rs:351-367` (`subscribe`, `plan == PLAN_FREE` branch) - **Code evidence:** if plan == PLAN_FREE { sqlx::query("UPDATE store_subscriptions SET plan = 'free', status = 'cancelled', auto_renew = false, expires_at = now(), updated_at = now() WHERE store_id = $1").bind(store.id).execute(pool).await ...; // No tx, no store FOR UPDATE lock, no status filter — unlike the paid path below it. - **Impact / exploit scenario:** A free-switch racing a paid `subscribe` (which holds
- **Fix:** Run the free-switch inside the same transaction/lock discipline as the paid path: begin tx, lock the store row, then update the subscription row, then commit.

### ME-90 — Cancelling renewal can lose to the sweep: seller cancels, sweep still charges

- **Severity:** Medium | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/subscriptions.rs:452-490`
- **Found by:** agent-04 (M3)
- **Impact:** Between the load and the update, the worker's `settle_lapsed_subscription` can renew the plan (inserting a `seller_charges` row and extending `expires_at`) for a seller who just cancelled renewal — the seller is charged for another 30 days they explicitly declined. Conversely the update can flip `auto_renew = false` on a row the sweep already expired (harmless but shows the …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/subscriptions.rs:452-490` (`cancel_subscription`) - **Code evidence:** let row = load_subscription_on(pool, store.id).await? .filter(|row| row.status == "active" && row.plan != PLAN_FREE).ok_or_else(...)?; sqlx::query("UPDATE store_subscriptions SET auto_renew = false, updated_at = now() WHERE id = $1") .bind(row.id).execute(pool).await ...; // No tx, no lock, and the UPDATE has no status/auto_renew predicate. - **Impact / exploit scenario:** Between the load and the update, the worker's `settle_lapsed_subscription
- **Fix:** Do the cancel in a transaction under the store row lock (as `subscribe` does), or at least `UPDATE ... WHERE id = $1 AND status = 'active' AND auto_renew = true` and treat zero affected rows as "already changed, re-read".

### ME-91 — Payout account can be redirected to any number with no ownership verification, no re-auth, no audit trail

- **Severity:** Medium | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/payouts.rs:384-421`
- **Found by:** agent-04 (M4)
- **Impact:** Anyone with a hijacked seller session (or a seller's own typo) can point all future payouts at an attacker's bkash number in one unauthenticated `PUT`, then request a payout. Because the payout row snapshots the account at request time, the money goes to the attacker's wallet and the legitimate seller has no record of who changed it or when. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/payouts.rs:384-421` (`save_payout_account`); `crates/domain/src/lib.rs:1222-1256` (`validate_payout_account`) - **Code evidence:** validate_payout_account(method, account_name, account_number).map_err(ApiError::bad_request)?; // ... ON CONFLICT (store_id) DO UPDATE SET method/account_name/account_number ... `validate_payout_account` checks only *shape*: bkash/nagad must be a valid Bangladeshi mobile format, bank must be 6–24 alphanumerics. For bkash/nagad the `account_name` is not validated at all (empty name accepted). No OTP
- **Fix:** Verify ownership of a new payout account (OTP to the bkash/nagad number, or micro-deposit/bank verification) before it can receive payouts; require password/OTP re-authentication on change; write an audit-log row on every account save; optionally impose a cool-down (e.g. no payout for 24 h after an account change).

### ME-92 — Full payout account numbers are exposed to store managers (and in admin lists) with no masking

- **Severity:** Medium | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/payouts.rs:445-520`
- **Found by:** agent-04 (M5)
- **Impact:** A store manager (day-to-day staff, explicitly barred from `manage_payouts`) can read the owner's full bkash/nagad/bank account numbers from the payout history — PII/financial identifiers that enable social engineering or fraudulent transfers. Admin list (`admin.rs:590`) likewise returns full numbers where a masked form would suffice for a queue view. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/payouts.rs:445-520` (`list_payouts_for_store`, returns `Payout` with full `account_name`/`account_number`); `crates/api/src/payouts.rs:505-520` (scoped list under `view_finances`); `migrations/0012_create_payouts.sql:19-21` (snapshot columns) - **Code evidence:** `Payout` struct (`payouts.rs:133-148`) serializes `account_name` and `account_number` in full; `list_payouts_handler` grants the scoped list to any caller with `view_finances` — which managers hold (`store_role_can`, domain: manager may `view_finances`). The domain already has `m
- **Fix:** Mask account numbers in all seller/manager-facing responses (`mask_phone`-style, last few digits only) and return full numbers only on the specific admin payout-processing view that needs them.

### ME-93 — Loyalty balance nets all redemptions/adjustments against only the unexpired earn pool (no FIFO): fresh points are eaten by old spending, expired clawbacks eat fresh points

- **Severity:** Medium | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/loyalty.rs:75-103`
- **Found by:** agent-04 (M6)
- **Impact:** Buyer earns 100 points, redeems all 100, then earns 100 fresh points: balance = 100 (unexpired earn) + (−100) (old redeem) = 0 — the fresh points are silently consumed by a redemption that already spent the *old* lot. Symmetrically, a clawback `adjust` for an earn lot that has already expired still subtracts from the fresh pool (a returned order's clawback after the 365-day …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/loyalty.rs:75-103` (`loyalty_balance_on`) - **Code evidence:** SELECT COALESCE(SUM(delta) FILTER (WHERE reason = 'earn' AND expires_at > now()), 0) AS unexpired_earn, COALESCE(SUM(delta) FILTER (WHERE reason IN ('redeem', 'adjust')), 0) AS net_moves, ... FROM loyalty_points WHERE user_id = $1 -- available_points: (row.0 + row.1).max(0) The module doc (`loyalty.rs:10-19`) acknowledges this as a "deliberate simplification … no per-point FIFO tracking". - **Impact / exploit scenario:** Buyer earns 100 points, redeems a
- **Fix:** Track redemptions against specific earn lots (FIFO consumption with per-lot remaining balances), or at minimum expire `redeem`/`adjust` netting together with the earn lots they consumed (net only redemptions that post-date the oldest unexpired earn).

### ME-94 — A courier-returned delivered parcel never claws back loyalty points (or seller earnings, consistently)

- **Severity:** Medium | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/shipments.rs:880-920`
- **Found by:** agent-04 (M7)
- **Impact:** A parcel delivered (buyer earns points, seller earns payout) that the courier then returns (`delivered → return_in_transit → returned`) is silently ignored by the sync: the sub-order stays `delivered`, the buyer keeps the points, the seller keeps the earnings, even though the goods came back. The formal return flow (the only clawback path) is never triggered for RTO. A …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/shipments.rs:880-920` (`sync_sub_order_status`, called by the shipment refresh path); `crates/domain/src/lib.rs:391-401` (`can_transition_sub_order`) - **Code evidence:** // shipments.rs: the sync only maps delivered/returned, then: if !can_transition_sub_order(sub_status, target) { return Ok(()); } // can_transition_sub_order allows shipped -> returned, but NOT delivered -> returned. // The loyalty clawback lives only in returns::mark_return_received, and migration 0018 // states: "Courier-driven returns (RTO) deliberately c
- **Fix:** Allow `delivered → returned` in the shipment sync (or handle the shipment-`returned` case explicitly) and invoke `clawback_for_returned_sub_order` there, mirroring `mark_return_received`. Decide explicitly whether RTO also reverses seller earnings (see H1).

### ME-95 — Subscription price is read outside the transaction/lock, so a price change mid-purchase charges a stale price

- **Severity:** Medium | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/subscriptions.rs:370`
- **Found by:** agent-04 (M8)
- **Impact:** An admin lowers the Pro price (or a seller times a purchase against a known price change) between the read and the commit; the seller is charged the old price for a period bought at the new price, or vice versa. The charge row and the receipt then disagree with the price in force at commit time, and `available_after_poisha` is computed from the stale price. -
- **Evidence:** - **Severity:** Medium (low exploitability — requires an admin price change mid-request — but a real integrity gap) - **File:line:** `crates/api/src/subscriptions.rs:370` (`subscribe`); `crates/api/src/subscriptions.rs:544` (`settle_lapsed_subscription`) - **Code evidence:** // subscribe: price read BEFORE begin() and before the store FOR UPDATE lock let price = effective_subscription_price(state, plan).await; let mut tx = pool.begin().await ...; // settle_lapsed_subscription: same shape — price read before begin() let price = effective_subscription_price(state, &candidate.plan).a
- **Fix:** Read `effective_subscription_price` inside the transaction, after acquiring the store row lock (it reads `platform_settings`, so it can run on `&mut *tx`).

### ME-96 — Any store member (including `staff`) can read a store's earnings via the dashboard, bypassing the `view_finances` gate

- **Severity:** Medium | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/payouts.rs:562-580`
- **Found by:** agent-04 (M9); agent-18 (M1)
- **Impact:** A `staff` member — who by the role matrix (`store_role_can`) may only do `manage_catalog`, `manage_orders`, `answer_questions` and is explicitly not finance — can read the store's earned/paid-out/pending/available figures by calling `GET /v1/seller/dashboard?store_id=<id>`. Financial data leaks to the lowest-privilege role. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/payouts.rs:562-580` (`dashboard_handler`); contrast `crates/api/src/payouts.rs:582-601` (`balance_handler`) - **Code evidence:** // dashboard_handler: only seller_access (any role, incl. staff) — no action check Some(store_id) => { crate::access::seller_access(pool, user.user_id, store_id).await?; let store = crate::stores::get_store_by_id(pool, store_id).await?; Ok(Json(get_dashboard_for_store(pool, store).await?)) // includes Earnings } // balance_handler: requires STORE_ACTION_VIEW_FINANCES (owner or manage
- **Fix:** Gate the scoped dashboard on `view_finances` like `balance_handler` (or strip `earnings` from the dashboard response when the caller lacks `view_finances`). ---

### ME-97 — No price bounds: price `0` (free) and up to `i64::MAX` are accepted

- **Severity:** Medium | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/products.rs:422-437`
- **Found by:** agent-05 (M1); agent-19 (M4)
- **Impact:** A seller can list a product at 0 and have it checked out for free (delivery-only order, COD 0 — downstream totals/payment code may assume a positive goods total), or at `i64::MAX` poisha, where `domain::CartItem::line_total_poisha` (`unit_price_poisha * quantity`, `domain/src/lib.rs:57-59`, plain `i64` multiplication, no `checked_mul`) overflows — panic in debug / wrap in …
- **Evidence:** `validate_product_input` rejects only negative prices; there is no minimum (e.g. 1 poisha) and no maximum. `CreateProductInput.price_poisha: i64` (`products.rs:597`) is passed straight through. -
- **Fix:** Set a domain price range (e.g. `1..=MAX_PRODUCT_PRICE_POISHA`, pick a sane ceiling such as 100_000_000 poisha / 10 lakh taka) in `validate_product_input` and mirror it with a DB `CHECK`; use `checked_mul` / `checked_add` in money totals. Decide explicitly whether 0-price (freebie) products are a supported feature — if so, document and handle 0-total checkout, otherwise reject 0.

### ME-98 — `compare_at_poisha` is unchecked against the real price: fake strikethrough discounts

- **Severity:** Medium | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/products.rs:438-442`
- **Found by:** agent-05 (M2); agent-17 (?); agent-19 (L5)
- **Impact:** Seller sets `price_poisha = 100` (৳1) with `compare_at_poisha = 1_000_000_000` and the UI renders a near-100% "discount"; or `compare_at < price` (a negative discount, confusing/misleading). There is no rule that the "was" price was ever the selling price. -
- **Evidence:** if let Some(compare_at) = input.compare_at_poisha { if compare_at < 0 { return Err(ApiError::bad_request("compare_at_poisha must be >= 0")); } } // no check that compare_at > price_poisha, or that it is close to it DB check is likewise only `>= 0` (`migrations/0004:13`). -
- **Fix:** Require `compare_at_poisha > price_poisha` when set (and optionally cap the discount ratio, e.g. compare_at ≤ 10× price, and require the product to have actually sold at that price before allowing it — at minimum the `>` rule). Reject in both create and update, and in the Facebook import path (which currently sets `compare_at_poisha: None`, so it is safe today).

### ME-99 — Variant final-price guard is check-then-act with no lock: a base-price PATCH racing a variant create/update can strand a variant at ≤ 0 (or overflow)

- **Severity:** Medium | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/variants.rs:289-312`
- **Found by:** agent-05 (M3)
- **Impact:** Concurrent `PATCH /v1/products/:id` (lower base) + `POST …/variants` (negative delta) can both pass their checks and commit a variant whose `base + delta <= 0`. The failure is then silent: `load_public_variants` (`variants.rs:203-221`) drops such variants via `filter_map` (buyer sees a product whose effective stock includes the hidden variant — see H4/M11 — but cannot buy it), …
- **Evidence:** // variants.rs:296,304 — validation reads a stale base price, insert is a separate statement require_positive_final_price(product.price_poisha, delta)?; let row: VariantRow = sqlx::query_as("INSERT INTO product_variants (…) VALUES ($1,…) …") // products.rs:542-549 — the mirror check is also a bare SELECT COUNT(*) before the UPDATE "SELECT COUNT(*) FROM product_variants WHERE product_id = $1 AND is_active AND $2 + price_delta_poisha <= 0" Neither side locks `products` (`SELECT … FOR UPDATE`) or serialises on the product row. -
- **Fix:** In both variant write paths and the product price PATCH, lock the product row first (`SELECT … FROM products WHERE id=$1 FOR UPDATE` inside a transaction) and re-check the final price under the lock; or add a DB-level guard (a trigger / generated check that cannot be expressed as a plain `CHECK` across tables — the lock is the practical fix).

### ME-100 — Product-level adjust-stock race: a variant created concurrently makes the adjustment write to a dead pool

- **Severity:** Medium | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/inventory.rs:265-340`
- **Found by:** agent-05 (M4)
- **Impact:** `adjust-stock` checks "no active variants" → passes → concurrent variant create commits → `adjust-stock` still `UPDATE`s `products.stock`, which nothing now reads (effective stock is the variant sum). The adjustment is audited (`stock_adjustments`, `variant_id: None`) against the wrong pool, the seller believes stock moved, buyers see no change, and the audit trail cannot be …
- **Evidence:** // inventory.rs:290-301 — product row IS locked, but the variant-existence check is a bare EXISTS "SELECT p.store_id, s.owner_user_id, p.stock FROM products p … WHERE p.id = $1 FOR UPDATE OF p" let has_active_variants: bool = sqlx::query_scalar( "SELECT EXISTS (SELECT 1 FROM product_variants WHERE product_id = $1 AND is_active)") /* … */; if has_active_variants { return Err(/* adjust each variant instead */); } // … then: UPDATE products SET stock = $2 … // variants.rs create: plain INSERT into product_variants, no lock on products -
- **Fix:** Lock the variant set as well in `adjust_stock_for_seller` (e.g. `SELECT … FROM product_variants WHERE product_id=$1 FOR UPDATE` / `LOCK TABLE product_variants IN SHARE ROW EXCLUSIVE MODE` for the product, or take a `FOR UPDATE` lock on the product in `create_variant_for_seller` too so the two writers serialise on the same row), and re-check `has_active_variants` after acquiring the lock, immediately before the `UPDATE`.

### ME-101 — Search suggestions accept an unbounded `q`: expensive trigram work on arbitrary input

- **Severity:** Medium | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/search.rs:198-215`
- **Found by:** agent-05 (M5)
- **Impact:** An unauthenticated caller can send a multi-KB/MB `q` to `GET /v1/search/suggest` on every keystroke; each request runs `ILIKE` + `similarity(p.title || ' ' || title_bn, $2)` (pg_trgm) over the catalogue. Repeated calls are a cheap asymmetric DoS on Postgres. The main search endpoint caps `q` at 100 chars, so this is an inconsistent bypass of that cap. -
- **Evidence:** // search.rs:290-292 — no validation at all pub async fn suggest_handler(State(state): State<AppState>, Query(query): Query<SuggestQuery>) -> Result<Json<SuggestResults>, ApiError> { let results = search_suggestions(pool, query.q.as_deref().unwrap_or("")).await?; // search.rs:198-206 — only a minimum length is enforced let trimmed = q.trim(); if trimmed.chars().count() < MIN_SUGGEST_CHARS { return Ok(/* empty */); } let pattern = search_like_pattern(trimmed); // then ILIKE + similarity() over titles `SuggestQuery.q: Option<String>` (`search.rs:282`) has no length bound; the handler never calls `validate_search_params`. -
- **Fix:** Validate `q` in `suggest_handler` with the same 100-char rule (reuse `validate_search_params` or a shared `validate_suggest_q`), returning empty lists / 400 above the cap.

### ME-102 — Unbounded variants per product, no name/SKU uniqueness

- **Severity:** Medium | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/variants.rs:289-316`
- **Found by:** agent-05 (M7); agent-13 (M7)
- **Impact:** A seller (or a compromised/low-privilege staff member with `manage_catalog`) can create thousands of variants on one product — bloating the product-detail payload (`load_public_variants` returns all active variants, no limit), slowing `PRODUCT_COLUMNS` (one correlated subquery per variant set) and checkout locking. Duplicate names make the buyer picker ambiguous (two "XL" …
- **Evidence:** `normalize_sku` (`variants.rs:252-266`) only trims and caps at 80 chars; `validate_variant_name` caps at 80 chars. Nothing checks for an existing variant with the same name or SKU, and nothing caps the number of variants on a product. -
- **Fix:** Add a per-product variant cap (e.g. 50–100), a `UNIQUE(product_id, lower(name))` and a `UNIQUE(product_id, sku) WHERE sku IS NOT NULL` constraint (with a migration + an application-level pre-check returning 400), and paginate/limit the public variant list.

### ME-103 — No store/product-status gate on seller writes: suspended / archived products keep being edited, stocked and repriced

- **Severity:** Medium | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/products.rs:334-350`
- **Found by:** agent-05 (M8)
- **Impact:** A suspended store's seller can keep creating/activating products and variants and inflating stock, so the moment a suspension is lifted (or if a status check is ever missed downstream) a backlog of unreviewed catalogue changes goes live at once; an archived product can be silently re-armed. It also weakens suspension as an enforcement tool. -
- **Evidence:** // products.rs:343-349 let _store = get_store_by_id(pool, store_id).await?; match crate::access::store_role(pool, user.user_id, store_id).await? { … } // products.rs:702-716 — the READ side does gate on suspension; the write side never does if !is_owner && (product.status != "active" || store.status == "suspended") { return Err(ApiError::not_found("product not found")); } Read paths hide suspended stores' products from everyone except the team; write paths accept creates, status flips (`draft → active`), variant changes and stock moves on them. -
- **Fix:** In `require_store_catalog_access` / `owned_product` / the inventory and variant service functions, reject writes when `store.status != "active"` (403 with a bilingual message), and reject activating a product (`status → active`) or creating variants on a non-`active` product unless that is an explicitly supported draft workflow — document whichever is chosen.

### ME-104 — Authenticated (viewer-dependent) catalogue responses carry no `Cache-Control: private` / `Vary: Authorization`

- **Severity:** Medium | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/products.rs:52-73`
- **Found by:** agent-05 (M9)
- **Impact:** A shared cache / CDN in front of the API that applies heuristic caching (no `Cache-Control` does not mean "do not cache" for all intermediaries) can store an owner's draft-containing listing/detail response under the bare URL and serve it to anonymous buyers, disclosing draft titles, prices and stock. The code's own comment ("an authenticated response must never land in a …
- **Evidence:** // products.rs:60-68 — the only cache logic is "anonymous → public, max-age=30" pub fn public_cache_headers(anonymous: bool) -> HeaderMap { let mut headers = HeaderMap::new(); if anonymous { headers.insert(CACHE_CONTROL, /* "public, max-age=30" */); } headers // authenticated: NO header at all — neither `private` nor `no-store`, no Vary } // products.rs:662-670, 724 — the authenticated response is returned with those (empty) headers Ok((public_cache_headers(viewer.is_none()), Json(list))) The payload is viewer-dependent (an owner/member also sees their own drafts/archived products — `PRODUCT_WHERE`, `products.rs:213-216`; `get_product` draft gate, `products.rs:702-716`). -
- **Fix:** For any authenticated response return `Cache-Control: private, no-store` (or at least `private`) and add `Vary: Authorization` (and `Vary: Cookie` if cookies are ever used); for anonymous responses keep `public, max-age=30` plus `Vary: Authorization` so the two variants are never conflated by key.

### ME-105 — Phantom stock: unaudited creation stock up to `i32::MAX`, plus unlimited ±100k manual adjustments, with no total-stock ceiling

- **Severity:** Medium | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/products.rs:422-485`
- **Found by:** agent-05 (M11)
- **Impact:** A seller lists stock they do not hold, accepts COD orders against phantom availability (checkout only checks the stored number, `orders.rs:750-770`), then fails to fulfil — buyers pay / wait, the platform absorbs cancellations, returns and reputation damage. This is the seller-side mirror of the buyer-side oversell the locking in `orders.rs` is designed to prevent. -
- **Evidence:** Creation stock is the audit trail's starting point by design ("the history starts there"), so a product/variant can be born with stock `2,147,483,647` and no `stock_adjustments` row ever explains it; a seller can then add 100,000 per adjust call without limit. -
- **Fix:** Cap creation stock and total on-hand stock per product/variant at a sane ceiling (e.g. 100,000, matching the single-adjustment cap, or a per-store configurable limit), require an audit row even for creation stock above a threshold (or always — a creation row with reason `initial_stock`), and alert on large jumps (e.g. any adjustment taking stock above the ceiling needs a second confirmation / admin flag). ---

### ME-106 — Seller inventory overview has no pagination/limit: one unbounded response for the whole catalogue

- **Severity:** Medium | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/inventory.rs:454-494`
- **Found by:** agent-05 (M10)
- **Impact:** A business-plan store (cap `i64::MAX`, `domain::plan_product_cap`) — or any store that grew via the H1 import bypass — forces the API to load, serialise and return its entire catalogue (with a correlated effective-stock subquery per row) in one response on every dashboard load. Combined with H1 this is reachable on a free plan too. -
- **Evidence:** // inventory.rs:460-472 — every product of every accessible store, no LIMIT/OFFSET let rows: Vec<InventoryRow> = sqlx::query_as(&format!( "SELECT p.id AS product_id, p.title, s.slug AS store_slug, \ {EFFECTIVE_STOCK} AS stock, p.low_stock_threshold \ FROM products p JOIN stores s ON s.id = p.store_id \ WHERE p.store_id = ANY($1) ORDER BY stock ASC, p.title ASC", …)) Contrast `list_stock_history` (`inventory.rs:378-400`, clamped pagination) and `low_stock_items_for_seller` (`inventory.rs:515-530`, `LIMIT 10`). -
- **Fix:** Paginate `get_inventory` like the other list endpoints (`clamp_pagination`, `LIMIT/OFFSET`, `total` count), defaulting to a small page (e.g. 50), and keep the summary counts as a separate cheap aggregate.

### ME-107 — `create_return_request` checks `delivered` outside any transaction — a return can be opened for a sub-order that has already been returned via the courier path

- **Severity:** Medium | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/returns.rs:284-360`
- **Found by:** agent-09 (M1)
- **Impact:** Sub-order is `delivered`; buyer files a return; before the insert lands, the parcel is returned via the shipment path (sub-order `returned`, items restocked). The request is still inserted, can be approved, and `received` restocks a second time and opens a refund (see H1 — this is its entry point). -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/returns.rs:284-360` - **Code evidence:** the sub-order is loaded and `can_request_return(&sub_status)` is checked on the pool with no `FOR UPDATE` and no transaction; the `INSERT` happens in a separate statement afterwards. The UNIQUE constraint on `sub_order_id` only stops a *second return request*, not a return request racing an RTO/manual return. - **Impact / exploit scenario:** Sub-order is `delivered`; buyer files a return; before the insert lands, the parcel is returned via the shipment path (sub-order `returned`, items re
- **Fix:** `SELECT … FROM sub_orders WHERE id=$1 FOR UPDATE` inside a transaction covering the status check and the insert, and re-check the status in the insert path; also reject creation when the sub-order is already `returned`.

### ME-108 — No return window: a return can be requested arbitrarily long after delivery

- **Severity:** Medium | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/returns.rs:284-360`
- **Found by:** agent-09 (M2)
- **Impact:** A buyer can demand a return months after delivery for used/worn goods; the seller has no policy basis in code to refuse (only a manual reject), and an approved late return still creates a full `goods − coupon` refund obligation plus restock of now-unsellable stock. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/returns.rs:284-360` and `crates/domain/src/lib.rs:2331-2335` - **Code evidence:** `pub fn can_request_return(sub_order_status: &str) -> bool { sub_order_status == "delivered" }` — the only eligibility rule is the status string. No `delivered_at` timestamp is read, no day-limit (e.g. 7 days) exists anywhere in `returns.rs`. - **Impact / exploit scenario:** A buyer can demand a return months after delivery for used/worn goods; the seller has no policy basis in code to refuse (only a manual reject), and an approved late return stil
- **Fix:** Record `delivered_at` (or reuse the delivered shipment-event timestamp) and enforce a return window in `can_request_return` / `create_return_request` (e.g. 7 days, configurable), returning a 409 outside it.

### ME-109 — Store `staff` can approve/reject returns and mark goods received, creating a refund obligation — financial decisions need no owner/manager

- **Severity:** Medium | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/returns.rs:429-499`
- **Found by:** agent-09 (M3)
- **Impact:** The lowest-privilege team role can approve any return and mark it received, which snapshots a `pending` refund the platform then owes the buyer — a money-creating action with no owner approval, no amount threshold, and no audit row (see M4). A compromised or malicious staff account can manufacture refund obligations across the store's delivered orders. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/returns.rs:429-499` (`decide_return_request`) and `:501-590` (`mark_return_received`); `crates/domain/src/lib.rs:1153-1157` - **Code evidence:** both functions gate on `require_role(&role, domain::STORE_ACTION_MANAGE_ORDERS)`, and `store_role_can` grants `MANAGE_ORDERS` to `"staff"` as well as `"manager"`/`"owner"`. - **Impact / exploit scenario:** The lowest-privilege team role can approve any return and mark it received, which snapshots a `pending` refund the platform then owes the buyer — a money-creating action with no owner
- **Fix:** Require a distinct action (e.g. `STORE_ACTION_MANAGE_REFUNDS`, owner/manager only) for approve/received, or at least require owner/manager for `received`; keep `staff` at most able to view / reject with a note. Log the actor on every decision.

### ME-110 — Public tracking is a single low-entropy phone factor with no rate limiting, no expiry, and it exposes the order total

- **Severity:** Medium | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/tracking.rs:117-216`
- **Found by:** agent-09 (M5)
- **Impact:** Anyone who learns or guesses an `order_id` (UUIDs leak via shared links, screenshots, support threads) can brute-force the delivery phone with unlimited unauthenticated requests and then read the full order: items, store slugs, per-sub-order status, courier + consignment id, full event history, payment method/status and `grand_total_poisha`. The link also never expires, so a …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/tracking.rs:117-216` (`track_order`), `:218-232` (`track_handler`); `crates/api/src/lib.rs:1563-1566` - **Code evidence:** // GET /v1/track — "deliberately unauthenticated" (lib.rs:1563-1566); the only gate: if !phones_match_for_tracking(&delivery_phone, supplied_phone) { return Err(tracking_not_found()); } // … and the response includes: pub grand_total_poisha: i64, // tracking.rs:95, :203 No rate limit, CAPTCHA, attempt counter or lockout exists on this route (the only rate limiter in the codebase, `crates/api/src/
- **Fix:** Rate-limit `/v1/track` per IP and per `order_id` (e.g. 10 attempts / hour, then temporary block), add a short-lived signed token option for shared links, stop returning `grand_total_poisha` (or return it only to the signed-in buyer), and consider a constant-time comparison for the phone check.

### ME-111 — A returned sub-order's invoice still shows the full COD amount as due — no credit note, void, or refund link

- **Severity:** Medium | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/invoices.rs:199-255`
- **Found by:** agent-09 (M7)
- **Impact:** After goods are returned and a refund is `pending` or `done`, the invoice still presents `cod_amount_due_poisha` = the full slice price as payable cash. The document can be (mis)used to demand COD payment a second time, to claim the sale is still outstanding in a dispute, or to double-count revenue — and a buyer checking the invoice cannot tell the sale was reversed. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/invoices.rs:199-255` (`get_or_issue_invoice`) and `:355-410` (`build_invoice_view`) - **Code evidence:** // `returned` counts as invoicable, with no adjustment at all: if ctx.status != "delivered" && ctx.status != "returned" { /* 409 */ } // … let cod_amount_due_poisha = if order.payment_method == "cod" { (grand_total_poisha - advance_share_poisha).max(0) } else { 0 }; // Invoice struct has no refund_poisha / refund_status / voided field. - **Impact / exploit scenario:** After goods are returned and a refund is `pe
- **Fix:** When the sub-order is `returned`, set `cod_amount_due_poisha` to 0 (or show it as a negative/credit line), include the linked return's `refund_poisha` / `refund_status` on the invoice view, and label the document as reversed/credited (or issue a separate credit-note document).

### ME-112 — Manual tracking (receipt) number can be overwritten at any time, to any value, with no audit or uniqueness check

- **Severity:** Medium | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/shipments.rs:1408-1432`
- **Found by:** agent-09 (M8)
- **Impact:** A seller can change the receipt number after delivery (rewriting the verifiable reference the module doc calls *"the booking's audit reference"*), point two shipments at the same receipt number (consignment id is only indexed, `migrations/0010`, not unique), or substitute a fake number to defeat a buyer/support check at the courier counter. The public tracking view then shows …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/shipments.rs:1408-1432` (`set_manual_tracking_id`) - **Code evidence:** sqlx::query( "UPDATE shipments SET manual_tracking_id = $2, consignment_id = $2, \ updated_at = now() WHERE id = $1",) // no status predicate, no audit, no uniqueness The handler (`shipments.rs:1454-1466`) validates only shape (`validate_manual_tracking_id`: 4–64 chars, no whitespace) — not ownership of the number, not whether the shipment is already delivered/returned, and not whether another shipment already uses it. - **Impact / explo
- **Fix:** Allow the receipt number to be set only before the shipment leaves `created`/`picked_up` (or require an explicit "correction" flow with a reason that preserves the old value in `shipment_events`); enforce uniqueness of `manual_tracking_id` per courier; audit every change with actor + old/new value.

### ME-113 — Poller retries failed shipments forever with no backoff — a courier outage turns into a self-inflicted hammering loop

- **Severity:** Medium | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/shipments.rs:1125-1165`
- **Found by:** agent-09 (M9)
- **Impact:** If a courier API is down, credentials are revoked, or a consignment id is permanently invalid, every poll cycle re-hits the courier for every stale shipment (up to `limit` per run, worker loop in `crates/worker/src/main.rs:224`). That burns courier quota, fills logs, and can get the platform's merchant credentials rate-limited/banned by the courier — turning a transient outage …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/shipments.rs:1125-1165` (`refresh_stale_shipments`) - **Code evidence:** // "A shipment that errors … is counted `failed` and logged; it never stops the batch, // and because a failed refresh writes nothing its `updated_at` stays stale, so // the next run retries it." (shipments.rs:1120-1124, code at :1133-1150) Err(error) => { report.failed += 1; eprintln!("shipment poll: … failed: {}", error.message); } - **Impact / exploit scenario:** If a courier API is down, credentials are revoked, or a consignment id is permanent
- **Fix:** On failure, bump a `last_poll_attempt_at` / `poll_failures` counter and apply exponential backoff (skip shipments failing repeatedly, alert after N consecutive failures); never retry a permanently-invalid consignment id without human review. ---

### ME-114 — No audit trail for seller return decisions, manual shipment moves, or manual tracking-id changes (only `refund_done` is audited)

- **Severity:** Medium | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/returns.rs:429-590`
- **Found by:** agent-09 (M4)
- **Impact:** A fraudulent approve/receive or a fabricated manual delivery cannot be attributed to a person after the fact; disputes ("who marked this delivered / who approved this refund?") are unanswerable, and H3/H1 abuse is undetectable in the audit log. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/returns.rs:429-590` (no `record_admin_action` / audit call), `crates/api/src/shipments.rs:1343-1466` - **Code evidence:** `mark_refund_done` is the only function in scope that calls `record_admin_action` (`returns.rs:677-686`, action `"return.refund_done"`). `decide_return_request`, `mark_return_received`, `set_manual_shipment_status` and `set_manual_tracking_id` write financially significant state (refund `pending`, sub-order `returned`, restock, fake delivery) with only a `shipment_events` row or a notification as a side rec
- **Fix:** Write an audit row (actor user id, action, entity id, from→to, amount where applicable) in the same transaction for every return decision, receipt, manual status move and manual tracking-id change — mirroring `record_admin_action`.

### ME-115 — Reviews can be created and listed on non-public (draft / archived / suspended-store) products

- **Severity:** Medium | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/reviews.rs:236`
- **Found by:** agent-08 (?)
- **Impact:** A buyer who received a product before it was archived or its store suspended can still post (and anyone can still read) reviews on a product the marketplace has deliberately hidden, leaking that the product/store exists and keeping its rating surface alive. Inconsistent with the stated visibility rule used by the neighbouring modules. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/reviews.rs:236` and `reviews.rs:273` - **Code evidence:** // reviews.rs:236 (create_review) and :273 (list_product_reviews): get_product_by_id(pool, product_id).await?; `get_product_by_id` (products.rs:292-304) returns any product regardless of status/store status. Q&A and wishlist use the stricter `get_public_product` (products.rs:313-323: product `active` AND store `active`, else 404) — qna.rs:260, wishlist.rs:76. - **Impact / exploit:** A buyer who received a product before it was archived or its store suspended can
- **Fix:** Use `get_public_product` in both `create_review` and `list_product_reviews` (or document a deliberate exception for already-delivered buyers, and still hide the list for non-public products).

### ME-116 — Review edits are unlimited, survive return/refund, and orphan the seller's reply

- **Severity:** Medium | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/reviews.rs:305-350`
- **Found by:** agent-08 (?)
- **Impact:** (a) Buy-then-return-then-1-star: refunded buyer keeps a live, editable review. (b) Rating-flip extortion/manipulation: flip 5★→1★ after the seller replies, or repeatedly flip the rating to game the public average. (c) Bait-and-switch: write a glowing review, get a seller reply, then rewrite the body to something entirely different — the reply (reviews.rs:392-420) stays …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/reviews.rs:305-350` (`update_own_review`) - **Code evidence:** // reviews.rs:305-348 — ownership is checked, nothing else is: let (buyer_id, old_rating, old_body) = existing.ok_or_else(...)?; if buyer_id != user.user_id { return Err(ApiError::not_found("review not found")); } let rating = patch.rating.unwrap_or(i64::from(old_rating)); // ... no re-check of find_delivered_order_for_review, no edit count, no window: sqlx::query("UPDATE reviews SET rating = $2, body = $3, updated_at = now() WHERE id = $1") Eligibility is
- **Fix:** Re-check delivered eligibility on update (or freeze the review when its order is returned/refunded); cap edits (e.g. one edit, or edits within N days); keep an edit-history/audit row; on edit, flag or detach the seller reply as "reply to an earlier version" (or notify the seller).

### ME-117 — Wishlist list endpoints are unbounded: no pagination, no limit, no size cap

- **Severity:** Medium | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/wishlist.rs:111-144`
- **Found by:** agent-08 (?)
- **Impact:** A bot account adds thousands of products (add is a single cheap idempotent INSERT with no cap) and then `GET /v1/wishlist` returns all of them as full `Product` views in one response — server memory, serialisation time, and bandwidth blow up; a handful of such accounts is a cheap DoS. The ids endpoint has the same shape. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/wishlist.rs:111-144` (`list_wishlist`, `list_wishlist_ids`); `crates/api/src/wishlist.rs:67-91` (`add_to_wishlist`) - **Code evidence:** // wishlist.rs:111-128 — no page/limit parameters at all: pub async fn list_wishlist(pool: &PgPool, user_id: Uuid) -> Result<WishlistList, ApiError> { let items: Vec<Product> = sqlx::query_as(&format!( "SELECT {PRODUCT_COLUMNS} FROM wishlist_items w JOIN products p ... WHERE w.user_id = $1 ORDER BY w.created_at DESC")) .bind(user_id).fetch_all(pool).await ... //
- **Fix:** Add `page`/`limit` (reuse `clamp_pagination`, cap 100) to both list endpoints, and cap wishlist size per user (e.g. 500–1000 items, 400/409 beyond it).

### ME-118 — Wishlist list deliberately returns hidden/suspended products' full details (visibility bypass)

- **Severity:** Medium | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/wishlist.rs:111-128`
- **Found by:** agent-08 (?)
- **Impact:** A user who wishlisted a product before its store was suspended (or the product archived/draft) keeps seeing its live title, price, stock, and description through `GET /v1/wishlist` — including price changes made while hidden. The add path's 404 rule (wishlist.rs:76, `get_public_product`) is thus only a gate at add time, not a visibility guarantee. An attacker can also use it …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/wishlist.rs:111-128` (`list_wishlist`); module doc `crates/api/src/wishlist.rs:24-30` - **Code evidence:** // wishlist.rs:24-30 (documented as deliberate): // "It does not re-filter by current visibility: the list is the user's own // record of what they saved, and the product page itself enforces // visibility when they click through." // wishlist.rs:112-117 — joins wishlist_items -> products -> stores with no // status filter, returning full PRODUCT_COLUMNS (title, price, stock, images). - **Impact / exploit:** A u
- **Fix:** Either re-filter the list by current public visibility (marking hidden items as unavailable rather than returning their details), or at minimum return only `product_id` + a `still_visible: bool` for hidden rows instead of the full `Product` view.

### ME-119 — Q&A: asker can never edit or delete their own question, so leaked PII cannot be retracted

- **Severity:** Medium | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/qna.rs`
- **Found by:** agent-08 (?)
- **Impact:** A user who pastes their phone number, address, or order details into a question (easy — the body is free text, 3–500 chars, qna.rs:221) has no self-service way to remove it. They must hope the seller notices and hides it; the seller may be the party they are complaining about. This is a privacy/retraction gap, and the opposite of the review module, where the author *can* …
- **Evidence:** - **Severity:** Medium - **File:line:** Absence in `crates/api/src/qna.rs` and route table `crates/api/src/lib.rs:1366-1396` (routes: ask, list, seller list, answer, hide/unhide, admin hide/unhide — no PATCH/DELETE for the asker) - **Code evidence:** `ask_question` (qna.rs:221-252) inserts the row; no `update_own_question` / `delete_own_question` function exists, and no route exposes one. The only removal is seller/admin `is_hidden` (qna.rs:358). - **Impact / exploit:** A user who pastes their phone number, address, or order details into a question (easy — the body is free text, 3–50
- **Fix:** Add `PATCH /v1/questions/:id` and `DELETE /v1/questions/:id` for the asker only (404 for others, same no-leak rule), ideally with a short edit window; hiding by the asker should also be possible.

### ME-120 — Q&A self-seeding block checks only the store *owner*: staff/managers can astroturf their own store

- **Severity:** Medium | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/qna.rs:221-235`
- **Found by:** agent-08 (?); agent-18 (L1)
- **Impact:** A store stages a fake Q&A on its own product page: sockpuppet questions from a staff account, glowing self-answers from the owner account. Readers cannot distinguish these from genuine buyers, because Q&A shows only a masked phone and no verified-purchase marker. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/qna.rs:221-235` (`ask_question`) - **Code evidence:** // qna.rs:228-234 — only the owner is blocked: let product = get_public_product(pool, product_id).await?; let store = get_store_by_id(pool, product.store_id).await?; if store.owner_user_id == user.user_id { return Err(ApiError::bad_request("you cannot ask a question about your own product")); } The check does not consult `access::store_role` (used correctly for answering at qna.rs:301-327). A `manager`/`staff` member of the store — who can also *answer* ques
- **Fix:** Replace the owner check with `store_role(pool, user.user_id, store_id)`: reject the ask if the caller has *any* role in the store (owner/manager/staff).

### ME-121 — Conversation list is unbounded, with per-row subqueries (DoS / slow inbox)

- **Severity:** Medium | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/messaging.rs:401-419`
- **Found by:** agent-08 (?)
- **Impact:** A heavily-messaged seller (or a victim of conversation-creation spam) gets an ever-slower, ever-larger `GET /v1/conversations` response; each row costs three subquery scans. Same unbounded-list shape as M-03. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/messaging.rs:401-419` (`list_conversations_for_user`) - **Code evidence:** // messaging.rs:401-419 — no page/limit, fetch_all: pub async fn list_conversations_for_user(pool: &PgPool, user: &AuthUser) -> Result<Vec<ConversationView>, ApiError> { let rows: Vec<ConversationRow> = sqlx::query_as(&format!( "{CONVERSATION_SELECT} WHERE c.buyer_id = $1 OR s.owner_user_id = $1 ORDER BY c.last_message_at DESC, c.id DESC")) .bind(user.user_id).fetch_all(pool).await ...; `CONVERSATION_SELECT` (messa
- **Fix:** Paginate (`page`/`limit` via `clamp_pagination`, cursor on `last_message_at`), and compute last-message/unread in a single lateral join / windowed aggregate instead of per-row subqueries.

### ME-122 — Stored user content is never sanitised: reviews, Q&A, and messages can carry scripts, phishing links, and off-platform payment lures

- **Severity:** Medium | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/reviews.rs:225-260`
- **Found by:** agent-08 (?)
- **Impact:** (a) Stored XSS if any client renders bodies with `innerHTML` / `dangerouslySetInnerHTML` — `<script>`, `<img onerror>`, `javascript:` links in a review/Q&A/message execute for every viewer. (b) Phishing / fraud without any script: a review, answer, or chat message can contain `bKash 01XXXXXXXXX — pay 500tk outside the app to confirm your order`, fake support links, or a …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/reviews.rs:225-260, 305-350, 392-445`; `crates/api/src/qna.rs:221-252, 329-356`; `crates/api/src/messaging.rs:506-534, 620-720`; validators `crates/domain/src/lib.rs:1001-1076, 1686-1700` (length only) - **Code evidence:** Every validator in scope checks only length / emptiness (e.g. `validate_review_body`: "at most 1000 characters"; `validate_message_body`: "1 to 2000 characters"). A grep for `sanitiz|escape|html|strip|link|url|phone` over the four files and the validators returns nothing except SQL `LIKE` escaping (unrelated
- **Fix:** At the API boundary: strip/escape HTML (allowlist, never raw `<script>`/event handlers), reject or link-wrap URLs in public content (reviews/Q&A) and flag phone numbers / payment keywords (`bKash`, `Nagad`, `pay outside`) in messages for review; enforce the same escaping on the frontend (React text rendering, never `innerHTML` for these fields) and add a report/spam path (H-06).

### ME-123 — API keys (not just login sessions) can write reviews, Q&A, wishlist, and messages as the user

- **Severity:** Medium | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/api_keys.rs:140-159`
- **Found by:** agent-08 (?)
- **Impact:** A leaked `bdm_live_...` key with write scope can post fake 5-star reviews, plant Q&A, spam every store's inbox, and toggle SIDRA auto-reply as the victim — all without the victim's login session, and indistinguishable from the user in the data (no `api_key` marker is stored on reviews/messages). Key leakage (committed to git, pasted in chat, logged) is the realistic path. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/api_keys.rs:140-159` (`api_key_forbidden_route`); `crates/api/src/auth.rs:209-248` (`AuthUser` extractor routes keys to handlers) - **Code evidence:** // api_keys.rs:140-159 — the JWT-only list is: /v1/admin/*, /v1/api-keys*, // /v1/seller/payout-account, /v1/{seller,buyer}/export, POST /v1/seller/payouts, // POST /v1/seller/subscription*. Reviews, Q&A, wishlist, conversations, // messages, and SIDRA controls are NOT on it. pub fn api_key_forbidden_route(path: &str, method: &Method) -> bool { if path.starts_with("/v1/
- **Fix:** Add the in-scope write routes (at minimum review create/update, Q&A ask/answer/hide, conversation create/message, SIDRA toggle) to the JWT-only exclusion list, or introduce a narrower scope (e.g. keys may read but never post user-generated content). Record the acting credential type on sensitive writes. ---

### ME-124 — Adding a member is a phone-number enumeration oracle + forced seating with no consent, invite, notification, or audit

- **Severity:** Medium | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/stores.rs:432-488`
- **Found by:** agent-06 (?)
- **Impact:** (a) **Enumeration:** any store owner can probe arbitrary Bangladeshi numbers and learn which ones have platform accounts — the 404 message says so explicitly, while a hit returns 201/409/400. (b) **Forced membership:** seating needs no consent from the target. An owner can seat any user as `manager`, instantly giving that user `view_finances`, `manage_catalog`, …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/stores.rs:432-488` (`add_store_member`), handler `:545-558` - **Code evidence:** // stores.rs:447-461 — phone lookup distinguishes "no account" from every other outcome let target: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM users WHERE phone = $1").bind(&phone)... let (target_user_id,) = target.ok_or_else(|| { ApiError::not_found("এই নম্বরে কোনো অ্যাকাউন্ট নেই — no account exists for this phone number") })?; // ... then distinct 400 (owner), 409 (already member) for existing accounts No invite/accept st
- **Fix:** Return a single generic error for unknown phone vs. already-member vs. other failures (or require the target's user id rather than a phone lookup); add an invite → accept flow (pending membership, target accepts) or at minimum notify the target and let them leave a store themselves; audit all three member mutations (who seated/changed/removed whom, when) in an append-only log.

### ME-125 — Seller verification view discloses the full trade-licence number (and document URL) to any manager

- **Severity:** Medium | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/verification.rs:70-95`
- **Found by:** agent-06 (?)
- **Impact:** A manager (or anyone who compromises a manager session) reads the owner's full trade-licence number plus the document image URL, enabling business-identity impersonation / reuse of the licence number in another store's application. The asymmetry (NID masked, licence not) suggests an oversight, not a decision. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/verification.rs:70-95` (`SellerVerificationView`), `:161-186` (`seller_view`) - **Code evidence:** // verification.rs:70-83 pub struct SellerVerificationView { pub status: String, pub tier: String, pub nid_masked: Option<String>, // masked — correct pub trade_license_number: Option<String>, // FULL number, unmasked pub document_url: Option<String>, pub reviewer_note: Option<String>, ... } // verification.rs:173-178 — seller_view copies it verbatim trade_license_number: row.tra
- **Fix:** Mask the trade-licence number in `SellerVerificationView` the same way as the NID (e.g. show last 4 only), and restrict the view to the owner (see H-3).

### ME-126 — Verification can be approved with no document at all, and `verified` needs only a licence *number string*

- **Severity:** Medium | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/verification.rs:363-433`
- **Found by:** agent-06 (?); agent-13 (M5)
- **Impact:** An admin (rushed, mistaken, or compromised) can grant `basic`/`verified` — the badge that opens the KYC payout gate — on the strength of a typed NID number and a typed licence number, with no document image ever submitted or examined. Combined with H-1, the "human document review" the module docs promise is not enforced by the code at all. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/verification.rs:363-433` (`approve_verification`); `crates/domain/src/lib.rs:2615` (`validate_trade_license`) - **Code evidence:** `submit_verification` accepts `document_url: None` (`validate_branding_url(None)` → `Ok(None)`), and `approve_verification` never checks `row.document_url`. The only `verified`-tier evidence check is `if tier == "verified" && row.trade_license_number.is_none()` — a 4–40 character string (`validate_trade_license`), with no document proving the licence exists. - **Impact / exploit scenario:** An admin
- **Fix:** Require `document_url.is_some()` (and, for `verified`, a licence document) before approval; if a document-less path is genuinely intended, record that explicitly and require a reviewer note justifying it.

### ME-127 — No limit on stores per user: unlimited store creation, slug squatting, and a split-brain default store

- **Severity:** Medium | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/stores.rs:87-132`
- **Found by:** agent-06 (?)
- **Impact:** Any authenticated user (a `buyer` — no seller check exists on this endpoint) can create unlimited stores, squatting slugs (the only uniqueness is the `slug UNIQUE` constraint) and creating unlimited storefronts for spam / phishing / impersonation. Worse, the platform's own model is "one storefront per seller" (`stores.rs:140-147` comment), but nothing enforces it: once a user …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/stores.rs:87-132` (`create_store_for_user`), handler `:267-282`; `crates/api/src/access.rs:66-89` (`resolve_scoped_store`) - **Code evidence:** // stores.rs:87-118 — no count check, no role check, no per-user cap before INSERT pub async fn create_store_for_user(pool, owner, name, slug, description) -> ... { if name.trim().is_empty() { ... } if !is_valid_slug(slug) { ... } let store: Store = sqlx::query_as(INSERT_STORE_SQL)... // no LIMIT check // access.rs:83-87 — the no-?store_id= path always picks the
- **Fix:** Enforce a per-user store cap (ideally 1, matching the documented model, or an explicit small number) in `create_store_for_user` inside a transaction; require an explicit `?store_id=` once a user owns more than one store instead of silently defaulting to the earliest.

### ME-128 — Store creation skips the description validator: the 1000-char branding limit is bypassed at creation

- **Severity:** Medium | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/stores.rs:87-132`
- **Found by:** agent-06 (?)
- **Impact:** A seller can store a ~1 MiB name/description at creation, which is then returned verbatim on every public `GET /v1/stores/:slug` and admin list response — storage bloat and oversized payloads served to every storefront visitor, and an inconsistency a client that trusts the 1000-char limit will not expect. (Stored-markup risk is frontend-dependent and out of scope; the length …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/stores.rs:87-132` vs `:200-259` (`update_store_branding_for_store`) - **Code evidence:** `create_store_for_user` binds `description` straight into the INSERT with no call to `validate_store_description` (max 1000 chars, `domain/src/lib.rs:2493`), while the branding update path validates it. `name` likewise has no maximum length (only non-empty) — `stores.name` is unbounded `text` (migration `0002`). The only size bound is the global 1 MiB request-body cap (`lib.rs:543,1756`). - **Impact / exploit scenario:** A seller can store
- **Fix:** Validate `name` (add a domain max length, e.g. 100 chars) and `description` with `validate_store_description` in `create_store_for_user`, exactly as the branding path does.

### ME-129 — Admin verification queue is unbounded: no pagination on the full KYC list

- **Severity:** Medium | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/verification.rs:293-341`
- **Found by:** agent-06 (?)
- **Impact:** As the verification table grows, one `GET /v1/admin/verifications?status=all` loads every application (masked NIDs, phones, reviewer notes, document URLs) into memory and into a single response — a slow, memory-heavy endpoint an admin (or a compromised admin session) can hammer for a cheap DoS and a one-shot dump of the whole KYC queue. -
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/verification.rs:293-341` (`list_admin_verifications`), handler `:546-556` - **Code evidence:** // verification.rs:293-312 — no page/limit params, unlike every other admin list pub async fn list_admin_verifications(pool: &PgPool, status: Option<&str>) -> Result<Vec<AdminVerificationItem>, ApiError> { ... let rows: Vec<AdminVerificationRow> = sqlx::query_as("SELECT ... \ FROM store_verifications v JOIN stores s ... JOIN users u ... \ WHERE ($1::text IS NULL OR v.status = $1) ORDER BY v.submit
- **Fix:** Add `page`/`limit` (clamped to ≤ 100, as the other admin lists do) and select only `nid_last4` / the masked form from the DB instead of the full `nid_number`.

### ME-130 — KYC payout gate fails open on database error

- **Severity:** Medium | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/admin.rs:111-121`
- **Found by:** agent-06 (?)
- **Impact:** The module docs frame swallowing errors as "a bad settings row must never take checkout down" — but for the KYC gate the failure mode is backwards: a transient DB error (or a corrupted `platform_settings` row) silently turns the gate **off**, so `unverified` stores can request payouts during the outage. The same fail-open applies to the commission/advance/min-payout readers …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/admin.rs:111-121` (`platform_setting_i64`), `:190-221` (`platform_setting_text`, `effective_kyc_payout_gate`) - **Code evidence:** // admin.rs:111-121 — ANY db error is swallowed and read as "no override" pub async fn platform_setting_i64(pool: &PgPool, key: &str) -> Option<i64> { let raw: Option<Option<String>> = sqlx::query_scalar(...) .fetch_optional(pool).await.ok(); // <-- .ok() swallows the error raw.flatten().and_then(|value| parse_setting_i64(&value)) } // admin.rs:208-221 — gate is ON only on
- **Fix:** Distinguish "row missing" (gate off, by design) from "query failed" (propagate the error / fail closed for the gate — refuse payout creation while the setting cannot be read). At minimum log the swallowed error so the failure is visible.

### ME-131 — Suspension is not enforced on any in-scope write path: a suspended store can still gain members, rebrand, and get verified

- **Severity:** Medium | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/stores.rs:200-259`
- **Found by:** agent-06 (?)
- **Impact:** Suspension is the admin kill switch for a fraudulent store, but the switch only covers listing/checkout. While suspended, the store's team can still seat new members, change branding/logo/announcement, and submit (and have approved) a KYC application that raises `verified_tier` — so the moment an admin reactivates the store (or the suspension is lifted in error), it returns …
- **Evidence:** - **Severity:** Medium - **File:line:** `crates/api/src/stores.rs:200-259, 432-488` (branding, members), `crates/api/src/verification.rs:209-291` (submit); `crates/api/src/admin.rs:15-22` - **Code evidence:** No `status` check appears in `stores.rs`, `verification.rs` or `access.rs` at all (grep for `suspended` in the three files returns only the doc comment in `stores.rs:26`). The admin docs say suspension "hides its products … and makes checkout refuse carts containing its products" and that "the seller can still sign in and manage the catalogue". - **Impact / exploit scenario:** S
- **Fix:** Decide the intended policy explicitly and enforce it: at minimum, block verification submission/approval and member seating while `status='suspended'` (409 with a clear message), and document that catalogue management is the only permitted activity during suspension. Enforce it in `resolve_scoped_store` / the service functions so every caller inherits it. ---

### ME-132 — One giant test per file: the first failing assert aborts the rest of the wave, so later sections never run

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave10.rs:218`
- **Found by:** agent-16 (M1)
- **Impact:** A failure in section 1 (e.g. admin overview) hides a failure in section 8 (e.g. settings audit). CI reports "1 test failed" with no granularity, and a regression in a late section can be masked for weeks if an early section is broken. -
- **Evidence:** 37 test functions across 16,860 lines. Helpers `assert_eq!` inside `login()`/`create_store()`/`checkout_cod()` mean a setup failure aborts the whole file with no indication of which later security check would have failed. -
- **Fix:** Split each wave file into one `#[tokio::test]` per numbered section (sharing a small `mod common` or a per-file setup helper), or at minimum split the security-negative sections (auth, IDOR, audit) into their own tests so they run independently.

### ME-133 — Weak `>=` / `contains` assertions pass with foreign rows or extra data

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave5.rs:304`
- **Found by:** agent-16 (M2)
- **Impact:** These assertions cannot detect duplication bugs (e.g. a webhook logged twice, a poller advancing a shipment twice) — the exact failure modes they exist to catch. A test that passes with 1 *or* 100 rows is not pinning behaviour. -
- **Evidence:** `assert!(invalid_events >= 1, "rejected webhook is audit-logged");` — passes if *any* invalid event exists, including one left by another suite (see C1) or a duplicate. Wave30's header (wave30.rs:20-23) explicitly justifies `>=`: "Where a count could still legitimately include a foreign row, the assertions use `>=`". -
- **Fix:** Scope counts to the test's own rows (filter by `payment_id`/`shipment_id` already available in the test, or assert exact counts after a `TRUNCATE` that is actually exclusive). Replace `>= 1` with `== 1` wherever the fixture is owned by the test.

### ME-134 — Absolute totals (`total == 1`, `total == 8`) depend on global `TRUNCATE` + no parallel interference

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave3.rs:271`
- **Found by:** agent-16 (M3)
- **Impact:** Under parallel execution (C1) these are flaky by construction; under sequential execution they pass but prove nothing about isolation. A notification-enqueue regression in another flow could break wave29 spuriously, training the team to ignore failures. -
- **Evidence:** wave29 asserts exactly 8 notifications were queued by its own flow — true only if no other suite enqueued any and its own `TRUNCATE` (wave29.rs:244-258) ran last. Same pattern for order totals, audit-log totals (`assert_eq!(log["total"], 4 …)` wave28.rs:520-524). -
- **Fix:** Assert on the test's own rows (filter by `order_id`/`user_id`/kind) instead of global totals, or move these counts into the owning suite's isolated database (C1 fix).

### ME-135 — Wave 32 gzip is header-only: the body is never decompressed, so a corrupt gzip stream passes

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave32.rs:19-21`
- **Found by:** agent-16 (M4)
- **Impact:** A middleware that emits a `Content-Encoding: gzip` header with truncated/corrupt bytes (or double-compresses) passes the suite while every gzip-accepting client fails to decode real catalogue/search responses. -
- **Evidence:** `"The body bytes are asserted non-empty only — no gzip decoder is in the dev-dependencies, so decompression is not asserted here (the header contract is what this wave ships)."` The catalogue case does check the gzip magic bytes (`&bytes[..2] == &[0x1f, 0x8b]`, wave32.rs:355-360) but never decodes. The gzip test also only exercises `/health` (a tiny JSON body) plus `/v1/products`. -
- **Fix:** Add `flate2` (or `gzip` via `libz`) as a dev-dependency and assert the decompressed body parses as JSON and equals the plain body. Also assert gzip on a large payload (search results) and that `Accept-Encoding: br` / `deflate` do not produce a false `gzip` header.

### ME-136 — Security headers and body limit are proven on one endpoint each, not "every response" as claimed

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave28.rs:310-331`
- **Found by:** agent-16 (M5)
- **Impact:** Middleware ordering bugs (headers missing on error responses, on 401s, on CORS preflights, on the export download) would ship undetected. An oversized body to a different endpoint (e.g. product create with a huge description, or a JSON bomb to `/v1/orders`) might bypass the 1 MiB cap if the limit is applied per-route rather than globally. -
- **Evidence:** `"1. Security headers ride on every response — checked on the liveness probe, which needs no auth and no database."` — the comment claims "every response" but the test checks exactly one. The 413 test sends one oversized body to one endpoint. -
- **Fix:** Loop the header assertions over a matrix: `GET /health`, a 200 JSON response, a 401, a 404, a 413, and an OPTIONS preflight. Send oversized bodies to at least `/v1/orders`, `/v1/products`, and `/v1/auth/verify-otp` and assert 413 on each.

### ME-137 — CORS is tested only as a preflight to one path; real cross-origin requests, credentials, and error responses are untested

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave20.rs:59-80`
- **Found by:** agent-16 (M6)
- **Impact:** A misconfiguration that reflects arbitrary origins on real responses (credentialed CORS) or strips CORS on errors would pass the suite while exposing authenticated endpoints to cross-origin theft. -
- **Evidence:** `preflight()` sends `OPTIONS` with `Access-Control-Request-Method: GET` to `/v1/products` and reads only `access-control-allow-origin`. No test performs an actual `GET` with an `Origin` header (allowed or disallowed), checks `Access-Control-Allow-Credentials`, checks a disallowed method/header (`Access-Control-Request-Headers`), or checks CORS headers on an error response. -
- **Fix:** Add tests: `GET /v1/products` with allowed origin → `allow-origin` echoed; with disallowed origin → no header; with `Origin` + credentials → `allow-credentials` as configured; preflight with a disallowed method → no allowance; error response (401) with an allowed origin → headers still present (or explicitly absent, pinned either way).

### ME-138 — Wave 34 role matrix is incomplete: manager/staff are never tested on the money/sensitive surfaces they must be refused

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave34.rs:445-469`
- **Found by:** agent-16 (M7)
- **Impact:** The permission matrix (`domain::store_role_can`) is the authorisation boundary for staff accounts. An endpoint that forgets to call the matrix (or calls it with the wrong action string) lets a staff member spend the owner's balance or mint an API key with the owner's identity — and no test would fail. -
- **Evidence:** The matrix is tested for: members (wave34.rs:388-410), orders/shipments (wave34.rs:435-496), balance read (wave34.rs:442,520), coupons (wave34.rs:516,536), dashboard (wave34.rs:475-481). Never tested for manager/staff: subscription purchase (`POST /v1/seller/subscription` — spends balance), API-key creation (`POST /v1/api-keys` — creates a credential acting as the owner), seller export (`GET /v1/seller/export` — full data exfiltration), promotion purchase (`POST /v1/seller/products/:id/promote` — spends balance), verification submit (`POST /v1/seller/store/verification`), branding write (`PUT /v1/seller/store/branding`), and store suspension-adjacent admin surfaces. -
- **Fix:** Add a role-matrix sweep test: for each sensitive endpoint, assert manager → 403 and staff → 403 (and owner → success where applicable), driven from a table so new endpoints are added to the table when they are added to the router.

### ME-139 — API-key rate limiting is tested for one key, one window, one route — per-key isolation and window reset are untested

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave37.rs:362-385`
- **Found by:** agent-16 (M8)
- **Impact:** A global-bucket bug would let one abusive key deny service to all other integrations; a never-resetting window would permanently lock out a key after 3 requests. Both are plausible limiter bugs with no test. -
- **Evidence:** a fresh key gets 3 admissions and a 429 on the 4th; JWT traffic sails past. Never tested: a *second* key is unaffected when the first is limited (per-key isolation — a shared-bucket bug would rate-limit all keys together); the limit resets after a minute (window behaviour); the limiter counts writes as well as reads (only `GET /v1/auth/me` is exercised); a revoked/expired key's requests do not consume quota (or do — pin it); the 429 response shape (does it carry `Retry-After`?). -
- **Fix:** Add tests: two keys, exhaust key A, assert key B still gets 200; advance the clock (or inject a fake clock into the limiter) and assert admission resumes; exercise a write route under the limit; assert the 429 body/headers.

### ME-140 — Facebook import is only tested on the happy linked path; unlinked/stranger/malformed imports are untested

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave12.rs:63-71`
- **Found by:** agent-16 (M9)
- **Impact:** An unlinked import that succeeds would create products from attacker-supplied drafts with no Facebook provenance; a cross-store import would let one seller inject products into another's catalogue. The zero-price skip (wave12.rs:265, `price_poisha: 0` skipped) is the only input-validation pin in the whole flow. -
- **Evidence:** Every import call in the file follows a successful link (`POST /v1/seller/store/facebook` with a valid page id). Never tested: `import-preview`/`import`/`import-from-page` without any linked page; import as a stranger/other-store seller (the import writes products into *whose* store?); a draft with negative price/stock/weight; a draft with a duplicate `source_ref` inside a single batch (re-import idempotency is tested across calls, wave12.rs:181-198, but not within one batch); `import-from-page` with an unknown/unlinked page id. -
- **Fix:** Add tests: import-preview/import without linking → 400/404; import with another seller's token → 404; negative-price draft → skipped with a price reason; duplicate `source_ref` twice in one batch → one created, one skipped.

### ME-141 — Search: no injection/wildcard test, no pagination abuse test

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave15.rs:460-472`
- **Found by:** agent-16 (M10)
- **Impact:** An unescaped `%` in `q` could match the entire catalogue (information disclosure / scraping amplifier); an unclamped `per_page` could let one request dump the whole catalogue and DoS the DB; a negative page could panic or return wrong slices. -
- **Evidence:** Search is `ILIKE`-based, so `%` and `_` are wildcards and `'`/`\` are escape-sensitive. No test searches for `q=%`, `q=_`, `q=' OR '1'='1`, `q=\`, or a 10,000-character query. Pagination is tested only for well-behaved values (`per_page=2`, `page=1..3`, wave15.rs:388-401). No test sends `per_page=0`, `per_page=100000`, `page=0`, `page=-1`, or `limit` abuse on any list endpoint. -
- **Fix:** Add tests: `q=%` and `q=_` return 0 or literal-match results (pin the escaping rule); SQL-injection strings return 400 or 0 results, never an error 500; `per_page=0/100000`, `page=0/-1` → 400 or clamped to the documented bounds (assert the clamp values, not just "not 500").

### ME-142 — Reviews: only rating 6 is out-of-range; body/reply length limits, self-review, and stranger edit/delete are untested

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave8.rs:339-341`
- **Found by:** agent-16 (M11)
- **Impact:** An overlong review/reply could bypass the length cap (stored-XSS / DB bloat vector if the cap is enforced only in the domain function and a handler forgets to call it); a seller self-reviewing (or a stranger editing a 1-star review into 5 stars) would corrupt the rating summary the storefront displays. -
- **Evidence:** Rating 0 and negative ratings are never sent (only 6 is tested). Review-body length (1,000 chars) and reply length (500 chars) are never tested at the boundary. The seller reviewing their *own* product is never attempted (eligibility is "only products you received" — wave8.rs:314-327 — but a seller could buy their own product). Review edit (wave8.rs:377-393) and delete (wave8.rs:447-459) are tested only by the owning buyer; a stranger editing/deleting someone else's review is never attempted. -
- **Fix:** Add tests: rating 0, -1 → 400; 1,001-char review body → 400; 501-char reply → 400; seller buys own product then reviews → 403/400 (pin the rule); stranger PATCH/DELETE on another buyer's review → 403/404.

### ME-143 — Coupons: no stacking, no coupon+loyalty combination, no usage-limit race

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave14.rs:236-588`
- **Found by:** agent-16 (M12)
- **Impact:** Discount stacking is the classic coupon-abuse vector (two 50% coupons = free order, or coupon + points driving the total negative). If the checkout applies both, the seller loses money on every stacked order. -
- **Evidence:** Every checkout in wave14 uses at most one coupon code. Never tested: two coupon codes in one checkout (is the API single-code by design? pin it — 400 or second ignored); a coupon combined with loyalty-point redemption (do discounts stack? does the coupon apply to the pre- or post-points total?); the usage-limit race (H8); a coupon whose `min_order` is met only *because* of loyalty points (order-of-application matters for the money math). -
- **Fix:** Add tests: checkout with two codes → assert the documented behaviour (400, or only the first applies, with the discount total proving it); coupon + loyalty redemption → assert the exact `discount_total_poisha` and `grand_total_poisha`; assert the total never goes below the delivery charge / below zero.

### ME-144 — Verification submit: no test that a non-owner cannot apply for another store; admin full-view masking is asserted only indirectly

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave21.rs:109-124`
- **Found by:** agent-16 (M13)
- **Impact:** A verification application is an identity document submission. If the submit endpoint trusts a caller-supplied `store_id` without an ownership check, one seller could file (or overwrite) another store's KYC application; if the admin full view returns the raw NID, any admin-token leak exposes identity documents. -
- **Evidence:** Wave21 thoroughly tests masking on the seller view and admin queue ("the full number appears in no JSON body anywhere in this flow", wave21.rs:1-9) and the trade-license gate for `verified` tier. But no test submits a verification application scoped to a store the caller does not own, and no test asserts the *admin full-detail* endpoint (`GET /v1/admin/verifications/:store_id/full`, wave21.rs:373-380 area) masks the NID/trade-license in its own response body (the masking assertions are on list/queue views). -
- **Fix:** Add tests: submit verification for another seller's store → 404/403; fetch the admin full view and assert the NID/trade-license fields are masked or absent (string-search the whole body for the submitted NID, as wave28.rs:497-500 does for the admin phone).

### ME-145 — Exports: cross-store leakage is checked by ID substring only; seller-coupon delete and admin-coupon toggle are untested

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave31.rs:320-345`
- **Found by:** agent-16 (M14)
- **Impact:** An export that includes another store's financial rows is a data breach; an untested coupon-delete could delete another seller's coupon (if the handler forgets the store scope) or fail to delete at all while the UI reports success. -
- **Evidence:** wave31's leak proof is a raw-string search for two UUIDs — it does not assert that the export excludes the other store's payouts, balances, charges, or buyer phones, nor that a seller export contains no admin-only fields. Separately, `DELETE /v1/seller/coupons/:id` (route at lib.rs:1546-1549) is never called in any test (`grep -rn "DELETE" crates/api/tests/wave14.rs` → only cart clears), and `PATCH /v1/admin/coupons/:id` is never called in any form (`grep -rn "admin/coupons/"` → 0 hits). The admin toggle is therefore entirely unproven while the seller toggle is tested (wave14.rs:567-582). -
- **Fix:** Add tests: parse the seller export and assert the *absence of whole sections* (no `payouts`/`balances` for other stores, no other store's buyer phones); call seller coupon DELETE (own → 200, other's → 404, then assert the coupon is gone from the list); call admin coupon PATCH toggle (admin → 200, seller → 403, and the coupon's `active` flips).

### ME-146 — Courier poller: the failure path is never triggered (`failed == 0` in every poll)

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave30.rs:374`
- **Found by:** agent-16 (M15)
- **Impact:** A courier outage would produce `failed` rows the operator has never seen in tests; if the poller treats failures as "unchanged" or drops them, shipments stall with no alert and no test fails. -
- **Evidence:** wave30 proves progression (created → delivered), staleness, and exclusions (delivered/manual never polled), but every poll asserts `failed == 0`. No test makes the courier adapter fail (bad consignment, gateway error, malformed tracking response) and asserts `report.failed >= 1`, the shipment stays non-terminal, and the failure is recorded (shipment_events / last error) rather than silently skipped. The poller's batch cap (50, wave30.rs:347) is never exercised with >50 stale shipments. -
- **Fix:** Add a failing-courier fixture (a shipment whose refresh errors in the DEV simulator or a stubbed adapter) and assert `failed == 1`, the shipment status is unchanged, and a subsequent successful poll still advances it. Enqueue >50 stale shipments and assert the batch cap is respected.

### ME-147 — Reviews/Q&A/messaging: SIDRA callback replay and unknown-message callbacks are untested

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave13.rs:344-420`
- **Found by:** agent-16 (M16)
- **Impact:** SIDRA is an external AI bridge with a shared secret; a replayed callback could double-post AI replies into a buyer conversation, and a cross-conversation callback could inject an AI reply into the wrong thread. -
- **Evidence:** The wrong-secret callback is tested (wave13.rs:312-330, 401 + nothing stored). Never tested: the *same* valid callback delivered twice (replay — does it create two suggestions?); a callback referencing an unknown `message_id` or a message from a different conversation (cross-conversation injection); a callback after the suggestion was dismissed (is re-send a 409 like re-send after send?); a seller sending a suggestion for a conversation they do not own (stranger send → ?). -
- **Fix:** Add tests: identical callback twice → exactly one suggestion (assert count, not just status); callback with an unknown message id → 404; callback for a message in conversation A but naming conversation B → 404; send-as-stranger → 404/403.

### ME-148 — Loyalty: earn timing on cancelled/failed orders is untested (earn is only proven on delivery)

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave35.rs:314-341`
- **Found by:** agent-16 (M17)
- **Impact:** If earn is granted at checkout rather than delivery (or not reversed on cancel), buyers can farm points by placing and cancelling orders — free discounts with no purchase. -
- **Evidence:** Wave35 proves earn-on-delivery, redeem, cap, refund-on-cancel *after* redeem, and clawback-on-return. Never tested: an order cancelled *before* delivery earns nothing (no points row); a failed-payment order earns nothing; points earned on a delivered order that is later cancelled (not returned) — is the earn clawed back or kept? The "cancel restocks points" path (wave35.rs:438-444) covers redeemed points, not earned points. -
- **Fix:** Add tests: cancel a placed order → `available_points` unchanged and no `earn` ledger row; fail a payment → no earn; deliver then cancel (if reachable) → assert the documented earn treatment.

### ME-149 — Real courier adapters are never tested: no mock HTTP anywhere; only the DEV simulator and the unconfigured 503

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave6.rs:142-169`
- **Found by:** agent-16 (M18)
- **Impact:** A real-gateway regression (wrong auth header, mis-parsed tracking status, a 500 mapped to "delivered") ships undetected and is discovered only in production against live couriers. The "honest production behaviour" tests prove the *absence* of couriers, not their presence. -
- **Evidence:** Every courier test either uses `CouriersConfig { dev_mode: true, ..Default::default() }` (deterministic `DEV-…` consignment ids, e.g. wave6.rs:87) or the default config (all `not_configured`, 503 on booking). The real adapter code paths — request signing, response parsing, error mapping, tracking-status normalisation for Steadfast/Pathao/RedX/eCourier/Paperfly — are never executed by any test. -
- **Fix:** Add a mock-HTTP test double (e.g. `wiremock`/`httpmock` as a dev-dependency, or a tiny local Axum stub server) and test one real adapter end-to-end: booking returns the gateway's consignment id, tracking maps each gateway status to the unified status, and a gateway 500 → `failed`/retryable, not a panic or a false "delivered".

### ME-150 — Manual couriers: counter-receipt uniqueness, RTO/return-via-manual, and invalid manual transitions are untested

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave19.rs:288-460`
- **Found by:** agent-16 (M19)
- **Impact:** Manual couriers are the fallback when API couriers are down; a duplicate receipt number could conflate two parcels, and an invalid transition could mark an order delivered (releasing earnings, wave9.rs) for a parcel that never moved. -
- **Evidence:** Wave19 proves manual booking writes the shipment directly, statuses sync the sub-order, and manual ops on an API-courier shipment are a 409 (per its header). Never tested: the counter receipt number — is it required? unique per courier? what happens on a duplicate? A manual shipment going to RTO/returned (the return-via-manual path, distinct from the API-courier return flow in wave16); an invalid manual status transition (e.g. `created → delivered` skipping steps, or a backwards transition); manual tracking updates by the buyer (read-only?). -
- **Fix:** Add tests: duplicate receipt number → 409/400; RTO/returned via manual status → sub-order `returned`, stock restocked, earnings reversed; invalid manual transition → 400; buyer manual-tracking write → 403/404.

### ME-151 — Returns: double `refund-done`, return on a cancelled sub-order, and return-window expiry are untested

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave16.rs:501`
- **Found by:** agent-16 (M20)
- **Impact:** Double `refund-done` is a direct money leak (pay the buyer twice); a return on a cancelled order could restock/refund for goods never shipped. -
- **Evidence:** The return lifecycle (request → approve → received → admin refund-done) is proven once, linearly. Never tested: `refund-done` called twice (is the refund double-paid? 409 or idempotent?); a return request on a cancelled/never-delivered sub-order; a return request after the return window (if one exists); a return approved by a stranger seller (wave16.rs:377-380 tests stranger *request*, not stranger *approve/receive*); the refund amount when a coupon/loyalty discount applied (is the refund the discounted or full amount?). -
- **Fix:** Add tests: second `refund-done` → 409 or provably-idempotent (assert payout/balance unchanged); return on cancelled sub-order → 400/404; stranger approve/receive → 404/403; refund amount with coupon+loyalty → assert the exact refunded poisha.

### ME-152 — Product creation validation is never tested at the HTTP layer (negative price/stock, bad status/category/weight)

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/domain/src/lib.rs:978-993`
- **Found by:** agent-16 (M21)
- **Impact:** If `validate_product_input` is ever bypassed in the handler (or its bounds change), negative-price products (pay the buyer to "buy") or negative-stock products (oversell from creation) enter the catalogue with no test failing. -
- **Evidence:** The only price-boundary data point in the suite is wave12's import flow, where a zero-price draft is *skipped* (wave12.rs:265, `price_poisha: 0` → skipped with a price reason) — an import rule, not the product-create rule. Note the domain rule *allows* price 0 (`>= 0`), so a free product is creatable via the API and no test pins either behaviour. -
- **Fix:** Add tests: `price_poisha: -1` → 400; `stock: -1` → 400; `status: "banana"` → 400; invalid `category` → 400; `weight_grams: 0/-1/huge` → 400 or the documented default; `price_poisha: 0` → assert the documented outcome (accepted or rejected, pinned either way).

### ME-153 — Inconsistent `TRUNCATE` discipline: wave2 truncates 4 tables; only 4 files use `RESTART IDENTITY`

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave2.rs:71`
- **Found by:** agent-16 (M22)
- **Impact:** Same family as C1: order-dependent assertions, and a false sense of isolation. A suite that truncates narrowly can inherit another suite's rows and pass or fail for the wrong reason. -
- **Evidence:** Wave2 leaves every other table (orders, payments, notifications, coupons, …) untouched, so its "clean slate" is not clean if another suite ran first — its `total == 1` assertions (wave2.rs:198-206) depend on that. Without `RESTART IDENTITY`, sequences survive truncation; any test depending on a sequence-backed value (invoice counters are table-backed and truncated, but other serial ids are not) can drift across runs. -
- **Fix:** Standardise one shared `TRUNCATE … RESTART IDENTITY CASCADE` list (all tables) in a single helper used by every file, or (better) fix C1 with per-binary databases so truncation scope stops mattering.

### ME-154 — Branding: no injection / hostile-URL / overlong-value tests; no role test for branding writes

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave18.rs:49-68`
- **Found by:** agent-16 (M23)
- **Impact:** Stored XSS via the announcement/description or a `javascript:` logo URL would execute in the storefront for every visitor; a staff member silently rebranding a store is a lesser but real integrity issue. -
- **Evidence:** Invalid branding values are tested (bad theme/colour → 400), but never: `logo_url: "javascript:alert(1)"`, `banner_url: "data:text/html,…"`, `announcement: "<script>…</script>"` or `"<img onerror=…>"`, an overlong announcement/description, or a branding write by a manager/staff member (wave34 never covers branding). The values are rendered publicly (`GET /v1/stores/:slug`, wave18.rs:71-75), so stored markup/URLs reach every visitor. -
- **Fix:** Add tests: `javascript:`/`data:` URLs → 400; HTML/script in announcement/description → 400 or escaped on read (assert the *stored and returned* value, pinning the policy); overlong values → 400; manager/staff branding write → 403.

### ME-155 — Notification enqueue atomicity at the API layer is claimed but never tested

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/notify/src/lib.rs`
- **Found by:** agent-16 (M24)
- **Impact:** If the outbox write ever moves outside the order transaction (a refactor, a new flow that enqueues after commit), orders will silently lose their SMS notifications (buyer never told their order shipped; seller never told a payout landed) with no test failing. -
- **Evidence:** Wave29 proves notifications are *queued* by each flow (counts per kind, wave29.rs:283-402) and the notify unit test proves `enqueue` writes a row (outbox.rs:88-103). But no test proves the atomicity claim: if the notification insert fails (or the order transaction rolls back), does the order also roll back — and conversely, can an order commit *without* its notification? The notify test calls `enqueue` directly on the pool, never inside a transaction that is rolled back. -
- **Fix:** Add a test that enqueues inside a transaction and rolls it back, asserting the notification row is gone; and a test that a failing enqueue (e.g. invalid `user_id` FK) fails the enclosing API operation rather than committing the order alone.

### ME-156 — Wave 37: API keys are never tested on seller-write endpoints, and `last_used_at` is never proven *not* to update on failed auth

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave37.rs:239-263`
- **Found by:** agent-16 (M26)
- **Impact:** A scope bug that lets a write key create products in a store it should not touch, or a `last_used_at` update on failed auth (letting an attacker keep a stolen key looking "used" / masking brute-force probing), would pass the suite. -
- **Evidence:** The read+write key is proven to write exactly once — `POST /v1/cart/items` (wave37.rs:252-263). It is never used to create a product, place an order, or change a sub-order status (the seller-write surfaces a "mobile app" key would actually use). Conversely, `last_used_at` stamping is asserted after *successful* calls (wave37.rs:326-332), but no test asserts a *failed* call (401 unknown key, 403 read-only write, 429) does not stamp `last_used_at` or update any row. -
- **Fix:** Add tests: write key creates a product / places an order as the owner (200), and a read key attempting the same → 403; assert `last_used_at` is unchanged after a 401 and after a 403.

### ME-157 — Wave 10/27: admin/analytics list endpoints have no pagination or cross-store leakage tests beyond the happy path

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave10.rs:315-337`
- **Found by:** agent-16 (M27)
- **Impact:** A filter that is ignored (returning all orders regardless of `status=`) would pass, leaking cross-store data to an admin view that is supposed to be filtered — and a missing pagination bound repeats M25 on the most sensitive lists. -
- **Evidence:** The `days` clamp *is* tested (wave27.rs:423-426), which is good. But admin list endpoints (`/v1/admin/orders?status=cancelled`, `/v1/admin/stores?q=wave10`, `/v1/admin/users?q=…`) are only checked for the presence of expected rows, never for the absence of rows outside the filter, never for pagination bounds, and never for a non-admin calling them with a `store_id`/`user_id` filter to probe other tenants' data. -
- **Fix:** Add tests: filtered admin lists contain *only* matching rows (assert every returned row matches the filter); `page`/`per_page` abuse on admin lists; non-admin with filter params → 403 regardless of filters. ---

### ME-158 — Search/list pagination: no abuse test for huge/zero/negative `per_page`/`page`/`limit` on any endpoint

- **Severity:** Medium | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave15.rs:388-401`
- **Found by:** agent-16 (M25)
- **Impact:** `per_page=100000` on `/v1/search`, `/v1/products`, `/v1/admin/orders`, or the audit log could dump the table in one request (DoS + scraping amplifier). A negative page could panic or return an unintended slice. -
- **Evidence:** `grep -rn "per_page=1000|per_page=0|page=0|page=-1|limit=1000|limit=0" crates/api/tests/` → **0 hits**. Every list endpoint accepts a caller-controlled page size; none is tested with an abusive value. -
- **Fix:** Add tests: `per_page=0`, `per_page=100000`, `page=0`, `page=-1` on search, product list, admin orders, and audit log → assert 400 or clamping to the documented max (pin the exact clamped value in the response).

## Low (99)

### LO-01 — CORS origin entries are only HeaderValue-checked: typos silently never match, `http://` origins allowed without warning

- **Severity:** Low | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:1026`
- **Found by:** agent-15 (L1)
- **Impact:** Availability footgun (misconfigured prod = broken web app, confusing to debug) and a plaintext-origin downgrade if an operator lists one. -
- **Evidence:** entries are split on `,`, trimmed, and filtered only for emptiness; `cors_layer` only rejects values that fail `HeaderValue` parsing. An entry like `https://shop.example.com/` (trailing slash), `shop.example.com` (no scheme), or `null` parses fine but never equals a browser-sent `Origin` header, so the web app is silently broken with no startup warning. Conversely `http://shop.example.com` is accepted with no warning, letting a plaintext origin read API responses. -
- **Fix:** Validate each entry as a proper origin (`scheme://host[:port]`, no path/trailing slash, `https` required unless the host is loopback) at startup; warn loudly and skip (or fail to start on) invalid entries.

### LO-02 — HSTS header lacks `includeSubDomains` (and `preload`)

- **Severity:** Low | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:1193`
- **Found by:** agent-15 (L2)
- **Impact:** Subdomains of the API host are not covered by the HSTS policy, so a first-visit HTTP downgrade/cookie-injection attack remains possible on sibling subdomains. -
- **Evidence:** `headers.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));` -
- **Fix:** `"max-age=31536000; includeSubDomains"` (and consider `preload` once the whole domain is HTTPS-only).

### LO-03 — Verbose error paths: internal details returned to clients; DB errors logged with full context

- **Severity:** Low | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:837`
- **Found by:** agent-15 (L4)
- **Impact:** Low-grade information disclosure and log contamination (phone numbers / provider internals in logs); the OTP-state oracle aids targeted guessing slightly. -
- **Evidence:** // lib.rs:838 — full sqlx error (Postgres messages can embed user-supplied values, e.g. duplicate-key details) to stderr: eprintln!("database error in {context}: {err}"); // auth.rs:141-144 — jsonwebtoken error text returned to the client: ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("failed to sign token: {e}")) // auth.rs:324 — SMS provider error text returned to the client: .map_err(|e| ApiError::service_unavailable(format!("SMS delivery failed: {e}")))?; Additionally, Axum's default extractor rejections (bad JSON, bad `Path<Uuid>`, bad query params) bypass the `{"error": ...}` envelope and return framework-default bodies containing serde error details (field names, expected …
- **Fix:** Return a static `"failed to sign token"` / `"SMS delivery failed"` to clients and log the detail server-side only; log sqlx errors with the error *code* only (or redact bound values); unify all verify-otp failures into one generic message; add a `Fallback` handler / custom rejection mapping so every error uses the `{"error": ...}` envelope.

### LO-04 — Health endpoints disclose version and DB status; `/health/ready` is an unauthenticated DB ping

- **Severity:** Low | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:1096`
- **Found by:** agent-15 (L7)
- **Impact:** Version fingerprinting (lets an attacker target known-vulnerable releases) and a free unauthenticated DB round-trip per request (see H1 — usable as a low-effort load probe/DoS amplifier). -
- **Evidence:** Json(HealthResponse { status: "ok", service: "bd-marketplace-api", version: env!("CARGO_PKG_VERSION"), database: if state.pool.is_some() { "configured" } else { "not configured" } }) // health_ready: runs `SELECT 1` per request; 200 {"status":"ready"} / 503 {"status":"not ready"}. -
- **Fix:** Drop `version` (or return it only on an authenticated/admin endpoint); restrict `/health/ready` to the compose network / require a shared health-check token or rate-limit it heavily.

### LO-05 — `API_PORT` parse failure silently falls back to 8080

- **Severity:** Low | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:1883`
- **Found by:** agent-15 (L9)
- **Impact:** A typo (`API_PORT=80x0`) silently binds 8080 instead of failing; combined with `docker-compose.yml` mapping `"${API_PORT:-8080}:8080"`, the container can end up listening on a port the operator did not intend (or `API_PORT=0` binds an ephemeral port and the service becomes unreachable through the mapped port). -
- **Evidence:** let port: u16 = std::env::var("API_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(8080); -
- **Fix:** Fail startup with a clear error on an unparseable `API_PORT` (same exit-code-2 discipline as the JWT-secret check).

### LO-06 — gzip compression on authenticated responses (BREACH-adjacent; low practical risk here)

- **Severity:** Low | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:1765`
- **Found by:** agent-15 (L10)
- **Impact:** Classically a BREACH-style oracle needs attacker-controlled plaintext reflected in the same compressed response as a secret plus a cross-site request primitive; this API has no cookies and CORS is an explicit allow-list, so practical exploitability is low — but the compression is applied unconditionally with no risk assessment. -
- **Evidence:** `CompressionLayer::new()` is the outermost layer, so it compresses every response with `Accept-Encoding: gzip`, including `POST /v1/auth/verify-otp` (which returns a JWT) and other authenticated JSON. No `Vary`/compression predicate is set. -
- **Fix:** Keep compression but exclude sensitive responses (e.g. only compress responses under a size/type predicate, or skip compression on `/v1/auth/*`); document the BREACH reasoning next to the layer. ---

### LO-07 — No `Permissions-Policy` header

- **Severity:** Low | **Area:** API Surface / Routing | **File:** `crates/api/src/lib.rs:1193`
- **Found by:** agent-15 (L3)
- **Impact:** Minor — a JSON API serves no HTML, so the practical risk is small; the header would only disable powerful browser features if markup were ever accidentally served. -
- **Evidence:** The header set is `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, `CSP: default-src 'none'; frame-ancestors 'none'`, `Strict-Transport-Security`. No `Permissions-Policy`. -
- **Fix:** Add `Permissions-Policy: camera=(), microphone=(), geolocation=(), payment=()` for completeness.

### LO-08 — OTP hash comparison is not constant-time

- **Severity:** Low | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:364`
- **Found by:** agent-01 (F13)
- **Impact:** A remote timing oracle on the hex hash comparison could in principle leak the stored hash prefix byte-by-byte. Practical exploitability over a network is very low (noise, and the attacker still needs the code, not the hash), and F02's offline attack dominates it — but it is a free fix. -
- **Evidence:** `if hash_otp(&phone, code) != row.code_hash {` — Rust `String: PartialEq` short-circuits on the first differing byte. -
- **Fix:** Compare with a constant-time equality (`subtle::ConstantTimeEq` on the decoded hash bytes) after hashing the supplied code.

### LO-09 — `accessible_store_ids` can return duplicate store ids (`UNION ALL`)

- **Severity:** Low | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/access.rs:190-212`
- **Found by:** agent-01 (F14)
- **Impact:** A user who is both owner and member of the same store (the schema's PK on `store_members` does not forbid adding the owner as a member — `0032_store_members.sql`) gets the store id twice. Callers use the list as an `IN (...)` filter, so today this is harmless, but any caller that counts, paginates, or sums over the ids (or over rows joined to them) will double-count that …
- **Evidence:** sqlx::query_as( "SELECT id, 'owner' FROM stores WHERE owner_user_id = $1 \ UNION ALL \ SELECT m.store_id, m.role FROM store_members m WHERE m.user_id = $1") -
- **Fix:** Use `UNION` instead of `UNION ALL`, or dedupe in Rust (`.unique()` / collect into a `HashSet`). Also forbid adding a store's owner as a member row.

### LO-10 — The JWT is delivered only as a JSON body; the API offers no HttpOnly-cookie transport

- **Severity:** Low | **Area:** Auth / OTP / Sessions | **File:** `crates/api/src/auth.rs:420-435`
- **Found by:** agent-01 (F16)
- **Impact:** Any XSS hole in the web app (frontend is out of scope for this audit, so this is an API-design note, not a confirmed XSS) can exfiltrate the 7-day token from storage; an `HttpOnly; Secure; SameSite` cookie would keep the token out of JavaScript's reach. The API currently gives integrators no safer option. -
- **Evidence:** `verify_otp` returns `{ "token": "<jwt>", "user": {...} }` as JSON. The only place the client can keep a Bearer token from a JSON body is script-accessible storage (typically `localStorage`). -
- **Fix:** Offer an HttpOnly, Secure, SameSite=Lax/Strict cookie transport for the web client (keeping the JSON bearer for native/API-key clients), and document that the JSON token must never be persisted in `localStorage` for the web app. ---

### LO-11 — Payout KYC gate reads a pre-resolved store struct — verification revoked between resolve and insert bypasses the gate

- **Severity:** Low | **Area:** Concurrency / Transactions / TOCTOU | **File:** `crates/api/src/payouts.rs:423-440`
- **Found by:** agent-20 (L4)
- **Evidence:** `if crate::admin::effective_kyc_payout_gate(state).await && store.verified_tier == "unverified"` — `store` was resolved by the handler (`resolve_scoped_store`) before this function runs, on a different pool read, with no re-read under lock. Combined with C2's missing transaction, the tier is never re-validated at insert time. -
- **Fix:** Re-read `verified_tier` inside the fixed C2 transaction (`SELECT verified_tier FROM stores WHERE id=$1 FOR UPDATE`) and gate on that value.

### LO-12 — Webhook event log has no dedup key — duplicate deliveries each write an audit row, and the log cannot distinguish a retry from a distinct callback

- **Severity:** Low | **Area:** Concurrency / Transactions / TOCTOU | **File:** `crates/api/src/payments.rs:293-306`
- **Found by:** agent-20 (L5)
- **Impact:** Audit noise only, but it weakens the exact log the module doc cites as the reason rejected callbacks are auditable ("every received callback is written to `payment_events` … so rejected callbacks are auditable too"). Under the C1 race, two concurrent deliveries write two rows while applying one logical result. -
- **Evidence:** - **File:** `crates/api/src/payments.rs:293-306`; `migrations/0009_create_payments.sql` (`payment_events` has no unique key on `(payment_id, event_type, gateway_event_id)` — indeed no gateway event id is stored at all). - **Impact:** Audit noise only, but it weakens the exact log the module doc cites as the reason rejected callbacks are auditable ("every received callback is written to `payment_events` … so rejected callbacks are auditable too"). Under the C1 race, two concurrent deliveries write two rows while applying one logical result. - **Fix:** Store the gateway's event/delivery id when
- **Fix:** Store the gateway's event/delivery id when available and add `UNIQUE(payment_id, gateway_event_id)` (or a hash of the raw body) with `ON CONFLICT DO NOTHING` for the log insert; keep the state transition itself in the C1 transaction. ---

### LO-13 — Migrations enforce almost none of the coupon/promotion rules in the DB — every invariant lives only in application code

- **Severity:** Low | **Area:** Coupons / Promotions | **File:** `migrations/0016_coupons.sql`
- **Found by:** agent-07 (L1)
- **Impact:** Any future endpoint, admin script, worker, or direct-SQL fix that inserts a coupon/promotion bypasses the API validators entirely: a 365-day promotion, an `ends_at <= starts_at` coupon, a fixed coupon of `i64::MAX` are all storable. Today there is one writer per table, so this is latent — but the overlap invariant (H2) is already unenforceable precisely because the DB has no …
- **Evidence:** -- 0016: no ends_at > starts_at, no upper bound on fixed discount_value, -- no constraint relating max_discount_poisha to discount_value: discount_value bigint NOT NULL CHECK (discount_value > 0), max_discount_poisha bigint CHECK (max_discount_poisha >= 0), -- 0027: days has a floor but no ceiling (API cap is 30), and no link -- between the stored window and days: days integer NOT NULL CHECK (days >= 1), -- seller_charges.ref_id has no FK and no uniqueness: ref_id uuid, -- cf. promotions.id; nothing stops a dangling or duplicated ref -
- **Fix:** Add DB `CHECK`s mirroring the domain validators: `ends_at > starts_at`, `days BETWEEN 1 AND 30`, `ends_at = starts_at + make_interval(days => days)` (or a trigger), an upper bound on `discount_value`, and `FOREIGN KEY (ref_id) REFERENCES promotions(id)` + `UNIQUE (kind, ref_id)` on `seller_charges`.

### LO-14 — No admin delete for platform coupons and no edit endpoint for any coupon — a mispriced live coupon cannot be corrected, only deactivated

- **Severity:** Low | **Area:** Coupons / Promotions | **File:** `crates/api/src/lib.rs:1552-1558`
- **Found by:** agent-07 (L2)
- **Impact:** An admin who creates a platform coupon with a wrong value (e.g. the H3 fixed-value typo, or a wrong `usage_limit`) cannot fix the value or delete the row — only flip `active` off. The bad row remains in the admin list and in exports, and re-activating it later re-exposes the bad terms. A seller *can* delete their own unused coupon, so the asymmetry is admin-only. Operational, …
- **Evidence:** // lib.rs — admin coupons: GET + POST on /v1/admin/coupons, PATCH only on /v1/admin/coupons/:id. // No DELETE route for admins, no PUT/PATCH that changes value/dates/limit anywhere: .route("/v1/admin/coupons", get(...).post(create_admin_coupon_handler)) .route("/v1/admin/coupons/:id", patch(set_admin_coupon_active_handler)) // coupons.rs delete_coupon requires a (non-null) store_id, so a platform coupon // (store_id NULL) can never match `WHERE id = $1 AND store_id = $2` anyway. -
- **Fix:** Add `DELETE /v1/admin/coupons/:id` (same redemption guard as the seller path: 409 if redemptions exist) and a guarded update endpoint for `discount_value`/`max_discount_poisha`/`usage_limit`/`ends_at` that refuses to lower `usage_limit` below `used_count` and refuses to raise a discount on a coupon that already has redemptions — or at minimum document the deactivate-only workflow.

### LO-15 — Preview trusts client-supplied money totals, including a missing `store_id` falling through to an arbitrary `store_goods_poisha`

- **Severity:** Low | **Area:** Coupons / Promotions | **File:** `crates/api/src/coupons.rs:200-247`
- **Found by:** agent-07 (L3)
- **Impact:** For a store coupon, a client that omits `store_id` entirely (`None`) falls into the `_` arm and can supply any `store_goods_poisha` — e.g. claim the whole cart total belongs to that store — and preview reports a discount the buyer cannot actually get at checkout. No money moves in preview, so the direct impact is a misleading UI / social-engineering aid ("look, the site says I …
- **Evidence:** // coupons.rs:240-247: let eligible = match coupon.store_id { None => input.goods_total_poisha.max(0), Some(store_id) => match input.store_id { Some(other) if other != store_id => 0, _ => input.store_goods_poisha.unwrap_or(0).max(0), // store_id == None also lands here }, }; -
- **Fix:** Require `store_id` to be present and equal to the coupon's store for a store-coupon preview (reject otherwise), and ideally compute preview server-side from the caller's actual cart instead of accepting totals from the client.

### LO-16 — Coupon creation accepts already-expired / never-starting windows with no warning

- **Severity:** Low | **Area:** Coupons / Promotions | **File:** `crates/api/src/coupons.rs:326-352`
- **Found by:** agent-07 (L5)
- **Impact:** A seller/admin typo (`ends_at` last month, or `starts_at`/`ends_at` swapped in the wrong year) creates a coupon that is born dead — checkout and preview both reject it, buyers complain, and the code is burned (a replacement needs a new code because codes are globally unique). Not a money leak, but a predictable misconfiguration the API could catch for free. -
- **Evidence:** // create_coupon validates only: coupon fields, usage_limit > 0, ends_at > starts_at. if input.starts_at.zip(input.ends_at) .is_some_and(|(starts_at, ends_at)| ends_at <= starts_at) { return Err(ApiError::bad_request("ends_at must be after starts_at")); } // No check that ends_at is in the future, or that starts_at is not in the past. -
- **Fix:** Warn (or 400, behind an explicit `allow_past` flag) when `ends_at <= now()` at creation; reject `starts_at` in the past unless the caller explicitly wants a backdated start. ---

### LO-17 — `chrono 0.4.45` is unmaintained (RUSTSEC-2026-0543) — no patched version exists

- **Severity:** Low | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `crates/api/Cargo.toml`
- **Found by:** agent-14 (A14-20)
- **Impact:** Not a vulnerability — no known bug is being exploited. The risk is forward-looking: a future bug in date parsing/formatting (used for OTP expiry, order timestamps, courier polling) would receive no upstream fix, and `cargo deny` with default settings fails on it. -
- **Evidence:** RUSTSEC-2026-0543 (2026-09-14, announced in `chronotope/chrono#1768`): *"The chrono crate is unmaintained; all versions are affected"*; `patched = []`. Recommended alternatives: `jiff`, `time` (already in the tree at `time 0.3.55`). -
- **Fix:** Plan the `chrono` → `time`/`jiff` migration (sqlx also depends on chrono, so this is blocked until sqlx migrates or the feature is dropped); until then record a dated `cargo deny` ignore with owner and review date.

### LO-18 — CI release artifacts are unsigned, un-checksummed, and retained only 14 days

- **Severity:** Low | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `(see agent report)`
- **Found by:** agent-14 (A14-21)
- **Impact:** Users are told (per the comment at lines 70-72) to download and run these binaries instead of building locally. With no SHA-256 sidecar, no signature (`cosign`/`minisign`), and no provenance attestation, a tampered artifact (compromised runner, A14-15) is indistinguishable from a good one, and after 14 days the artifact simply disappears, encouraging ad-hoc rebuilds from …
- **Evidence:** - name: Build release server binaries run: cargo build --release -p api -p worker - uses: actions/upload-artifact@v4 with: name: server-binaries path: | target/release/api target/release/worker retention-days: 14 # (web-standalone: same pattern, lines 108-118, tar -czf /tmp/web-standalone.tar.gz) -
- **Fix:** Publish `sha256sum` files alongside each artifact, sign the binaries (`cosign sign-blob` / SLSA provenance via `actions/attest-build-provenance`), and record the expected hashes in the release notes. Keep retention at 14 days only if a tagged release flow replaces it.

### LO-19 — Builder stage runs as root and the worker image ships without CA certificates

- **Severity:** Low | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `Dockerfile.api:15-20`
- **Found by:** agent-14 (A14-22)
- **Impact:** A malicious/compromised build script or crate build-script (`build.rs`) running as root in the builder can write anywhere in the image layers; the missing CA bundle is not exploitable today, but the first worker job that makes an HTTPS call (courier polling, webhook) will fail TLS verification or, worse, be "fixed" by disabling verification. Both are latent, not active, …
- **Evidence:** Both builder stages run `cargo build` as root (no `USER` before line 20/18). `Dockerfile.worker`'s runtime stage installs *no* `ca-certificates` — its header (lines 6-8) says *"this runtime needs no CA bundle yet (the Wave 1 worker makes no network calls)"* — while the API stage does install them (`Dockerfile.api:26-28`). -
- **Fix:** Run the builder as a non-root user (or use BuildKit `--mount=type=cache` + a dedicated builder user), and install `ca-certificates` in the worker runtime too so TLS works by default when network calls land. Add `apt-get upgrade` handling or a scheduled base-image rebuild (A14-12).

### LO-20 — Duplicate major versions of the same crates in the lockfile (supply-chain / bloat)

- **Severity:** Low | **Area:** Docker / CI / Infra / Secrets / Dependencies | **File:** `crates/api/Cargo.toml`
- **Found by:** agent-14 (A14-23)
- **Impact:** Two copies of a networking/middleware crate mean two audit surfaces and two sets of advisories to track, larger binaries, and subtle behaviour differences (e.g. which `tower` version's `Service` impl a middleware actually uses). `rand 0.8` is also an older line; no CVE was found for the locked versions in this pass, so this is hygiene, not an active vulnerability. -
- **Evidence:** Direct grep of `Cargo.lock` for `^name = "tower"$` / `"tower-http"` / `"rand"` returns two versions each (see §5 table). The workspace pins `rand = "0.8"` (`crates/api/Cargo.toml`) while newer transitive deps already resolve `rand 0.9.2`. -
- **Fix:** Align on one major version per crate where the API allows (single `tower`/`tower-http` line via `cargo update` + feature alignment; reconcile the `rand 0.8` direct dep with the transitive `rand 0.10`), and add `cargo deny check bans` with `multiple-versions = "deny"` (with explicit allowances where a duplicate is genuinely unavoidable). ---

### LO-21 — Manual tracking ids allow any non-whitespace characters, including HTML/control characters

- **Severity:** Low | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:544-573`
- **Found by:** agent-12 (L2)
- **Impact:** A seller can store `<script>alert(1)</script>`, control characters, or a fake-looking id in the only verifiable reference for a manual booking. If the id is rendered unescaped in tracking pages/admin views, it is a stored-XSS vector; at minimum it pollutes the audit trail. -
- **Evidence:** `if !(4..=64).contains(&char_count) { return Err(..) } if trimmed.chars().any(char::is_whitespace) { return Err(..) } Ok(Some(trimmed.to_string()))` — no charset restriction. -
- **Fix:** Restrict to `[A-Za-z0-9 _\-/]` (or the couriers' actual receipt alphabet) and reject control characters.

### LO-22 — Free-text validators (review / reply / question / answer / message / announcement) cap length only — no HTML/control-character handling

- **Severity:** Low | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:1009-1073`
- **Found by:** agent-12 (L3)
- **Impact:** Stored content is only safe if *every* renderer escapes it. The domain layer is the shared gate, so a single unescaped renderer (email/SMS body, admin view, export CSV — note `csv_escape` exists for CSV but no HTML escaper exists in this crate) turns a review/question into stored XSS or SMS/header injection. Low because rendering is out of scope, but the gap belongs in this …
- **Evidence:** each checks only `trim().chars().count()` against a max; none strips or rejects `<`, `>`, control characters, or zero-width characters. -
- **Fix:** Either reject control characters here and document "callers must HTML-escape on render", or add a shared `escape_html` helper and use it at render sites.

### LO-23 — Pathao token cache: 60-second floor can cache an already-expired token; lock `expect()` can poison the adapter

- **Severity:** Low | **Area:** Domain / Courier / Payments core | **File:** `crates/courier/src/lib.rs:760-810`
- **Found by:** agent-12 (L4)
- **Impact:** If Pathao returns `expires_in < 60` (or `0`), the token is cached for a full 60 s even though it is already expired — every call in that window fails with an auth error until the cache lapses. If any thread panics while holding the `Mutex`, the `expect()` makes *every* subsequent Pathao call panic (persistent DoS until restart). Concurrent cache misses also issue duplicate …
- **Evidence:** let ttl = expires_in.saturating_sub(60).max(60); let valid_until = std::time::Instant::now() + std::time::Duration::from_secs(ttl); // ... .lock().expect("pathao token cache lock is not poisoned"); -
- **Fix:** `ttl = expires_in.saturating_sub(60)` with no `.max(60)` (or a 5 s floor), replace `expect()` with a poison-tolerant lock (`lock().unwrap_or_else(|e| e.into_inner())`), and single-flight the refresh with a `tokio::Mutex` / `OnceCell`.

### LO-24 — SIDRA signature payload uses the same ambiguous `:` join

- **Severity:** Low | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:1709-1716`
- **Found by:** agent-12 (L6)
- **Impact:** Two different `(conversation_id, message_id, text)` tuples can produce the same signed string if any field contains `:`. The doc says ids are canonical hyphenated UUIDs (no `:`), so the practical risk is low, but `text` is free-form and the scheme has no domain separator — a future id format change breaks the unambiguity silently. -
- **Evidence:** `pub fn sidra_signature_payload(conversation_id: &str, message_id: &str, text: &str) -> String { format!("{conversation_id}:{message_id}:{text}") }` -
- **Fix:** Length-prefix fields or join with a separator that cannot appear in ids and escape it in `text` (or sign a canonical JSON encoding).

### LO-25 — OTP request cap (20/hour) is a documented test-suite compromise, not a security cap

- **Severity:** Low | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:2927-2945`
- **Found by:** agent-12 (L7)
- **Impact:** 20 codes/hour/phone × 5 verify attempts × a 6-digit space bounds SMS cost at 480 messages/day/number (the comment's own math) and leaves a slow brute-force channel open. This is a deliberate trade-off, recorded here so the next reviewer does not mistake 20 for a threat-modelled number. -
- **Evidence:** the comment states the design target was 5, but 20 was chosen because the shared CI database logs 9 requests/hour for one fixture phone. -
- **Fix:** Move the limit to config (per-environment), keep 5 in production, and keep 20 only for CI via an env override; consider per-IP as well as per-phone limiting.

### LO-26 — Plan commission discount has no floor; other per-plan values can go negative / unbounded

- **Severity:** Low | **Area:** Domain / Courier / Payments core | **File:** `crates/domain/src/lib.rs:1411-1426`
- **Found by:** agent-12 (L8)
- **Impact:** The discount is a bare `i64` with no floor. If the API computes `effective_rate = base_rate - discount` and `base_rate < discount` (e.g. a 0–50 bps promotional rate on a pro store), the effective rate goes negative — and `compute_commission` takes a `u32`, so the conversion either underflows/panics or is silently clamped somewhere undocumented. `plan_product_cap` returning …
- **Evidence:** pub fn plan_commission_discount_bps(plan: &str) -> i64 { match plan { PLAN_PRO => 100, PLAN_BUSINESS => 200, _ => 0 } } -
- **Fix:** Provide `effective_commission_bps(base, plan) -> u32` in this crate that saturates at 0, and document that `i64::MAX` must never be arithmetically combined.

### LO-27 — API-key hash is a fast unsalted SHA-256 with no pepper

- **Severity:** Low | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/api_keys.rs:101-107`
- **Found by:** agent-11 (L1)
- **Impact:** Honest calibration: with 40 alphanumeric chars (~238 bits) offline brute force of a leaked `key_hash` is infeasible even for a fast hash, so this is **not** exploitable as-is. The risk is fragility: the security of every key rests entirely on generator entropy; a future shorter key, a weakened RNG, or a partial leak (e.g. the 12-char prefix + a log fragment) gets no …
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/api_keys.rs:101-107` (`hash_api_key`); contrast `auth.rs:hash_otp` (also unsalted SHA-256, but OTPs are 6-digit and short-lived) - **Code evidence:** pub fn hash_api_key(full_key: &str) -> String { let mut hasher = Sha256::new(); hasher.update(full_key.as_bytes()); format!("{:x}", hasher.finalize()) } - **Impact:** Honest calibration: with 40 alphanumeric chars (~238 bits) offline brute force of a leaked `key_hash` is infeasible even for a fast hash, so this is **not** exploitable as-is. The risk is fragility: the
- **Fix:** Use a per-key salt stored alongside the hash, or an HMAC with a server-side pepper (`HMAC-SHA256(pepper, key)`), keeping the lookup by the resulting hash. Do not switch to a slow KDF (Argon2/bcrypt) for a per-request auth lookup without benchmarking — a peppered HMAC is the right trade-off here.

### LO-28 — API-key `last_used_at` is stamped before the handler runs, and a failed stamp is silently swallowed

- **Severity:** Low | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/api_keys.rs:500-525`
- **Found by:** agent-11 (L2)
- **Impact:** `last_used_at` records *authentication*, not a completed request: a key that is admitted and then gets a 403/500 downstream, or whose handler never runs, still looks "recently used", and a key used only for probing forbidden routes looks active. Every authenticated request also costs an extra `UPDATE` (write amplification on a hot path). Forensically this makes `last_used_at` …
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/api_keys.rs:500-525` (`authenticate_request`, tail) - **Code evidence:** if let Some(pool) = state.pool.as_ref() { if let Err(e) = sqlx::query( "UPDATE api_keys SET last_used_at = now() WHERE id = $1")… { eprintln!("warning: failed to stamp api key last_used_at: {e}"); } } Ok(user) - **Impact:** `last_used_at` records *authentication*, not a completed request: a key that is admitted and then gets a 403/500 downstream, or whose handler never runs, still looks "recently used", and a key used only for probing f
- **Fix:** Stamp `last_used_at` only on successful (2xx) responses (e.g. in a middleware/response hook), or at least record it alongside the request outcome; keep the best-effort behaviour.

### LO-29 — Facebook status endpoint discloses whether the platform's Meta app credentials exist

- **Severity:** Low | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/facebook.rs:121-145`
- **Found by:** agent-11 (L3)
- **Impact:** Any authenticated seller learns whether the platform has a Meta app configured — minor reconnaissance (tells an attacker whether the Graph import path is live and worth probing). Intended as a seller-facing status, so low severity, but the information is not needed by the seller. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/facebook.rs:121-145` (`facebook_status_for_owner`); `crates/api/src/lib.rs:991-995` (`facebook_config_from_env`) - **Code evidence:** `graph_api: if state.facebook_config.graph_api_configured() { "configured" } else { "not_configured" }`, where configured = both `FACEBOOK_APP_ID` and `FACEBOOK_APP_SECRET` present in env. - **Impact:** Any authenticated seller learns whether the platform has a Meta app configured — minor reconnaissance (tells an attacker whether the Graph import path is live and worth probing). Intended as a seller-facing sta
- **Fix:** Return `graph_api` status only to admins, or collapse it to a generic "facebook import: paste / graph" capability flag without naming credential presence.

### LO-30 — Facebook preview error message reflects untrusted `source_ref` verbatim (log-injection / error-channel reflection)

- **Severity:** Low | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/facebook.rs:229-237`
- **Found by:** agent-11 (L5); agent-19 (L4)
- **Impact:** A crafted `source_ref` containing newlines/ANSI/control characters can inject fake lines into logs and confuse error parsing; no XSS (JSON-encoded) and no stored effect. Minor hygiene issue. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/facebook.rs:229-237` (`preview_import`) - **Code evidence:** return Err(ApiError::bad_request(format!( "post '{}' is longer than {MAX_POST_TEXT_CHARS} characters", post.source_ref))); `source_ref` is fully attacker-controlled (no charset/length validation in preview) and is interpolated into the error string, which is returned to the caller and may be logged. - **Impact:** A crafted `source_ref` containing newlines/ANSI/control characters can inject fake lines into logs and confuse error parsing; no XSS (JSON-encoded) and no
- **Fix:** Truncate `source_ref` to e.g. 64 chars and strip control characters before interpolating into any error/log string; validate its charset/length at the preview entry point (see M4).

### LO-31 — `seller_analytics_scoped` / `seller_analytics_csv_scoped` are `pub` and take an arbitrary store list with no authorisation of their own

- **Severity:** Low | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/analytics.rs:348-349`
- **Found by:** agent-11 (L7)
- **Impact:** No live vulnerability was found — grep confirms the only callers in the crate are the two handlers, both of which authorise first. The risk is a future-caller footgun: any new caller that passes user-supplied ids without the same gate gets cross-store financial data (earnings, GMV, courier breakdown) with no compiler or runtime check. Defence-in-depth gap only. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/analytics.rs:348-349` (`seller_analytics_scoped`), `analytics.rs:459-460` (`seller_analytics_csv_scoped`) - **Code evidence:** Both are `pub async fn … (pool, store_ids: &[Uuid], days)` and query `WHERE so.store_id = ANY($2)` with whatever ids are passed; authorisation lives only in their callers (`seller_analytics_handler` / `seller_analytics_csv_handler` via `resolve_scoped_store(..., view_finances)` or `owned_store_ids`). - **Impact:** No live vulnerability was found — grep confirms the only callers in the crate are the two handlers, both
- **Fix:** Make both functions `pub(crate)` and/or take a proven `ScopedStore` token (a type that can only be constructed by `resolve_scoped_store`/`owned_store_ids`) instead of a raw `&[Uuid]`. ---

### LO-32 — API-key scope rule is method-based, so read-only keys are refused on read-only POST endpoints (and the rule trusts the method, not the operation)

- **Severity:** Low | **Area:** Facebook / API Keys / Risk / Audit | **File:** `crates/api/src/api_keys.rs:125-131`
- **Found by:** agent-11 (L6)
- **Impact:** A `read`-only key gets 403 on genuinely read-only POST endpoints (`POST /v1/delivery-charge`, `POST /v1/cart/split`, `POST /v1/commission/quote`, `POST /v1/coupons/preview`) — availability/UX wart for mobile-app integrations, not a security hole. Conversely, the model assumes no GET ever mutates; if a future GET endpoint gains a side effect, a read key would inherit it …
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/api_keys.rs:125-131` (`method_is_write`); `crates/domain/src/lib.rs:3285-3292` (`api_key_scope_allows`) - **Code evidence:** fn method_is_write(method: &Method) -> bool { !matches!(method, &Method::GET | &Method::HEAD | &Method::OPTIONS) } - **Impact:** A `read`-only key gets 403 on genuinely read-only POST endpoints (`POST /v1/delivery-charge`, `POST /v1/cart/split`, `POST /v1/commission/quote`, `POST /v1/coupons/preview`) — availability/UX wart for mobile-app integrations, not a security hole. Conversely, the model assumes
- **Fix:** Replace the method heuristic with a per-route capability table (route + method ⇒ required scope) as part of the H3 allowlist work.

### LO-33 — Manager spends their *own* earnings to promote another store's product (cross-store money confusion)

- **Severity:** Low | **Area:** IDOR / Authorization (cross-cutting) | **File:** `crates/api/src/promotions.rs:186`
- **Found by:** agent-18 (L4)
- **Impact:** low — self-harm only, no theft from the promoted store's owner; but the UX never warns whose balance is being spent, and the charge is attributed to the wrong store in the owner's view. -
- **Evidence:** - **Severity:** Low - **Location:** `crates/api/src/promotions.rs:186` (`promote_product`) - **Code evidence:** // promotions.rs:206-229 match crate::access::store_role(pool, user.user_id, store.id).await? { Some(role) => { crate::access::require_role(&role, domain::STORE_ACTION_MANAGE_MARKETING)?; } None => return Err(ApiError::not_found("product not found")), } … let payout_store = get_store_by_owner(pool, user.user_id).await?; // the CALLER's own store … sqlx::query("INSERT INTO seller_charges (seller_user_id, …) VALUES ($1, $2, 'promotion', $3)") .bind(us
- **Fix:** either (a) restrict `promote_product` to the store owner (money out of a store should be the owner's call, like payouts/subscriptions), or (b) charge the *promoted store's* owner with that owner's explicit consent flow — never silently charge the acting member's unrelated store. At minimum, surface "charged to YOUR store <slug>" in the response/UI.

### LO-34 — Public tracking uses the delivery phone as the only credential (documented design; weak/shared factor)

- **Severity:** Low | **Area:** IDOR / Authorization (cross-cutting) | **File:** `crates/api/src/tracking.rs:117`
- **Found by:** agent-18 (L5)
- **Impact:** the delivery phone is known to the seller, the courier, and anyone the buyer shares the link with, and phone numbers are low-entropy/guessable in a small range; combined with a UUID order id (unguessable) the practical risk is low. Not an IDOR (the phone check *is* the authorisation), but it is the weakest credential in the API and should be a conscious decision, not an …
- **Evidence:** - **Severity:** Low (residual / design-accepted, not a bug) - **Location:** `crates/api/src/tracking.rs:117` (`track_order`; handler `tracking.rs:218`, route `lib.rs:1566`) - **Code evidence:** deliberately unauthenticated by design ("a buyer shares the link with family, or opens it from an SMS"); gate is `phones_match_for_tracking(&delivery_phone, supplied_phone)` (`tracking.rs:143`), with all failures returning the same 404. The response exposes `grand_total_poisha`, items, statuses and shipment/consignment data. - **Impact:** the delivery phone is known to the seller, the courier, and anyon
- **Fix:** keep as-is if the shareable-link behaviour is required; otherwise add a short-lived signed tracking token per order, or throttle repeated wrong-phone attempts per order id so the phone cannot be brute-forced. ---

### LO-35 — `export_filename` uses `kind` and `yyyymmdd` unsanitised (latent header-injection if ever reused with other callers)

- **Severity:** Low | **Area:** Input Validation / SSRF / Uploads | **File:** `crates/domain/src/lib.rs:3080-3100`
- **Found by:** agent-19 (L1)
- **Impact:** Not exploitable through the current callers — `kind` is the hardcoded `"seller"`/`"buyer"` and `yyyymmdd` is `chrono`'s `%Y%m%d` (`exports.rs:258`, `:366`). If a future caller passes user-controlled `kind`/date, a `"` / CR / LF in either would break out of the quoted `filename="…"` and inject response headers (or, on a filesystem consumer, a path separator). The sanitiser's …
- **Evidence:** - **Severity:** Low - **File:line:** `crates/domain/src/lib.rs:3080-3100`, `crates/api/src/exports.rs:477-489` (`download_headers`) - **Code evidence:** // domain/src/lib.rs:3080-3100 — only slug_or_phone is sanitised pub fn export_filename(kind: &str, slug_or_phone: &str, yyyymmdd: &str) -> String { let sanitized: String = slug_or_phone.chars().map(|c| { /* [a-z0-9-] else '-' */ }).collect(); format!("bd-marketplace-{kind}-{sanitized}-{yyyymmdd}.json") } // exports.rs:477-489 — the filename goes into a header; HeaderValue::from_str // rejects CR/LF and falls back to
- **Fix:** Sanitise `kind` and `yyyymmdd` in `export_filename` the same way as `slug_or_phone` (or restrict `kind` to an enum and validate `yyyymmdd` as exactly 8 ASCII digits). Keep the `HeaderValue::from_str` fallback.

### LO-36 — No length or empty-string guard on any text column (systemic)

- **Severity:** Low | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (L1)
- **Impact:** `''` (empty string) passes every `NOT NULL` and every `CHECK … IN (…)` in the schema, so empty titles, names, reasons, and message bodies are storable; unbounded bodies/titles allow multi-megabyte rows (storage / listing DoS) if the service length checks are ever bypassed. This is low severity only because the service layer is stated to validate lengths. -
- **Evidence:** e.g. `users.name text` (`0001:8`), `stores.name text NOT NULL` (`0002:9`), `products.title text NOT NULL` (`0004:9`), `reviews.body text`, `messages.body text NOT NULL` (`0015:41`), `product_questions.body text NOT NULL` (`0026:16`) — all unbounded `text`, `NOT NULL` only. -
- **Fix:** add `CHECK (length(col) > 0)` / `CHECK (length(col) <= N)` (or `varchar(N)`) on user-supplied text columns, at least for `title`, `name`, `slug`, `body`, `reason`, and `announcement`.

### LO-37 — No `updated_at` trigger anywhere: every `updated_at` is write-it-yourself

- **Severity:** Low | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (L2)
- **Impact:** any code path that updates a row without setting `updated_at` leaves a stale timestamp; `orders` cannot even report when its status last changed, and the shipment worker poller (`0030:24-28` sweeps on `updated_at`) can miss or re-process rows whose timestamp never moved. -
- **Evidence:** `updated_at timestamptz NOT NULL DEFAULT now()` appears in `products` (`0004:17`), `carts` (`0005:9`), `payments` (`0009:27`), `shipments` (`0010:27`), `reviews` (`0011:18`), `payout_accounts` (`0012:19`), `product_variants` (`0031:59`), `store_subscriptions` (`0034:50`) — and `orders` (`0006`) has no `updated_at` at all. Nothing updates these columns. -
- **Fix:** add one `BEFORE UPDATE` trigger function (`SET NEW.updated_at = now()`) and attach it to every table with `updated_at`; add `updated_at` to `orders`.

### LO-38 — Missing indexes on hot FK / queue columns (verified against the index list)

- **Severity:** Low | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (L3)
- **Impact:** slow admin/lookup queries and slow `ON DELETE CASCADE` probes (e.g. deleting an order scans `return_requests` and `reviews` by `order_id`); at scale this is a latency / lock-holding DoS vector, not a correctness bug. -
- **Evidence:** no index on `reviews.order_id` (`0011:14`), `return_requests.order_id` (`0018:22`), `cart_items.product_id` (`0005:15` — the unique index leads with `cart_id`, so a product-only probe cannot use it), `notifications.user_id` (`0029:21` — only `(status, created_at)` exists, so a user's inbox is a seq scan), `promotions.store_id` / `promotions.seller_user_id` (`0027:41-43` — only `(product_id, ends_at)` exists), `messages.sender_id` (`0015:40`), `product_questions.answered_by_user_id` (`0026:18`), `products.category` (`0017:7` — the header says search filters on it, yet no index), `products (status, created_at)` for the public listing, `orders (status)` / `orders (payment_status)` for admin …
- **Fix:** add the indexes above (leading with the FK/queue column actually filtered on); prefer partial indexes for queues (e.g. `notifications (user_id, created_at DESC)`, `payouts (status, created_at) WHERE status = 'requested'`).

### LO-39 — `CREATE INDEX` (not `CONCURRENTLY`) inside the migration transaction; `IF NOT EXISTS` masks drift

- **Severity:** Low | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (L4)
- **Impact:** on a large production table a plain `CREATE INDEX` takes a lock that blocks writes for the whole build (checkout downtime / lock-queue pile-up). `IF NOT EXISTS` also silently skips the index if one with that name already exists with a *different* definition, leaving the intended index missing without error. -
- **Evidence:** -- Plain CREATE INDEX inside the migration transaction: at the current catalogue size that is a sub-second lock. -- On a large production table the same indexes should be built with CREATE INDEX CONCURRENTLY instead ... CREATE INDEX IF NOT EXISTS idx_order_items_product ON order_items (product_id); CREATE INDEX IF NOT EXISTS idx_shipments_status_updated ON shipments (status, updated_at); -
- **Fix:** for large tables build with `CREATE INDEX CONCURRENTLY` as an ops step outside the migration transaction (as the file's own comment suggests); in migrations, assert the index definition after creation (e.g. check `pg_indexes`) rather than relying on `IF NOT EXISTS`.

### LO-40 — `DROP CONSTRAINT` by auto-generated name is brittle and non-idempotent

- **Severity:** Low | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (L5)
- **Impact:** if the constraint was renamed, already dropped, or the DB was restored with different naming, the migration fails mid-run; if the migration runner autocommits each statement, the window between the `DROP` and the re-`ADD` leaves the column with *no* check at all. A failed re-run cannot be retried cleanly. -
- **Evidence:** ALTER TABLE orders DROP CONSTRAINT orders_payment_status_check; ALTER TABLE seller_charges DROP CONSTRAINT seller_charges_kind_check; Neither uses `IF EXISTS`, and both assume Postgres' auto-generated `<table>_<column>_check` name. -
- **Fix:** name every `CHECK` constraint explicitly at creation time (`CONSTRAINT orders_payment_status_check CHECK (…)`), and use `DROP CONSTRAINT IF EXISTS` plus a post-migration assertion that the new constraint exists.

### LO-41 — Same Facebook Page can be linked to multiple stores

- **Severity:** Low | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (L6)
- **Impact:** two stores can claim the same Facebook Page identity, so Facebook imports / provenance for that page are ambiguous and one seller can import (or appear to own) another seller's page posts. -
- **Evidence:** `ALTER TABLE stores ADD COLUMN facebook_page_id text; ALTER TABLE stores ADD COLUMN facebook_page_name text;` — both nullable, no `UNIQUE` on `facebook_page_id`. -
- **Fix:** `CREATE UNIQUE INDEX stores_facebook_page_uidx ON stores (facebook_page_id) WHERE facebook_page_id IS NOT NULL;`

### LO-42 — `invoice_number` is not unique and need not match its store/sequence; invoice store need not match the sub-order's store

- **Severity:** Low | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (L7)
- **Impact:** two invoices (different stores, or the same store with different `seq`) can carry the same `invoice_number` — a legal-document number collision — and an invoice can cite a sub-order belonging to a different store than `invoices.store_id`, so the per-store sequence and the document disagree. -
- **Evidence:** CREATE TABLE invoices ( store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE, sub_order_id uuid NOT NULL UNIQUE REFERENCES sub_orders (id) ON DELETE CASCADE, seq bigint NOT NULL CHECK (seq >= 1), invoice_number text NOT NULL, -- no UNIQUE, no format CHECK UNIQUE (store_id, seq)); -
- **Fix:** `UNIQUE (invoice_number)` (or `UNIQUE (store_id, invoice_number)` if numbers are per-store by design, plus a format `CHECK` tying it to the store prefix + `seq`); add a trigger verifying `invoices.store_id = (SELECT store_id FROM sub_orders WHERE id = NEW.sub_order_id)`.

### LO-43 — Notifications: no per-user index, no idempotency key, attempts and `sent_at` incoherent

- **Severity:** Low | **Area:** Migrations / Schema / SQL | **File:** `(see agent report)`
- **Found by:** agent-13 (L9)
- **Impact:** the same event can enqueue duplicate notifications (double SMS — cost and spam); `attempts` can exceed the stated `MAX_ATTEMPTS (3)` or go negative; `status='sent'` with `sent_at IS NULL` (or `'pending'` with `sent_at` set) corrupts delivery reporting. -
- **Evidence:** CREATE TABLE notifications ( user_id uuid NOT NULL REFERENCES users (id), phone text NOT NULL, kind text NOT NULL, body text NOT NULL, payload jsonb NOT NULL DEFAULT '{}', status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','sending','sent','failed')), attempts integer NOT NULL DEFAULT 0, -- no CHECK (attempts BETWEEN 0 AND 3) last_error text, created_at timestamptz NOT NULL DEFAULT now(), sent_at timestamptz); CREATE INDEX notifications_pending_idx ON notifications (status, created_at); `kind` has no `CHECK`/vocabulary; there is no `UNIQUE` on (event, user) and no index on `user_id` (see L3). -
- **Fix:** add an idempotency key (`event_key text UNIQUE`, e.g. `<event_type>:<entity_id>`); `CHECK (attempts BETWEEN 0 AND 3)`; `CHECK (status <> 'sent' OR sent_at IS NOT NULL)`; `CHECK (kind IN (…))`; index `(user_id, created_at DESC)`. ---

### LO-44 — Rounding is inconsistent across money functions (floor vs ceil vs half-up)

- **Severity:** Low | **Area:** Money Math / Currency | **File:** `domain/src/lib.rs:154`
- **Found by:** agent-17 (?)
- **Impact:** < 1 poisha per line — immaterial per order, but the directions are unprincipled: the platform systematically loses a poisha on every commission/coupon computation and systematically over-collects a poisha on advances. Sum over volume, it is a small, one-directional leakage. -
- **Evidence:** commission and coupon discounts truncate in the platform's favour-loss direction (seller/platform loses < 1 poisha per sub-order); the COD advance rounds *up* against the buyer; courier whole-taka rounds half-up. -
- **Fix:** document one rounding policy (e.g. "all buyer-facing charges round up, all platform/seller credits round down" or a single half-up rule) and apply it uniformly; add property tests asserting `Σ sub-order figures == order figure` for each function.

### LO-45 — Facebook price extraction truncates a third decimal digit instead of rounding

- **Severity:** Low | **Area:** Money Math / Currency | **File:** `crates/domain/src/lib.rs:1837-1852`
- **Found by:** agent-17 (?)
- **Impact:** a seller's pasted price with 3+ decimal places imports 1 poisha (or more, for the first-labelled-price rule) below the stated price. Only affects the FB-import draft price, which the seller reviews before publishing. -
- **Evidence:** the fraction parser caps at 2 digits (`frac_end - frac_start < 2`); `"100.555 tk"` parses as 100.55 (10,055 poisha) and the leftover `5` starts a new token. -
- **Fix:** parse up to 3 fraction digits and round half-up to poisha (or reject > 2 decimal places with a draft error).

### LO-46 — `mask_phone` is weak for short numbers and reveals 6 of 11 digits for normal ones

- **Severity:** Low | **Area:** Notifications / Notify / Worker | **File:** `crates/api/src/notifications.rs:104`
- **Found by:** agent-10 (L1)
- **Impact:** an admin-view reader (or anyone who obtains an admin response) can reconstruct or brute-force numbers far more easily than the "masked" label suggests. -
- **Evidence:** // domain/src/lib.rs:1076-1090 pub fn mask_phone(phone: &str) -> String { let local = to_local_phone(phone).unwrap_or_else(|| phone.to_string()); let chars: Vec<char> = local.chars().collect(); if chars.len() < 7 { return "****".to_string(); } let head: String = chars.iter().take(3).collect(); let tail: String = chars.iter().skip(chars.len() - 3).collect(); format!("{head}****{tail}") } For a valid BD number this yields `017****678` — 6 of 11 digits visible, enough to narrow a number sharply when combined with a name/order. Worse, if `to_local_phone` fails, the raw input is masked instead: a 7-character input reveals 6 of its 7 characters (`123****567`). -
- **Fix:** mask to first 2 + last 2 digits max (`01*****78`), and fully hide (`****`) any number that fails canonicalisation instead of masking the raw input.

### LO-47 — `last_error` is returned verbatim to admins; a future gateway error could leak PII into the API response

- **Severity:** Low | **Area:** Notifications / Notify / Worker | **File:** `crates/api/src/notifications.rs:36-47`
- **Found by:** agent-10 (L2)
- **Impact:** a real gateway implementation that formats errors with the destination number, message body, or API key fragment (`send failed for +880...: ...`) would pipe that straight into the stored row and the admin API. -
- **Evidence:** `last_error: row.last_error` is serialised unchanged into the admin JSON, and `SmsError { message: String }` places no constraint on content. Today's senders use fixed strings, so nothing leaks today. -
- **Fix:** define a stable error-code enum for `SmsError` (store the code in `last_error`, keep free-form detail only in server logs), and/or scrub phone-number patterns from error strings before storing them.

### LO-48 — Dev-mode selection logic is duplicated in the worker instead of shared

- **Severity:** Low | **Area:** Notifications / Notify / Worker | **File:** `crates/worker/src/main.rs:118-124`
- **Found by:** agent-10 (L3)
- **Impact:** a future edit to one copy (e.g. adding `SMS_DEV_MODE=1`, case-insensitive matching) silently desynchronises the worker's sender from the API's OTP sender — the exact disagreement `sender_from_env` was created to prevent. -
- **Evidence:** the worker re-implements `APP_ENV == "dev" || SMS_DEV_MODE == "true"` locally and passes it to `sender_from_env`, while `AppState::from_env` — called two lines later in the same `main` — computes the same value independently. The two are identical today, but nothing enforces that. -
- **Fix:** delete the worker's local copy; use `state.sms_dev_mode` (or a single shared `notify::sms_dev_mode_from_env()` helper) for both.

### LO-49 — `process_pending`'s `batch` parameter is unvalidated (negative/zero/huge)

- **Severity:** Low | **Area:** Notifications / Notify / Worker | **File:** `crates/notify/src/lib.rs:242-265`
- **Found by:** agent-10 (L4)
- **Evidence:** `batch: i64` is bound straight into `LIMIT $1` with no clamp. The only caller passes the constant 20, so this is latent — but the function is `pub`, and a negative batch makes Postgres reject the query (`LIMIT must not be negative`), failing every worker cycle, while `batch = 0` silently processes nothing forever. -
- **Fix:** clamp in `process_pending` (`batch.clamp(1, 100)` or return an error for `batch < 1`), mirroring the `limit.clamp(1, 100)` already done in `notifications.rs:77`.

### LO-50 — Worker env knobs have no upper bound: a huge `WORKER_POLL_SECONDS` silently disables the worker

- **Severity:** Low | **Area:** Notifications / Notify / Worker | **File:** `crates/worker/src/main.rs:74-89`
- **Found by:** agent-10 (L5)
- **Impact:** a config typo (extra zeros) silently stops notifications, shipment updates, and renewals for hours, with no warning beyond a startup line. -
- **Evidence:** Ok(seconds) if seconds >= 1 => seconds, // no upper bound `WORKER_POLL_SECONDS=86400` is accepted without complaint; the worker then drains the outbox once a day. `SHIPMENT_POLL_STALE_MINUTES=0` is explicitly allowed ("every non-terminal API shipment is stale"), which as a steady state hammers the shared courier accounts every cycle. -
- **Fix:** clamp `WORKER_POLL_SECONDS` to a sane range (e.g. 1–300 s) and warn loudly when the value is outside it; require an explicit separate flag for the zero-staleness catch-up mode.

### LO-51 — `return_update` fallback renders the raw DB status verbatim into the buyer's SMS

- **Severity:** Low | **Area:** Notifications / Notify / Worker | **File:** `crates/domain/src/lib.rs:3054-3058`
- **Found by:** agent-10 (L6)
- **Impact:** confusing or spoofable message content if the status value is ever influenced by input; at minimum a broken-looking SMS for an internal state. -
- **Evidence:** _ => format!("BD Marketplace: আপনার রিটার্ন অনুরোধের হালনাগাদ অবস্থা: {} ({store}, অর্ডার #{short})।", vars.status) The status comes from the `return_requests.status` column; today it is one of a small internal set, so the branch is nearly unreachable — but any unexpected value (a future status, a bad migration, a manual DB edit) is messaged to the buyer verbatim. -
- **Fix:** render only the known statuses and map anything else to a generic "your return request has been updated" line without interpolating the raw value.

### LO-52 — Worker builds `AppState` via `from_env`, silently falling back to the dev JWT secret

- **Severity:** Low | **Area:** Notifications / Notify / Worker | **File:** `crates/worker/src/main.rs:192-197`
- **Found by:** agent-10 (L7)
- **Impact:** any future worker feature that verifies or issues tokens (a worker HTTP endpoint, a signed callback) would silently inherit a forgeable secret; and the "worker started fine" log gives no indication the deployment is misconfigured. -
- **Evidence:** // lib.rs:655-663 — from_env falls back with only an eprintln warning let jwt_secret = match std::env::var("JWT_SECRET") { Ok(secret) if !secret.is_empty() => secret, _ => { eprintln!("WARNING: JWT_SECRET is not set — using the DEV-ONLY default secret. ..."); DEV_JWT_SECRET.to_string() } }; The worker's own doc comment (`main.rs:28-35`) notes the JWT gate in `api::run` is not triggered here — true, but the worker still carries a state object whose JWT secret is the publicly-known dev default, and the worker happily runs in production that way. The worker serves no HTTP and issues no tokens today, which is why this is Low, not higher. -
- **Fix:** make the worker fail startup when `JWT_SECRET` is unset (it does not need the secret at all — prefer a constructor that does not load it), or at least exit non-zero on the fallback in non-dev environments. ---

### LO-53 — Delivery name/address have no length cap

- **Severity:** Low | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/orders.rs:617`
- **Found by:** agent-03 (L2)
- **Impact:** A buyer (or a script) can store multi-kilobyte names/addresses on every order, bloating order rows, receipts, courier payloads and the seller's delivery views. Minor storage/UX abuse, no money impact. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/orders.rs:617–623` (`place_order` validation) - **Code evidence:** only `trim().is_empty()` is checked for `delivery_name` / `delivery_address`; the columns are unbounded `text` (migration 0006). The only bound is the 1 MB global body limit (`MAX_REQUEST_BODY_BYTES`, `lib.rs:543`). - **Impact:** A buyer (or a script) can store multi-kilobyte names/addresses on every order, bloating order rows, receipts, courier payloads and the seller's delivery views. Minor storage/UX abuse, no money impact. - **Fix:** Cap `delivery_name` (e.g. 100 chars) a
- **Fix:** Cap `delivery_name` (e.g. 100 chars) and `delivery_address` (e.g. 500 chars) in `place_order`, mirroring the existing 500-char cap on stock-adjustment notes.

### LO-54 — Checkout reads risk, commission rates and settings outside the transaction (mixed-snapshot pricing)

- **Severity:** Low | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/orders.rs:875`
- **Found by:** agent-03 (L5)
- **Impact:** An admin changing the commission/advance settings mid-checkout can price different seller groups in one order at different effective rates, and the risk verdict can be computed from history that a concurrent checkout is about to change. In practice the window is milliseconds and the effect is a few poisha or a borderline advance decision; the threshold endpoint's window only …
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/orders.rs:875–895` (`buyer_risk_for_user(pool, …)`), `crates/api/src/orders.rs:838–860` (per-group `effective_commission_bps_for_store`), `crates/api/src/orders.rs:862` (`effective_commission_bps`); `crates/api/src/inventory.rs:385–405` (`set_low_stock_threshold_for_seller` check-then-update on the pool) - **Code evidence:** the risk verdict is read on `pool`, not on `tx`, before the order exists; each seller group's commission rate is resolved with a separate pool/state read in a loop, and the order-wide rate is read after them — all outsid
- **Fix:** Resolve the effective commission/advance settings once, before the transaction, and reuse the snapshot for every group; run the risk read inside the checkout transaction; lock the product row (`FOR UPDATE`) in `set_low_stock_threshold_for_seller` before the ownership check and update. ---

### LO-55 — Cart add / set-quantity do not re-check product active / store suspended (checkout is the only backstop)

- **Severity:** Low | **Area:** Orders / Cart / Checkout | **File:** `crates/api/src/cart.rs:177`
- **Found by:** agent-03 (L3)
- **Impact:** A buyer can fill a cart with products from a suspended store, or raise the quantity on a line whose product was archived after adding; everything fails only at checkout. UX dead-end, no money impact (checkout refuses before writing anything). -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/cart.rs:177–246` (`add_cart_item` checks `products.status` once, never the store), `crates/api/src/cart.rs:249–285` (`set_cart_item_quantity` checks neither) - **Code evidence:** `add_cart_item` rejects non-`active` products but never reads `stores.status`; `set_cart_item_quantity` only validates the quantity and updates the row. Checkout later 409s on suspended stores (orders.rs:717) and 400s on inactive products (orders.rs:727). - **Impact:** A buyer can fill a cart with products from a suspended store, or raise the quantity on a line whos
- **Fix:** Check `stores.status` in `add_cart_item` (409 like checkout) and re-validate product/variant activeness in `set_cart_item_quantity`; keep checkout as the authoritative backstop.

### LO-56 — `gateway_adapter!::refund()` can falsely report success; `verify()`/`payout_status()` are dead, trivially-true stubs

- **Severity:** Low | **Area:** Payments | **File:** `crates/payments/src/lib.rs:258-264`
- **Found by:** agent-02 (?)
- **Impact:** A future edit that makes `check()` return `Ok` on success (the natural shape once live calls land) silently turns `refund()` into a function that reports a successful refund while doing nothing — the exact bug H-04 warns about, planted in advance. The `verify()` stubs likewise give a false sense of a verification layer that does not verify anything (and, per H-06, nothing …
- **Evidence:** // 287-294: unlike its siblings, refund() returns Ok(()) after check() — if check() ever returned Ok, this claims a refund happened without moving money async fn refund(&self, _reference: &str, _amount_poisha: i64) -> Result<(), PaymentError> { self.check()?; Ok(()) } // 221-224: COD "verification" verifies nothing async fn verify(&self, _reference: &str) -> Result<PaymentStatus, PaymentError> { Ok(PaymentStatus::Recorded) } -
- **Fix:** Make `refund()` match its siblings (`self.check()?; unreachable!(...)`) until a real refund is implemented, or delete the trait methods until they have real implementations. Make `CodAdapter::verify` validate the reference format (`COD-<order_id>` for a real order) and `DevGatewayAdapter::verify` look the reference up instead of prefix-matching.

### LO-57 — No currency anywhere: BDT-only is a comment, not a check

- **Severity:** Low | **Area:** Payments | **File:** `migrations/0009_create_payments.sql:17-29`
- **Found by:** agent-02 (?)
- **Impact:** A gateway callback reporting 4,690 (taka) for a 469,000-poisha payment fails closed today (exact-equality check), but there is no positive defence: nothing records which unit an amount is in, so a future gateway integration that normalises units in one place and not another can mark orders paid for 1/100th of the price, or vice-versa. Same for a multi-currency future. -
- **Evidence:** `grep -ri "currency\|BDT\|taka" crates/payments/src/lib.rs crates/api/src/payments.rs migrations/0009_create_payments.sql migrations/0019_advance_payments.sql` finds only the "BDT only" doc comment. -
- **Fix:** Add `currency CHAR(3) NOT NULL DEFAULT 'BDT'` to `payments` (and to the webhook body, included in the HMAC), and reject any callback whose currency is not `BDT`.

### LO-58 — `payouts.store_id` FK has no `ON DELETE CASCADE`, unlike `payout_accounts` (inconsistent; store deletion fails or strands rows)

- **Severity:** Low | **Area:** Payouts / Subscriptions / Loyalty | **File:** `migrations/0012_create_payouts.sql:8-10`
- **Found by:** agent-04 (L2)
- **Evidence:** - **Severity:** Low - **File:line:** `migrations/0012_create_payouts.sql:8-10` vs `:17-18` - **Code evidence:** store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE, -- payout_accounts store_id uuid NOT NULL REFERENCES stores (id), -- payouts: no action - **Fix:** Add `ON DELETE CASCADE` (or an explicit archival rule) to `payouts.store_id` for consistency; decide deliberately whether deleting a store should ever delete its payout history.
- **Fix:** Add `ON DELETE CASCADE` (or an explicit archival rule) to `payouts.store_id` for consistency; decide deliberately whether deleting a store should ever delete its payout history.

### LO-59 — Subscription sweep loads every lapsed row with no LIMIT (unbounded batch)

- **Severity:** Low | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/subscriptions.rs:500-530`
- **Found by:** agent-04 (L3)
- **Impact:** After a long worker outage the candidate set can be large; one sweep cycle can stall the worker loop (which also runs notifications and shipment polling) for a long time. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/subscriptions.rs:500-530` (`process_subscriptions`) - **Code evidence:** `SELECT ... FROM store_subscriptions WHERE status = 'active' AND plan <> 'free' AND expires_at <= now() ORDER BY expires_at ASC` with `fetch_all`, no `LIMIT`; each candidate then gets its own transaction in a sequential loop. - **Impact:** After a long worker outage the candidate set can be large; one sweep cycle can stall the worker loop (which also runs notifications and shipment polling) for a long time. - **Fix:** Add `LIMIT` (e.g. 100–500) per run; the next cycle p
- **Fix:** Add `LIMIT` (e.g. 100–500) per run; the next cycle picks up the remainder.

### LO-60 — Cancel-refund rounding keeps the remainder with the platform on every cancelled sub-order

- **Severity:** Low | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/loyalty.rs:200-231`
- **Found by:** agent-04 (L5)
- **Impact:** A multi-sub-order cancellation loses up to just under 1 point per sub-order (up to 99 poisha of discount value each). Small per order, systematic across the platform. -
- **Evidence:** - **Severity:** Low (documented behaviour; systematic small buyer loss) - **File:line:** `crates/api/src/loyalty.rs:200-231` (`refund_for_cancelled_sub_order`: `share_poisha / LOYALTY_POINT_VALUE_POISHA`, floored) - **Code evidence:** `let refund_points = share_poisha / LOYALTY_POINT_VALUE_POISHA;` — the module doc acknowledges "the few remainder poisha stay with the platform". - **Impact:** A multi-sub-order cancellation loses up to just under 1 point per sub-order (up to 99 poisha of discount value each). Small per order, systematic across the platform. - **Fix:** Carry the remainder: refund
- **Fix:** Carry the remainder: refund on whole-order cancel from the order-level `loyalty_points_used`, or accumulate remainders per buyer and credit a point when they total 100 poisha.

### LO-61 — KYC payout gate does not cover subscription spend from the same balance

- **Severity:** Low | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/payouts.rs:423-447`
- **Found by:** agent-04 (L6)
- **Impact:** An unverified seller who cannot withdraw can still spend the balance on platform subscriptions. The money goes to the platform, not out of it, so this is not exfiltration — but it is an inconsistent application of the "unverified sellers cannot move money" rule, and a seller could be charged while believing their funds are frozen pending verification. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/payouts.rs:423-447` (gate in `request_payout` only); `crates/api/src/subscriptions.rs:339-444` (`subscribe` has no gate check) - **Code evidence:** `request_payout` returns 400 when `effective_kyc_payout_gate(state).await && store.verified_tier == "unverified"`; `subscribe` never calls `effective_kyc_payout_gate`. - **Impact:** An unverified seller who cannot withdraw can still spend the balance on platform subscriptions. The money goes to the platform, not out of it, so this is not exfiltration — but it is an inconsistent application of the
- **Fix:** Apply the same gate (or an explicit documented exemption) to `subscribe` and promotion purchases.

### LO-62 — `seller_charges` are seller-level but subtracted in every store's balance, so multi-store totals double-count the same charge

- **Severity:** Low | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/payouts.rs:196-214`
- **Found by:** agent-04 (L7)
- **Evidence:** - **Severity:** Low (conservative direction — it understates, never overstates, spendable balance) - **File:line:** `crates/api/src/payouts.rs:196-214` (`seller_earnings_on`); `migrations/0034_subscriptions.sql:26-35` - **Code evidence:** (SELECT COALESCE(SUM(amount_poisha), 0) FROM seller_charges WHERE seller_user_id = (SELECT owner_user_id FROM stores WHERE id = $1)) AS charges_total_poisha The subquery is keyed by seller, not by store — so a subscription/promotion bought for store B reduces the reported available balance of store A by the same amount, and summing "available" a
- **Fix:** Attribute charges to a store (`store_id` on `seller_charges`, backfilled from the subscription/promotion's store) and subtract only that store's charges in its aggregate; or document that per-store "available" is not additive and show a seller-level total.

### LO-63 — `processed_at` is overwritten on every payout transition, losing the approval timestamp; rejection note handling is asymmetric

- **Severity:** Low | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/admin.rs:646-656`
- **Found by:** agent-04 (L8)
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/admin.rs:646-656` (`transition_payout`) - **Code evidence:** `UPDATE payouts SET status = $2, processed_at = now(), note = COALESCE($3, note)`. Every transition (approve, then mark-paid) re-stamps `processed_at`, so the time the payout was *approved* is lost once it is marked paid; and a `note` supplied on approve can never be cleared, only replaced. - **Fix:** Stamp `processed_at` only on the first transition out of `requested` (or add `approved_at` / `paid_at` columns), and handle note clearing explicitly. ---
- **Fix:** Stamp `processed_at` only on the first transition out of `requested` (or add `approved_at` / `paid_at` columns), and handle note clearing explicitly. ---

### LO-64 — Loyalty point counts are silently clamped to `i32` at checkout (order snapshot and redeem row can disagree with the real count)

- **Severity:** Low | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/orders.rs:966`
- **Found by:** agent-04 (L4); agent-17 (?)
- **Evidence:** - **Severity:** Low (unrealistic at normal volumes, but silent truncation of money-adjacent data) - **File:line:** `crates/api/src/orders.rs:966` and `:1213` (checkout, in-scope via the loyalty redemption path) - **Code evidence:** .bind(i32::try_from(loyalty_points_used).unwrap_or(i32::MAX)) // orders.loyalty_points_used .bind(i32::try_from(-loyalty_points_used).unwrap_or(i32::MIN)) // loyalty_points redeem row A `loyalty_points_used` above `i32::MAX` would be stored as `i32::MAX` on the order and as `i32::MIN` (a huge *positive* delta after negation semantics) on the ledger —
- **Fix:** Reject the checkout with a 400 if the redeemable points exceed `i32::MAX` instead of clamping, or widen the columns/ledger delta handling and keep a single source of truth.

### LO-65 — A 0-poisha payout passes validation when the admin minimum is 0, then fails with a 500 from the DB check

- **Severity:** Low | **Area:** Payouts / Subscriptions / Loyalty | **File:** `crates/api/src/payouts.rs:459-467`
- **Found by:** agent-04 (L1); agent-17 (?)
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/payouts.rs:459-467`; `crates/domain/src/lib.rs:1260-1272` (`validate_payout_amount_with_min`); `migrations/0012_create_payouts.sql:21` (`CHECK (amount_poisha > 0)`) - **Code evidence:** `validate_payout_amount_with_min` only checks `amount < min` and `amount > available`. `valid_min_payout_setting` (`admin.rs:121-124`) allows `0`, so with `min_payout_poisha = 0`, `amount = 0` passes validation (`0 < 0` false, `0 > available` false) and the `INSERT` then violates the DB `CHECK`, surfacing as an internal error instead of a 400. - **Fix:** Reje
- **Fix:** Reject `amount_poisha <= 0` in `validate_payout_amount_with_min` regardless of the configured minimum.

### LO-66 — Public product view leaks seller-internal fields: `low_stock_threshold` and `source_ref`

- **Severity:** Low | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/products.rs:100-123`
- **Found by:** agent-05 (L2)
- **Impact:** Any anonymous caller learns a seller's low-stock warning level (operational detail useful for timing stock-outs) and, for Facebook imports, the source Facebook post id (`source_ref`), which can be used to find/enumerate the seller's Facebook posts. Low value per field, but it is data the buyer never needs. -
- **Evidence:** `get_product` (`products.rs:683`) and `list_products_handler` (`products.rs:652`) are unauthenticated-optional endpoints returning the full `Product` shape; there is no separate public serialisation that strips seller fields. -
- **Fix:** Split the serialisation: a `PublicProduct` view without `low_stock_threshold` / `source_ref` (and any future seller-only fields) for anonymous/public endpoints; keep the full shape for seller endpoints only.

### LO-67 — `compare_at_poisha` (and `category`) can never be cleared via PATCH

- **Severity:** Low | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/products.rs:565-580`
- **Found by:** agent-05 (L3)
- **Impact:** Once a seller sets a `compare_at` strikethrough price (see M2), there is no API way to remove it — the misleading "was" price stays on the listing even after the seller wants the promotion ended. Same for `category`. This is a minor functional trap, not an access-control bug. -
- **Evidence:** .bind(patch.compare_at_poisha.or(existing.compare_at_poisha)) // None in the patch keeps the old value // … and: let merged_category = normalize_category(patch.category.as_deref()).or(existing.category); // the comment at products.rs:578-580 acknowledges it: "a PATCH can set the category but not clear it back to NULL" -
- **Fix:** Support explicit nulls (e.g. `Option<Option<i64>>` / a `clear_compare_at: bool` flag, or `serde(default)` double-Option) for `compare_at_poisha`, `category`, `title_bn` and `description` in `UpdateProductInput`.

### LO-68 — Variant adjust-stock works on inactive (deactivated) variants

- **Severity:** Low | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/variants.rs:420-505`
- **Found by:** agent-05 (L4)
- **Impact:** Stock can be moved on a variant buyers cannot see or buy; when the variant is later reactivated (`PATCH is_active: true`, `variants.rs:347`), that pre-loaded stock goes live without any further check — the "deactivated means frozen" assumption is false. The audit row exists, so this is detectable, but it is surprising and inconsistent with product-level adjust-stock refusing …
- **Evidence:** The locked-row query (`variants.rs:458-468`) selects `v.stock, p.store_id, s.owner_user_id … WHERE v.id = $1 AND v.product_id = $2 FOR UPDATE OF v` with no `AND v.is_active` predicate, and the function never checks `is_active` before updating stock and writing the audit row. -
- **Fix:** Reject adjust-stock on an inactive variant (400, "reactivate the variant first"), or document that inactive-variant stock is intentionally adjustable and surface the pending stock in the seller UI.

### LO-69 — The stock audit trail cannot be replayed once variants are involved

- **Severity:** Low | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/inventory.rs:178-215`
- **Found by:** agent-05 (L5)
- **Impact:** Forensic/dispute use of the trail (oversell investigation, seller stock-fraud review) is unreliable for any product with variants; a wrong `new_stock` passed by a future caller would be stored without complaint. -
- **Evidence:** The module doc (`inventory.rs:1-40`) promises "the trail alone replays a product's stock history", but a variant movement's `new_stock` is the variant's on-hand count, not the product's. A product-level replay summing `delta` / trusting `new_stock` across mixed product- and variant-level rows yields a wrong product total, and `record_stock_adjustment` never verifies `new_stock == previous new_stock + delta`. -
- **Fix:** Store both snapshots on variant rows (`new_stock` = variant count, plus a `product_new_stock` / effective-stock snapshot), or document that variant rows replay per-variant only and add a `variant_id`-scoped replay query; add a debug assertion / DB check that `new_stock` is consistent where it can be verified.

### LO-70 — Draft visibility is inconsistent: a store *member* can open a draft by ID but cannot see it in any listing

- **Severity:** Low | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/products.rs:213-216`
- **Found by:** agent-05 (L6)
- **Impact:** Staff with `manage_catalog` (who can edit the catalogue, including drafts) cannot find drafts through the API they manage them with unless they guess/know the UUID — a usability/inconsistency bug that pushes staff toward sharing direct links or owner credentials. Not a disclosure bug (members are authorised for the catalogue). -
- **Evidence:** `list_products` / `list_store_products` filter drafts to the owner; `get_product` admits any member (`Wave 34` comment, `products.rs:707-712`). The same split applies to suspended-store products. -
- **Fix:** Make the listing predicate match the detail gate: `(p.status = 'active' OR <viewer has a role in the store>)` — pass a viewer store-role set / `EXISTS (SELECT 1 FROM store_members …)` into `PRODUCT_WHERE` — or document that listing is owner-only by design and give members a seller catalogue endpoint.

### LO-71 — Deactivating a variant strands buyers' carts and writes no audit row

- **Severity:** Low | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/variants.rs:393-418`
- **Found by:** agent-05 (L7)
- **Impact:** A seller (or staff member) deactivating a variant — accidentally or maliciously — breaks every in-flight buyer cart containing it, with no warning and no trail entry saying who deactivated it or when beyond `updated_at`. Reactivation is the only recovery. -
- **Evidence:** // variants.rs:402-408 — a bare UPDATE, no check for carts/orders, no stock_adjustments row "UPDATE product_variants SET is_active = false, updated_at = now() \ WHERE id = $1 AND product_id = $2 RETURNING {VARIANT_COLUMNS}" Checkout rejects inactive variants (`orders.rs:744-749`), and cart-add rejects them (`cart.rs:188-200`), so existing cart lines become un-checkout-able. -
- **Fix:** Before deactivation, check for cart lines / pending sub-orders referencing the variant and either warn (return the count) or block while any exist; record the deactivation/reactivation in the audit trail (or an `audit_log` entry with actor + variant id).

### LO-72 — `set_low_stock_threshold` is an unlocked, non-transactional lost update

- **Severity:** Low | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/inventory.rs:418-438`
- **Found by:** agent-05 (L8); agent-20 (L3)
- **Impact:** Last-writer-wins on a low-value setting; a threshold change can also land after the product was transferred / the caller's role revoked, in the (small) window between the check and the `UPDATE`. No money or stock is directly at stake. -
- **Evidence:** // inventory.rs:426-435 — ownership is checked on the pool, then a bare UPDATE on the pool let threshold = validate_low_stock_threshold(threshold).map_err(ApiError::bad_request)?; owned_product_row(pool, user, product_id).await?; sqlx::query("UPDATE products SET low_stock_threshold = $2, updated_at = now() WHERE id = $1") .bind(product_id).bind(threshold_i32).execute(pool).await?; Contrast `adjust_stock_for_seller` (`inventory.rs:265`, `FOR UPDATE` in a transaction). Two concurrent threshold writes can interleave, and the check-then-write is not atomic with the ownership check. -
- **Fix:** Do the ownership check and the `UPDATE` in one transaction with `SELECT … FOR UPDATE`, or fold the role check into the `UPDATE … WHERE id=$1 AND store_id IN (SELECT …)` so a revoked caller updates zero rows.

### LO-73 — Recommendations: "not bought" counts unpaid orders, and also-bought leaks exact co-purchase counts

- **Severity:** Low | **Area:** Products / Variants / Inventory / Search | **File:** `crates/api/src/recommend.rs:110-125`
- **Found by:** agent-05 (L9)
- **Impact:** (a) A buyer who placed but never paid / never received an order will never be recommended that product again, even though they do not own it — inconsistent with the personalised rail, whose signals require `delivered`. (b) `co_count` is competitor intelligence: anyone can read the exact number of distinct orders pairing two products, i.e. a lower bound on a rival store's sales …
- **Evidence:** // recommend.rs:118-122 — any non-cancelled sub-order counts as "bought" WHERE o.buyer_user_id = $1 AND oi.product_id = p.id AND so.status <> 'cancelled' // recommend.rs:96-99 — the exact count is returned to any anonymous caller /// Distinct orders containing both this product (in a different sub-order) and the base product. pub co_count: i64, -
- **Fix:** Align `NOT_BOUGHT_BY_CALLER` with the delivered/paid rule used for signals (or document the "ordered = bought" choice); consider returning a banded/rounded popularity signal instead of the exact `co_count`, or restrict `co_count` to seller/analytics surfaces. ---

### LO-74 — Return reason and seller note are stored verbatim with no sanitisation; seller note has no length limit at all

- **Severity:** Low | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/returns.rs:284-360`
- **Found by:** agent-09 (L1)
- **Impact:** Stored-XSS if any consumer renders `reason` / `seller_note` as HTML without escaping (frontend is out of scope, so this is conditional — but the API is the last place that can guarantee it), and a trivial storage/egress DoS via multi-MB notes on every return decision. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/returns.rs:284-360` (`create_return_request`, `validate_return_reason` checks length ≤ 500 chars only), `:429-499` and `:470-473` (`decide_return_request`: `let note = note.map(str::trim).filter(|note| !note.is_empty());` then `seller_note = COALESCE($3, seller_note)`) - **Code evidence:** `validate_return_reason` (`domain/lib.rs:2367-2377`) validates only non-empty / ≤ 500 chars. The seller note has no validation whatsoever — an arbitrarily large string is accepted and stored in `return_requests.seller_note` (`text`, `migrations
- **Fix:** Cap `seller_note` at the same 500 chars (validate in `decide_return_request` before the update), and strip/escape HTML control characters on write (or document and test that all renderers escape these fields).

### LO-75 — Buyer export filename embeds the buyer's full phone number in cleartext

- **Severity:** Low | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/exports.rs:457`
- **Found by:** agent-09 (L2)
- **Impact:** The buyer's full phone number leaks into logs and shared/downloaded-file metadata for anyone who sees the filename — PII exposure beyond the document itself, which at least requires the authenticated session. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/exports.rs:457` - **Code evidence:** let filename = export_filename("buyer", &profile.phone, &now.format("%Y%m%d").to_string()); // domain export_filename sanitises to [a-z0-9-] but KEEPS all digits: // "bd-marketplace-buyer-01712345678-20261126.json" The filename is sent in the `Content-Disposition: attachment` header (`exports.rs:500-515`) and lands in download history, proxy logs and filenames on disk. - **Impact / exploit scenario:** The buyer's full phone number leaks into logs and shared/downloaded-file metadata for
- **Fix:** Build the buyer filename from the user id (or a hash / the literal word `buyer`) instead of the phone, e.g. `bd-marketplace-buyer-<uuid-prefix>-<date>.json`.

### LO-76 — Invoice sequence numbers are burned by any entitled peek — including an admin preview

- **Severity:** Low | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/invoices.rs:199-255`
- **Found by:** agent-09 (L3)
- **Impact:** An admin opening an invoice to check something, or a buyer idly viewing it, permanently consumes a number in the store's legal sequence; combined with M7 (returned sales keep their invoice) this creates gaps/orphan numbers in a sequence that tax/accounting consumers expect to be gapless. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/invoices.rs:199-255` - **Code evidence:** `get_or_issue_invoice` issues the invoice (advancing the store's `next_seq`) on the *first entitled read* — buyer, seller *or admin* — even if that read is only a preview. A sequence, once issued, is never reused (by design). - **Impact / exploit scenario:** An admin opening an invoice to check something, or a buyer idly viewing it, permanently consumes a number in the store's legal sequence; combined with M7 (returned sales keep their invoice) this creates gaps/orphan numbers in a sequence
- **Fix:** Issue numbers only when the invoice is actually rendered/downloaded for a business purpose (or on a distinct `issue` action), not on a plain `GET`; at minimum exclude the admin read path from issuance.

### LO-77 — Seller invoice is owner-only: store team members who manage orders cannot view the invoice

- **Severity:** Low | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/invoices.rs:223`
- **Found by:** agent-09 (L4)
- **Impact:** A manager/staff member handling a delivered order cannot pull its invoice for the buyer, forcing owner intervention for routine work. -
- **Evidence:** - **Severity:** Low (availability / inconsistency, not a leak) - **File:line:** `crates/api/src/invoices.rs:223` - **Code evidence:** `InvoiceActor::Seller => ctx.store_owner_user_id == user.user_id` — no `store_role_on` / `require_role` check, unlike returns (`returns.rs:429-460`), shipments (`shipments.rs:513-560`) and the scoped export, all of which admit `manager`/`staff` under `STORE_ACTION_MANAGE_ORDERS` / `VIEW_FINANCES`. - **Impact / exploit scenario:** A manager/staff member handling a delivered order cannot pull its invoice for the buyer, forcing owner intervention for routin
- **Fix:** Admit members under `STORE_ACTION_VIEW_FINANCES` (or `MANAGE_ORDERS`) in `get_or_issue_invoice`, mirroring the other seller surfaces.

### LO-78 — Courier gateway errors are returned verbatim as 502 bodies, and raw courier status strings are exposed on public tracking

- **Severity:** Low | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/shipments.rs:282-300`
- **Found by:** agent-09 (L5)
- **Impact:** Upstream courier error bodies (which may contain internal codes, account hints or request echoes) are passed to the caller unchanged; raw courier strings on the unauthenticated tracking view leak courier-internal vocabulary and, if a courier ever echoes an identifier, that too. Low likelihood, but the API should be the sanitising boundary. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/shipments.rs:282-300` (`courier_error_to_api`), `crates/api/src/tracking.rs:169-190` - **Code evidence:** CourierError::Gateway(message) => ApiError::new(StatusCode::BAD_GATEWAY, message) // … and tracking exposes the courier's raw string per event: "SELECT status, raw_status, created_at FROM shipment_events …" // → TrackedShipmentEvent { status, note: raw_status, … } - **Impact / exploit scenario:** Upstream courier error bodies (which may contain internal codes, account hints or request echoes) are passed to the caller
- **Fix:** Map gateway failures to a fixed generic 502 message and log the detail server-side; map `raw_status` to a display-safe enum/string (or drop `note` from the public tracking view).

### LO-79 — Phone comparison on the tracking gate is not constant-time

- **Severity:** Low | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/tracking.rs:132`
- **Found by:** agent-09 (L7)
- **Impact:** A timing oracle for the delivery phone is theoretically possible (earlier mismatch = faster response). In practice network jitter makes this very hard to exploit, and the phone space is the bigger issue (M5). -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/tracking.rs:132` and `crates/domain/src/lib.rs:2389-2398` - **Code evidence:** `phones_match_for_tracking` ends in `stored == supplied` on normalised `String`s — a short-circuiting equality comparison. - **Impact / exploit scenario:** A timing oracle for the delivery phone is theoretically possible (earlier mismatch = faster response). In practice network jitter makes this very hard to exploit, and the phone space is the bigger issue (M5). - **Fix:** Compare with a constant-time equality (e.g. `subtle::ConstantTimeEq`) once both side
- **Fix:** Compare with a constant-time equality (e.g. `subtle::ConstantTimeEq`) once both sides are normalised; keep the rate limiting from M5 as the primary defence.

### LO-80 — `SUB-<8 hex chars>` courier invoice reference can collide across sub-orders

- **Severity:** Low | **Area:** Returns / Shipments / Tracking / Invoices / Exports | **File:** `crates/api/src/shipments.rs:686`
- **Found by:** agent-09 (L8)
- **Impact:** Two parcels booked under the same courier invoice reference can be conflated in courier-side lookups / reconciliation, or a booking rejected as a duplicate. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/shipments.rs:686` - **Code evidence:** `invoice: format!("SUB-{}", &sub_order_id.simple().to_string()[..8])` — only the first 32 bits of the sub-order UUID are sent to the courier as the invoice number. Steadfast requires this invoice value; two sub-orders can share a 32-bit prefix (birthday bound ≈ 77k sub-orders for a 50% collision chance). - **Impact / exploit scenario:** Two parcels booked under the same courier invoice reference can be conflated in courier-side lookups / reconciliation, or a booking rejected as a duplicate. -
- **Fix:** Send the full sub-order UUID (or `SUB-<full-uuid>`) where the courier accepts it; if a courier caps invoice length, use a longer prefix (≥ 12 hex chars) plus a per-store sequence. ---

### LO-81 — `update_reply` does not constrain the UPDATE to the caller's store (defence-in-depth gap)

- **Severity:** Low | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/reviews.rs:422-445`
- **Found by:** agent-08 (?)
- **Impact:** Currently not directly exploitable on its own; it is a missing second check on a privileged write. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/reviews.rs:422-445` (`update_reply`) - **Code evidence:** // reviews.rs:422-442: validate_reply_body(body).map_err(ApiError::bad_request)?; require_review_store(pool, user, review_id).await?; // checks caller owns the product's store sqlx::query_as( "UPDATE review_replies SET body = $2, updated_at = now() WHERE review_id = $1 RETURNING id, review_id, store_id, body, created_at, updated_at") .bind(review_id).bind(body.trim()) ... The UPDATE filters only on `review_id`, not `AND store_id = $3`. Today the
- **Fix:** Add `AND store_id = $3` (bind the `store_id` returned by `require_review_store`) to the UPDATE, and return 404 when it matches nothing.

### LO-82 — `set_question_hidden` is a public, unauthenticated helper; hidden questions can still be answered

- **Severity:** Low | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/qna.rs:358-379`
- **Found by:** agent-08 (?)
- **Impact:** Defence-in-depth / logic gap rather than a live exploit through the current routes; the hidden-answer case wastes effort and can confuse a later unhide (a spam question reappears pre-answered). -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/qna.rs:358-379` (`pub async fn set_question_hidden`), `crates/api/src/qna.rs:329-356` (`answer_question`) - **Code evidence:** `set_question_hidden` is `pub` and performs the flip with no authorisation check of its own — every caller must remember to guard it (the two handlers do, qna.rs:566-586, but a future caller may not). Separately, `answer_question` calls `require_question_store` and then unconditionally `UPDATE product_questions SET answer_body = ...` — it never checks `is_hidden`, so a seller can answer a question an admi
- **Fix:** Make `set_question_hidden` private (or take an authorisation proof / actor parameter and audit inside it); in `answer_question`, refuse (409) or auto-unhide-with-audit when the question is hidden.

### LO-83 — Store owner can open a conversation with their own store; conversations can target draft/archived products

- **Severity:** Low | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/messaging.rs:348-368`
- **Found by:** agent-08 (?); agent-18 (L2)
- **Impact:** Self-chat pollutes the inbox, can trigger SIDRA notifications to the owner's own store, and (via a draft product id) confirms the existence/title of a non-public product through the conversation view's `product_title`. Low impact, but it is a missing invariant that also feeds M-07's duplicate-thread confusion. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/messaging.rs:348-368` (`find_or_create_conversation`) - **Code evidence:** // messaging.rs:348-364 — no buyer != owner check; product checked with get_product_by_id: let store = get_store_by_id(pool, store_id).await?; if store.status != "active" { return Err(ApiError::not_found("store not found")); } if let Some(product_id) = product_id { let product = get_product_by_id(pool, product_id).await?; // not get_public_product if product.store_id != store_id { return Err(ApiError::bad_request("product does not belong to th
- **Fix:** Reject `user.user_id == store.owner_user_id` (400) in `find_or_create_conversation`, and use `get_public_product` for the product check (or explicitly allow the seller's own draft only for the seller side, documented).

### LO-84 — `insert_message` is non-atomic, and the read marker is stamped before messages are loaded

- **Severity:** Low | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/messaging.rs:465-503`
- **Found by:** agent-08 (?)
- **Impact:** Stale ordering/unread counts and silently "read" messages; a reliability/UX bug rather than a security hole. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/messaging.rs:465-503` (`insert_message`), `crates/api/src/messaging.rs:421-463` (`read_conversation_thread`) - **Code evidence:** // messaging.rs:473-500: INSERT message, then a separate UPDATE conversations ... // messaging.rs:430-448: read path stamps the marker FIRST, then loads messages: sqlx::query(&format!("UPDATE conversations SET {marker_column} = now() WHERE id = $1")) ... let messages: Vec<Message> = sqlx::query_as("SELECT ... FROM messages WHERE conversation_id = $1 ORDER BY created_at DESC, id DESC LIMIT $2 ..
- **Fix:** Put both statements of `insert_message` in one transaction (see H-04); in `read_conversation_thread`, capture `now()` first, load messages, then stamp the marker to that captured timestamp (or stamp `LEAST(now(), oldest-unseen)` semantics).

### LO-85 — SIDRA suggestion queue/status are owner-only via `get_store_by_owner` (LIMIT 1): multi-store owners and staff are locked out or see the wrong store

- **Severity:** Low | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/messaging.rs:777-794`
- **Found by:** agent-08 (?)
- **Impact:** (a) An owner of two stores sees suggestions for only one store; the other's AI replies sit pending forever. (b) Manager/staff who can answer questions (qna.rs:301) get a 404 ("you do not have a store yet") from the SIDRA queue/status endpoints even though they work the store. (c) Inconsistency between who can enable auto-reply and who can review its output. …
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/messaging.rs:777-794` (`list_pending_suggestions`), `messaging.rs:796-815` (`load_suggestion_for_owner`), `messaging.rs:895-907` (`sidra_status`); `crates/api/src/stores.rs:79-82` (`SELECT_STORE_BY_OWNER_SQL ... ORDER BY created_at ASC LIMIT 1`) - **Code evidence:** All three functions call `get_store_by_owner`, which returns only the *earliest* store a user owns. Q&A, by contrast, spans all accessible stores via `accessible_store_ids` (qna.rs:422-450), and the SIDRA auto-reply toggle itself uses the scoped `resolve_scoped_store`
- **Fix:** Scope the suggestion queue/status by `accessible_store_ids` with the SIDRA / answer-questions action (or a dedicated `manage_sidra` action), mirroring `list_seller_questions`; add a `?store_id=` scope like the toggle.

### LO-86 — Thread history is silently truncated at 200 messages with no pagination; messages cannot be edited or deleted

- **Severity:** Low | **Area:** Reviews / Q&A / Wishlist / Messaging | **File:** `crates/api/src/messaging.rs:70`
- **Found by:** agent-08 (?)
- **Impact:** Long support threads lose their early history, and a user who pastes a password, address, or payment detail into chat cannot retract it (same retraction gap as M-05 for Q&A, lower stakes here because threads are private to the two parties rather than public). -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/messaging.rs:70` (`pub const THREAD_MESSAGE_LIMIT: i64 = 200`), `messaging.rs:421-463` (`read_conversation_thread`); absence of edit/delete handlers in `messaging.rs` / route table `lib.rs:1453-1494` - **Code evidence:** The thread query takes the newest 200 and re-sorts oldest-first; there is no `before` cursor, page, or "has more" flag, so older history is unreachable through the API. No message edit/delete endpoint exists. - **Impact / exploit:** Long support threads lose their early history, and a user who pastes a password,
- **Fix:** Add cursor pagination (`before=<message_id>`, `has_more`) to the thread read, and add owner-only message delete (and a short edit window) with a "message deleted" tombstone so the other side is not silently confused. ---

### LO-87 — Service functions perform no authorization themselves; every check lives only in the handler

- **Severity:** Low | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/stores.rs:432-529`
- **Found by:** agent-06 (?)
- **Impact:** The next caller (a new handler, a worker, a test) that calls one of these `pub` functions without repeating the gate silently bypasses authorization. The `access.rs` doc comment even anticipates passing a transaction for "guarded writes that check access under their row locks" — no in-scope write does that. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/stores.rs:432-529` (`add_store_member`, `update_store_member_role`, `remove_store_member`); `crates/api/src/verification.rs:363-491` (`approve/reject_verification`); `crates/api/src/admin.rs:622,763` (`transition_payout`, `set_user_admin`) - **Code evidence:** e.g. `update_store_member_role(pool, store_id, member_user_id, role)` validates the role and writes, with no caller/role parameter at all; `approve_verification(pool, admin_id, …)` trusts the `admin_id` it is handed. All current HTTP handlers do gate correctly (verified by
- **Fix:** Pass the acting user/role into the service functions and re-check the gate there (or mark the functions `pub(crate)` and keep the single handler as the only entry point), and add a test that calls each service function directly with a non-owner / non-admin and expects failure.

### LO-88 — Member add / role change / remove write no audit row (accountability gap)

- **Severity:** Low | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/stores.rs:432-529`
- **Found by:** agent-06 (?)
- **Impact:** A compromised owner account seats a sockpuppet manager; the owner later sees an unfamiliar member but cannot tell when they were seated, by whom, or what their role history was. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/stores.rs:432-529` - **Code evidence:** No audit/notifications call in any of the three member mutations, while the codebase has an established audit pattern (`record_admin_action`, `audit.rs`) and even audits verification decisions. Member changes are the store-level equivalent of admin-flag changes (H-4): they grant persistent access to a store's money-adjacent surfaces (`view_finances`, orders, catalogue). - **Impact:** A compromised owner account seats a sockpuppet manager; the owner later sees an unfamiliar member but cannot t
- **Fix:** Append member-mutation events (actor, store, target user, old/new role, timestamp) to an audit log visible to the owner (and to admins).

### LO-89 — `add_store_member` check-then-insert race turns a concurrent duplicate into a 500

- **Severity:** Low | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/stores.rs:469-487`
- **Found by:** agent-06 (?)
- **Impact:** Minor — a spurious 500 on a legitimate concurrent double-add, and a 500 that can leak (via timing/error text) that a constraint fired. No data corruption: the PK holds. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/stores.rs:469-487` - **Code evidence:** let existing: Option<String> = sqlx::query_scalar( "SELECT role FROM store_members WHERE store_id = $1 AND user_id = $2")...; if existing.is_some() { return Err(ApiError::conflict("this user is already a member...")); } sqlx::query("INSERT INTO store_members (store_id, user_id, role) VALUES ($1, $2, $3)") ... .map_err(|e| ApiError::internal("add store member", e))?; The composite PK `(store_id, user_id)` (migration `0032`) is the only thing stopping a duplicate; two concurre
- **Fix:** Map SQLSTATE 23505 on this insert to the same 409 as the pre-check (as `create_store_for_user` already does for duplicate slugs, `stores.rs:120-134`), or do the check-and-insert in one transaction with a row lock.

### LO-90 — Store creation is not atomic: store row and seller-role upgrade are two separate statements

- **Severity:** Low | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/stores.rs:105-131`
- **Found by:** agent-06 (?)
- **Impact:** If the `UPDATE` fails (or the process dies between the two statements), the store exists but the owner is still a `buyer` in the DB — an inconsistent state the rest of the code does not expect (several surfaces key off role as well as ownership). The reverse order has the same hazard. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/stores.rs:105-131` - **Code evidence:** let store: Store = sqlx::query_as(INSERT_STORE_SQL)...fetch_one(pool).await...?; // commits sqlx::query("UPDATE users SET role = 'seller' WHERE id = $1 AND role = 'buyer'") .bind(owner.user_id).execute(pool).await...?; // separate statement, no tx - **Impact:** If the `UPDATE` fails (or the process dies between the two statements), the store exists but the owner is still a `buyer` in the DB — an inconsistent state the rest of the code does not expect (several surfaces key off role
- **Fix:** Wrap both statements in one transaction.

### LO-91 — `resolve_scoped_store`'s default-store path skips the role gate (and an unknown action passes there but 403s with `?store_id=`)

- **Severity:** Low | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/access.rs:66-89`
- **Found by:** agent-06 (?)
- **Impact:** Today every caller passes a known action and owners pass every known action, so there is no live exploit; but the two branches disagree, and a future caller that passes a mistyped/unknown action would silently succeed on the default-store path while correctly 403ing on the explicit-store path. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/access.rs:66-89` - **Code evidence:** None => { let store = get_store_by_owner(pool, user_id).await?; Ok((store, "owner".to_string())) } // no require_role / store_role_can call // vs. Some(store_id) => require_store_action(pool, user_id, store_id, action).await? `store_role_can("owner", action)` is `STORE_ACTIONS.contains(&action)` — an unknown action fails — but the `None` branch never calls it. - **Impact:** Today every caller passes a known action and owners pass every known action, so there is no live explo
- **Fix:** Call `require_role("owner", action)?` in the `None` branch too, so both paths go through the same gate.

### LO-92 — Admin read endpoints expose full phones and payout account numbers with no audit, and `list_orders` is N+1

- **Severity:** Low | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/admin.rs:369-381`
- **Found by:** agent-06 (?)
- **Impact:** Admin-only by design, so this is not an unauthorized-disclosure finding — but it widens the blast radius of any compromised admin session (full bKash/Nagad numbers and bank account numbers for every payout, full phone for every user) with no read-audit to detect bulk export, and the N+1 order load is a ready-made slow endpoint (100 ids × several queries each). -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/admin.rs:369-381` (`AdminStore.owner_phone: String`), `:544-559` (`AdminPayout` — `owner_phone`, `account_number` in full), `:561-568` (`AdminUser.phone` in full), `:496-543` (`list_orders` — `load_order` per id in a loop) - **Code evidence:** `AdminPayout` selects `p.account_number` unmasked; `AdminStore` / `AdminUser` select `u.phone` unmasked. `list_orders` fetches up to 100 ids then calls `load_order(pool, id)` once per id. - **Impact:** Admin-only by design, so this is not an unauthorized-disclosure finding — but it widens the
- **Fix:** Mask phones/account numbers in list views (full values only on a single-record view, audited); batch-load orders with a single query instead of one `load_order` per id.

### LO-93 — Public store view leaks the owner's internal user id; admin store-status changes need no reason

- **Severity:** Low | **Area:** Stores / Members / Verification / Admin | **File:** `crates/api/src/stores.rs:46-70`
- **Found by:** agent-06 (?)
- **Impact:** `owner_user_id` lets anyone enumerate store → owner-user-id mappings (useful for correlating accounts across the platform's public surfaces). Suspension without a recorded reason makes a wrongful suspension undiscoverable and unappealable from the log. -
- **Evidence:** - **Severity:** Low - **File:line:** `crates/api/src/stores.rs:46-70` (`Store.owner_user_id`), handler `:284-291` (`get_store`); `crates/api/src/admin.rs:442-494` (`set_store_status`) - **Code evidence:** // stores.rs:46-70 — Store is both the DB row and the public JSON shape pub struct Store { pub id: Uuid, pub owner_user_id: Uuid, pub slug: String, ... } pub async fn get_store(State(state), Path(slug)) -> ... { Ok(Json(get_store_by_slug(pool, &slug).await?)) } // admin.rs:442-455 — any admin can suspend/reactivate with no note if status != "active" && status != "suspende
- **Fix:** Strip `owner_user_id` from the public `Store` JSON (use a separate internal row type); require a short reason on `set_store_status`, store it in the audit detail, and return 409 when the store already has the requested status. ---

### LO-94 — ~150 lines of helpers are copy-pasted into all 36 files; they have already drifted

- **Severity:** Low | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave14.rs:69`
- **Found by:** agent-16 (L1)
- **Impact:** A fix to the shared `login()`/`call()` helper (e.g. to send a new required header) must be applied 36 times; drift means files silently test slightly different things. -
- **Evidence:** The files themselves note it: "The small helpers are copied from the Wave 4 test on purpose: integration test files stay self-contained rather than sharing test code across files" (wave5.rs:5-8, repeated in most headers). -
- **Fix:** Extract one `tests/common.rs` (or a `test-support` crate) with `call`/`login`/`create_store` and use it everywhere; keep the per-wave fixtures local.

### LO-95 — Some money assertions are `contains`-only on formatted strings, not exact values

- **Severity:** Low | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave29.rs:292`
- **Found by:** agent-16 (L2)
- **Impact:** A template regression that moves the amount into the wrong sentence, or renders the wrong order's amount, passes. -
- **Evidence:** A body of `"৳2060"` anywhere (even in the wrong field or duplicated) satisfies the assertion; the full rendered template is never asserted exactly for any kind except by eyeballing. -
- **Fix:** Assert the exact rendered body for at least one fixture per notification kind (the templates are short, deterministic strings).

### LO-96 — Wave 32 index test asserts index *names* only, not that the index covers the right columns

- **Severity:** Low | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave32.rs:256-272`
- **Found by:** agent-16 (L3)
- **Impact:** The performance fix could silently regress (planner stops using the index) with the suite green. -
- **Evidence:** An index with the right name but the wrong columns (or a duplicate index with the same name on a different expression) passes; no `EXPLAIN` is ever run. -
- **Fix:** Assert `pg_indexes.indexdef` contains the expected column list, or run `EXPLAIN` on the four query shapes and assert an index scan is chosen.

### LO-97 — No test for unknown routes (404) or wrong-method (405) handling

- **Severity:** Low | **Area:** Tests / Coverage | **File:** `crates/a`
- **Found by:** agent-16 (L4)
- **Impact:** A routing misconfiguration (e.g. a handler mounted on the wrong method, or a fallback that returns 200 for unknown paths) is invisible; error-body shape for 404/405 is unpinned. -
- **Evidence:** `grep -rn "/v1/banana|/v1/nonexistent|METHOD_NOT_ALLOWED|405" crates/api/tests/` → 0 hits. Every test calls a route that exists with the method it expects. -
- **Fix:** Add a small test: `GET /v1/definitely-not-a-route` → 404 with the standard error body; `DELETE /v1/products` (collection with wrong method) → 405.

### LO-98 — Wave 20 pins `http://localhost:3000` as an allowed CORS origin with no test that production does not ship it

- **Severity:** Low | **Area:** Tests / Coverage | **File:** `crates/api/tests/wave20.rs:119-126`
- **Found by:** agent-16 (L5)
- **Impact:** If the deployed config ever falls back to the default list, any site running on the user's localhost (or any attacker page that can get the victim's browser to use that origin) can call the API cross-origin. -
- **Evidence:** The default origin list contains the local dev origin; the test proves it is allowed and that an explicit list replaces it — but nothing proves the *production* configuration (from env) excludes `localhost:3000`. -
- **Fix:** Add a test (or a CI check) asserting `AppState::from_env` with production env does not include `http://localhost:3000` in its CORS origins.

### LO-99 — Notify `sender_from_env` is tested only for the boolean, never for the env wiring

- **Severity:** Low | **Area:** Tests / Coverage | **File:** `crates/notify/tests/outbox.rs:168-180`
- **Found by:** agent-16 (L6)
- **Impact:** OTPs could be "sent" via the log sender while outbox notifications fail with `sms_not_configured`, or vice versa — with no test failing. -
- **Evidence:** The function takes a bool, so the test proves the branch, but nothing proves the API and the worker actually pass the *same* bool (the lib doc claims `sender_from_env` "is the ONE selection rule both the API (OTP codes) and the worker (outbox) use, so the two can never disagree" — notify lib.rs header). A caller passing `true` in one binary and `false` in the other would silently split behaviour. -
- **Fix:** Add a test asserting both call sites (`crates/api/src/lib.rs` `from_env` and the worker's startup) resolve the sender from the same env var/value, or centralise the resolution so there is only one call site to test. ---

## Dependency / config / infra issues (summary)

Full entries for these are in the list above (agent-14 area, plus agent-02/12 where noted). This section pulls them together because they are fixed in config/dependencies, not in handler code.

| Issue | Where | Fix |
|---|---|---|
| `jsonwebtoken` 9.3.1 — CVE-2026-25537 type-confusion claim-validation bypass | `Cargo.lock`, `crates/api/Cargo.toml` (`jsonwebtoken = "9"`) | Upgrade to `>= 10.3.0` (v10 needs an explicit crypto-backend feature: `rust_crypto` or `aws_lc_rs`); add every validated claim to `required_spec_claims` in the meantime. |
| `rsa` 0.9.10 — Marvin timing attack (RUSTSEC-2023-0071), no fixed release; plus panic advisory RUSTSEC-2026-0583 | `Cargo.lock` | Remove/replace the `rsa` dependency (no patched version exists). |
| `libsqlite3-sys` 0.30.1 — bundled SQLite < 3.54.0, uninitialised page-cache memory (RUSTSEC-2026-0459 / CVE-2026-119375) | `Cargo.lock` | Fix unreachable on sqlx 0.8 — track sqlx upgrade; remove unused sqlite driver from the graph. |
| `chrono` 0.4.45 unmaintained (RUSTSEC-2026-0543) | `Cargo.lock` | Plan migration off `chrono` (no patch exists). |
| Production compose boots with public DB password `marketplace_dev_only` + dev-mode defaults | `docker-compose.yml:27,49,65`, `.env.example:27-28` | Generate real secrets per environment; never commit defaults; fail startup when defaults are detected. |
| Plaintext, unencrypted backups, no off-site copy; `restore.sh --yes` is a one-command prod wipe with no target guard | `scripts/backup.sh`, `scripts/restore.sh` | Encrypt backups, verify checksums on restore, require explicit target + pre-restore backup, add off-site copy. |
| No supply-chain scanning in CI (no `cargo audit`/`deny`, image scan, SBOM, Dependabot) | `.github/workflows/ci.yml` | Add `cargo audit` + `cargo deny` to CI, pin actions by hash, add `permissions:` blocks, scan images. |
| Unpinned images (`postgres:16`, `minio/minio:latest`, `debian:bookworm-slim`), no digests; no `--locked` builds; CI floats on `rust-toolchain@stable` | `Dockerfile.*`, `docker-compose.yml`, `infra/docker-compose.yml`, CI | Pin images by digest, build with `--locked`, pin the toolchain. |
| `.dockerignore` missing `backups/` and `.env.*` — plaintext DB dumps enter the Docker build context | `.dockerignore` | Exclude `backups/`, `.env*`, and secrets from build context. |
| Postgres/Redis/MinIO published with no auth in compose; no restart/healthcheck/resource limits/`cap_drop`/`read_only` | `docker-compose.yml`, `infra/docker-compose.yml` | Bind data services to internal networks only, add auth, healthchecks, limits, drop capabilities. |

## What was NOT covered

- **Frontend (`apps/web/`) — explicitly excluded by the user.** No `.tsx`/`.ts` UI code, CSS, or browser behaviour was audited. Any frontend vulnerability (XSS in rendering, client-side price trust, token storage in the browser) is still unknown.
- **Live/dynamic testing.** This was a static code audit. No running instance was attacked, no fuzzing, no load/race reproduction, and no dependency scan was executed beyond reading `Cargo.lock` (agents cited advisories from the lockfile versions; run `cargo audit` to confirm).
- **Production data, real gateway credentials, and the live deployment** were not available and not examined.
- **Test-suite findings (agent-16) are about the safety net, not the product:** the suites share one database while CI runs them in parallel, all 37 files silently pass when `DATABASE_URL` is unset, and several security behaviours (OTP lockout, JWT tampering, `dev_otp` absence, concurrency) have no tests at all — so a green CI run currently proves little.

## Source reports

- `agent-01-auth-otp-sessions.md`
- `agent-02-payments-money.md`
- `agent-03-orders-cart-checkout-races.md`
- `agent-04-payouts-subscriptions-loyalty.md`
- `agent-05-products-variants-inventory-search.md`
- `agent-06-stores-members-verification-admin.md`
- `agent-07-coupons-promotions-abuse.md`
- `agent-08-reviews-qna-wishlist-messaging.md`
- `agent-09-returns-shipments-tracking-invoices-exports.md`
- `agent-10-notifications-notify-worker.md`
- `agent-11-facebook-apikeys-risk-audit.md`
- `agent-12-domain-courier-payments-core.md`
- `agent-13-migrations-schema-sql.md`
- `agent-14-docker-ci-infra-secrets.md`
- `agent-15-api-surface-routing.md`
- `agent-16-tests-coverage-gaps.md`
- `agent-17-money-math-currency.md`
- `agent-18-idor-authorization-crosscutting.md`
- `agent-19-input-validation-ssrf-uploads.md`
- `agent-20-concurrency-transactions-toctou.md`
