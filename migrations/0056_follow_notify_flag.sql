-- Wave 54 (extension): per-follow notification mute. A
-- follow normally subscribes the buyer to the store's
-- new-product notices (the Wave 54 fan-out); this flag
-- lets the buyer keep the follow (the store stays on
-- their /following shelf and in their feed) while
-- silencing the notices. Default true = subscribed, so
-- every existing follow keeps its Wave 54 behaviour.
ALTER TABLE store_follows
    ADD COLUMN notify_new_products boolean NOT NULL DEFAULT true;
