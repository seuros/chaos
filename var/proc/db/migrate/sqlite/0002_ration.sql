-- Ration: rate-limit and usage snapshots sniffed from provider response
-- headers. ration_usage holds the latest reading per (provider, label)
-- so the TUI can render "85% left" without an API round-trip.
-- ration_history is append-only and never pruned — this is a database,
-- not a jsonl tail; keep every snapshot so trends survive forever.

CREATE TABLE ration_usage (
    provider TEXT NOT NULL,
    label TEXT NOT NULL,
    limit_value INTEGER,
    remaining INTEGER,
    utilization REAL NOT NULL,
    resets_at INTEGER,
    observed_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (UNIXEPOCH()),
    PRIMARY KEY (provider, label),
    CHECK (utilization >= 0.0 AND utilization <= 1.0),
    CHECK (limit_value IS NULL OR limit_value >= 0),
    CHECK (remaining IS NULL OR remaining >= 0)
);

CREATE INDEX idx_ration_usage_resets_at ON ration_usage(resets_at);

CREATE TRIGGER ration_usage_touch
AFTER UPDATE ON ration_usage
FOR EACH ROW
WHEN NEW.updated_at = OLD.updated_at
BEGIN
    UPDATE ration_usage
    SET updated_at = UNIXEPOCH()
    WHERE provider = NEW.provider AND label = NEW.label;
END;

CREATE TABLE ration_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider TEXT NOT NULL,
    label TEXT NOT NULL,
    limit_value INTEGER,
    remaining INTEGER,
    utilization REAL NOT NULL,
    resets_at INTEGER,
    observed_at INTEGER NOT NULL,
    CHECK (utilization >= 0.0 AND utilization <= 1.0),
    CHECK (limit_value IS NULL OR limit_value >= 0),
    CHECK (remaining IS NULL OR remaining >= 0)
);

CREATE INDEX idx_ration_history_provider_observed
    ON ration_history(provider, observed_at DESC, id DESC);
CREATE INDEX idx_ration_history_provider_label_observed
    ON ration_history(provider, label, observed_at DESC, id DESC);
