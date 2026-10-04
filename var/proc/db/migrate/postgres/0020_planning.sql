CREATE TABLE planning_workspace_sequence (
    id BIGINT PRIMARY KEY CHECK (id = 1),
    next_workspace BIGINT NOT NULL
);
INSERT INTO planning_workspace_sequence VALUES (1, 0);
CREATE TABLE planning_workspaces (
    id TEXT PRIMARY KEY,
    code TEXT NOT NULL UNIQUE CHECK (code ~ '^[0-9A-F]{6}$'),
    name TEXT NOT NULL,
    revision SMALLINT NOT NULL DEFAULT 1,
    next_plan BIGINT NOT NULL DEFAULT 0
);
CREATE TABLE planning_projects (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES planning_workspaces(id),
    name TEXT NOT NULL,
    revision SMALLINT NOT NULL DEFAULT 1,
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
    code TEXT NOT NULL CHECK (code ~ '^[0-9A-F]{4}$'),
    title TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','completed','cancelled')),
    revision SMALLINT NOT NULL DEFAULT 1,
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
    revision SMALLINT NOT NULL DEFAULT 1,
    parent_id TEXT,
    position BIGINT NOT NULL DEFAULT 0,
    UNIQUE (plan_id, id),
    UNIQUE (plan_id, number),
    FOREIGN KEY (plan_id, parent_id) REFERENCES planning_tasks(plan_id, id),
    CHECK (parent_id IS NULL OR parent_id <> id)
);
CREATE INDEX planning_tasks_order ON planning_tasks(plan_id, parent_id, position, number);
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
    event JSONB NOT NULL,
    created_at TEXT NOT NULL DEFAULT clock_timestamp()::text,
    UNIQUE (plan_id, seq)
);
CREATE INDEX planning_history_page ON planning_history(plan_id, seq);
CREATE TABLE planning_requests (
    actor TEXT NOT NULL,
    request_id TEXT NOT NULL,
    payload JSONB NOT NULL,
    result JSONB,
    PRIMARY KEY (actor, request_id)
);
CREATE TABLE planning_attachments (
    session_id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL REFERENCES planning_plans(id)
);
CREATE TABLE planning_edges (
    plan_id TEXT NOT NULL REFERENCES planning_plans(id),
    parent_id TEXT NOT NULL,
    child_id TEXT NOT NULL,
    PRIMARY KEY (plan_id, parent_id, child_id),
    FOREIGN KEY (plan_id, parent_id) REFERENCES planning_tasks(plan_id, id),
    FOREIGN KEY (plan_id, child_id) REFERENCES planning_tasks(plan_id, id),
    CHECK (parent_id <> child_id)
);
CREATE TABLE planning_paths (
    plan_id TEXT NOT NULL REFERENCES planning_plans(id),
    ancestor_id TEXT NOT NULL REFERENCES planning_tasks(id),
    descendant_id TEXT NOT NULL REFERENCES planning_tasks(id),
    min_depth BIGINT NOT NULL,
    path_count NUMERIC NOT NULL,
    PRIMARY KEY (plan_id, ancestor_id, descendant_id)
);
CREATE INDEX planning_paths_reverse ON planning_paths(plan_id, descendant_id, ancestor_id);

-- Shared lock order: plan row, then graph advisory lock. Never a session lock.
CREATE FUNCTION planning_lock(p TEXT) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'planning graph writes require READ COMMITTED';
    END IF;
    PERFORM 1 FROM planning_plans WHERE id = p FOR UPDATE;
    PERFORM pg_advisory_xact_lock(hashtextextended('chaos:planning:' || p, 0));
END $$;

CREATE FUNCTION planning_task_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM planning_lock(NEW.plan_id);
    IF TG_OP = 'UPDATE' AND (NEW.id <> OLD.id OR NEW.plan_id <> OLD.plan_id OR NEW.number <> OLD.number) THEN
        RAISE EXCEPTION 'task identity is immutable';
    END IF;
    IF NEW.parent_id IS NOT NULL AND EXISTS (
        WITH RECURSIVE ancestors(id) AS (
            SELECT NEW.parent_id UNION
            SELECT t.parent_id FROM planning_tasks t JOIN ancestors a ON t.id = a.id
            WHERE t.parent_id IS NOT NULL
        ) SELECT 1 FROM ancestors WHERE id = NEW.id
    ) THEN
        RAISE EXCEPTION 'containment cycle';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER planning_task_guard BEFORE INSERT OR UPDATE ON planning_tasks
