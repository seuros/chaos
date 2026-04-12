-- Let there be a runtime database.
-- And it was one.
-- And it was not acting like it was built by someone who watched Fireship in 100 seconds without subscribing.

PRAGMA foreign_keys = ON;

CREATE TABLE processes (
    id TEXT PRIMARY KEY NOT NULL,
    parent_process_id TEXT REFERENCES processes(id) ON DELETE RESTRICT,
    fork_at_seq INTEGER,
    source TEXT NOT NULL,
    source_json TEXT NOT NULL,
    model_provider TEXT NOT NULL DEFAULT 'unknown',
    cwd TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    archived_at INTEGER,
    title TEXT NOT NULL DEFAULT '',
    sandbox_policy TEXT NOT NULL DEFAULT '',
    approval_mode TEXT NOT NULL DEFAULT '',
    tokens_used INTEGER NOT NULL DEFAULT 0,
    first_user_message TEXT NOT NULL DEFAULT '',
    cli_version TEXT NOT NULL DEFAULT '',
    agent_nickname TEXT,
    agent_role TEXT,
    git_sha TEXT,
    git_branch TEXT,
    git_origin_url TEXT,
    memory_mode TEXT NOT NULL DEFAULT 'enabled',
    model TEXT,
    reasoning_effort TEXT,
    agent_path TEXT,
    process_name TEXT
);

CREATE INDEX idx_processes_created_at ON processes(created_at DESC, id DESC);
CREATE INDEX idx_processes_updated_at ON processes(updated_at DESC, id DESC);
CREATE INDEX idx_processes_archived_at ON processes(archived_at);
CREATE INDEX idx_processes_source ON processes(source);
CREATE INDEX idx_processes_provider ON processes(model_provider);
CREATE INDEX idx_processes_parent_process_id ON processes(parent_process_id);
CREATE INDEX idx_processes_git_branch ON processes(git_branch);
CREATE INDEX idx_processes_process_name ON processes(process_name);

CREATE TABLE process_leases (
    process_id TEXT PRIMARY KEY NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    owner_id TEXT NOT NULL,
    lease_token TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX idx_process_leases_expires_at ON process_leases(expires_at);

CREATE TABLE journal_entries (
    process_id TEXT NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    recorded_at TEXT NOT NULL,
    item_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (process_id, seq)
);

CREATE INDEX idx_journal_entries_process_seq ON journal_entries(process_id, seq);

CREATE TABLE logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    ts_nanos INTEGER NOT NULL,
    level TEXT NOT NULL,
    target TEXT NOT NULL,
    message TEXT,
    process_id TEXT,
    process_uuid TEXT,
    module_path TEXT,
    file TEXT,
    line INTEGER,
    estimated_bytes INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_logs_ts ON logs(ts DESC, ts_nanos DESC, id DESC);
CREATE INDEX idx_logs_process_id ON logs(process_id);
CREATE INDEX idx_logs_process_uuid ON logs(process_uuid);
CREATE INDEX idx_logs_process_id_ts ON logs(process_id, ts DESC, ts_nanos DESC, id DESC);
CREATE INDEX idx_logs_process_uuid_processless_ts
    ON logs(process_uuid, ts DESC, ts_nanos DESC, id DESC)
    WHERE process_id IS NULL;

CREATE TABLE process_dynamic_tools (
    process_id TEXT NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    input_schema TEXT NOT NULL,
    defer_loading INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (process_id, position)
);

CREATE INDEX idx_process_dynamic_tools_thread ON process_dynamic_tools(process_id);

CREATE TABLE stage1_outputs (
    process_id TEXT PRIMARY KEY REFERENCES processes(id) ON DELETE CASCADE,
    source_updated_at INTEGER NOT NULL,
    raw_memory TEXT NOT NULL,
    rollout_summary TEXT NOT NULL,
    generated_at INTEGER NOT NULL,
    rollout_slug TEXT,
    usage_count INTEGER,
    last_usage INTEGER,
    selected_for_phase2 INTEGER NOT NULL DEFAULT 0,
    selected_for_phase2_source_updated_at INTEGER
);

CREATE INDEX idx_stage1_outputs_source_updated_at
    ON stage1_outputs(source_updated_at DESC, process_id DESC);

CREATE TABLE jobs (
    kind TEXT NOT NULL,
    job_key TEXT NOT NULL,
    status TEXT NOT NULL,
    worker_id TEXT,
    ownership_token TEXT,
    started_at INTEGER,
    finished_at INTEGER,
    lease_until INTEGER,
    retry_at INTEGER,
    retry_remaining INTEGER NOT NULL,
    last_error TEXT,
    input_watermark INTEGER,
    last_success_watermark INTEGER,
    PRIMARY KEY (kind, job_key)
);

CREATE INDEX idx_jobs_kind_status_retry_lease
    ON jobs(kind, status, retry_at, lease_until);

CREATE TABLE backfill_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    status TEXT NOT NULL,
    last_watermark TEXT,
    last_success_at INTEGER,
    updated_at INTEGER NOT NULL
);

INSERT INTO backfill_state (id, status, last_watermark, last_success_at, updated_at)
VALUES (1, 'pending', NULL, NULL, CAST(strftime('%s', 'now') AS INTEGER));

