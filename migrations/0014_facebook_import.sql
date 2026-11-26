-- Wave 12: Facebook Page linking + product provenance.
-- Stores gain the seller's linked Facebook Page identity (page id
-- + display name). No tokens are stored: fetching posts through
-- the Graph API needs a page access token, and that OAuth flow
-- lands in a later wave; this wave links the page identity and
-- imports from posts the seller pastes.
-- Products gain provenance: `source` says where the row came from
-- ('manual' for the seller form, 'facebook' for an import) and
-- `source_ref` is the origin's own id — for Facebook imports, the
-- post id the seller supplied. The partial UNIQUE index on
-- (store_id, source_ref) makes re-importing the same post a no-op
-- at the database level too, not just in the import service.

ALTER TABLE stores ADD COLUMN facebook_page_id text;
ALTER TABLE stores ADD COLUMN facebook_page_name text;

ALTER TABLE products ADD COLUMN source text NOT NULL DEFAULT 'manual'
    CHECK (source IN ('manual', 'facebook'));
ALTER TABLE products ADD COLUMN source_ref text;

CREATE UNIQUE INDEX products_store_source_ref_uidx
    ON products (store_id, source_ref)
    WHERE source_ref IS NOT NULL;
