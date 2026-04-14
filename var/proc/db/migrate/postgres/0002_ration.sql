-- Ration: rate-limit and usage snapshots sniffed from provider response
-- headers. ration_usage holds the latest reading per (provider, label)
-- so the TUI can render "85% left" without an API round-trip.
-- ration_history is append-only and never pruned — this is a database,
-- not a jsonl tail; keep every snapshot so trends survive forever.

CREATE TABLE ration_usage (
    provider TEXT NOT NULL,
    label TEXT NOT NULL,
    limit_value BIGINT,
    remaining BIGINT,
    utilization DOUBLE PRECISION NOT NULL,
    resets_at BIGINT,
    observed_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM clock_timestamp())::BIGINT,
    PRIMARY KEY (provider, label),
    CHECK (utilization >= 0.0 AND utilization <= 1.0),
    CHECK (limit_value IS NULL OR limit_value >= 0),
    CHECK (remaining IS NULL OR remaining >= 0)
);

CREATE INDEX idx_ration_usage_resets_at ON ration_usage(resets_at);

CREATE TRIGGER ration_usage_touch
BEFORE UPDATE ON ration_usage
FOR EACH ROW
EXECUTE FUNCTION chaos_touch_updated_at_epoch();

CREATE TABLE ration_history (
    id BIGSERIAL PRIMARY KEY,
    provider TEXT NOT NULL,
    label TEXT NOT NULL,
    limit_value BIGINT,
    remaining BIGINT,
    utilization DOUBLE PRECISION NOT NULL,
    resets_at BIGINT,
    observed_at BIGINT NOT NULL,
    CHECK (utilization >= 0.0 AND utilization <= 1.0),
    CHECK (limit_value IS NULL OR limit_value >= 0),
    CHECK (remaining IS NULL OR remaining >= 0)
);

CREATE INDEX idx_ration_history_provider_observed
    ON ration_history(provider, observed_at DESC, id DESC);
CREATE INDEX idx_ration_history_provider_label_observed
    ON ration_history(provider, label, observed_at DESC, id DESC);
