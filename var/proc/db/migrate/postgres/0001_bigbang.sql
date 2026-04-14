-- Let there be a runtime database.
-- PostgreSQL edition: use native types and trigger machinery where it helps.

CREATE TABLE processes (
    id TEXT PRIMARY KEY NOT NULL,
    parent_process_id TEXT REFERENCES processes(id) ON DELETE RESTRICT,
    fork_at_seq BIGINT,
    source TEXT NOT NULL,
    source_json JSONB NOT NULL,
    model_provider TEXT NOT NULL DEFAULT 'unknown',
    cwd TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    archived_at BIGINT,
    title TEXT NOT NULL DEFAULT '',
    sandbox_policy TEXT NOT NULL DEFAULT '',
    approval_mode TEXT NOT NULL DEFAULT '',
    tokens_used BIGINT NOT NULL DEFAULT 0,
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
    process_name TEXT,
    CHECK (parent_process_id IS NULL OR parent_process_id <> id),
    CHECK (
        (parent_process_id IS NULL AND fork_at_seq IS NULL)
        OR (parent_process_id IS NOT NULL AND fork_at_seq IS NOT NULL)
    )
);

CREATE INDEX idx_processes_created_at ON processes(created_at DESC, id DESC);
CREATE INDEX idx_processes_updated_at ON processes(updated_at DESC, id DESC);
CREATE INDEX idx_processes_archived_at ON processes(archived_at);
CREATE INDEX idx_processes_source ON processes(source);
CREATE INDEX idx_processes_provider ON processes(model_provider);
CREATE INDEX idx_processes_parent_process_id ON processes(parent_process_id);
CREATE INDEX idx_processes_git_branch ON processes(git_branch);
CREATE INDEX idx_processes_process_name ON processes(process_name);
CREATE INDEX idx_processes_active_updated_at
    ON processes(updated_at DESC, created_at DESC, id DESC)
    WHERE archived_at IS NULL;
CREATE INDEX idx_processes_archived_updated_at
    ON processes(archived_at DESC, updated_at DESC, id DESC)
    WHERE archived_at IS NOT NULL;

CREATE TABLE process_closure (
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

CREATE INDEX idx_process_closure_ancestor_depth
    ON process_closure(ancestor_process_id, depth, descendant_process_id);
CREATE INDEX idx_process_closure_descendant_depth
    ON process_closure(descendant_process_id, depth, ancestor_process_id);

CREATE TABLE process_leases (
    process_id TEXT PRIMARY KEY NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    owner_id TEXT NOT NULL,
    lease_token TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE INDEX idx_process_leases_expires_at ON process_leases(expires_at);

CREATE TABLE journal_entries (
    process_id TEXT NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    seq BIGINT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL,
    item_type TEXT NOT NULL,
    payload_json JSONB NOT NULL,
    PRIMARY KEY (process_id, seq)
);

CREATE INDEX idx_journal_entries_process_seq ON journal_entries(process_id, seq);

CREATE TABLE logs (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ts BIGINT NOT NULL,
    ts_nanos INTEGER NOT NULL,
    level TEXT NOT NULL,
    target TEXT NOT NULL,
    message TEXT,
    process_id TEXT,
    process_uuid TEXT,
    module_path TEXT,
    file TEXT,
    line INTEGER,
    estimated_bytes BIGINT NOT NULL DEFAULT 0
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
    input_schema JSONB NOT NULL,
    defer_loading BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (process_id, position)
);

CREATE INDEX idx_process_dynamic_tools_thread ON process_dynamic_tools(process_id);

CREATE TABLE stage1_outputs (
    process_id TEXT PRIMARY KEY REFERENCES processes(id) ON DELETE CASCADE,
    source_updated_at BIGINT NOT NULL,
    raw_memory TEXT NOT NULL,
    rollout_summary TEXT NOT NULL,
    generated_at BIGINT NOT NULL,
    rollout_slug TEXT,
    usage_count BIGINT,
    last_usage BIGINT,
    selected_for_phase2 BOOLEAN NOT NULL DEFAULT FALSE,
    selected_for_phase2_source_updated_at BIGINT
);

CREATE INDEX idx_stage1_outputs_source_updated_at
    ON stage1_outputs(source_updated_at DESC, process_id DESC);

CREATE TABLE jobs (
    kind TEXT NOT NULL,
    job_key TEXT NOT NULL,
    status TEXT NOT NULL,
    worker_id TEXT,
    ownership_token TEXT,
    started_at BIGINT,
    finished_at BIGINT,
    lease_until BIGINT,
    retry_at BIGINT,
    retry_remaining INTEGER NOT NULL,
    last_error TEXT,
    input_watermark BIGINT,
    last_success_watermark BIGINT,
    PRIMARY KEY (kind, job_key)
);

CREATE INDEX idx_jobs_kind_status_retry_lease
    ON jobs(kind, status, retry_at, lease_until);

CREATE TABLE backfill_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    status TEXT NOT NULL,
    last_watermark TEXT,
    last_success_at BIGINT,
    updated_at BIGINT NOT NULL
);

INSERT INTO backfill_state (id, status, last_watermark, last_success_at, updated_at)
VALUES (1, 'pending', NULL, NULL, EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT);

CREATE TABLE agent_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    status TEXT NOT NULL,
    instruction TEXT NOT NULL,
    output_schema_json JSONB,
    input_headers_json JSONB NOT NULL,
    input_csv_path TEXT NOT NULL,
    output_csv_path TEXT NOT NULL,
    auto_export BOOLEAN NOT NULL DEFAULT TRUE,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    started_at BIGINT,
    completed_at BIGINT,
    last_error TEXT,
    max_runtime_seconds INTEGER
);

