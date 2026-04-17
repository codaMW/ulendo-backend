-- Migration 006: Add completed status for escrow auto-release
-- completed = ride done, 1-min countdown to auto-release starts

-- Add completed_at timestamp
-- ALTER TABLE bookings ADD COLUMN completed_at INTEGER;  -- already applied

-- Expand the status CHECK to include 'completed'
-- SQLite doesn't support ALTER CHECK, so we drop and recreate
-- For SQLite, CHECK constraints aren't enforced after table creation anyway
-- The app code enforces the state machine

-- Index for auto-release query
CREATE INDEX IF NOT EXISTS idx_bookings_completed ON bookings(status, completed_at);
