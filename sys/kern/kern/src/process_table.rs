use crate::AuthManager;
use crate::ChaosAuth;
use crate::ModelProviderInfo;

use crate::chaos::Chaos;
use crate::chaos::ChaosSpawnArgs;
use crate::chaos::ChaosSpawnOk;
use crate::chaos::INITIAL_SUBMIT_ID;
use crate::config::Config;
use crate::error::ChaosErr;
use crate::error::Result as ChaosResult;
use crate::file_watcher::FileWatcher;
use crate::file_watcher::FileWatcherEvent;
use crate::mcp::McpManager;
use crate::minions::AgentControl;
use crate::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use crate::models_manager::manager::ModelsManager;
use crate::process::Process;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::SessionConfiguredEvent;
use crate::rollout::RolloutRecorder;
use crate::rollout::truncation;
use crate::shell_snapshot::ShellSnapshot;
use crate::skills::SkillsManager;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::CollaborationModeMask;
use chaos_ipc::openai_models::ModelPreset;
use chaos_ipc::protocol::InitialHistory;
use chaos_ipc::protocol::McpServerRefreshConfig;
use chaos_ipc::protocol::Op;
use chaos_ipc::protocol::ResumedHistory;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::W3cTraceContext;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::runtime::RuntimeFlavor;
use tokio::sync::RwLock;
use tokio::sync::broadcast;
use tracing::warn;

const PROCESS_CREATED_CHANNEL_CAPACITY: usize = 1024;
/// Test-only override for enabling process-table behaviors used by integration
/// tests.
///
/// In production builds this value should remain at its default (`false`) and
/// must not be toggled.
static FORCE_TEST_PROCESS_TABLE_BEHAVIOR: AtomicBool = AtomicBool::new(false);

type CapturedOps = Vec<(ProcessId, Op)>;
type SharedCapturedOps = Arc<std::sync::Mutex<CapturedOps>>;

pub(crate) fn set_process_table_test_mode_for_tests(enabled: bool) {
    FORCE_TEST_PROCESS_TABLE_BEHAVIOR.store(enabled, Ordering::Relaxed);
}

fn should_use_process_table_test_behavior() -> bool {
    FORCE_TEST_PROCESS_TABLE_BEHAVIOR.load(Ordering::Relaxed)
}

struct TempChaosHomeGuard {
    path: PathBuf,
}

impl Drop for TempChaosHomeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn build_file_watcher(chaos_home: PathBuf, skills_manager: Arc<SkillsManager>) -> Arc<FileWatcher> {
    if should_use_process_table_test_behavior()
        && let Ok(handle) = Handle::try_current()
        && handle.runtime_flavor() == RuntimeFlavor::CurrentThread
    {
        // The real watcher spins background tasks that can starve the
        // current-thread test runtime and cause event waits to time out.
        warn!("using noop file watcher under current-thread test runtime");
        return Arc::new(FileWatcher::noop());
    }

    let file_watcher = match FileWatcher::new(chaos_home) {
        Ok(file_watcher) => Arc::new(file_watcher),
        Err(err) => {
            warn!("failed to initialize file watcher: {err}");
            Arc::new(FileWatcher::noop())
        }
    };

    let mut rx = file_watcher.subscribe();
    let skills_manager = Arc::clone(&skills_manager);
    if let Ok(handle) = Handle::try_current() {
        handle.spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(FileWatcherEvent::SkillsChanged { .. }) => {
                        skills_manager.clear_cache();
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });
    } else {
        warn!("file watcher listener skipped: no Tokio runtime available");
    }

    file_watcher
}

/// Represents a newly created process, including the first event
/// (which is [`EventMsg::SessionConfigured`]).
pub struct NewProcess {
    pub process_id: ProcessId,
    pub process: Arc<Process>,
    pub session_configured: SessionConfiguredEvent,
}

impl NewProcess {
    pub fn process_id(&self) -> ProcessId {
        self.process_id
    }

    pub fn process(&self) -> Arc<Process> {
        Arc::clone(&self.process)
    }

