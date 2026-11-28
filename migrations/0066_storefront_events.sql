-- Wave 58: storefront section analytics events.
--
-- The Wave 52 merch_events pattern applied to storefront
-- sections: an append-only, aggregate-only log. There is
-- NO user id, NO session id, NO IP and NO user agent —
-- the seller sees "this section was seen N times, its
-- link clicked M times", never who did it. A section is
-- identified by its layout id (a seller-chosen slug, not
-- a uuid), which is why this is its own table rather than
-- a merch_events target kind: merch targets are uuids.
--
-- kind is validated in the domain crate
-- (validate_storefront_event_kind: section_view |
-- section_click); the CHECK keeps a bad direct write out
-- anyway. Events are recorded only for sections in the
-- store's CURRENT effective layout (the endpoint
-- validates the id against it), so the counts a seller
-- sees always describe blocks their storefront can show.

CREATE TABLE storefront_events (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    store_id   UUID NOT NULL REFERENCES stores (id) ON DELETE CASCADE,
    section_id TEXT NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('section_view', 'section_click')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX storefront_events_store_time_idx
    ON storefront_events (store_id, created_at);
CREATE INDEX storefront_events_store_section_idx
    ON storefront_events (store_id, section_id, kind);
