ALTER TABLE planning_plans ADD COLUMN body TEXT NOT NULL DEFAULT '';
ALTER TABLE planning_plans ADD COLUMN body_revision SMALLINT NOT NULL DEFAULT 1;
ALTER TABLE planning_plans ADD COLUMN incorporated_seq BIGINT NOT NULL DEFAULT 0;
ALTER TABLE planning_tasks ADD COLUMN body TEXT NOT NULL DEFAULT '';
ALTER TABLE planning_tasks ADD COLUMN body_revision SMALLINT NOT NULL DEFAULT 1;
ALTER TABLE planning_tasks ADD COLUMN incorporated_seq BIGINT NOT NULL DEFAULT 0;

CREATE TABLE planning_clarifications (
    plan_id TEXT NOT NULL REFERENCES planning_plans(id),
    task_id TEXT,
    seq BIGINT NOT NULL,
    text TEXT NOT NULL,
    actor TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP::TEXT,
    PRIMARY KEY (plan_id, seq),
    FOREIGN KEY (plan_id, task_id) REFERENCES planning_tasks(plan_id, id)
);
CREATE INDEX planning_clarifications_item ON planning_clarifications(plan_id, task_id, seq);
