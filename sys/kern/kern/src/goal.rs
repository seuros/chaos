//! Durable projection and trusted evidence for driver-owned goals.

mod evaluator;
mod evidence;
mod journal;
#[cfg(test)]
mod tests;

use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::{BackgroundEventEvent, EventMsg, WarningEvent};
use chaos_mcp_protocol::goals::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::chaos::{Session, TurnContext};

#[derive(Default)]
pub(crate) struct GoalGate {
    turn_id: String,
    user_request: String,
    request_truncated: bool,
    evidence: evidence::Evidence,
    fence: Option<String>,
    initial_input: bool,
    permit: Option<Permit>,
    journal: Arc<Mutex<journal::Journal>>,
}

#[derive(Clone)]
struct Permit {
    token: String,
    server: String,
    turn_id: String,
    tool: String,
    arguments: Value,
}

struct Checkpoint {
    state: Value,
    criteria_count: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Complete,
    Incomplete(Vec<String>),
    Unknown(&'static str),
}

pub(crate) enum Outcome {
    None,
    Continue(String),
    Complete(String),
    Paused(String),
}

impl Outcome {
    pub(crate) fn follow_up_prompt(&self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Continue(message) => Some(message.clone()),
            Self::Complete(message) => Some(format!(
                "{message} The goal is complete, but this turn remains open. \
                 Respond to the user with the result and limitations. Do not recheck unchanged evidence."
            )),
            Self::Paused(message) => Some(format!(
                "{message} This turn remains open for your response. Explain the blocker; \
                 do not claim verified completion or restart this goal."
            )),
        }
    }

    pub(crate) fn status_event(&self) -> Option<EventMsg> {
        match self {
            Self::None => None,
            Self::Continue(_) => Some(EventMsg::BackgroundEvent(BackgroundEventEvent {
                message: "Goal check: more work is needed.".into(),
            })),
            Self::Complete(message) => Some(EventMsg::BackgroundEvent(BackgroundEventEvent {
                message: message.clone(),
            })),
            Self::Paused(message) => Some(EventMsg::Warning(WarningEvent {
                message: message.clone(),
            })),
        }
    }
}

fn outcome(goal: &Snapshot) -> Outcome {
    match goal.status {
        Status::Active => Outcome::Continue(format!(
            "Goal check did not pass: {}. Continue within the original scope, gather new \
             evidence, then check again. {} checks remain. Pause if blocked.",
            goal.reason,
            MAX_CHECKS.saturating_sub(goal.checks),
        )),
        Status::Complete => Outcome::Complete(
            "Goal complete: Jev accepted the available evidence (not a correctness guarantee)."
                .into(),
        ),
        Status::Paused => Outcome::Paused(format!(
            "Goal paused: {}; completion is unverified.",
            goal.reason,
        )),
        Status::Checking => Outcome::None,
    }
}

fn bounded(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        text.to_owned()
    } else {
        let marker = " [truncated]";
        let end = text.floor_char_boundary(bytes.saturating_sub(marker.len()));
        format!("{}{marker}", &text[..end])
    }
}

impl GoalGate {
    fn release_permit(&mut self, token: &str) {
        if self
            .permit
            .as_ref()
            .is_some_and(|permit| permit.token == token)
        {
            self.permit = None;
        }
    }

    pub(crate) fn begin_turn(&mut self, turn_id: &str, user_request: &str) {
        self.turn_id = turn_id.into();
        self.request_truncated = user_request.len() > 16_384;
        self.user_request = bounded(user_request, 16_384);
        self.evidence = Default::default();
        self.fence = None;
        self.initial_input = true;
        self.permit = None;
    }

    pub(crate) fn record(&mut self, item: &ResponseItem) {
        if self.turn_id.is_empty() {
            return;
        }
        if matches!(item, ResponseItem::Message { role, .. } if role == "user") {
            if self.initial_input {
                self.initial_input = false;
            } else {
                self.pause("user_input_changed");
            }
        }
        self.evidence.record(item);
    }

    pub(crate) fn pause(&mut self, reason: &str) {
        self.fence = Some(reason.into());
        self.permit = None;
    }

