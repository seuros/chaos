use super::PostgresRuntime;
use super::StateRuntime;
use crate::ReviewAttemptState;
use crate::ReviewAttemptTransitionData;
use crate::ReviewRun;
use crate::ReviewRunCreateParams;
use crate::ReviewerAttempt;
use crate::ReviewerAttemptCreateParams;
use crate::model::ReviewRunRow;
use crate::model::ReviewerAttemptRow;

impl StateRuntime {
    pub(crate) async fn create_review_run(
        &self,
        run: &ReviewRunCreateParams,
        attempts: &[ReviewerAttemptCreateParams],
    ) -> anyhow::Result<ReviewRun> {
        let now = jiff::Timestamp::now().as_second();
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            r#"
INSERT INTO review_runs (
    id, review_run_subject, owner_process_id, created_at, updated_at
)
VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind(&run.id)
        .bind(&run.review_run_subject)
        .bind(&run.owner_process_id)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        for attempt in attempts {
            sqlx::query(
                r#"
INSERT INTO reviewer_attempts (
    id,
    run_id,
    ordinal,
    state,
    provider_id,
    model,
    account_subject,
    model_family_subject,
    reviewer_attempt_subject,
    idempotency_key,
    prompt,
    mcp_server,
    mcp_tool,
    created_at,
    updated_at
) VALUES (?, ?, ?, 'selection', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(&attempt.id)
            .bind(&run.id)
            .bind(attempt.ordinal)
            .bind(&attempt.provider_id)
            .bind(&attempt.model)
            .bind(&attempt.account_subject)
            .bind(&attempt.model_family_subject)
            .bind(&attempt.reviewer_attempt_subject)
            .bind(&attempt.idempotency_key)
            .bind(&attempt.prompt)
            .bind(&attempt.mcp_server)
            .bind(&attempt.mcp_tool)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        Ok(ReviewRun {
            id: run.id.clone(),
            review_run_subject: run.review_run_subject.clone(),
            owner_process_id: run.owner_process_id.clone(),
            created_at: jiff::Timestamp::from_second(now)?,
            updated_at: jiff::Timestamp::from_second(now)?,
        })
    }

    pub(crate) async fn get_review_run(&self, run_id: &str) -> anyhow::Result<Option<ReviewRun>> {
        sqlx::query_as::<_, ReviewRunRow>(
            r#"
SELECT id, review_run_subject, owner_process_id, created_at, updated_at
FROM review_runs
WHERE id = ?
            "#,
        )
        .bind(run_id)
        .fetch_optional(self.pool())
        .await?
        .map(TryInto::try_into)
        .transpose()
    }

    pub(crate) async fn list_reviewer_attempts(
        &self,
        run_id: &str,
    ) -> anyhow::Result<Vec<ReviewerAttempt>> {
        let rows = sqlx::query_as::<_, ReviewerAttemptRow>(
            r#"
SELECT
    id, run_id, ordinal, state, provider_id, model, account_subject,
    model_family_subject, reviewer_attempt_subject, idempotency_key,
    prompt, mcp_server, mcp_tool, process_id, raw_output, submission_json,
    failure, created_at, updated_at, completed_at
FROM reviewer_attempts
WHERE run_id = ?
ORDER BY ordinal ASC
            "#,
        )
        .bind(run_id)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub(crate) async fn get_reviewer_attempt(
        &self,
        attempt_id: &str,
    ) -> anyhow::Result<Option<ReviewerAttempt>> {
        sqlx::query_as::<_, ReviewerAttemptRow>(
            r#"
SELECT
    id, run_id, ordinal, state, provider_id, model, account_subject,
    model_family_subject, reviewer_attempt_subject, idempotency_key,
    prompt, mcp_server, mcp_tool, process_id, raw_output, submission_json,
    failure, created_at, updated_at, completed_at
FROM reviewer_attempts
WHERE id = ?
            "#,
        )
        .bind(attempt_id)
        .fetch_optional(self.pool())
        .await?
        .map(TryInto::try_into)
        .transpose()
    }

    pub(crate) async fn transition_reviewer_attempt(
        &self,
        attempt_id: &str,
        expected: ReviewAttemptState,
        target: ReviewAttemptState,
        data: &ReviewAttemptTransitionData,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let submission_json = data
            .submission
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let mut tx = self.pool().begin().await?;
        let result = sqlx::query(
            r#"
UPDATE reviewer_attempts
SET
    state = ?,
    process_id = COALESCE(?, process_id),
    raw_output = COALESCE(?, raw_output),
    submission_json = COALESCE(?, submission_json),
    failure = COALESCE(?, failure),
    updated_at = ?,
    completed_at = CASE WHEN ? THEN ? ELSE completed_at END
WHERE id = ? AND state = ?
            "#,
        )
        .bind(target.as_str())
        .bind(data.process_id.as_deref())
        .bind(data.raw_output.as_deref())
        .bind(submission_json.as_deref())
        .bind(data.failure.as_deref())
        .bind(now)
        .bind(target.is_terminal())
        .bind(now)
        .bind(attempt_id)
        .bind(expected.as_str())
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            r#"
UPDATE review_runs
SET updated_at = ?
WHERE id = (SELECT run_id FROM reviewer_attempts WHERE id = ?)
            "#,
        )
        .bind(now)
        .bind(attempt_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}

impl PostgresRuntime {
    pub(crate) async fn create_review_run(
        &self,
        run: &ReviewRunCreateParams,
        attempts: &[ReviewerAttemptCreateParams],
    ) -> anyhow::Result<ReviewRun> {
        let now = jiff::Timestamp::now().as_second();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
INSERT INTO review_runs (
    id, review_run_subject, owner_process_id, created_at, updated_at
)
VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(&run.id)
        .bind(&run.review_run_subject)
        .bind(&run.owner_process_id)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        for attempt in attempts {
            sqlx::query(
                r#"
INSERT INTO reviewer_attempts (
    id,
    run_id,
    ordinal,
    state,
    provider_id,
    model,
    account_subject,
    model_family_subject,
    reviewer_attempt_subject,
    idempotency_key,
    prompt,
    mcp_server,
    mcp_tool,
    created_at,
    updated_at
) VALUES ($1, $2, $3, 'selection', $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                "#,
            )
            .bind(&attempt.id)
            .bind(&run.id)
            .bind(attempt.ordinal)
            .bind(&attempt.provider_id)
            .bind(&attempt.model)
            .bind(&attempt.account_subject)
            .bind(&attempt.model_family_subject)
            .bind(&attempt.reviewer_attempt_subject)
            .bind(&attempt.idempotency_key)
            .bind(&attempt.prompt)
            .bind(&attempt.mcp_server)
            .bind(&attempt.mcp_tool)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        Ok(ReviewRun {
            id: run.id.clone(),
            review_run_subject: run.review_run_subject.clone(),
            owner_process_id: run.owner_process_id.clone(),
            created_at: jiff::Timestamp::from_second(now)?,
            updated_at: jiff::Timestamp::from_second(now)?,
        })
    }

    pub(crate) async fn get_review_run(&self, run_id: &str) -> anyhow::Result<Option<ReviewRun>> {
        sqlx::query_as::<_, ReviewRunRow>(
            r#"
SELECT id, review_run_subject, owner_process_id, created_at, updated_at
FROM review_runs
WHERE id = $1
            "#,
        )
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await?
        .map(TryInto::try_into)
        .transpose()
    }

    pub(crate) async fn list_reviewer_attempts(
        &self,
        run_id: &str,
    ) -> anyhow::Result<Vec<ReviewerAttempt>> {
        let rows = sqlx::query_as::<_, ReviewerAttemptRow>(
            r#"
SELECT
    id, run_id, ordinal, state, provider_id, model, account_subject,
    model_family_subject, reviewer_attempt_subject, idempotency_key,
    prompt, mcp_server, mcp_tool, process_id, raw_output,
    submission_json::text AS submission_json,
    failure, created_at, updated_at, completed_at
FROM reviewer_attempts
WHERE run_id = $1
ORDER BY ordinal ASC
            "#,
        )
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    pub(crate) async fn get_reviewer_attempt(
        &self,
        attempt_id: &str,
    ) -> anyhow::Result<Option<ReviewerAttempt>> {
        sqlx::query_as::<_, ReviewerAttemptRow>(
            r#"
SELECT
    id, run_id, ordinal, state, provider_id, model, account_subject,
    model_family_subject, reviewer_attempt_subject, idempotency_key,
    prompt, mcp_server, mcp_tool, process_id, raw_output,
    submission_json::text AS submission_json,
    failure, created_at, updated_at, completed_at
FROM reviewer_attempts
WHERE id = $1
            "#,
        )
        .bind(attempt_id)
        .fetch_optional(&self.pool)
        .await?
        .map(TryInto::try_into)
        .transpose()
    }

    pub(crate) async fn transition_reviewer_attempt(
        &self,
        attempt_id: &str,
        expected: ReviewAttemptState,
        target: ReviewAttemptState,
        data: &ReviewAttemptTransitionData,
    ) -> anyhow::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let submission_json = data
            .submission
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
UPDATE reviewer_attempts
SET
    state = $1,
    process_id = COALESCE($2, process_id),
    raw_output = COALESCE($3, raw_output),
    submission_json = COALESCE($4::jsonb, submission_json),
    failure = COALESCE($5, failure),
    updated_at = $6,
    completed_at = CASE WHEN $7 THEN $8 ELSE completed_at END
WHERE id = $9 AND state = $10
            "#,
        )
        .bind(target.as_str())
        .bind(data.process_id.as_deref())
        .bind(data.raw_output.as_deref())
        .bind(submission_json.as_deref())
        .bind(data.failure.as_deref())
        .bind(now)
        .bind(target.is_terminal())
        .bind(now)
        .bind(attempt_id)
        .bind(expected.as_str())
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            r#"
UPDATE review_runs
SET updated_at = $1
WHERE id = (SELECT run_id FROM reviewer_attempts WHERE id = $2)
            "#,
        )
        .bind(now)
        .bind(attempt_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}
