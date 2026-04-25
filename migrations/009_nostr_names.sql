CREATE TABLE IF NOT EXISTS nostr_names (
    username    TEXT PRIMARY KEY CHECK(username = lower(username) AND length(username) >= 2 AND length(username) <= 30 AND username GLOB '[a-z0-9_.-]*'),
    pubkey_hex  TEXT NOT NULL,
    relays_json TEXT NOT NULL DEFAULT '[]',
    created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at  INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_nostr_names_pubkey ON nostr_names(pubkey_hex);
