-- Explicit point-in-time export and operator attestation. No payload deletion:
-- live deduplication still compares original immutable receipts/outcomes.
CREATE TABLE execution_archives (
    session_id TEXT NOT NULL REFERENCES execution_sessions(session_id),
    scope TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    manifest_json TEXT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    exported_at_ms BIGINT NOT NULL,
    sealed_at_ms BIGINT,
    archive_reference TEXT,
    actor TEXT,
    PRIMARY KEY (session_id, manifest_digest),
    UNIQUE (session_id, revision),
    CHECK ((sealed_at_ms IS NULL AND archive_reference IS NULL AND actor IS NULL)
        OR (sealed_at_ms IS NOT NULL AND archive_reference IS NOT NULL AND actor IS NOT NULL))
);
