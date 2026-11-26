-- Wave 24: the wishlist.
--
-- One row per (user, product) pair a signed-in user has
-- saved for later. The wishlist is account data, so it is
-- server-side: the same list follows the user to any device
-- they sign in on. (Recently-viewed, the other Wave 24
-- discovery aid, is deliberately NOT here — it lives in the
-- browser's localStorage on one device and is never sent to
-- the server.)
--
-- The composite primary key makes "add" naturally
-- idempotent at the storage layer: the endpoint inserts
-- with ON CONFLICT DO NOTHING, and re-adding a product the
-- user already saved is a successful no-op, not an error.
-- Both foreign keys cascade: a deleted user or product
-- takes its wishlist rows with it, so the list can never
-- point at a product that no longer exists.
--
-- The product index serves the reverse lookup (everyone who
-- saved this product) and the cascade from products.

CREATE TABLE wishlist_items (
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, product_id)
);

CREATE INDEX wishlist_items_product_idx ON wishlist_items (product_id);
