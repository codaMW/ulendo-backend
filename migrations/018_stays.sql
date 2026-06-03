-- ─── ULENDO STAYS: ACCOMMODATION BOOKING ──────────────────────────────────────
-- Purpose-built for ABC26 (December 2026) and post-conference scale to Africa.
-- Source-of-truth: backend DB. Listing metadata also mirrored to Nostr (kind 30382)
-- for portability, but availability + booking state lives only in this DB.
--
-- Money model: two-stage Lightning payments
--   1. Deposit (20% of total) at booking time
--   2. Balance (80% of total) at check-in day
-- Ulendo platform fee: 10% of total, deducted at release to host after checkout.
--
-- Cancellation policy (system default):
--   - Guest cancels >7 days before checkin: full deposit refund
--   - Guest cancels <=7 days before checkin: 50% deposit refund, 50% to host
--   - Host cancels at any time: full refund to guest + reputation penalty
--
-- Dispute model: Ulendo arbitration (Tier 1) for launch. Guest opens within
-- 48h of checkin. 7-day resolution SLA.
-- ──────────────────────────────────────────────────────────────────────────────

-- ─── LISTINGS ─────────────────────────────────────────────────────────────────
-- One row per bookable property. Hosted manually for ABC26 (operator-vetted).
CREATE TABLE IF NOT EXISTS stays_listings (
    id                      TEXT PRIMARY KEY,
    host_pubkey             TEXT NOT NULL,                          -- Nostr pubkey of host
    host_lud16              TEXT NOT NULL,                          -- Lightning address for payouts
    -- Property type (controls UX and pricing patterns)
    listing_type            TEXT NOT NULL CHECK (listing_type IN (
                                'entire_place', 'private_room', 'shared_room',
                                'hotel_room', 'resort_unit'
                            )),
    property_class          TEXT NOT NULL CHECK (property_class IN (
                                'apartment', 'house', 'hotel', 'resort',
                                'guesthouse', 'lodge', 'villa', 'cottage'
                            )),
    -- Descriptive content (also published to Nostr 30382)
    title                   TEXT NOT NULL,
    description             TEXT NOT NULL,
    house_rules             TEXT,                                   -- free-form, optional
    -- Location: city + neighborhood for search, lat/lng for map
    country                 TEXT NOT NULL DEFAULT 'MW',
    city                    TEXT NOT NULL,                          -- 'Blantyre', 'Lilongwe', etc.
    neighborhood            TEXT,                                   -- 'Sunnyside', optional
    lat                     REAL NOT NULL,                          -- exact location for map
    lng                     REAL NOT NULL,
    fuzzy_lat               REAL,                                   -- ~500m radius randomized for pre-booking display
    fuzzy_lng               REAL,                                   -- (privacy: exact only after booking confirmed)
    -- Capacity
    max_guests              INTEGER NOT NULL CHECK (max_guests > 0),
    bedrooms                INTEGER NOT NULL CHECK (bedrooms >= 0),
    beds                    INTEGER NOT NULL CHECK (beds > 0),
    bathrooms               REAL NOT NULL CHECK (bathrooms > 0),    -- 1.5 = one full + one half bath
    -- Pricing (all in sats)
    price_per_night_sats    INTEGER NOT NULL CHECK (price_per_night_sats >= 1000),  -- min 1000 sats/night (~$0.50)
    cleaning_fee_sats       INTEGER NOT NULL DEFAULT 0 CHECK (cleaning_fee_sats >= 0),
    -- Stay rules
    min_nights              INTEGER NOT NULL DEFAULT 1 CHECK (min_nights >= 1),
    max_nights              INTEGER NOT NULL DEFAULT 30 CHECK (max_nights >= min_nights),
    checkin_time            TEXT NOT NULL DEFAULT '15:00',          -- 24h format HH:MM
    checkout_time           TEXT NOT NULL DEFAULT '11:00',
    -- Amenities + photos as JSON arrays (SQLite stores as TEXT)
    amenities               TEXT NOT NULL DEFAULT '[]',             -- ["wifi","ac","pool","kitchen","parking"]
    photo_urls              TEXT NOT NULL DEFAULT '[]',             -- Cloudinary URLs
    -- Operator state
    cancellation_policy     TEXT NOT NULL DEFAULT 'strict_7day_50' -- only one policy for launch
                                CHECK (cancellation_policy IN ('strict_7day_50')),
    verified                INTEGER NOT NULL DEFAULT 0,             -- 0=pending, 1=verified by Ulendo
    active                  INTEGER NOT NULL DEFAULT 1,             -- 0=hidden from search
    -- Audit
    created_at              INTEGER NOT NULL,                       -- unix timestamp
    updated_at              INTEGER NOT NULL,
    deleted_at              INTEGER                                 -- soft delete; NULL = active
);

