-- The memory-consolidation and backfill subsystems were removed; nothing
-- reads or writes these tables anymore.
DROP INDEX IF EXISTS idx_stage1_outputs_source_updated_at;
DROP TABLE IF EXISTS stage1_outputs;
DROP INDEX IF EXISTS idx_jobs_kind_status_retry_lease;
DROP TABLE IF EXISTS jobs;
DROP TABLE IF EXISTS backfill_state;