    pub(crate) fn end_turn(&mut self, turn_id: &str, reason: &str) {
        if self.turn_id == turn_id {
            self.pause(reason);
            self.turn_id.clear();
            self.user_request.clear();
            self.evidence = Default::default();
        }
    }

    fn context(&self) -> Context {
        Context {
            turn_id: self.turn_id.clone(),
            eligible: !self.turn_id.is_empty()
                && !self.user_request.trim().is_empty()
                && !self.request_truncated
                && self.fence.is_none(),
            pause_reason: self.fence.clone(),
            evidence_digest: self.evidence.digest(),
            has_evidence: !self.evidence.is_empty(),
        }
    }
}

pub(crate) async fn authorize(
    sess: &Session,
    turn: &TurnContext,
    server: &str,
    tool: &str,
    arguments: Value,
) -> String {
    let token = uuid::Uuid::new_v4().to_string();
    if tool == "start_goal" && crate::reflex::action_risk_settings(&turn.config).is_none() {
        sess.state.lock().await.goal.permit = None;
        return token;
    }
    sess.state.lock().await.goal.permit = Some(Permit {
        token: token.clone(),
        server: server.into(),
        turn_id: turn.sub_id.clone(),
        tool: tool.into(),
        arguments,
    });
    token
}

pub(crate) async fn finish_call(
    sess: &Session,
    turn: &TurnContext,
    server: &str,
    tool: &str,
    token: &str,
) {
    let journal = {
        let mut state = sess.state.lock().await;
        state.goal.release_permit(token);
        Arc::clone(&state.goal.journal)
    };
    let guard = journal.lock().await;
    if tool == "check_goal"
        && guard.server.as_deref() == Some(server)
        && let Some(goal) = &guard.goal
        && let Some(event) = outcome(goal).status_event()
    {
        drop(guard);
        sess.send_event(turn, event).await;
    }
}

