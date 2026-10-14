CREATE TABLE execution_delivery_targets (
    scope TEXT PRIMARY KEY REFERENCES execution_scopes(scope),
    destination_digest TEXT NOT NULL
);
CREATE TABLE execution_outbox (
    event_id TEXT PRIMARY KEY,
    event_key TEXT NOT NULL UNIQUE,
    scope TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES execution_sessions(session_id),
    turn_id TEXT,
    kind TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    body_json TEXT NOT NULL,
    attempts BIGINT NOT NULL DEFAULT 0,
    next_attempt_at_ms BIGINT NOT NULL,
    owner_id TEXT NOT NULL DEFAULT '',
    lease_token TEXT,
    expires_at_ms BIGINT NOT NULL DEFAULT 0,
    delivered_at_ms BIGINT
);
CREATE INDEX execution_outbox_due ON execution_outbox(scope, delivered_at_ms, next_attempt_at_ms);
