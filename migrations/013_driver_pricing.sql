-- Migration 013: Add pricing to driver locations
ALTER TABLE driver_locations ADD COLUMN price_per_km INTEGER NOT NULL DEFAULT 0;
