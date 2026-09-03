ALTER TABLE review_runs
    ADD COLUMN owner_process_id TEXT NOT NULL DEFAULT 'legacy-unowned';

CREATE TRIGGER review_runs_owner_immutable
BEFORE UPDATE OF owner_process_id ON review_runs
FOR EACH ROW
BEGIN
    SELECT RAISE(FAIL, 'review run identity is immutable');
END;
