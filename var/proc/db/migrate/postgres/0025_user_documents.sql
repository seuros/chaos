-- Named user documents that used to be files: custom modes, prompts,
-- syntax themes, and halluacinate scripts.
CREATE TABLE user_documents (
    kind TEXT NOT NULL CHECK (kind IN ('mode', 'prompt', 'theme', 'script')),
    name TEXT NOT NULL,
    source TEXT NOT NULL,
    revision BIGINT NOT NULL,
    enabled BIGINT NOT NULL CHECK (enabled IN (0, 1)),
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (kind, name)
);
