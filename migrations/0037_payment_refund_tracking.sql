-- Wave 41 (security audit HI-52): payments learn how much of
-- them has been refunded, and when the refund completed.
--
-- `refunded` has been a legal payment/order status since
-- migration 0009 and `paid -> refunded` a legal transition in
-- the domain table since Wave 5, but no code ever moved a
-- payment there: a return refund recorded `done` off-platform
-- left `payments.status = 'paid'` and
-- `orders.payment_status = 'paid'` behind, so everything that
-- keys off `paid` (analytics, payout reasoning, COD netting)
-- treated refunded money as still collected.
--
-- `refund_amount_poisha` is the running total of the return
-- refunds recorded against this payment by the admin
-- refund-done path (the returns module) — the exact poisha
-- figure, accumulated per return, so a PARTIAL refund (one
-- sub-order of several returned) is representable instead of
-- forcing an all-or-nothing status. `refunded_at` stamps the
-- moment the payment's order became fully refunded (every
-- sub-order returned with its refund done) and the status
-- flipped to `refunded`. No gateway refund API is wired by
-- this migration — the amounts record off-platform refunds,
-- the same honesty rule the returns flow has always used.

ALTER TABLE payments
    ADD COLUMN refund_amount_poisha bigint NOT NULL DEFAULT 0
        CHECK (refund_amount_poisha >= 0),
    ADD COLUMN refunded_at timestamptz;
