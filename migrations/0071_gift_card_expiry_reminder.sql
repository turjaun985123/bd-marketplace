-- Wave 60 extension: the worker's gift-card expiry sweep
-- stamps a card when it has reminded the issuing seller
-- about the card's approaching expiry, so each card is
-- reminded about exactly once. NULL = never reminded.
-- The stamp lives on the card (not in a side table)
-- because the sweep's dedupe question — "have we told
-- anyone about THIS card's expiry?" — is a fact about
-- the card itself, and expiry never changes after
-- issuance in this design, so one stamp is exact.
ALTER TABLE gift_cards ADD COLUMN expiry_reminded_at TIMESTAMPTZ;
