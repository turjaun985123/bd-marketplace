-- Wave 51: one photo per review, through the same media
-- pipeline as product images (the api crate's `media`
-- module). The photo is stored under a review-scoped base
-- key (`reviews/<review_id>/photo`); only the key and the
-- original's dimensions live on the review row, and the
-- public review view derives the display URL from them
-- exactly like product views derive theirs. A review with
-- no photo keeps all three columns NULL — the columns are
-- the whole feature's footprint on the table.
--
-- Replacing a review photo overwrites the same objects and
-- updates these columns; deleting the photo (or the review)
-- removes the objects best-effort from the API layer, since
-- the store is not transactional with the database.

ALTER TABLE reviews
    ADD COLUMN image_storage_key text NULL,
    ADD COLUMN image_width integer NULL,
    ADD COLUMN image_height integer NULL;
