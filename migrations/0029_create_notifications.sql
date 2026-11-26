-- Wave 29: the notification outbox. A state-changing service
-- function INSERTs one row here inside the SAME transaction as
-- the change it announces (order placed, sub-order shipped /
-- delivered / cancelled / returned, payout paid, return
-- update), so a notification exists exactly when the event
-- committed — never for a rolled-back change, never missing
-- for a committed one. The worker claims pending rows
-- (FOR UPDATE SKIP LOCKED), sends them through the configured
-- SmsSender, and records the outcome on the row: `sent` with
-- `sent_at`, or one more attempt. After MAX_ATTEMPTS (3)
-- failed attempts the row is `failed` and stays visible in
-- the admin notifications view — a notification is never
-- silently dropped.
--
-- `body` is the RENDERED SMS text (domain::render_body at
-- enqueue time), so a queued message's wording is frozen at
-- the moment of the event; `payload` keeps the template
-- variables the body was rendered from, for debugging.
CREATE TABLE notifications (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL REFERENCES users (id),
    phone text NOT NULL,
    kind text NOT NULL,
    body text NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}',
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'sending', 'sent', 'failed')),
    attempts integer NOT NULL DEFAULT 0,
    last_error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz
);

-- The worker's claim scan: pending rows, oldest first.
CREATE INDEX notifications_pending_idx ON notifications (status, created_at);
