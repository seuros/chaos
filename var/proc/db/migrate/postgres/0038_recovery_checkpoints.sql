-- Counters outlive retained notes, including an empty checkpoint history.
CREATE TABLE recovery_checkpoint_counters (
    process_id TEXT PRIMARY KEY NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    last_checkpoint_id BIGINT NOT NULL DEFAULT 0 CHECK (last_checkpoint_id >= 0),
    last_recovery_id BIGINT NOT NULL DEFAULT 0 CHECK (last_recovery_id >= 0)
);

CREATE TABLE recovery_checkpoints (
    process_id TEXT NOT NULL REFERENCES processes(id) ON DELETE CASCADE,
    id BIGINT NOT NULL CHECK (id > 0),
    recovery_id BIGINT NOT NULL CHECK (recovery_id > 0),
    call_id TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    note_json TEXT NOT NULL CHECK (octet_length(note_json) <= 16384),
    observation_json TEXT NOT NULL CHECK (octet_length(observation_json) <= 16384),
    PRIMARY KEY (process_id, id),
    UNIQUE (process_id, call_id)
);
