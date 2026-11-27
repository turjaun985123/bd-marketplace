-- Wave 44: payout requests learn an optional client-supplied
-- `Idempotency-Key` — the third element of the security
-- audit's payout prescription (CR-10 in the audit's full
-- list), deferred by Wave 39 once the double-spend race
-- itself was closed by the store-row lock.
--
-- A seller whose browser double-submits the payout form, or
-- whose client retries the POST after a network hiccup, used
-- to create a second `requested` row an admin then had to
-- reject by hand. With a key stored on the row, the API can
-- recognise the retry, hand back the ORIGINAL payout, and
-- refuse a key reused for a different amount — see
-- `request_payout` in the payouts module for the contract.
--
-- Scope: the key is unique per STORE, not globally. The
-- payouts table has no seller-user column — a payout belongs
-- to a store, and the request path is owner-only
-- (`manage_payouts`, Wave 34), resolving exactly the owner's
-- payout store, so (store_id, key) is exactly "this seller's
-- request with this key". The partial UNIQUE index (WHERE
-- the key is present) keeps keyless rows — still the
-- majority, and byte-for-byte the pre-Wave-44 behaviour —
-- entirely outside the rule, and it is the race backstop
-- behind the service's lookup: two concurrent same-key
-- inserts cannot both land even if a future code path ever
-- reaches the insert without the store lock.
--
-- The CHECK mirrors the domain rule
-- (`domain::validate_idempotency_key`: 1..=128 visible-ASCII
-- characters) at the storage layer, the same defence-in-depth
-- shape migration 0037 uses for its refund total. A key
-- lives exactly as long as its payout row: no expiry, no
-- separate key table.

ALTER TABLE payouts
    ADD COLUMN idempotency_key text
        CHECK (idempotency_key IS NULL
               OR char_length(idempotency_key) BETWEEN 1 AND 128);

CREATE UNIQUE INDEX payouts_store_idempotency_unique
    ON payouts (store_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
