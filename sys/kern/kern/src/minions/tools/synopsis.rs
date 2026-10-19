use super::common::check_depth_limit;
use super::common::impl_function_tool_kind;
use super::{
    AgentStatus, FunctionCallError, ResponseInputItem, ToolHandler, ToolInvocation, ToolKind,
    ToolOutput, ToolPayload, UserInput, apply_spawn_agent_overrides,
    apply_spawn_agent_runtime_overrides, build_agent_spawn_config, function_arguments,
    parse_arguments, process_spawn_source, tool_output_response_item,
};
use crate::config::Config;
use crate::minions::control::{AgentControl, SpawnAgentOptions};
use crate::minions::role::apply_role_to_config;
use crate::minions::status::is_final;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use chaos_synopsis::runs::{RunId, RunKind, RunOutcome};
use chaos_synopsis::tree::TreeNode;
use chaos_synopsis::{ActionExecutor, ActionFuture, ActionId, ActionOutcome};
#[cfg(test)]
use chaos_synopsis::{Node, Outcome, Runner, Synopsis};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

const DEFAULT_SYNOPSIS_TIMEOUT_MS: i64 = 30 * 60 * 1000;
const MIN_SYNOPSIS_TIMEOUT_MS: i64 = 10_000;
const MAX_SYNOPSIS_TIMEOUT_MS: i64 = 60 * 60 * 1000;
const MAX_SYNOPSIS_JOBS: usize = 16;

pub(crate) struct Handler;

mod tree;

impl ToolHandler for Handler {
    type Output = RunSynopsisResult;

    impl_function_tool_kind!();

    fn is_execution_container(&self) -> bool {
        // Job leaves create separately policy-scoped children; the parent
        // must neither retain a leaf permit nor log its orchestration input.
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;
        let arguments = function_arguments(payload)?;
        let args: RunSynopsisArgs = parse_arguments(&arguments)?;
        let jobs = normalize_jobs(args.jobs)?;
        let timeout_ms = normalize_timeout(args.timeout_ms)?;
        let root = match (args.mode, args.tree) {
            (None, Some(root)) => root,
            (Some(mode), None) => build_legacy_tree(mode, &jobs),
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "provide exactly one of mode or tree".into(),
                ));
            }
        };
        tree::run(session, turn, root, jobs, timeout_ms).await
    }
}

fn normalize_timeout(timeout_ms: Option<i64>) -> Result<i64, FunctionCallError> {
    match timeout_ms.unwrap_or(DEFAULT_SYNOPSIS_TIMEOUT_MS) {
        timeout if timeout <= 0 => Err(FunctionCallError::RespondToModel(
            "timeout_ms must be greater than zero".to_string(),
        )),
        timeout => Ok(timeout.clamp(MIN_SYNOPSIS_TIMEOUT_MS, MAX_SYNOPSIS_TIMEOUT_MS)),
    }
}

fn normalize_jobs(jobs: Vec<SynopsisJobArgs>) -> Result<Vec<NormalizedJob>, FunctionCallError> {
    if jobs.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "jobs must be non-empty".to_string(),
        ));
    }
    if jobs.len() > MAX_SYNOPSIS_JOBS {
        return Err(FunctionCallError::RespondToModel(format!(
            "jobs cannot contain more than {MAX_SYNOPSIS_JOBS} entries"
        )));
    }

    let mut ids = HashSet::with_capacity(jobs.len());
    jobs.into_iter()
        .map(|job| {
            let id = job.id.trim().to_string();
            if id.is_empty() {
                return Err(FunctionCallError::RespondToModel(
                    "job ids cannot be empty".to_string(),
                ));
            }
            if !ids.insert(id.clone()) {
                return Err(FunctionCallError::RespondToModel(format!(
                    "duplicate job id `{id}`"
                )));
            }
            if job.message.trim().is_empty() {
                return Err(FunctionCallError::RespondToModel(format!(
                    "job `{id}` has an empty message"
                )));
            }
            let agent_type = job
                .agent_type
                .map(|role| role.trim().to_string())
                .filter(|role| !role.is_empty());
            Ok(NormalizedJob {
                id,
                message: job.message,
                agent_type,
                allow_replay: job.allow_replay,
            })
        })
        .collect()
}

