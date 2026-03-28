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
    updated_at INTEGER NOT NULL
);

CREATE INDEX idx_cron_jobs_scope ON cron_jobs(scope);
CREATE INDEX idx_cron_jobs_next_run ON cron_jobs(next_run_at) WHERE enabled = 1;
CREATE INDEX idx_cron_jobs_project ON cron_jobs(project_path) WHERE project_path IS NOT NULL;
CREATE INDEX idx_cron_jobs_session ON cron_jobs(session_id) WHERE session_id IS NOT NULL;
