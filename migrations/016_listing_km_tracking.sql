-- 016_listing_km_tracking.sql
-- Per-listing odometer + service interval. Driver phone POSTs actual km driven
-- (from local GPS path) after each completed ride; backend increments the
-- listing's km_driven_total. Frontend computes service-due = next multiple of
-- service_interval_km above km_driven_total.

ALTER TABLE driver_listings ADD COLUMN service_interval_km INTEGER NOT NULL DEFAULT 5000;
ALTER TABLE driver_listings ADD COLUMN km_driven_total INTEGER NOT NULL DEFAULT 0;