    pub fn into_parts(self) -> (ProcessId, Arc<Process>, SessionConfiguredEvent) {
        (self.process_id, self.process, self.session_configured)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProcessShutdownReport {
    pub completed: Vec<ProcessId>,
    pub submit_failed: Vec<ProcessId>,
    pub timed_out: Vec<ProcessId>,
}

enum ShutdownOutcome {
    Complete,
    SubmitFailed,
    TimedOut,
}

/// [`ProcessTable`] is responsible for creating processes and maintaining
/// them in memory.
pub struct ProcessTable {
    state: Arc<ProcessTableState>,
    _test_chaos_home_guard: Option<TempChaosHomeGuard>,
}

/// Shared, `Arc`-owned state for [`ProcessTable`]. This `Arc` is required to have a single
/// `Arc` reference that can be downgraded to by `AgentControl` while preventing every single
/// function to require an `Arc<&Self>`.
pub(crate) struct ProcessTableState {
    processes: Arc<RwLock<HashMap<ProcessId, Arc<Process>>>>,
    closed_process_histories: Arc<RwLock<HashMap<ProcessId, Vec<RolloutItem>>>>,
    process_created_tx: broadcast::Sender<ProcessId>,
    auth_manager: Arc<AuthManager>,
    models_manager: Arc<ModelsManager>,
    skills_manager: Arc<SkillsManager>,
    mcp_manager: Arc<McpManager>,
    file_watcher: Arc<FileWatcher>,
    session_source: SessionSource,
    // Captures submitted ops for testing purpose when test mode is enabled.
    ops_log: Option<SharedCapturedOps>,
}

impl ProcessTable {
    pub fn new(
        config: &Config,
        auth_manager: Arc<AuthManager>,
        session_source: SessionSource,
        collaboration_modes_config: CollaborationModesConfig,
    ) -> Self {
        let chaos_home = config.chaos_home.clone();
        // Use the active model provider for discovery, not hardcoded OpenAI.
        let models_provider = config.model_provider.clone();
        let (process_created_tx, _) = broadcast::channel(PROCESS_CREATED_CHANNEL_CAPACITY);
        let mcp_manager = Arc::new(McpManager::new());
        let skills_manager = Arc::new(SkillsManager::new(
            chaos_home.clone(),
            config.bundled_skills_enabled(),
        ));
        let file_watcher = build_file_watcher(chaos_home.clone(), Arc::clone(&skills_manager));
        Self {
            state: Arc::new(ProcessTableState {
                processes: Arc::new(RwLock::new(HashMap::new())),
                closed_process_histories: Arc::new(RwLock::new(HashMap::new())),
                process_created_tx,
                models_manager: Arc::new(ModelsManager::new_with_provider(
                    chaos_home,
                    auth_manager.clone(),
                    config.model_catalog.clone(),
                    collaboration_modes_config,
                    models_provider,
                )),
                skills_manager,
                mcp_manager,
                file_watcher,
                auth_manager,
                session_source,
                ops_log: should_use_process_table_test_behavior()
                    .then(|| Arc::new(std::sync::Mutex::new(Vec::new()))),
            }),
            _test_chaos_home_guard: None,
        }
    }

    /// Construct with a dummy AuthManager containing the provided ChaosAuth.
    /// Used for integration tests: should not be used by ordinary business logic.
    pub(crate) fn with_models_provider_for_tests(
        auth: ChaosAuth,
        provider: ModelProviderInfo,
    ) -> Self {
        set_process_table_test_mode_for_tests(/*enabled*/ true);
        let chaos_home = std::env::temp_dir().join(format!(
            "chaos-thread-manager-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&chaos_home)
            .unwrap_or_else(|err| panic!("temp chaos home dir create failed: {err}"));
        let mut manager =
            Self::with_models_provider_and_home_for_tests(auth, provider, chaos_home.clone());
        manager._test_chaos_home_guard = Some(TempChaosHomeGuard { path: chaos_home });
        manager
    }

    /// Construct with a dummy AuthManager containing the provided ChaosAuth and chaos home.
    /// Used for integration tests: should not be used by ordinary business logic.
    pub(crate) fn with_models_provider_and_home_for_tests(
        auth: ChaosAuth,
        provider: ModelProviderInfo,
        chaos_home: PathBuf,
    ) -> Self {
        set_process_table_test_mode_for_tests(/*enabled*/ true);
        let auth_manager = AuthManager::from_auth_for_testing(auth);
        let (process_created_tx, _) = broadcast::channel(PROCESS_CREATED_CHANNEL_CAPACITY);
        let mcp_manager = Arc::new(McpManager::new());
        let skills_manager = Arc::new(SkillsManager::new(
            chaos_home.clone(),
            /*bundled_skills_enabled*/ true,
        ));
        let file_watcher = build_file_watcher(chaos_home.clone(), Arc::clone(&skills_manager));
        Self {
            state: Arc::new(ProcessTableState {
                processes: Arc::new(RwLock::new(HashMap::new())),
                closed_process_histories: Arc::new(RwLock::new(HashMap::new())),
                process_created_tx,
                models_manager: Arc::new(ModelsManager::with_provider_for_tests(
                    chaos_home,
                    auth_manager.clone(),
                    provider,
                )),
                skills_manager,
                mcp_manager,
                file_watcher,
                auth_manager,
                session_source: SessionSource::Exec,
                ops_log: should_use_process_table_test_behavior()
                    .then(|| Arc::new(std::sync::Mutex::new(Vec::new()))),
            }),
            _test_chaos_home_guard: None,
        }
    }

    pub fn session_source(&self) -> SessionSource {
        self.state.session_source.clone()
    }

    pub fn skills_manager(&self) -> Arc<SkillsManager> {
        self.state.skills_manager.clone()
    }

    pub fn mcp_manager(&self) -> Arc<McpManager> {
        self.state.mcp_manager.clone()
    }

    pub fn subscribe_file_watcher(&self) -> broadcast::Receiver<FileWatcherEvent> {
        self.state.file_watcher.subscribe()
    }

    pub fn get_models_manager(&self) -> Arc<ModelsManager> {
        self.state.models_manager.clone()
    }

    pub async fn list_models(
        &self,
        refresh_strategy: crate::models_manager::manager::RefreshStrategy,
    ) -> Vec<ModelPreset> {
        self.state
            .models_manager
            .list_models(refresh_strategy)
            .await
    }

    pub fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        self.state.models_manager.list_collaboration_modes()
    }

    pub async fn list_process_ids(&self) -> Vec<ProcessId> {
        self.state.list_process_ids().await
    }

    pub async fn refresh_mcp_servers(&self, refresh_config: McpServerRefreshConfig) {
        let processes = self
            .state
            .processes
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for process in processes {
            if let Err(err) = process
                .submit(Op::RefreshMcpServers {
                    config: refresh_config.clone(),
                })
                .await
            {
                warn!("failed to request MCP server refresh: {err}");
            }
        }
    }

    /// Reload the project `.mcp.json` config layer for a specific process and
    /// refresh its MCP server registry using the canonical session path.
    pub async fn reload_project_mcp_for_process(&self, process_id: ProcessId) -> ChaosResult<()> {
        let process = self.state.get_process(process_id).await?;
        let turn_context = process.chaos.session.new_default_turn().await;
        process
            .chaos
            .session
            .reload_project_mcp_layer_and_refresh(turn_context.as_ref())
            .await;
        Ok(())
    }

    pub fn subscribe_process_created(&self) -> broadcast::Receiver<ProcessId> {
        self.state.process_created_tx.subscribe()
    }

    pub async fn get_process(&self, process_id: ProcessId) -> ChaosResult<Arc<Process>> {
        self.state.get_process(process_id).await
    }

    pub async fn start_process(&self, config: Config) -> ChaosResult<NewProcess> {
        // Box delegated thread-spawn futures so these convenience wrappers do
        // not inline the full spawn path into every caller's async state.
        Box::pin(self.start_process_with_tools(
            config,
            Vec::new(),
            /*persist_extended_history*/ false,
        ))
        .await
    }

    pub async fn start_process_with_tools(
        &self,
        config: Config,
        dynamic_tools: Vec<chaos_ipc::dynamic_tools::DynamicToolSpec>,
        persist_extended_history: bool,
    ) -> ChaosResult<NewProcess> {
        Box::pin(self.start_process_with_tools_and_service_name(
            config,
            dynamic_tools,
            persist_extended_history,
            /*metrics_service_name*/ None,
            /*parent_trace*/ None,
        ))
        .await
    }

    pub async fn start_process_with_tools_and_service_name(
        &self,
        config: Config,
        dynamic_tools: Vec<chaos_ipc::dynamic_tools::DynamicToolSpec>,
        persist_extended_history: bool,
        metrics_service_name: Option<String>,
        parent_trace: Option<W3cTraceContext>,
    ) -> ChaosResult<NewProcess> {
        Box::pin(self.state.spawn_process(
            config,
            InitialHistory::New,
            Arc::clone(&self.state.auth_manager),
            self.agent_control(),
            dynamic_tools,
            persist_extended_history,
            metrics_service_name,
            parent_trace,
        ))
        .await
    }

    pub async fn resume_process(
        &self,
        config: Config,
        process_id: ProcessId,
        auth_manager: Arc<AuthManager>,
        parent_trace: Option<W3cTraceContext>,
    ) -> ChaosResult<NewProcess> {
        let initial_history = RolloutRecorder::get_rollout_history_for_process(process_id).await?;
        Box::pin(self.resume_process_with_history(
            config,
            initial_history,
            auth_manager,
            /*persist_extended_history*/ false,
            parent_trace,
        ))
        .await
    }

    pub async fn resume_process_with_history(
        &self,
        config: Config,
        initial_history: InitialHistory,
        auth_manager: Arc<AuthManager>,
        persist_extended_history: bool,
        parent_trace: Option<W3cTraceContext>,
    ) -> ChaosResult<NewProcess> {
        Box::pin(self.state.spawn_process(
            config,
            initial_history,
            auth_manager,
            self.agent_control(),
            Vec::new(),
            persist_extended_history,
            /*metrics_service_name*/ None,
            parent_trace,
        ))
        .await
    }

    /// Removes the process from the manager's internal map. Though the process is
    /// stored as `Arc<Process>`, other references may still exist elsewhere.
    /// Returns the process if it was found and removed.
    pub async fn remove_process(&self, process_id: &ProcessId) -> Option<Arc<Process>> {
        self.state.remove_process(process_id).await
    }

    /// Tries to shut down all tracked processes concurrently within the provided timeout.
    /// Processes that complete shutdown are removed from the manager; incomplete shutdowns
    /// remain tracked so callers can retry or inspect them later.
    pub async fn shutdown_all_processes_bounded(&self, timeout: Duration) -> ProcessShutdownReport {
        let processes = {
            let processes = self.state.processes.read().await;
            processes
                .iter()
                .map(|(process_id, process)| (*process_id, Arc::clone(process)))
                .collect::<Vec<_>>()
        };

        let mut shutdowns = processes
            .into_iter()
            .map(|(process_id, process)| async move {
                let outcome = match tokio::time::timeout(timeout, process.shutdown_and_wait()).await
                {
                    Ok(Ok(())) => ShutdownOutcome::Complete,
                    Ok(Err(_)) => ShutdownOutcome::SubmitFailed,
                    Err(_) => ShutdownOutcome::TimedOut,
                };
                (process_id, outcome)
            })
            .collect::<FuturesUnordered<_>>();
        let mut report = ProcessShutdownReport::default();

        while let Some((process_id, outcome)) = shutdowns.next().await {
            match outcome {
                ShutdownOutcome::Complete => report.completed.push(process_id),
                ShutdownOutcome::SubmitFailed => report.submit_failed.push(process_id),
                ShutdownOutcome::TimedOut => report.timed_out.push(process_id),
            }
        }

        let mut tracked_processes = self.state.processes.write().await;
        for process_id in &report.completed {
            tracked_processes.remove(process_id);
        }

        report
            .completed
            .sort_by_key(std::string::ToString::to_string);
        report
            .submit_failed
            .sort_by_key(std::string::ToString::to_string);
        report
            .timed_out
            .sort_by_key(std::string::ToString::to_string);
        report
    }

    pub async fn fork_process_by_id(
        &self,
        nth_user_message: usize,
        config: Config,
        process_id: ProcessId,
        persist_extended_history: bool,
        parent_trace: Option<W3cTraceContext>,
    ) -> ChaosResult<NewProcess> {
        let history = RolloutRecorder::get_rollout_history_for_process(process_id).await?;
        let history = truncate_before_nth_user_message(history, nth_user_message);
        Box::pin(self.state.spawn_process(
            config,
            history,
            Arc::clone(&self.state.auth_manager),
            self.agent_control(),
            Vec::new(),
            persist_extended_history,
            /*metrics_service_name*/ None,
            parent_trace,
        ))
        .await
    }

    pub(crate) fn agent_control(&self) -> AgentControl {
        AgentControl::new(Arc::downgrade(&self.state))
    }

    #[cfg(test)]
    pub(crate) fn captured_ops(&self) -> Vec<(ProcessId, Op)> {
        self.state
            .ops_log
            .as_ref()
            .and_then(|ops_log| ops_log.lock().ok().map(|log| log.clone()))
            .unwrap_or_default()
    }
}

impl ProcessTableState {
    pub(crate) async fn list_process_ids(&self) -> Vec<ProcessId> {
        self.processes.read().await.keys().copied().collect()
    }

    /// Fetch a process by ID or return ProcessNotFound.
    pub(crate) async fn get_process(&self, process_id: ProcessId) -> ChaosResult<Arc<Process>> {
        let processes = self.processes.read().await;
        processes
            .get(&process_id)
            .cloned()
            .ok_or_else(|| ChaosErr::ProcessNotFound(process_id))
    }

    /// Send an operation to a process by ID.
    pub(crate) async fn send_op(&self, process_id: ProcessId, op: Op) -> ChaosResult<String> {
        let process = self.get_process(process_id).await?;
        if let Some(ops_log) = &self.ops_log
            && let Ok(mut log) = ops_log.lock()
        {
            log.push((process_id, op.clone()));
        }
        process.submit(op).await
    }

    /// Remove a process from the manager by ID, returning it when present.
    pub(crate) async fn remove_process(&self, process_id: &ProcessId) -> Option<Arc<Process>> {
        let removed = self.processes.write().await.remove(process_id);
        if let Some(process) = removed.as_ref() {
            let snapshot = process
                .chaos
                .session
                .clone_history()
                .await
                .raw_items()
                .iter()
                .cloned()
                .map(RolloutItem::ResponseItem)
                .collect::<Vec<_>>();
            if !snapshot.is_empty() {
                self.closed_process_histories
                    .write()
                    .await
                    .insert(*process_id, snapshot);
            }
        }
        removed
    }

    /// Spawn a new thread with no history using a provided config.
    pub(crate) async fn spawn_new_process(
        &self,
        config: Config,
        agent_control: AgentControl,
    ) -> ChaosResult<NewProcess> {
        Box::pin(self.spawn_new_process_with_source(
            config,
            agent_control,
            self.session_source.clone(),
            /*persist_extended_history*/ false,
            /*metrics_service_name*/ None,
            /*inherited_shell_snapshot*/ None,
        ))
        .await
    }

    pub(crate) async fn spawn_new_process_with_source(
        &self,
        config: Config,
        agent_control: AgentControl,
        session_source: SessionSource,
        persist_extended_history: bool,
        metrics_service_name: Option<String>,
        inherited_shell_snapshot: Option<Arc<ShellSnapshot>>,
    ) -> ChaosResult<NewProcess> {
        Box::pin(self.spawn_process_with_source(
            config,
            InitialHistory::New,
            Arc::clone(&self.auth_manager),
            agent_control,
            session_source,
            Vec::new(),
            persist_extended_history,
            metrics_service_name,
            inherited_shell_snapshot,
            /*parent_trace*/ None,
        ))
        .await
    }

    pub(crate) async fn resume_process_with_source(
        &self,
        config: Config,
        process_id: ProcessId,
        agent_control: AgentControl,
        session_source: SessionSource,
        inherited_shell_snapshot: Option<Arc<ShellSnapshot>>,
    ) -> ChaosResult<NewProcess> {
        let stashed_history = self
            .closed_process_histories
            .read()
            .await
            .get(&process_id)
            .cloned();
        let initial_history = match RolloutRecorder::journal_contains_process(process_id).await {
            Ok(true) => RolloutRecorder::get_rollout_history_for_process(process_id).await?,
            Ok(false) => stashed_history
                .map(|history| {
                    InitialHistory::Resumed(ResumedHistory {
                        conversation_id: process_id,
                        history,
                    })
                })
                .ok_or(ChaosErr::ProcessNotFound(process_id))?,
            Err(err) => match stashed_history {
                Some(history) => InitialHistory::Resumed(ResumedHistory {
                    conversation_id: process_id,
                    history,
                }),
                None => {
                    tracing::warn!(
                        process_id = %process_id,
                        error = %err,
                        "journal lookup failed while resuming agent without local fallback history"
                    );
                    return Err(ChaosErr::ProcessNotFound(process_id));
                }
            },
        };
        Box::pin(self.spawn_process_with_source(
            config,
            initial_history,
            Arc::clone(&self.auth_manager),
            agent_control,
            session_source,
            Vec::new(),
            /*persist_extended_history*/ false,
            /*metrics_service_name*/ None,
            inherited_shell_snapshot,
            /*parent_trace*/ None,
        ))
        .await
    }

    pub(crate) async fn fork_process_with_source(
        &self,
        config: Config,
        initial_history: InitialHistory,
        agent_control: AgentControl,
        session_source: SessionSource,
        persist_extended_history: bool,
        inherited_shell_snapshot: Option<Arc<ShellSnapshot>>,
    ) -> ChaosResult<NewProcess> {
        Box::pin(self.spawn_process_with_source(
            config,
            initial_history,
            Arc::clone(&self.auth_manager),
            agent_control,
            session_source,
            Vec::new(),
            persist_extended_history,
            /*metrics_service_name*/ None,
            inherited_shell_snapshot,
            /*parent_trace*/ None,
        ))
        .await
    }

    /// Spawn a new thread with optional history and register it with the manager.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn spawn_process(
        &self,
        config: Config,
        initial_history: InitialHistory,
        auth_manager: Arc<AuthManager>,
        agent_control: AgentControl,
        dynamic_tools: Vec<chaos_ipc::dynamic_tools::DynamicToolSpec>,
        persist_extended_history: bool,
        metrics_service_name: Option<String>,
        parent_trace: Option<W3cTraceContext>,
    ) -> ChaosResult<NewProcess> {
        Box::pin(self.spawn_process_with_source(
            config,
            initial_history,
            auth_manager,
            agent_control,
            self.session_source.clone(),
            dynamic_tools,
            persist_extended_history,
            metrics_service_name,
            /*inherited_shell_snapshot*/ None,
            parent_trace,
        ))
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn spawn_process_with_source(
        &self,
        config: Config,
        initial_history: InitialHistory,
        auth_manager: Arc<AuthManager>,
        agent_control: AgentControl,
        session_source: SessionSource,
        dynamic_tools: Vec<chaos_ipc::dynamic_tools::DynamicToolSpec>,
        persist_extended_history: bool,
        metrics_service_name: Option<String>,
        inherited_shell_snapshot: Option<Arc<ShellSnapshot>>,
        parent_trace: Option<W3cTraceContext>,
    ) -> ChaosResult<NewProcess> {
        let watch_registration = self
            .file_watcher
            .register_config(&config, self.skills_manager.as_ref());
        let ChaosSpawnOk {
            chaos, process_id, ..
        } = Chaos::spawn(ChaosSpawnArgs {
            config,
            auth_manager,
            models_manager: Arc::clone(&self.models_manager),
            skills_manager: Arc::clone(&self.skills_manager),
            mcp_manager: Arc::clone(&self.mcp_manager),
            file_watcher: Arc::clone(&self.file_watcher),
            conversation_history: initial_history,
            session_source,
            agent_control,
            dynamic_tools,
            persist_extended_history,
            metrics_service_name,
            inherited_shell_snapshot,
            parent_trace,
        })
        .await?;
        self.finalize_process_spawn(chaos, process_id, watch_registration)
            .await
    }

    async fn finalize_process_spawn(
        &self,
        chaos: Chaos,
        process_id: ProcessId,
        watch_registration: crate::file_watcher::WatchRegistration,
    ) -> ChaosResult<NewProcess> {
        let event = chaos.next_event().await?;
        let session_configured = match event {
            Event {
                id,
                msg: EventMsg::SessionConfigured(session_configured),
            } if id == INITIAL_SUBMIT_ID => session_configured,
            _ => {
                return Err(ChaosErr::SessionConfiguredNotFirstEvent);
            }
        };

        let process = Arc::new(Process::new(chaos, watch_registration));
        {
            let mut processes = self.processes.write().await;
            processes.insert(process_id, process.clone());
        }
        self.closed_process_histories
            .write()
            .await
            .remove(&process_id);

        Ok(NewProcess {
            process_id,
            process,
            session_configured,
        })
    }

    pub(crate) fn notify_process_created(&self, process_id: ProcessId) {
        let _ = self.process_created_tx.send(process_id);
    }
}

/// Return a prefix of `items` obtained by cutting strictly before the nth user message
/// (0-based) and all items that follow it.
fn truncate_before_nth_user_message(history: InitialHistory, n: usize) -> InitialHistory {
    let items: Vec<RolloutItem> = history.get_rollout_items();
    let rolled = truncation::truncate_rollout_before_nth_user_message_from_start(&items, n);

    if rolled.is_empty() {
        InitialHistory::New
    } else {
        InitialHistory::Forked(rolled)
    }
}

#[cfg(test)]
#[path = "process_table_tests.rs"]
mod tests;
