PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS processes (
    id TEXT PRIMARY KEY NOT NULL,
    parent_process_id TEXT NULL REFERENCES processes(id) ON DELETE RESTRICT,
    fork_at_seq INTEGER NULL,
    source_json TEXT NOT NULL,
    cwd TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    archived_at INTEGER NULL,
    title TEXT NOT NULL DEFAULT '',
    model_provider TEXT NOT NULL DEFAULT 'unknown',
    cli_version TEXT NULL,
    agent_nickname TEXT NULL,
    agent_role TEXT NULL
);

CREATE INDEX IF NOT EXISTS idx_processes_parent_process_id ON processes(parent_process_id);
CREATE INDEX IF NOT EXISTS idx_processes_updated_at ON processes(updated_at DESC);

CREATE TABLE IF NOT EXISTS process_leases (
    process_id TEXT PRIMARY KEY NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    owner_id TEXT NOT NULL,
    lease_token TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_process_leases_expires_at ON process_leases(expires_at);

CREATE TABLE IF NOT EXISTS journal_entries (
    process_id TEXT NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    recorded_at TEXT NOT NULL,
    item_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (process_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_journal_entries_process_seq ON journal_entries(process_id, seq);
