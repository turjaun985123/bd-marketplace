-- Wave 59: store post view events — the aggregate-only
-- analytics for the store blog, in the exact pattern of
-- the Wave 58 storefront events (0066) and the Wave 52
-- merch events before them. One row per public post
-- READ, recorded by an anonymous beacon when the post
-- page mounts; the only kind is `post_view`.
--
-- Aggregate-only is the privacy rule: no user, no
-- session, no IP is ever recorded — the seller's stats
-- read counts rows (all-time and the trailing window),
-- never people. Deleting the post removes its events
-- with it (the counts were the post's own history).
CREATE TABLE store_post_events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    post_id uuid NOT NULL REFERENCES store_posts (id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('post_view')),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- The seller stats read: one post's events inside a
-- time window.
CREATE INDEX store_post_events_post_created_idx
    ON store_post_events (post_id, created_at DESC);
