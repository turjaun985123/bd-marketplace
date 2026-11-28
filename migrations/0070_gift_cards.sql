-- Wave 60: gift cards — store-scoped stored value.
--
-- A gift card is a BEARER instrument: the code is the
-- credential (the Wave 37 API-key shape). Only the SHA-256
-- hash of the normalised code is stored; the full code is
-- shown exactly once at issuance. `code_prefix` (first 12
-- characters of the issued code) and `last4` are display
-- data so lists and support can name a card without ever
-- being able to spend it.
--
-- STORE-scoped only: every card belongs to one store and
-- redeems only against that store's sub-order. Issuance is
-- promotional / manual (seller or admin) — no money is
-- collected at issuance, so redemption is SELLER-FUNDED
-- and nets out of that sub-order's payout snapshot exactly
-- like a store coupon (see the domain Wave 60 section).
--
-- The balance is a CACHED column kept consistent with the
-- append-only ledger in the same transaction as every
-- movement (the Wave 35 loyalty ledger is the precedent):
-- the ledger is the audit trail, the column is what
-- checkout locks and reads. Tests pin that the ledger sums
-- to the cached balance at every step.
--
-- Expiry is TIME-DERIVED at read (expires_at compared to
-- now, the loyalty precedent) — there is deliberately no
-- status column to flip. Disabling IS a stored fact
-- (disabled_at) because a human did it and can undo it.

CREATE TABLE gift_cards (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    store_id uuid NOT NULL REFERENCES stores (id),
    -- SHA-256 hex of the normalised code; unique because a
    -- code identifies exactly one card.
    code_hash text NOT NULL UNIQUE,
    -- Display-only fragments of the issued code.
    code_prefix text NOT NULL,
    last4 text NOT NULL,
    initial_amount_poisha bigint NOT NULL
        CHECK (initial_amount_poisha > 0),
    balance_poisha bigint NOT NULL
        CHECK (balance_poisha >= 0),
    -- Optional issuer note ("ঈদ মোবারক") and recipient
    -- name, printed on the card the seller hands over.
    note text,
    recipient_name text,
    issued_by_user_id uuid NOT NULL REFERENCES users (id),
    -- NULL = never expires.
    expires_at timestamptz,
    -- NULL = not disabled. Set/cleared by the owning
    -- store (owner-level) or an admin (audited).
    disabled_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX gift_cards_store_idx
    ON gift_cards (store_id, created_at DESC);
CREATE INDEX gift_cards_prefix_idx
    ON gift_cards (code_prefix);

-- Every balance movement, append-only. delta_poisha is
-- SIGNED: positive when value enters the card (issue,
-- restores, positive admin adjustments), negative when it
-- leaves (redeem, negative adjustments).
-- balance_after_poisha snapshots the card balance right
-- after the movement so the ledger alone reconstructs the
-- balance history; the API asserts the running sum in
-- tests against the cached column.
CREATE TABLE gift_card_ledger (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    gift_card_id uuid NOT NULL REFERENCES gift_cards (id),
    kind text NOT NULL CHECK (kind IN (
        'issue', 'redeem', 'cancel_restore', 'return_restore',
        'admin_adjust'
    )),
    delta_poisha bigint NOT NULL CHECK (delta_poisha <> 0),
    balance_after_poisha bigint NOT NULL
        CHECK (balance_after_poisha >= 0),
    -- The order / sub-order a redeem or restore belongs
    -- to; NULL for issue and admin adjustments.
    order_id uuid REFERENCES orders (id),
    sub_order_id uuid REFERENCES sub_orders (id),
    -- Who caused the movement: the issuer, the buyer (via
    -- checkout/cancel/return funnels), or the admin.
    actor_user_id uuid REFERENCES users (id),
    note text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX gift_card_ledger_card_idx
    ON gift_card_ledger (gift_card_id, created_at DESC);
CREATE INDEX gift_card_ledger_sub_order_idx
    ON gift_card_ledger (sub_order_id)
    WHERE sub_order_id IS NOT NULL;

-- The checkout snapshots. The order carries the card it
-- used (at most one — a checkout accepts one code), the
-- display tail for the receipt, and the total redeemed;
-- the funded sub-order carries its own redemption share
-- (the only non-zero one) so the seller payout snapshot,
-- the courier COD computation and the return-restore rule
-- all read it without joining back to the order.
ALTER TABLE orders
    ADD COLUMN gift_card_id uuid REFERENCES gift_cards (id),
    ADD COLUMN gift_card_last4 text,
    ADD COLUMN gift_card_discount_poisha bigint NOT NULL DEFAULT 0
        CHECK (gift_card_discount_poisha >= 0);

ALTER TABLE sub_orders
    ADD COLUMN gift_card_id uuid REFERENCES gift_cards (id),
    ADD COLUMN gift_card_discount_poisha bigint NOT NULL DEFAULT 0
        CHECK (gift_card_discount_poisha >= 0);
