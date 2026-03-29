CREATE TABLE model_catalog_cache (
    provider_name TEXT NOT NULL,
    wire_api TEXT NOT NULL,
    base_url TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    etag TEXT,
    client_version TEXT,
    models_json TEXT NOT NULL,
    PRIMARY KEY (provider_name, wire_api, base_url)
);

CREATE INDEX idx_model_catalog_cache_fetched_at
    ON model_catalog_cache(fetched_at);