CREATE INDEX IF NOT EXISTS idx_stays_listings_city            ON stays_listings(city, country) WHERE deleted_at IS NULL AND active = 1;
CREATE INDEX IF NOT EXISTS idx_stays_listings_host            ON stays_listings(host_pubkey) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_stays_listings_verified        ON stays_listings(verified, active) WHERE deleted_at IS NULL;
-- Geo index for map searches; bounding-box queries will use lat/lng directly
CREATE INDEX IF NOT EXISTS idx_stays_listings_geo             ON stays_listings(lat, lng) WHERE deleted_at IS NULL AND active = 1;


-- ─── AVAILABILITY CALENDAR ────────────────────────────────────────────────────
-- One row per (listing, date) pair. Simple model — 500 listings × 365 days ≈ 180k rows max.
-- Dates without rows are treated as 'available' by default (host's calendar is opt-out blocking).
-- A row exists when status changes from default — either blocked by host or held by booking.
CREATE TABLE IF NOT EXISTS stays_availability (
    listing_id              TEXT NOT NULL,
    date                    TEXT NOT NULL,                          -- 'YYYY-MM-DD' format
    status                  TEXT NOT NULL CHECK (status IN (
                                'available',   -- explicitly marked available (overrides default if needed)
                                'blocked',     -- host blocked (not for booking)
                                'pending',     -- booking requested, awaiting deposit
                                'booked'       -- deposit paid, dates locked
                            )),
    booking_id              TEXT,                                   -- references stays_bookings(id) when status IN (pending, booked)
    -- Per-night price override (NULL = use listing's default)
    price_override_sats     INTEGER,
    -- Audit
    updated_at              INTEGER NOT NULL,
    PRIMARY KEY (listing_id, date),
    FOREIGN KEY (listing_id) REFERENCES stays_listings(id)
);

CREATE INDEX IF NOT EXISTS idx_stays_avail_listing_status ON stays_availability(listing_id, status);
CREATE INDEX IF NOT EXISTS idx_stays_avail_booking       ON stays_availability(booking_id) WHERE booking_id IS NOT NULL;


-- ─── BOOKINGS ─────────────────────────────────────────────────────────────────
-- A reservation request. Auto-accepted on creation (operator-vetted listings).
-- State machine (see STATUS field below):
--   awaiting_deposit → deposit_paid → active → completed
--                                              ↘ disputed
--                                  ↘ cancelled_*
--                  ↘ expired (deposit not paid within 1h)
CREATE TABLE IF NOT EXISTS stays_bookings (
    id                          TEXT PRIMARY KEY,
    listing_id                  TEXT NOT NULL,
    guest_pubkey                TEXT NOT NULL,
    guest_npub                  TEXT NOT NULL DEFAULT '',
    -- Stay dates (checkout exclusive: checkin=2026-12-01, checkout=2026-12-04 = 3 nights)
    checkin_date                TEXT NOT NULL,                      -- 'YYYY-MM-DD'
    checkout_date               TEXT NOT NULL,                      -- 'YYYY-MM-DD'
    nights                      INTEGER NOT NULL CHECK (nights > 0),
    guest_count                 INTEGER NOT NULL CHECK (guest_count > 0),
    -- State machine
    status                      TEXT NOT NULL CHECK (status IN (
                                    'awaiting_deposit',
                                    'deposit_paid',
                                    'active',
                                    'completed',
                                    'disputed',
                                    'cancelled_guest_pre_7d',
                                    'cancelled_guest_under_7d',
                                    'cancelled_host',
                                    'no_show',
                                    'expired'
                                )),
    -- Money breakdown (sats)
    total_sats                  INTEGER NOT NULL CHECK (total_sats > 0),
    deposit_sats                INTEGER NOT NULL CHECK (deposit_sats > 0),
    balance_sats                INTEGER NOT NULL CHECK (balance_sats >= 0),
    platform_fee_sats           INTEGER NOT NULL CHECK (platform_fee_sats >= 0),   -- 10% of total
    host_payout_sats            INTEGER NOT NULL CHECK (host_payout_sats >= 0),    -- total - platform_fee
    -- Lightning invoice tracking
    deposit_invoice_id          TEXT,                               -- Blink invoice ID for deposit
    deposit_invoice_bolt11      TEXT,                               -- BOLT11 string
    deposit_invoice_expires_at  INTEGER,                            -- unix timestamp (booking expires 1h after creation if unpaid)
    deposit_paid_at             INTEGER,                            -- unix timestamp when deposit confirmed
    balance_invoice_id          TEXT,                               -- Blink invoice ID for balance (generated check-in day)
    balance_invoice_bolt11      TEXT,
    balance_invoice_expires_at  INTEGER,                            -- noon on checkin_date
    balance_paid_at             INTEGER,
    -- Release tracking (after checkout + 24h auto-release window)
    release_eligible_at         INTEGER,                            -- unix timestamp: checkout_date + 24h
    released_at                 INTEGER,                            -- unix timestamp when host actually got paid
    host_release_invoice_id     TEXT,                               -- Blink invoice from host's lud16 for the payout
    -- Cancellation tracking
    cancelled_at                INTEGER,
    cancellation_reason         TEXT,                               -- free-form, who/why
    refund_amount_sats          INTEGER,                            -- what we paid back to guest
    refund_invoice_id           TEXT,                               -- Blink invoice for guest refund (if applicable)
    -- Dispute tracking (Tier 2 arbitration: Ulendo decides)
    dispute_opened_at           INTEGER,
    dispute_opened_by           TEXT,                               -- 'guest' or 'host'
    dispute_evidence_guest      TEXT,                               -- free-form
    dispute_evidence_host       TEXT,
    dispute_resolved_at         INTEGER,
    dispute_resolution          TEXT,                               -- 'refund_full'|'refund_partial'|'release_full' + admin notes
    dispute_admin_pubkey        TEXT,                               -- which admin resolved
    -- Guest's optional note to host at booking time
    guest_note                  TEXT,
    -- Audit
    created_at                  INTEGER NOT NULL,
    updated_at                  INTEGER NOT NULL,
    FOREIGN KEY (listing_id) REFERENCES stays_listings(id),
    -- Logical constraint: checkout > checkin
    CHECK (checkout_date > checkin_date)
);

CREATE INDEX IF NOT EXISTS idx_stays_bookings_listing      ON stays_bookings(listing_id, status);
CREATE INDEX IF NOT EXISTS idx_stays_bookings_guest        ON stays_bookings(guest_pubkey, status);
CREATE INDEX IF NOT EXISTS idx_stays_bookings_status       ON stays_bookings(status);
CREATE INDEX IF NOT EXISTS idx_stays_bookings_deposit_exp  ON stays_bookings(deposit_invoice_expires_at) WHERE status = 'awaiting_deposit';
CREATE INDEX IF NOT EXISTS idx_stays_bookings_balance_exp  ON stays_bookings(balance_invoice_expires_at) WHERE status = 'deposit_paid';
CREATE INDEX IF NOT EXISTS idx_stays_bookings_release      ON stays_bookings(release_eligible_at) WHERE status = 'completed' AND released_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_stays_bookings_disputes     ON stays_bookings(dispute_opened_at) WHERE status = 'disputed' AND dispute_resolved_at IS NULL;


-- ─── REVIEWS CACHE ────────────────────────────────────────────────────────────
-- Backend cache of Nostr-published reviews (kind 1 with #stays-review tag).
-- Nostr is the source of truth; this table is for aggregation and fast queries.
-- Backend enforces 1 review per booking via UNIQUE constraint.
CREATE TABLE IF NOT EXISTS stays_reviews_cache (
    booking_id              TEXT PRIMARY KEY,
    listing_id              TEXT NOT NULL,
    host_pubkey             TEXT NOT NULL,                          -- subject of review
    guest_pubkey            TEXT NOT NULL,                          -- author
    rating                  INTEGER NOT NULL CHECK (rating BETWEEN 1 AND 5),
    comment                 TEXT NOT NULL DEFAULT '',
    nostr_event_id          TEXT NOT NULL,                          -- Nostr event id for source-of-truth lookup
    created_at              INTEGER NOT NULL,                       -- unix timestamp
    FOREIGN KEY (listing_id)  REFERENCES stays_listings(id),
    FOREIGN KEY (booking_id)  REFERENCES stays_bookings(id)
);

CREATE INDEX IF NOT EXISTS idx_stays_reviews_listing  ON stays_reviews_cache(listing_id);
CREATE INDEX IF NOT EXISTS idx_stays_reviews_host     ON stays_reviews_cache(host_pubkey);
