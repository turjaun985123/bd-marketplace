-- Wave 51 (extension): the duplicate-upload guard.
--
-- `content_hash` is the SHA-256 (hex) of the ORIGINAL
-- bytes a seller uploaded, computed by the media pipeline
-- at insert time. The upload paths refuse a second copy
-- of the same bytes on the same product (a bilingual 400)
-- — sellers double-upload the same photo constantly, and
-- every duplicate costs storage and gallery order.
-- External (hotlinked) images hold no bytes and keep the
-- column NULL; they are never duplicate-checked.
--
-- The index supports the guard's lookup; it is NOT
-- unique — the guard is a checked read-then-insert like
-- the wave's other caps, and a unique index would turn a
-- rare concurrent double-submit into a 500 instead of a
-- clean refusal of the second request.
ALTER TABLE product_images ADD COLUMN content_hash text;

CREATE INDEX product_images_product_hash_idx
    ON product_images (product_id, content_hash)
    WHERE content_hash IS NOT NULL;
