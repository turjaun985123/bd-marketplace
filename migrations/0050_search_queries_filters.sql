-- Wave 53 follow-on: the canonical filters, stored as
-- JSON beside their fingerprint.
--
-- Migration 0049 stored only the fingerprint (a grouping
-- key). A hash alone is opaque to the operator: the
-- analytics read could count filter combinations but
-- never say WHAT they were. The canonical filter set —
-- the validated browse filters in their stored
-- (saved-search) shape — is small, carries no identifier
-- (it is the same shape a buyer may save against their
-- own account), and makes the combinations readable.
-- Existing rows keep NULL: they were logged before the
-- shape was stored, and inventing their filters would be
-- fabrication; the combinations read covers rows that
-- carry the shape.
ALTER TABLE search_queries
    ADD COLUMN filters_json jsonb;
