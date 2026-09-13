use super::*;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::SubAgentSource;
use chaos_proc::StateRuntime;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[test]
fn invalid_output_diagnostics_are_content_free() {
    for (raw, format) in [
        (" \n", "empty"),
        ("```json\nprivate-marker\n```", "fenced"),
        ("private-marker", "non_json"),
        (r#"{"private-marker":"private-marker"}"#, "json_shaped"),
    ] {
        let error = parse_strict_review_output(raw).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains(&format!("format={format};")));
        assert!(!message.contains("private-marker"));
    }
    let raw = valid_output();
    assert!(parse_strict_review_output(&raw).is_ok());
    assert!(parse_strict_review_output(&format!("```json\n{raw}\n```")).is_err());
    let mut output: Value = serde_json::from_str(&raw).unwrap();
    output["overall_correctness"] = json!("private-marker");
    let error =
        prepare_submission(REVIEW_VERDICT_TOOL, "test-key", &output.to_string()).unwrap_err();
    assert!(!format!("{error:#}").contains("private-marker"));
}

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
        _attempt_id: &str,
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

#[test]
fn attested_reviewer_can_cross_the_generic_delegation_depth_boundary() {
    let supervisor = SessionSource::SubAgent(SubAgentSource::ProcessSpawn {
        parent_process_id: ProcessId::new(),
        depth: 1,
        agent_nickname: None,
        agent_role: None,
    });

    let reviewer_depth = attested_reviewer_spawn_depth(&supervisor);

    assert_eq!(reviewer_depth, 2);
    assert!(crate::minions::exceeds_process_spawn_depth_limit(
        reviewer_depth,
        1
    ));
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
        mcp_server: "review-service".to_string(),
        mcp_tool: REVIEW_VERDICT_TOOL.to_string(),
        idempotency_key: format!("review-{index}"),
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

const OWNER: &str = "owner-process";

#[tokio::test]
async fn diverse_fake_provider_accounts_complete_with_verified_bindings() {
    let db = database().await;
    let boundary = FakeBoundary::default();
    let orchestrator = ReviewerOrchestrator::new(db, boundary.clone());
    let run = orchestrator
        .start_run(
            OWNER,
            None,
            vec![selection(0, 'a', 'b'), selection(1, 'c', 'd')],
        )
        .await
        .unwrap();
    let attempts = orchestrator.resume_run(OWNER, &run.id).await.unwrap();

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
async fn failed_execution_keeps_public_progress_and_exact_replay() {
    let db = database().await;
    let boundary = FakeBoundary::default();
    boundary.state.lock().await.outputs.insert(
        "next".into(),
        ReviewerOutput::Failed("reviewer completed without output".into()),
    );
    let orchestrator = ReviewerOrchestrator::new(db, boundary.clone());
    let selected = selection(0, 'a', 'b');
    let run = orchestrator
        .start_run(OWNER, None, vec![selected.clone()])
        .await
        .unwrap();
    let progress = orchestrator.resume_progress(OWNER, &run.id).await.unwrap();
    assert_eq!(progress["run_id"], run.id);
    assert_eq!(progress["terminal"], true);
    assert_eq!(progress["acknowledged"], false);
    assert_eq!(progress["driver_error"], "review_execution_failed");
    assert_eq!(progress["attempts"][0]["state"], "terminal_failure");
    assert!(
        orchestrator
            .resume_progress("another-owner", &run.id)
            .await
            .is_err()
    );
    let replay = orchestrator
        .start_run(OWNER, None, vec![selected])
        .await
        .unwrap();
    assert_eq!(replay.id, run.id);
    let recovered = orchestrator.resume_progress(OWNER, &run.id).await.unwrap();
    assert_eq!(recovered["attempts"], progress["attempts"]);
    let state = boundary.state.lock().await;
    assert_eq!(state.spawn_calls.len(), 1);
    assert!(state.submit_keys.is_empty());
}

#[tokio::test]
async fn exact_start_replay_recovers_the_persisted_run() {
    let db = database().await;
    let orchestrator = ReviewerOrchestrator::new(db.clone(), FakeBoundary::default());
    let selected = selection(0, 'a', 'b');
    let created = orchestrator
        .start_run(OWNER, None, vec![selected.clone()])
        .await
        .unwrap();

    let replayed = orchestrator
        .start_run(OWNER, None, vec![selected])
        .await
        .unwrap();

    assert_eq!(replayed, created);
    assert_eq!(
        db.reviewer_orchestrations()
            .list_attempts(&created.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn independent_runs_share_one_scoped_attestation_subject() {
    let db = database().await;
    let orchestrator = ReviewerOrchestrator::new(db, FakeBoundary::default());
    let first = orchestrator
        .start_run(
            "owner-one",
            Some("review-round-1"),
            vec![selection(0, 'a', 'b')],
        )
        .await
        .unwrap();
    let second = orchestrator
        .start_run(
            "owner-two",
            Some("review-round-1"),
            vec![selection(1, 'c', 'd')],
        )
        .await
        .unwrap();

    assert_ne!(first.id, second.id);
    assert_ne!(first.review_run_subject, second.review_run_subject);
    assert_eq!(first.attestation_subject, second.attestation_subject);
}

#[tokio::test]
async fn start_replay_rejects_changed_content() {
    let db = database().await;
    let orchestrator = ReviewerOrchestrator::new(db, FakeBoundary::default());
    let selected = selection(0, 'a', 'b');
    orchestrator
        .start_run(OWNER, None, vec![selected.clone()])
        .await
        .unwrap();
    let mut changed = selected;
    changed.prompt.push_str(" with changed criteria");

    let error = orchestrator
        .start_run(OWNER, None, vec![changed])
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("reused with a different review request")
    );
}

#[tokio::test]
async fn start_replay_rejects_changed_review_scope() {
    let db = database().await;
    let orchestrator = ReviewerOrchestrator::new(db, FakeBoundary::default());
    let selected = selection(0, 'a', 'b');
    orchestrator
        .start_run(OWNER, Some("review-round-1"), vec![selected.clone()])
        .await
        .unwrap();

    let error = orchestrator
        .start_run(OWNER, Some("review-round-2"), vec![selected])
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("reused with a different review scope")
    );
}

#[tokio::test]
async fn duplicate_credential_is_rejected_before_spawn_or_submission() {
    let db = database().await;
    let boundary = FakeBoundary::default();
    let orchestrator = ReviewerOrchestrator::new(db, boundary.clone());
    let error = orchestrator
        .start_run(
            OWNER,
            None,
            vec![selection(0, 'a', 'b'), selection(1, 'a', 'c')],
        )
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
        .start_run(OWNER, None, vec![selection(0, 'a', 'b')])
        .await
        .unwrap();
    let error = orchestrator.resume_run(OWNER, &run.id).await.unwrap_err();

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
        .start_run(OWNER, None, vec![selection(0, 'a', 'b')])
        .await
        .unwrap();

    let error = orchestrator.resume_run(OWNER, &run.id).await.unwrap_err();
    assert!(error.to_string().contains("acknowledgement unknown"));
    let unknown = &db
        .reviewer_orchestrations()
        .list_attempts(&run.id)
        .await
        .unwrap()[0];
    assert_eq!(unknown.state, ReviewAttemptState::SubmissionUnknown);
    let exact_key = unknown.idempotency_key.clone();
    let exact_payload = unknown.submission.clone();

    let attempts = orchestrator.resume_run(OWNER, &run.id).await.unwrap();
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
        .start_run(OWNER, None, vec![selection(0, 'a', 'b')])
        .await
        .unwrap();
    let error = orchestrator.resume_run(OWNER, &run.id).await.unwrap_err();

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

#[tokio::test]
async fn run_is_fenced_to_its_owner_process() {
    let db = database().await;
    let orchestrator = ReviewerOrchestrator::new(db, FakeBoundary::default());
    let run = orchestrator
        .start_run(OWNER, None, vec![selection(0, 'a', 'b')])
        .await
        .unwrap();

    let error = orchestrator
        .resume_run("different-process", &run.id)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("another process"));
}

#[tokio::test]
async fn owner_can_cancel_its_pending_attempt() {
    let db = database().await;
    let boundary = FakeBoundary::default();
    boundary
        .state
        .lock()
        .await
        .outputs
        .insert("next".to_string(), ReviewerOutput::Pending);
    let orchestrator = ReviewerOrchestrator::new(db, boundary.clone());
    let run = orchestrator
        .start_run(OWNER, None, vec![selection(0, 'a', 'b')])
        .await
        .unwrap();
    let attempts = orchestrator.resume_run(OWNER, &run.id).await.unwrap();
    let attempt_id = attempts[0].id.clone();
    assert!(
        progress_json(&run.id, &attempts)["attempts"][0]
            .get("process_id")
            .is_none()
    );

    assert!(
        orchestrator
            .cancel_attempt(OWNER, &attempt_id, "review timed out")
            .await
            .unwrap()
    );
    let attempts = orchestrator.resume_run(OWNER, &run.id).await.unwrap();
    assert_eq!(attempts[0].state, ReviewAttemptState::Cancelled);
    assert_eq!(boundary.state.lock().await.cancelled, vec!["process-1"]);
}

#[test]
fn verdict_submission_maps_strict_review_output_and_reuses_exact_key() {
    let submission =
        prepare_submission(REVIEW_VERDICT_TOOL, "stable-verdict-key", &valid_output()).unwrap();

    assert_eq!(submission["verdict"], "approve");
    assert_eq!(submission["summary"], "No findings.");
    assert_eq!(submission["idempotency_key"], "stable-verdict-key");
    assert_eq!(submission["findings"]["format"], "chaos.review_output.v1");
    assert!(
        submission["findings"]["overall_confidence_score"]
            .as_f64()
            .is_some_and(|confidence| (confidence - 0.98).abs() < 0.000_001)
    );
}

#[test]
fn verdict_submission_rejects_ambiguous_correctness() {
    let raw = json!({
        "findings": [],
        "overall_correctness": "probably fine",
        "overall_explanation": "Ambiguous.",
        "overall_confidence_score": 0.5
    })
    .to_string();

    let error = prepare_submission(REVIEW_VERDICT_TOOL, "stable-key", &raw).unwrap_err();

    assert!(error.to_string().contains("overall_correctness"));
}
