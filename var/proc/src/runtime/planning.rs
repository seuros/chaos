use super::*;
use crate::planning::*;
use sqlx::types::Json;

const PLAN: &str = "SELECT p.id,p.workspace_id,w.code || '/' || p.code AS reference,
    p.title,p.status,p.revision FROM planning_plans p
    JOIN planning_workspaces w ON w.id=p.workspace_id
    WHERE p.id=$1 OR w.code || '/' || p.code=$1";
const TASK: &str = "SELECT t.id,'T' || CAST(t.number AS TEXT) AS reference,t.title,t.status,
    t.revision,t.parent_id,t.position FROM planning_tasks t
    JOIN planning_plans p ON p.id=t.plan_id JOIN planning_workspaces w ON w.id=p.workspace_id
    WHERE t.plan_id=$1 AND (t.id=$2 OR 'T' || CAST(t.number AS TEXT)=$2
    OR w.code || '/' || p.code || '/T' || CAST(t.number AS TEXT)=$2)";
const TASK_PAGE: &str = "
WITH RECURSIVE ranked AS (
    SELECT t.*,row_number() OVER(PARTITION BY parent_id ORDER BY position,number) AS ordinal
    FROM planning_tasks t WHERE plan_id=$1
), tree AS (
    SELECT id,CAST(0 AS BIGINT) AS depth,substr('00000000000000000000' || CAST(ordinal AS TEXT),
      length(CAST(ordinal AS TEXT))+1,20) AS path FROM ranked WHERE parent_id IS NULL
    UNION ALL
    SELECT r.id,h.depth+1,h.path || '/' || substr('00000000000000000000' || CAST(r.ordinal AS TEXT),
      length(CAST(r.ordinal AS TEXT))+1,20)
    FROM ranked r JOIN tree h ON h.id=r.parent_id
)
SELECT r.id,'T' || CAST(r.number AS TEXT) AS reference,r.title,r.status,r.revision,
    r.parent_id,r.position,tree.depth FROM ranked r JOIN tree ON tree.id=r.id
    ORDER BY tree.path LIMIT 51 OFFSET $2";

#[derive(sqlx::FromRow)]
struct SavedRequest {
    payload: Json<PlanMutation>,
    result: Option<Json<MutationResult>>,
}

