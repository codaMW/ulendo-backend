-- Migration 005: nostr_relay_cache
-- Local index of Nostr events crawled from relays.
-- Lets us serve listing searches without hitting relays on every request.
-- kind:30402 listing events + kind:30402 driver events.

CREATE TABLE IF NOT EXISTS nostr_relay_cache (
    event_id    TEXT PRIMARY KEY,           -- Nostr event id (sha256 hex)
    kind        INTEGER NOT NULL,           -- 30402 for listings/drivers
    pubkey      TEXT NOT NULL,              -- author hex pubkey
    d_tag       TEXT,                       -- NIP-33 d tag (listing/driver id)
    t_tags      TEXT NOT NULL DEFAULT '[]', -- JSON array of #t tag values
    content     TEXT NOT NULL DEFAULT '',
    tags_json   TEXT NOT NULL DEFAULT '[]', -- full tags array as JSON
    created_at  INTEGER NOT NULL,           -- event created_at (from relay)
    indexed_at  INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_cache_kind       ON nostr_relay_cache(kind);
CREATE INDEX IF NOT EXISTS idx_cache_pubkey     ON nostr_relay_cache(pubkey);
CREATE INDEX IF NOT EXISTS idx_cache_d_tag      ON nostr_relay_cache(d_tag);
CREATE INDEX IF NOT EXISTS idx_cache_created    ON nostr_relay_cache(created_at DESC);