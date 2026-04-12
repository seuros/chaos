use std::sync::Arc;

use async_channel::Receiver;
use async_channel::Sender;
use chaos_ipc::ProcessId;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::SubAgentSource;
use chaos_ipc::protocol::Submission;
use chaos_ipc::protocol::W3cTraceContext;
use chaos_ipc::user_input::UserInput;
use chaos_syslog::current_span_w3c_trace_context;
use chaos_syslog::set_parent_from_w3c_trace_context;
use futures::future::BoxFuture;
use futures::future::Shared;
use futures::FutureExt;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::info_span;
use tracing::warn;
use tracing::Instrument;
use uuid::Uuid;

use crate::AuthManager;
use crate::config::Config;
use crate::config::ConstraintResult;
use crate::config::ManagedFeatures;
use crate::error::ChaosErr;
use crate::error::Result as ChaosResult;
use crate::exec_policy::ExecPolicyManager;
use crate::features::Feature;
use crate::file_watcher::FileWatcher;
use crate::minions::AgentControl;
use crate::minions::AgentStatus;
use crate::mcp::McpManager;
use crate::models_manager::manager::ModelsManager;
use crate::models_manager::manager::RefreshStrategy;
use crate::process::ProcessConfigSnapshot;
use crate::rollout::map_session_init_error;
use crate::rollout::RolloutRecorder;
use crate::rollout::RolloutRecorderParams;
use crate::runtime_db;
use crate::shell_snapshot::ShellSnapshot;
use crate::skills::SkillsManager;

use super::Session;
use super::SessionConfiguration;
use super::SessionSettingsUpdate;
use super::SteerInputError;
use super::submission_loop::submission_loop;
use super::turn_context::make_turn_context;

pub(crate) type SessionLoopTermination = Shared<BoxFuture<'static, ()>>;

/// The high-level interface to the Chaos system.
/// It operates as a queue pair where you send submissions and receive events.
pub struct Chaos {
    pub(crate) tx_sub: Sender<Submission>,
    pub(crate) rx_event: Receiver<Event>,
    // Last known status of the agent.
    pub(crate) agent_status: watch::Receiver<AgentStatus>,
    pub(crate) session: Arc<Session>,
    // Shared future for the background submission loop completion so multiple
    // callers can wait for shutdown.
    pub(crate) session_loop_termination: SessionLoopTermination,
}

/// Wrapper returned by [`Chaos::spawn`] containing the spawned [`Chaos`],
/// the submission id for the initial `ConfigureSession` request and the
/// unique session id.
pub struct ChaosSpawnOk {
    pub chaos: Chaos,
    pub process_id: ProcessId,
    pub conversation_id: ProcessId,
}

pub(crate) struct ChaosSpawnArgs {
    pub(crate) config: Config,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) models_manager: Arc<ModelsManager>,
    pub(crate) skills_manager: Arc<SkillsManager>,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) file_watcher: Arc<FileWatcher>,
    pub(crate) conversation_history: chaos_ipc::protocol::InitialHistory,
    pub(crate) session_source: SessionSource,
    pub(crate) agent_control: AgentControl,
    pub(crate) dynamic_tools: Vec<DynamicToolSpec>,
    pub(crate) persist_extended_history: bool,
    pub(crate) metrics_service_name: Option<String>,
    pub(crate) inherited_shell_snapshot: Option<Arc<ShellSnapshot>>,
    pub(crate) parent_trace: Option<W3cTraceContext>,
}

pub(crate) const INITIAL_SUBMIT_ID: &str = "";
pub(crate) const SUBMISSION_CHANNEL_CAPACITY: usize = 512;

impl Chaos {
    /// Spawn a new [`Chaos`] and initialize the session.
    pub(crate) async fn spawn(args: ChaosSpawnArgs) -> ChaosResult<ChaosSpawnOk> {
        let parent_trace = match args.parent_trace {
            Some(trace) => {
                if chaos_syslog::context_from_w3c_trace_context(&trace).is_some() {
                    Some(trace)
                } else {
                    warn!("ignoring invalid thread spawn trace carrier");
                    None
                }
            }
            None => None,
        };
        let process_spawn_span = info_span!("process_spawn", otel.name = "process_spawn");
        if let Some(trace) = parent_trace.as_ref() {
            let _ = set_parent_from_w3c_trace_context(&process_spawn_span, trace);
        }
        Self::spawn_internal(ChaosSpawnArgs {
            parent_trace,
            ..args
        })
        .instrument(process_spawn_span)
        .await
    }

