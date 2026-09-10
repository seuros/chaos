CREATE TABLE user_settings (
    id BIGINT PRIMARY KEY CHECK (id = 1),
    schema_version BIGINT NOT NULL CHECK (schema_version = 1),
    revision BIGINT NOT NULL,
    settings_json TEXT NOT NULL,
    migration_digest TEXT
);
INSERT INTO user_settings VALUES (1, 1, 0, '{}', NULL);

CREATE TABLE remembered_approvals (
    id TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    kind TEXT NOT NULL,
    subject TEXT NOT NULL,
    identity TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('active', 'revoked', 'pending_reapproval')),
    payload_json TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    UNIQUE (installation_id, scope, kind, subject, identity)
);
CREATE INDEX remembered_approvals_lookup
    ON remembered_approvals (installation_id, scope, kind, state);

CREATE TABLE configuration_events (
    id TEXT PRIMARY KEY,
    event TEXT NOT NULL,
    subject TEXT NOT NULL,
    created_at BIGINT NOT NULL
);

CREATE TABLE approval_revision (
    id BIGINT PRIMARY KEY CHECK (id = 1),
    revision BIGINT NOT NULL
);
INSERT INTO approval_revision VALUES (1, 0);

CREATE TABLE user_policy_sources (
    id BIGINT PRIMARY KEY CHECK (id = 1),
    sources_json TEXT NOT NULL
);
INSERT INTO user_policy_sources VALUES (1, '[]');
