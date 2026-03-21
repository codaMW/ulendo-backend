-- Migration 004: push_subscriptions
-- Web Push VAPID subscription objects keyed by npub.
-- One identity can have multiple devices.

CREATE TABLE IF NOT EXISTS push_subscriptions (
    id          TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    npub        TEXT NOT NULL REFERENCES identities(npub) ON DELETE CASCADE,
    endpoint    TEXT NOT NULL UNIQUE,   -- browser-assigned push endpoint URL
    p256dh      TEXT NOT NULL,          -- client public key (base64url)
    auth        TEXT NOT NULL,          -- auth secret (base64url)
    platform    TEXT,                   -- 'web' | 'android' | 'ios'
    user_agent  TEXT,
    created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
    last_used   INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_push_npub ON push_subscriptions(npub);