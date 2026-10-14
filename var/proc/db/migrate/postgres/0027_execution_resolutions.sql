-- Same audit/idempotency contract as SQLite.
ALTER TABLE execution_sessions ADD COLUMN recovery_epoch BIGINT NOT NULL DEFAULT 0
    CHECK (recovery_epoch >= 0);
CREATE TABLE execution_resolutions (
    session_id TEXT PRIMARY KEY REFERENCES execution_sessions(session_id),
    resolution_id TEXT NOT NULL UNIQUE,
    request_json TEXT NOT NULL,
    resolved_at_ms BIGINT NOT NULL
);