fn build_legacy_tree(mode: SynopsisMode, jobs: &[NormalizedJob]) -> TreeNode {
    // Generated node IDs keep legacy job IDs unrestricted and result order intact.
    let children = jobs
        .iter()
        .enumerate()
        .map(|(index, job)| TreeNode::Job {
            id: format!("job_{index}"),
            job_id: job.id.clone(),
            result_key: None,
        })
        .collect();
    let id = "root".to_string();
    match mode {
        SynopsisMode::Sequence => TreeNode::Sequence { id, children },
        SynopsisMode::ParallelAll => TreeNode::ParallelAll { id, children },
        SynopsisMode::Fallback => TreeNode::Fallback { id, children },
        SynopsisMode::Race => TreeNode::Race { id, children },
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunSynopsisArgs {
    mode: Option<SynopsisMode>,
    tree: Option<TreeNode>,
    jobs: Vec<SynopsisJobArgs>,
    timeout_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SynopsisMode {
    Sequence,
    ParallelAll,
    Fallback,
    Race,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SynopsisJobArgs {
    id: String,
    message: String,
    agent_type: Option<String>,
    #[serde(default)]
    allow_replay: bool,
}

#[derive(Clone)]
struct NormalizedJob {
    id: String,
    message: String,
    agent_type: Option<String>,
    allow_replay: bool,
}

struct AgentAction {
    config: Config,
    input_items: Vec<UserInput>,
    session_source: SessionSource,
}

type BackendFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

trait AgentBackend: Send + Sync + 'static {
    fn spawn(&self, id: ActionId, action: AgentAction) -> BackendFuture<Result<ProcessId, String>>;

    fn subscribe_status(
        &self,
        process_id: ProcessId,
    ) -> BackendFuture<Result<watch::Receiver<AgentStatus>, String>>;

    fn get_status(&self, process_id: ProcessId) -> BackendFuture<AgentStatus>;

    fn get_agent_info(
        &self,
        process_id: ProcessId,
    ) -> BackendFuture<(Option<String>, Option<String>)>;

    fn shutdown(&self, process_id: ProcessId) -> BackendFuture<Result<(), String>>;
}

struct LiveAgentBackend {
    control: AgentControl,
}

impl AgentBackend for LiveAgentBackend {
    fn spawn(
        &self,
        _id: ActionId,
        action: AgentAction,
    ) -> BackendFuture<Result<ProcessId, String>> {
        let control = self.control.clone();
        Box::pin(async move {
            control
                .spawn_agent_with_options(
                    action.config,
                    action.input_items,
                    Some(action.session_source),
                    SpawnAgentOptions {
                        suppress_parent_completion_notification: true,
                        ..SpawnAgentOptions::default()
                    },
                )
                .await
                .map(|spawned| spawned.process_id)
                .map_err(|error| error.to_string())
        })
    }

    fn subscribe_status(
        &self,
        process_id: ProcessId,
    ) -> BackendFuture<Result<watch::Receiver<AgentStatus>, String>> {
        let control = self.control.clone();
        Box::pin(async move {
            control
                .subscribe_status(process_id)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn get_status(&self, process_id: ProcessId) -> BackendFuture<AgentStatus> {
        let control = self.control.clone();
        Box::pin(async move { control.get_status(process_id).await })
    }

    fn get_agent_info(
        &self,
        process_id: ProcessId,
    ) -> BackendFuture<(Option<String>, Option<String>)> {
        let control = self.control.clone();
        Box::pin(async move {
            control
                .get_agent_nickname_and_role(process_id)
                .await
                .unwrap_or((None, None))
        })
    }

    fn shutdown(&self, process_id: ProcessId) -> BackendFuture<Result<(), String>> {
        let control = self.control.clone();
        Box::pin(async move {
            control
                .shutdown_agent(process_id)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }
}

struct AgentExecutor {
    backend: Arc<dyn AgentBackend>,
    state: Arc<ExecutionState>,
    run_cancellation: CancellationToken,
}

impl ActionExecutor<AgentAction> for AgentExecutor {
    fn execute(
        &self,
        id: ActionId,
        action: AgentAction,
        cancellation: CancellationToken,
    ) -> ActionFuture {
        let backend = Arc::clone(&self.backend);
        let state = Arc::clone(&self.state);
        let run_cancellation = self.run_cancellation.clone();

        Box::pin(async move {
            state.mark_running(&id);
            state.begin_spawn();

            let spawn_backend = Arc::clone(&backend);
            let spawn_state = Arc::clone(&state);
            let spawn_id = id.clone();
            let spawn_cancellation = cancellation.clone();
            let spawn_run_cancellation = run_cancellation.clone();
            let spawn_task = tokio::spawn(async move {
                let _spawn_guard = InFlightSpawn::new(Arc::clone(&spawn_state));
                let process_id = match spawn_backend.spawn(spawn_id.clone(), action).await {
                    Ok(process_id) => process_id,
                    Err(error) => {
                        spawn_state.mark_failed(&spawn_id, error);
                        return SpawnTaskResult::Failed;
                    }
                };
                spawn_state.mark_spawned(&spawn_id, process_id, Arc::clone(&spawn_backend));
                let lease = AgentLease::new(spawn_id.clone(), process_id, Arc::clone(&spawn_state));

                if spawn_cancellation.is_cancelled() || spawn_run_cancellation.is_cancelled() {
                    spawn_state.mark_cancelled(&spawn_id);
                    lease.close().await;
                    SpawnTaskResult::Cancelled
                } else {
                    SpawnTaskResult::Spawned(lease)
                }
            });

            let lease = match spawn_task.await {
                Ok(SpawnTaskResult::Spawned(lease)) => lease,
                Ok(SpawnTaskResult::Failed | SpawnTaskResult::Cancelled) => {
                    return ActionOutcome::Failure;
                }
                Err(error) => {
                    state.mark_failed(&id, format!("agent spawn task failed: {error}"));
                    return ActionOutcome::Failure;
                }
            };

            let process_id = lease.process_id;
            let status = wait_for_final_status(backend.as_ref(), process_id).await;
            let (nickname, agent_type) = backend.get_agent_info(process_id).await;
            let outcome = if matches!(status, AgentStatus::Completed(_)) {
                ActionOutcome::Success
            } else {
                ActionOutcome::Failure
            };
            state.mark_terminal(&id, status, nickname, agent_type, outcome);
            lease.close().await;
            outcome
        })
    }
}

enum SpawnTaskResult {
    Spawned(AgentLease),
    Failed,
    Cancelled,
}

async fn wait_for_final_status(backend: &dyn AgentBackend, process_id: ProcessId) -> AgentStatus {
    let mut receiver = match backend.subscribe_status(process_id).await {
        Ok(receiver) => receiver,
        Err(error) => {
            let status = backend.get_status(process_id).await;
            return if is_final(&status) {
                status
            } else {
                AgentStatus::Errored(format!("failed to watch agent status: {error}"))
            };
        }
    };

    loop {
        let status = receiver.borrow().clone();
        if is_final(&status) {
            return status;
        }
        if receiver.changed().await.is_err() {
            let status = backend.get_status(process_id).await;
            return if is_final(&status) {
                status
            } else {
                AgentStatus::Errored("agent status stream closed before completion".to_string())
            };
        }
    }
}

struct AgentLease {
    id: ActionId,
    process_id: ProcessId,
    state: Arc<ExecutionState>,
    armed: bool,
}

impl AgentLease {
    fn new(id: ActionId, process_id: ProcessId, state: Arc<ExecutionState>) -> Self {
        Self {
            id,
            process_id,
            state,
            armed: true,
        }
    }

    async fn close(mut self) {
        self.state.close_agent(&self.id).await;
        self.armed = false;
    }
}

impl Drop for AgentLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.state.mark_cancelled(&self.id);
        let state = Arc::clone(&self.state);
        let id = self.id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                state.close_agent(&id).await;
            });
        }
    }
}

struct AgentResource {
    process_id: tokio::sync::Mutex<Option<ProcessId>>,
    backend: Arc<dyn AgentBackend>,
}

impl std::fmt::Debug for AgentResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentResource").finish_non_exhaustive()
    }
}

impl AgentResource {
    async fn close(&self) {
        let mut process = self.process_id.lock().await;
        if let Some(process_id) = *process {
            let _ = self.backend.shutdown(process_id).await;
            *process = None;
        }
    }
}

impl Drop for AgentResource {
    fn drop(&mut self) {
        let Some(process_id) = self.process_id.get_mut().take() else {
            return;
        };
        let backend = Arc::clone(&self.backend);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = backend.shutdown(process_id).await;
            });
        }
    }
}

