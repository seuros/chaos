CREATE TABLE execution_price_schedules (
    scope TEXT NOT NULL REFERENCES execution_scopes(scope),
    version TEXT NOT NULL,
    schedule_json TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (scope, version)
);
CREATE TABLE execution_reconciliations (
    reconciliation_id TEXT PRIMARY KEY,
    scope TEXT NOT NULL REFERENCES execution_scopes(scope),
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    invocation_id TEXT NOT NULL REFERENCES execution_invocations(invocation_id),
    receipt_key TEXT NOT NULL UNIQUE,
    record_json TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    FOREIGN KEY (session_id, turn_id) REFERENCES execution_turns(session_id, turn_id)
);
CREATE INDEX execution_reconciliations_turn ON execution_reconciliations(session_id, turn_id);