CREATE TABLE agent_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    status TEXT NOT NULL,
    instruction TEXT NOT NULL,
    output_schema_json TEXT,
    input_headers_json TEXT NOT NULL,
    input_csv_path TEXT NOT NULL,
    output_csv_path TEXT NOT NULL,
    auto_export INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    started_at INTEGER,
    completed_at INTEGER,
    last_error TEXT,
    max_runtime_seconds INTEGER
);

CREATE TABLE agent_job_items (
    job_id TEXT NOT NULL REFERENCES agent_jobs(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    row_index INTEGER NOT NULL,
    source_id TEXT,
    row_json TEXT NOT NULL,
    status TEXT NOT NULL,
    assigned_process_id TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    result_json TEXT,
    last_error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER,
    reported_at INTEGER,
    PRIMARY KEY (job_id, item_id)
);

CREATE INDEX idx_agent_jobs_status ON agent_jobs(status, updated_at DESC);
CREATE INDEX idx_agent_job_items_status ON agent_job_items(job_id, status, row_index ASC);

CREATE TABLE process_spawn_edges (
    parent_process_id TEXT NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    child_process_id TEXT NOT NULL PRIMARY KEY REFERENCES processes(id) ON DELETE CASCADE,
    status TEXT NOT NULL
);

CREATE INDEX idx_process_spawn_edges_parent_status
    ON process_spawn_edges(parent_process_id, status);

CREATE TABLE message_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    ts INTEGER NOT NULL,
    text TEXT NOT NULL,
    estimated_bytes INTEGER NOT NULL CHECK (estimated_bytes >= 0)
);

CREATE TABLE cron_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    schedule TEXT NOT NULL,
    command TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT 'project',
    project_path TEXT,
    session_id TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_run_at INTEGER,
    next_run_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (enabled IN (0, 1))
);

CREATE INDEX idx_cron_jobs_scope ON cron_jobs(scope);
CREATE INDEX idx_cron_jobs_next_run ON cron_jobs(next_run_at) WHERE enabled = 1;
CREATE INDEX idx_cron_jobs_project ON cron_jobs(project_path) WHERE project_path IS NOT NULL;
CREATE INDEX idx_cron_jobs_session ON cron_jobs(session_id) WHERE session_id IS NOT NULL;

CREATE TRIGGER cron_jobs_touch
AFTER UPDATE ON cron_jobs
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE cron_jobs SET updated_at = UNIXEPOCH() WHERE id = NEW.id;
END;

CREATE VIEW due_cron_jobs AS
    SELECT *
    FROM cron_jobs
    WHERE enabled = 1
      AND next_run_at IS NOT NULL
      AND next_run_at <= UNIXEPOCH()
    ORDER BY next_run_at ASC;

CREATE TABLE model_catalog_cache (
    provider_name TEXT NOT NULL,
    wire_api TEXT NOT NULL,
    base_url TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    etag TEXT,
    client_version TEXT,
    models_json TEXT NOT NULL,
    PRIMARY KEY (provider_name, wire_api, base_url)
);

CREATE INDEX idx_model_catalog_cache_fetched_at
    ON model_catalog_cache(fetched_at);

CREATE VIEW valid_model_cache AS
    SELECT *
    FROM model_catalog_cache
    WHERE fetched_at + 86400 >= UNIXEPOCH();

CREATE TRIGGER processes_touch
AFTER UPDATE ON processes
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE processes SET updated_at = UNIXEPOCH() WHERE id = NEW.id;
END;

CREATE TRIGGER process_leases_touch
AFTER UPDATE ON process_leases
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE process_leases SET updated_at = UNIXEPOCH() WHERE process_id = NEW.process_id;
END;

CREATE TRIGGER agent_jobs_touch
AFTER UPDATE ON agent_jobs
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE agent_jobs SET updated_at = UNIXEPOCH() WHERE id = NEW.id;
END;

CREATE TRIGGER agent_job_items_touch
AFTER UPDATE ON agent_job_items
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE agent_job_items
    SET updated_at = UNIXEPOCH()
    WHERE job_id = NEW.job_id AND item_id = NEW.item_id;
END;

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

CREATE VIEW active_processes AS
    SELECT *
    FROM processes
    WHERE archived_at IS NULL
    ORDER BY updated_at DESC, created_at DESC, id DESC;

CREATE VIEW archived_processes AS
    SELECT *
    FROM processes
    WHERE archived_at IS NOT NULL
    ORDER BY archived_at DESC, updated_at DESC, id DESC;

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
      AND pl.expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    ORDER BY pl.expires_at ASC, pl.process_id ASC;

CREATE VIEW process_message_counts AS
    SELECT
        conversation_id AS process_id,
        COUNT(*) AS message_count,
        MAX(ts) AS last_message_ts,
        SUM(estimated_bytes) AS total_estimated_bytes
    FROM message_history
    GROUP BY conversation_id;

CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (UNIXEPOCH()),
    updated_at INTEGER NOT NULL DEFAULT (UNIXEPOCH())
);

CREATE TRIGGER settings_touch
AFTER UPDATE ON settings
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE settings SET updated_at = UNIXEPOCH() WHERE key = NEW.key;
END;
