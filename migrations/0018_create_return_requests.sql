-- Wave 16: return_requests — a buyer's request to send back one
-- DELIVERED sub-order, the seller's decision, and the refund the
-- platform owes once the goods are received back.
-- One return per sub-order (UNIQUE on sub_order_id): the whole
-- sub-order is the unit of return in this wave — partial-item
-- returns are a later wave's problem, not a second row here.
-- The request status and the refund status are SEPARATE fields:
-- the request flows requested -> approved/rejected -> received
-- (the domain can_transition_return table), while the refund
-- flows none -> pending (set when the goods are received) ->
-- done (set when an admin records the off-platform refund).
-- refund_poisha is a snapshot computed at receipt time by the
-- pure domain function compute_refund_poisha (goods minus the
-- sub-order's coupon discount; delivery is never refunded).
-- Courier-driven returns (RTO) deliberately create NO row here:
-- they restock through the shipment refresh path and never imply
-- a refund on their own.

CREATE TABLE return_requests (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    sub_order_id uuid NOT NULL UNIQUE REFERENCES sub_orders (id) ON DELETE CASCADE,
    order_id uuid NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    buyer_id uuid NOT NULL REFERENCES users (id),
    store_id uuid NOT NULL REFERENCES stores (id),
    reason text NOT NULL,
    status text NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'approved', 'rejected', 'received')),
    refund_poisha bigint CHECK (refund_poisha IS NULL OR refund_poisha >= 0),
    refund_status text NOT NULL DEFAULT 'none'
        CHECK (refund_status IN ('none', 'pending', 'done')),
    seller_note text,
    created_at timestamptz NOT NULL DEFAULT now(),
    decided_at timestamptz
);

CREATE INDEX return_requests_store_id_idx ON return_requests (store_id, created_at DESC);
CREATE INDEX return_requests_buyer_id_idx ON return_requests (buyer_id, created_at DESC);
CREATE INDEX return_requests_refund_pending_idx ON return_requests (created_at)
    WHERE refund_status = 'pending';
