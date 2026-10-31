CREATE TABLE installation_settings (
    installation_id TEXT PRIMARY KEY,
    revision BIGINT NOT NULL,
    settings_json TEXT NOT NULL
);
