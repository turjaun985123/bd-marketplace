-- Wave 55: save-for-later. A cart line gains a `saved`
-- flag: the line stays the buyer's (same row, same
-- (cart, product, variant) identity under the Wave 33
-- NULLS NOT DISTINCT unique index) but leaves the ACTIVE
-- cart — checkout locks, totals and clears only
-- `saved = false` lines, and the cart view returns the
-- saved lines as their own array. Moving a line between
-- the cart and the shelf is an UPDATE of this one flag;
-- adding a product whose line sits on the shelf revives
-- the line into the active cart (the add path clears the
-- flag), so a product can never exist twice for a buyer
-- in two states at once.

ALTER TABLE cart_items ADD COLUMN saved boolean NOT NULL DEFAULT false;

-- The cart read splits lines by the flag; the checkout
-- lock scan filters on it too.
CREATE INDEX cart_items_cart_saved_idx ON cart_items (cart_id, saved);
