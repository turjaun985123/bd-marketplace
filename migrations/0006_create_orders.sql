-- Wave 3: orders — the buyer's single unified order (one checkout).
-- The per-seller split lives in sub_orders (migration 0007). All money
-- is integer poisha (bigint). Wave 3 accepts COD only; payment_method is
-- stored now so later waves can add bKash/Nagad without a schema change.
-- delivery_zone stores the snake_case zone string (inside_dhaka /
-- dhaka_suburb / outside_dhaka) — see domain::parse_delivery_zone.

CREATE TABLE orders (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    buyer_user_id uuid NOT NULL REFERENCES users (id),
    status text NOT NULL DEFAULT 'placed'
        CHECK (status IN ('placed', 'confirmed', 'cancelled', 'delivered')),
    payment_method text NOT NULL DEFAULT 'cod',
    delivery_name text NOT NULL,
    delivery_phone text NOT NULL,
    delivery_address text NOT NULL,
    delivery_zone text NOT NULL
        CHECK (delivery_zone IN ('inside_dhaka', 'dhaka_suburb', 'outside_dhaka')),
    goods_total_poisha bigint NOT NULL CHECK (goods_total_poisha >= 0),
    delivery_total_poisha bigint NOT NULL CHECK (delivery_total_poisha >= 0),
    grand_total_poisha bigint NOT NULL CHECK (grand_total_poisha >= 0),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX orders_buyer_user_id_idx ON orders (buyer_user_id, created_at DESC);
