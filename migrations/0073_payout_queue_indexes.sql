-- Admin payouts remake: indexes for the rebuilt queue.
--
-- The remade admin list orders the queue by status and age
-- (oldest-first is the working order for a processing
-- queue) and the payout detail reads one payout's audit
-- history by target. The existing payout indexes lead with
-- `store_id` (migration 0012) and cannot serve either.
-- Plain CREATE INDEX only — migrations run inside one
-- transaction, where CONCURRENTLY is impossible (the
-- migration 0030 rule); at admin-queue scale the plain
-- build is fine.
CREATE INDEX payouts_status_created_idx
    ON payouts (status, created_at DESC, id DESC);

CREATE INDEX admin_audit_log_target_idx
    ON admin_audit_log (target_type, target_id, created_at DESC);
