CREATE TABLE IF NOT EXISTS fiat_escrow (
    id              TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    ride_id         TEXT NOT NULL,
    rider_pubkey    TEXT NOT NULL,
    driver_pubkey   TEXT NOT NULL,
    amount_mwk      INTEGER NOT NULL,
    commission_mwk  INTEGER NOT NULL,
    driver_payout   INTEGER NOT NULL,
    driver_phone    TEXT DEFAULT '',
    reference_code  TEXT NOT NULL,
    sms_raw         TEXT DEFAULT '',
    txn_ref         TEXT DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'pending',
    created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    funded_at       INTEGER,
    completed_at    INTEGER,
    released_at     INTEGER
);
CREATE INDEX IF NOT EXISTS idx_fiat_ride ON fiat_escrow(ride_id);
CREATE INDEX IF NOT EXISTS idx_fiat_status ON fiat_escrow(status);

CREATE TABLE IF NOT EXISTS used_txn_refs (
    txn_ref         TEXT PRIMARY KEY,
    escrow_id       TEXT NOT NULL,
    created_at      INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS driver_payouts (
    id              TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    driver_pubkey   TEXT NOT NULL,
    driver_phone    TEXT NOT NULL,
    amount_mwk      INTEGER NOT NULL,
    ride_id         TEXT NOT NULL,
    escrow_id       TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending',
    created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    processed_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_payout_status ON driver_payouts(status);
