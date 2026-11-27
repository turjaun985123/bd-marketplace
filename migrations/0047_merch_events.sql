-- Wave 52: merchandising events — an append-only counter log
-- for the two merchandising interactions the platform wants
-- to measure: a home banner being clicked and a collection
-- landing page being viewed. Deliberately minimal: no user
-- id, no session, no fingerprinting — the admin merchandising
-- stats (the metrics endpoint) only ever read COUNTS per
-- kind, so the table stores exactly what the counts need and
-- nothing that could identify a visitor.
--
-- Rows are only ever inserted (by the public beacon endpoint)
-- and counted (by the admin metrics read). There is no update
-- or delete path.

CREATE TABLE merch_events (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('banner_click', 'collection_view')),
    target_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- The stats read counts per kind, all-time and per Dhaka day.
CREATE INDEX merch_events_kind_created
    ON merch_events (kind, created_at);
