-- Observation-only replay. These rows never authorize execution or recovery.
CREATE TABLE execution_replay (
    session_id TEXT PRIMARY KEY REFERENCES execution_sessions(session_id),
    sequence BIGINT NOT NULL DEFAULT 0 CHECK (sequence >= 0)
);
CREATE TABLE execution_replay_events (
    session_id TEXT NOT NULL REFERENCES execution_sessions(session_id),
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    event_json TEXT NOT NULL,
    event_bytes BIGINT NOT NULL CHECK (event_bytes > 0),
    PRIMARY KEY (session_id, sequence)
);
