CREATE TABLE global_mcp_servers (
    name TEXT PRIMARY KEY NOT NULL,
    config_json JSONB NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE INDEX idx_global_mcp_servers_updated_at
    ON global_mcp_servers(updated_at DESC);
