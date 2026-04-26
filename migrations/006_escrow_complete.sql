-- Migration 006: Add completed status for escrow auto-release
ALTER TABLE bookings ADD COLUMN completed_at INTEGER;
CREATE INDEX IF NOT EXISTS idx_bookings_completed ON bookings(status, completed_at);
