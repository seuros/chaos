ALTER TABLE execution_turns ADD COLUMN accounting_enabled INTEGER NOT NULL DEFAULT 0;
CREATE TABLE execution_invocations (
    invocation_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    request_json TEXT NOT NULL,
    started_at_ms BIGINT NOT NULL,
    state TEXT NOT NULL,
    receipt_json TEXT,
    duplicate_receipt_of TEXT,
    finished_at_ms BIGINT,
    FOREIGN KEY (session_id, turn_id) REFERENCES execution_turns(session_id, turn_id)
);
CREATE INDEX execution_invocations_turn ON execution_invocations(session_id, turn_id);
CREATE TABLE execution_provider_receipts (
    receipt_key TEXT PRIMARY KEY,
    invocation_id TEXT NOT NULL UNIQUE REFERENCES execution_invocations(invocation_id)
);
