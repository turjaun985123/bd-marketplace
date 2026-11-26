-- Wave 22: invoices with per-store sequential numbers. An
-- invoice is the per-SELLER legal document for one sub-order
-- (the Wave 4 receipt stays the buyer's whole-order summary):
-- it is issued lazily, the first time anyone entitled asks for
-- it, and only for a sub-order that reached `delivered` (a
-- later `returned` sub-order keeps the invoice of its sale).
--
-- The number is sequential PER STORE: each store has one
-- counter row, and issuing takes the counter's `next_seq`
-- under a row lock inside the issuing transaction, so two
-- concurrent issues for one store can never draw the same
-- sequence. A sequence, once issued, is never reused — the
-- counter only moves forward, and `invoices.sub_order_id` is
-- UNIQUE, so re-asking for the same sub-order returns the
-- same row (and the same number) forever.
CREATE TABLE store_invoice_counters (
    store_id uuid PRIMARY KEY REFERENCES stores (id) ON DELETE CASCADE,
    next_seq bigint NOT NULL DEFAULT 1 CHECK (next_seq >= 1)
);

CREATE TABLE invoices (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE,
    sub_order_id uuid NOT NULL UNIQUE REFERENCES sub_orders (id) ON DELETE CASCADE,
    seq bigint NOT NULL CHECK (seq >= 1),
    invoice_number text NOT NULL,
    issued_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (store_id, seq)
);
