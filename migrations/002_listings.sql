-- Migration 002: listings
-- Mirrors the Nostr kind:30402 event structure but indexed for fast queries.
-- nostr_event_id links back to the canonical on-relay version.

CREATE TABLE IF NOT EXISTS listings (
    id              TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    owner_npub      TEXT NOT NULL REFERENCES identities(npub) ON DELETE CASCADE,
    nostr_event_id  TEXT UNIQUE,           -- kind:30402 event id — null if backend-only
    category        TEXT NOT NULL CHECK (category IN ('guide','transport','stay','restaurant')),
    name            TEXT NOT NULL,
    area            TEXT NOT NULL,
    description     TEXT,
    price_sats      INTEGER NOT NULL DEFAULT 0,
    price_unit      TEXT NOT NULL DEFAULT 'per day',
    lud16           TEXT,
    photos_json     TEXT NOT NULL DEFAULT '[]',  -- JSON array of https:// URLs
    phone           TEXT,
    available       INTEGER NOT NULL DEFAULT 1,  -- 0 = paused
    verified        INTEGER NOT NULL DEFAULT 0,  -- 1 = owner proved Nostr key
    created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at      INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_listings_owner   ON listings(owner_npub);
CREATE INDEX IF NOT EXISTS idx_listings_category ON listings(category);
CREATE INDEX IF NOT EXISTS idx_listings_area     ON listings(area);
CREATE INDEX IF NOT EXISTS idx_listings_available ON listings(available);