CREATE TABLE clamp_exchanges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT,
    turn_id TEXT,
    created_at INTEGER NOT NULL,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    status INTEGER,
    headers_json TEXT NOT NULL,
    request_json TEXT,
    response_body TEXT,
    response_truncated INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_clamp_exchanges_created_at
    ON clamp_exchanges(created_at DESC);

CREATE INDEX idx_clamp_exchanges_session
    ON clamp_exchanges(session_id);
