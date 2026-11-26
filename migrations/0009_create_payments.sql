-- Wave 5: online payments foundation.
-- orders gains a payment_status: pay_on_delivery for COD orders (the
-- default, matching every existing row), then pending / paid / failed
-- / refunded for online payments as their payment attempt settles.
-- The payments table holds one row per payment ATTEMPT against an
-- order; COD orders deliberately have NO payments row (cash is
-- collected by the courier, there is no gateway attempt to record).
-- payment_events is the auditable webhook log: every callback the
-- webhook endpoint receives is stored with its raw body and whether
-- its signature verified, including the rejected ones.
-- All money is integer poisha (bigint).

ALTER TABLE orders
    ADD COLUMN payment_status text NOT NULL DEFAULT 'pay_on_delivery'
        CHECK (payment_status IN ('pay_on_delivery', 'pending', 'paid', 'failed', 'refunded'));

CREATE TABLE payments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id uuid NOT NULL REFERENCES orders (id),
    method text NOT NULL
        CHECK (method IN ('cod', 'bkash', 'nagad', 'sslcommerz', 'shurjopay')),
    amount_poisha bigint NOT NULL CHECK (amount_poisha >= 0),
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'paid', 'failed', 'refunded')),
    gateway_ref text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    paid_at timestamptz
);

CREATE INDEX payments_order_id_idx ON payments (order_id, created_at DESC);
CREATE INDEX payments_gateway_ref_idx ON payments (gateway_ref);

CREATE TABLE payment_events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    payment_id uuid NOT NULL REFERENCES payments (id),
    event_type text NOT NULL,
    raw_body text,
    signature_valid boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX payment_events_payment_id_idx ON payment_events (payment_id, created_at);
