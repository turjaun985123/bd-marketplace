-- Wave 4: product weight. Delivery charges are weight-based from this
-- wave on: checkout sums each product's weight_grams times quantity per
-- seller group and bills per started kilogram (domain:
-- weight_kg_from_grams + calculate_delivery_charge). Existing products
-- get the historical default of 1,000 g (the Wave 3 assumption), so old
-- rows keep behaving as before until sellers set real weights.
ALTER TABLE products
    ADD COLUMN weight_grams int NOT NULL DEFAULT 1000 CHECK (weight_grams > 0);
