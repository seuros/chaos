CREATE TABLE clamp_exchanges (
    id BIGSERIAL PRIMARY KEY,
    session_id TEXT,
    turn_id TEXT,
    created_at BIGINT NOT NULL,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    status INTEGER,
    headers_json JSONB NOT NULL,
    request_json JSONB,
    response_body TEXT,
    response_truncated BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE INDEX idx_clamp_exchanges_created_at
    ON clamp_exchanges(created_at DESC);

CREATE INDEX idx_clamp_exchanges_session
    ON clamp_exchanges(session_id);