struct InFlightSpawn {
    state: Arc<ExecutionState>,
}

impl InFlightSpawn {
    fn new(state: Arc<ExecutionState>) -> Self {
        Self { state }
    }
}

impl Drop for InFlightSpawn {
    fn drop(&mut self) {
        self.state.finish_spawn();
    }
}

struct RunCancellationGuard {
    cancellation: CancellationToken,
    armed: bool,
}

impl RunCancellationGuard {
    fn new(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RunCancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

mod lifecycle;

struct ExecutionState {
    jobs: Mutex<HashMap<String, lifecycle::Job>>,
    in_flight_spawns: AtomicUsize,
    spawns_finished: Notify,
}

impl ExecutionState {
    fn new(jobs: &[NormalizedJob]) -> Self {
        let jobs = jobs
            .iter()
            .map(|job| (job.id.clone(), lifecycle::Job::new(job)))
            .collect();
        Self {
            jobs: Mutex::new(jobs),
            in_flight_spawns: AtomicUsize::new(0),
            spawns_finished: Notify::new(),
        }
    }

    fn lock_jobs(&self) -> MutexGuard<'_, HashMap<String, lifecycle::Job>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn begin_spawn(&self) {
        self.in_flight_spawns.fetch_add(1, Ordering::AcqRel);
    }

    fn finish_spawn(&self) {
        if self.in_flight_spawns.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.spawns_finished.notify_waiters();
        }
    }

