-- Wave 34: store members — a store gains a team. Until now the
-- only person who could work in a store was its owner
-- (`stores.owner_user_id`); every seller surface resolved "the
-- caller's store" through ownership alone. This table adds the
-- member rows: another user, plus the role they hold IN THIS
-- STORE (`manager` or `staff` — the permission matrix itself
-- lives in the domain crate as `store_role_can`).
--
-- `owner` is deliberately NOT a storable role: ownership stays
-- on the store row, so there is exactly one owner per store and
-- no member write can mint a second one. The CHECK enforces
-- that at the database level too.
--
-- The composite primary key makes duplicate membership
-- impossible by construction; the API still pre-checks so it
-- can answer a clean 409 instead of a constraint error.
--
-- The user index serves the member's own lookups ("which stores
-- do I work in?" — the seller stores list and every
-- accessible-stores scope query filter by user first).
CREATE TABLE store_members (
    store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role text NOT NULL CHECK (role IN ('manager', 'staff')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (store_id, user_id)
);

CREATE INDEX idx_store_members_user ON store_members (user_id);
