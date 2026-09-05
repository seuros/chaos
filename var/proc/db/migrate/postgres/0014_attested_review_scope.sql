ALTER TABLE review_runs
    ADD COLUMN attestation_subject TEXT;

UPDATE review_runs
SET attestation_subject = review_run_subject
WHERE attestation_subject IS NULL;

ALTER TABLE review_runs
    ALTER COLUMN attestation_subject SET NOT NULL;

DROP TRIGGER review_runs_identity_immutable ON review_runs;

CREATE TRIGGER review_runs_identity_immutable
BEFORE UPDATE OF id, review_run_subject, attestation_subject, owner_process_id ON review_runs
FOR EACH ROW
EXECUTE FUNCTION chaos_review_run_identity_immutable();
