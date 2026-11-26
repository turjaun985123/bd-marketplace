-- Wave 36: seller subscription plans.
--
-- A store sits on exactly one plan at a time, so the current
-- subscription is ONE row per store (`store_id` UNIQUE): the
-- row says which plan, whether the current period is live
-- (`status`), when it started and when it ends, and whether
-- the worker should renew it from the seller's earnings
-- balance when it ends (`auto_renew`).
--
-- `status` vocabulary:
-- - 'active': a paid period is running (or paid for and not
--   yet swept past its end). Plan benefits apply while the
--   row is `active` AND `expires_at` is still in the future —
--   the activity rule the API applies everywhere; the stored
--   status alone never grants a benefit past its paid end.
-- - 'cancelled': the seller moved to the free plan (effective
--   immediately, no refund) — the row remains as the record
--   that a paid plan was given up early.
-- - 'expired': the period ended and the worker's renewal
--   sweep could not (or, with `auto_renew` off, would not)
--   charge for another one. The store falls back to the free
--   plan until the seller subscribes again.
--
-- Plans are prepaid from the seller's earnings balance
-- exactly like promotions (Wave 26): every purchase and
-- renewal writes a `seller_charges` row, so the one earnings
-- aggregate subtracts plan spend from the same available
-- balance that gates payouts. That is why the `kind` CHECK
-- on `seller_charges` widens here from ('promotion') to
-- ('promotion', 'subscription'); a subscription charge's
-- `ref_id` points at the store_subscriptions row it paid
-- for. There is no external payment rail for platform
-- billing: a seller with no earnings cannot buy a plan.
--
-- The free plan has no row of its own in the normal case: a
-- store with no `store_subscriptions` row (or a lapsed one)
-- IS on the free plan. A row appears when the seller first
-- buys a paid plan (or explicitly switches back to free,
-- which marks it 'cancelled').

CREATE TABLE store_subscriptions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    store_id uuid NOT NULL UNIQUE REFERENCES stores (id) ON DELETE CASCADE,
    plan text NOT NULL CHECK (plan IN ('free', 'pro', 'business')),
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'cancelled', 'expired')),
    started_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    auto_renew boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Serves the worker's renewal sweep (live rows whose period
-- has ended) and the per-store activity lookups at checkout.
CREATE INDEX store_subscriptions_status_expiry_idx
    ON store_subscriptions (status, expires_at);

-- Widen the charges ledger to subscription spend (see the
-- header). The constraint was created inline in migration
-- 0027, so Postgres named it after the table and column.
ALTER TABLE seller_charges
    DROP CONSTRAINT seller_charges_kind_check;
ALTER TABLE seller_charges
    ADD CONSTRAINT seller_charges_kind_check
    CHECK (kind IN ('promotion', 'subscription'));
