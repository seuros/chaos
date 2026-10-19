//! Live run state owned by exactly one authenticated/native session.
//!
//! Keep a registry on the session, never in a process-global tool catalog.
//! Possession of a run ID conveys no authority across registries. Dropping the
//! last registry cancels owned work; there is deliberately no restore API.

mod budget;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::tree::MAX_LEAF_ATTEMPTS;

/// Maximum simultaneously running/cancelling runs per session.
pub const MAX_ACTIVE_RUNS: usize = 4;
/// Maximum retained terminal snapshots per session.
pub const MAX_TERMINAL_RUNS: usize = 128;
/// Retention duration for terminal snapshots.
pub const TERMINAL_RETENTION: Duration = Duration::from_secs(60 * 60);
/// Serialized snapshot limit, including metadata.
pub const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;

/// An opaque, live-only identifier. It is not a bearer credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(Uuid);

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Execution surface, sharing the same session budget and status APIs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    /// A tree of authorized tool calls and interactions.
    Workflow,
    /// A tree of explicitly authorized agent jobs.
    Synopsis,
}

/// Terminal result, supplied only after the executor has cleaned up children.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// The root succeeded.
    Success,
    /// The root failed normally.
    Failure,
    /// Cancellation completed.
    Cancelled,
    /// The outer run deadline expired.
    TimedOut,
    /// The executor reported an error.
    Error,
    /// The executor future was abandoned without recording an outcome.
    Interrupted,
}

/// Current lifecycle state. Cancellation requests are not false completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "outcome", rename_all = "snake_case")]
pub enum RunStatus {
    /// The executor is active.
    Running,
    /// Cancellation was requested; cleanup may still be running.
    Cancelling,
    /// The executor terminated.
    Terminal(RunOutcome),
}

/// Terminal leaf-attempt result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    /// The attempt succeeded.
    Success,
    /// The attempt failed.
    Failure,
    /// The attempt was halted.
    Cancelled,
}

/// Leaf-attempt lifecycle, including pending user/model interaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "outcome", rename_all = "snake_case")]
pub enum StepStatus {
    /// Work started.
    Running,
    /// Waiting on existing user/model interaction paths.
    WaitingForInput,
    /// Work ended.
    Terminal(StepOutcome),
}

/// One attempt, in launch order rather than completion order.
#[derive(Clone, Debug, Serialize)]
pub struct StepSnapshot {
    /// Stable declarative node ID.
    pub node_id: String,
    /// One-based activation number for this node.
    pub attempt: usize,
    /// Current attempt state.
    pub status: StepStatus,
    /// Structured output or failure diagnostic, already subject to output policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    /// Detail was discarded to respect the snapshot budget.
    pub truncated: bool,
}

/// Owned, bounded snapshot. No snapshot can authorize replay after restart.
#[derive(Clone, Debug, Serialize)]
pub struct RunSnapshot {
    /// Live-only run identifier.
    pub run_id: RunId,
    /// Workflow or synopsis.
    pub kind: RunKind,
    /// Current run lifecycle state.
    pub status: RunStatus,
    /// Bounded per-attempt records.
    pub steps: Vec<StepSnapshot>,
    /// Optional bounded root result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// At least one detail/root result was discarded.
    pub truncated: bool,
}

/// Metadata-only bounded page. Details remain available through owned status.
#[derive(Clone, Debug, Serialize)]
pub struct RunSummaryPage {
    /// At most ten snapshots with payload details omitted.
    pub runs: Vec<RunSnapshot>,
    /// Next offset, if more retained runs exist. Pages are live, not a replay log.
    pub next_offset: Option<usize>,
    /// This endpoint deliberately omits all output payloads.
    pub details_omitted: bool,
}

