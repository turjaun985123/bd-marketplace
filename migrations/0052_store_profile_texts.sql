-- Wave 54: store profile texts. The storefront v2 shows
-- two seller-written long texts next to the Wave 2
-- `description` (the short summary): `about` — who the
-- store is, its story, its sourcing — and `return_policy`
-- — the store's own returns wording, shown beside the
-- platform's returns flow so a buyer reads the terms
-- BEFORE ordering. Both are plain text, validated by the
-- domain crate (`validate_store_about` /
-- `validate_store_return_policy`, at most 2,000 characters
-- each) and rendered as text, never markup. NULL means
-- the seller has written nothing; the storefront hides
-- the section entirely rather than showing a placeholder.
ALTER TABLE stores
    ADD COLUMN about text,
    ADD COLUMN return_policy text;
