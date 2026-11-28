-- Admin verifications remake: decision metadata, submission
-- history and queue indexes for `store_verifications`.
--
-- The Wave 21 model kept exactly one row per store and
-- overwrote it on every resubmission, so a store's KYC
-- history — what was submitted, when, by whom, and what a
-- reviewer decided before — survived only in fragments of
-- the Wave 28 admin audit log. The remake keeps the one-row
-- model (it is the CURRENT application, and the queue reads
-- it) and adds the missing provenance around it:
--
-- * `version` — an optimistic-concurrency counter for the
--   application payload. It starts at 1, increments on every
--   payload-changing submit/edit and on every decision, and
--   admin approve/reject must present the version they
--   reviewed (`expected_version`): a mismatch is a 409, so a
--   decision can never silently land on documents the
--   reviewer did not see. This column is new — no earlier
--   migration has a row-version precedent — and its
--   semantics live here and in the API's verification
--   module, nowhere else.
--
-- * `applicant_name` — the name as it appears on the NID,
--   supplied by the seller at submit time going forward
--   (validated by domain::validate_applicant_name when
--   present; the field is optional because the seller page
--   predates it). It is self-asserted free text: the
--   authoritative submitter identity is the stamped user id
--   below, and the reviewer compares this name against the
--   document image. Existing rows stay NULL — the name on a
--   past applicant's NID is not the account display name
--   (`users.name`), and backfilling it from there would
--   fabricate KYC data.
--
-- * `submitted_by_user_id` — the account that filed the
--   current application, stamped by the API from the
--   authenticated caller (submission is owner-only as of
--   this remake, so this is the store owner). Existing rows
--   stay NULL: pre-remake submissions could be filed by a
--   store manager under the old shared action, so no exact
--   source exists and none is inferred (the migration 0072
--   rule: exact, or honestly absent).
--
-- * `reviewed_by_user_id` — the admin who made the CURRENT
--   decision (paired with the existing `reviewed_at`; a
--   store has exactly one current decision at a time, unlike
--   payouts' three terminal stamps, so one actor column is
--   the honest shape). Backfilled below from the audit log,
--   status-guarded and pinned to the decision's own audit
--   row: verification decisions share target_type 'store'
--   with store.suspend/reactivate, so the action filter is
--   load-bearing, and a store may have several decision
--   cycles (reject → resubmit → approve), so the row that
--   counts is the one written in the same transaction as
--   `reviewed_at` — matched by exact timestamp equality.
--   Pending rows and pre-audit-era (pre-Wave-28) decisions
--   keep NULL: unknown is recorded as unknown.
--
-- * `nid_last4` integrity — the masked read paths now trust
--   the stored last4 instead of re-deriving it from the full
--   number, so the database finally enforces the derivation
--   the API has always performed. Any divergent legacy row
--   is repaired from the authoritative `nid_number` FIRST
--   (the column is purely derived, so repair loses nothing),
--   and only then is the CHECK added — adding it first could
--   abort the migration on a single bad row.
--
-- * `store_verification_events` — the submission history
--   migration 0022 deliberately did not build ("no
--   application history table"). That choice is reversed
--   here for SUBMISSIONS ONLY: decisions and full-NID views
--   already have a canonical record — `admin_audit_log`,
--   written in the decision's own transaction — and a second
--   decision log would diverge from it. What the audit log
--   can never hold is seller-side submissions (it is an
--   admin-action log), so events record exactly those:
--   `submitted` (first application or resubmission after a
--   rejection) and `resubmitted` (a payload-changing edit of
--   a still-pending application). An event NEVER carries the
--   full NID — only `nid_last4`. It also carries NO hash of
--   the number: NIDs are 10/13/17 digits, and an unsalted
--   hash of a low-entropy identifier is a brute-forceable
--   second copy of it, which would break the platform rule
--   that the full number is stored exactly once. Duplicate
--   detection is a read-time exact-match count in the API,
--   not a stored fingerprint. The store FK is deliberately
--   NO ACTION (the gift-card-ledger precedent, migration
--   0070): KYC history must not evaporate if a store row is
--   ever deleted, the way disposable analytics events do.
--
-- * Indexes — the queue orders by (status, submitted_at) and
--   paginates, so the single-column status index from 0022
--   is superseded by a composite with the store_id tiebreak
--   that makes page boundaries deterministic; the events
--   table gets the per-store history index every event
--   table carries. Plain DDL only, inside the migration
--   transaction (the migration 0030 rule).
ALTER TABLE store_verifications
    ADD COLUMN version bigint NOT NULL DEFAULT 1,
    ADD COLUMN applicant_name text
        CHECK (applicant_name IS NULL
               OR char_length(applicant_name) BETWEEN 1 AND 80),
    ADD COLUMN submitted_by_user_id uuid
        REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN reviewed_by_user_id uuid
        REFERENCES users (id) ON DELETE SET NULL;

UPDATE store_verifications
SET nid_last4 = right(nid_number, 4)
WHERE nid_last4 <> right(nid_number, 4);

ALTER TABLE store_verifications
    ADD CONSTRAINT store_verifications_nid_last4_matches
        CHECK (nid_last4 = right(nid_number, 4));

UPDATE store_verifications v
SET reviewed_by_user_id = a.admin_user_id
FROM admin_audit_log a
WHERE v.status = 'approved'
  AND a.target_type = 'store'
  AND a.target_id = v.store_id::text
  AND a.action = 'verification.approve'
  AND a.created_at = v.reviewed_at;

UPDATE store_verifications v
SET reviewed_by_user_id = a.admin_user_id
FROM admin_audit_log a
WHERE v.status = 'rejected'
  AND a.target_type = 'store'
  AND a.target_id = v.store_id::text
  AND a.action = 'verification.reject'
  AND a.created_at = v.reviewed_at;

CREATE TABLE store_verification_events (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    store_id uuid NOT NULL REFERENCES stores (id),
    actor_user_id uuid REFERENCES users (id) ON DELETE SET NULL,
    kind text NOT NULL CHECK (kind IN ('submitted', 'resubmitted')),
    nid_last4 text NOT NULL,
    has_trade_license boolean NOT NULL,
    document_url text,
    applicant_name text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX store_verification_events_store_created_idx
    ON store_verification_events (store_id, created_at DESC, id DESC);

DROP INDEX store_verifications_status_idx;

CREATE INDEX store_verifications_status_submitted_idx
    ON store_verifications (status, submitted_at ASC, store_id ASC);
