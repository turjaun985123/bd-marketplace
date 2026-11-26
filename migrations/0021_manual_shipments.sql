-- Wave 19: manual couriers — Sundarban, SA Paribahan, Karatoa and
-- Carrybee have NO public merchant APIs, so they join the shipment
-- system as a seller-driven manual workflow behind the same tables:
-- a manual booking is a `shipments` row with courier = 'manual',
-- the courier's display name in `manual_courier_name`, and the
-- counter receipt number in `manual_tracking_id` once the seller
-- records it. The receipt number is also mirrored into
-- `consignment_id` (which is NOT NULL): until the seller records
-- one, `consignment_id` carries a generated `MANUAL-…`
-- placeholder, so a manual shipment without a receipt number is
-- visibly different from one that has it. Statuses move through
-- the same unified set and the same domain transition table as
-- API-courier shipments — the seller enters them by hand, and the
-- `shipment_events` rows they write carry raw_status 'manual'.

ALTER TABLE shipments ADD COLUMN manual_courier_name text;
ALTER TABLE shipments ADD COLUMN manual_tracking_id text;
