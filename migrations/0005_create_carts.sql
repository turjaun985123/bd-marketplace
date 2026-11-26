-- Wave 3: carts. One cart per buyer (user_id is UNIQUE); cart_items join
-- the cart to products. Quantity is bounded by the domain rule
-- (1..=99, enforced in the service layer) and the DB CHECK (> 0).

CREATE TABLE carts (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL UNIQUE REFERENCES users (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE cart_items (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    cart_id uuid NOT NULL REFERENCES carts (id) ON DELETE CASCADE,
    product_id uuid NOT NULL REFERENCES products (id),
    quantity int NOT NULL CHECK (quantity > 0),
    UNIQUE (cart_id, product_id)
);

CREATE INDEX cart_items_cart_id_idx ON cart_items (cart_id);
