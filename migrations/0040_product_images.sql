-- Wave 51: product images — the media system the storefront
-- parity program stands on. Until this wave the data model
-- had NO product image field at all (found in Wave 32, left
-- untouched there): every product surface rendered text only,
-- which is the single largest visible gap against any real
-- marketplace.
--
-- One row per image. An image is either an UPLOADED object
-- (`storage_key` set: the base key in the media store, from
-- which the original + derived-size object keys are computed
-- deterministically — see the api crate's `media` module) or
-- an EXTERNAL hotlink (`external_url` set: Facebook import
-- photos and URL-based sellers). The CHECK keeps exactly one
-- of the two non-null, so a row can never be both or neither.
--
-- `variant_id` optionally ties the image to one variant of
-- the same product (a red shoe's photo belongs to the red
-- variant). It references `product_variants` (migration
-- 0031) with ON DELETE SET NULL — variants are only ever
-- soft-deleted in practice, but if a row ever goes, the
-- image falls back to a plain product image instead of
-- vanishing with it.
--
-- `position` orders the gallery (0 first); `is_primary`
-- marks the one image every card/list surface shows. The
-- partial UNIQUE index enforces at most one primary per
-- product at the database level; the API's write paths keep
-- exactly one primary whenever a product has images (the
-- first upload becomes primary, deleting the primary
-- promotes the next by position — see the domain crate's
-- `normalize_image_order`).
--
-- `width` / `height` / `size_bytes` describe the ORIGINAL
-- upload (null for external images, whose bytes the
-- platform never sees). The derived sizes are planned from
-- the dimensions by the domain rule, so they are not stored.

CREATE TABLE product_images (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    variant_id uuid NULL REFERENCES product_variants (id) ON DELETE SET NULL,
    storage_key text NULL,
    external_url text NULL,
    width integer NULL,
    height integer NULL,
    size_bytes bigint NULL,
    position integer NOT NULL DEFAULT 0,
    is_primary boolean NOT NULL DEFAULT false,
    alt_text text NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT product_images_exactly_one_source
        CHECK ((storage_key IS NOT NULL) <> (external_url IS NOT NULL))
);

-- At most one primary image per product.
CREATE UNIQUE INDEX product_images_one_primary
    ON product_images (product_id) WHERE is_primary;

-- Gallery reads are always "this product's images, in order".
CREATE INDEX product_images_product_position
    ON product_images (product_id, position);
