-- Lease tokens become monotonic fencing tokens issued per resource. The token
-- high-water survives release so every successor receives a greater token.
-- Grants issued before this step carried opaque strings no history issued;
-- their holders are fenced on their next write and must acquire again.
DROP VIEW IF EXISTS active_process_leases;

ALTER TABLE process_leases DROP COLUMN lease_token;
ALTER TABLE process_leases ADD COLUMN lease_token INTEGER;
ALTER TABLE process_leases ADD COLUMN last_issued INTEGER NOT NULL DEFAULT 0;

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
      AND pl.lease_token IS NOT NULL
      AND pl.expires_at > CAST(strftime('%s', 'now') AS INTEGER)
    ORDER BY pl.expires_at ASC, pl.process_id ASC;

ALTER TABLE execution_sessions DROP COLUMN lease_token;
ALTER TABLE execution_sessions ADD COLUMN lease_token INTEGER;
ALTER TABLE execution_sessions ADD COLUMN last_issued INTEGER NOT NULL DEFAULT 0;

ALTER TABLE execution_outbox DROP COLUMN lease_token;
ALTER TABLE execution_outbox ADD COLUMN lease_token INTEGER;
ALTER TABLE execution_outbox ADD COLUMN last_issued INTEGER NOT NULL DEFAULT 0;
