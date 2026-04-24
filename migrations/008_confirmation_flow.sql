-- Migration 008: Dual confirmation flow + timeout auto-release
-- rider_confirmed_at: rider taps "Confirm arrival"
-- driver_confirmed_at: driver taps "Mark complete"
-- pickup_confirmed_at: driver confirms pickup happened (starts no-show timer)

ALTER TABLE bookings ADD COLUMN rider_confirmed_at INTEGER;
ALTER TABLE bookings ADD COLUMN driver_confirmed_at INTEGER;
ALTER TABLE bookings ADD COLUMN pickup_confirmed_at INTEGER;

-- Index for timeout queries
CREATE INDEX IF NOT EXISTS idx_bookings_confirmations
  ON bookings(status, rider_confirmed_at, driver_confirmed_at, pickup_confirmed_at);
