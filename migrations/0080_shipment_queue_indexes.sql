-- Admin shipments remake: indexes for the oversight queue.
--
-- The shipments table carried the poller's index
-- (`idx_shipments_status_updated`, migration 0030) and a
-- consignment lookup, but nothing for the admin queue's
-- own walks. Plain CREATE INDEX only — migrations run
-- inside one transaction, where CONCURRENTLY is
-- impossible (the migration 0030 rule).
--
-- * `(created_at, id)` — the default newest-first page
--   order (and oldest-first, read backwards), with the
--   id tiebreaker the deterministic-order rule requires,
--   in the same shape as the order queue's index (0077).
-- * `(status, created_at, id)` — the status-filtered
--   queue: filter by unified status, then walk in page
--   order without a sort (the payout/store queue shape,
--   0073/0076).
-- * `(courier, created_at, id)` — the courier-filtered
--   queue. Courier is low-cardinality, so this earns its
--   keep from the combined filter+sort, not selectivity.
-- * `shipment_events (shipment_id, status, created_at)` —
--   the queue's per-row status-since probe: the moment a
--   shipment entered its CURRENT status is the newest
--   event in that status, answered by one descent of
--   this composite. The 0010 index `(shipment_id,
--   created_at)` stays — the timeline and tracking reads
--   order by it — but it must filter status after
--   fetching all of a shipment's events.
--
-- The store filter deliberately gets no index here:
-- store lives on `sub_orders`, whose own store indexes
-- (0007/0030) find the store's sub-orders, each joining
-- to its shipment through the UNIQUE
-- `shipments_sub_order_id_key`.

CREATE INDEX shipments_created_at_id_idx
    ON shipments (created_at DESC, id DESC);

CREATE INDEX shipments_status_created_idx
    ON shipments (status, created_at DESC, id DESC);

CREATE INDEX shipments_courier_created_idx
    ON shipments (courier, created_at DESC, id DESC);

CREATE INDEX shipment_events_shipment_status_created_idx
    ON shipment_events (shipment_id, status, created_at DESC);