/// Registry/lifecycle rejection. Errors do not reveal other sessions' runs.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RunError {
    /// Shared workflow/synopsis active-run budget exhausted.
    #[error("session already has four active runs")]
    ActiveLimit,
    /// Unknown, expired, another session's, or lost on restart.
    #[error("unknown run `{0}`; expired or interrupted runs are never restored")]
    UnknownRun(RunId),
    /// The owning session no longer exists.
    #[error("owning session has closed")]
    SessionClosed,
    /// A terminal run cannot be reopened.
    #[error("run is already terminal")]
    AlreadyTerminal,
    /// No new attempts may start after cancellation.
    #[error("run cancellation has been requested")]
    Cancelling,
    /// Attempt count or identifier is outside the bounded contract.
    #[error("invalid or excessive step attempts")]
    AttemptLimit,
    /// An attempt cannot be reopened or repeated while still active.
    #[error("step attempt is already present or an earlier attempt is active")]
    InvalidTransition,
    /// No attempt with this node/number was started.
    #[error("step attempt is unknown")]
    UnknownStep,
    /// Cleanup must finish before a normal terminal outcome is recorded.
    #[error("run still has active step attempts")]
    ActiveSteps,
}

/// Cloneable handle bound to one session. Clones share limits and ownership.
#[derive(Clone, Default)]
pub struct RunRegistry {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    runs: BTreeMap<RunId, Entry>,
    terminal_order: VecDeque<RunId>,
    closed: bool,
}

struct Entry {
    snapshot: RunSnapshot,
    cancellation: CancellationToken,
    finished: Option<Instant>,
    budget: budget::PayloadBudget,
}

/// Single executor lease. Dropping it cancels work and marks the run
/// interrupted. Its weak reference does not keep a closed session alive.
pub struct RunLease {
    id: RunId,
    registry: Weak<Inner>,
    cancellation: CancellationToken,
    armed: bool,
}

/// Cloneable attempt recorder for scoped SDK observers and native adapters.
/// It cannot complete a run, retain its session, or replace its executor lease.
/// Dropping the lease still interrupts the run even while recorders exist.
#[derive(Clone)]
pub struct RunRecorder {
    id: RunId,
    registry: Weak<Inner>,
}

impl RunRegistry {
    /// At most ten metadata-only entries, ordered by live run ID. Never clone
    /// large payloads merely to discard them for a dashboard response.
    pub fn summaries(&self, offset: usize) -> RunSummaryPage {
        let mut state = self.inner.lock();
        state.purge();
        let runs = state
            .runs
            .values()
            .skip(offset)
            .take(10)
            .map(|entry| {
                let snapshot = &entry.snapshot;
                RunSnapshot {
                    run_id: snapshot.run_id,
                    kind: snapshot.kind,
                    status: snapshot.status,
                    steps: snapshot
                        .steps
                        .iter()
                        .map(|step| StepSnapshot {
                            node_id: step.node_id.clone(),
                            attempt: step.attempt,
                            status: step.status,
                            detail: None,
                            truncated: step.truncated || step.detail.is_some(),
                        })
                        .collect(),
                    result: None,
                    truncated: snapshot.truncated
                        || snapshot.result.is_some()
                        || snapshot.steps.iter().any(|step| step.detail.is_some()),
                }
            })
            .collect::<Vec<_>>();
        let next = offset.saturating_add(runs.len());
        RunSummaryPage {
            runs,
            next_offset: (next < state.runs.len()).then_some(next),
            details_omitted: true,
        }
    }

    /// Reserve a live run slot, after validating the entire input tree.
    pub fn start(&self, kind: RunKind) -> Result<RunLease, RunError> {
        let mut state = self.inner.lock();
        state.purge();
        if state.closed {
            return Err(RunError::SessionClosed);
        }
        if state
            .runs
            .values()
            .filter(|entry| entry.finished.is_none())
            .count()
            >= MAX_ACTIVE_RUNS
        {
            return Err(RunError::ActiveLimit);
        }
        let id = RunId(Uuid::new_v4());
        let cancellation = CancellationToken::new();
        state.runs.insert(
            id,
            Entry {
                snapshot: RunSnapshot {
                    run_id: id,
                    kind,
                    status: RunStatus::Running,
                    steps: Vec::new(),
                    result: None,
                    truncated: false,
                },
                cancellation: cancellation.clone(),
                finished: None,
                budget: budget::PayloadBudget::default(),
            },
        );
        Ok(RunLease {
            id,
            registry: Arc::downgrade(&self.inner),
            cancellation,
            armed: true,
        })
    }

    /// Inspect only this session's live/retained runs.
    pub fn status(&self, id: RunId) -> Result<RunSnapshot, RunError> {
        let mut state = self.inner.lock();
        state.purge();
        state
            .runs
            .get(&id)
            .map(|entry| entry.snapshot.clone())
            .ok_or(RunError::UnknownRun(id))
    }

