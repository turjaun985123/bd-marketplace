-- Wave 2: products. Money is integer poisha (bigint), never floats.
-- Search: pg_trgm GIN index on title so ILIKE / similarity search over
-- product titles stays fast; title_bn holds the Bangla title.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE products (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    store_id uuid NOT NULL REFERENCES stores (id),
    title text NOT NULL,
    title_bn text,
    description text,
    price_poisha bigint NOT NULL CHECK (price_poisha >= 0),
    compare_at_poisha bigint CHECK (compare_at_poisha IS NULL OR compare_at_poisha >= 0),
    stock int NOT NULL DEFAULT 0 CHECK (stock >= 0),
    status text NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'archived')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX products_store_id_idx ON products (store_id);
CREATE INDEX products_title_trgm_idx ON products USING gin (title gin_trgm_ops);
CREATE INDEX products_title_bn_trgm_idx ON products USING gin (title_bn gin_trgm_ops);
