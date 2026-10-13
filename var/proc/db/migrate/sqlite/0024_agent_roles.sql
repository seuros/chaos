CREATE TABLE agent_roles (
    name TEXT PRIMARY KEY NOT NULL,
    format TEXT NOT NULL CHECK (format IN ('toml', 'md')),
    source TEXT NOT NULL,
    base_dir TEXT,
    revision BIGINT NOT NULL,
    enabled BIGINT NOT NULL CHECK (enabled IN (0, 1)),
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
