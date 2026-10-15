-- Control-plane identity is independent of the process transcript. Losing a
-- writer's lease never proves that its external side effects have stopped.
CREATE TABLE execution_sessions (
    session_id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    process_id TEXT,
    state TEXT NOT NULL CHECK (state IN ('idle', 'active', 'unknown', 'closed')),
    active_turn_id TEXT,
    owner_id TEXT NOT NULL,
    lease_token TEXT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    CHECK ((state IN ('idle', 'closed') AND active_turn_id IS NULL)
        OR (state = 'active' AND active_turn_id IS NOT NULL)
        OR state = 'unknown')
);
CREATE INDEX execution_sessions_scope ON execution_sessions (scope, session_id);
CREATE UNIQUE INDEX execution_sessions_live_process ON execution_sessions (process_id)
    WHERE state != 'closed' AND process_id IS NOT NULL;

CREATE TABLE execution_turns (
    session_id TEXT NOT NULL REFERENCES execution_sessions(session_id),
    turn_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN
        ('accepted', 'running', 'completed', 'failed', 'interrupted', 'timed_out', 'unknown')),
    outcome_json TEXT,
    PRIMARY KEY (session_id, turn_id)
);
