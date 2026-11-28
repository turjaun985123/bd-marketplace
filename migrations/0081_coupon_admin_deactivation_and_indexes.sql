-- Admin coupons remake: admin-deactivation stamps on
-- coupons, plus the rebuilt queue's indexes.
--
-- Until now an admin could flip ANY coupon's `active`
-- flag (platform or a seller's store coupon) through one
-- unaudited UPDATE: no transaction, no actor, no reason,
-- no audit row, and the seller could not even tell their
-- own deactivation from the platform's. The remade admin
-- toggle is a locked, audited transition; these columns
-- are where its reason stands, in the 0078 products
-- pattern:
--
-- * `admin_deactivated_at` is the marker: a coupon is
--   ADMIN-deactivated iff it is NOT NULL. The seller's
--   own deactivation (the pre-remake shared toggle) never
--   sets it, so the two stay distinguishable.
-- * The reason is REQUIRED at admin-deactivation time
--   (validated by the domain's
--   `validate_coupon_deactivation_reason`: trimmed,
--   1..=500 characters) and stored here so the seller
--   can see it on their own coupons page; the acting
--   admin follows the 0072/0074/0075/0078
--   `*_by_user_id ... ON DELETE SET NULL` pattern, so
--   the stamp survives the admin's own account deletion.
-- * Reactivation (by admin or seller) clears all three
--   columns, but only after the outgoing reason + actor
--   are copied into the reactivation's audit detail, so
--   clearing never destroys the only copy.
-- * The consistency CHECK couples the timestamp and the
--   reason so a partial write can never desync the pair
--   (the actor column is deliberately NOT coupled: it
--   legitimately becomes NULL if the admin is deleted).
--   Existing rows are all-NULL, which satisfies the
--   first branch — no backfill, and no reason is ever
--   fabricated for a deactivation that has not happened.
ALTER TABLE coupons
    ADD COLUMN admin_deactivated_at timestamptz,
    ADD COLUMN admin_deactivated_by_user_id uuid
        REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN admin_deactivated_reason text
        CHECK (admin_deactivated_reason IS NULL
               OR char_length(admin_deactivated_reason) BETWEEN 1 AND 500),
    ADD CONSTRAINT coupons_admin_deactivated_consistency CHECK (
        (admin_deactivated_at IS NULL AND admin_deactivated_reason IS NULL)
        OR (admin_deactivated_at IS NOT NULL AND admin_deactivated_reason IS NOT NULL)
    );

-- The remade queue's indexes, in the established style
-- (0073/0076/0077/0080: plain CREATE INDEX, no
-- CONCURRENTLY — migrations run inside one transaction).
--
-- * `coupons (created_at, id)` — the default
--   newest-first page order WITH the id tiebreaker the
--   deterministic-order rule requires; the pre-remake
--   admin list ordered by created_at alone, so pages
--   could repeat or skip rows sharing a timestamp. The
--   queue's filters (scope, discount type, and a
--   lifecycle derived from active / starts_at / ends_at
--   / usage_limit vs used_count) are low-cardinality or
--   derived, so — like the order queue's parent-status
--   split (0077) — they deliberately get no index of
--   their own and are applied while walking the sort
--   order. The 0016 seller index stays.
-- * `coupon_redemptions (coupon_id, created_at, id)` —
--   the coupon detail's recent-redemptions list: one
--   coupon's redemptions newest first. Redemptions grow
--   with orders (one row per coupon checkout), and the
--   0016 index on coupon_id alone must sort a popular
--   coupon's whole history per read. It stays — this
--   composite extends it, it does not replace it (the
--   0080 shipment_events pattern).
CREATE INDEX coupons_created_at_id_idx
    ON coupons (created_at DESC, id DESC);

CREATE INDEX coupon_redemptions_coupon_created_idx
    ON coupon_redemptions (coupon_id, created_at DESC, id DESC);
