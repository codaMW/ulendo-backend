-- Call credits: Bitcoin-funded talk balance per user
CREATE TABLE IF NOT EXISTS call_credits (
    pubkey       TEXT PRIMARY KEY,
    balance_sats INTEGER NOT NULL DEFAULT 0,
    updated_at   INTEGER NOT NULL
);

-- Lightning top-up invoices
CREATE TABLE IF NOT EXISTS call_topups (
    id           TEXT PRIMARY KEY,
    pubkey       TEXT NOT NULL,
    invoice      TEXT NOT NULL,
    payment_hash TEXT NOT NULL,
    amount_sats  INTEGER NOT NULL,
    paid         INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL
);

-- PSTN call log (outbound + inbound)
CREATE TABLE IF NOT EXISTS call_pstn_log (
    id            TEXT PRIMARY KEY,
    pubkey        TEXT NOT NULL,
    direction     TEXT NOT NULL,  -- 'outbound' | 'inbound'
    phone         TEXT NOT NULL,
    duration_secs INTEGER NOT NULL DEFAULT 0,
    sats_spent    INTEGER NOT NULL DEFAULT 0,
    at_session_id TEXT,
    status        TEXT NOT NULL DEFAULT 'initiated',
    created_at    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_call_topups_pubkey  ON call_topups(pubkey);
CREATE INDEX IF NOT EXISTS idx_call_topups_hash    ON call_topups(payment_hash);
CREATE INDEX IF NOT EXISTS idx_call_pstn_log_pubkey ON call_pstn_log(pubkey);
