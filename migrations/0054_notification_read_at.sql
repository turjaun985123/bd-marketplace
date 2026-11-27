-- Wave 54 (extension): the buyer's notification inbox.
-- The outbox row gains a read marker so a user can tell
-- what they have already seen in their in-app inbox
-- (GET /v1/notifications). NULL = unread. The marker is
-- the USER's state on their own copy — it never affects
-- delivery: `status` keeps its delivery meaning
-- (pending/sending/sent/failed), and the worker's drain
-- and reaper do not consult this column.
ALTER TABLE notifications ADD COLUMN read_at timestamptz NULL;

CREATE INDEX notifications_user_unread_idx
    ON notifications (user_id, created_at DESC)
    WHERE read_at IS NULL;
