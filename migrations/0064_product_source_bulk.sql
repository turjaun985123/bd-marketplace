-- Wave 57: bulk CSV imports are a third product provenance.
-- Migration 0014 constrained products.source to ('manual',
-- 'facebook'); the bulk importer (and the demo seed, which rides
-- it) writes source = 'bulk', so the check widens additively.
-- Existing rows are unaffected.
ALTER TABLE products DROP CONSTRAINT products_source_check;
ALTER TABLE products ADD CONSTRAINT products_source_check
    CHECK (source IN ('manual', 'facebook', 'bulk'));
