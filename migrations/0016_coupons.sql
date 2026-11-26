-- Wave 14: coupons and promotions.
-- A coupon is either a PLATFORM coupon (store_id NULL: discounts
-- the whole order's goods, the platform bears the discount, so
-- commission stays on the full goods) or a STORE coupon (discounts
-- only that store's goods subtotal, the seller bears it, so
-- commission is computed on the discounted goods). Codes are
-- stored in their normalized (upper-case) form — the UNIQUE
-- constraint is on that form, so codes are case-insensitive.
-- used_count is a denormalized counter incremented inside the
-- checkout transaction (the coupon row is locked FOR UPDATE, so
-- concurrent checkouts cannot both pass the usage-limit check);
-- coupon_redemptions is the per-order audit trail (one row per
-- order, UNIQUE on order_id).
-- Orders and sub_orders snapshot the applied discount: the order
-- carries the code and the total, each sub_order carries its
-- allocated share (allocated proportionally by the pure domain
-- function allocate_discount), and sub_orders.seller_payout_poisha
-- already nets the discount out (goods - discount - commission).

CREATE TABLE coupons (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    code text NOT NULL UNIQUE,
    store_id uuid REFERENCES stores (id),
    discount_type text NOT NULL CHECK (discount_type IN ('percent', 'fixed')),
    discount_value bigint NOT NULL CHECK (discount_value > 0),
    min_order_poisha bigint NOT NULL DEFAULT 0 CHECK (min_order_poisha >= 0),
    max_discount_poisha bigint CHECK (max_discount_poisha >= 0),
    starts_at timestamptz,
    ends_at timestamptz,
    usage_limit int CHECK (usage_limit > 0),
    used_count int NOT NULL DEFAULT 0 CHECK (used_count >= 0),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX coupons_store_id_idx ON coupons (store_id, created_at DESC);

CREATE TABLE coupon_redemptions (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    coupon_id uuid NOT NULL REFERENCES coupons (id),
    order_id uuid NOT NULL UNIQUE REFERENCES orders (id),
    buyer_id uuid NOT NULL REFERENCES users (id),
    discount_poisha bigint NOT NULL CHECK (discount_poisha >= 0),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX coupon_redemptions_coupon_id_idx ON coupon_redemptions (coupon_id);

ALTER TABLE sub_orders
    ADD COLUMN discount_poisha bigint NOT NULL DEFAULT 0
        CHECK (discount_poisha >= 0);

ALTER TABLE orders
    ADD COLUMN coupon_code text,
    ADD COLUMN discount_total_poisha bigint NOT NULL DEFAULT 0
        CHECK (discount_total_poisha >= 0);
