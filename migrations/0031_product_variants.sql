-- Wave 33: product variants (size / colour / …) — one product
-- gains purchasable variants, each with its own price delta
-- and its own stock.
--
-- 1. `product_variants` — the variant rows. A variant's price
--    is never stored outright: it is the product's base price
--    plus `price_delta_poisha` (negative deltas are allowed;
--    the API refuses any combination whose final price would
--    not be positive). `stock` is the variant's own on-hand
--    count. Deactivation is soft (`is_active = false`) — a
--    variant is never hard-deleted, so order history below
--    can always reference it.
--
-- 2. `cart_items.variant_id` — a cart line buys ONE variant
--    (NULL for variant-less products). The line's uniqueness
--    widens from (cart_id, product_id) to (cart_id,
--    product_id, variant_id). Two lines for the same product
--    with different variants must coexist, while two
--    variant-less lines for the same product must still
--    merge — plain SQL NULL semantics would treat every
--    (…, NULL) pair as distinct and break the merge, so the
--    replacement is a UNIQUE index with NULLS NOT DISTINCT
--    (Postgres 15+; the platform runs 16). The old table
--    constraint (Postgres' default name for the inline UNIQUE
--    in 0005) is dropped first.
--
-- 3. `order_items.variant_id` + `variant_name` — the
--    checkout snapshot: which variant was bought (the
--    restock paths restore stock to it) and its name at
--    purchase time, so later renames never rewrite history,
--    exactly like `title_snapshot`.
--
-- 4. `stock_adjustments.variant_id` — audit fidelity: a
--    movement that touched a VARIANT's stock says so (the
--    row still carries the product id, so the per-product
--    history shows variant movements in place, labelled by
--    the variant they moved). Product-level movements keep
--    NULL here.
--
-- Stock semantics switch (enforced in the API, recorded
-- here so the schema tells the same story): while a product
-- has at least one ACTIVE variant, availability lives on the
-- variants — the product view's stock is the SUM of active
-- variant stocks, checkout draws from variant rows, and
-- `products.stock` becomes a display fallback that no write
-- path touches for that product. With no active variants,
-- `products.stock` rules exactly as before.

CREATE TABLE product_variants (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    name text NOT NULL,
    sku text NULL,
    price_delta_poisha bigint NOT NULL DEFAULT 0,
    stock integer NOT NULL DEFAULT 0 CHECK (stock >= 0),
    is_active boolean NOT NULL DEFAULT true,
    sort integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX product_variants_product_idx
    ON product_variants (product_id, is_active, sort, name);

ALTER TABLE cart_items
    ADD COLUMN variant_id uuid NULL REFERENCES product_variants (id);

ALTER TABLE cart_items
    DROP CONSTRAINT IF EXISTS cart_items_cart_id_product_id_key;

CREATE UNIQUE INDEX cart_items_cart_product_variant_key
    ON cart_items (cart_id, product_id, variant_id) NULLS NOT DISTINCT;

ALTER TABLE order_items
    ADD COLUMN variant_id uuid NULL REFERENCES product_variants (id),
    ADD COLUMN variant_name text NULL;

ALTER TABLE stock_adjustments
    ADD COLUMN variant_id uuid NULL REFERENCES product_variants (id);
