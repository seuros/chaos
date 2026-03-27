CREATE TABLE process_spawn_edges (
    parent_process_id TEXT NOT NULL,
    child_process_id TEXT NOT NULL PRIMARY KEY,
    status TEXT NOT NULL
);

CREATE INDEX idx_process_spawn_edges_parent_status
    ON process_spawn_edges(parent_process_id, status);