pub(crate) async fn request(
    sess: &Session,
    server: String,
    endpoint: String,
    connection: u64,
    params: Value,
) -> Result<Value, String> {
    let request = Request::parse(params)?;
    let journal = Arc::clone(&sess.state.lock().await.goal.journal);
    let mut guard = journal.lock().await;
    guard.reconcile(sess).await?;
    guard.bind(
        &server,
        &endpoint,
        connection,
        matches!(request.operation, Operation::Restore),
    )?;
    match request.operation {
        Operation::Restore => {
            let context = sess.state.lock().await.goal.context();
            Ok(json!(Restored {
                version: VERSION,
                goal: guard.goal.clone(),
                context,
                interrupted: guard.interrupted,
            }))
        }
        Operation::Checkpoint {
            expected_revision,
            goal,
        } => {
            if guard.goal.as_ref() == Some(&goal)
                && expected_revision.checked_add(1) == Some(goal.revision)
            {
                return Ok(json!(goal));
            }
            let state = sess.state.lock().await;
            guard.validate(&state.goal, &server, expected_revision, &goal)?;
            guard
                .commit(sess, &server, &endpoint, goal.clone(), None)
                .await?;
            drop(state);
            Ok(json!(goal))
        }
        Operation::Evaluate { revision, claim } => {
            if guard.interrupted {
                return Err("interrupted goals must pause before further work".into());
            }
            let goal = guard.goal.clone().ok_or("no goal")?;
            if goal.revision != revision || goal.status != Status::Checking {
                return Err("stale goal evaluation".into());
            }
            if let Some(verdict) = guard.verdict.as_ref().filter(|v| v.revision == revision) {
                return Ok(json!(verdict));
            }
            if guard.evaluating.is_some() {
                return Err("goal evaluation already in progress".into());
            }
            let (turn, cancellation) = sess
                .active_turn
                .lock()
                .await
                .as_ref()
                .and_then(|active| {
                    active
                        .tasks
                        .values()
                        .find(|task| task.turn_context.sub_id == goal.turn_id)
                        .map(|task| {
                            (
                                Arc::clone(&task.turn_context),
                                task.cancellation_token.clone(),
                            )
                        })
                })
                .ok_or("goal turn is no longer active")?;
            let state = sess.state.lock().await;
            let gate = &state.goal;
            let context = gate.context();
            if !context.eligible
                || context.turn_id != goal.turn_id
                || Some(&context.evidence_digest) != goal.evidence_digest.as_ref()
            {
                return Err("goal evidence or turn changed".into());
            }
            let snapshot = Checkpoint {
                criteria_count: goal.criteria.len(),
                state: json!({
                    "original_user_request":gate.user_request, "objective":goal.objective,
                    "criteria":goal.criteria, "evidence_digest":context.evidence_digest,
                    "host_recorded_tool_evidence":gate.evidence.entries(),
                    "evidence_truncated":gate.evidence.truncated, "worker_claim_not_evidence":claim,
                }),
            };
            drop(state);
            guard.evaluating = Some(revision);
            drop(guard);
            let verdict = tokio::select! {
                _ = cancellation.cancelled() => Verdict::Unknown("cancelled"),
                verdict = evaluator::evaluate(&turn, &snapshot) => verdict,
            };
            let mut guard = journal.lock().await;
            if guard.evaluating == Some(revision) {
                guard.evaluating = None;
            }
            guard.bind(&server, &endpoint, connection, false)?;
            let state = sess.state.lock().await;
            let context = state.goal.context();
            if cancellation.is_cancelled()
                || guard.goal.as_ref() != Some(&goal)
                || !context.eligible
                || context.turn_id != goal.turn_id
                || Some(&context.evidence_digest) != goal.evidence_digest.as_ref()
            {
                return Err("stale goal verdict".into());
            }
            let verdict = VerdictRecord {
                revision,
                attempt_id: goal.attempt_id.clone().ok_or("missing attempt")?,
                evidence_digest: context.evidence_digest,
                reference: uuid::Uuid::new_v4().to_string(),
                evaluation: match verdict {
                    Verdict::Complete => Evaluation::Complete,
                    Verdict::Incomplete(missing) => Evaluation::Incomplete { missing },
                    Verdict::Unknown(reason) => Evaluation::Unknown {
                        reason: reason.into(),
                    },
                },
            };
            guard
                .commit(sess, &server, &endpoint, goal, Some(verdict.clone()))
                .await?;
            drop(state);
            Ok(json!(verdict))
        }
    }
}

pub(crate) async fn recover(
    sess: &Session,
    items: &[chaos_ipc::protocol::RolloutItem],
) -> Result<(), String> {
    let journal = Arc::clone(&sess.state.lock().await.goal.journal);
    journal.lock().await.restore(sess.conversation_id, items)
}

pub(crate) async fn checkpoint(sess: &Session, turn: &TurnContext, claim: &str) -> Outcome {
    if !sess.state.lock().await.goal.context().eligible {
        return Outcome::None;
    }
    let journal = Arc::clone(&sess.state.lock().await.goal.journal);
    let guard = journal.lock().await;
    let Some(goal) = guard.goal.as_ref().filter(|g| g.turn_id == turn.sub_id) else {
        return Outcome::None;
    };
    if goal.status == Status::Paused {
        return Outcome::None;
    }
    if goal.status == Status::Complete
        && goal.evidence_digest.as_deref() == Some(&sess.state.lock().await.goal.evidence.digest())
    {
        return Outcome::None;
    }
    let Some(server) = guard.server.clone() else {
        return Outcome::None;
    };
    drop(guard);
    let arguments = json!({"claim":bounded(claim, 8192)});
    let token = authorize(sess, turn, &server, "check_goal", arguments.clone()).await;
    let result = sess
        .call_tool(&server, "check_goal", Some(arguments), None)
        .await;
    {
        let mut state = sess.state.lock().await;
        state.goal.release_permit(&token);
    }
    if result.is_err() || result.is_ok_and(|r| r.is_error == Some(true)) {
        sess.state.lock().await.goal.pause("driver_unavailable");
        return Outcome::Paused("Goal check unavailable; completion is unverified.".into());
    }
    let guard = journal.lock().await;
    guard.goal.as_ref().map(outcome).unwrap_or(Outcome::None)
}
