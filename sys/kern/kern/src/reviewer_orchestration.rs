use crate::protocol::ReviewOutputEvent;
use crate::review_provenance::review_run_subject;
use crate::review_provenance::reviewer_attempt_subject;
use anyhow::Context;
use anyhow::bail;
use chaos_mcp_runtime::TrustedReviewProvenance;
use chaos_proc::ReviewAttemptState;
use chaos_proc::ReviewAttemptTransitionData;
use chaos_proc::ReviewRun;
use chaos_proc::ReviewRunCreateParams;
use chaos_proc::ReviewerAttempt;
use chaos_proc::ReviewerAttemptCreateParams;
use chaos_proc::RuntimeDbHandle;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewerBinding {
    pub provider_id: String,
    pub model: String,
    pub account_subject: String,
    pub model_family_subject: String,
}

#[derive(Clone, Debug)]
pub struct ReviewerSelection {
    pub binding: ReviewerBinding,
    pub prompt: String,
    pub mcp_server: String,
    pub mcp_tool: String,
    /// The exact idempotency key allocated by Skynet for this reviewer.
    pub idempotency_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnedReviewer {
    pub process_id: String,
    pub effective_binding: ReviewerBinding,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewerOutput {
    Pending,
    Completed(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmissionOutcome {
    Acknowledged,
    Rejected(String),
}

#[allow(async_fn_in_trait)]
pub trait ReviewerBoundary {
    async fn spawn_reviewer(
        &self,
        binding: &ReviewerBinding,
        prompt: &str,
    ) -> anyhow::Result<SpawnedReviewer>;

    async fn reviewer_output(&self, process_id: &str) -> anyhow::Result<ReviewerOutput>;

    async fn submit_review(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        provenance: TrustedReviewProvenance,
    ) -> anyhow::Result<SubmissionOutcome>;

    async fn cancel_reviewer(&self, process_id: &str) -> anyhow::Result<()>;
}

pub struct ReviewerOrchestrator<B> {
    db: RuntimeDbHandle,
    boundary: B,
}

impl<B> ReviewerOrchestrator<B>
where
    B: ReviewerBoundary,
{
    pub fn new(db: RuntimeDbHandle, boundary: B) -> Self {
        Self { db, boundary }
    }

    /// Validate the complete diversity set before persistence or any boundary
    /// call, then atomically persist immutable reviewer bindings in Selection.
    pub async fn start_run(&self, selections: Vec<ReviewerSelection>) -> anyhow::Result<ReviewRun> {
        validate_diverse_selection(&selections)?;

        let run_id = Uuid::new_v4().to_string();
        let run_subject = review_run_subject(&run_id);
        let mut attempts = Vec::with_capacity(selections.len());
        for (ordinal, selection) in selections.into_iter().enumerate() {
            let attempt_id = Uuid::new_v4().to_string();
            let attempt_subject = reviewer_attempt_subject(&attempt_id);
            // Construction is the host-only schema gate. It rejects malformed
            // opaque subjects and non-wire-safe Skynet keys before DB or spawn.
            TrustedReviewProvenance::new(
                selection.binding.account_subject.clone(),
                selection.binding.model_family_subject.clone(),
                run_subject.clone(),
                attempt_subject.clone(),
                selection.idempotency_key.clone(),
            )?;
            attempts.push(ReviewerAttemptCreateParams {
                id: attempt_id,
                ordinal: i64::try_from(ordinal).context("too many selected reviewers")?,
                provider_id: selection.binding.provider_id,
                model: selection.binding.model,
                account_subject: selection.binding.account_subject,
                model_family_subject: selection.binding.model_family_subject,
                reviewer_attempt_subject: attempt_subject,
                idempotency_key: selection.idempotency_key,
                prompt: selection.prompt,
                mcp_server: selection.mcp_server,
                mcp_tool: selection.mcp_tool,
            });
        }

        self.db
            .reviewer_orchestrations()
            .create_run(
                &ReviewRunCreateParams {
                    id: run_id,
                    review_run_subject: run_subject,
                },
                &attempts,
            )
            .await
    }

    /// Continue every non-terminal attempt from its persisted state.
    ///
    /// An acknowledged attempt is never submitted again. A submission whose
    /// acknowledgement was lost remains SubmissionUnknown, so reconnect
    /// retries the exact persisted JSON with the exact persisted Skynet key.
    pub async fn resume_run(&self, run_id: &str) -> anyhow::Result<Vec<ReviewerAttempt>> {
        let store = self.db.reviewer_orchestrations();
        let run = store
            .get_run(run_id)
            .await?
            .with_context(|| format!("review run `{run_id}` not found"))?;
        for attempt in store.list_attempts(run_id).await? {
            self.drive_attempt(&run, &attempt.id).await?;
        }
        store.list_attempts(run_id).await
    }

    pub async fn cancel_attempt(&self, attempt_id: &str, reason: &str) -> anyhow::Result<bool> {
        if reason.trim().is_empty() {
            bail!("reviewer cancellation reason cannot be empty");
        }
        let store = self.db.reviewer_orchestrations();
        let Some(attempt) = store.get_attempt(attempt_id).await? else {
            bail!("reviewer attempt `{attempt_id}` not found");
        };
        if attempt.state.is_terminal() {
            return Ok(false);
        }
        if let Some(process_id) = attempt.process_id.as_deref() {
            self.boundary.cancel_reviewer(process_id).await?;
        }
        store
            .transition_attempt(
                attempt_id,
                attempt.state,
                ReviewAttemptState::Cancelled,
                &ReviewAttemptTransitionData {
                    failure: Some(reason.to_string()),
                    ..Default::default()
                },
            )
            .await
    }

    async fn drive_attempt(&self, run: &ReviewRun, attempt_id: &str) -> anyhow::Result<()> {
        let store = self.db.reviewer_orchestrations();
        loop {
            let attempt = store
                .get_attempt(attempt_id)
                .await?
                .with_context(|| format!("reviewer attempt `{attempt_id}` not found"))?;
            match attempt.state {
                ReviewAttemptState::Selection => {
                    store
                        .transition_attempt(
                            attempt_id,
                            ReviewAttemptState::Selection,
                            ReviewAttemptState::Spawn,
                            &ReviewAttemptTransitionData::default(),
                        )
                        .await?;
                }
                ReviewAttemptState::Spawn => {
                    let expected = binding_from_attempt(&attempt);
                    let spawned = match self
                        .boundary
                        .spawn_reviewer(&expected, &attempt.prompt)
                        .await
                    {
                        Ok(spawned) => spawned,
                        Err(error) => {
                            self.fail_attempt(
                                &attempt,
                                format!("reviewer spawn failed: {error:#}"),
                            )
                            .await?;
                            return Err(error.context("reviewer spawn failed"));
                        }
                    };
                    if spawned.effective_binding != expected {
                        let _ = self.boundary.cancel_reviewer(&spawned.process_id).await;
                        let reason = "spawned reviewer effective provider/account/model binding \
                                      did not match immutable selection"
                            .to_string();
                        self.fail_attempt(&attempt, reason.clone()).await?;
                        bail!("{reason}");
                    }
                    let advanced = store
                        .transition_attempt(
                            attempt_id,
                            ReviewAttemptState::Spawn,
                            ReviewAttemptState::ModelExecution,
                            &ReviewAttemptTransitionData {
                                process_id: Some(spawned.process_id.clone()),
                                ..Default::default()
                            },
                        )
                        .await?;
                    if !advanced {
                        // Another owner moved the row after this spawn. Never
                        // leave an untracked reviewer running.
                        let _ = self.boundary.cancel_reviewer(&spawned.process_id).await;
                    }
                }
                ReviewAttemptState::ModelExecution => {
                    let process_id = attempt.process_id.as_deref().with_context(|| {
                        format!("model_execution attempt `{attempt_id}` has no process id")
                    })?;
                    match self.boundary.reviewer_output(process_id).await? {
                        ReviewerOutput::Pending => return Ok(()),
                        ReviewerOutput::Completed(raw_output) => {
                            store
                                .transition_attempt(
                                    attempt_id,
                                    ReviewAttemptState::ModelExecution,
                                    ReviewAttemptState::OutputParse,
                                    &ReviewAttemptTransitionData {
                                        raw_output: Some(raw_output),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                        }
                        ReviewerOutput::Failed(reason) => {
                            self.fail_attempt(&attempt, reason.clone()).await?;
                            bail!("reviewer model execution failed: {reason}");
                        }
                    }
                }
                ReviewAttemptState::OutputParse => {
                    let raw_output = attempt.raw_output.as_deref().with_context(|| {
                        format!("output_parse attempt `{attempt_id}` has no raw output")
                    })?;
                    let submission = match parse_strict_review_output(raw_output) {
                        Ok(submission) => submission,
                        Err(error) => {
                            let reason = format!("invalid reviewer output: {error:#}");
                            self.fail_attempt(&attempt, reason.clone()).await?;
                            bail!("{reason}");
                        }
                    };
                    store
                        .transition_attempt(
                            attempt_id,
                            ReviewAttemptState::OutputParse,
                            ReviewAttemptState::SubmissionUnknown,
                            &ReviewAttemptTransitionData {
                                submission: Some(submission),
                                ..Default::default()
                            },
                        )
                        .await?;
                }
                ReviewAttemptState::SubmissionUnknown => {
                    let submission = attempt.submission.clone().with_context(|| {
                        format!("submission_unknown attempt `{attempt_id}` has no payload")
                    })?;
                    let provenance = TrustedReviewProvenance::new(
                        attempt.account_subject.clone(),
                        attempt.model_family_subject.clone(),
                        run.review_run_subject.clone(),
                        attempt.reviewer_attempt_subject.clone(),
                        attempt.idempotency_key.clone(),
                    )
                    .context("persisted reviewer provenance is invalid")?;
                    let outcome = self
                        .boundary
                        .submit_review(
                            &attempt.mcp_server,
                            &attempt.mcp_tool,
                            submission,
                            provenance,
                        )
                        .await
                        .context("review submission acknowledgement unknown")?;
                    match outcome {
                        SubmissionOutcome::Acknowledged => {
                            store
                                .transition_attempt(
                                    attempt_id,
                                    ReviewAttemptState::SubmissionUnknown,
                                    ReviewAttemptState::Acknowledged,
                                    &ReviewAttemptTransitionData::default(),
                                )
                                .await?;
                        }
                        SubmissionOutcome::Rejected(reason) => {
                            self.fail_attempt(&attempt, reason.clone()).await?;
                            bail!("review submission rejected: {reason}");
                        }
                    }
                }
                ReviewAttemptState::Acknowledged
                | ReviewAttemptState::Cancelled
                | ReviewAttemptState::TerminalFailure => return Ok(()),
            }
        }
    }

    async fn fail_attempt(&self, attempt: &ReviewerAttempt, reason: String) -> anyhow::Result<()> {
        self.db
            .reviewer_orchestrations()
            .transition_attempt(
                &attempt.id,
                attempt.state,
                ReviewAttemptState::TerminalFailure,
                &ReviewAttemptTransitionData {
                    failure: Some(reason),
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }
}

fn binding_from_attempt(attempt: &ReviewerAttempt) -> ReviewerBinding {
    ReviewerBinding {
        provider_id: attempt.provider_id.clone(),
        model: attempt.model.clone(),
        account_subject: attempt.account_subject.clone(),
        model_family_subject: attempt.model_family_subject.clone(),
    }
}

fn validate_diverse_selection(selections: &[ReviewerSelection]) -> anyhow::Result<()> {
    if selections.is_empty() {
        bail!("review run must select at least one reviewer");
    }
    let mut accounts = HashSet::with_capacity(selections.len());
    let mut families = HashSet::with_capacity(selections.len());
    let mut keys = HashSet::with_capacity(selections.len());
    for selection in selections {
        if !accounts.insert(selection.binding.account_subject.as_str()) {
            bail!("duplicate credential subject in reviewer selection");
        }
        if !families.insert(selection.binding.model_family_subject.as_str()) {
            bail!("duplicate canonical model family in reviewer selection");
        }
        if !keys.insert(selection.idempotency_key.as_str()) {
            bail!("duplicate Skynet idempotency key in reviewer selection");
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictReviewOutput {
    findings: Vec<StrictReviewFinding>,
    overall_correctness: String,
    overall_explanation: String,
    overall_confidence_score: f32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictReviewFinding {
    title: String,
    body: String,
    confidence_score: f32,
    priority: i32,
    code_location: StrictReviewCodeLocation,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictReviewCodeLocation {
    absolute_file_path: std::path::PathBuf,
    line_range: StrictReviewLineRange,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictReviewLineRange {
    start: u32,
    end: u32,
}

fn parse_strict_review_output(raw_output: &str) -> anyhow::Result<Value> {
    let strict: StrictReviewOutput =
        serde_json::from_str(raw_output).context("expected strict ReviewOutputEvent JSON")?;
    // Convert through the protocol type as a compatibility assertion: the
    // persisted payload is exactly what current ChaOS review consumers accept.
    let value = serde_json::to_value(strict)?;
    serde_json::from_value::<ReviewOutputEvent>(value.clone())
        .context("review output does not match the protocol schema")?;
    Ok(value)
}

/// Production adapter for exact provider/model spawning, live or persisted
/// output recovery, and host-attested MCP submission.
///
/// Construction stays kernel-private because Session and TurnContext are
/// kernel capabilities, while the orchestration API above remains testable at
/// a real boundary without exposing those capabilities.
#[derive(Clone)]
#[expect(
    dead_code,
    reason = "constructed by the v0.9 review coordinator integration point"
)]
pub(crate) struct SessionReviewerBoundary {
    session: std::sync::Arc<crate::chaos::Session>,
    turn: std::sync::Arc<crate::chaos::TurnContext>,
}

#[expect(
    dead_code,
    reason = "constructed by the v0.9 review coordinator integration point"
)]
impl SessionReviewerBoundary {
    pub(crate) fn new(
        session: std::sync::Arc<crate::chaos::Session>,
        turn: std::sync::Arc<crate::chaos::TurnContext>,
    ) -> Self {
        Self { session, turn }
    }

    pub(crate) fn orchestrator(
        session: std::sync::Arc<crate::chaos::Session>,
        turn: std::sync::Arc<crate::chaos::TurnContext>,
    ) -> anyhow::Result<ReviewerOrchestrator<Self>> {
        let db = session
            .services
            .runtime_db
            .clone()
            .context("reviewer orchestration requires a runtime database")?;
        Ok(ReviewerOrchestrator::new(db, Self::new(session, turn)))
    }
}

impl ReviewerBoundary for SessionReviewerBoundary {
    async fn spawn_reviewer(
        &self,
        binding: &ReviewerBinding,
        prompt: &str,
    ) -> anyhow::Result<SpawnedReviewer> {
        use chaos_ipc::protocol::SessionSource;
        use chaos_ipc::protocol::SubAgentSource;
        use chaos_ipc::user_input::UserInput;

        let configured_account = self
            .session
            .services
            .auth_manager
            .credential_subject_fingerprint_for_provider(
                &binding.provider_id,
                crate::review_provenance::REVIEW_ACCOUNT_SUBJECT_DOMAIN,
            )
            .map(crate::auth::CredentialSubjectFingerprint::into_string)
            .context("selected provider has no attestable credential subject")?;
        if configured_account != binding.account_subject {
            bail!("selected provider credential changed before reviewer spawn");
        }

        let child_depth = crate::minions::next_process_spawn_depth(&self.turn.session_source);
        if crate::minions::exceeds_process_spawn_depth_limit(
            child_depth,
            self.turn.config.agent_max_depth,
        ) {
            bail!("reviewer spawn exceeds agent depth limit");
        }
        let mut config = crate::minions::tools::build_agent_spawn_config(
            &self.session.get_base_instructions().await,
            self.turn.as_ref(),
        )
        .map_err(|error| anyhow::anyhow!("{error}"))?;
        crate::minions::tools::apply_requested_spawn_agent_provider_binding(
            &self.session,
            &mut config,
            &binding.provider_id,
            Some(&binding.model),
            None,
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
        crate::minions::tools::apply_spawn_agent_overrides(&mut config, child_depth);
        let spawned = self
            .session
            .services
            .agent_control
            .spawn_agent_with_options(
                config,
                vec![UserInput::Text {
                    text: prompt.to_string(),
                    text_elements: Vec::new(),
                }],
                Some(SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
                    parent_process_id: self.session.conversation_id,
                    depth: child_depth,
                    agent_nickname: None,
                    agent_role: Some("reviewer".to_string()),
                })),
                crate::minions::control::SpawnAgentOptions {
                    suppress_parent_completion_notification: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;

        let account_subject = match spawned.provenance.account_subject {
            Some(subject) => subject,
            None => {
                let _ = self
                    .session
                    .services
                    .agent_control
                    .shutdown_agent(spawned.process_id)
                    .await;
                bail!("spawned reviewer has no effective credential subject");
            }
        };
        let model_family_subject = match spawned.provenance.model_family_subject {
            Some(subject) => subject,
            None => {
                let _ = self
                    .session
                    .services
                    .agent_control
                    .shutdown_agent(spawned.process_id)
                    .await;
                bail!("spawned reviewer has no canonical model family");
            }
        };
        Ok(SpawnedReviewer {
            process_id: spawned.process_id.to_string(),
            effective_binding: ReviewerBinding {
                provider_id: spawned.provenance.effective_model_provider,
                model: spawned.provenance.effective_model,
                account_subject,
                model_family_subject,
            },
        })
    }

    async fn reviewer_output(&self, process_id: &str) -> anyhow::Result<ReviewerOutput> {
        use crate::minions::AgentStatus;
        use chaos_ipc::ProcessId;
        use chaos_ipc::protocol::EventMsg;

        let process_id =
            ProcessId::from_string(process_id).context("invalid reviewer process id")?;
        match self
            .session
            .services
            .agent_control
            .get_status(process_id)
            .await
        {
            AgentStatus::PendingInit | AgentStatus::Running => Ok(ReviewerOutput::Pending),
            AgentStatus::Completed(Some(output)) => Ok(ReviewerOutput::Completed(output)),
            AgentStatus::Completed(None) => Ok(ReviewerOutput::Failed(
                "reviewer completed without output".to_string(),
            )),
            AgentStatus::Interrupted => Ok(ReviewerOutput::Failed(
                "reviewer execution was interrupted".to_string(),
            )),
            AgentStatus::Errored(error) => Ok(ReviewerOutput::Failed(error)),
            AgentStatus::Shutdown => Ok(ReviewerOutput::Failed(
                "reviewer shut down before producing output".to_string(),
            )),
            AgentStatus::NotFound => {
                let history = crate::RolloutRecorder::get_rollout_history_for_process(process_id)
                    .await
                    .context("reviewer is not live and persisted rollout could not be read")?;
                let output = history.get_event_msgs().and_then(|events| {
                    events.into_iter().rev().find_map(|event| match event {
                        EventMsg::TurnComplete(event) => event.last_agent_message,
                        _ => None,
                    })
                });
                output.map_or_else(
                    || {
                        Ok(ReviewerOutput::Failed(
                            "reviewer is not live and its rollout has no completed output"
                                .to_string(),
                        ))
                    },
                    |output| Ok(ReviewerOutput::Completed(output)),
                )
            }
        }
    }

    async fn submit_review(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        provenance: TrustedReviewProvenance,
    ) -> anyhow::Result<SubmissionOutcome> {
        let result = self
            .session
            .call_tool_with_review_provenance(server, tool, Some(arguments), None, provenance)
            .await?;
        if result.is_error == Some(true) {
            return Ok(SubmissionOutcome::Rejected(
                serde_json::to_string(&result.content)
                    .unwrap_or_else(|_| "MCP server rejected review submission".to_string()),
            ));
        }
        Ok(SubmissionOutcome::Acknowledged)
    }

    async fn cancel_reviewer(&self, process_id: &str) -> anyhow::Result<()> {
        let process_id =
            chaos_ipc::ProcessId::from_string(process_id).context("invalid reviewer process id")?;
        self.session
            .services
            .agent_control
            .shutdown_agent(process_id)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_proc::StateRuntime;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct FakeState {
        spawn_calls: Vec<ReviewerBinding>,
        outputs: HashMap<String, ReviewerOutput>,
        submit_keys: Vec<String>,
        submit_payloads: Vec<Value>,
        submit_provenance: Vec<Value>,
        accepted_keys: HashSet<String>,
        accepted_count: usize,
        cancelled: Vec<String>,
        mismatch_model: bool,
        drop_first_ack: bool,
    }

    #[derive(Clone, Default)]
    struct FakeBoundary {
        state: Arc<Mutex<FakeState>>,
    }

    impl ReviewerBoundary for FakeBoundary {
        async fn spawn_reviewer(
            &self,
            binding: &ReviewerBinding,
            _prompt: &str,
        ) -> anyhow::Result<SpawnedReviewer> {
            let mut state = self.state.lock().await;
            state.spawn_calls.push(binding.clone());
            let process_id = format!("process-{}", state.spawn_calls.len());
            let output = state
                .outputs
                .get("next")
                .cloned()
                .unwrap_or_else(|| ReviewerOutput::Completed(valid_output()));
            state.outputs.insert(process_id.clone(), output);
            let mut effective_binding = binding.clone();
            if state.mismatch_model {
                effective_binding.model.push_str("-wrong");
            }
            Ok(SpawnedReviewer {
                process_id,
                effective_binding,
            })
        }

        async fn reviewer_output(&self, process_id: &str) -> anyhow::Result<ReviewerOutput> {
            Ok(self
                .state
                .lock()
                .await
                .outputs
                .get(process_id)
                .cloned()
                .unwrap_or(ReviewerOutput::Pending))
        }

        async fn submit_review(
            &self,
            _server: &str,
            _tool: &str,
            arguments: Value,
            provenance: TrustedReviewProvenance,
        ) -> anyhow::Result<SubmissionOutcome> {
            let metadata = serde_json::to_value(provenance)?;
            let key = metadata["idempotency_key"]
                .as_str()
                .context("provenance idempotency key")?
                .to_string();
            let mut state = self.state.lock().await;
            state.submit_keys.push(key.clone());
            state.submit_payloads.push(arguments);
            state.submit_provenance.push(metadata);
            if state.accepted_keys.insert(key) {
                state.accepted_count += 1;
            }
            if state.drop_first_ack {
                state.drop_first_ack = false;
                bail!("simulated dropped acknowledgement");
            }
            Ok(SubmissionOutcome::Acknowledged)
        }

        async fn cancel_reviewer(&self, process_id: &str) -> anyhow::Result<()> {
            self.state
                .lock()
                .await
                .cancelled
                .push(process_id.to_string());
            Ok(())
        }
    }

    async fn database() -> RuntimeDbHandle {
        let home = std::env::temp_dir().join(format!(
            "chaos-kernel-review-orchestration-{}",
            Uuid::new_v4()
        ));
        RuntimeDbHandle::Sqlite(
            StateRuntime::init(home, "test".to_string())
                .await
                .expect("runtime database"),
        )
    }

    fn subject(prefix: &str, byte: char) -> String {
        format!("{prefix}{}", byte.to_string().repeat(64))
    }

    fn selection(index: usize, account: char, family: char) -> ReviewerSelection {
        ReviewerSelection {
            binding: ReviewerBinding {
                provider_id: format!("provider-{index}"),
                model: format!("model-{index}"),
                account_subject: subject("credential:v1:", account),
                model_family_subject: subject("review-subject:v1:", family),
            },
            prompt: "Review and return strict JSON only".to_string(),
            mcp_server: "skynet".to_string(),
            mcp_tool: "submit_review".to_string(),
            idempotency_key: format!("skynet-review-{index}"),
        }
    }

    fn valid_output() -> String {
        json!({
            "findings": [],
            "overall_correctness": "patch is correct",
            "overall_explanation": "No findings.",
            "overall_confidence_score": 0.98
        })
        .to_string()
    }

    #[tokio::test]
    async fn diverse_fake_provider_accounts_complete_with_verified_bindings() {
        let db = database().await;
        let boundary = FakeBoundary::default();
        let orchestrator = ReviewerOrchestrator::new(db, boundary.clone());
        let run = orchestrator
            .start_run(vec![selection(0, 'a', 'b'), selection(1, 'c', 'd')])
            .await
            .unwrap();
        let attempts = orchestrator.resume_run(&run.id).await.unwrap();

        assert_eq!(attempts.len(), 2);
        assert!(
            attempts
                .iter()
                .all(|attempt| attempt.state == ReviewAttemptState::Acknowledged)
        );
        let state = boundary.state.lock().await;
        assert_eq!(state.spawn_calls.len(), 2);
        assert_ne!(
            state.spawn_calls[0].account_subject,
            state.spawn_calls[1].account_subject
        );
        assert_ne!(
            state.spawn_calls[0].model_family_subject,
            state.spawn_calls[1].model_family_subject
        );
        assert_eq!(state.accepted_count, 2);
    }

    #[tokio::test]
    async fn duplicate_credential_is_rejected_before_spawn_or_submission() {
        let db = database().await;
        let boundary = FakeBoundary::default();
        let orchestrator = ReviewerOrchestrator::new(db, boundary.clone());
        let error = orchestrator
            .start_run(vec![selection(0, 'a', 'b'), selection(1, 'a', 'c')])
            .await
            .unwrap_err();

        assert!(error.to_string().contains("duplicate credential"));
        let state = boundary.state.lock().await;
        assert!(state.spawn_calls.is_empty());
        assert!(state.submit_keys.is_empty());
    }

    #[tokio::test]
    async fn effective_binding_mismatch_fails_closed() {
        let db = database().await;
        let boundary = FakeBoundary::default();
        boundary.state.lock().await.mismatch_model = true;
        let orchestrator = ReviewerOrchestrator::new(db.clone(), boundary.clone());
        let run = orchestrator
            .start_run(vec![selection(0, 'a', 'b')])
            .await
            .unwrap();
        let error = orchestrator.resume_run(&run.id).await.unwrap_err();

        assert!(error.to_string().contains("did not match"));
        let attempt = &db
            .reviewer_orchestrations()
            .list_attempts(&run.id)
            .await
            .unwrap()[0];
        assert_eq!(attempt.state, ReviewAttemptState::TerminalFailure);
        let state = boundary.state.lock().await;
        assert_eq!(state.spawn_calls.len(), 1);
        assert!(state.submit_keys.is_empty());
        assert_eq!(state.cancelled, vec!["process-1"]);
    }

    #[tokio::test]
    async fn dropped_ack_retries_exact_persisted_key_without_double_counting() {
        let db = database().await;
        let boundary = FakeBoundary::default();
        boundary.state.lock().await.drop_first_ack = true;
        let orchestrator = ReviewerOrchestrator::new(db.clone(), boundary.clone());
        let run = orchestrator
            .start_run(vec![selection(0, 'a', 'b')])
            .await
            .unwrap();

        let error = orchestrator.resume_run(&run.id).await.unwrap_err();
        assert!(error.to_string().contains("acknowledgement unknown"));
        let unknown = &db
            .reviewer_orchestrations()
            .list_attempts(&run.id)
            .await
            .unwrap()[0];
        assert_eq!(unknown.state, ReviewAttemptState::SubmissionUnknown);
        let exact_key = unknown.idempotency_key.clone();
        let exact_payload = unknown.submission.clone();

        let attempts = orchestrator.resume_run(&run.id).await.unwrap();
        assert_eq!(attempts[0].state, ReviewAttemptState::Acknowledged);
        assert_eq!(attempts[0].submission, exact_payload);
        let state = boundary.state.lock().await;
        assert_eq!(state.submit_keys, vec![exact_key.clone(), exact_key]);
        assert_eq!(state.submit_payloads.len(), 2);
        assert_eq!(state.submit_payloads[0], state.submit_payloads[1]);
        assert_eq!(state.submit_payloads[0], exact_payload.unwrap());
        assert_eq!(state.submit_provenance.len(), 2);
        assert_eq!(state.submit_provenance[0], state.submit_provenance[1]);
        for field in [
            "account_subject",
            "model_family_subject",
            "review_run_subject",
            "reviewer_attempt_subject",
            "idempotency_key",
        ] {
            assert!(
                state.submit_provenance[0].get(field).is_some(),
                "missing trusted provenance field {field}"
            );
        }
        assert_eq!(state.accepted_count, 1);
        assert_eq!(state.spawn_calls.len(), 1);
    }

    #[tokio::test]
    async fn invalid_reviewer_output_is_an_explicit_terminal_failure() {
        let db = database().await;
        let boundary = FakeBoundary::default();
        boundary.state.lock().await.outputs.insert(
            "next".to_string(),
            ReviewerOutput::Completed("looks good".to_string()),
        );
        let orchestrator = ReviewerOrchestrator::new(db.clone(), boundary.clone());
        let run = orchestrator
            .start_run(vec![selection(0, 'a', 'b')])
            .await
            .unwrap();
        let error = orchestrator.resume_run(&run.id).await.unwrap_err();

        assert!(error.to_string().contains("invalid reviewer output"));
        let attempt = &db
            .reviewer_orchestrations()
            .list_attempts(&run.id)
            .await
            .unwrap()[0];
        assert_eq!(attempt.state, ReviewAttemptState::TerminalFailure);
        assert!(
            attempt
                .failure
                .as_deref()
                .is_some_and(|failure| failure.contains("invalid reviewer output"))
        );
        assert!(boundary.state.lock().await.submit_keys.is_empty());
    }
}
