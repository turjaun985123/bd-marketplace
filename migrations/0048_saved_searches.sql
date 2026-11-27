-- Wave 53: saved searches — a signed-in buyer may bookmark
-- a faceted-browse filter set under a name and re-apply it
-- later. The filters are stored as the CANONICAL validated
-- shape (the JSON the API itself returns), so a stored row
-- can never carry a filter the live endpoint would refuse:
-- the write path validates through the same domain
-- validator as `GET /v1/search` before storing.
--
-- Account data like the wishlist (Wave 24): it follows the
-- user, so it lives server-side — the deliberate opposite
-- of the device-local recently-viewed history.

CREATE TABLE saved_searches (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name text NOT NULL,
    filters jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- The list read is "my searches, newest first".
CREATE INDEX saved_searches_user_created
    ON saved_searches (user_id, created_at DESC);
