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
    agent_role TEXT NULL,
    CHECK (parent_process_id IS NULL OR parent_process_id <> id),
    CHECK (
        (parent_process_id IS NULL AND fork_at_seq IS NULL)
        OR (parent_process_id IS NOT NULL AND fork_at_seq IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS idx_processes_parent_process_id ON processes(parent_process_id);
CREATE INDEX IF NOT EXISTS idx_processes_updated_at ON processes(updated_at DESC);

CREATE TABLE IF NOT EXISTS process_closure (
    ancestor_process_id TEXT NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    descendant_process_id TEXT NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    depth INTEGER NOT NULL,
    PRIMARY KEY (ancestor_process_id, descendant_process_id),
    CHECK (depth >= 0),
    CHECK (
        (ancestor_process_id = descendant_process_id AND depth = 0)
        OR (ancestor_process_id <> descendant_process_id AND depth > 0)
    )
);

CREATE INDEX IF NOT EXISTS idx_process_closure_ancestor_depth
    ON process_closure(ancestor_process_id, depth, descendant_process_id);
CREATE INDEX IF NOT EXISTS idx_process_closure_descendant_depth
    ON process_closure(descendant_process_id, depth, ancestor_process_id);

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

CREATE TRIGGER IF NOT EXISTS processes_parent_process_id_immutable
BEFORE UPDATE ON processes
FOR EACH ROW
WHEN NEW.parent_process_id IS NOT OLD.parent_process_id
BEGIN
    SELECT RAISE(FAIL, 'processes.parent_process_id is immutable');
END;

CREATE TRIGGER IF NOT EXISTS processes_fork_at_seq_immutable
BEFORE UPDATE ON processes
FOR EACH ROW
WHEN NEW.fork_at_seq IS NOT OLD.fork_at_seq
BEGIN
    SELECT RAISE(FAIL, 'processes.fork_at_seq is immutable');
END;

CREATE TRIGGER IF NOT EXISTS processes_insert_closure
AFTER INSERT ON processes
FOR EACH ROW
BEGIN
    INSERT INTO process_closure (
        ancestor_process_id,
        descendant_process_id,
        depth
    ) VALUES (
        NEW.id,
        NEW.id,
        0
    );

    INSERT INTO process_closure (
        ancestor_process_id,
        descendant_process_id,
        depth
    )
    SELECT
        ancestor_process_id,
        NEW.id,
        depth + 1
    FROM process_closure
    WHERE descendant_process_id = NEW.parent_process_id;
END;
