-- Wave 59: store blog posts — the blog half of the
-- store content system. A post belongs to exactly one
-- store; its slug is unique WITHIN that store.
--
-- The lifecycle is the domain's content state machine
-- (draft / published / archived). Publishing stamps
-- `published_at`; a published post whose `published_at`
-- is still in the future is SCHEDULED — committed but
-- not yet public (domain `content_is_live`). Unpublishing
-- (back to draft) clears `published_at` to NULL, so a
-- re-publish always takes a fresh moment unless the
-- seller names one. Archiving keeps the stamp as the
-- record of when the post was last live.
--
-- `excerpt` is the seller's hand-written teaser; NULL
-- means the public surfaces derive one from the body
-- (domain `derive_post_excerpt`). `tags` are stored
-- already normalised by the domain rule (lowercase,
-- hyphen-joined, deduplicated). `word_count` is computed
-- at write time from the body so list reads can show
-- the reading-time estimate without loading bodies.
-- `cover_image_url` is either a validated external URL
-- or the public URL of an uploaded cover (the Wave 51
-- media pipeline under a stores/... key); the column
-- holds one URL either way.
--
-- Bodies are PLAIN TEXT (see 0067) — paragraphs, never
-- markup.
CREATE TABLE store_posts (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    store_id uuid NOT NULL REFERENCES stores (id) ON DELETE CASCADE,
    slug text NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    excerpt text NULL,
    tags text[] NOT NULL DEFAULT '{}',
    cover_image_url text NULL,
    status text NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'published', 'archived')),
    published_at timestamptz NULL,
    word_count integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT store_posts_store_slug_unique UNIQUE (store_id, slug)
);

-- The public blog read: one store's live posts, newest
-- first, plus the tag-filtered variant of the same read.
CREATE INDEX store_posts_public_idx
    ON store_posts (store_id, status, published_at DESC);

-- The tag filter (`$tag = ANY(tags)`) over one store's
-- posts.
CREATE INDEX store_posts_tags_idx
    ON store_posts USING gin (tags);
