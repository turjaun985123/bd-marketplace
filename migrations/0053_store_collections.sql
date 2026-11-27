-- Wave 54 (extension): store collections — a seller's own
-- curated product groups on their storefront ("ঈদের
-- কালেকশন", "নতুন এসেছে"), distinct from the platform
-- collections of Wave 52 (admin-curated, marketplace-wide).
-- A collection belongs to exactly one store; its slug is
-- unique WITHIN the store (two stores may both run an
-- "eid" collection — the storefront URL carries the store
-- slug first). Membership is an explicit ordered list:
-- `position` is the seller's arrangement, and the public
-- storefront renders members in that order. Deleting a
-- collection removes its membership rows only; deleting a
-- product removes it from every collection (the product
-- itself is the seller's catalogue fact, the membership
-- is presentation).
CREATE TABLE store_collections (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE,
    slug text NOT NULL,
    title text NOT NULL,
    description text NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT store_collections_store_slug_unique UNIQUE (store_id, slug)
);

CREATE INDEX store_collections_store_idx
    ON store_collections (store_id, created_at DESC);

CREATE TABLE store_collection_products (
    collection_id uuid NOT NULL REFERENCES store_collections (id) ON DELETE CASCADE,
    product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    position integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (collection_id, product_id)
);

CREATE INDEX store_collection_products_product_idx
    ON store_collection_products (product_id);
