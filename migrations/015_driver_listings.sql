-- 015_driver_listings.sql
-- One row per driver listing. A driver can have multiple listings
-- (e.g. one for their Hilux, one for their Benz, one for their Sienta).
-- Listings are mirrored from Nostr kind:30402 events. Nostr remains the
-- source of truth; this table is a fast-query cache for discovery.

CREATE TABLE IF NOT EXISTS driver_listings (
  id                TEXT PRIMARY KEY,           -- the listing id (matches Nostr 'd' tag)
  driver_pubkey     TEXT NOT NULL,
  driver_npub       TEXT,                       -- bech32 form for display
  listing_name      TEXT,                       -- e.g. "Dodi Rides"
  vehicle           TEXT,                       -- e.g. "Toyota Hilux"
  vehicle_type      TEXT,                       -- sedan|suv|pickup|4x4|minibus|van
  seats             INTEGER DEFAULT 4,
  price_per_km      INTEGER DEFAULT 500,        -- sats
  ride_categories   TEXT,                       -- comma-separated: city,intercity,tourist,selfdrive
  photo_urls        TEXT,                       -- JSON array of urls
  description       TEXT,
  location_country  TEXT,
  location_city     TEXT,
  lud16             TEXT,                       -- Lightning address for this listing
  nostr_event_id    TEXT,                       -- the kind:30402 event id, for audit
  created_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL,
  deleted_at        INTEGER                     -- soft delete (NULL = active)
);

-- Discovery: nearby_listings will JOIN this with driver_locations on driver_pubkey
CREATE INDEX IF NOT EXISTS idx_driver_listings_driver
  ON driver_listings(driver_pubkey);

-- Filtering by category/vehicle type
CREATE INDEX IF NOT EXISTS idx_driver_listings_categories
  ON driver_listings(ride_categories);

CREATE INDEX IF NOT EXISTS idx_driver_listings_vehicle_type
  ON driver_listings(vehicle_type);

-- Soft delete: most queries should WHERE deleted_at IS NULL
CREATE INDEX IF NOT EXISTS idx_driver_listings_active
  ON driver_listings(deleted_at);
