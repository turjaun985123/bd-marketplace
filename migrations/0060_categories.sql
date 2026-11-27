-- Wave 56: the managed category taxonomy.
--
-- Until this wave a product's category was a free-text label
-- (`products.category`, Wave 15): sellers typed anything, the
-- browse facets grouped the strings, and the home page tiled
-- them. That model cannot nest, cannot be curated, and gives
-- the storefront no stable category pages. This migration
-- adds the real taxonomy next to it:
--
-- * `categories` — a self-referencing tree. `slug` is the
--   stable public identifier (UNIQUE across the whole tree,
--   not per parent: a slug is a URL, and two nodes sharing a
--   slug would make `/categories/<slug>` ambiguous).
--   `icon_key` names one entry of the small built-in icon set
--   in the domain crate (`CATEGORY_ICON_KEYS`) — icons are
--   keys, never uploaded images. `is_featured` (Wave 56
--   merchandising) promotes a node in the home feed's
--   taxonomy strip. Depth is capped at 3 levels by the
--   domain rules, not by a constraint — the cap is a
--   merchandising decision enforced in the one service
--   layer, like the rest of the taxonomy invariants.
-- * `products.category_id` — the product's link into the
--   tree, nullable: a product whose seller-typed label
--   matches no taxonomy node keeps `category_id` NULL and
--   its free-text label, exactly as before. The legacy text
--   column STAYS as the denormalised display name, kept in
--   sync by the service layer (setting `category_id` writes
--   the node's English name into `products.category`), so
--   search, facets, receipts and exports — every surface
--   that reads the text column — keep working
--   byte-identically.
--
-- The FK is the default RESTRICT: the service layer refuses
-- to delete a category that still has products (a bilingual
-- 409 with the count), and the constraint is the backstop
-- that keeps that promise honest.
CREATE TABLE categories (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_id uuid REFERENCES categories (id),
    slug text NOT NULL UNIQUE,
    name_en text NOT NULL,
    name_bn text,
    icon_key text NOT NULL DEFAULT 'tag',
    sort_order integer NOT NULL DEFAULT 0,
    is_active boolean NOT NULL DEFAULT true,
    is_featured boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX categories_parent_idx ON categories (parent_id);

ALTER TABLE products
    ADD COLUMN category_id uuid REFERENCES categories (id);

CREATE INDEX products_category_idx ON products (category_id);
