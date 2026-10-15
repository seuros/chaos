CREATE TABLE execution_descendants (
    process_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    root_turn_id TEXT NOT NULL,
    parent_process_id TEXT NOT NULL,
    registered_at_ms BIGINT NOT NULL,
    FOREIGN KEY (session_id, root_turn_id) REFERENCES execution_turns(session_id, turn_id)
);
CREATE INDEX execution_descendants_root ON execution_descendants(session_id, root_turn_id);
ALTER TABLE execution_invocations ADD COLUMN source_turn_id TEXT;
ALTER TABLE execution_turns ADD COLUMN accounting_coverage TEXT NOT NULL DEFAULT 'direct_logical_sampling_and_compaction';
