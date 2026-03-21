-- Migration 001: identities
-- Nostr public keys are the primary identity — no passwords, no emails.
-- We store only the public parts; private keys never touch the server.

CREATE TABLE IF NOT EXISTS identities (
    npub        TEXT PRIMARY KEY,          -- bech32 npub1... format
    public_key  TEXT NOT NULL UNIQUE,      -- hex pubkey
    name        TEXT,
    role        TEXT NOT NULL DEFAULT 'visitor'  CHECK (role IN ('visitor', 'merchant')),
    lud16       TEXT,                      -- lightning address e.g. alice@blink.sv
    created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at  INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_identities_public_key ON identities(public_key);