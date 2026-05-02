-- Migration 011: Verified ratings + driver stats
CREATE TABLE IF NOT EXISTS ratings (
    id              TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    ride_id         TEXT NOT NULL,
    driver_pubkey   TEXT NOT NULL,
    rider_pubkey    TEXT NOT NULL,
    score           INTEGER NOT NULL CHECK (score >= 1 AND score <= 5),
    comment         TEXT DEFAULT '',
    category        TEXT DEFAULT 'city',
    created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE(ride_id, rider_pubkey)
);
CREATE INDEX IF NOT EXISTS idx_ratings_driver ON ratings(driver_pubkey);

CREATE TABLE IF NOT EXISTS driver_stats (
    pubkey          TEXT PRIMARY KEY,
    total_rides     INTEGER DEFAULT 0,
    total_ratings   INTEGER DEFAULT 0,
    avg_rating      REAL DEFAULT 0.0,
    sum_scores      INTEGER DEFAULT 0,
    total_earned_sats INTEGER DEFAULT 0,
    total_earned_mwk  INTEGER DEFAULT 0,
    category        TEXT DEFAULT 'city',
    updated_at      INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX IF NOT EXISTS idx_stats_rating ON driver_stats(avg_rating DESC);
