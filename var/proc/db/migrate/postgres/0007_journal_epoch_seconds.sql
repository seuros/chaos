-- Lease and journal timestamps are epoch seconds, like every other timestamp
-- column in this schema. Databases created before that alignment carry
-- TIMESTAMPTZ here; convert them in place.

DROP VIEW IF EXISTS active_process_leases;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'process_leases'
          AND column_name = 'expires_at'
          AND data_type <> 'bigint'
    ) THEN
        EXECUTE 'ALTER TABLE process_leases
                 ALTER COLUMN expires_at TYPE BIGINT
                 USING EXTRACT(EPOCH FROM expires_at)::BIGINT';
    END IF;

    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'journal_entries'
          AND column_name = 'recorded_at'
          AND data_type <> 'bigint'
    ) THEN
        EXECUTE 'ALTER TABLE journal_entries
                 ALTER COLUMN recorded_at TYPE BIGINT
                 USING EXTRACT(EPOCH FROM recorded_at)::BIGINT';
    END IF;
END
$$;

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
      AND pl.expires_at > EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT
    ORDER BY pl.expires_at ASC, pl.process_id ASC;
