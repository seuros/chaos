-- Lease and journal timestamps are epoch seconds, like every other timestamp
-- column in this schema. Databases created before that alignment declared these
-- two as TEXT, which gives them text affinity and stores integers as strings, so
-- the tables are rebuilt rather than altered. Rows written either way carry over.

DROP VIEW IF EXISTS active_process_leases;

CREATE TABLE process_leases_epoch (
    process_id TEXT PRIMARY KEY NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    owner_id TEXT NOT NULL,
    lease_token TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

INSERT INTO process_leases_epoch (process_id, owner_id, lease_token, expires_at, updated_at)
    SELECT
        process_id,
        owner_id,
        lease_token,
        CAST(COALESCE(strftime('%s', expires_at), expires_at) AS INTEGER),
        updated_at
    FROM process_leases;

DROP TABLE process_leases;
ALTER TABLE process_leases_epoch RENAME TO process_leases;

CREATE INDEX idx_process_leases_expires_at ON process_leases(expires_at);

CREATE TRIGGER process_leases_touch
AFTER UPDATE ON process_leases
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE process_leases SET updated_at = UNIXEPOCH() WHERE process_id = NEW.process_id;
END;

CREATE TABLE journal_entries_epoch (
    process_id TEXT NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    recorded_at INTEGER NOT NULL,
    item_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (process_id, seq)
);

INSERT INTO journal_entries_epoch (process_id, seq, recorded_at, item_type, payload_json)
    SELECT
        process_id,
        seq,
        CAST(COALESCE(strftime('%s', recorded_at), recorded_at) AS INTEGER),
        item_type,
        payload_json
    FROM journal_entries;

DROP TABLE journal_entries;
ALTER TABLE journal_entries_epoch RENAME TO journal_entries;

CREATE INDEX idx_journal_entries_process_seq ON journal_entries(process_id, seq);

CREATE TRIGGER journal_entries_no_update
BEFORE UPDATE ON journal_entries
FOR EACH ROW
BEGIN
    SELECT RAISE(FAIL, 'journal_entries is append-only');
END;

CREATE TRIGGER journal_entries_no_delete
BEFORE DELETE ON journal_entries
FOR EACH ROW
BEGIN
    SELECT RAISE(FAIL, 'journal_entries is append-only');
END;

CREATE VIEW active_process_leases AS
    SELECT
        pl.process_id,
        pl.owner_id,
        pl.lease_token,
        pl.expires_at,
        pl.updated_at,
        p.source,
        p.title,
        p.cwd,
        p.model_provider
    FROM process_leases pl
    JOIN processes p ON p.id = pl.process_id
    WHERE p.archived_at IS NULL
      AND pl.expires_at > CAST(strftime('%s', 'now') AS INTEGER)
    ORDER BY pl.expires_at ASC, pl.process_id ASC;
