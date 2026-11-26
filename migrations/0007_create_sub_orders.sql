-- Wave 3: sub_orders + order_items — the per-seller split of an order.
-- Each sub_order carries its own fulfilment status and its own money
-- breakdown (goods subtotal, delivery charge, platform commission,
-- seller payout), all computed by the pure domain functions
-- (build_order_totals / compute_commission) at checkout time.
-- order_items snapshot title and unit price so later product edits can
-- never rewrite a placed order's history.

CREATE TABLE sub_orders (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id uuid NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    store_id uuid NOT NULL REFERENCES stores (id),
    status text NOT NULL DEFAULT 'placed'
        CHECK (status IN ('placed', 'confirmed', 'shipped', 'delivered', 'returned', 'cancelled')),
    goods_subtotal_poisha bigint NOT NULL CHECK (goods_subtotal_poisha >= 0),
    delivery_charge_poisha bigint NOT NULL CHECK (delivery_charge_poisha >= 0),
    commission_poisha bigint NOT NULL CHECK (commission_poisha >= 0),
    seller_payout_poisha bigint NOT NULL CHECK (seller_payout_poisha >= 0),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX sub_orders_order_id_idx ON sub_orders (order_id);
CREATE INDEX sub_orders_store_id_idx ON sub_orders (store_id, created_at DESC);

CREATE TABLE order_items (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    sub_order_id uuid NOT NULL REFERENCES sub_orders (id) ON DELETE CASCADE,
    product_id uuid NOT NULL REFERENCES products (id),
    title_snapshot text NOT NULL,
    unit_price_poisha bigint NOT NULL CHECK (unit_price_poisha >= 0),
    quantity int NOT NULL CHECK (quantity > 0),
    line_total_poisha bigint NOT NULL CHECK (line_total_poisha >= 0)
);

CREATE INDEX order_items_sub_order_id_idx ON order_items (sub_order_id);
