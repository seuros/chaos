CREATE TABLE execution_scopes (
    scope TEXT PRIMARY KEY,
    policy_digest TEXT
);
CREATE TABLE execution_process_bindings (
    process_id TEXT PRIMARY KEY,
    scope TEXT,
    policy_digest TEXT
);
-- Legacy histories have no verified policy. Ambiguous cross-scope histories
-- get a NULL owner and cannot be implicitly adopted by a new tenant.
INSERT INTO execution_process_bindings (process_id, scope, policy_digest)
SELECT process_id, CASE WHEN COUNT(DISTINCT scope) = 1 THEN MIN(scope) ELSE NULL END, NULL
FROM execution_sessions WHERE process_id IS NOT NULL GROUP BY process_id;
