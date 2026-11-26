-- Wave 32: performance indexes for the query shapes the API
-- actually runs today. Every index below was checked against
-- the full migration history (0001-0029) first: the hot paths
-- that already carry an index (orders by buyer, sub_orders by
-- order / by store+created, order_items by sub_order, reviews
-- by product, messages by conversation, wishlist by product,
-- stock_adjustments by product, notifications by status) are
-- NOT repeated here. These four are the gaps:
--
-- Plain CREATE INDEX inside the migration transaction: at the
-- current catalogue size that is a sub-second lock. On a large
-- production table the same indexes should be built with
-- CREATE INDEX CONCURRENTLY instead (which cannot run inside
-- a transaction, so it would ship as an ops step, not a
-- migration) — noted here so the future path is explicit.

-- Seller top-products analytics (Wave 27) groups order_items
-- by product, and the also-bought rail (Wave 24) probes
-- order_items by product inside an EXISTS. Until now the only
-- order_items index was by sub_order.
CREATE INDEX IF NOT EXISTS idx_order_items_product
    ON order_items (product_id);

-- The Wave 30 worker poller sweeps shipments by status set +
-- stale updated_at, oldest first, every cycle. Until now the
-- only shipments index was by consignment id.
CREATE INDEX IF NOT EXISTS idx_shipments_status_updated
    ON shipments (status, updated_at);

-- Seller earnings and dashboard counts filter sub_orders by
-- store AND status (earned = store + 'delivered'). The
-- existing store index is (store_id, created_at DESC), which
-- cannot answer the status equality.
CREATE INDEX IF NOT EXISTS idx_sub_orders_store_status
    ON sub_orders (store_id, status);

-- The public question list (Wave 25) reads one product's
-- visible questions newest first: product + NOT is_hidden,
-- ordered by created_at. The existing index leads with
-- (product_id, created_at DESC) and filters hidden rows only
-- after the sort.
CREATE INDEX IF NOT EXISTS idx_product_questions_product_hidden
    ON product_questions (product_id, is_hidden, created_at DESC);
