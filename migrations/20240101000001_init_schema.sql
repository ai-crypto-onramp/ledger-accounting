-- Chart of accounts: versioned catalog of account types, normal balances,
-- and allowed directions.
CREATE TABLE IF NOT EXISTS chart_of_accounts (
    version          TEXT        NOT NULL,
    type_name        TEXT        NOT NULL,
    normal_balance   TEXT        NOT NULL CHECK (normal_balance IN ('debit', 'credit', 'either')),
    allowed_directions TEXT[]    NOT NULL,
    asset_class      TEXT        NOT NULL CHECK (asset_class IN ('fiat', 'crypto', 'both')),
    PRIMARY KEY (version, type_name)
);

-- Accounts: account definitions (per-user, per-asset, per-venue, treasury).
CREATE TABLE IF NOT EXISTS accounts (
    account_id       TEXT        PRIMARY KEY,
    type_name        TEXT        NOT NULL,
    asset_class      TEXT        NOT NULL,
    label            TEXT        NOT NULL,
    parent_id        TEXT        REFERENCES accounts(account_id),
    status           TEXT        NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'inactive')),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Postings: a logical double-entry posting (caller-supplied unique posting_id).
CREATE TABLE IF NOT EXISTS postings (
    posting_id       TEXT        PRIMARY KEY,
    ref_tx_id        TEXT,
    memo             TEXT,
    status           TEXT        NOT NULL DEFAULT 'posted',
    hash_chain_head  TEXT        NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Entries: the individual debit/credit lines (append-only).
CREATE TABLE IF NOT EXISTS entries (
    entry_id         TEXT        PRIMARY KEY,
    posting_id       TEXT        NOT NULL REFERENCES postings(posting_id),
    account_id       TEXT        NOT NULL,
    direction        TEXT        NOT NULL CHECK (direction IN ('debit', 'credit')),
    amount           NUMERIC(38,0) NOT NULL CHECK (amount > 0),
    asset            TEXT        NOT NULL,
    sequence_number  BIGINT       NOT NULL,
    prev_hash        TEXT        NOT NULL,
    this_hash        TEXT        NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (entry_id)
);

-- FK from entries.account_id to accounts.account_id: only active accounts.
ALTER TABLE entries
    ADD CONSTRAINT entries_account_fk
    FOREIGN KEY (account_id) REFERENCES accounts(account_id)
    DEFERRABLE INITIALLY IMMEDIATE;

-- Append-only enforcement for entries: reject UPDATE and DELETE.
CREATE OR REPLACE FUNCTION reject_entry_mutation() RETURNS TRIGGER
AS $$
BEGIN
    RAISE EXCEPTION 'entries is append-only: UPDATE and DELETE are not permitted';
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS entries_no_update ON entries;
CREATE TRIGGER entries_no_update
    BEFORE UPDATE ON entries
    FOR EACH ROW
    EXECUTE FUNCTION reject_entry_mutation();

DROP TRIGGER IF EXISTS entries_no_delete ON entries;
CREATE TRIGGER entries_no_delete
    BEFORE DELETE ON entries
    FOR EACH ROW
    EXECUTE FUNCTION reject_entry_mutation();

-- Balance snapshots: materialized balances per account/asset.
CREATE TABLE IF NOT EXISTS balance_snapshots (
    account_id       TEXT        NOT NULL,
    asset            TEXT        NOT NULL,
    balance          NUMERIC(38,0) NOT NULL,
    as_of_ts         TIMESTAMPTZ NOT NULL,
    last_entry_id    TEXT        NOT NULL,
    PRIMARY KEY (account_id, asset, as_of_ts)
);

-- Hash chain: anchor/head rows for the tamper-evident chain.
CREATE TABLE IF NOT EXISTS hash_chain (
    posting_id       TEXT        PRIMARY KEY REFERENCES postings(posting_id),
    head_hash        TEXT        NOT NULL,
    global_sequence_head TEXT     NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_entries_account_asset
    ON entries (account_id, asset, created_at);
CREATE INDEX IF NOT EXISTS idx_entries_sequence
    ON entries (sequence_number);
CREATE INDEX IF NOT EXISTS idx_balance_snapshots
    ON balance_snapshots (account_id, asset, as_of_ts);