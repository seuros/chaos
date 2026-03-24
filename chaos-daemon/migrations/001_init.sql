-- The daemon's brain

CREATE TABLE identity (
    id       INTEGER PRIMARY KEY CHECK (id = 1), -- singleton
    name     TEXT    NOT NULL,
    persona  TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    last_seen  INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;

CREATE TABLE memories (
    id           INTEGER PRIMARY KEY,
    scope        TEXT    NOT NULL DEFAULT 'global',  -- 'global' | 'project:<name>'
    category     TEXT    NOT NULL,                    -- 'preference' | 'feedback' | 'fact' | 'skill'
    content      TEXT    NOT NULL,
    confidence   REAL    NOT NULL DEFAULT 1.0,
    created_at   INTEGER NOT NULL DEFAULT (unixepoch()),
    accessed_at  INTEGER NOT NULL DEFAULT (unixepoch()),
    access_count INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE INDEX idx_memories_scope    ON memories(scope);
CREATE INDEX idx_memories_category ON memories(category);

CREATE TABLE auth (
    provider    TEXT PRIMARY KEY,
    credentials BLOB    NOT NULL,  -- encrypted with age
    last_used   INTEGER
) STRICT;

CREATE TABLE skills (
    name       TEXT PRIMARY KEY,
    definition TEXT    NOT NULL,
    source     TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;

CREATE TABLE sessions (
    id         TEXT    PRIMARY KEY,
    project    TEXT,
    summary    TEXT,
    started_at INTEGER NOT NULL DEFAULT (unixepoch()),
    ended_at   INTEGER
) STRICT;

CREATE INDEX idx_sessions_project ON sessions(project);
