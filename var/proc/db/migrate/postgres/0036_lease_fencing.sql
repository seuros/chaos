-- Same fencing contract as SQLite: monotonic per-resource tokens with a
-- high-water that survives release. Pre-existing grants are fenced.
DROP VIEW IF EXISTS active_process_leases;

ALTER TABLE process_leases DROP COLUMN lease_token;
ALTER TABLE process_leases ADD COLUMN lease_token BIGINT;
ALTER TABLE process_leases ADD COLUMN last_issued BIGINT NOT NULL DEFAULT 0;

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
      AND pl.lease_token IS NOT NULL
      AND pl.expires_at > EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT
    ORDER BY pl.expires_at ASC, pl.process_id ASC;

ALTER TABLE execution_sessions DROP COLUMN lease_token;
ALTER TABLE execution_sessions ADD COLUMN lease_token BIGINT;
ALTER TABLE execution_sessions ADD COLUMN last_issued BIGINT NOT NULL DEFAULT 0;

ALTER TABLE execution_outbox DROP COLUMN lease_token;
ALTER TABLE execution_outbox ADD COLUMN lease_token BIGINT;
ALTER TABLE execution_outbox ADD COLUMN last_issued BIGINT NOT NULL DEFAULT 0;