FOR EACH ROW EXECUTE FUNCTION planning_task_guard();

CREATE FUNCTION planning_task_self() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO planning_paths VALUES (NEW.plan_id, NEW.id, NEW.id, 0, 1);
    RETURN NEW;
END $$;
CREATE TRIGGER planning_task_self AFTER INSERT ON planning_tasks
FOR EACH ROW EXECUTE FUNCTION planning_task_self();

-- Rebuild and validation use edges, never potentially damaged closure rows.
CREATE FUNCTION planning_path_truth(p TEXT)
RETURNS TABLE (ancestor_id TEXT, descendant_id TEXT, min_depth BIGINT, path_count NUMERIC)
LANGUAGE sql AS $$
    WITH RECURSIVE walk(a, d, depth) AS (
        SELECT id, id, 0::bigint FROM planning_tasks WHERE plan_id = p
        UNION ALL
        SELECT w.a, e.child_id, w.depth + 1 FROM walk w
        JOIN planning_edges e ON e.parent_id = w.d AND e.plan_id = p
    )
    SELECT a, d, min(depth), count(*)::numeric FROM walk GROUP BY a, d
$$;
CREATE FUNCTION planning_rebuild_paths(p TEXT) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    PERFORM planning_lock(p);
    DELETE FROM planning_paths WHERE plan_id = p;
    INSERT INTO planning_paths SELECT p, * FROM planning_path_truth(p);
END $$;
CREATE FUNCTION planning_validate_paths(p TEXT)
RETURNS TABLE (ancestor_id TEXT, descendant_id TEXT) LANGUAGE sql AS $$
    SELECT coalesce(s.ancestor_id,t.ancestor_id), coalesce(s.descendant_id,t.descendant_id)
    FROM (SELECT * FROM planning_paths WHERE plan_id = p) s
    FULL JOIN planning_path_truth(p) t USING (ancestor_id, descendant_id)
    WHERE s.min_depth IS DISTINCT FROM t.min_depth OR s.path_count IS DISTINCT FROM t.path_count
$$;

CREATE FUNCTION planning_edge_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        RAISE EXCEPTION 'replace edges with delete and insert';
    END IF;
    IF TG_OP = 'DELETE' THEN
        PERFORM planning_lock(OLD.plan_id);
        RETURN OLD;
    END IF;
    PERFORM planning_lock(NEW.plan_id);
    IF NEW.parent_id = NEW.child_id OR EXISTS (
        WITH RECURSIVE reachable(id) AS (
            SELECT NEW.child_id UNION
            SELECT e.child_id FROM planning_edges e JOIN reachable r ON e.parent_id = r.id
            WHERE e.plan_id = NEW.plan_id
        ) SELECT 1 FROM reachable WHERE id = NEW.parent_id
    ) THEN
        RAISE EXCEPTION 'dependency cycle';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER planning_edge_guard BEFORE INSERT OR UPDATE OR DELETE ON planning_edges
FOR EACH ROW EXECUTE FUNCTION planning_edge_guard();
CREATE FUNCTION planning_edge_apply() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        -- Repair exact multiplicities and minimum depths after removing alternate paths.
        PERFORM planning_rebuild_paths(OLD.plan_id);
        RETURN OLD;
    END IF;
    INSERT INTO planning_paths
    SELECT NEW.plan_id, a.ancestor_id, d.descendant_id,
           a.min_depth + d.min_depth + 1, a.path_count * d.path_count
    FROM planning_paths a CROSS JOIN planning_paths d
    WHERE a.plan_id = NEW.plan_id AND d.plan_id = NEW.plan_id
      AND a.descendant_id = NEW.parent_id AND d.ancestor_id = NEW.child_id
    ON CONFLICT (plan_id, ancestor_id, descendant_id) DO UPDATE
    SET path_count = planning_paths.path_count + excluded.path_count,
        min_depth = least(planning_paths.min_depth, excluded.min_depth);
    RETURN NEW;
END $$;
CREATE TRIGGER planning_edge_apply AFTER INSERT OR DELETE ON planning_edges
FOR EACH ROW EXECUTE FUNCTION planning_edge_apply();
