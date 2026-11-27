-- Wave 41 (security audit HI-53): `gateway_ref` gains the
-- uniqueness the settlement lookup always assumed.
--
-- Until now `payments_gateway_ref_idx` (migration 0009) was a
-- plain index and the webhook lookup resolved duplicates
-- newest-first, so two rows carrying the same reference made
-- settlement depend on insertion order — and nothing stopped a
-- second SETTLEABLE (pending) row from existing for one order.
-- The references stored today are minted by this platform
-- (the DEV simulator's `DEV-<method>-<order id>`, one per order
-- and method; real gateway references are not stored yet —
-- their initiate calls are still unimplemented), so a reference
-- is unique across the whole table, not just within a method,
-- and a plain partial UNIQUE on the reference is the honest
-- scope. The partial form (WHERE gateway_ref IS NOT NULL)
-- keeps any future NULL-reference rows outside the rule.
--
-- The second index closes the other half of the finding: at
-- most one `pending` attempt per order. Checkout inserts
-- exactly one pending row per order (an online attempt XOR a
-- COD advance), and retry flips the SAME row failed -> pending,
-- so a second pending row can only be a bug or tampering.
--
-- OPERATOR NOTE: on a database that already contains duplicate
-- references (or two pending rows for one order), CREATE UNIQUE
-- INDEX fails and migrations stop — resolve the duplicates
-- first (keep the newest row per reference; fail or delete the
-- extra pending rows). Failing loudly is deliberate: silently
-- picking a winner is how the ambiguity started.

CREATE UNIQUE INDEX payments_gateway_ref_unique
    ON payments (gateway_ref)
    WHERE gateway_ref IS NOT NULL;

CREATE UNIQUE INDEX payments_one_pending_per_order
    ON payments (order_id)
    WHERE status = 'pending';
