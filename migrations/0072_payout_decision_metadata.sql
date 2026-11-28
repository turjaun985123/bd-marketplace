-- Admin payouts remake: per-transition decision metadata on
-- the payout row.
--
-- Until now a payout carried ONE timestamp, `processed_at`,
-- which every transition overwrote — so after mark-paid the
-- approval moment was gone, and the row named NO actor at
-- all (who approved / paid / rejected lived only in the
-- admin audit log). The remake records each decision on the
-- row itself:
--
-- - `approved_at` / `paid_at` / `rejected_at`: when each
--   transition happened. `processed_at` keeps being written
--   on every transition (older readers pin it); these are
--   the per-stage stamps that never get overwritten.
-- - `approved_by_user_id` / `paid_by_user_id` /
--   `rejected_by_user_id`: the acting admin, mirroring the
--   audit row written in the same transaction. ON DELETE
--   SET NULL (the actor-column precedent, migration 0024):
--   the stamp survives the admin, the name does not have to.
-- - `payment_reference`: the bKash/Nagad transaction id or
--   bank transfer reference the admin recorded when marking
--   the payout paid — the proof that ties the platform row
--   to the real-world send. Required by the mark-paid API
--   from this wave on; the CHECK mirrors the domain
--   validator (`validate_payment_reference`): 4 to 128
--   characters when present. Deliberately NOT unique —
--   references are only unique per gateway/account in the
--   real world, and no domain rule states a global one.
--
-- Backfill, in two steps, both exact or honestly absent:
--
-- Step A takes the per-status stamp from `processed_at`.
-- That is exact, not approximate: `processed_at` holds the
-- LAST transition time, and for a row currently `approved`
-- or `rejected` the last transition IS the decision; for a
-- `paid` row it is the payment. `requested` rows have a
-- NULL `processed_at` and keep NULL stamps.
--
-- Step B takes the actors — and `approved_at` for `paid`
-- rows, which step A cannot know — from the admin audit
-- log (Wave 28), one pivot per action. The pivot matters:
-- a `paid` payout legitimately has TWO audit rows
-- (`payout.approve`, then `payout.mark_paid`), so a join on
-- the target alone would let Postgres pick either. Audit
-- rows were written in the same transaction as the
-- transition, so an audit `created_at` EQUALS that
-- transition's `processed_at` (Postgres `now()` is the
-- transaction start) — audit-derived stamps are exact too.
-- Payouts decided before the audit log existed (pre-Wave
-- 28) keep NULL actors: unknown is recorded as unknown,
-- never inferred (in particular a paid row's approver is
-- never guessed from its payer).
--
-- `payment_reference` has no backfill source and stays
-- NULL on every existing row; it is never mined from
-- `note` (the note is a rejection reason, not a reference).
ALTER TABLE payouts
    ADD COLUMN approved_at TIMESTAMPTZ,
    ADD COLUMN paid_at TIMESTAMPTZ,
    ADD COLUMN rejected_at TIMESTAMPTZ,
    ADD COLUMN approved_by_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN paid_by_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN rejected_by_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN payment_reference TEXT
        CHECK (payment_reference IS NULL
               OR char_length(payment_reference) BETWEEN 4 AND 128);

-- Step A: per-status stamps from the last-transition time.
UPDATE payouts SET approved_at = processed_at WHERE status = 'approved';
UPDATE payouts SET rejected_at = processed_at WHERE status = 'rejected';
UPDATE payouts SET paid_at = processed_at WHERE status = 'paid';

-- Step B: actors (and the approval stamp of paid rows) from
-- the audit log, one pivot per action, earliest row wins
-- (the transition table allows each action at most once per
-- payout, so duplicates could only come from hand-written
-- SQL; the real transition is the earliest).
UPDATE payouts p
SET approved_by_user_id = a.admin_user_id,
    approved_at = a.created_at
FROM (SELECT DISTINCT ON (target_id) target_id, admin_user_id, created_at
      FROM admin_audit_log
      WHERE target_type = 'payout' AND action = 'payout.approve'
      ORDER BY target_id, created_at ASC) a
WHERE a.target_id = p.id::text AND p.status IN ('approved', 'paid');

UPDATE payouts p
SET rejected_by_user_id = a.admin_user_id,
    rejected_at = a.created_at
FROM (SELECT DISTINCT ON (target_id) target_id, admin_user_id, created_at
      FROM admin_audit_log
      WHERE target_type = 'payout' AND action = 'payout.reject'
      ORDER BY target_id, created_at ASC) a
WHERE a.target_id = p.id::text AND p.status = 'rejected';

UPDATE payouts p
SET paid_by_user_id = a.admin_user_id,
    paid_at = a.created_at
FROM (SELECT DISTINCT ON (target_id) target_id, admin_user_id, created_at
      FROM admin_audit_log
      WHERE target_type = 'payout' AND action = 'payout.mark_paid'
      ORDER BY target_id, created_at ASC) a
WHERE a.target_id = p.id::text AND p.status = 'paid';

-- Support lookups by recorded reference (partial, like the
-- idempotency key's index in migration 0038: keyless and
-- pre-remake rows stay out of it entirely).
CREATE INDEX payouts_payment_reference_idx
    ON payouts (payment_reference)
    WHERE payment_reference IS NOT NULL;
