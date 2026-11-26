-- Wave 21: seller KYC & verification tiers. A store's tier is
-- trust metadata earned through manual document review:
-- `unverified` (every new store), `basic` (the platform approved
-- the seller's NID) and `verified` (NID plus a trade license
-- approved). The CHECK mirrors the domain crate's
-- VERIFICATION_TIERS, and the default matches
-- domain::DEFAULT_VERIFIED_TIER.
ALTER TABLE stores
    ADD COLUMN verified_tier text NOT NULL DEFAULT 'unverified'
        CHECK (verified_tier IN ('unverified', 'basic', 'verified'));

-- One verification application per store (the storefront model
-- is one store per seller, so the application hangs off the
-- store). No row means the store has never applied — the API
-- reports that virtual status as `none`; the stored statuses
-- are the decided/in-flight ones (see
-- domain::can_transition_verification). Re-applying UPDATES
-- this row, so there is no application history table: the row
-- always holds the CURRENT application and its latest review.
--
-- `nid_number` stores the FULL national ID number exactly
-- once. It is an identity document identifier: the API returns
-- it from exactly one admin endpoint (the reviewer's full
-- view, needed to check the number against the submitted
-- document) and every other surface — seller views, admin
-- lists, public store views — carries only `nid_last4` /
-- the domain-masked form. `document_url` is a hotlinked image
-- URL like the Wave 18 branding URLs (no upload/storage yet).
CREATE TABLE store_verifications (
    store_id uuid PRIMARY KEY REFERENCES stores (id) ON DELETE CASCADE,
    status text NOT NULL
        CHECK (status IN ('pending', 'approved', 'rejected')),
    nid_number text NOT NULL,
    nid_last4 text NOT NULL,
    trade_license_number text,
    document_url text,
    reviewer_note text,
    submitted_at timestamptz NOT NULL DEFAULT now(),
    reviewed_at timestamptz
);

CREATE INDEX store_verifications_status_idx ON store_verifications (status);
