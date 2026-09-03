ALTER TABLE review_runs
    ADD COLUMN owner_process_id TEXT;

UPDATE review_runs
SET owner_process_id = 'legacy-unowned'
WHERE owner_process_id IS NULL;

ALTER TABLE review_runs
    ALTER COLUMN owner_process_id SET NOT NULL;

DROP TRIGGER review_runs_identity_immutable ON review_runs;

CREATE TRIGGER review_runs_identity_immutable
BEFORE UPDATE OF id, review_run_subject, owner_process_id ON review_runs
FOR EACH ROW
EXECUTE FUNCTION chaos_review_run_identity_immutable();