CREATE TABLE agent_job_items (
    job_id TEXT NOT NULL REFERENCES agent_jobs(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    row_index INTEGER NOT NULL,
    source_id TEXT,
    row_json JSONB NOT NULL,
    status TEXT NOT NULL,
    assigned_process_id TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    result_json JSONB,
    last_error TEXT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    completed_at BIGINT,
    reported_at BIGINT,
    PRIMARY KEY (job_id, item_id)
);

CREATE INDEX idx_agent_jobs_status ON agent_jobs(status, updated_at DESC);
CREATE INDEX idx_agent_job_items_status ON agent_job_items(job_id, status, row_index ASC);

CREATE TABLE message_history (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    ts BIGINT NOT NULL,
    text TEXT NOT NULL,
    estimated_bytes BIGINT NOT NULL CHECK (estimated_bytes >= 0)
);

CREATE TABLE cron_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    schedule TEXT NOT NULL,
    command TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT 'project',
    project_path TEXT,
    session_id TEXT,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_run_at BIGINT,
    next_run_at BIGINT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE INDEX idx_cron_jobs_scope ON cron_jobs(scope);
CREATE INDEX idx_cron_jobs_next_run ON cron_jobs(next_run_at) WHERE enabled = TRUE;
CREATE INDEX idx_cron_jobs_project ON cron_jobs(project_path) WHERE project_path IS NOT NULL;
CREATE INDEX idx_cron_jobs_session ON cron_jobs(session_id) WHERE session_id IS NOT NULL;

CREATE VIEW due_cron_jobs AS
    SELECT *
    FROM cron_jobs
    WHERE enabled = TRUE
      AND next_run_at IS NOT NULL
      AND next_run_at <= EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT
    ORDER BY next_run_at ASC;

CREATE TABLE model_catalog_cache (
    provider_name TEXT NOT NULL,
    wire_api TEXT NOT NULL,
    base_url TEXT NOT NULL,
    fetched_at BIGINT NOT NULL,
    etag TEXT,
    client_version TEXT,
    models_json JSONB NOT NULL,
    PRIMARY KEY (provider_name, wire_api, base_url)
);

CREATE INDEX idx_model_catalog_cache_fetched_at
    ON model_catalog_cache(fetched_at);

CREATE VIEW valid_model_cache AS
    SELECT *
    FROM model_catalog_cache
    WHERE fetched_at + 86400 >= EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT;

CREATE OR REPLACE FUNCTION chaos_touch_updated_at_epoch()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.updated_at = OLD.updated_at THEN
        NEW.updated_at := EXTRACT(EPOCH FROM clock_timestamp())::BIGINT;
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION chaos_processes_enforce_lineage_immutability()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.parent_process_id IS DISTINCT FROM OLD.parent_process_id THEN
        RAISE EXCEPTION 'processes.parent_process_id is immutable';
    END IF;

    IF NEW.fork_at_seq IS DISTINCT FROM OLD.fork_at_seq THEN
        RAISE EXCEPTION 'processes.fork_at_seq is immutable';
    END IF;

    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION chaos_processes_insert_closure()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF EXISTS (
        WITH RECURSIVE lineage_walk AS (
            SELECT
                np.id AS start_id,
                np.parent_process_id,
                ARRAY[np.id]::TEXT[] AS path
            FROM new_processes AS np
            WHERE np.parent_process_id IS NOT NULL

            UNION ALL

            SELECT
                lw.start_id,
                p.parent_process_id,
                lw.path || p.id
            FROM lineage_walk AS lw
            JOIN processes AS p
              ON p.id = lw.parent_process_id
            WHERE NOT p.id = ANY(lw.path)
        )
        SELECT 1
        FROM lineage_walk AS lw
        JOIN processes AS p
          ON p.id = lw.parent_process_id
        WHERE p.id = ANY(lw.path)
        LIMIT 1
    ) THEN
        RAISE EXCEPTION 'process lineage cannot contain cycles';
    END IF;

    INSERT INTO process_closure (
        ancestor_process_id,
        descendant_process_id,
        depth
    )
    WITH RECURSIVE lineage AS (
        SELECT
            np.id AS descendant_process_id,
            np.id AS ancestor_process_id,
            0::INTEGER AS depth,
            np.parent_process_id
        FROM new_processes AS np

        UNION ALL

        SELECT
            lineage.descendant_process_id,
            p.id AS ancestor_process_id,
            lineage.depth + 1,
            p.parent_process_id
        FROM lineage
        JOIN processes AS p
          ON p.id = lineage.parent_process_id
    )
    SELECT
        ancestor_process_id,
        descendant_process_id,
        depth
    FROM lineage
    ON CONFLICT DO NOTHING;

    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION chaos_raise_append_only()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'journal_entries is append-only';
END;
$$;

CREATE TRIGGER processes_touch
BEFORE UPDATE ON processes
FOR EACH ROW
EXECUTE FUNCTION chaos_touch_updated_at_epoch();

CREATE TRIGGER processes_lineage_immutable
BEFORE UPDATE ON processes
FOR EACH ROW
EXECUTE FUNCTION chaos_processes_enforce_lineage_immutability();

CREATE TRIGGER processes_insert_closure
AFTER INSERT ON processes
REFERENCING NEW TABLE AS new_processes
FOR EACH STATEMENT
EXECUTE FUNCTION chaos_processes_insert_closure();

CREATE TRIGGER process_leases_touch
BEFORE UPDATE ON process_leases
FOR EACH ROW
EXECUTE FUNCTION chaos_touch_updated_at_epoch();

CREATE TRIGGER agent_jobs_touch
BEFORE UPDATE ON agent_jobs
FOR EACH ROW
EXECUTE FUNCTION chaos_touch_updated_at_epoch();

CREATE TRIGGER agent_job_items_touch
BEFORE UPDATE ON agent_job_items
FOR EACH ROW
EXECUTE FUNCTION chaos_touch_updated_at_epoch();

CREATE TRIGGER cron_jobs_touch
BEFORE UPDATE ON cron_jobs
FOR EACH ROW
EXECUTE FUNCTION chaos_touch_updated_at_epoch();

CREATE TRIGGER journal_entries_no_update
BEFORE UPDATE ON journal_entries
FOR EACH ROW
EXECUTE FUNCTION chaos_raise_append_only();

CREATE TRIGGER journal_entries_no_delete
BEFORE DELETE ON journal_entries
FOR EACH ROW
EXECUTE FUNCTION chaos_raise_append_only();

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
    FROM process_leases AS pl
    JOIN processes AS p ON p.id = pl.process_id
    WHERE p.archived_at IS NULL
      AND pl.expires_at > CURRENT_TIMESTAMP
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
    created_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT),
    updated_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT)
);

CREATE TRIGGER settings_touch
BEFORE UPDATE ON settings
FOR EACH ROW
EXECUTE FUNCTION chaos_touch_updated_at_epoch();
