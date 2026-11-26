-- Wave 17: partial advance for COD + the COD fraud shield.
-- A COD order whose grand total reaches the platform threshold (or
-- whose buyer the risk shield flags) must collect a partial advance
-- online before any parcel ships. The two new columns snapshot the
-- policy outcome at checkout: how much advance the order requires,
-- and how much has actually been collected (set only by the signed
-- payment callback path — never at checkout). Both default to 0,
-- which is every pre-Wave-17 order and every ordinary COD order.
-- orders.payment_status gains the two advance states:
-- 'advance_pending' (advance required, not yet collected) and
-- 'advance_paid' (advance collected; the balance is still COD and
-- is collected by the courier on delivery). The CHECK constraint
-- added inline by migration 0009 carries Postgres' auto-generated
-- name for a column check on this table.
-- All money is integer poisha (bigint).

ALTER TABLE orders
    ADD COLUMN advance_required_poisha bigint NOT NULL DEFAULT 0
        CHECK (advance_required_poisha >= 0),
    ADD COLUMN advance_paid_poisha bigint NOT NULL DEFAULT 0
        CHECK (advance_paid_poisha >= 0);

ALTER TABLE orders DROP CONSTRAINT orders_payment_status_check;

ALTER TABLE orders
    ADD CONSTRAINT orders_payment_status_check
        CHECK (payment_status IN ('pay_on_delivery', 'pending', 'paid',
                                  'failed', 'refunded',
                                  'advance_pending', 'advance_paid'));
