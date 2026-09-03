CREATE TABLE review_runs (
    id TEXT PRIMARY KEY NOT NULL,
    review_run_subject TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE reviewer_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL REFERENCES review_runs(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    state TEXT NOT NULL CHECK (
        state IN (
            'selection',
            'spawn',
            'model_execution',
            'output_parse',
            'submission_unknown',
            'acknowledged',
            'cancelled',
            'terminal_failure'
        )
    ),
    provider_id TEXT NOT NULL,
    model TEXT NOT NULL,
    account_subject TEXT NOT NULL,
    model_family_subject TEXT NOT NULL,
    reviewer_attempt_subject TEXT NOT NULL UNIQUE,
    idempotency_key TEXT NOT NULL UNIQUE,
    prompt TEXT NOT NULL,
    mcp_server TEXT NOT NULL,
    mcp_tool TEXT NOT NULL,
    process_id TEXT,
    raw_output TEXT,
    submission_json TEXT,
    failure TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER,
    UNIQUE (run_id, ordinal),
    UNIQUE (run_id, account_subject),
    UNIQUE (run_id, model_family_subject)
);

CREATE INDEX idx_reviewer_attempts_run_state
    ON reviewer_attempts(run_id, state, ordinal);

CREATE TRIGGER reviewer_attempts_bindings_immutable
BEFORE UPDATE ON reviewer_attempts
FOR EACH ROW
WHEN
    NEW.run_id IS NOT OLD.run_id
    OR NEW.ordinal IS NOT OLD.ordinal
    OR NEW.provider_id IS NOT OLD.provider_id
    OR NEW.model IS NOT OLD.model
    OR NEW.account_subject IS NOT OLD.account_subject
    OR NEW.model_family_subject IS NOT OLD.model_family_subject
    OR NEW.reviewer_attempt_subject IS NOT OLD.reviewer_attempt_subject
    OR NEW.idempotency_key IS NOT OLD.idempotency_key
    OR NEW.prompt IS NOT OLD.prompt
    OR NEW.mcp_server IS NOT OLD.mcp_server
    OR NEW.mcp_tool IS NOT OLD.mcp_tool
    OR (OLD.process_id IS NOT NULL AND NEW.process_id IS NOT OLD.process_id)
    OR (OLD.raw_output IS NOT NULL AND NEW.raw_output IS NOT OLD.raw_output)
    OR (OLD.submission_json IS NOT NULL AND NEW.submission_json IS NOT OLD.submission_json)
BEGIN
    SELECT RAISE(FAIL, 'reviewer attempt binding is immutable');
END;

CREATE TRIGGER review_runs_identity_immutable
BEFORE UPDATE OF id, review_run_subject ON review_runs
FOR EACH ROW
BEGIN
    SELECT RAISE(FAIL, 'review run identity is immutable');
END;
