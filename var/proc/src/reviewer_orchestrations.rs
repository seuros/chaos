use crate::ReviewAttemptState;
use crate::ReviewAttemptTransitionData;
use crate::ReviewRun;
use crate::ReviewRunCreateParams;
use crate::ReviewerAttempt;
use crate::ReviewerAttemptCreateParams;
use crate::RuntimeDbHandle;
use crate::model::reviewer_orchestration_machine::ReviewAttemptWorkflow;
use anyhow::Context;
use anyhow::bail;
use std::collections::HashSet;

#[derive(Clone)]
pub struct ReviewerOrchestrations {
    db: RuntimeDbHandle,
}

impl RuntimeDbHandle {
    pub fn reviewer_orchestrations(&self) -> ReviewerOrchestrations {
        ReviewerOrchestrations { db: self.clone() }
    }
}

impl ReviewerOrchestrations {
    /// Atomically persist a run and its immutable selected reviewer bindings.
    pub async fn create_run(
        &self,
        run: &ReviewRunCreateParams,
        attempts: &[ReviewerAttemptCreateParams],
    ) -> anyhow::Result<ReviewRun> {
        validate_selection(run, attempts)?;
        match &self.db {
            RuntimeDbHandle::Postgres(runtime) => runtime.create_review_run(run, attempts).await,
            RuntimeDbHandle::Sqlite(runtime) => runtime.create_review_run(run, attempts).await,
        }
    }

    pub async fn get_run(&self, run_id: &str) -> anyhow::Result<Option<ReviewRun>> {
        match &self.db {
            RuntimeDbHandle::Postgres(runtime) => runtime.get_review_run(run_id).await,
            RuntimeDbHandle::Sqlite(runtime) => runtime.get_review_run(run_id).await,
        }
    }

    pub async fn list_attempts(&self, run_id: &str) -> anyhow::Result<Vec<ReviewerAttempt>> {
        match &self.db {
            RuntimeDbHandle::Postgres(runtime) => runtime.list_reviewer_attempts(run_id).await,
            RuntimeDbHandle::Sqlite(runtime) => runtime.list_reviewer_attempts(run_id).await,
        }
    }

    pub async fn get_attempt(&self, attempt_id: &str) -> anyhow::Result<Option<ReviewerAttempt>> {
        match &self.db {
            RuntimeDbHandle::Postgres(runtime) => runtime.get_reviewer_attempt(attempt_id).await,
            RuntimeDbHandle::Sqlite(runtime) => runtime.get_reviewer_attempt(attempt_id).await,
        }
    }

    pub async fn get_attempt_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> anyhow::Result<Option<ReviewerAttempt>> {
        match &self.db {
            RuntimeDbHandle::Postgres(runtime) => {
                runtime
                    .get_reviewer_attempt_by_idempotency_key(idempotency_key)
                    .await
            }
            RuntimeDbHandle::Sqlite(runtime) => {
                runtime
                    .get_reviewer_attempt_by_idempotency_key(idempotency_key)
                    .await
            }
        }
    }

    /// Compare-and-swap one declared lifecycle transition.
    ///
    /// `false` means another owner already advanced the attempt. Callers must
    /// reload instead of replaying the side effect they just attempted.
    pub async fn transition_attempt(
        &self,
        attempt_id: &str,
        expected: ReviewAttemptState,
        target: ReviewAttemptState,
        data: &ReviewAttemptTransitionData,
    ) -> anyhow::Result<bool> {
        if !ReviewAttemptWorkflow::from_state(expected).permits(target) {
            bail!(
                "invalid reviewer attempt transition: {} -> {}",
                expected.as_str(),
                target.as_str()
            );
        }
        validate_transition_data(target, data)?;
        match &self.db {
            RuntimeDbHandle::Postgres(runtime) => {
                runtime
                    .transition_reviewer_attempt(attempt_id, expected, target, data)
                    .await
            }
            RuntimeDbHandle::Sqlite(runtime) => {
                runtime
                    .transition_reviewer_attempt(attempt_id, expected, target, data)
                    .await
            }
        }
    }
}

