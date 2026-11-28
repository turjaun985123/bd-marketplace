-- Admin stores remake: the queue's working index.
--
-- The remade admin store list filters by status and tier and
-- pages in created order — the same queue-index move the
-- payouts remake made for its own list (migration 0073).
-- Postgres can already serve each
-- predicate from existing indexes; this composite serves the
-- combined filter + sort in one walk.
CREATE INDEX stores_status_tier_created_idx
    ON stores (status, verified_tier, created_at DESC, id DESC);
