-- Wave 55: the buyer address book. A signed-in buyer keeps
-- a small set of named delivery addresses so checkout stops
-- re-asking for the same typing; checkout may reference one
-- by id, and the server then takes the order's delivery
-- fields from THIS row (the client-sent fields are ignored
-- on that path — the address book is the source of truth).
--
-- The district is stored as the canonical English district
-- name from the domain district table (`domain::DISTRICTS`)
-- — the delivery zone is DERIVED from it by the domain rule
-- at read/order time and never stored, so a future mapping
-- change moves every saved address at once and no stored
-- zone can disagree with the district beside it.
--
-- `is_default` marks the one address checkout pre-selects.
-- The partial UNIQUE index below makes "at most one
-- default per user" a database fact, not a service hope;
-- the service still switches the flag inside one
-- transaction so a switch never transiently violates it.
--
-- `last_used_at` is stamped by checkout whenever an order
-- is placed against the address; the list orders by it
-- (after the default) so the address a buyer actually
-- ships to floats to the top.
--
-- Deleting the user removes their addresses with it (the
-- book is account data, like the wishlist and follows).

CREATE TABLE buyer_addresses (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    label text NOT NULL,
    recipient_name text NOT NULL,
    phone text NOT NULL,
    district text NOT NULL,
    area text NOT NULL,
    address_line text NOT NULL,
    is_default boolean NOT NULL DEFAULT false,
    last_used_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX buyer_addresses_user_idx
    ON buyer_addresses (user_id, created_at DESC);

-- At most one default address per user, enforced by the
-- database itself.
CREATE UNIQUE INDEX buyer_addresses_one_default_per_user
    ON buyer_addresses (user_id) WHERE is_default;
