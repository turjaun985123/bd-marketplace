-- Wave 59: store pages — a store's own standing documents
-- (about, shipping information, size guide, FAQ), the
-- custom-pages half of the store content system. A page
-- belongs to exactly one store and its slug is unique
-- WITHIN that store (two stores may both run a "faq"
-- page — the storefront URL carries the store slug
-- first, exactly like store collections in 0053).
--
-- The lifecycle is the domain's content state machine
-- (draft / published / archived, `can_transition_content_
-- status`): only published pages are public, and only
-- published pages flagged `show_in_nav` join the
-- storefront's navigation, in the seller's `position`
-- order. Pages carry no schedule — the status alone
-- decides visibility (domain `content_is_live` with a
-- `None` publish moment).
--
-- Bodies are PLAIN TEXT, rendered as paragraphs by the
-- web app. No markup is stored or honoured anywhere in
-- the content system — that is the XSS stance, taken at
-- the schema's door: there is no HTML column because
-- there is no HTML.
CREATE TABLE store_pages (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE,
    slug text NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    status text NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'published', 'archived')),
    show_in_nav boolean NOT NULL DEFAULT false,
    position integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT store_pages_store_slug_unique UNIQUE (store_id, slug)
);

-- The public nav read (published + flagged, position
-- order) and the seller list (one store, position order)
-- both read by store.
CREATE INDEX store_pages_store_idx
    ON store_pages (store_id, position, created_at);
