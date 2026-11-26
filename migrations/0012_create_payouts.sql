-- Wave 9: seller payouts. A seller's earnings come from DELIVERED
-- sub_orders (goods subtotal minus the commission snapshot; the
-- delivery charge is courier/platform money and never earns). One
-- payout account per store says where the money goes; every payout
-- request snapshots the account fields, so later account edits can
-- never rewrite where an earlier payout was sent.
-- Payout lifecycle: requested -> approved -> paid, or requested ->
-- rejected (see domain::can_transition_payout). Sellers create
-- `requested` rows in this wave; approval and payment processing
-- land with the admin wave, so rows stay `requested` until then.

CREATE TABLE payout_accounts (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE,
    method text NOT NULL CHECK (method IN ('bkash', 'nagad', 'bank')),
    account_name text NOT NULL,
    account_number text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (store_id)
);

CREATE TABLE payouts (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    store_id uuid NOT NULL REFERENCES stores (id),
    amount_poisha bigint NOT NULL CHECK (amount_poisha > 0),
    method text NOT NULL CHECK (method IN ('bkash', 'nagad', 'bank')),
    account_name text NOT NULL,
    account_number text NOT NULL,
    status text NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'approved', 'paid', 'rejected')),
    note text,
    created_at timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz
);

CREATE INDEX payouts_store_status_idx ON payouts (store_id, status);
CREATE INDEX payouts_store_created_idx ON payouts (store_id, created_at DESC);
