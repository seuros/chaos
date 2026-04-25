CREATE TABLE project_trust (
    project_path TEXT PRIMARY KEY NOT NULL,
    trust_level TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE INDEX idx_project_trust_updated_at
    ON project_trust(updated_at DESC);
