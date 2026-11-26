-- Wave 18: store branding. A storefront's look is a small set of
-- validated values, never seller-supplied markup: `theme_key`
-- picks one of the three layout variants the web app renders
-- (CHECK mirrors the domain crate's THEME_KEYS), `brand_color`
-- is one #RRGGBB accent (domain `validate_hex_color` normalizes
-- it to uppercase before storage), `logo_url` / `banner_url`
-- are hotlinked image URLs (domain `validate_branding_url` —
-- there is no image upload/storage yet), and `announcement` is
-- the one-line banner text (domain `validate_announcement`,
-- at most 120 characters). The store `description` column from
-- migration 0002 is reused as-is; it is NOT re-added here.
ALTER TABLE stores
    ADD COLUMN theme_key text NOT NULL DEFAULT 'classic'
        CHECK (theme_key IN ('classic', 'minimal', 'bold')),
    ADD COLUMN brand_color text,
    ADD COLUMN logo_url text,
    ADD COLUMN banner_url text,
    ADD COLUMN announcement text;
