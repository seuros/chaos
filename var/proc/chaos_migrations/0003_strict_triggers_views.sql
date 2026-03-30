-- Recreate cron_jobs as STRICT with CHECK constraint, add triggers and views.
-- Recreate model_catalog_cache as STRICT with a TTL view.

-- ── cron_jobs ────────────────────────────────────────────────────────

CREATE TABLE cron_jobs_new (
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
) STRICT;

INSERT INTO cron_jobs_new SELECT * FROM cron_jobs;
DROP TABLE cron_jobs;
ALTER TABLE cron_jobs_new RENAME TO cron_jobs;

CREATE INDEX idx_cron_jobs_scope ON cron_jobs(scope);
CREATE INDEX idx_cron_jobs_next_run ON cron_jobs(next_run_at) WHERE enabled = 1;
CREATE INDEX idx_cron_jobs_project ON cron_jobs(project_path) WHERE project_path IS NOT NULL;
CREATE INDEX idx_cron_jobs_session ON cron_jobs(session_id) WHERE session_id IS NOT NULL;

-- Auto-touch updated_at on any UPDATE that does not explicitly set it.
CREATE TRIGGER cron_jobs_touch AFTER UPDATE ON cron_jobs
FOR EACH ROW WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE cron_jobs SET updated_at = UNIXEPOCH() WHERE id = NEW.id;
END;

-- Convenience view: jobs the scheduler should pick up right now.
CREATE VIEW due_cron_jobs AS
    SELECT *
    FROM cron_jobs
    WHERE enabled = 1
      AND next_run_at IS NOT NULL
      AND next_run_at <= UNIXEPOCH()
    ORDER BY next_run_at ASC;

-- ── model_catalog_cache ──────────────────────────────────────────────

CREATE TABLE model_catalog_cache_new (
    provider_name TEXT NOT NULL,
    wire_api TEXT NOT NULL,
    base_url TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    etag TEXT,
    client_version TEXT,
    models_json TEXT NOT NULL,
    PRIMARY KEY (provider_name, wire_api, base_url)
) STRICT;

INSERT INTO model_catalog_cache_new SELECT * FROM model_catalog_cache;
DROP TABLE model_catalog_cache;
ALTER TABLE model_catalog_cache_new RENAME TO model_catalog_cache;

CREATE INDEX idx_model_catalog_cache_fetched_at
    ON model_catalog_cache(fetched_at);

-- TTL view: only entries fetched within the last 24 hours.
CREATE VIEW valid_model_cache AS
    SELECT *
    FROM model_catalog_cache
    WHERE fetched_at + 86400 >= UNIXEPOCH();
