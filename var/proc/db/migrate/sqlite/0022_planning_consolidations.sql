CREATE TABLE planning_consolidations (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL REFERENCES planning_plans(id),
    task_id TEXT,
    actor TEXT NOT NULL,
    installation_id TEXT NOT NULL,
    origin_call_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    request TEXT NOT NULL,
    input TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'queued'
        CHECK (state IN ('queued','running','completed','failed','cancelled','stale')),
    execution_id TEXT,
    result_body TEXT,
    error TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (actor, request_id),
    FOREIGN KEY (plan_id, task_id) REFERENCES planning_tasks(plan_id, id)
);
CREATE INDEX planning_consolidations_owner ON planning_consolidations(actor, state, id);
