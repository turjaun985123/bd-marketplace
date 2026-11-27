-- Wave 54: store follows — a signed-in user may follow a
-- store to collect it on their /following page and to be
-- notified (through the Wave 29 outbox) when the store
-- publishes a new product. The composite primary key IS
-- the idempotency rule: following twice is one row, and
-- the write path uses ON CONFLICT DO NOTHING.
--
-- A follow is account data like the wishlist (Wave 24):
-- it follows the user, so it lives server-side. Deleting
-- the user or the store removes the follow with it.

CREATE TABLE store_follows (
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, store_id)
);

-- The follower-count read and the "who follows this store"
-- fan-out (the new-product notice) both read by store.
CREATE INDEX store_follows_store_created
    ON store_follows (store_id, created_at DESC);
