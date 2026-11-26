-- Wave 15: product categories + the search indexes.
--
-- `category` is a free-text seller label (validated in the domain
-- crate: optional, at most 60 characters) — deliberately NOT a
-- taxonomy table yet; search filters on the label (case-insensitive
-- exact match). Existing rows stay NULL = uncategorised.
ALTER TABLE products ADD COLUMN category text;

-- The Wave 15 search rank reads title and title_bn together, so
-- one multicolumn GIN trigram index covers both columns. The
-- per-column title indexes from migration 0004 stay: the Wave 2
-- listing predicates use them. The description index backs the
-- search WHERE clause's description ILIKE arm.
CREATE INDEX products_search_trgm_idx ON products
    USING gin (title gin_trgm_ops, title_bn gin_trgm_ops);
CREATE INDEX products_description_trgm_idx ON products
    USING gin (description gin_trgm_ops);
