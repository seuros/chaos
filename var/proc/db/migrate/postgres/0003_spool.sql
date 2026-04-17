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
    submitted_at   BIGINT,
    completed_at   BIGINT,
    created_at     BIGINT NOT NULL,
    updated_at     BIGINT NOT NULL,
    CHECK (request_count >= 1)
);

CREATE INDEX idx_spool_jobs_status   ON spool_jobs(status);
CREATE INDEX idx_spool_jobs_batch_id ON spool_jobs(batch_id) WHERE batch_id IS NOT NULL;
CREATE INDEX idx_spool_jobs_backend  ON spool_jobs(backend);

ALTER TABLE cron_jobs ADD COLUMN kind        TEXT NOT NULL DEFAULT 'shell';
ALTER TABLE cron_jobs ADD COLUMN manifest_id TEXT;

CREATE INDEX idx_cron_jobs_kind        ON cron_jobs(kind);
CREATE INDEX idx_cron_jobs_manifest_id ON cron_jobs(manifest_id) WHERE manifest_id IS NOT NULL;

DROP VIEW IF EXISTS due_cron_jobs;
CREATE VIEW due_cron_jobs AS
    SELECT *
    FROM cron_jobs
    WHERE enabled = TRUE
      AND next_run_at IS NOT NULL
      AND next_run_at <= EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT
    ORDER BY next_run_at ASC;

-- spool_jobs updated_at trigger
CREATE OR REPLACE FUNCTION spool_jobs_touch_fn() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.updated_at IS NOT DISTINCT FROM OLD.updated_at THEN
        NEW.updated_at := EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER spool_jobs_touch
BEFORE UPDATE ON spool_jobs
FOR EACH ROW
EXECUTE FUNCTION spool_jobs_touch_fn();
