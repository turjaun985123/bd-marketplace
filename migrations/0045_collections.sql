-- Wave 52: collections — admin-curated, ordered product sets
-- ("Eid picks", "Under ৳999", "New seller spotlight") rendered
-- as named sections on the home page and as their own landing
-- pages. A collection is a merchandising object, not a
-- category: the same product may sit in many collections, and
-- membership order is the admin's chosen order (`position`),
-- never a sort the reader re-derives.
--
-- `slug` is the landing-page segment (`/collections/<slug>`)
-- and follows the store-slug rule (the domain crate's
-- `validate_collection_slug` mirrors `is_valid_slug`).
--
-- Membership rows cascade on BOTH sides: deleting a
-- collection clears its membership, and if a product row ever
-- goes, its memberships go with it (products are not
-- hard-deleted in practice — their lifecycle is
-- draft/active/archived — but the cascade keeps the join
-- table honest if that ever changes).

CREATE TABLE collections (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    slug text NOT NULL UNIQUE,
    title text NOT NULL,
    subtitle text NULL,
    is_active boolean NOT NULL DEFAULT true,
    position integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE collection_products (
    collection_id uuid NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    position integer NOT NULL DEFAULT 0,
    PRIMARY KEY (collection_id, product_id)
);

-- Membership reads are always "this collection's products, in
-- the admin's order".
CREATE INDEX collection_products_order
    ON collection_products (collection_id, position);

-- The home feed reads active collections in position order.
CREATE INDEX collections_active_position
    ON collections (is_active, position, created_at);
