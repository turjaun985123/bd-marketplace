-- Wave 23: inventory management & the stock audit trail.
--
-- Two additions:
--
-- 1. `products.low_stock_threshold` — the seller's per-product
--    warning level for the low-stock status. NULL means "use
--    the platform default" (domain LOW_STOCK_DEFAULT = 5); the
--    effective threshold is computed at read time, never stored.
--
-- 2. `stock_adjustments` — THE audit trail for stock. From
--    this migration on, every write to `products.stock` after
--    a product's creation inserts one row here in the SAME
--    transaction: checkouts (actor 'system', reason 'order'),
--    cancel / return / RTO restocks (actor 'system', reasons
--    'cancel_restock' / 'return_restock' / 'rto_restock'), and
--    the seller's own manual adjustments through the
--    adjust-stock endpoint (actor 'seller', the validated
--    reason vocabulary, the seller's user id). `new_stock`
--    snapshots the on-hand count right after the movement, so
--    the trail alone can replay a product's stock history.
--    Product CREATION writes no row: the history starts at
--    the creation stock, which the product row itself records
--    as its starting point.
--
-- Movements that happened before this migration are not
-- backfilled — the trail is honest about starting now.

ALTER TABLE products
    ADD COLUMN low_stock_threshold integer NULL
    CHECK (low_stock_threshold IS NULL OR low_stock_threshold >= 0);

CREATE TABLE stock_adjustments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE,
    actor text NOT NULL CHECK (actor IN ('seller', 'system')),
    actor_user_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    delta integer NOT NULL CHECK (delta <> 0),
    new_stock integer NOT NULL,
    reason text NOT NULL,
    note text NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX stock_adjustments_product_created_idx
    ON stock_adjustments (product_id, created_at DESC);
