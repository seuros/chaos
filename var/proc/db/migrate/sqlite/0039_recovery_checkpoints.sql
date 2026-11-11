-- Counters outlive retained notes, including an empty checkpoint history.
CREATE TABLE recovery_checkpoint_counters (
    process_id TEXT PRIMARY KEY NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    last_checkpoint_id INTEGER NOT NULL DEFAULT 0 CHECK (last_checkpoint_id >= 0),
    last_recovery_id INTEGER NOT NULL DEFAULT 0 CHECK (last_recovery_id >= 0)
);

CREATE TABLE recovery_checkpoints (
    process_id TEXT NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    id INTEGER NOT NULL CHECK (id > 0),
    recovery_id INTEGER NOT NULL CHECK (recovery_id > 0),
    call_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    note_json TEXT NOT NULL CHECK (length(CAST(note_json AS BLOB)) <= 16384),
    observation_json TEXT NOT NULL CHECK (length(CAST(observation_json AS BLOB)) <= 16384),
    PRIMARY KEY (process_id, id),
    UNIQUE (process_id, call_id)
);
