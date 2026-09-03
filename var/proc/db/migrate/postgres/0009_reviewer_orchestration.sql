CREATE TABLE review_runs (
    id TEXT PRIMARY KEY NOT NULL,
    review_run_subject TEXT NOT NULL UNIQUE,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE reviewer_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    run_id TEXT NOT NULL REFERENCES review_runs(id) ON DELETE CASCADE,
    ordinal BIGINT NOT NULL CHECK (ordinal >= 0),
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
    submission_json JSONB,
    failure TEXT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    completed_at BIGINT,
    UNIQUE (run_id, ordinal),
    UNIQUE (run_id, account_subject),
    UNIQUE (run_id, model_family_subject)
);

CREATE INDEX idx_reviewer_attempts_run_state
    ON reviewer_attempts(run_id, state, ordinal);

CREATE OR REPLACE FUNCTION chaos_reviewer_attempt_bindings_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF
        NEW.run_id IS DISTINCT FROM OLD.run_id
        OR NEW.ordinal IS DISTINCT FROM OLD.ordinal
        OR NEW.provider_id IS DISTINCT FROM OLD.provider_id
        OR NEW.model IS DISTINCT FROM OLD.model
        OR NEW.account_subject IS DISTINCT FROM OLD.account_subject
        OR NEW.model_family_subject IS DISTINCT FROM OLD.model_family_subject
        OR NEW.reviewer_attempt_subject IS DISTINCT FROM OLD.reviewer_attempt_subject
        OR NEW.idempotency_key IS DISTINCT FROM OLD.idempotency_key
        OR NEW.prompt IS DISTINCT FROM OLD.prompt
        OR NEW.mcp_server IS DISTINCT FROM OLD.mcp_server
        OR NEW.mcp_tool IS DISTINCT FROM OLD.mcp_tool
        OR (OLD.process_id IS NOT NULL AND NEW.process_id IS DISTINCT FROM OLD.process_id)
        OR (OLD.raw_output IS NOT NULL AND NEW.raw_output IS DISTINCT FROM OLD.raw_output)
        OR (
            OLD.submission_json IS NOT NULL
            AND NEW.submission_json IS DISTINCT FROM OLD.submission_json
        )
    THEN
        RAISE EXCEPTION 'reviewer attempt binding is immutable';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER reviewer_attempts_bindings_immutable
BEFORE UPDATE ON reviewer_attempts
FOR EACH ROW
EXECUTE FUNCTION chaos_reviewer_attempt_bindings_immutable();

CREATE OR REPLACE FUNCTION chaos_review_run_identity_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'review run identity is immutable';
END;
$$;

CREATE TRIGGER review_runs_identity_immutable
BEFORE UPDATE OF id, review_run_subject ON review_runs
FOR EACH ROW
EXECUTE FUNCTION chaos_review_run_identity_immutable();
