-- Admin orders remake: indexes for the oversight queue.
--
-- The orders table carried no secondary index on its read
-- paths at all: the admin queue's default order (newest
-- first), its amount orders, and the advance-queue filters
-- all sequential-scanned and sorted the whole table. Two
-- composites cover the queue's real access paths:
--
-- * `(created_at, id)` — the default newest-first page
--   order (and oldest-first, read backwards), with the id
--   tiebreaker the deterministic-order rule requires, in
--   the same shape as the store queue's index (0076).
-- * `(payment_status, created_at, id)` — the payment and
--   advance queues (advance_pending above all): filter by
--   payment status, then walk in age order without a sort.
--
-- The parent-status filter (placed/cancelled) is a
-- two-value split over the same created_at walk, so it
-- deliberately gets no index of its own.

CREATE INDEX orders_created_at_id_idx
    ON orders (created_at DESC, id DESC);

CREATE INDEX orders_payment_status_created_idx
    ON orders (payment_status, created_at DESC, id DESC);
