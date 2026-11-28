-- Admin stores remake: suspension metadata on the store row.
--
-- Until now a suspension left exactly one trace: an
-- admin_audit_log row. The store row itself carried neither
-- WHEN the status last changed, nor WHICH admin changed it,
-- nor WHY — so the seller could not be shown the reason on
-- their own dashboard, and the admin queue could not say how
-- long a store has been dark without mining the audit log.
--
-- status_changed_at / status_changed_by_user_id stamp the
-- LATEST transition (either direction). suspension_note is
-- the admin's reason for the CURRENT suspension: set on
-- suspend, cleared on reactivate, never carried across a
-- restore (a stale reason must not outlive the state it
-- justified). The note is seller-visible by design — it is
-- the explanation the seller acts on — and is capped at 500
-- characters, the platform's reviewer-note cap.
ALTER TABLE stores
    ADD COLUMN status_changed_at timestamptz,
    ADD COLUMN status_changed_by_user_id uuid
        REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN suspension_note text
        CHECK (suspension_note IS NULL
               OR char_length(suspension_note) BETWEEN 1 AND 500);

-- Backfill the stamp from the audit log's LATEST transition
-- pivot per store — but only where the pivot's target status
-- still equals the store's current status. A pivot that no
-- longer matches (an older suspend under a since-restored
-- store whose reactivate row is missing, say) would stamp a
-- lie, so those stores stay unstamped rather than guessed.
-- The note is NEVER backfilled: no reason was recorded
-- before this migration, and inventing one would put words
-- in a past admin's mouth.
UPDATE stores s
SET status_changed_at = pivot.created_at,
    status_changed_by_user_id = pivot.admin_user_id
FROM (
    SELECT DISTINCT ON (a.target_id)
        a.target_id,
        a.created_at,
        a.admin_user_id,
        (a.detail ->> 'status') AS pivot_status
    FROM admin_audit_log a
    WHERE a.action IN ('store.suspend', 'store.reactivate')
      AND a.target_type = 'store'
    ORDER BY a.target_id, a.created_at DESC, a.id DESC
) AS pivot
WHERE pivot.target_id = s.id::text
  AND pivot.pivot_status = s.status;
