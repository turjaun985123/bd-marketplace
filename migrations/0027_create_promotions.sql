-- Wave 26: sponsored product promotions + seller charges.
--
-- A promotion is a prepaid sponsored window for ONE product:
-- the seller buys a whole number of days (1..=30, the domain
-- cap) at the platform's per-day price, and while the window
-- is live the product carries the `is_promoted` flag in the
-- shared product view and sorts first in the default and
-- relevance listings (labelled "স্পন্সরড" in the UI — explicit
-- price and rating sorts stay pure). The window is stored,
-- never recomputed from the charge, so a later price change
-- never moves an already-bought promotion's end.
--
-- `status` is the seller's word: 'active' until the seller
-- cancels, then 'cancelled'. Expiry is NOT a status — a row
-- whose `ends_at` has passed is simply out of its window (the
-- domain `promotion_is_active` rule decides, over the stored
-- timestamps). Cancelling stops the placement immediately
-- and refunds NOTHING: the charge stands. That no-refund
-- policy is stated in the seller UI before purchase and in
-- the README.
--
-- seller_charges is the balance deduction ledger: every
-- poisha the platform takes from a seller's earnings balance
-- outside the payout flow lands here as one row. Wave 26 has
-- exactly one kind, 'promotion', with `ref_id` pointing at
-- the promotion it paid for. The seller earnings aggregate
-- (the ONE balance function in the `payouts` module)
-- subtracts the sum of these rows, so a promotion is paid
-- exactly once and can never be spent twice — the same
-- available balance gates both promotions and payouts.
--
-- Charges belong to the SELLER (user), not to one store: a
-- promotion may target a product in any of the seller's
-- stores, while money settles through the seller's one
-- payout store — so the deduction is recorded against the
-- user and the earnings aggregate subtracts the owner's
-- charges whichever of their stores it is summarising.

CREATE TABLE promotions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE,
    seller_user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    starts_at timestamptz NOT NULL,
    ends_at timestamptz NOT NULL,
    days integer NOT NULL CHECK (days >= 1),
    cost_poisha bigint NOT NULL CHECK (cost_poisha > 0),
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'cancelled')),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Serves the overlap check (one live window per product) and
-- the `is_promoted` EXISTS in the shared product view.
CREATE INDEX promotions_product_window_idx ON promotions (product_id, ends_at);

CREATE TABLE seller_charges (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    seller_user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    amount_poisha bigint NOT NULL CHECK (amount_poisha > 0),
    kind text NOT NULL CHECK (kind IN ('promotion')),
    ref_id uuid,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Serves the earnings aggregate's per-seller charge sum.
CREATE INDEX seller_charges_seller_idx ON seller_charges (seller_user_id);