macro_rules! planning_backend {
    ($runtime:ty, $dag:expr) => {
        impl $runtime {
            async fn planning_create_workspace(&self, name: &str) -> anyhow::Result<Workspace> {
                let mut tx = self.pool().begin().await?;
                let number: i64 = sqlx::query_scalar("UPDATE planning_workspace_sequence SET next_workspace=next_workspace+1 WHERE id=1 AND next_workspace<16777216 RETURNING next_workspace-1")
                    .fetch_optional(&mut *tx).await?.ok_or_else(|| anyhow::anyhow!("workspace reference namespace exhausted"))?;
                let workspace = sqlx::query_as("INSERT INTO planning_workspaces(id,code,name) VALUES($1,$2,$3) RETURNING id,code,name,revision")
                    .bind(Uuid::now_v7().to_string()).bind(format!("{number:06X}")).bind(name)
                    .fetch_one(&mut *tx).await?;
                tx.commit().await?;
                Ok(workspace)
            }

            async fn planning_rename_workspace(&self, workspace: &str, expected_revision: i16, name: &str) -> anyhow::Result<Workspace> {
                let mut tx = self.pool().begin().await?;
                let old: Workspace = sqlx::query_as("UPDATE planning_workspaces SET revision=revision WHERE id=$1 OR code=$1 RETURNING id,code,name,revision")
                    .bind(workspace).fetch_one(&mut *tx).await?;
                check_revision(&old.id,Some(expected_revision),old.revision)?;
                let updated = sqlx::query_as("UPDATE planning_workspaces SET name=$1,revision=revision+1 WHERE id=$2 RETURNING id,code,name,revision")
                    .bind(name).bind(&old.id).fetch_one(&mut *tx).await?;
                tx.commit().await?;
                Ok(updated)
            }

            async fn planning_register_project(&self, workspace: &str, name: &str) -> anyhow::Result<Project> {
                Ok(sqlx::query_as("INSERT INTO planning_projects(id,workspace_id,name)
                    SELECT $1,id,$2 FROM planning_workspaces WHERE id=$3 OR code=$3
                    RETURNING id,workspace_id,name,revision")
                    .bind(Uuid::now_v7().to_string()).bind(name).bind(workspace)
                    .fetch_one(self.pool()).await?)
            }

            async fn planning_bind(&self, installation: &str, project: &str, path: &Path) -> anyhow::Result<Checkout> {
                let path = std::fs::canonicalize(path)?;
                let mut tx = self.pool().begin().await?;
                sqlx::query("INSERT INTO planning_checkouts(installation_id,path,project_id) VALUES($1,$2,$3) ON CONFLICT(installation_id,path) DO NOTHING")
                    .bind(installation).bind(path.to_string_lossy().as_ref()).bind(project).execute(&mut *tx).await?;
                let checkout: Checkout = sqlx::query_as("SELECT c.path,c.project_id,p.workspace_id FROM planning_checkouts c JOIN planning_projects p ON p.id=c.project_id WHERE c.installation_id=$1 AND c.path=$2")
                    .bind(installation).bind(path.to_string_lossy().as_ref()).fetch_one(&mut *tx).await?;
                anyhow::ensure!(checkout.project_id == project, "checkout is already bound to a different project");
                tx.commit().await?;
                Ok(checkout)
            }

            async fn planning_workspaces(&self, offset: i64) -> anyhow::Result<Page<Workspace>> {
                let rows = sqlx::query_as("SELECT id,code,name,revision FROM planning_workspaces ORDER BY code LIMIT 51 OFFSET $1")
                    .bind(offset).fetch_all(self.pool()).await?;
                Ok(Page::new(rows,offset))
            }

            async fn planning_projects(&self, workspace: &str, offset: i64) -> anyhow::Result<Page<Project>> {
                let rows = sqlx::query_as("SELECT p.id,p.workspace_id,p.name,p.revision FROM planning_projects p JOIN planning_workspaces w ON w.id=p.workspace_id WHERE w.id=$1 OR w.code=$1 ORDER BY p.id LIMIT 51 OFFSET $2")
                    .bind(workspace).bind(offset).fetch_all(self.pool()).await?;
                Ok(Page::new(rows,offset))
            }

            async fn planning_checkout(&self, installation: &str, cwd: &Path) -> anyhow::Result<Option<Checkout>> {
                let cwd = std::fs::canonicalize(cwd)?;
                let rows: Vec<Checkout> = sqlx::query_as("SELECT c.path,c.project_id,p.workspace_id FROM planning_checkouts c JOIN planning_projects p ON p.id=c.project_id WHERE c.installation_id=$1 ORDER BY length(c.path) DESC")
                    .bind(installation).fetch_all(self.pool()).await?;
                Ok(rows.into_iter().find(|checkout|cwd.starts_with(&checkout.path)))
            }

            async fn planning_resolve_plan(&self, reference: &str) -> anyhow::Result<String> {
                Ok(sqlx::query_scalar("SELECT p.id FROM planning_plans p JOIN planning_workspaces w ON w.id=p.workspace_id WHERE p.id=$1 OR w.code || '/' || p.code=$1")
                    .bind(reference).fetch_one(self.pool()).await?)
            }

            async fn planning_attachment(&self, session: &str) -> anyhow::Result<Option<String>> {
                Ok(sqlx::query_scalar("SELECT plan_id FROM planning_attachments WHERE session_id=$1")
                    .bind(session).fetch_optional(self.pool()).await?)
            }

            async fn planning_attach(&self, session: &str, plan: Option<&str>) -> anyhow::Result<()> {
                if let Some(plan) = plan {
                    let id = self.planning_resolve_plan(plan).await?;
                    sqlx::query("INSERT INTO planning_attachments(session_id,plan_id) VALUES($1,$2) ON CONFLICT(session_id) DO UPDATE SET plan_id=excluded.plan_id")
                        .bind(session).bind(id).execute(self.pool()).await?;
                } else {
                    sqlx::query("DELETE FROM planning_attachments WHERE session_id=$1").bind(session).execute(self.pool()).await?;
                }
                Ok(())
            }

            async fn planning_detach(&self, session: &str, expected_plan: &str) -> anyhow::Result<()> {
                let result = sqlx::query("DELETE FROM planning_attachments WHERE session_id=$1 AND plan_id=$2")
                    .bind(session).bind(expected_plan).execute(self.pool()).await?;
                anyhow::ensure!(result.rows_affected()==1,"attachment_conflict: refresh before continuing");
                Ok(())
            }

            async fn planning_list(&self, workspace: &str, offset: i64) -> anyhow::Result<Page<Plan>> {
                let rows = sqlx::query_as("SELECT p.id,p.workspace_id,w.code || '/' || p.code AS reference,p.title,p.status,p.revision FROM planning_plans p JOIN planning_workspaces w ON w.id=p.workspace_id WHERE w.id=$1 OR w.code=$1 ORDER BY p.code LIMIT 51 OFFSET $2")
                    .bind(workspace).bind(offset).fetch_all(self.pool()).await?;
                Ok(Page::new(rows,offset))
            }

            async fn planning_read(&self, reference: &str, offset: i64) -> anyhow::Result<PlanSnapshot> {
                let mut tx = self.pool().begin().await?;
                if $dag {
                    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                }
                let plan: Plan = sqlx::query_as(PLAN).bind(reference).fetch_one(&mut *tx).await?;
                let rows = sqlx::query_as(TASK_PAGE).bind(&plan.id).bind(offset).fetch_all(&mut *tx).await?;
                let page = Page::new(rows,offset);
                tx.commit().await?;
                Ok(PlanSnapshot {
                    capabilities: if $dag {PlanningCapabilities::POSTGRES} else {PlanningCapabilities::SQLITE},
                    plan,tasks:page.items,next_offset:page.next_offset,
                })
            }

            async fn planning_task(&self, plan: &str, task: &str, offset: i64) -> anyhow::Result<TaskDetail> {
                let mut tx = self.pool().begin().await?;
                if $dag {
                    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                }
                let plan: Plan = sqlx::query_as(PLAN).bind(plan).fetch_one(&mut *tx).await?;
                let task: Task = sqlx::query_as(TASK).bind(&plan.id).bind(task).fetch_one(&mut *tx).await?;
                let projects = sqlx::query_as("SELECT p.id,p.workspace_id,p.name,p.revision FROM planning_projects p
                    JOIN planning_targets t ON t.project_id=p.id WHERE t.task_id=$1 ORDER BY p.id LIMIT 51 OFFSET $2")
                    .bind(&task.id).bind(offset).fetch_all(&mut *tx).await?;
                tx.commit().await?;
                Ok(TaskDetail {task,projects:Page::new(projects,offset)})
            }

            async fn planning_history(&self, plan: &str, after: i64) -> anyhow::Result<HistoryPage> {
                let id = self.planning_resolve_plan(plan).await?;
                let mut entries: Vec<HistoryEntry> = sqlx::query_as("SELECT id,seq,task_id,actor,installation_id,event,created_at FROM planning_history WHERE plan_id=$1 AND seq>$2 ORDER BY seq LIMIT 51")
                    .bind(id).bind(after).fetch_all(self.pool()).await?;
                let next_after = if entries.len()>50 {Some(entries[49].seq)} else {None};
                entries.truncate(50);
                Ok(HistoryPage { entries,next_after })
            }

            async fn planning_mutate(&self, actor: &PlanningActor, request: &PlanMutation) -> anyhow::Result<MutationResult> {
                self.planning_commit(actor,request,None).await
            }

            async fn planning_mutate_attached(&self, actor: &PlanningActor, request: &PlanMutation, attached: &str) -> anyhow::Result<MutationResult> {
                self.planning_commit(actor,request,Some(attached)).await
            }

            async fn planning_commit(&self, actor: &PlanningActor, request: &PlanMutation, attached: Option<&str>) -> anyhow::Result<MutationResult> {
                let mut tx = self.pool().begin().await?;
                if let Some(attached) = attached {
                    let current: Option<String> = sqlx::query_scalar("UPDATE planning_attachments SET plan_id=plan_id WHERE session_id=$1 RETURNING plan_id")
                        .bind(&actor.session).fetch_optional(&mut *tx).await?;
                    anyhow::ensure!(current.as_deref()==Some(attached),"attachment_conflict: refresh before continuing");
                }
                sqlx::query("INSERT INTO planning_requests(actor,request_id,payload) VALUES($1,$2,$3) ON CONFLICT(actor,request_id) DO NOTHING")
                    .bind(&actor.session).bind(&request.request_id).bind(Json(request)).execute(&mut *tx).await?;
                let saved: SavedRequest = sqlx::query_as("SELECT payload,result FROM planning_requests WHERE actor=$1 AND request_id=$2")
                    .bind(&actor.session).bind(&request.request_id).fetch_one(&mut *tx).await?;
                anyhow::ensure!(saved.payload.0 == *request, "request_id reused with different arguments");
                if let Some(Json(result)) = saved.result {
                    tx.commit().await?;
                    return Ok(result);
                }

                let plan_id;
                let mut task_id = None;
                if let PlanChange::Create { workspace,title } = &request.change {
                    let (workspace,number): (String,i64) = sqlx::query_as("UPDATE planning_workspaces SET next_plan=next_plan+1 WHERE (id=$1 OR code=$1) AND next_plan<65536 RETURNING id,next_plan-1")
                        .bind(workspace).fetch_optional(&mut *tx).await?
                        .ok_or_else(||anyhow::anyhow!("workspace missing or plan reference namespace exhausted"))?;
                    plan_id=Uuid::now_v7().to_string();
                    sqlx::query("INSERT INTO planning_plans(id,workspace_id,code,title) VALUES($1,$2,$3,$4)")
                        .bind(&plan_id).bind(workspace).bind(format!("{number:04X}")).bind(title).execute(&mut *tx).await?;
                } else {
                    let reference=request.plan.as_deref().ok_or_else(||anyhow::anyhow!("plan is required"))?;
                    plan_id=sqlx::query_scalar("UPDATE planning_plans SET revision=revision WHERE id=$1 OR id IN (SELECT p.id FROM planning_plans p JOIN planning_workspaces w ON w.id=p.workspace_id WHERE w.code || '/' || p.code=$1) RETURNING id")
                        .bind(reference).fetch_one(&mut *tx).await?;
                    let plan: Plan=sqlx::query_as(PLAN).bind(&plan_id).fetch_one(&mut *tx).await?;
                    if let Some(attached)=attached {
                        anyhow::ensure!(plan.id==attached,"attachment_conflict: mutation targets another plan");
                    }
                    if request.change.structural() {check_revision(&plan.id,request.expected_revision,plan.revision)?;}
                    if !matches!(request.change,PlanChange::Reopen { .. }) {
                        anyhow::ensure!(plan.status==PlanStatus::Active,"plan is terminal; reopen before editing");
                    }
                    macro_rules! task {
                        ($reference:expr) => {
                            sqlx::query_as::<_,Task>(TASK).bind(&plan.id).bind($reference).fetch_one(&mut *tx).await?
                        }
                    }
                    match &request.change {
                        PlanChange::Edit { title } => {
                            sqlx::query("UPDATE planning_plans SET title=$1 WHERE id=$2").bind(title).bind(&plan.id).execute(&mut *tx).await?;
                        }
                        PlanChange::AddTask { title,parent,position } => {
                            let parent=match parent {Some(p)=>Some(task!(p).id),None=>None};
                            let number: i64=sqlx::query_scalar("UPDATE planning_plans SET next_task=next_task+1 WHERE id=$1 RETURNING next_task-1")
                                .bind(&plan.id).fetch_one(&mut *tx).await?;
                            let id=Uuid::now_v7().to_string();
                            sqlx::query("INSERT INTO planning_tasks(id,plan_id,number,title,parent_id,position) VALUES($1,$2,$3,$4,$5,$6)")
                                .bind(&id).bind(&plan.id).bind(number).bind(title).bind(parent).bind(position).execute(&mut *tx).await?;
                            task_id=Some(id);
                        }
                        PlanChange::EditTask { task,task_revision,title,projects } => {
                            let task=task!(task);
                            check_revision(&task.id,Some(*task_revision),task.revision)?;
                            sqlx::query("UPDATE planning_tasks SET title=$1,revision=revision+1 WHERE id=$2").bind(title).bind(&task.id).execute(&mut *tx).await?;
                            sqlx::query("DELETE FROM planning_targets WHERE task_id=$1").bind(&task.id).execute(&mut *tx).await?;
                            for project in projects {
                                sqlx::query("INSERT INTO planning_targets(workspace_id,plan_id,task_id,project_id) VALUES($1,$2,$3,$4)")
                                    .bind(&plan.workspace_id).bind(&plan.id).bind(&task.id).bind(project).execute(&mut *tx).await?;
                            }
                            task_id=Some(task.id);
                        }
                        PlanChange::MoveTask { task,task_revision,parent,position } => {
                            let task=task!(task);
                            check_revision(&task.id,Some(*task_revision),task.revision)?;
                            let parent=match parent {Some(p)=>Some(task!(p).id),None=>None};
                            sqlx::query("UPDATE planning_tasks SET parent_id=$1,position=$2,revision=revision+1 WHERE id=$3")
                                .bind(parent).bind(position).bind(&task.id).execute(&mut *tx).await?;
                            task_id=Some(task.id);
                        }
                        PlanChange::Transition { task,task_revision,event,reason } => {
                            anyhow::ensure!(!reason.trim().is_empty(),"a transition requires a reason");
                            let task=task!(task);
                            check_revision(&task.id,Some(*task_revision),task.revision)?;
                            let status=transition(task.status,*event)?;
                            sqlx::query("UPDATE planning_tasks SET status=$1,revision=revision+1 WHERE id=$2")
                                .bind(status.as_ref()).bind(&task.id).execute(&mut *tx).await?;
                            task_id=Some(task.id);
                        }
                        PlanChange::Note { task,.. } => {
                            if let Some(reference)=task {task_id=Some(task!(reference).id);}
                        }
                        PlanChange::Link { parent,child } | PlanChange::Unlink { parent,child } => {
                            anyhow::ensure!($dag,"unsupported_capability: dependencies require PostgreSQL");
                            let parent=task!(parent);
                            let child=task!(child);
                            if matches!(request.change,PlanChange::Link { .. }) {
                                sqlx::query("INSERT INTO planning_edges(plan_id,parent_id,child_id) VALUES($1,$2,$3)")
                                    .bind(&plan.id).bind(&parent.id).bind(&child.id).execute(&mut *tx).await?;
                            } else {
                                sqlx::query("DELETE FROM planning_edges WHERE plan_id=$1 AND parent_id=$2 AND child_id=$3")
                                    .bind(&plan.id).bind(&parent.id).bind(&child.id).execute(&mut *tx).await?;
                            }
                            sqlx::query("UPDATE planning_tasks SET revision=revision+1 WHERE id=$1 OR id=$2")
                                .bind(parent.id).bind(child.id).execute(&mut *tx).await?;
                        }
                        PlanChange::Complete { reason } | PlanChange::Cancel { reason } | PlanChange::Reopen { reason } => {
                            anyhow::ensure!(!reason.trim().is_empty(),"a transition requires a reason");
                            let event=match request.change {
                                PlanChange::Complete { .. } => PlanEvent::Complete,
                                PlanChange::Cancel { .. } => PlanEvent::Cancel,
                                _ => PlanEvent::Reopen,
                            };
                            if matches!(event,PlanEvent::Complete) {
                                let unfinished: i64=sqlx::query_scalar("SELECT count(*) FROM planning_tasks WHERE plan_id=$1 AND status NOT IN ('completed','cancelled')")
                                    .bind(&plan.id).fetch_one(&mut *tx).await?;
                                anyhow::ensure!(unfinished==0,"plan has unfinished tasks");
                            }
                            let status=plan_transition(plan.status,event)?;
                            sqlx::query("UPDATE planning_plans SET status=$1 WHERE id=$2").bind(status.as_ref()).bind(&plan.id).execute(&mut *tx).await?;
                        }
                        PlanChange::Create { .. } => unreachable!("creation is handled before existing-plan mutations"),
                    }
                    if request.change.structural() {
                        sqlx::query("UPDATE planning_plans SET revision=revision+1 WHERE id=$1").bind(&plan.id).execute(&mut *tx).await?;
                    }
                }
                let plan=sqlx::query_as(PLAN).bind(&plan_id).fetch_one(&mut *tx).await?;
                let task=match &task_id {
                    Some(task)=>Some(sqlx::query_as(TASK).bind(&plan_id).bind(task).fetch_one(&mut *tx).await?),
                    None=>None,
                };
                let result=MutationResult {plan,task};
                let seq: i64=sqlx::query_scalar("UPDATE planning_plans SET next_event=next_event+1 WHERE id=$1 RETURNING next_event-1")
                    .bind(&plan_id).fetch_one(&mut *tx).await?;
                sqlx::query("INSERT INTO planning_history(id,plan_id,seq,task_id,actor,installation_id,event) VALUES($1,$2,$3,$4,$5,$6,$7)")
                    .bind(Uuid::now_v7().to_string()).bind(&plan_id).bind(seq).bind(task_id)
                    .bind(&actor.session).bind(&actor.installation).bind(Json(&request.change)).execute(&mut *tx).await?;
                sqlx::query("UPDATE planning_requests SET result=$1 WHERE actor=$2 AND request_id=$3")
                    .bind(Json(&result)).bind(&actor.session).bind(&request.request_id).execute(&mut *tx).await?;
                tx.commit().await?;
                Ok(result)
            }
        }
    }
}

planning_backend!(StateRuntime, false);
planning_backend!(PostgresRuntime, true);

impl RuntimeDbHandle {
    pub fn planning_capabilities(&self) -> PlanningCapabilities {
        match self {
            Self::Postgres(_) => PlanningCapabilities::POSTGRES,
            Self::Sqlite(_) => PlanningCapabilities::SQLITE,
        }
    }

    chaos_dispatch::backend_dispatch! {
        pub async fn planning_create_workspace(&self, name: &str) -> anyhow::Result<Workspace>;
        pub async fn planning_rename_workspace(&self, workspace: &str, expected_revision: i16, name: &str) -> anyhow::Result<Workspace>;
        pub async fn planning_register_project(&self, workspace: &str, name: &str) -> anyhow::Result<Project>;
        pub async fn planning_bind(&self, installation: &str, project: &str, path: &Path) -> anyhow::Result<Checkout>;
        pub async fn planning_workspaces(&self, offset: i64) -> anyhow::Result<Page<Workspace>>;
        pub async fn planning_projects(&self, workspace: &str, offset: i64) -> anyhow::Result<Page<Project>>;
        pub async fn planning_checkout(&self, installation: &str, cwd: &Path) -> anyhow::Result<Option<Checkout>>;
        pub async fn planning_resolve_plan(&self, reference: &str) -> anyhow::Result<String>;
        pub async fn planning_attachment(&self, session: &str) -> anyhow::Result<Option<String>>;
        pub async fn planning_attach(&self, session: &str, plan: Option<&str>) -> anyhow::Result<()>;
        pub async fn planning_detach(&self, session: &str, expected_plan: &str) -> anyhow::Result<()>;
        pub async fn planning_list(&self, workspace: &str, offset: i64) -> anyhow::Result<Page<Plan>>;
        pub async fn planning_read(&self, plan: &str, offset: i64) -> anyhow::Result<PlanSnapshot>;
        pub async fn planning_task(&self, plan: &str, task: &str, offset: i64) -> anyhow::Result<TaskDetail>;
        pub async fn planning_history(&self, plan: &str, after: i64) -> anyhow::Result<HistoryPage>;
        pub async fn planning_mutate(&self, actor: &PlanningActor, request: &PlanMutation) -> anyhow::Result<MutationResult>;
        pub async fn planning_mutate_attached(&self, actor: &PlanningActor, request: &PlanMutation, attached: &str) -> anyhow::Result<MutationResult>;
    }

    pub async fn planning_graph(
        &self,
        plan: &str,
        task: Option<&str>,
        offset: i64,
    ) -> anyhow::Result<Page<Dependency>> {
        let Self::Postgres(db) = self else {
            anyhow::bail!("unsupported_capability: dependencies require PostgreSQL");
        };
        let id = db.planning_resolve_plan(plan).await?;
        let root = match task {
            Some(task) => Some(db.planning_task(&id, task, 0).await?.task.id),
            None => None,
        };
        let rows = sqlx::query_as(
            "SELECT parent_id,child_id FROM planning_edges
            WHERE plan_id=$1 AND ($2::text IS NULL OR parent_id IN (
                SELECT descendant_id FROM planning_paths WHERE plan_id=$1 AND ancestor_id=$2
            )) ORDER BY parent_id,child_id LIMIT 51 OFFSET $3",
        )
        .bind(id)
        .bind(root)
        .bind(offset)
        .fetch_all(db.pool())
        .await?;
        Ok(Page::new(rows, offset))
    }

    pub async fn planning_validate_graph(
        &self,
        plan: &str,
        offset: i64,
    ) -> anyhow::Result<Page<ClosureMismatch>> {
        let Self::Postgres(db) = self else {
            anyhow::bail!("unsupported_capability: dependencies require PostgreSQL");
        };
        let id = db.planning_resolve_plan(plan).await?;
        let rows = sqlx::query_as(
            "SELECT ancestor_id,descendant_id FROM planning_validate_paths($1)
            ORDER BY ancestor_id,descendant_id LIMIT 51 OFFSET $2",
        )
        .bind(id)
        .bind(offset)
        .fetch_all(db.pool())
        .await?;
        Ok(Page::new(rows, offset))
    }

    pub async fn planning_rebuild_graph(
        &self,
        plan: &str,
        expected_revision: i16,
    ) -> anyhow::Result<()> {
        let Self::Postgres(db) = self else {
            anyhow::bail!("unsupported_capability: dependencies require PostgreSQL");
        };
        let id = db.planning_resolve_plan(plan).await?;
        let mut tx = db.pool().begin().await?;
        sqlx::query("SELECT planning_lock($1)")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        let plan: Plan = sqlx::query_as(PLAN).bind(&id).fetch_one(&mut *tx).await?;
        check_revision(&id, Some(expected_revision), plan.revision)?;
        // Closure is derived data: repairing it does not change plan intent or revisions.
        sqlx::query("SELECT planning_rebuild_paths($1)")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod postgres_tests;
#[cfg(test)]
mod tests;
