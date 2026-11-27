-- Wave 56: backfill `products.category_id` from the legacy
-- free-text label.
--
-- The rule is the service layer's resolution rule (domain
-- `resolve_category_link`), applied exactly once, here:
-- a product links when its trimmed label matches a taxonomy
-- node EXACTLY — the slug (`fashion`), the English name
-- case-insensitively (`Fashion`, `fashion`), or the Bangla
-- name (`ফ্যাশন`). No fuzzy matching, no substring matching:
-- a wrong link is worse than no link, and an unmatched
-- product simply keeps `category_id` NULL with its label
-- intact, exactly as it behaved before this wave.
--
-- The single-match guard keeps the backfill deterministic:
-- if a label ever matched two nodes (the seed's names are
-- unique, but an admin could later create a clash), the
-- product is left unlinked rather than attached to an
-- arbitrary one. The text column is NOT rewritten — the
-- label the seller typed stays the display name.
UPDATE products p
SET category_id = c.id
FROM categories c
WHERE p.category_id IS NULL
  AND p.category IS NOT NULL
  AND btrim(p.category) <> ''
  AND (
      lower(btrim(p.category)) = c.slug
      OR lower(btrim(p.category)) = lower(c.name_en)
      OR btrim(p.category) = c.name_bn
  )
  AND (
      SELECT COUNT(*)
      FROM categories c2
      WHERE lower(btrim(p.category)) = c2.slug
         OR lower(btrim(p.category)) = lower(c2.name_en)
         OR btrim(p.category) = c2.name_bn
  ) = 1;
