-- Wave 10: platform admin. Admin is a FLAG on the user, not a role:
-- a user keeps their buyer/seller role and may additionally be an
-- admin (the `role` column and its CHECK stay untouched). There is
-- deliberately NO endpoint that grants admin — the first admin is
-- promoted by SQL (see the README's Wave 10 section), and admins can
-- then toggle the flag on OTHER users via the admin API (never on
-- themselves, so the platform cannot lock itself out by accident).
ALTER TABLE users
    ADD COLUMN is_admin boolean NOT NULL DEFAULT false;

-- A store is `active` or `suspended`. Suspension is the platform's
-- kill switch for a storefront: its products disappear from the
-- public marketplace, storefront and product detail, and checkout
-- refuses carts containing them — but the seller can still sign in
-- and manage their catalogue, and reactivation restores everything
-- (nothing is deleted).
ALTER TABLE stores
    ADD COLUMN status text NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'suspended'));

-- Platform settings: string key/value rows an admin can change at
-- runtime. A setting row OVERRIDES the matching startup value
-- (environment variable, then the built-in default): today that is
-- `commission_bps` (over COMMISSION_RATE_BPS / 800) and
-- `min_payout_poisha` (over the domain MIN_PAYOUT_POISHA constant).
-- Values are stored as text and parsed by the pure domain function
-- `parse_setting_i64`; an unparseable or out-of-range value is
-- ignored on read, so a bad row can never break checkout.
CREATE TABLE platform_settings (
    key text PRIMARY KEY,
    value text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
