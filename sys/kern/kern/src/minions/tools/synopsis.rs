use super::common::check_depth_limit;
use super::common::impl_function_tool_kind;
use super::{
    AgentStatus, FunctionCallError, ResponseInputItem, ToolHandler, ToolInvocation, ToolKind,
    ToolOutput, ToolPayload, UserInput, apply_spawn_agent_overrides,
    apply_spawn_agent_runtime_overrides, build_agent_spawn_config, function_arguments,
    parse_arguments, process_spawn_source, tool_output_json_text, tool_output_response_item,
};
use crate::config::Config;
use crate::minions::control::{AgentControl, SpawnAgentOptions};
use crate::minions::role::apply_role_to_config;
use crate::minions::status::is_final;
use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use chaos_synopsis::{
    ActionExecutor, ActionFuture, ActionId, ActionOutcome, Node, Outcome, Runner, Synopsis,
};
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

impl ToolHandler for Handler {
    type Output = RunSynopsisResult;

    impl_function_tool_kind!();

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
        let child_depth = check_depth_limit(&turn.session_source, turn.config.agent_max_depth)?;

        let base_config =
            build_agent_spawn_config(&session.get_base_instructions().await, turn.as_ref())?;
        let child_mode_policy = session
            .child_mode_policy(turn.as_ref(), None, None, None)
            .await
            .map_err(FunctionCallError::RespondToModel)?;
        let state = Arc::new(ExecutionState::new(&jobs));
        let backend: Arc<dyn AgentBackend> = Arc::new(LiveAgentBackend {
            control: session.services.agent_control.clone(),
        });
        let run_cancellation = CancellationToken::new();
        let mut cancellation_guard = RunCancellationGuard::new(run_cancellation.clone());
        let executor: Arc<dyn ActionExecutor<AgentAction>> = Arc::new(AgentExecutor {
            backend: Arc::clone(&backend),
            state: Arc::clone(&state),
            run_cancellation: run_cancellation.clone(),
        });

        let mut action_nodes = Vec::with_capacity(jobs.len());
        let job_order = jobs.iter().map(|job| job.id.clone()).collect::<Vec<_>>();
        for job in jobs {
            let mut config = base_config.clone();
            apply_role_to_config(&mut config, job.agent_type.as_deref())
                .await
                .map_err(FunctionCallError::RespondToModel)?;
            apply_spawn_agent_runtime_overrides(&mut config, turn.as_ref())?;
            apply_spawn_agent_overrides(&mut config, child_depth);
            config.mode_policy_override = Some(child_mode_policy.clone());

            let role = job.agent_type.clone();
            let action = AgentAction {
                config,
                input_items: vec![UserInput::Text {
                    text: job.message,
                    text_elements: Vec::new(),
                }],
                session_source: process_spawn_source(
                    session.conversation_id,
                    child_depth,
                    role.as_deref(),
                ),
            };
            action_nodes.push(Node::action(job.id, action));
        }

        let root = build_root(args.mode, action_nodes);
        let runner = Runner::new(Synopsis::new(root), executor)
            .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
        let run_result = tokio::time::timeout(
            Duration::from_millis(timeout_ms as u64),
            runner.run(run_cancellation.clone()),
        )
        .await;

        let (outcome, error) = match run_result {
            Ok(Ok(Outcome::Success)) => (RunSynopsisOutcome::Success, None),
            Ok(Ok(Outcome::Failure)) => (RunSynopsisOutcome::Failure, None),
            Ok(Ok(Outcome::Cancelled)) => (RunSynopsisOutcome::Cancelled, None),
            Ok(Err(error)) => (RunSynopsisOutcome::Error, Some(error.to_string())),
            Err(_) => (RunSynopsisOutcome::TimedOut, None),
        };

        run_cancellation.cancel();
        state.shutdown_remaining(Arc::clone(&backend)).await;
        cancellation_guard.disarm();

        Ok(RunSynopsisResult {
            outcome,
            jobs: state.snapshot(&job_order),
            error,
        })
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
            })
        })
        .collect()
}