    async fn spawn_internal(args: ChaosSpawnArgs) -> ChaosResult<ChaosSpawnOk> {
        use crate::features::Feature;
        use crate::models_manager::manager::RefreshStrategy;
        use crate::project_doc::get_user_instructions;
        use crate::rollout::policy::EventPersistenceMode;
        use crate::skills::SkillsManager;
        use chaos_ipc::config_types::CollaborationMode;
        use chaos_ipc::config_types::ModeKind;
        use chaos_ipc::config_types::Settings;
        use chaos_ipc::models::BaseInstructions;
        use chaos_ipc::protocol::InitialHistory;
        use tracing::error;

        let ChaosSpawnArgs {
            mut config,
            auth_manager,
            models_manager,
            skills_manager,
            mcp_manager,
            file_watcher,
            conversation_history,
            session_source,
            agent_control,
            dynamic_tools,
            persist_extended_history,
            metrics_service_name,
            inherited_shell_snapshot,
            parent_trace: _,
        } = args;
        let (tx_sub, rx_sub) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);
        let (tx_event, rx_event) = async_channel::unbounded();

        let loaded_skills = skills_manager.skills_for_config(&config);

        for err in &loaded_skills.errors {
            tracing::error!(
                "failed to load skill {}: {}",
                err.path.display(),
                err.message
            );
        }

        if let SessionSource::SubAgent(SubAgentSource::ProcessSpawn { depth, .. }) = session_source
            && depth >= config.agent_max_depth
        {
            let _ = config.features.disable(Feature::SpawnCsv);
            config.collab_enabled = false;
        }

        let user_instructions = get_user_instructions(&config).await;

        let exec_policy = ExecPolicyManager::load(&config.config_layer_stack)
            .await
            .map_err(|err| ChaosErr::Fatal(format!("failed to load rules: {err}")))?;

        let config = Arc::new(config);
        let refresh_strategy = match session_source {
            SessionSource::SubAgent(_) => RefreshStrategy::Offline,
            _ => RefreshStrategy::OnlineIfUncached,
        };
        if config.model.is_none()
            || !matches!(refresh_strategy, RefreshStrategy::Offline)
        {
            let _ = models_manager.list_models(refresh_strategy).await;
        }
        let model = models_manager
            .get_default_model(&config.model, refresh_strategy)
            .await;

