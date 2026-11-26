-- Wave 35: buyer loyalty points — an append-only ledger.
-- A buyer earns 1 point per 100 taka (10_000 poisha) of NET
-- goods (goods minus the sub-order's coupon discount share) on
-- each sub-order that reaches `delivered`, and may redeem
-- points at checkout at 1 point = 100 poisha, capped at half
-- the order's post-coupon goods total. The platform funds the
-- redemption: seller payout snapshots and the commission basis
-- never see it (see the API `loyalty` module).
--
-- The ledger is the balance: there is no stored counter to
-- drift. `delta` is signed — positive for `earn` and for
-- refund `adjust` rows, negative for `redeem` and clawback
-- `adjust` rows. `ref_id` names the row the movement belongs
-- to: the SUB-ORDER for earn / sub-order adjustments, the
-- ORDER for a checkout redemption. Earn rows carry
-- `expires_at` (earn time + 365 days); expiry is computed at
-- read time, no sweeper rewrites history.
--
-- Idempotency: exactly one earn row may exist per sub-order
-- (the partial UNIQUE index below; the earn inserts run
-- ON CONFLICT DO NOTHING). Reversal rows (refund on cancel,
-- clawback on return-received) deliberately have NO unique
-- index — they are guarded in code instead: each is written
-- inside the transaction of a ONE-WAY sub-order transition
-- (placed/confirmed -> cancelled, delivered -> returned via
-- an approved return), and a one-way transition can commit
-- at most once, so a reversal can be attempted at most once.

CREATE TABLE loyalty_points (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL REFERENCES users (id),
    delta int NOT NULL,
    reason text NOT NULL CHECK (reason IN ('earn', 'redeem', 'adjust')),
    ref_id uuid,
    expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX loyalty_points_user_idx
    ON loyalty_points (user_id, created_at DESC);

CREATE UNIQUE INDEX loyalty_points_earn_once_idx
    ON loyalty_points (ref_id) WHERE reason = 'earn';

-- The checkout snapshot: how many points the buyer spent on
-- this order and the discount they bought. The grand total
-- stored on the order is already net of this discount.
ALTER TABLE orders
    ADD COLUMN loyalty_points_used int NOT NULL DEFAULT 0
        CHECK (loyalty_points_used >= 0),
    ADD COLUMN loyalty_discount_poisha bigint NOT NULL DEFAULT 0
        CHECK (loyalty_discount_poisha >= 0);

-- Each sub-order's allocated share of the order's loyalty
-- discount (allocated proportionally to post-coupon goods by
-- the domain `allocate_discount`): the audit trail for the
-- order-level figure, and the basis for the per-sub-order
-- point refund when a single sub-order is cancelled.
ALTER TABLE sub_orders
    ADD COLUMN loyalty_discount_poisha bigint NOT NULL DEFAULT 0
        CHECK (loyalty_discount_poisha >= 0);