fn build_root(mode: SynopsisMode, nodes: Vec<Node<AgentAction>>) -> Node<AgentAction> {
    match mode {
        SynopsisMode::Sequence => Node::sequence(nodes),
        SynopsisMode::ParallelAll => Node::parallel_all(nodes),
        SynopsisMode::Fallback => Node::fallback(nodes),
        SynopsisMode::Race => Node::race(nodes),
    }
}

#[derive(Debug, Deserialize)]
struct RunSynopsisArgs {
    mode: SynopsisMode,
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
struct SynopsisJobArgs {
    id: String,
    message: String,
    agent_type: Option<String>,
}

struct NormalizedJob {
    id: String,
    message: String,
    agent_type: Option<String>,
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
                spawn_state.mark_spawned(&spawn_id, process_id);
                let lease = AgentLease::new(
                    spawn_id.clone(),
                    process_id,
                    Arc::clone(&spawn_backend),
                    Arc::clone(&spawn_state),
                );

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
    backend: Arc<dyn AgentBackend>,
    state: Arc<ExecutionState>,
    armed: bool,
}

impl AgentLease {
    fn new(
        id: ActionId,
        process_id: ProcessId,
        backend: Arc<dyn AgentBackend>,
        state: Arc<ExecutionState>,
    ) -> Self {
        Self {
            id,
            process_id,
            backend,
            state,
            armed: true,
        }
    }

    async fn close(mut self) {
        let _ = self.backend.shutdown(self.process_id).await;
        self.state.remove_active(&self.id);
        self.armed = false;
    }
}

impl Drop for AgentLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.state.mark_cancelled(&self.id);
        let id = self.id.clone();
        let process_id = self.process_id;
        let backend = Arc::clone(&self.backend);
        let state = Arc::clone(&self.state);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = backend.shutdown(process_id).await;
                state.remove_active(&id);
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

struct ExecutionState {
    jobs: Mutex<HashMap<String, SynopsisJobResult>>,
    active: Mutex<HashMap<ActionId, ProcessId>>,
    in_flight_spawns: AtomicUsize,
    spawns_finished: Notify,
}

impl ExecutionState {
    fn new(jobs: &[NormalizedJob]) -> Self {
        let jobs = jobs
            .iter()
            .map(|job| {
                (
                    job.id.clone(),
                    SynopsisJobResult {
                        id: job.id.clone(),
                        state: SynopsisJobState::Pending,
                        agent_id: None,
                        nickname: None,
                        agent_type: job.agent_type.clone(),
                        status: None,
                        error: None,
                    },
                )
            })
            .collect();
        Self {
            jobs: Mutex::new(jobs),
            active: Mutex::new(HashMap::new()),
            in_flight_spawns: AtomicUsize::new(0),
            spawns_finished: Notify::new(),
        }
    }

