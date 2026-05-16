-- 014_direct_releases.sql
-- Records every direct release attempt so drivers can poll status.
-- Replaces the previous "fire-and-forget" pattern where releases left no audit trail.
CREATE TABLE IF NOT EXISTS direct_releases (
  id              TEXT PRIMARY KEY,
  ride_id         TEXT NOT NULL,
  payer_pubkey    TEXT NOT NULL,
  lud16           TEXT NOT NULL,
  amount_sats     INTEGER NOT NULL,
  driver_sats     INTEGER NOT NULL,
  fee_sats        INTEGER NOT NULL,
  status          TEXT NOT NULL,
  error_message   TEXT,
  blink_status    TEXT,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_direct_releases_ride
  ON direct_releases(ride_id);

CREATE INDEX IF NOT EXISTS idx_direct_releases_status
  ON direct_releases(status);
