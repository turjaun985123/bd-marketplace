-- Wave 55: two order-level facts checkout now records.
--
-- `order_note` — the buyer's free-text note to the
-- sellers on this order ("বাসায় না থাকলে গার্ডের কাছে
-- দিন" — leave it with the guard), capped at 500
-- characters by the domain rule
-- (`domain::validate_order_note`). NULL when the buyer
-- wrote nothing; the seller surfaces (sub-order list,
-- fulfillment queue, labels) read it from the parent
-- order exactly like the delivery fields they already
-- join for.
--
-- `delivery_district` — the Bangladesh district the
-- order ships to, when the platform KNOWS it: the
-- district of the saved address the buyer checked out
-- with, or the district the buyer picked in the checkout
-- district select (whose zone then derives from the
-- district table). Zone-only checkouts (the pre-Wave-55
-- flow) leave it NULL — the column claims a district
-- only when one was actually given, so the admin
-- per-district aggregate built on it never invents
-- geography for orders that never named one.

ALTER TABLE orders ADD COLUMN order_note text;
ALTER TABLE orders ADD COLUMN delivery_district text;

-- The admin orders-per-district aggregate reads by
-- district; the partial index keeps it off the NULL rows.
CREATE INDEX orders_delivery_district_idx
    ON orders (delivery_district) WHERE delivery_district IS NOT NULL;
