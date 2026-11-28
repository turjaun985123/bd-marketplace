-- Admin shipments remake: a real poll stamp for shipments.
--
-- Until now the platform recorded nothing about polling:
-- `updated_at` moves only when a status is APPLIED (an
-- advanced refresh, a manual status entry, a manual
-- tracking-id edit), so an unchanged or failed poll left
-- no trace anywhere. From the row alone nobody could
-- distinguish "the courier confirmed this status two
-- minutes ago" from "nobody has successfully asked about
-- this parcel in three days" — the admin oversight queue
-- needs those to be different facts.
--
-- `last_polled_at` is stamped by the ONE refresh service
-- (`refresh_shipment_by_id`) on every courier-adapter
-- attempt: an advance (inside its transaction), an
-- unchanged answer, and a failed attempt alike. It is
-- never stamped for manual shipments (there is no adapter
-- to attempt) and never backfilled: NULL honestly means
-- "no poll recorded" — every pre-existing row, and every
-- row not yet polled since this migration.
--
-- The partial index serves the poll-overdue queue
-- (shipments the poller owes a check): only live,
-- pollable rows ever enter it, so the queue walk stays
-- index-only over the small working set instead of the
-- whole historical table. Terminal and manual rows — the
-- large majority over time — are excluded by construction.

ALTER TABLE shipments ADD COLUMN last_polled_at timestamptz;

CREATE INDEX shipments_poll_due_idx
    ON shipments (last_polled_at ASC)
    WHERE courier <> 'manual'
      AND status NOT IN ('delivered', 'returned', 'cancelled');