    fn mark_running(&self, id: &ActionId) {
        if let Some(job) = self.lock_jobs().get_mut(id.as_str()) {
            job.apply(lifecycle::SynopsisJobEvent::Start);
        }
    }

    fn mark_spawned(&self, id: &ActionId, process_id: ProcessId, backend: Arc<dyn AgentBackend>) {
        if let Some(job) = self.lock_jobs().get_mut(id.as_str()) {
            let data = job.data_mut();
            data.agent_id = Some(process_id.to_string());
            data.agent = Some(Arc::new(AgentResource {
                process_id: tokio::sync::Mutex::new(Some(process_id)),
                backend,
            }));
        }
    }

    fn mark_failed(&self, id: &ActionId, error: String) {
        if let Some(job) = self.lock_jobs().get_mut(id.as_str())
            && job.apply(lifecycle::SynopsisJobEvent::Fail)
        {
            job.data_mut().error = Some(error);
        }
    }

    fn mark_cancelled(&self, id: &ActionId) {
        if let Some(job) = self.lock_jobs().get_mut(id.as_str()) {
            job.apply(lifecycle::SynopsisJobEvent::Cancel);
        }
    }

    fn mark_terminal(
        &self,
        id: &ActionId,
        status: AgentStatus,
        nickname: Option<String>,
        agent_type: Option<String>,
        outcome: ActionOutcome,
    ) {
        if let Some(job) = self.lock_jobs().get_mut(id.as_str()) {
            let event = match outcome {
                ActionOutcome::Success => lifecycle::SynopsisJobEvent::Complete,
                ActionOutcome::Failure => lifecycle::SynopsisJobEvent::Fail,
            };
            if !job.apply(event) {
                return;
            }
            let data = job.data_mut();
            data.status = Some(status);
            data.nickname = nickname;
            if agent_type.is_some() {
                data.agent_type = agent_type;
            }
        }
    }

    async fn close_agent(&self, id: &ActionId) {
        let agent = self
            .lock_jobs()
            .get_mut(id.as_str())
            .and_then(|job| job.data_mut().agent.clone());
        if let Some(agent) = agent {
            agent.close().await;
            if let Some(job) = self.lock_jobs().get_mut(id.as_str()) {
                job.data_mut().agent = None;
            }
        }
    }

    async fn wait_for_spawns(&self) {
        loop {
            let notified = self.spawns_finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.in_flight_spawns.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }

    async fn shutdown_remaining(&self) {
        self.wait_for_spawns().await;
        let active = self
            .lock_jobs()
            .iter_mut()
            .filter_map(|(id, job)| {
                let agent = job.data_mut().agent.clone()?;
                job.apply(lifecycle::SynopsisJobEvent::Cancel);
                Some((id.clone(), agent))
            })
            .collect::<Vec<_>>();
        for (id, agent) in active {
            agent.close().await;
            if let Some(job) = self.lock_jobs().get_mut(&id) {
                job.data_mut().agent = None;
            }
        }
    }

    fn snapshot(&self, order: &[String]) -> Vec<SynopsisJobResult> {
        let jobs = self.lock_jobs();
        order
            .iter()
            .filter_map(|id| jobs.get(id).map(lifecycle::Job::snapshot))
            .collect()
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RunSynopsisResult {
    run_id: RunId,
    outcome: RunSynopsisOutcome,
    jobs: Vec<SynopsisJobResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ToolOutput for RunSynopsisResult {
    fn log_preview(&self) -> String {
        "[synopsis snapshot redacted]".into()
    }

    fn success_for_logging(&self) -> bool {
        matches!(self.outcome, RunSynopsisOutcome::Success)
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(
            call_id,
            payload,
            self,
            Some(self.success_for_logging()),
            "run_synopsis",
        )
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum RunSynopsisOutcome {
    Success,
    Failure,
    Cancelled,
    TimedOut,
    Error,
}

impl From<RunSynopsisOutcome> for RunOutcome {
    fn from(outcome: RunSynopsisOutcome) -> Self {
        match outcome {
            RunSynopsisOutcome::Success => Self::Success,
            RunSynopsisOutcome::Failure => Self::Failure,
            RunSynopsisOutcome::Cancelled => Self::Cancelled,
            RunSynopsisOutcome::TimedOut => Self::TimedOut,
            RunSynopsisOutcome::Error => Self::Error,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct SynopsisJobResult {
    id: String,
    state: SynopsisJobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<AgentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SynopsisJobState {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[cfg(test)]
mod tests;
