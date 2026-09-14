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
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

pub(crate) const REVIEW_VERDICT_TOOL: &str = "submit_review_verdict";

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
    /// The exact idempotency key allocated by the review service.
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
        attempt_id: &str,
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
    pub async fn start_run(
        &self,
        owner_process_id: &str,
        review_scope: Option<&str>,
        selections: Vec<ReviewerSelection>,
    ) -> anyhow::Result<ReviewRun> {
        if owner_process_id.trim().is_empty() {
            bail!("review owner process id cannot be empty");
        }
        validate_diverse_selection(&selections)?;
        let review_scope = match review_scope {
            Some(scope) => {
                let scope = scope.trim();
                if scope.is_empty() {
                    bail!("review scope cannot be empty");
                }
                Some(scope)
            }
            None => None,
        };
        let expected_attestation_subject = review_scope.map(review_run_subject);
        if let Some(run) = self
            .recover_idempotent_run(
                owner_process_id,
                expected_attestation_subject.as_deref(),
                &selections,
            )
            .await?
        {
            return Ok(run);
        }

        let run_id = Uuid::new_v4().to_string();
        let run_subject = review_run_subject(&run_id);
        let attestation_subject = expected_attestation_subject
            .clone()
            .unwrap_or_else(|| run_subject.clone());
        let mut attempts = Vec::with_capacity(selections.len());
        for (ordinal, selection) in selections.iter().enumerate() {
            let attempt_id = Uuid::new_v4().to_string();
            let attempt_subject = reviewer_attempt_subject(&attempt_id);
            // Construction is the host-only schema gate. It rejects malformed
            // opaque subjects and non-wire-safe idempotency keys before DB or spawn.
            TrustedReviewProvenance::new(
                selection.binding.account_subject.clone(),
                selection.binding.model_family_subject.clone(),
                attestation_subject.clone(),
                attempt_subject.clone(),
                selection.idempotency_key.clone(),
            )?;
            attempts.push(ReviewerAttemptCreateParams {
                id: attempt_id,
                ordinal: i64::try_from(ordinal).context("too many selected reviewers")?,
                provider_id: selection.binding.provider_id.clone(),
                model: selection.binding.model.clone(),
                account_subject: selection.binding.account_subject.clone(),
                model_family_subject: selection.binding.model_family_subject.clone(),
                reviewer_attempt_subject: attempt_subject,
                idempotency_key: selection.idempotency_key.clone(),
                prompt: selection.prompt.clone(),
                mcp_server: selection.mcp_server.clone(),
                mcp_tool: selection.mcp_tool.clone(),
            });
        }

        let created = self
            .db
            .reviewer_orchestrations()
            .create_run(
                &ReviewRunCreateParams {
                    id: run_id,
                    review_run_subject: run_subject,
                    attestation_subject,
                    owner_process_id: owner_process_id.to_string(),
                },
                &attempts,
            )
            .await;
        match created {
            Ok(run) => Ok(run),
            Err(error) => match self
                .recover_idempotent_run(
                    owner_process_id,
                    expected_attestation_subject.as_deref(),
                    &selections,
                )
                .await
            {
                Ok(Some(run)) => Ok(run),
                Ok(None) => Err(error),
                Err(recovery_error) => {
                    Err(error.context(format!("idempotent recovery failed: {recovery_error:#}")))
                }
            },
        }
    }

    async fn recover_idempotent_run(
        &self,
        owner_process_id: &str,
        expected_attestation_subject: Option<&str>,
        selections: &[ReviewerSelection],
    ) -> anyhow::Result<Option<ReviewRun>> {
        let store = self.db.reviewer_orchestrations();
        let mut attempts = Vec::with_capacity(selections.len());
        for selection in selections {
            if let Some(attempt) = store
                .get_attempt_by_idempotency_key(&selection.idempotency_key)
                .await?
            {
                attempts.push(attempt);
            }
        }
        if attempts.is_empty() {
            return Ok(None);
        }
        if attempts.len() != selections.len() {
            bail!("review idempotency key was reused with a different review request");
        }

        let run_id = attempts[0].run_id.as_str();
        for (ordinal, (attempt, selection)) in attempts.iter().zip(selections).enumerate() {
            let ordinal =
                i64::try_from(ordinal).context("too many selected reviewers during recovery")?;
            if attempt.run_id != run_id
                || attempt.ordinal != ordinal
                || attempt.provider_id != selection.binding.provider_id
                || attempt.model != selection.binding.model
                || attempt.account_subject != selection.binding.account_subject
                || attempt.model_family_subject != selection.binding.model_family_subject
                || attempt.prompt != selection.prompt
                || attempt.mcp_server != selection.mcp_server
                || attempt.mcp_tool != selection.mcp_tool
            {
                bail!("review idempotency key was reused with a different review request");
            }
        }

        let run = store
            .get_run(run_id)
            .await?
            .context("persisted idempotent review run is missing")?;
        require_owner(&run, owner_process_id)?;
        if expected_attestation_subject.is_some_and(|expected| run.attestation_subject != expected)
        {
            bail!("review idempotency key was reused with a different review scope");
        }
        Ok(Some(run))
    }

    /// Continue every non-terminal attempt from its persisted state.
    ///
    /// An acknowledged attempt is never submitted again. A submission whose
    /// acknowledgement was lost remains SubmissionUnknown, so reconnect
    /// retries the exact persisted JSON with the exact persisted idempotency key.
    pub async fn resume_run(
        &self,
        owner_process_id: &str,
        run_id: &str,
    ) -> anyhow::Result<Vec<ReviewerAttempt>> {
        let store = self.db.reviewer_orchestrations();
        let run = store
            .get_run(run_id)
            .await?
            .with_context(|| format!("review run `{run_id}` not found"))?;
        require_owner(&run, owner_process_id)?;
        for attempt in store.list_attempts(run_id).await? {
            self.drive_attempt(&run, &attempt.id).await?;
        }
        store.list_attempts(run_id).await
    }

    /// Return owner-checked progress even when execution fails.
    pub(crate) async fn resume_progress(
        &self,
        owner_process_id: &str,
        run_id: &str,
    ) -> anyhow::Result<Value> {
        let store = self.db.reviewer_orchestrations();
        let run = store
            .get_run(run_id)
            .await?
            .context("review run not found")?;
        require_owner(&run, owner_process_id)?;
        match self.resume_run(owner_process_id, run_id).await {
            Ok(attempts) => Ok(progress_json(run_id, &attempts)),
            Err(_) => {
                // Expose durable failures, not raw boundary errors.
                let mut output = match store.list_attempts(run_id).await {
                    Ok(attempts) => progress_json(run_id, &attempts),
                    Err(_) => json!({
                        "run_id": run_id,
                        "terminal": false,
                        "acknowledged": false,
                        "state_unknown": true,
                        "attempts": []
                    }),
                };
                output["driver_error"] = json!("review_execution_failed");
                Ok(output)
            }
        }
    }

    pub async fn cancel_attempt(
        &self,
        owner_process_id: &str,
        attempt_id: &str,
        reason: &str,
    ) -> anyhow::Result<bool> {
        if reason.trim().is_empty() {
            bail!("reviewer cancellation reason cannot be empty");
        }
        let store = self.db.reviewer_orchestrations();
        let Some(attempt) = store.get_attempt(attempt_id).await? else {
            bail!("reviewer attempt `{attempt_id}` not found");
        };
        let run = store
            .get_run(&attempt.run_id)
            .await?
            .with_context(|| format!("review run `{}` not found", attempt.run_id))?;
        require_owner(&run, owner_process_id)?;
        if attempt.state == ReviewAttemptState::SubmissionUnknown {
            bail!("reviewer attempt cannot be cancelled after submission begins");
        }
        if matches!(
            attempt.state,
            ReviewAttemptState::Acknowledged | ReviewAttemptState::TerminalFailure
        ) {
            return Ok(false);
        }
        if attempt.state == ReviewAttemptState::Cancelled {
            if let Some(process_id) = attempt.process_id.as_deref() {
                self.boundary.cancel_reviewer(process_id).await?;
            }
            return Ok(false);
        }
        let cancelled = store
            .transition_attempt(
                attempt_id,
                attempt.state,
                ReviewAttemptState::Cancelled,
                &ReviewAttemptTransitionData {
                    failure: Some(reason.to_string()),
                    ..Default::default()
                },
            )
            .await?;
        if !cancelled {
            let current = store
                .get_attempt(attempt_id)
                .await?
                .with_context(|| format!("reviewer attempt `{attempt_id}` not found"))?;
            if current.state == ReviewAttemptState::SubmissionUnknown {
                bail!("reviewer attempt cannot be cancelled after submission begins");
            }
            if current.state != ReviewAttemptState::Cancelled {
                return Ok(false);
            }
        }
        if let Some(process_id) = attempt.process_id.as_deref() {
            self.boundary.cancel_reviewer(process_id).await?;
        }
        Ok(cancelled)
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
                        .spawn_reviewer(attempt_id, &expected, &attempt.prompt)
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
                            let _ = self.boundary.cancel_reviewer(process_id).await;
                        }
                        ReviewerOutput::Failed(reason) => {
                            self.fail_attempt(&attempt, reason.clone()).await?;
                            let _ = self.boundary.cancel_reviewer(process_id).await;
                            bail!("reviewer model execution failed: {reason}");
                        }
                    }
                }
                ReviewAttemptState::OutputParse => {
                    let raw_output = attempt.raw_output.as_deref().with_context(|| {
                        format!("output_parse attempt `{attempt_id}` has no raw output")
                    })?;
                    let submission = match prepare_submission(
                        &attempt.mcp_tool,
                        &attempt.idempotency_key,
                        raw_output,
                    ) {
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
                        run.attestation_subject.clone(),
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

fn require_owner(run: &ReviewRun, owner_process_id: &str) -> anyhow::Result<()> {
    if run.owner_process_id != owner_process_id {
        bail!("review run is owned by another process");
    }
    Ok(())
}

fn validate_diverse_selection(selections: &[ReviewerSelection]) -> anyhow::Result<()> {
    if selections.is_empty() {
        bail!("review run must select at least one reviewer");
    }
    let mut accounts = HashSet::with_capacity(selections.len());
    let mut families = HashSet::with_capacity(selections.len());
    let mut keys = HashSet::with_capacity(selections.len());
    for selection in selections {
        if selection.mcp_server.trim().is_empty() {
            bail!("attested review MCP server cannot be empty");
        }
        if selection.mcp_tool != REVIEW_VERDICT_TOOL {
            bail!("attested reviews can submit only through the review verdict capability");
        }
        if !accounts.insert(selection.binding.account_subject.as_str()) {
            bail!("duplicate credential subject in reviewer selection");
        }
        if !families.insert(selection.binding.model_family_subject.as_str()) {
            bail!("duplicate canonical model family in reviewer selection");
        }
        if !keys.insert(selection.idempotency_key.as_str()) {
            bail!("duplicate review idempotency key in reviewer selection");
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
    let strict: StrictReviewOutput = serde_json::from_str(raw_output).map_err(|error| {
        // Serde's Display may quote private field names or values. Expose only
        // content-free format/error categories to the orchestration supervisor.
        let text = raw_output.trim_start();
        let format = if text.is_empty() {
            "empty"
        } else if text.starts_with("```") || text.starts_with("~~~") {
            "fenced"
        } else if text.starts_with('{') || text.starts_with('[') {
            "json_shaped"
        } else {
            "non_json"
        };
        anyhow::anyhow!(
            "expected strict ReviewOutputEvent JSON (format={format}; category={:?})",
            error.classify()
        )
    })?;
    // Convert through the protocol type as a compatibility assertion: the
    // persisted payload is exactly what current ChaOS review consumers accept.
    let value = serde_json::to_value(strict)?;
    serde_json::from_value::<ReviewOutputEvent>(value.clone())
        .map_err(|_| anyhow::anyhow!("review output does not match the protocol schema"))?;
    Ok(value)
}

fn prepare_submission(
    mcp_tool: &str,
    idempotency_key: &str,
    raw_output: &str,
) -> anyhow::Result<Value> {
    let output = parse_strict_review_output(raw_output)?;
    if mcp_tool != REVIEW_VERDICT_TOOL {
        return Ok(output);
    }

    let strict: StrictReviewOutput = serde_json::from_value(output)?;
    let verdict = match strict.overall_correctness.trim() {
        "patch is correct" => "approve",
        "patch is incorrect" => "changes_requested",
        _ => bail!("overall_correctness must be `patch is correct` or `patch is incorrect`"),
    };
    let summary = strict.overall_explanation.trim();
    if summary.is_empty() {
        bail!("overall_explanation cannot be empty");
    }

    Ok(json!({
        "verdict": verdict,
        "summary": summary,
        "findings": {
            "format": "chaos.review_output.v1",
            "items": strict.findings,
            "overall_confidence_score": strict.overall_confidence_score
        },
        "idempotency_key": idempotency_key
    }))
}

pub(crate) async fn resolve_reviewer_binding(
    session: &crate::chaos::Session,
    turn: &crate::chaos::TurnContext,
    provider_id: &str,
    model: &str,
) -> anyhow::Result<ReviewerBinding> {
    let provider_id = provider_id.trim();
    let model = model.trim();
    if provider_id.is_empty() {
        bail!("review model provider cannot be empty");
    }
    if model.is_empty() {
        bail!("review model cannot be empty");
    }
    let provider = turn
        .config
        .model_providers
        .get(provider_id)
        .with_context(|| format!("unknown review model provider `{provider_id}`"))?;
    let available = session
        .services
        .models_manager
        .usable_cached_models_for_provider(provider_id, provider)
        .await
        .with_context(|| format!("review model provider `{provider_id}` is unavailable"))?;
    let selected = available
        .iter()
        .find(|preset| preset.model == model)
        .with_context(|| {
            let models = available
                .iter()
                .map(|preset| preset.model.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "unknown review model `{model}` for provider `{provider_id}`; available models: {models}"
            )
        })?;
    let account_subject = session
        .services
        .auth_manager
        .credential_subject_fingerprint_for_provider(
            provider_id,
            crate::review_provenance::REVIEW_ACCOUNT_SUBJECT_DOMAIN,
        )
        .map(crate::auth::CredentialSubjectFingerprint::into_string)
        .context("selected review provider has no attestable credential subject")?;
    let model_family_subject =
        crate::review_provenance::model_family_subject(&selected.model_family)
            .context("selected review model has no canonical model family")?;

    Ok(ReviewerBinding {
        provider_id: provider_id.to_string(),
        model: model.to_string(),
        account_subject,
        model_family_subject,
    })
}

pub(crate) fn build_reviewer_prompt(instructions: &str) -> anyhow::Result<String> {
    let instructions = instructions.trim();
    if instructions.is_empty() {
        bail!("review instructions cannot be empty");
    }
    Ok(format!(
        "{}\n\nREVIEW ASSIGNMENT:\n{instructions}",
        include_str!("../review_prompt.md").trim()
    ))
}

pub(crate) fn progress_json(run_id: &str, attempts: &[ReviewerAttempt]) -> Value {
    let terminal = attempts.iter().all(|attempt| attempt.state.is_terminal());
    let acknowledged = attempts
        .iter()
        .all(|attempt| attempt.state == ReviewAttemptState::Acknowledged);
    json!({
        "run_id": run_id,
        "terminal": terminal,
        "acknowledged": acknowledged,
        "attempts": attempts.iter().map(|attempt| json!({
            "attempt_id": attempt.id,
            "state": attempt.state.as_str(),
            "failure": attempt.failure
        })).collect::<Vec<_>>()
    })
}

struct PersistedReviewerState {
    model_provider: String,
    model: String,
    output: Option<String>,
}

async fn persisted_reviewer_state(
    process_id: chaos_ipc::ProcessId,
) -> anyhow::Result<PersistedReviewerState> {
    use chaos_ipc::protocol::EventMsg;

    let history = crate::RolloutRecorder::get_rollout_history_for_process(process_id)
        .await
        .context("persisted reviewer rollout could not be read")?;
    let events = history
        .get_event_msgs()
        .context("persisted reviewer rollout contains no events")?;
    let configured = events
        .iter()
        .find_map(|event| match event {
            EventMsg::SessionConfigured(configured) => Some(configured),
            _ => None,
        })
        .context("persisted reviewer rollout has no session configuration")?;
    let output = events.iter().rev().find_map(|event| match event {
        EventMsg::TurnComplete(event) => event.last_agent_message.clone(),
        _ => None,
    });
    Ok(PersistedReviewerState {
        model_provider: configured.model_provider_id.clone(),
        model: configured.model.clone(),
        output,
    })
}

/// Production adapter for exact provider/model spawning, live or persisted
/// output recovery, and host-attested MCP submission.
///
/// Construction stays kernel-private because Session and TurnContext are
/// kernel capabilities, while the orchestration API above remains testable at
/// a real boundary without exposing those capabilities.
#[derive(Clone)]
pub(crate) struct SessionReviewerBoundary {
    session: std::sync::Arc<crate::chaos::Session>,
    turn: std::sync::Arc<crate::chaos::TurnContext>,
}

fn attested_reviewer_spawn_depth(session_source: &chaos_ipc::protocol::SessionSource) -> i32 {
    // This is a kernel-controlled, single-purpose reviewer rather than generic
    // delegation. Its configuration disables collaboration before the process
    // starts, so it cannot use this exemption to create another generation.
    crate::minions::next_process_spawn_depth(session_source)
}

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

    async fn reviewer_from_spawned(
        &self,
        spawned: crate::minions::control::SpawnedAgent,
    ) -> anyhow::Result<SpawnedReviewer> {
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
}

impl ReviewerBoundary for SessionReviewerBoundary {
    async fn spawn_reviewer(
        &self,
        attempt_id: &str,
        binding: &ReviewerBinding,
        prompt: &str,
    ) -> anyhow::Result<SpawnedReviewer> {
        use chaos_ipc::config_types::WebSearchMode;
        use chaos_ipc::permissions::SocketPolicy;
        use chaos_ipc::permissions::VfsPolicy;
        use chaos_ipc::protocol::ApprovalPolicy;
        use chaos_ipc::protocol::SandboxPolicy;
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

        let agent_role = crate::minions::internal_agent_role("attested-review", attempt_id);
        if let Some(spawned) = self
            .session
            .services
            .agent_control
            .find_direct_child_by_role(self.session.conversation_id, &agent_role)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?
        {
            return self.reviewer_from_spawned(spawned).await;
        }

        let runtime_db = self
            .session
            .services
            .runtime_db
            .as_ref()
            .context("reviewer orchestration requires a runtime database")?;
        let persisted_processes = runtime_db
            .find_process_ids_by_parent_and_role(self.session.conversation_id, &agent_role)
            .await?;
        if persisted_processes.len() > 1 {
            bail!("multiple persisted reviewer processes use role `{agent_role}`");
        }
        let persisted_process_id = persisted_processes.first().copied();
        if let Some(process_id) = persisted_process_id {
            let persisted = persisted_reviewer_state(process_id).await?;
            if persisted.model_provider != binding.provider_id || persisted.model != binding.model {
                bail!("persisted reviewer effective provider/model does not match selection");
            }
            if persisted.output.is_some() {
                return Ok(SpawnedReviewer {
                    process_id: process_id.to_string(),
                    effective_binding: binding.clone(),
                });
            }
        }

        let child_depth = attested_reviewer_spawn_depth(&self.turn.session_source);
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
        config.collab_enabled = false;
        config.minion_jobs_allowed = false;
        let final_output_json_schema = config
            .model_providers
            .get(&binding.provider_id)
            .filter(|provider| {
                !crate::model_provider_info::is_anthropic_wire(provider.base_url.as_deref())
            })
            .map(|_| crate::tasks::review_output_schema());
        config
            .permissions
            .approval_policy
            .set(ApprovalPolicy::Headless)
            .map_err(anyhow::Error::msg)?;
        config
            .permissions
            .sandbox_policy
            .set(SandboxPolicy::new_read_only_policy())
            .map_err(anyhow::Error::msg)?;
        config.permissions.vfs_policy = VfsPolicy::default();
        config.permissions.socket_policy = SocketPolicy::Restricted;
        config
            .web_search_mode
            .set(WebSearchMode::Disabled)
            .map_err(anyhow::Error::msg)?;
        config
            .mcp_servers
            .set(Default::default())
            .map_err(anyhow::Error::msg)?;
        config.mode_policy_override = Some(
            self.session
                .child_mode_policy(
                    self.turn.as_ref(),
                    Some("default"),
                    /*allowed_modes*/ None,
                    /*allow_mode_switching*/ Some(false),
                )
                .await
                .map_err(anyhow::Error::msg)?,
        );
        let session_source = SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
            parent_process_id: self.session.conversation_id,
            depth: child_depth,
            agent_nickname: None,
            agent_role: Some(agent_role.clone()),
        });
        let options = crate::minions::control::SpawnAgentOptions {
            suppress_parent_completion_notification: true,
            final_output_json_schema,
            ..Default::default()
        };
        let spawned = if let Some(process_id) = persisted_process_id {
            let process_id = self
                .session
                .services
                .agent_control
                .resume_agent_from_rollout_with_options(config, process_id, session_source, options)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let provenance = self
                .session
                .services
                .agent_control
                .effective_spawn_provenance(process_id)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            crate::minions::control::SpawnedAgent {
                process_id,
                provenance,
            }
        } else {
            match self
                .session
                .services
                .agent_control
                .spawn_agent_with_options(
                    config,
                    vec![UserInput::Text {
                        text: prompt.to_string(),
                        text_elements: Vec::new(),
                    }],
                    Some(session_source),
                    options,
                )
                .await
            {
                Ok(spawned) => spawned,
                Err(error) => {
                    if let Some(spawned) = self
                        .session
                        .services
                        .agent_control
                        .find_direct_child_by_role(self.session.conversation_id, &agent_role)
                        .await
                        .map_err(|find_error| anyhow::anyhow!("{find_error}"))?
                    {
                        spawned
                    } else {
                        return Err(anyhow::anyhow!("{error}"));
                    }
                }
            }
        };
        self.reviewer_from_spawned(spawned).await
    }

    async fn reviewer_output(&self, process_id: &str) -> anyhow::Result<ReviewerOutput> {
        use crate::minions::AgentStatus;
        use chaos_ipc::ProcessId;
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
            AgentStatus::NotFound => persisted_reviewer_state(process_id)
                .await?
                .output
                .map_or_else(
                    || {
                        Ok(ReviewerOutput::Failed(
                            "reviewer is not live and its rollout has no completed output"
                                .to_string(),
                        ))
                    },
                    |output| Ok(ReviewerOutput::Completed(output)),
                ),
        }
    }

    async fn submit_review(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        provenance: TrustedReviewProvenance,
    ) -> anyhow::Result<SubmissionOutcome> {
        if server.trim().is_empty() {
            bail!("attested review MCP server cannot be empty");
        }
        if tool != REVIEW_VERDICT_TOOL {
            bail!("attested reviews can submit only through the review verdict capability");
        }
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
        use crate::minions::AgentStatus;

        let process_id =
            chaos_ipc::ProcessId::from_string(process_id).context("invalid reviewer process id")?;
        if matches!(
            self.session
                .services
                .agent_control
                .get_status(process_id)
                .await,
            AgentStatus::NotFound | AgentStatus::Shutdown
        ) {
            return Ok(());
        }
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
mod tests;
