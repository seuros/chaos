CREATE TABLE processes (
    id TEXT PRIMARY KEY,
    rollout_path TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    source TEXT NOT NULL,
    model_provider TEXT NOT NULL,
    cwd TEXT NOT NULL,
    title TEXT NOT NULL,
    sandbox_policy TEXT NOT NULL,
    approval_mode TEXT NOT NULL,
    tokens_used INTEGER NOT NULL DEFAULT 0,
    has_user_event INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0,
    archived_at INTEGER,
    git_sha TEXT,
    git_branch TEXT,
    git_origin_url TEXT
);

CREATE INDEX idx_threads_created_at ON processes(created_at DESC, id DESC);
CREATE INDEX idx_threads_updated_at ON processes(updated_at DESC, id DESC);
CREATE INDEX idx_threads_archived ON processes(archived);
CREATE INDEX idx_threads_source ON processes(source);
CREATE INDEX idx_threads_provider ON processes(model_provider);
