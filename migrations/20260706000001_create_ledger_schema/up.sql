-- Stage 1 schema: append-only double-entry ledger foundation.
-- All ledger transactions run at SERIALIZABLE isolation; the session default
-- is forced here so any connection that forgets to set it explicitly still
-- lands in serializable.

-- Force SERIALIZABLE as the default isolation level for every session in this
-- database. We can't parameterize ALTER DATABASE, so use a DO block that
-- resolves the current database name and sets the default for all new
-- sessions. Per PostgreSQL docs, this is equivalent to issuing
-- `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE`
-- on every new connection.
DO $$
DECLARE
    db_name TEXT := current_database();
BEGIN
    EXECUTE format('ALTER DATABASE %I SET default_transaction_isolation = %L', db_name, 'serializable');
END $$;

-- Chart of accounts: versioned catalog of account types, their normal
-- balance side, and the directions allowed on entries against them.
CREATE TABLE chart_of_accounts (
    account_type      TEXT PRIMARY KEY,
    version            INTEGER NOT NULL DEFAULT 1,
    normal_balance    TEXT NOT NULL CHECK (normal_balance IN ('debit', 'credit', 'either')),
    allowed_directions TEXT[] NOT NULL,
    description       TEXT NOT NULL DEFAULT '',
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Accounts: concrete account instances. account_id is caller-supplied but
-- must be unique; the type must exist in chart_of_accounts.
CREATE TABLE accounts (
    account_id    TEXT PRIMARY KEY,
    type          TEXT NOT NULL REFERENCES chart_of_accounts (account_type),
    asset_class   TEXT NOT NULL CHECK (asset_class IN ('fiat', 'crypto')),
    label         TEXT NOT NULL DEFAULT '',
    parent_id     TEXT REFERENCES accounts (account_id),
    status        TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'frozen', 'closed')),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX accounts_type_idx ON accounts (type);
CREATE INDEX accounts_parent_idx ON accounts (parent_id);

-- Postings: a logical double-entry posting. posting_id is caller-supplied and
-- unique, enabling exactly-once retries from the orchestrators.
CREATE TABLE postings (
    posting_id        TEXT PRIMARY KEY,
    ref_tx_id         TEXT,
    memo              TEXT NOT NULL DEFAULT '',
    status            TEXT NOT NULL DEFAULT 'committed' CHECK (status IN ('committed', 'rejected')),
    hash_chain_head   BYTEA,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Entries: the individual debit/credit lines. Append-only: there is no UPDATE
-- or DELETE path; a trigger below rejects any such attempt.
CREATE TABLE entries (
    entry_id      BIGSERIAL PRIMARY KEY,
    posting_id    TEXT NOT NULL REFERENCES postings (posting_id),
    account_id    TEXT NOT NULL REFERENCES accounts (account_id),
    direction     TEXT NOT NULL CHECK (direction IN ('debit', 'credit')),
    amount        NUMERIC(39, 0) NOT NULL CHECK (amount > 0),
    asset         TEXT NOT NULL,
    prev_hash     BYTEA,
    this_hash     BYTEA NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT entries_account_active
        CHECK (true) -- placeholder; the active-account rule is enforced via a trigger
);

CREATE INDEX entries_posting_idx ON entries (posting_id);
CREATE INDEX entries_account_idx ON entries (account_id, asset, created_at);

-- Append-only enforcement for entries: reject any UPDATE or DELETE.
CREATE OR REPLACE FUNCTION reject_entries_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'entries is append-only: UPDATE and DELETE are not permitted (use a reversing posting)'
        USING ERRCODE = 'check_violation';
END;
$$;

DROP TRIGGER IF EXISTS entries_no_update ON entries;
CREATE TRIGGER entries_no_update
    BEFORE UPDATE ON entries
    FOR EACH ROW
    EXECUTE FUNCTION reject_entries_mutation();

DROP TRIGGER IF EXISTS entries_no_delete ON entries;
CREATE TRIGGER entries_no_delete
    BEFORE DELETE ON entries
    FOR EACH ROW
    EXECUTE FUNCTION reject_entries_mutation();

-- Enforce that entries reference an active account. A plain CHECK cannot look
-- at another row, so use a trigger.
CREATE OR REPLACE FUNCTION enforce_entries_account_active()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    acct_status TEXT;
BEGIN
    SELECT status INTO acct_status FROM accounts WHERE account_id = NEW.account_id;
    IF acct_status IS NULL THEN
        RAISE EXCEPTION 'account_id % does not exist', NEW.account_id
            USING ERRCODE = 'foreign_key_violation';
    END IF;
    IF acct_status <> 'active' THEN
        RAISE EXCEPTION 'account_id % is not active (status=%)', NEW.account_id, acct_status
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS entries_account_active_trigger ON entries;
CREATE TRIGGER entries_account_active_trigger
    BEFORE INSERT ON entries
    FOR EACH ROW
    EXECUTE FUNCTION enforce_entries_account_active();

-- Balance snapshots: materialized balances per account/asset, rebuilt on a
-- fixed cadence to keep historical statement queries bounded.
CREATE TABLE balance_snapshots (
    account_id      TEXT NOT NULL REFERENCES accounts (account_id),
    asset           TEXT NOT NULL,
    balance         NUMERIC(39, 0) NOT NULL,
    as_of_ts        TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_entry_id   BIGINT NOT NULL REFERENCES entries (entry_id),
    PRIMARY KEY (account_id, asset, as_of_ts)
);

-- Hash chain: anchor/head rows for the tamper-evident chain per posting and
-- the global sequence.
CREATE TABLE hash_chain (
    head_id         BIGSERIAL PRIMARY KEY,
    scope           TEXT NOT NULL CHECK (scope IN ('global', 'posting')),
    posting_id      TEXT REFERENCES postings (posting_id),
    head_hash       BYTEA NOT NULL,
    last_entry_id   BIGINT REFERENCES entries (entry_id),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((scope = 'global' AND posting_id IS NULL) OR (scope = 'posting' AND posting_id IS NOT NULL))
);

CREATE INDEX hash_chain_posting_idx ON hash_chain (posting_id);