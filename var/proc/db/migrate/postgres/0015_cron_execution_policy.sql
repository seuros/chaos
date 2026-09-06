-- Existing shell jobs deliberately have no durable authorization.
ALTER TABLE cron_jobs ADD COLUMN execution_policy TEXT;

-- PostgreSQL expands SELECT * when the view is created.
CREATE OR REPLACE VIEW due_cron_jobs AS
SELECT * FROM cron_jobs
WHERE enabled = TRUE
  AND next_run_at IS NOT NULL
  AND next_run_at <= EXTRACT(EPOCH FROM NOW())::BIGINT
ORDER BY next_run_at ASC;