    /// Request cancellation, without pretending child cleanup has finished.
    pub fn cancel(&self, id: RunId) -> Result<RunSnapshot, RunError> {
        let mut state = self.inner.lock();
        state.purge();
        let entry = state.runs.get_mut(&id).ok_or(RunError::UnknownRun(id))?;
        if entry.finished.is_none() {
            entry.snapshot.status = RunStatus::Cancelling;
            entry.cancellation.cancel();
        }
        Ok(entry.snapshot.clone())
    }

    /// Close the session to new runs and cancel all active work, even when
    /// background executors still hold cloned registry handles. Executors
    /// remain responsible for bounded cleanup and terminal reporting.
    pub fn shutdown(&self) {
        let mut state = self.inner.lock();
        state.closed = true;
        for entry in state.runs.values_mut() {
            if entry.finished.is_none() {
                entry.snapshot.status = RunStatus::Cancelling;
                entry.cancellation.cancel();
            }
        }
    }

    /// Purge expired terminal data during existing session maintenance.
    /// Every public lookup/start also enforces expiry; no per-run timer task
    /// or replay scheduler is created.
    pub fn collect_expired(&self) {
        self.inner.lock().purge();
    }
}

impl RunLease {
    /// The ID used by status/cancel calls within the owning session.
    pub const fn id(&self) -> RunId {
        self.id
    }

    /// Token the executor must observe, including session teardown.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Share observation without sharing ownership of the running executor.
    pub fn recorder(&self) -> RunRecorder {
        RunRecorder {
            id: self.id,
            registry: self.registry.clone(),
        }
    }

    /// Record an activation before starting side effects.
    pub fn start_step(&self, node_id: &str, attempt: usize) -> Result<(), RunError> {
        self.recorder().start_step(node_id, attempt)
    }

    /// Mark an active attempt as waiting on native/client input.
    pub fn waiting_for_input(&self, node_id: &str, attempt: usize) -> Result<(), RunError> {
        self.recorder().waiting_for_input(node_id, attempt)
    }

    /// Record an attempt's terminal result, subject to the shared output bound.
    pub fn finish_step(
        &self,
        node_id: &str,
        attempt: usize,
        outcome: StepOutcome,
        detail: Option<Value>,
    ) -> Result<(), RunError> {
        self.recorder()
            .finish_step(node_id, attempt, outcome, detail)
    }
}

impl RunRecorder {
    fn update<T>(&self, f: impl FnOnce(&mut Entry) -> Result<T, RunError>) -> Result<T, RunError> {
        let registry = self.registry.upgrade().ok_or(RunError::SessionClosed)?;
        let mut state = registry.lock();
        let entry = state
            .runs
            .get_mut(&self.id)
            .ok_or(RunError::UnknownRun(self.id))?;
        if entry.finished.is_some() {
            return Err(RunError::AlreadyTerminal);
        }
        f(entry)
    }

    /// Record before starting side effects. Attempts for a node are sequential
    /// and one-based; the total attempt budget is enforced here as well.
    pub fn start_step(&self, node_id: &str, attempt: usize) -> Result<(), RunError> {
        self.update(|entry| {
            if entry.cancellation.is_cancelled() {
                return Err(RunError::Cancelling);
            }
            if node_id.is_empty()
                || node_id.len() > 128
                || attempt == 0
                || attempt > MAX_LEAF_ATTEMPTS
                || entry.snapshot.steps.len() >= MAX_LEAF_ATTEMPTS
            {
                return Err(RunError::AttemptLimit);
            }
            let previous = entry
                .snapshot
                .steps
                .iter()
                .rev()
                .find(|step| step.node_id == node_id);
            if let Some(previous) = previous {
                if previous.attempt + 1 != attempt
                    || !matches!(previous.status, StepStatus::Terminal(_))
                {
                    return Err(RunError::InvalidTransition);
                }
            } else if attempt != 1 {
                return Err(RunError::InvalidTransition);
            }
            entry.snapshot.steps.push(StepSnapshot {
                node_id: node_id.to_owned(),
                attempt,
                status: StepStatus::Running,
                detail: None,
                truncated: false,
            });
            Ok(())
        })
    }

