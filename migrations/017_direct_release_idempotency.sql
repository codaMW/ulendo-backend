-- 017_direct_release_idempotency.sql
-- SECURITY (CRITICAL): Prevent race-condition double-release.
-- Without this, two concurrent /escrow/release-direct calls for the same ride could
-- both insert pending rows and trigger two Blink payments.
-- A partial unique index makes this atomic: only one pending OR released row per ride.
-- (Failed rows are allowed any time — those are records of attempts.)
CREATE UNIQUE INDEX IF NOT EXISTS uniq_direct_release_active_per_ride
  ON direct_releases(ride_id)
  WHERE status IN ('pending', 'released');