    fn lock_jobs(&self) -> MutexGuard<'_, HashMap<String, SynopsisJobResult>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_active(&self) -> MutexGuard<'_, HashMap<ActionId, ProcessId>> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
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
            job.state = SynopsisJobState::Running;
        }
    }

    fn mark_spawned(&self, id: &ActionId, process_id: ProcessId) {
        self.lock_active().insert(id.clone(), process_id);
        if let Some(job) = self.lock_jobs().get_mut(id.as_str()) {
            job.agent_id = Some(process_id.to_string());
        }
    }

    fn mark_failed(&self, id: &ActionId, error: String) {
        if let Some(job) = self.lock_jobs().get_mut(id.as_str()) {
            job.state = SynopsisJobState::Failed;
            job.error = Some(error);
        }
    }

    fn mark_cancelled(&self, id: &ActionId) {
        if let Some(job) = self.lock_jobs().get_mut(id.as_str())
            && matches!(
                job.state,
                SynopsisJobState::Pending | SynopsisJobState::Running
            )
        {
            job.state = SynopsisJobState::Cancelled;
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
            job.state = match outcome {
                ActionOutcome::Success => SynopsisJobState::Completed,
                ActionOutcome::Failure => SynopsisJobState::Failed,
            };
            job.status = Some(status);
            job.nickname = nickname;
            if agent_type.is_some() {
                job.agent_type = agent_type;
            }
        }
    }

    fn remove_active(&self, id: &ActionId) {
        self.lock_active().remove(id);
    }

    async fn wait_for_spawns(&self) {
        loop {
            let notified = self.spawns_finished.notified();
            if self.in_flight_spawns.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }

    async fn shutdown_remaining(&self, backend: Arc<dyn AgentBackend>) {
        self.wait_for_spawns().await;
        let active = self.lock_active().drain().collect::<Vec<_>>();
        for (id, process_id) in active {
            self.mark_cancelled(&id);
            let _ = backend.shutdown(process_id).await;
        }
    }

    fn snapshot(&self, order: &[String]) -> Vec<SynopsisJobResult> {
        let jobs = self.lock_jobs();
        order
            .iter()
            .filter_map(|id| jobs.get(id).cloned())
            .collect()
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RunSynopsisResult {
    outcome: RunSynopsisOutcome,
    jobs: Vec<SynopsisJobResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ToolOutput for RunSynopsisResult {
    fn log_preview(&self) -> String {
        tool_output_json_text(self, "run_synopsis")
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
mod tests {
    use super::*;
    use crate::config::test_config;

    #[derive(Clone)]
    struct FakePlan {
        delay: Duration,
        status: AgentStatus,
    }

    #[derive(Default)]
    struct FakeBackend {
        plans: Mutex<HashMap<String, FakePlan>>,
        statuses: Mutex<HashMap<ProcessId, watch::Sender<AgentStatus>>>,
        shutdowns: Mutex<Vec<ProcessId>>,
    }

    impl FakeBackend {
        fn with_plans(plans: impl IntoIterator<Item = (&'static str, FakePlan)>) -> Arc<Self> {
            Arc::new(Self {
                plans: Mutex::new(
                    plans
                        .into_iter()
                        .map(|(id, plan)| (id.to_string(), plan))
                        .collect(),
                ),
                ..Self::default()
            })
        }

        fn shutdown_count(&self) -> usize {
            self.shutdowns
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len()
        }
    }

    impl AgentBackend for FakeBackend {
        fn spawn(
            &self,
            id: ActionId,
            _action: AgentAction,
        ) -> BackendFuture<Result<ProcessId, String>> {
            let plan = self
                .plans
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(id.as_str());
            let Some(plan) = plan else {
                return Box::pin(async move { Err(format!("no plan for {id}")) });
            };
            let process_id = ProcessId::new();
            let (sender, _receiver) = watch::channel(AgentStatus::Running);
            self.statuses
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(process_id, sender.clone());
            Box::pin(async move {
                tokio::spawn(async move {
                    tokio::time::sleep(plan.delay).await;
                    let _ = sender.send(plan.status);
                });
                Ok(process_id)
            })
        }

        fn subscribe_status(
            &self,
            process_id: ProcessId,
        ) -> BackendFuture<Result<watch::Receiver<AgentStatus>, String>> {
            let receiver = self
                .statuses
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&process_id)
                .map(watch::Sender::subscribe)
                .ok_or_else(|| "missing status".to_string());
            Box::pin(async move { receiver })
        }

        fn get_status(&self, process_id: ProcessId) -> BackendFuture<AgentStatus> {
            let status = self
                .statuses
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&process_id)
                .map(|sender| sender.borrow().clone())
                .unwrap_or(AgentStatus::NotFound);
            Box::pin(async move { status })
        }

        fn get_agent_info(
            &self,
            _process_id: ProcessId,
        ) -> BackendFuture<(Option<String>, Option<String>)> {
            Box::pin(async { (None, None) })
        }

        fn shutdown(&self, process_id: ProcessId) -> BackendFuture<Result<(), String>> {
            self.shutdowns
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(process_id);
            if let Some(sender) = self
                .statuses
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&process_id)
            {
                let _ = sender.send(AgentStatus::Shutdown);
            }
            Box::pin(async { Ok(()) })
        }
    }

    fn job(id: &str) -> NormalizedJob {
        NormalizedJob {
            id: id.to_string(),
            message: format!("run {id}"),
            agent_type: None,
        }
    }

    fn action(id: &str) -> Node<AgentAction> {
        Node::action(
            id,
            AgentAction {
                config: test_config(),
                input_items: vec![UserInput::Text {
                    text: format!("run {id}"),
                    text_elements: Vec::new(),
                }],
                session_source: process_spawn_source(ProcessId::new(), 1, None),
            },
        )
    }

    async fn run_fake(
        root: Node<AgentAction>,
        jobs: &[NormalizedJob],
        backend: Arc<FakeBackend>,
        cancellation: CancellationToken,
    ) -> (Outcome, Arc<ExecutionState>) {
        let state = Arc::new(ExecutionState::new(jobs));
        let backend_dyn: Arc<dyn AgentBackend> = backend;
        let executor: Arc<dyn ActionExecutor<AgentAction>> = Arc::new(AgentExecutor {
            backend: Arc::clone(&backend_dyn),
            state: Arc::clone(&state),
            run_cancellation: cancellation.clone(),
        });
        let runner = Runner::new(Synopsis::new(root), executor).expect("valid synopsis");
        let outcome = runner.run(cancellation.clone()).await.expect("runner");
        cancellation.cancel();
        state.shutdown_remaining(backend_dyn).await;
        (outcome, state)
    }

    #[test]
    fn normalizes_jobs_and_rejects_duplicates() {
        let jobs = normalize_jobs(vec![
            SynopsisJobArgs {
                id: " first ".to_string(),
                message: "one".to_string(),
                agent_type: Some(" scout ".to_string()),
            },
            SynopsisJobArgs {
                id: "first".to_string(),
                message: "two".to_string(),
                agent_type: None,
            },
        ]);
        assert!(jobs.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn parallel_agents_complete_and_are_closed() {
        let jobs = vec![job("left"), job("right")];
        let backend = FakeBackend::with_plans([
            (
                "left",
                FakePlan {
                    delay: Duration::from_millis(10),
                    status: AgentStatus::Completed(Some("left done".to_string())),
                },
            ),
            (
                "right",
                FakePlan {
                    delay: Duration::from_millis(20),
                    status: AgentStatus::Completed(Some("right done".to_string())),
                },
            ),
        ]);

        let (outcome, state) = run_fake(
            Node::parallel_all([action("left"), action("right")]),
            &jobs,
            Arc::clone(&backend),
            CancellationToken::new(),
        )
        .await;

        assert_eq!(outcome, Outcome::Success);
        assert_eq!(backend.shutdown_count(), 2);
        assert!(
            state
                .snapshot(&["left".to_string(), "right".to_string()])
                .iter()
                .all(|job| job.state == SynopsisJobState::Completed)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn errored_agent_fails_the_synopsis() {
        let jobs = vec![job("broken")];
        let backend = FakeBackend::with_plans([(
            "broken",
            FakePlan {
                delay: Duration::from_millis(10),
                status: AgentStatus::Errored("boom".to_string()),
            },
        )]);

        let (outcome, state) = run_fake(
            action("broken"),
            &jobs,
            Arc::clone(&backend),
            CancellationToken::new(),
        )
        .await;

        assert_eq!(outcome, Outcome::Failure);
        assert_eq!(
            state.snapshot(&["broken".to_string()])[0].state,
            SynopsisJobState::Failed
        );
        assert_eq!(backend.shutdown_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_runs_backup_after_errored_agent() {
        let jobs = vec![job("primary"), job("backup")];
        let backend = FakeBackend::with_plans([
            (
                "primary",
                FakePlan {
                    delay: Duration::from_millis(10),
                    status: AgentStatus::Errored("primary failed".to_string()),
                },
            ),
            (
                "backup",
                FakePlan {
                    delay: Duration::from_millis(10),
                    status: AgentStatus::Completed(Some("backup done".to_string())),
                },
            ),
        ]);

        let (outcome, state) = run_fake(
            Node::fallback([action("primary"), action("backup")]),
            &jobs,
            Arc::clone(&backend),
            CancellationToken::new(),
        )
        .await;

        assert_eq!(outcome, Outcome::Success);
        assert_eq!(backend.shutdown_count(), 2);
        let results = state.snapshot(&["primary".to_string(), "backup".to_string()]);
        assert_eq!(results[0].state, SynopsisJobState::Failed);
        assert_eq!(
            results[0].status,
            Some(AgentStatus::Errored("primary failed".to_string()))
        );
        assert_eq!(results[1].state, SynopsisJobState::Completed);
        assert_eq!(
            results[1].status,
            Some(AgentStatus::Completed(Some("backup done".to_string())))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_runs_backup_after_spawn_failure_without_leaking_agent() {
        let jobs = vec![job("primary"), job("backup")];
        let backend = FakeBackend::with_plans([(
            "backup",
            FakePlan {
                delay: Duration::from_millis(10),
                status: AgentStatus::Completed(Some("backup done".to_string())),
            },
        )]);

        let (outcome, state) = run_fake(
            Node::fallback([action("primary"), action("backup")]),
            &jobs,
            Arc::clone(&backend),
            CancellationToken::new(),
        )
        .await;

        assert_eq!(outcome, Outcome::Success);
        assert_eq!(backend.shutdown_count(), 1);
        let results = state.snapshot(&["primary".to_string(), "backup".to_string()]);
        assert_eq!(results[0].state, SynopsisJobState::Failed);
        assert_eq!(results[0].agent_id, None);
        assert_eq!(results[0].error.as_deref(), Some("no plan for primary"));
        assert_eq!(results[1].state, SynopsisJobState::Completed);
        assert!(results[1].agent_id.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn race_cancels_and_closes_the_loser() {
        let jobs = vec![job("winner"), job("loser")];
        let backend = FakeBackend::with_plans([
            (
                "winner",
                FakePlan {
                    delay: Duration::from_millis(10),
                    status: AgentStatus::Completed(Some("won".to_string())),
                },
            ),
            (
                "loser",
                FakePlan {
                    delay: Duration::from_secs(60),
                    status: AgentStatus::Completed(Some("late".to_string())),
                },
            ),
        ]);

        let (outcome, state) = run_fake(
            Node::race([action("winner"), action("loser")]),
            &jobs,
            Arc::clone(&backend),
            CancellationToken::new(),
        )
        .await;

        assert_eq!(outcome, Outcome::Success);
        assert_eq!(backend.shutdown_count(), 2);
        let results = state.snapshot(&["winner".to_string(), "loser".to_string()]);
        assert_eq!(results[0].state, SynopsisJobState::Completed);
        assert_eq!(results[1].state, SynopsisJobState::Cancelled);
    }

    #[tokio::test(start_paused = true)]
    async fn external_cancellation_closes_running_agents() {
        let backend = FakeBackend::with_plans([(
            "slow",
            FakePlan {
                delay: Duration::from_secs(60),
                status: AgentStatus::Completed(Some("late".to_string())),
            },
        )]);
        let cancellation = CancellationToken::new();
        let runner_cancellation = cancellation.clone();
        let backend_for_task = Arc::clone(&backend);
        let handle = tokio::spawn(async move {
            run_fake(
                action("slow"),
                &[job("slow")],
                backend_for_task,
                runner_cancellation,
            )
            .await
        });
        tokio::task::yield_now().await;
        cancellation.cancel();

        let (outcome, state) = handle.await.expect("join");
        assert_eq!(outcome, Outcome::Cancelled);
        assert_eq!(backend.shutdown_count(), 1);
        assert_eq!(
            state.snapshot(&["slow".to_string()])[0].state,
            SynopsisJobState::Cancelled
        );
    }
}
