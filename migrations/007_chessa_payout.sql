-- 007_chessa_payout.sql
-- Adds Chessa mobile money payout tracking to bookings

ALTER TABLE bookings ADD COLUMN chessa_order_id       TEXT;
ALTER TABLE bookings ADD COLUMN chessa_crypto_address TEXT;
ALTER TABLE bookings ADD COLUMN payout_choice         TEXT DEFAULT 'sats'
    CHECK(payout_choice IN ('sats', 'kwacha'));

CREATE INDEX IF NOT EXISTS idx_bookings_chessa_order
    ON bookings(chessa_order_id)
    WHERE chessa_order_id IS NOT NULL;
