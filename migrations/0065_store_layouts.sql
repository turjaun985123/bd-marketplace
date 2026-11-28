-- Wave 58: storefront section layouts — the section editor.
--
-- One row per store: the seller's composed storefront
-- layout as a single JSONB document
-- ({ "version": 1, "sections": [...] }), validated and
-- normalised by the domain crate
-- (validate_storefront_layout) before every write. The
-- document is stored whole rather than shredded into
-- per-section rows: a layout is one ordered composition
-- edited as one document (full-replace PUT, last write
-- wins), and the domain model — not the schema — owns
-- the per-type config rules, so a future section type
-- needs no migration.
--
-- A store with NO row renders the platform default
-- layout (domain::default_storefront_layout — code, not
-- a seeded row); resetting a layout deletes the row and
-- lands back on the default, so `is_custom` in the API
-- is simply "a row exists".
--
-- updated_by keeps the Wave 18/54 provenance convention
-- (the user who last wrote the branding-shaped surface);
-- no FK, like the other updated_by columns.

CREATE TABLE store_layouts (
    store_id   UUID PRIMARY KEY REFERENCES stores (id) ON DELETE CASCADE,
    layout     JSONB NOT NULL,
    updated_by UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