    /// Mark an active attempt as awaiting a user/model response.
    pub fn waiting_for_input(&self, node_id: &str, attempt: usize) -> Result<(), RunError> {
        self.update(|entry| {
            if entry.cancellation.is_cancelled() {
                return Err(RunError::Cancelling);
            }
            let step = entry.step(node_id, attempt)?;
            if matches!(step.status, StepStatus::Terminal(_)) {
                return Err(RunError::InvalidTransition);
            }
            step.status = StepStatus::WaitingForInput;
            Ok(())
        })
    }

    /// Record a terminal attempt. Large details are explicitly discarded, not
    /// retained elsewhere in the registry or silently declared complete.
    pub fn finish_step(
        &self,
        node_id: &str,
        attempt: usize,
        outcome: StepOutcome,
        detail: Option<Value>,
    ) -> Result<(), RunError> {
        self.update(|entry| {
            if matches!(
                entry.step(node_id, attempt)?.status,
                StepStatus::Terminal(_)
            ) {
                return Err(RunError::InvalidTransition);
            }
            let (detail, truncated) = entry.budget.take(detail);
            entry.snapshot.truncated |= truncated;
            let step = entry.step(node_id, attempt)?;
            step.status = StepStatus::Terminal(outcome);
            step.detail = detail;
            step.truncated = truncated;
            Ok(())
        })
    }
}

impl RunLease {
    /// Complete after all attempts have terminated. This consumes the lease,
    /// prevents terminal rewrites, and bounds the retained result.
    pub fn finish(
        mut self,
        outcome: RunOutcome,
        result: Option<Value>,
    ) -> Result<RunSnapshot, RunError> {
        let registry = self.registry.upgrade().ok_or(RunError::SessionClosed)?;
        let mut state = registry.lock();
        let entry = state
            .runs
            .get_mut(&self.id)
            .ok_or(RunError::UnknownRun(self.id))?;
        if entry.finished.is_some() {
            return Err(RunError::AlreadyTerminal);
        }
        if entry
            .snapshot
            .steps
            .iter()
            .any(|step| !matches!(step.status, StepStatus::Terminal(_)))
        {
            return Err(RunError::ActiveSteps);
        }
        entry.terminate(outcome, result);
        let snapshot = entry.snapshot.clone();
        state.terminal_order.push_back(self.id);
        state.purge();
        self.armed = false;
        Ok(snapshot)
    }
}

impl Drop for RunLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancellation.cancel();
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let mut state = registry.lock();
        if let Some(entry) = state.runs.get_mut(&self.id)
            && entry.finished.is_none()
        {
            for step in &mut entry.snapshot.steps {
                if !matches!(step.status, StepStatus::Terminal(_)) {
                    step.status = StepStatus::Terminal(StepOutcome::Cancelled);
                }
            }
            entry.terminate(RunOutcome::Interrupted, None);
            state.terminal_order.push_back(self.id);
            state.purge();
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        for entry in state.runs.values() {
            entry.cancellation.cancel();
        }
    }
}

impl Entry {
    fn step(&mut self, node: &str, attempt: usize) -> Result<&mut StepSnapshot, RunError> {
        self.snapshot
            .steps
            .iter_mut()
            .find(|step| step.node_id == node && step.attempt == attempt)
            .ok_or(RunError::UnknownStep)
    }

    fn terminate(&mut self, outcome: RunOutcome, result: Option<Value>) {
        self.cancellation.cancel();
        self.finished = Some(Instant::now());
        self.snapshot.status = RunStatus::Terminal(outcome);
        let (result, truncated) = self.budget.take(result);
        self.snapshot.result = result;
        self.snapshot.truncated |= truncated;
    }
}

impl State {
    fn purge(&mut self) {
        while let Some(id) = self.terminal_order.front() {
            let expired = self
                .runs
                .get(id)
                .and_then(|entry| entry.finished)
                .is_none_or(|finished| finished.elapsed() >= TERMINAL_RETENTION);
            if self.terminal_order.len() <= MAX_TERMINAL_RUNS && !expired {
                break;
            }
            if let Some(id) = self.terminal_order.pop_front() {
                self.runs.remove(&id);
            }
        }
    }
}
