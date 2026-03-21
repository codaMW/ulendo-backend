-- Migration 003: bookings
-- Each booking tracks the full escrow lifecycle.
-- payment_hash links to the Blink HODL invoice.

CREATE TABLE IF NOT EXISTS bookings (
    id                  TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    listing_id          TEXT NOT NULL REFERENCES listings(id) ON DELETE RESTRICT,
    booker_npub         TEXT NOT NULL REFERENCES identities(npub) ON DELETE RESTRICT,
    booking_type        TEXT NOT NULL DEFAULT 'listing' CHECK (booking_type IN ('listing','ride')),

    -- Escrow state machine
    -- pending → funded → held → released | disputed | refunded | cancelled
    status              TEXT NOT NULL DEFAULT 'pending'
                        CHECK (status IN ('pending','funded','held','released','disputed','refunded','cancelled')),

    amount_sats         INTEGER NOT NULL,
    fee_sats            INTEGER NOT NULL DEFAULT 0,  -- platform fee retained on release
    lud16_refund        TEXT,                        -- booker's address for refunds

    -- Blink HODL invoice
    payment_hash        TEXT UNIQUE,
    payment_request     TEXT,                        -- bolt11 lnbc... string
    invoice_expires_at  INTEGER,

    -- Ride-specific fields (null for non-ride bookings)
    ride_id             TEXT,
    pickup_text         TEXT,
    destination_text    TEXT,
    pickup_gps_lat      REAL,
    pickup_gps_lng      REAL,

    -- Timestamps for each state transition
    funded_at           INTEGER,
    held_at             INTEGER,
    released_at         INTEGER,
    disputed_at         INTEGER,
    refunded_at         INTEGER,
    cancelled_at        INTEGER,
    created_at          INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at          INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_bookings_listing   ON bookings(listing_id);
CREATE INDEX IF NOT EXISTS idx_bookings_booker    ON bookings(booker_npub);
CREATE INDEX IF NOT EXISTS idx_bookings_status    ON bookings(status);
CREATE INDEX IF NOT EXISTS idx_bookings_pay_hash  ON bookings(payment_hash);