fn validate_selection(
    run: &ReviewRunCreateParams,
    attempts: &[ReviewerAttemptCreateParams],
) -> anyhow::Result<()> {
    require_value("review run id", &run.id)?;
    require_value("review run subject", &run.review_run_subject)?;
    require_value("review run owner process id", &run.owner_process_id)?;
    if attempts.is_empty() {
        bail!("review run must select at least one reviewer");
    }

    let mut ids = HashSet::with_capacity(attempts.len());
    let mut ordinals = HashSet::with_capacity(attempts.len());
    let mut account_subjects = HashSet::with_capacity(attempts.len());
    let mut model_family_subjects = HashSet::with_capacity(attempts.len());
    let mut attempt_subjects = HashSet::with_capacity(attempts.len());
    let mut idempotency_keys = HashSet::with_capacity(attempts.len());
    for attempt in attempts {
        if attempt.ordinal < 0 {
            bail!("reviewer attempt ordinal cannot be negative");
        }
        for (name, value) in [
            ("reviewer attempt id", attempt.id.as_str()),
            ("provider id", attempt.provider_id.as_str()),
            ("model", attempt.model.as_str()),
            ("account subject", attempt.account_subject.as_str()),
            (
                "model family subject",
                attempt.model_family_subject.as_str(),
            ),
            (
                "reviewer attempt subject",
                attempt.reviewer_attempt_subject.as_str(),
            ),
            ("idempotency key", attempt.idempotency_key.as_str()),
            ("prompt", attempt.prompt.as_str()),
            ("MCP server", attempt.mcp_server.as_str()),
            ("MCP tool", attempt.mcp_tool.as_str()),
        ] {
            require_value(name, value)?;
        }
        require_unique(&mut ids, &attempt.id, "reviewer attempt id")?;
        require_unique(&mut ordinals, attempt.ordinal, "reviewer ordinal")?;
        require_unique(
            &mut account_subjects,
            &attempt.account_subject,
            "credential subject",
        )?;
        require_unique(
            &mut model_family_subjects,
            &attempt.model_family_subject,
            "model family subject",
        )?;
        require_unique(
            &mut attempt_subjects,
            &attempt.reviewer_attempt_subject,
            "reviewer attempt subject",
        )?;
        require_unique(
            &mut idempotency_keys,
            &attempt.idempotency_key,
            "idempotency key",
        )?;
    }
    Ok(())
}

fn require_value(name: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        bail!("{name} cannot be empty");
    }
    Ok(())
}

fn require_unique<T>(values: &mut HashSet<T>, value: T, name: &str) -> anyhow::Result<()>
where
    T: Eq + std::hash::Hash,
{
    if !values.insert(value) {
        bail!("duplicate {name} in reviewer selection");
    }
    Ok(())
}

