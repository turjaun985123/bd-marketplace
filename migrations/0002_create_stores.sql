-- Wave 2: stores. One store = one seller storefront; `slug` is the
-- storefront subdomain segment (shop.<slug> style routing uses it too).
-- store_id tenancy: every tenant-owned row in later tables carries a
-- store_id referencing this table.
CREATE TABLE stores (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_user_id uuid NOT NULL REFERENCES users (id),
    slug text UNIQUE NOT NULL,
    name text NOT NULL,
    description text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX stores_owner_idx ON stores (owner_user_id);