        // Resolve base instructions for the session. Priority order:
        // 1. config.base_instructions override
        // 2. conversation history => session_meta.base_instructions
        // 3. base_instructions for current model
        let model_info = models_manager.get_model_info(model.as_str(), &config).await;
        let base_instructions = config
            .base_instructions
            .clone()
            .or_else(|| conversation_history.get_base_instructions().map(|s| s.text))
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality));

        // Respect process-start tools. When missing (resumed/forked processes), read from the db
        // first, then fall back to rollout-file tools.
        let persisted_tools = if dynamic_tools.is_empty() {
            let process_id = match &conversation_history {
                InitialHistory::Resumed(resumed) => Some(resumed.conversation_id),
                InitialHistory::Forked(_) => conversation_history.forked_from_id(),
                InitialHistory::New => None,
            };
            match process_id {
                Some(process_id) => {
                    let runtime_db_ctx = runtime_db::get_runtime_db(&config).await;
                    runtime_db::get_dynamic_tools(
                        runtime_db_ctx.as_deref(),
                        process_id,
                        "codex_spawn",
                    )
                    .await
                }
                None => None,
            }
        } else {
            None
        };
        let dynamic_tools = if dynamic_tools.is_empty() {
            persisted_tools
                .or_else(|| conversation_history.get_dynamic_tools())
                .unwrap_or_default()
        } else {
            dynamic_tools
        };

        // TODO (aibrahim): Consolidate config.model and config.model_reasoning_effort into config.collaboration_mode
        // to avoid extracting these fields separately and constructing CollaborationMode here.
        let collaboration_mode = CollaborationMode {
            mode: ModeKind::Default,
            settings: Settings {
                model: model.clone(),
                reasoning_effort: config.model_reasoning_effort,
                minion_instructions: None,
            },
        };
        let session_configuration = SessionConfiguration {
            provider: config.model_provider.clone(),
            collaboration_mode,
            model_reasoning_summary: config.model_reasoning_summary,
            service_tier: config.service_tier,
            minion_instructions: config.minion_instructions.clone(),
            user_instructions,
            personality: config.personality,
            base_instructions,
            compact_prompt: config.compact_prompt.clone(),
            approval_policy: config.permissions.approval_policy.clone(),
            approvals_reviewer: config.approvals_reviewer,
            sandbox_policy: config.permissions.sandbox_policy.clone(),
            file_system_sandbox_policy: config.permissions.file_system_sandbox_policy.clone(),
            network_sandbox_policy: config.permissions.network_sandbox_policy,
            cwd: config.cwd.clone(),
            chaos_home: config.chaos_home.clone(),
            process_name: None,
            original_config_do_not_use: Arc::clone(&config),
            metrics_service_name,
            app_server_client_name: None,
            session_source,
            dynamic_tools,
            persist_extended_history,
            inherited_shell_snapshot,
        };

        // Generate a unique ID for the lifetime of this Chaos session.
        let session_source_clone = session_configuration.session_source.clone();
        let (agent_status_tx, agent_status_rx) = watch::channel(AgentStatus::PendingInit);

        let session = Session::new(
            session_configuration,
            config.clone(),
            auth_manager.clone(),
            models_manager.clone(),
            exec_policy,
            tx_event.clone(),
            agent_status_tx.clone(),
            conversation_history,
            session_source_clone,
            skills_manager,
            mcp_manager.clone(),
            file_watcher,
            agent_control,
        )
        .await
        .map_err(|e| {
            error!("Failed to create session: {e:#}");
            map_session_init_error(&e, &config.chaos_home)
        })?;
        let process_id = session.conversation_id;

        // This task will run until Op::Shutdown is received.
        let session_for_loop = Arc::clone(&session);
        let session_loop_handle = tokio::spawn(async move {
            submission_loop(session_for_loop, config, rx_sub)
                .instrument(info_span!("session_loop", process_id = %process_id))
                .await;
        });
        let chaos = Chaos {
            tx_sub,
            rx_event,
            agent_status: agent_status_rx,
            session,
            session_loop_termination: session_loop_termination_from_handle(session_loop_handle),
        };

        Ok(ChaosSpawnOk {
            chaos,
            process_id,
            conversation_id: process_id,
        })
    }

    /// Submit the `op` wrapped in a `Submission` with a unique ID.
    pub async fn submit(&self, op: chaos_ipc::protocol::Op) -> ChaosResult<String> {
        self.submit_with_trace(op, /*trace*/ None).await
    }

    pub async fn submit_with_trace(
        &self,
        op: chaos_ipc::protocol::Op,
        trace: Option<W3cTraceContext>,
    ) -> ChaosResult<String> {
        let id = Uuid::now_v7().to_string();
        let sub = Submission {
            id: id.clone(),
            op,
            trace,
        };
        self.submit_with_id(sub).await?;
        Ok(id)
    }

    /// Use sparingly: prefer `submit()` so Chaos is responsible for generating
    /// unique IDs for each submission.
    pub async fn submit_with_id(&self, mut sub: Submission) -> ChaosResult<()> {
        if sub.trace.is_none() {
            sub.trace = current_span_w3c_trace_context();
        }
        self.tx_sub
            .send(sub)
            .await
            .map_err(|_| ChaosErr::InternalAgentDied)?;
        Ok(())
    }

    pub async fn shutdown_and_wait(&self) -> ChaosResult<()> {
        let session_loop_termination = self.session_loop_termination.clone();
        match self.submit(chaos_ipc::protocol::Op::Shutdown).await {
            Ok(_) => {}
            Err(ChaosErr::InternalAgentDied) => {}
            Err(err) => return Err(err),
        }
        session_loop_termination.await;
        Ok(())
    }

    pub async fn next_event(&self) -> ChaosResult<Event> {
        let event = self
            .rx_event
            .recv()
            .await
            .map_err(|_| ChaosErr::InternalAgentDied)?;
        Ok(event)
    }

    pub async fn steer_input(
        &self,
        input: Vec<UserInput>,
        expected_turn_id: Option<&str>,
    ) -> Result<String, SteerInputError> {
        self.session.steer_input(input, expected_turn_id).await
    }

    pub(crate) async fn set_app_server_client_name(
        &self,
        app_server_client_name: Option<String>,
    ) -> ConstraintResult<()> {
        self.session
            .update_settings(SessionSettingsUpdate {
                app_server_client_name,
                ..Default::default()
            })
            .await
    }

    pub(crate) async fn agent_status(&self) -> AgentStatus {
        self.agent_status.borrow().clone()
    }

    pub(crate) async fn process_config_snapshot(&self) -> ProcessConfigSnapshot {
        let state = self.session.state.lock().await;
        state.session_configuration.process_config_snapshot()
    }

    pub(crate) fn runtime_db(&self) -> Option<runtime_db::RuntimeDbHandle> {
        self.session.runtime_db()
    }

    pub(crate) fn enabled(&self, feature: Feature) -> bool {
        self.session.enabled(feature)
    }
}

#[cfg(test)]
pub(crate) fn completed_session_loop_termination() -> SessionLoopTermination {
    futures::future::ready(()).boxed().shared()
}

pub(crate) fn session_loop_termination_from_handle(
    handle: JoinHandle<()>,
) -> SessionLoopTermination {
    async move {
        let _ = handle.await;
    }
    .boxed()
    .shared()
}
