-- Wave 48: notifications learn when they were claimed.
--
-- The Wave 29 outbox claim (`notify::process_pending`)
-- flips a batch of pending rows to `sending` and commits
-- BEFORE any send happens, so no row lock is held across a
-- network call — but until now the claim stamped nothing,
-- so a row stranded in `sending` by a worker that died
-- mid-batch was indistinguishable from one claimed a
-- second ago: there was no timestamp to judge staleness
-- by (the table has no `updated_at`, and `created_at`
-- says when the event happened, not when the claim did —
-- a long-queued row claimed moments ago would look
-- "old"). `claimed_at` is that stamp: the claim UPDATE
-- sets it, every outcome write-back clears it, so it is
-- non-NULL exactly while a row sits in `sending` with a
-- live claim. The worker's reaper
-- (`notify::reap_stale_sending`) keys on it: a `sending`
-- row whose claim is older than the staleness threshold
-- (default 10 minutes) belongs to a worker that never
-- reported an outcome and is recovered — see the Wave 48
-- README section.
--
-- Rows already in `sending` when this migration lands
-- keep `claimed_at` NULL; the reaper treats a NULL stamp
-- on a `sending` row as stale by definition — such a row
-- can only be a pre-Wave-48 claim, i.e. precisely the
-- stuck population the reaper exists to rescue.

ALTER TABLE notifications
    ADD COLUMN claimed_at timestamptz;

-- The reaper's scan: `sending` rows by claim age.
CREATE INDEX notifications_sending_claimed_idx
    ON notifications (status, claimed_at);
