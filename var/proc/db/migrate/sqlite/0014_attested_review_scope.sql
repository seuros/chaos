ALTER TABLE review_runs
    ADD COLUMN attestation_subject TEXT;

UPDATE review_runs
SET attestation_subject = review_run_subject
WHERE attestation_subject IS NULL;

CREATE TRIGGER review_runs_attestation_subject_immutable
BEFORE UPDATE OF attestation_subject ON review_runs
FOR EACH ROW
BEGIN
    SELECT RAISE(FAIL, 'review run identity is immutable');
END;