fn validate_transition_data(
    target: ReviewAttemptState,
    data: &ReviewAttemptTransitionData,
) -> anyhow::Result<()> {
    let empty = |field: &Option<String>| field.is_none();
    match target {
        ReviewAttemptState::Spawn | ReviewAttemptState::Acknowledged => {
            if !empty(&data.process_id)
                || !empty(&data.raw_output)
                || data.submission.is_some()
                || !empty(&data.failure)
            {
                bail!("transition to {} cannot attach data", target.as_str());
            }
        }
        ReviewAttemptState::ModelExecution => {
            require_value(
                "spawned process id",
                data.process_id
                    .as_deref()
                    .context("model_execution transition requires a process id")?,
            )?;
            if !empty(&data.raw_output) || data.submission.is_some() || !empty(&data.failure) {
                bail!("model_execution transition only accepts a process id");
            }
        }
        ReviewAttemptState::OutputParse => {
            data.raw_output
                .as_deref()
                .context("output_parse transition requires raw output")?;
            if !empty(&data.process_id) || data.submission.is_some() || !empty(&data.failure) {
                bail!("output_parse transition only accepts raw output");
            }
        }
        ReviewAttemptState::SubmissionUnknown => {
            if data.submission.is_none() {
                bail!("submission_unknown transition requires parsed submission JSON");
            }
            if !empty(&data.process_id) || !empty(&data.raw_output) || !empty(&data.failure) {
                bail!("submission_unknown transition only accepts submission JSON");
            }
        }
        ReviewAttemptState::Cancelled | ReviewAttemptState::TerminalFailure => {
            require_value(
                "terminal reason",
                data.failure
                    .as_deref()
                    .context("terminal transition requires a reason")?,
            )?;
            if !empty(&data.process_id) || !empty(&data.raw_output) || data.submission.is_some() {
                bail!("terminal transition only accepts a reason");
            }
        }
        ReviewAttemptState::Selection => bail!("selection is only an initial state"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StateRuntime;
    use serde_json::json;

    async fn database() -> RuntimeDbHandle {
        let home = std::env::temp_dir().join(format!(
            "chaos-reviewer-orchestration-{}",
            uuid::Uuid::new_v4()
        ));
        RuntimeDbHandle::Sqlite(
            StateRuntime::init(home, "test".to_string())
                .await
                .expect("runtime database"),
        )
    }

    fn attempt(ordinal: i64, account: char, family: char) -> ReviewerAttemptCreateParams {
        ReviewerAttemptCreateParams {
            id: format!("attempt-{ordinal}"),
            ordinal,
            provider_id: format!("provider-{ordinal}"),
            model: format!("model-{ordinal}"),
            account_subject: format!("credential:v1:{}", account.to_string().repeat(64)),
            model_family_subject: format!("review-subject:v1:{}", family.to_string().repeat(64)),
            reviewer_attempt_subject: format!(
                "reviewer-attempt:v1:{}",
                (ordinal + 1).to_string().repeat(64)
            )
            .chars()
            .take("reviewer-attempt:v1:".len() + 64)
            .collect(),
            idempotency_key: format!("review-{ordinal}"),
            prompt: "Return strict review JSON".to_string(),
            mcp_server: "review-service".to_string(),
            mcp_tool: "submit_review".to_string(),
        }
    }

    fn run() -> ReviewRunCreateParams {
        ReviewRunCreateParams {
            id: "run-1".to_string(),
            review_run_subject: format!("review-run:v1:{}", "a".repeat(64)),
            attestation_subject: format!("review-run:v1:{}", "b".repeat(64)),
            owner_process_id: "owner-process-1".to_string(),
        }
    }

    #[tokio::test]
    async fn persists_immutable_selection_and_declared_state_progression() {
        let db = database().await;
        let store = db.reviewer_orchestrations();
        let persisted = store
            .create_run(&run(), &[attempt(0, 'a', 'b'), attempt(1, 'b', 'c')])
            .await
            .unwrap();
        assert_eq!(persisted.owner_process_id, "owner-process-1");
        assert_eq!(
            store
                .get_run("run-1")
                .await
                .unwrap()
                .unwrap()
                .owner_process_id,
            "owner-process-1"
        );

        let first = &store.list_attempts("run-1").await.unwrap()[0];
        assert_eq!(first.state, ReviewAttemptState::Selection);
        assert!(
            store
                .transition_attempt(
                    "attempt-0",
                    ReviewAttemptState::Selection,
                    ReviewAttemptState::Spawn,
                    &ReviewAttemptTransitionData::default(),
                )
                .await
                .unwrap()
        );
        assert!(
            store
                .transition_attempt(
                    "attempt-0",
                    ReviewAttemptState::Spawn,
                    ReviewAttemptState::ModelExecution,
                    &ReviewAttemptTransitionData {
                        process_id: Some("process-1".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap()
        );
        assert!(
            store
                .transition_attempt(
                    "attempt-0",
                    ReviewAttemptState::ModelExecution,
                    ReviewAttemptState::OutputParse,
                    &ReviewAttemptTransitionData {
                        raw_output: Some("{}".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap()
        );
        assert!(
            store
                .transition_attempt(
                    "attempt-0",
                    ReviewAttemptState::OutputParse,
                    ReviewAttemptState::SubmissionUnknown,
                    &ReviewAttemptTransitionData {
                        submission: Some(json!({"findings": []})),
                        ..Default::default()
                    },
                )
                .await
                .unwrap()
        );

        let reloaded = store.get_attempt("attempt-0").await.unwrap().unwrap();
        assert_eq!(reloaded.state, ReviewAttemptState::SubmissionUnknown);
        assert_eq!(reloaded.process_id.as_deref(), Some("process-1"));
        assert_eq!(reloaded.raw_output.as_deref(), Some("{}"));
        assert_eq!(reloaded.submission, Some(json!({"findings": []})));
    }

    #[tokio::test]
    async fn rejects_duplicate_credential_or_family_before_database_write() {
        let db = database().await;
        let store = db.reviewer_orchestrations();
        let error = store
            .create_run(&run(), &[attempt(0, 'a', 'b'), attempt(1, 'a', 'c')])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("duplicate credential subject"));
        assert!(store.get_run("run-1").await.unwrap().is_none());

        let error = store
            .create_run(&run(), &[attempt(0, 'a', 'b'), attempt(1, 'c', 'b')])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("duplicate model family subject"));
        assert!(store.get_run("run-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_empty_owner_process_id_before_database_write() {
        let db = database().await;
        let store = db.reviewer_orchestrations();
        let mut invalid = run();
        invalid.owner_process_id = "  ".to_string();

        let error = store
            .create_run(&invalid, &[attempt(0, 'a', 'b')])
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("review run owner process id cannot be empty")
        );
        assert!(store.get_run("run-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn database_trigger_rejects_binding_mutation() {
        let db = database().await;
        let store = db.reviewer_orchestrations();
        store
            .create_run(&run(), &[attempt(0, 'a', 'b')])
            .await
            .unwrap();
        let pool = db.sqlite_pool_cloned().unwrap();
        let error = sqlx::query("UPDATE reviewer_attempts SET model = ? WHERE id = ?")
            .bind("forged")
            .bind("attempt-0")
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("immutable"));

        let error = sqlx::query("UPDATE review_runs SET owner_process_id = ? WHERE id = ?")
            .bind("another-owner")
            .bind("run-1")
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("immutable"));
    }
}
