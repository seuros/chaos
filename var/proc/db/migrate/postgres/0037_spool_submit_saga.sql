-- saga_json holds the submit saga's progress (direction, step, attempt) plus
-- the poll wiring it needs to finish; NULL once the saga completes or aborts.
ALTER TABLE spool_jobs ADD COLUMN saga_json TEXT;
