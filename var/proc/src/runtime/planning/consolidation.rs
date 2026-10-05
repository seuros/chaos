use super::*;

const JOB: &str =
    "SELECT id,plan_id,task_id,actor,installation_id,origin_call_id,request,input,state,
    execution_id,error,created_at FROM planning_consolidations WHERE id=$1";

macro_rules! consolidation_backend {
    ($runtime:ty) => {
        impl $runtime {
            async fn planning_enqueue_consolidation(
                &self, actor: &PlanningActor, request: &ConsolidationRequest,
                attached: Option<&str>, call_id: &str,
            ) -> anyhow::Result<ConsolidationJob> {
                let mut tx = self.pool().begin().await?;
                Self::planning_lock_attachment(&mut tx, &actor.session, attached).await?;
                let plan_id = Self::planning_lock_plan(&mut tx, &request.plan, attached).await?;
                let saved: Option<String> = sqlx::query_scalar(
                    "SELECT id FROM planning_consolidations WHERE actor=$1 AND request_id=$2"
                ).bind(&actor.session).bind(&request.request_id).fetch_optional(&mut *tx).await?;
                if let Some(id) = saved {
                    let job: ConsolidationJob = sqlx::query_as(JOB).bind(id).fetch_one(&mut *tx).await?;
                    anyhow::ensure!(job.request == *request, "request_id reused with different arguments");
                    tx.commit().await?;
                    return Ok(job);
                }
                let plan: Plan = sqlx::query_as(PLAN).bind(&plan_id).fetch_one(&mut *tx).await?;
                anyhow::ensure!(plan.status == PlanStatus::Active, "plan is terminal; reopen before editing");
                let task = match &request.task {
                    Some(task) => Some(sqlx::query_as::<_, Task>(TASK)
                        .bind(&plan_id).bind(task).fetch_one(&mut *tx).await?),
                    None => None,
                };
                let task_id = task.as_ref().map(|t| t.id.as_str());
                let row = Self::planning_content_row(&mut tx, &plan_id, task_id).await?;
                let clarifications = Self::planning_clarification_rows(
                    &mut tx, &plan_id, task_id, row.incorporated_seq, false,
                ).await?;
                let through_seq = clarifications.last().ok_or_else(|| anyhow::anyhow!("no pending clarifications"))?.seq;
                let input = ConsolidationInput {
                    title: task.as_ref().map_or(plan.title.as_str(), |t| t.title.as_str()).into(),
                    body: row.body, body_revision: row.body_revision, through_seq, clarifications,
                };
                let id = Uuid::now_v7().to_string();
                sqlx::query(
                    "INSERT INTO planning_consolidations(id,plan_id,task_id,actor,installation_id,request_id,request,input,origin_call_id)
                     VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)"
                ).bind(&id).bind(&plan_id).bind(task_id).bind(&actor.session).bind(&actor.installation)
                    .bind(&request.request_id).bind(Json(request)).bind(Json(&input)).bind(call_id).execute(&mut *tx).await?;
                let job = sqlx::query_as(JOB).bind(id).fetch_one(&mut *tx).await?;
                tx.commit().await?;
                Ok(job)
            }

            async fn planning_consolidation(&self, id: &str) -> anyhow::Result<ConsolidationJob> {
                Ok(sqlx::query_as(JOB).bind(id).fetch_one(self.pool()).await?)
            }

            async fn planning_pending_consolidations(&self, actor: &str, after: &str) -> anyhow::Result<Vec<ConsolidationJob>> {
                Ok(sqlx::query_as(
                    "SELECT id,plan_id,task_id,actor,installation_id,origin_call_id,request,input,state,execution_id,error,created_at
                     FROM planning_consolidations WHERE actor=$1 AND state IN ('queued','running')
                     AND id>$2 ORDER BY id LIMIT 50"
                ).bind(actor).bind(after).fetch_all(self.pool()).await?)
            }

            async fn planning_claim_consolidation(
                &self, id: &str, actor: &str, previous_execution: Option<&str>,
            ) -> anyhow::Result<Option<String>> {
                let execution = Uuid::now_v7().to_string();
                Ok(sqlx::query_scalar(
                    "UPDATE planning_consolidations SET state='running',execution_id=$1
                     WHERE id=$2 AND actor=$3 AND (
                       (state='queued' AND $4 IS NULL) OR (state='running' AND execution_id=$4)
                     ) RETURNING execution_id"
                ).bind(execution).bind(id).bind(actor).bind(previous_execution)
                    .fetch_optional(self.pool()).await?)
            }

            async fn planning_cancel_consolidation(&self, id: &str) -> anyhow::Result<ConsolidationJob> {
                sqlx::query("UPDATE planning_consolidations SET state='cancelled' WHERE id=$1 AND state IN ('queued','running')")
                    .bind(id).execute(self.pool()).await?;
                self.planning_consolidation(id).await
            }

            async fn planning_fail_consolidation(&self, id: &str, execution: &str, error: &str) -> anyhow::Result<ConsolidationJob> {
                sqlx::query("UPDATE planning_consolidations SET state='failed',error=$1 WHERE id=$2 AND execution_id=$3 AND state='running'")
                    .bind(error).bind(id).bind(execution).execute(self.pool()).await?;
                self.planning_consolidation(id).await
            }

            async fn planning_publish_consolidation(&self, id: &str, execution: &str, body: &str) -> anyhow::Result<ConsolidationJob> {
                let mut tx = self.pool().begin().await?;
                let (plan_id, active): (String, String) = sqlx::query_as(
                    "UPDATE planning_plans SET revision=revision
                     WHERE id=(SELECT plan_id FROM planning_consolidations WHERE id=$1)
                     RETURNING id,status"
                ).bind(id).fetch_one(&mut *tx).await?;
                let claimed: Option<String> = sqlx::query_scalar(
                    "UPDATE planning_consolidations SET state=state WHERE id=$1 AND state='running' AND execution_id=$2 RETURNING id"
                ).bind(id).bind(execution).fetch_optional(&mut *tx).await?;
                let job: ConsolidationJob = sqlx::query_as(JOB).bind(id).fetch_one(&mut *tx).await?;
                if claimed.is_none() {
                    tx.commit().await?;
                    return Ok(job);
                }
                let updated = if active != "active" {
                    false
                } else {
                    let query = if job.task_id.is_some() {
                        "UPDATE planning_tasks SET body=$1,body_revision=body_revision+1,incorporated_seq=$2,revision=revision+1 WHERE id=$3 AND body_revision=$4 AND title=$5"
                    } else {
                        "UPDATE planning_plans SET body=$1,body_revision=body_revision+1,incorporated_seq=$2,revision=revision+1 WHERE id=$3 AND body_revision=$4 AND title=$5"
                    };
                    sqlx::query(query).bind(body).bind(job.input.through_seq)
                        .bind(job.task_id.as_deref().unwrap_or(&plan_id)).bind(job.input.body_revision)
                        .bind(&job.input.title)
                        .execute(&mut *tx).await?.rows_affected() == 1
                };
                if updated {
                    let event = PlanChange::Note {
                        task: job.task_id.clone(), note: format!("Body consolidated by job {}", job.id),
                    };
                    Self::planning_record_event(&mut tx, &plan_id, job.task_id.as_deref(),
                        &job.actor, &job.installation_id, &event).await?;
                }
                let state = if updated { ConsolidationState::Completed } else { ConsolidationState::Stale };
                sqlx::query("UPDATE planning_consolidations SET state=$1,result_body=$2 WHERE id=$3")
                    .bind(state.as_ref()).bind(body).bind(id).execute(&mut *tx).await?;
                let result = sqlx::query_as(JOB).bind(id).fetch_one(&mut *tx).await?;
                tx.commit().await?;
                Ok(result)
            }
        }
    };
}

consolidation_backend!(StateRuntime);
consolidation_backend!(PostgresRuntime);

impl RuntimeDbHandle {
    chaos_dispatch::backend_dispatch! {
        pub async fn planning_enqueue_consolidation(&self, actor: &PlanningActor, request: &ConsolidationRequest, attached: Option<&str>, call_id: &str) -> anyhow::Result<ConsolidationJob>;
        pub async fn planning_consolidation(&self, id: &str) -> anyhow::Result<ConsolidationJob>;
        pub async fn planning_pending_consolidations(&self, actor: &str, after: &str) -> anyhow::Result<Vec<ConsolidationJob>>;
        pub async fn planning_claim_consolidation(&self, id: &str, actor: &str, previous_execution: Option<&str>) -> anyhow::Result<Option<String>>;
        pub async fn planning_cancel_consolidation(&self, id: &str) -> anyhow::Result<ConsolidationJob>;
        pub async fn planning_fail_consolidation(&self, id: &str, execution: &str, error: &str) -> anyhow::Result<ConsolidationJob>;
        pub async fn planning_publish_consolidation(&self, id: &str, execution: &str, body: &str) -> anyhow::Result<ConsolidationJob>;
    }
}

#[cfg(test)]
pub(super) mod tests;
