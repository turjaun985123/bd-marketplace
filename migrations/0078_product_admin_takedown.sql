-- Admin products remake: platform takedown state for a
-- product. Until now the only platform lever on a bad
-- listing was suspending the seller's WHOLE store
-- (migration 0013); admins had no per-product action and
-- no admin product queue at all.
--
-- The takedown is a FLAG, orthogonal to the seller-owned
-- `status` ('draft'/'active'/'archived', migration 0004):
-- a status value would corrupt the seller's own lifecycle
-- state (restore could not know whether the product was
-- draft or active before the takedown), while the flag
-- leaves `status` untouched, so restore returns the
-- product to EXACTLY its seller-chosen state. The two
-- layers are independent on purpose: a takedown must be
-- allowed on a draft/archived product and on a suspended
-- store's product, precisely so it outlives a store
-- reactivation (the store flag coming off must never
-- resurrect a listing the platform removed).
--
-- A product is taken down iff `admin_hidden_at IS NOT
-- NULL`. The reason is REQUIRED at takedown time (the
-- service validates it with the domain's
-- `validate_product_takedown_reason`: trimmed, 1..=500
-- characters — the 0075 suspension-note vocabulary) and
-- is stored here so the seller can see it on their own
-- product list; the acting admin is recorded with the
-- 0072/0074/0075 `*_by_user_id ... ON DELETE SET NULL`
-- pattern, so the actor stamp survives the admin's own
-- account deletion. The full record (reason, actor,
-- prior status, restore note) also lands in
-- `admin_audit_log` in the same transaction; restore
-- clears all three columns only after copying the
-- outgoing reason + hider into the restore audit detail,
-- so clearing never destroys the only copy.
--
-- The consistency CHECK couples the timestamp and the
-- reason so a partial write can never desync the pair
-- (the actor column is deliberately NOT coupled: it
-- legitimately becomes NULL if the admin is deleted).
-- Existing rows are all-NULL, which satisfies the first
-- branch — no backfill, and no reason is ever fabricated
-- for a takedown that has not happened.
ALTER TABLE products
    ADD COLUMN admin_hidden_at timestamptz,
    ADD COLUMN admin_hidden_by_user_id uuid
        REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN admin_hidden_reason text
        CHECK (admin_hidden_reason IS NULL
               OR char_length(admin_hidden_reason) BETWEEN 1 AND 500),
    ADD CONSTRAINT products_admin_hidden_consistency CHECK (
        (admin_hidden_at IS NULL AND admin_hidden_reason IS NULL)
        OR (admin_hidden_at IS NOT NULL AND admin_hidden_reason IS NOT NULL)
    );

-- The taken-down worklist: newest takedowns first.
-- Partial, like the other queue indexes (0073/0077
-- precedent: plain CREATE INDEX, no CONCURRENTLY —
-- migrations run in one transaction).
CREATE INDEX products_admin_hidden_idx
    ON products (admin_hidden_at DESC, id DESC)
    WHERE admin_hidden_at IS NOT NULL;

-- The admin queue's main shapes: filter by seller
-- status, newest/oldest-first paging (mirrors
-- payouts_status_created_idx in 0073 and
-- orders_created_at_id_idx in 0077); and the per-store
-- queue (the store detail's "this store's products"
-- jump), which products_store_id_idx (0004) alone
-- cannot serve with a status filter + sort.
CREATE INDEX products_status_created_idx
    ON products (status, created_at DESC, id DESC);

CREATE INDEX products_store_status_created_idx
    ON products (store_id, status, created_at DESC, id DESC);
