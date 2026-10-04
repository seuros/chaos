CREATE TABLE planning_workspace_sequence (
    id BIGINT PRIMARY KEY CHECK (id = 1),
    next_workspace BIGINT NOT NULL
);
INSERT INTO planning_workspace_sequence VALUES (1, 0);
CREATE TABLE planning_workspaces (
    id TEXT PRIMARY KEY,
    code TEXT NOT NULL UNIQUE CHECK (length(code) = 6 AND code NOT GLOB '*[^0-9A-F]*'),
    name TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    next_plan BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE planning_projects (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES planning_workspaces(id),
    name TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    UNIQUE (workspace_id, name),
    UNIQUE (workspace_id, id)
);
CREATE TABLE planning_checkouts (
    installation_id TEXT NOT NULL,
    path TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES planning_projects(id),
    PRIMARY KEY (installation_id, path)
);
CREATE TABLE planning_plans (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES planning_workspaces(id),
    code TEXT NOT NULL CHECK (length(code) = 4 AND code NOT GLOB '*[^0-9A-F]*'),
    title TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','completed','cancelled')),
    revision INTEGER NOT NULL DEFAULT 1,
    next_task BIGINT NOT NULL DEFAULT 1,
    next_event BIGINT NOT NULL DEFAULT 1,
    UNIQUE (workspace_id, code),
    UNIQUE (workspace_id, id)
);
CREATE TABLE planning_tasks (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL REFERENCES planning_plans(id),
    number BIGINT NOT NULL CHECK (number > 0),
    title TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','in_progress','blocked','completed','cancelled')),
    revision INTEGER NOT NULL DEFAULT 1,
    parent_id TEXT,
    position BIGINT NOT NULL DEFAULT 0,
    UNIQUE (plan_id, id),
    UNIQUE (plan_id, number),
    FOREIGN KEY (plan_id, parent_id) REFERENCES planning_tasks(plan_id, id),
    CHECK (parent_id IS NULL OR parent_id <> id)
);
CREATE INDEX planning_tasks_order ON planning_tasks(plan_id, parent_id, position, number);
CREATE TRIGGER planning_task_identity BEFORE UPDATE OF id,plan_id,number ON planning_tasks
WHEN NEW.id <> OLD.id OR NEW.plan_id <> OLD.plan_id OR NEW.number <> OLD.number BEGIN
    SELECT RAISE(ABORT, 'task identity is immutable');
END;
CREATE TRIGGER planning_parent_cycle BEFORE UPDATE OF parent_id ON planning_tasks
WHEN NEW.parent_id IS NOT NULL BEGIN
    SELECT RAISE(ABORT, 'containment cycle') WHERE EXISTS (
        WITH RECURSIVE ancestors(id) AS (
            SELECT NEW.parent_id
            UNION
            SELECT t.parent_id FROM planning_tasks t JOIN ancestors a ON t.id = a.id
            WHERE t.parent_id IS NOT NULL
        ) SELECT 1 FROM ancestors WHERE id = NEW.id
    );
END;
CREATE TRIGGER planning_parent_insert_cycle BEFORE INSERT ON planning_tasks
WHEN NEW.parent_id IS NOT NULL BEGIN
    SELECT RAISE(ABORT, 'containment cycle') WHERE EXISTS (
        WITH RECURSIVE ancestors(id) AS (
            SELECT NEW.parent_id
            UNION
            SELECT t.parent_id FROM planning_tasks t JOIN ancestors a ON t.id = a.id
            WHERE t.parent_id IS NOT NULL
        ) SELECT 1 FROM ancestors WHERE id = NEW.id
    );
END;
CREATE TABLE planning_targets (
    workspace_id TEXT NOT NULL,
    plan_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    PRIMARY KEY (task_id, project_id),
    FOREIGN KEY (workspace_id, plan_id) REFERENCES planning_plans(workspace_id, id),
    FOREIGN KEY (plan_id, task_id) REFERENCES planning_tasks(plan_id, id),
    FOREIGN KEY (workspace_id, project_id) REFERENCES planning_projects(workspace_id, id)
);
CREATE TABLE planning_history (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL REFERENCES planning_plans(id),
    seq BIGINT NOT NULL,
    task_id TEXT REFERENCES planning_tasks(id),
    actor TEXT NOT NULL,
    installation_id TEXT NOT NULL,
    event TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (plan_id, seq)
);
CREATE INDEX planning_history_page ON planning_history(plan_id, seq);
CREATE TABLE planning_requests (
    actor TEXT NOT NULL,
    request_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    result TEXT,
    PRIMARY KEY (actor, request_id)
);
CREATE TABLE planning_attachments (
    session_id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL REFERENCES planning_plans(id)
);
