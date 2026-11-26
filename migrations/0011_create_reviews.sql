-- Wave 8: reviews + seller replies. A review belongs to a product and
-- to the buyer who wrote it; `order_id` records WHICH delivered order
-- proved eligibility (the buyer must have received the product in a
-- delivered sub-order — see the API's find_delivered_order_for_review).
-- One review per buyer per product (UNIQUE below); editing goes
-- through the same row. Ratings are whole stars, 1 to 5.
-- A review has at most one seller reply, written by the owner of the
-- store that sells the product; the reply row dies with its review.

CREATE TABLE reviews (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    buyer_id uuid NOT NULL REFERENCES users (id),
    order_id uuid NOT NULL REFERENCES orders (id),
    rating smallint NOT NULL CHECK (rating BETWEEN 1 AND 5),
    body text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (product_id, buyer_id)
);

CREATE INDEX reviews_product_id_idx ON reviews (product_id, created_at DESC);
CREATE INDEX reviews_buyer_id_idx ON reviews (buyer_id);

CREATE TABLE review_replies (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    review_id uuid NOT NULL REFERENCES reviews (id) ON DELETE CASCADE,
    store_id uuid NOT NULL REFERENCES stores (id),
    body text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (review_id)
);
