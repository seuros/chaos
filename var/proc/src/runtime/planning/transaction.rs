use super::*;

macro_rules! transaction_backend {
    ($runtime:ty, $database:ty, $pg:expr) => {
        impl $runtime {
            pub(super) async fn planning_read_transaction(
                &self,
            ) -> anyhow::Result<sqlx::Transaction<'_, $database>> {
                let mut tx = self.pool().begin().await?;
                if $pg {
                    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
                        .execute(&mut *tx)
                        .await?;
                }
                Ok(tx)
            }

            pub(super) async fn planning_lock_attachment(
                tx: &mut sqlx::Transaction<'_, $database>,
                session: &str,
                expected: Option<&str>,
            ) -> anyhow::Result<()> {
                if let Some(expected) = expected {
                    let current: Option<String> = sqlx::query_scalar(
                        "UPDATE planning_attachments SET plan_id=plan_id
                         WHERE session_id=$1 RETURNING plan_id",
                    )
                    .bind(session)
                    .fetch_optional(&mut **tx)
                    .await?;
                    anyhow::ensure!(
                        current.as_deref() == Some(expected),
                        "attachment_conflict: refresh before continuing"
                    );
                }
                Ok(())
            }

            pub(super) async fn planning_lock_plan(
                tx: &mut sqlx::Transaction<'_, $database>,
                reference: &str,
                attached: Option<&str>,
            ) -> anyhow::Result<String> {
                let id: String = sqlx::query_scalar(
                    "UPDATE planning_plans SET revision=revision WHERE id=$1 OR id IN (
                        SELECT p.id FROM planning_plans p JOIN planning_workspaces w ON w.id=p.workspace_id
                        WHERE w.code || '/' || p.code=$1) RETURNING id",
                )
                .bind(reference)
                .fetch_one(&mut **tx)
                .await?;
                if let Some(attached) = attached {
                    anyhow::ensure!(
                        id == attached,
                        "attachment_conflict: mutation targets another plan"
                    );
                }
                Ok(id)
            }

            pub(super) async fn planning_record_event(
                tx: &mut sqlx::Transaction<'_, $database>,
                plan: &str,
                task: Option<&str>,
                actor: &str,
                installation: &str,
                event: &PlanChange,
            ) -> anyhow::Result<()> {
                let seq: i64 = sqlx::query_scalar(
                    "UPDATE planning_plans SET next_event=next_event+1 WHERE id=$1 RETURNING next_event-1",
                )
                .bind(plan)
                .fetch_one(&mut **tx)
                .await?;
                if let PlanChange::Clarify { text, .. } = event {
                    sqlx::query(
                        "INSERT INTO planning_clarifications(plan_id,task_id,seq,text,actor) VALUES($1,$2,$3,$4,$5)",
                    )
                    .bind(plan).bind(task).bind(seq).bind(text).bind(actor)
                    .execute(&mut **tx).await?;
                }
                sqlx::query(
                    "INSERT INTO planning_history(id,plan_id,seq,task_id,actor,installation_id,event) VALUES($1,$2,$3,$4,$5,$6,$7)",
                )
                .bind(Uuid::now_v7().to_string()).bind(plan).bind(seq).bind(task)
                .bind(actor).bind(installation).bind(Json(event))
                .execute(&mut **tx).await?;
                Ok(())
            }
        }
    };
}

transaction_backend!(StateRuntime, sqlx::Sqlite, false);
transaction_backend!(PostgresRuntime, sqlx::Postgres, true);
