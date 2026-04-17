-- Spool: durable apprentice-job lifecycle.
--
-- payload_json holds a JSON array of caller-supplied custom_ids for traceability;
-- result_json holds Vec<(custom_id, TurnResult)>. batch_id is NULL until
-- the provider accepts submit.

CREATE TABLE spool_jobs (
    manifest_id    TEXT PRIMARY KEY,
    backend        TEXT NOT NULL,
    batch_id       TEXT,
    status         TEXT NOT NULL,
    request_count  INTEGER NOT NULL,
    payload_json   TEXT NOT NULL,
    result_json    TEXT,
    raw_result     TEXT,
    error          TEXT,
    submitted_at   INTEGER,
    completed_at   INTEGER,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    CHECK (request_count >= 1)
);

CREATE INDEX idx_spool_jobs_status   ON spool_jobs(status);
CREATE INDEX idx_spool_jobs_batch_id ON spool_jobs(batch_id) WHERE batch_id IS NOT NULL;
CREATE INDEX idx_spool_jobs_backend  ON spool_jobs(backend);

CREATE TRIGGER spool_jobs_touch
AFTER UPDATE ON spool_jobs
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE spool_jobs SET updated_at = UNIXEPOCH() WHERE manifest_id = NEW.manifest_id;
END;

ALTER TABLE cron_jobs ADD COLUMN kind        TEXT NOT NULL DEFAULT 'shell';
ALTER TABLE cron_jobs ADD COLUMN manifest_id TEXT;

CREATE INDEX idx_cron_jobs_kind        ON cron_jobs(kind);
CREATE INDEX idx_cron_jobs_manifest_id ON cron_jobs(manifest_id) WHERE manifest_id IS NOT NULL;

DROP VIEW IF EXISTS due_cron_jobs;
CREATE VIEW due_cron_jobs AS
    SELECT *
    FROM cron_jobs
    WHERE enabled = 1
      AND next_run_at IS NOT NULL
      AND next_run_at <= UNIXEPOCH()
    ORDER BY next_run_at ASC;
