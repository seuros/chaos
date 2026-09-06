-- Existing shell jobs deliberately have no durable authorization.
ALTER TABLE cron_jobs ADD COLUMN execution_policy TEXT;
