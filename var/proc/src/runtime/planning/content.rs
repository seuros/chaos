use super::*;

#[derive(sqlx::FromRow)]
pub(super) struct ContentRow {
    pub body: String,
    pub body_revision: i16,
    pub incorporated_seq: i64,
}

const CLARIFICATIONS: &str = "SELECT seq,text,actor,created_at FROM planning_clarifications
     WHERE plan_id=$1 AND (task_id=$2 OR (task_id IS NULL AND $2 IS NULL))
       AND seq>$3 ORDER BY seq";

macro_rules! content_backend {
    ($runtime:ty, $database:ty) => {
        impl $runtime {
            pub(super) async fn planning_content_row(
                tx: &mut sqlx::Transaction<'_, $database>,
                plan: &str,
                task: Option<&str>,
            ) -> anyhow::Result<ContentRow> {
                let (query, id) = match task {
                    Some(id) => ("SELECT body,body_revision,incorporated_seq FROM planning_tasks WHERE id=$1", id),
                    None => ("SELECT body,body_revision,incorporated_seq FROM planning_plans WHERE id=$1", plan),
                };
                Ok(sqlx::query_as(query).bind(id).fetch_one(&mut **tx).await?)
            }

            pub(super) async fn planning_clarification_rows(
                tx: &mut sqlx::Transaction<'_, $database>,
                plan: &str,
                task: Option<&str>,
                after: i64,
                paged: bool,
            ) -> anyhow::Result<Vec<Clarification>> {
                let mut query = sqlx::QueryBuilder::<$database>::new(CLARIFICATIONS);
                if paged {
                    query.push(" LIMIT 51");
                }
                Ok(query.build_query_as()
                    .bind(plan).bind(task).bind(after)
                    .fetch_all(&mut **tx).await?)
            }

            pub(super) async fn planning_read_content(
                tx: &mut sqlx::Transaction<'_, $database>,
                plan: &str,
                task: Option<&str>,
            ) -> anyhow::Result<PlanningContent> {
                let row = Self::planning_content_row(tx, plan, task).await?;
                let entries = Self::planning_clarification_rows(
                    tx, plan, task, row.incorporated_seq, true,
                ).await?;
                Ok(PlanningContent {
                    body: row.body,
                    body_revision: row.body_revision,
                    incorporated_seq: row.incorporated_seq,
                    clarifications: ClarificationPage::new(entries),
                })
            }

            async fn planning_detail(&self, reference: &str) -> anyhow::Result<PlanDetail> {
                let mut tx = self.planning_read_transaction().await?;
                let plan: Plan = sqlx::query_as(PLAN)
                    .bind(reference)
                    .fetch_one(&mut *tx)
                    .await?;
                let content = Self::planning_read_content(&mut tx, &plan.id, None).await?;
                tx.commit().await?;
                Ok(PlanDetail { plan, content })
            }

            async fn planning_clarifications(
                &self,
                plan: &str,
                task: Option<&str>,
                after: i64,
            ) -> anyhow::Result<ClarificationPage> {
                let mut tx = self.planning_read_transaction().await?;
                let plan: Plan = sqlx::query_as(PLAN).bind(plan).fetch_one(&mut *tx).await?;
                let task = match task {
                    Some(reference) => Some(
                        sqlx::query_as::<_, Task>(TASK)
                            .bind(&plan.id)
                            .bind(reference)
                            .fetch_one(&mut *tx)
                            .await?
                            .id,
                    ),
                    None => None,
                };
                let entries = Self::planning_clarification_rows(
                    &mut tx, &plan.id, task.as_deref(), after, true,
                ).await?;
                tx.commit().await?;
                Ok(ClarificationPage::new(entries))
            }
        }
    };
}

content_backend!(StateRuntime, sqlx::Sqlite);
content_backend!(PostgresRuntime, sqlx::Postgres);

impl RuntimeDbHandle {
    chaos_dispatch::backend_dispatch! {
        pub async fn planning_detail(&self, plan: &str) -> anyhow::Result<PlanDetail>;
        pub async fn planning_clarifications(&self, plan: &str, task: Option<&str>, after: i64) -> anyhow::Result<ClarificationPage>;
    }
}
