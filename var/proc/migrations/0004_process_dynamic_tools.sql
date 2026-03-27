CREATE TABLE process_dynamic_tools (
    process_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    input_schema TEXT NOT NULL,
    PRIMARY KEY(process_id, position),
    FOREIGN KEY(process_id) REFERENCES processes(id) ON DELETE CASCADE
);

CREATE INDEX idx_process_dynamic_tools_thread ON process_dynamic_tools(process_id);
