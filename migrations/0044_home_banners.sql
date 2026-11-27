-- Wave 52: home banners — the merchandising hero slots of the
-- marketplace home page. Until this wave the home page could
-- only list products: there was no data model for a campaign
-- banner at all, which is most of why the storefront read as a
-- database dump next to Daraz / AliExpress.
--
-- One row per banner. The image is either an UPLOADED object
-- (`storage_key` set: the base key in the media store, uploaded
-- through the Wave 51 pipeline under `site/banners/<id>`, with
-- the original's `width` / `height` stored so display URLs are
-- computed by the same rule as product images) or an EXTERNAL
-- hotlink (`external_image_url` set). The CHECK keeps the two
-- sources mutually exclusive — a banner has at most one image
-- source, exactly like `product_images`. Unlike product
-- images, BOTH may be null: a banner is created first and its
-- image uploaded second (the upload is a separate endpoint),
-- and a text-only banner is a legitimate campaign slot.
--
-- Visibility is a rule, not a stored flag: `is_active` AND the
-- optional window contains now, half-open [starts_at, ends_at)
-- — see the domain crate's `banner_is_visible`. `position`
-- orders the carousel (lowest first).

CREATE TABLE home_banners (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    title text NOT NULL,
    subtitle text NULL,
    link_url text NULL,
    storage_key text NULL,
    width integer NULL,
    height integer NULL,
    external_image_url text NULL,
    position integer NOT NULL DEFAULT 0,
    starts_at timestamptz NULL,
    ends_at timestamptz NULL,
    is_active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT home_banners_one_image_source
        CHECK (NOT (storage_key IS NOT NULL AND external_image_url IS NOT NULL)),
    CONSTRAINT home_banners_window_order
        CHECK (starts_at IS NULL OR ends_at IS NULL OR ends_at > starts_at)
);

-- The home feed reads visible banners in position order.
CREATE INDEX home_banners_visible_position
    ON home_banners (is_active, position, created_at);
