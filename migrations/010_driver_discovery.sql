-- Migration 010: Driver discovery — GPS presence + ride matching
-- Driver locations: updated every 30s via WebSocket heartbeat
CREATE TABLE IF NOT EXISTS driver_locations (
    pubkey          TEXT PRIMARY KEY,
    npub            TEXT,
    lat             REAL NOT NULL,
    lng             REAL NOT NULL,
    heading         REAL,              -- compass heading in degrees
    speed_kmh       REAL,              -- current speed
    country         TEXT DEFAULT '',
    city            TEXT DEFAULT '',
    vehicle_type    TEXT DEFAULT 'sedan',  -- sedan, suv, pickup, 4x4, minibus, van
    ride_categories TEXT DEFAULT 'city',   -- comma-separated: city,intercity,tourist,self_drive
    seats           INTEGER DEFAULT 4,
    lud16           TEXT DEFAULT '',
    display_name    TEXT DEFAULT '',
    picture_url     TEXT DEFAULT '',
    online          INTEGER DEFAULT 1,
    updated_at      INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX IF NOT EXISTS idx_driver_loc_geo ON driver_locations(lat, lng);
CREATE INDEX IF NOT EXISTS idx_driver_loc_online ON driver_locations(online, updated_at);
CREATE INDEX IF NOT EXISTS idx_driver_loc_country ON driver_locations(country);

-- Ride requests: rider submits, backend matches drivers
CREATE TABLE IF NOT EXISTS ride_requests (
    id              TEXT PRIMARY KEY,
    rider_pubkey    TEXT NOT NULL,
    rider_npub      TEXT,
    pickup_lat      REAL NOT NULL,
    pickup_lng      REAL NOT NULL,
    dest_lat        REAL,
    dest_lng        REAL,
    pickup_text     TEXT DEFAULT '',
    dest_text       TEXT DEFAULT '',
    vehicle_pref    TEXT DEFAULT '',    -- empty = any
    ride_category   TEXT DEFAULT 'city',
    estimated_km    REAL,
    fare_sats       INTEGER NOT NULL DEFAULT 0,
    status          TEXT DEFAULT 'searching',  -- searching, matched, accepted, cancelled, expired
    matched_driver  TEXT,              -- pubkey of matched driver
    drivers_notified TEXT DEFAULT '[]', -- JSON array of notified driver pubkeys
    accept_deadline INTEGER,           -- unix timestamp — 60s window
    round           INTEGER DEFAULT 1, -- which search round (expand radius each round)
    created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at      INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX IF NOT EXISTS idx_rides_status ON ride_requests(status);
CREATE INDEX IF NOT EXISTS idx_rides_rider ON ride_requests(rider_pubkey);
