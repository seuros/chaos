CREATE TABLE global_mcp_servers (
    name TEXT PRIMARY KEY NOT NULL,
    config_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX idx_global_mcp_servers_updated_at
    ON global_mcp_servers(updated_at DESC);
