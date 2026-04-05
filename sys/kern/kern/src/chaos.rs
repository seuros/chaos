use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crate::AuthManager;
use crate::ChaosAuth;
use crate::SandboxState;
use crate::analytics_client::AnalyticsEventsClient;
use crate::analytics_client::build_track_events_context;
use crate::compact;
use crate::compact::InitialContextInjection;
use crate::compact::collect_user_messages;
use crate::compact::run_inline_auto_compact_task;
use crate::compact::should_use_remote_compact_task;
use crate::compact_remote::run_inline_remote_auto_compact_task;
use crate::config::ManagedFeatures;
use crate::exec_policy::ExecPolicyManager;
use crate::features::Feature;
use crate::mcp::oauth_types::OAuthCredentialsStoreMode;
use crate::minions::AgentControl;
use crate::minions::AgentStatus;
use crate::minions::agent_status_from_event;
#[cfg(test)]
use crate::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use crate::models_manager::manager::ModelsManager;
use crate::models_manager::manager::RefreshStrategy;
use crate::parse_command::parse_command;
use crate::parse_turn_item;
use crate::rollout::process_names;
use crate::skills::render_skills_section;
use crate::stream_events_utils::HandleOutputCtx;
use crate::stream_events_utils::handle_non_tool_response_item;
use crate::stream_events_utils::handle_output_item_done;
use crate::stream_events_utils::last_assistant_message_from_item;
use crate::stream_events_utils::raw_assistant_output_text_from_item;
use crate::stream_events_utils::record_completed_response_item;
use crate::terminal;
use crate::truncate::TruncationPolicy;
use crate::turn_metadata::TurnMetadataState;
use crate::util::error_or_panic;
use crate::ws_version_from_features;
use async_channel::Receiver;
use async_channel::Sender;
use chaos_dtrace::HookEvent;
use chaos_dtrace::HookEventAfterAgent;
use chaos_dtrace::HookPayload;
use chaos_dtrace::HookResult;
use chaos_dtrace::Hooks;
use chaos_dtrace::HooksConfig;
use chaos_ipc::ProcessId;
use chaos_ipc::api::McpServerElicitationRequest;
use chaos_ipc::api::McpServerElicitationRequestParams;
use chaos_ipc::approvals::ElicitationRequestEvent;
use chaos_ipc::approvals::ExecApprovalRequestSkillMetadata;
use chaos_ipc::approvals::ExecPolicyAmendment;
use chaos_ipc::approvals::NetworkPolicyAmendment;
use chaos_ipc::approvals::NetworkPolicyRuleAction;
use chaos_ipc::config_types::ApprovalsReviewer;
use chaos_ipc::config_types::ModeKind;
use chaos_ipc::config_types::Settings;
use chaos_ipc::config_types::WebSearchMode;
use chaos_ipc::dynamic_tools::DynamicToolResponse;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::items::PlanItem;
use chaos_ipc::items::TurnItem;
use chaos_ipc::items::UserMessageItem;
use chaos_ipc::mcp::CallToolResult;
use chaos_ipc::models::BaseInstructions;
use chaos_ipc::models::PermissionProfile;
use chaos_ipc::models::format_allow_prefixes;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::permissions::FileSystemSandboxPolicy;
use chaos_ipc::permissions::NetworkSandboxPolicy;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::protocol::AgentMessageEvent;
use chaos_ipc::protocol::FileChange;
use chaos_ipc::protocol::ItemCompletedEvent;
use chaos_ipc::protocol::ItemStartedEvent;
use chaos_ipc::protocol::RawResponseItemEvent;
use chaos_ipc::protocol::ReviewRequest;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::SubAgentSource;
use chaos_ipc::protocol::TurnAbortReason;
use chaos_ipc::protocol::TurnContextItem;
use chaos_ipc::protocol::TurnContextNetworkItem;
use chaos_ipc::protocol::TurnStartedEvent;
use chaos_ipc::protocol::W3cTraceContext;
use chaos_ipc::request_permissions::PermissionGrantScope;
use chaos_ipc::request_permissions::RequestPermissionProfile;
use chaos_ipc::request_permissions::RequestPermissionsArgs;
use chaos_ipc::request_permissions::RequestPermissionsEvent;
use chaos_ipc::request_permissions::RequestPermissionsResponse;
use chaos_ipc::request_user_input::RequestUserInputArgs;
use chaos_ipc::request_user_input::RequestUserInputResponse;
use chaos_lex::AssistantTextChunk;
use chaos_lex::AssistantTextStreamParser;
use chaos_lex::ProposedPlanSegment;
use chaos_lex::extract_proposed_plan_text;
use chaos_lex::strip_citations;
use chaos_mcp_runtime::ElicitationResponse;
use chaos_mcp_runtime::ListResourceTemplatesResult;
use chaos_mcp_runtime::ListResourcesResult;
use chaos_mcp_runtime::McpRequestId as RequestId;
use chaos_mcp_runtime::PaginatedRequestParams;
use chaos_mcp_runtime::ReadResourceRequestParams;
use chaos_mcp_runtime::ReadResourceResult;
use chaos_pf::NetworkProxy;
use chaos_pf::NetworkProxyAuditMetadata;
use chaos_pf::normalize_host;
use chaos_syslog::current_span_trace_id;
use chaos_syslog::current_span_w3c_trace_context;
use chaos_syslog::set_parent_from_w3c_trace_context;
use futures::future::BoxFuture;
use futures::future::Shared;
use futures::prelude::*;
use futures::stream::FuturesOrdered;
use jiff::Timestamp;
use jiff::Zoned;
use serde_json;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::debug;
use tracing::error;
use tracing::field;
use tracing::info;
use tracing::info_span;
use tracing::instrument;
use tracing::trace;
use tracing::trace_span;
use tracing::warn;
use uuid::Uuid;

use crate::ModelProviderInfo;
use crate::client::ModelClient;
use crate::client::ModelClientSession;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::config::Config;
use crate::config::Constrained;
use crate::config::ConstraintResult;
use crate::config::GhostSnapshotConfig;
use crate::config::StartedNetworkProxy;
use crate::config::resolve_web_search_mode_for_turn;
use crate::config::types::McpServerConfig;
use crate::config::types::ShellEnvironmentPolicy;
use crate::context_manager::ContextManager;
use crate::context_manager::TotalTokenUsageBreakdown;
use crate::environment_context::EnvironmentContext;
use crate::error::ChaosErr;
use crate::error::Result as ChaosResult;
#[cfg(test)]
use crate::exec::StreamOutput;
use crate::process::ProcessConfigSnapshot;
use chaos_sysctl::CONFIG_TOML_FILE;

#[path = "chaos/rollout_reconstruction.rs"]
mod rollout_reconstruction;
#[cfg(test)]
#[path = "chaos/rollout_reconstruction_tests.rs"]
mod rollout_reconstruction_tests;

#[derive(Debug, PartialEq)]
pub enum SteerInputError {
    NoActiveTurn(Vec<UserInput>),
    ExpectedTurnMismatch { expected: String, actual: String },
    EmptyInput,
}

/// Notes from the previous real user turn.
///
/// Conceptually this is the same role that `previous_model` used to fill, but
/// it can carry other prior-turn settings that matter when constructing
/// sensible state-change diffs or full-context reinjection, such as model
/// switches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreviousTurnSettings {
    pub(crate) model: String,
}

use crate::exec_policy::ExecPolicyUpdateError;
use crate::feedback_tags;
use crate::file_watcher::FileWatcher;
use crate::file_watcher::FileWatcherEvent;
use crate::git_info::get_git_repo_root;

use crate::instructions::UserInstructions;
use crate::mcp::McpManager;
use crate::mcp::auth::compute_auth_statuses;
use crate::mcp::maybe_prompt_and_install_mcp_dependencies;
use crate::network_policy_decision::execpolicy_network_rule_amendment;
use crate::project_doc::get_user_instructions;
use crate::protocol::AgentMessageContentDeltaEvent;
use crate::protocol::AgentReasoningSectionBreakEvent;
use crate::protocol::ApplyPatchApprovalRequestEvent;
use crate::protocol::ApprovalPolicy;
use crate::protocol::BackgroundEventEvent;
use crate::protocol::CompactedItem;
use crate::protocol::DeprecationNoticeEvent;
use crate::protocol::ErrorEvent;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::ExecApprovalRequestEvent;
use crate::protocol::McpServerRefreshConfig;
use crate::protocol::ModelRerouteEvent;
use crate::protocol::ModelRerouteReason;
use crate::protocol::NetworkApprovalContext;
use crate::protocol::Op;
use crate::protocol::PlanDeltaEvent;
use crate::protocol::RateLimitSnapshot;
use crate::protocol::ReasoningContentDeltaEvent;
use crate::protocol::ReasoningRawContentDeltaEvent;
use crate::protocol::RequestUserInputEvent;
use crate::protocol::ReviewDecision;
use crate::protocol::SandboxPolicy;
use crate::protocol::SessionConfiguredEvent;
use crate::protocol::SessionNetworkProxyRuntime;
use crate::protocol::SkillDependencies as ProtocolSkillDependencies;
use crate::protocol::SkillErrorInfo;
use crate::protocol::SkillInterface as ProtocolSkillInterface;
use crate::protocol::SkillMetadata as ProtocolSkillMetadata;
use crate::protocol::SkillToolDependency as ProtocolSkillToolDependency;
use crate::protocol::StreamErrorEvent;
use crate::protocol::Submission;
use crate::protocol::TokenCountEvent;
use crate::protocol::TokenUsage;
use crate::protocol::TokenUsageInfo;
use crate::protocol::TurnDiffEvent;
use crate::protocol::WarningEvent;
use crate::rollout::RolloutRecorder;
use crate::rollout::RolloutRecorderParams;
use crate::rollout::map_session_init_error;
use crate::rollout::policy::EventPersistenceMode;
use crate::shell;
use crate::shell_snapshot::ShellSnapshot;
use crate::skills::SkillError;
use crate::skills::SkillInjections;
use crate::skills::SkillLoadOutcome;
use crate::skills::SkillMetadata;
use crate::skills::SkillsManager;
use crate::skills::build_skill_injections;
use crate::skills::collect_env_var_dependencies;
use crate::skills::collect_explicit_skill_mentions;
use crate::skills::resolve_skill_dependencies_for_turn;
use crate::state::ActiveTurn;
use crate::state::SessionServices;
use crate::state::SessionState;
use crate::state_db;
use crate::tasks::GhostSnapshotTask;
use crate::tasks::RegularTask;
use crate::tasks::ReviewTask;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskContext;
use crate::tools::ToolRouter;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::network_approval::NetworkApprovalService;
use crate::tools::network_approval::build_blocked_request_observer;
use crate::tools::network_approval::build_network_policy_decider;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::router::ToolRouterParams;
use crate::tools::sandboxing::ApprovalStore;
use crate::tools::spec::ToolsConfig;
use crate::tools::spec::ToolsConfigParams;
use crate::turn_diff_tracker::TurnDiffTracker;
use crate::turn_timing::TurnTimingState;
use crate::turn_timing::record_turn_ttfm_metric;
use crate::turn_timing::record_turn_ttft_metric;
use crate::unified_exec::UnifiedExecProcessManager;
use crate::util::backoff;
use chaos_epoll::OrCancelExt;
use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::config_types::Personality;
use chaos_ipc::config_types::ReasoningSummary as ReasoningSummaryConfig;
use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::config_types::WindowsSandboxLevel;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::DeveloperInstructions;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ReasoningEffort as ReasoningEffortConfig;
use chaos_ipc::protocol::CodexErrorInfo;
use chaos_ipc::protocol::InitialHistory;
use chaos_ipc::user_input::UserInput;
use chaos_mcp_runtime::manager::McpConnectionManager;
use chaos_ready::Readiness;
use chaos_ready::ReadinessFlag;
use chaos_realpath::AbsolutePathBuf;
use chaos_syslog::SessionTelemetry;
use chaos_syslog::TelemetryAuthMode;
use chaos_syslog::metrics::names::THREAD_STARTED_METRIC;

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

pub(crate) type SessionLoopTermination = Shared<BoxFuture<'static, ()>>;

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
    pub(crate) conversation_history: InitialHistory,
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
const CYBER_VERIFY_URL: &str = "https://chatgpt.com/cyber";
const CYBER_SAFETY_URL: &str = "https://developers.openai.com/codex/concepts/cyber-safety";
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
            error!(
                "failed to load skill {}: {}",
                err.path.display(),
                err.message
            );
        }

        if let SessionSource::SubAgent(SubAgentSource::ProcessSpawn { depth, .. }) = session_source
            && depth >= config.agent_max_depth
        {
            let _ = config.features.disable(Feature::SpawnCsv);
            let _ = config.features.disable(Feature::Collab);
        }

        let user_instructions = get_user_instructions(&config).await;

        let exec_policy = ExecPolicyManager::load(&config.config_layer_stack)
            .await
            .map_err(|err| ChaosErr::Fatal(format!("failed to load rules: {err}")))?;

        let config = Arc::new(config);
        let refresh_strategy = match session_source {
            SessionSource::SubAgent(_) => crate::models_manager::manager::RefreshStrategy::Offline,
            _ => crate::models_manager::manager::RefreshStrategy::OnlineIfUncached,
        };
        if config.model.is_none()
            || !matches!(
                refresh_strategy,
                crate::models_manager::manager::RefreshStrategy::Offline
            )
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
                    let state_db_ctx = state_db::get_state_db(&config).await;
                    state_db::get_dynamic_tools(state_db_ctx.as_deref(), process_id, "codex_spawn")
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
            windows_sandbox_level: WindowsSandboxLevel::Disabled,
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
        let codex = Chaos {
            tx_sub,
            rx_event,
            agent_status: agent_status_rx,
            session,
            session_loop_termination: session_loop_termination_from_handle(session_loop_handle),
        };

        Ok(ChaosSpawnOk {
            chaos: codex,
            process_id,
            conversation_id: process_id,
        })
    }

    /// Submit the `op` wrapped in a `Submission` with a unique ID.
    pub async fn submit(&self, op: Op) -> ChaosResult<String> {
        self.submit_with_trace(op, /*trace*/ None).await
    }

    pub async fn submit_with_trace(
        &self,
        op: Op,
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
        match self.submit(Op::Shutdown).await {
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

    pub(crate) fn state_db(&self) -> Option<state_db::StateDbHandle> {
        self.session.state_db()
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

/// Context for an initialized model agent
///
/// A session has at most 1 running task at a time, and can be interrupted by user input.
pub(crate) struct Session {
    pub(crate) conversation_id: ProcessId,
    pub(crate) tx_event: Sender<Event>,
    agent_status: watch::Sender<AgentStatus>,
    out_of_band_elicitation_paused: watch::Sender<bool>,
    state: Mutex<SessionState>,
    /// The set of enabled features should be invariant for the lifetime of the
    /// session.
    features: ManagedFeatures,
    pending_mcp_server_refresh_config: Mutex<Option<McpServerRefreshConfig>>,
    pub(crate) active_turn: Mutex<Option<ActiveTurn>>,

    pub(crate) services: SessionServices,
    next_internal_sub_id: AtomicU64,
}

#[derive(Clone, Debug)]
pub(crate) struct TurnSkillsContext {
    pub(crate) outcome: Arc<SkillLoadOutcome>,
}
impl TurnSkillsContext {
    pub(crate) fn new(outcome: Arc<SkillLoadOutcome>) -> Self {
        Self { outcome }
    }
}

/// The context needed for a single turn of the thread.
#[derive(Debug)]
pub(crate) struct TurnContext {
    pub(crate) sub_id: String,
    pub(crate) trace_id: Option<String>,
    pub(crate) config: Arc<Config>,
    pub(crate) auth_manager: Option<Arc<AuthManager>>,
    pub(crate) model_info: ModelInfo,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) provider: ModelProviderInfo,
    pub(crate) reasoning_effort: Option<ReasoningEffortConfig>,
    pub(crate) reasoning_summary: ReasoningSummaryConfig,
    pub(crate) session_source: SessionSource,
    /// The session's current working directory. All relative paths provided by
    /// the model as well as sandbox policies are resolved against this path
    /// instead of `std::env::current_dir()`.
    pub(crate) cwd: PathBuf,
    pub(crate) current_date: Option<String>,
    pub(crate) timezone: Option<String>,
    pub(crate) app_server_client_name: Option<String>,
    pub(crate) minion_instructions: Option<String>,
    pub(crate) compact_prompt: Option<String>,
    pub(crate) user_instructions: Option<String>,
    pub(crate) collaboration_mode: CollaborationMode,
    pub(crate) personality: Option<Personality>,
    pub(crate) approval_policy: Constrained<ApprovalPolicy>,
    pub(crate) sandbox_policy: Constrained<SandboxPolicy>,
    pub(crate) file_system_sandbox_policy: FileSystemSandboxPolicy,
    pub(crate) network_sandbox_policy: NetworkSandboxPolicy,
    pub(crate) network: Option<NetworkProxy>,
    pub(crate) windows_sandbox_level: WindowsSandboxLevel,
    pub(crate) shell_environment_policy: ShellEnvironmentPolicy,
    pub(crate) tools_config: ToolsConfig,
    pub(crate) features: ManagedFeatures,
    pub(crate) ghost_snapshot: GhostSnapshotConfig,
    pub(crate) final_output_json_schema: Option<Value>,
    pub(crate) alcatraz_macos_exe: Option<PathBuf>,
    pub(crate) alcatraz_linux_exe: Option<PathBuf>,
    pub(crate) alcatraz_freebsd_exe: Option<PathBuf>,
    pub(crate) tool_call_gate: Arc<ReadinessFlag>,
    pub(crate) truncation_policy: TruncationPolicy,
    pub(crate) dynamic_tools: Vec<DynamicToolSpec>,
    pub(crate) turn_metadata_state: Arc<TurnMetadataState>,
    pub(crate) turn_skills: TurnSkillsContext,
    pub(crate) turn_timing_state: Arc<TurnTimingState>,
}
impl TurnContext {
    pub(crate) fn model_context_window(&self) -> Option<i64> {
        let effective_context_window_percent = self.model_info.effective_context_window_percent;
        self.model_info.context_window.map(|context_window| {
            context_window.saturating_mul(effective_context_window_percent) / 100
        })
    }

    pub(crate) fn apps_enabled(&self) -> bool {
        false
    }

    pub(crate) async fn with_model(&self, model: String, models_manager: &ModelsManager) -> Self {
        let mut config = (*self.config).clone();
        config.model = Some(model.clone());
        let model_info = models_manager.get_model_info(model.as_str(), &config).await;
        let truncation_policy = model_info.truncation_policy.into();
        let supported_reasoning_levels = model_info
            .supported_reasoning_levels
            .iter()
            .map(|preset| preset.effort)
            .collect::<Vec<_>>();
        let reasoning_effort = if let Some(current_reasoning_effort) = self.reasoning_effort {
            if supported_reasoning_levels.contains(&current_reasoning_effort) {
                Some(current_reasoning_effort)
            } else {
                supported_reasoning_levels
                    .get(supported_reasoning_levels.len().saturating_sub(1) / 2)
                    .copied()
                    .or(model_info.default_reasoning_level)
            }
        } else {
            supported_reasoning_levels
                .get(supported_reasoning_levels.len().saturating_sub(1) / 2)
                .copied()
                .or(model_info.default_reasoning_level)
        };
        config.model_reasoning_effort = reasoning_effort;

        let collaboration_mode = self.collaboration_mode.with_updates(
            Some(model.clone()),
            Some(reasoning_effort),
            /*minion_instructions*/ None,
        );
        let features = self.features.clone();
        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            available_models: &models_manager
                .list_models(RefreshStrategy::OnlineIfUncached)
                .await,
            features: &features,
            web_search_mode: self.tools_config.web_search_mode,
            session_source: self.session_source.clone(),
            sandbox_policy: self.sandbox_policy.get(),
            windows_sandbox_level: self.windows_sandbox_level,
        })
        .with_unified_exec_shell_mode(self.tools_config.unified_exec_shell_mode.clone())
        .with_web_search_config(self.tools_config.web_search_config.clone())
        .with_allow_login_shell(self.tools_config.allow_login_shell)
        .with_agent_roles(config.agent_roles.clone());

        Self {
            sub_id: self.sub_id.clone(),
            trace_id: self.trace_id.clone(),
            config: Arc::new(config),
            auth_manager: self.auth_manager.clone(),
            model_info: model_info.clone(),
            session_telemetry: self
                .session_telemetry
                .clone()
                .with_model(model.as_str(), model_info.slug.as_str()),
            provider: self.provider.clone(),
            reasoning_effort,
            reasoning_summary: self.reasoning_summary,
            session_source: self.session_source.clone(),
            cwd: self.cwd.clone(),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
            app_server_client_name: self.app_server_client_name.clone(),
            minion_instructions: self.minion_instructions.clone(),
            compact_prompt: self.compact_prompt.clone(),
            user_instructions: self.user_instructions.clone(),
            collaboration_mode,
            personality: self.personality,
            approval_policy: self.approval_policy.clone(),
            sandbox_policy: self.sandbox_policy.clone(),
            file_system_sandbox_policy: self.file_system_sandbox_policy.clone(),
            network_sandbox_policy: self.network_sandbox_policy,
            network: self.network.clone(),
            windows_sandbox_level: self.windows_sandbox_level,
            shell_environment_policy: self.shell_environment_policy.clone(),
            tools_config,
            features,
            ghost_snapshot: self.ghost_snapshot.clone(),
            final_output_json_schema: self.final_output_json_schema.clone(),
            alcatraz_macos_exe: self.alcatraz_macos_exe.clone(),
            alcatraz_linux_exe: self.alcatraz_linux_exe.clone(),
            alcatraz_freebsd_exe: self.alcatraz_freebsd_exe.clone(),
            tool_call_gate: Arc::new(ReadinessFlag::new()),
            truncation_policy,
            dynamic_tools: self.dynamic_tools.clone(),
            turn_metadata_state: self.turn_metadata_state.clone(),
            turn_skills: self.turn_skills.clone(),
            turn_timing_state: Arc::clone(&self.turn_timing_state),
        }
    }

    pub(crate) fn resolve_path(&self, path: Option<String>) -> PathBuf {
        path.as_ref()
            .map(PathBuf::from)
            .map_or_else(|| self.cwd.clone(), |p| self.cwd.join(p))
    }

    pub(crate) fn compact_prompt(&self) -> &str {
        self.compact_prompt
            .as_deref()
            .unwrap_or(compact::SUMMARIZATION_PROMPT)
    }

    pub(crate) fn to_turn_context_item(&self) -> TurnContextItem {
        TurnContextItem {
            turn_id: Some(self.sub_id.clone()),
            trace_id: self.trace_id.clone(),
            cwd: self.cwd.clone(),
            current_date: self.current_date.clone(),
            timezone: self.timezone.clone(),
            approval_policy: self.approval_policy.value(),
            sandbox_policy: self.sandbox_policy.get().clone(),
            network: self.turn_context_network_item(),
            model: self.model_info.slug.clone(),
            personality: self.personality,
            collaboration_mode: Some(self.collaboration_mode.clone()),
            effort: self.reasoning_effort,
            summary: self.reasoning_summary,
            user_instructions: self.user_instructions.clone(),
            minion_instructions: self.minion_instructions.clone(),
            final_output_json_schema: self.final_output_json_schema.clone(),
            truncation_policy: Some(self.truncation_policy.into()),
        }
    }

    fn turn_context_network_item(&self) -> Option<TurnContextNetworkItem> {
        let network = self
            .config
            .config_layer_stack
            .requirements()
            .network
            .as_ref()?;
        Some(TurnContextNetworkItem {
            allowed_domains: network.allowed_domains.clone().unwrap_or_default(),
            denied_domains: network.denied_domains.clone().unwrap_or_default(),
        })
    }
}

fn local_time_context() -> (String, String) {
    match iana_time_zone::get_timezone() {
        Ok(timezone) => (Zoned::now().strftime("%Y-%m-%d").to_string(), timezone),
        Err(_) => (
            Timestamp::now()
                .to_zoned(jiff::tz::TimeZone::UTC)
                .strftime("%Y-%m-%d")
                .to_string(),
            "Etc/UTC".to_string(),
        ),
    }
}

#[derive(Clone)]
pub(crate) struct SessionConfiguration {
    /// Provider identifier ("openai", "openrouter", ...).
    provider: ModelProviderInfo,

    collaboration_mode: CollaborationMode,
    model_reasoning_summary: Option<ReasoningSummaryConfig>,
    service_tier: Option<ServiceTier>,

    /// Minion instructions that supplement the base instructions.
    minion_instructions: Option<String>,

    /// Model instructions that are appended to the base instructions.
    user_instructions: Option<String>,

    /// Personality preference for the model.
    personality: Option<Personality>,

    /// Base instructions for the session.
    base_instructions: String,

    /// Compact prompt override.
    compact_prompt: Option<String>,

    /// When to escalate for approval for execution
    approval_policy: Constrained<ApprovalPolicy>,
    approvals_reviewer: ApprovalsReviewer,
    /// How to sandbox commands executed in the system
    sandbox_policy: Constrained<SandboxPolicy>,
    file_system_sandbox_policy: FileSystemSandboxPolicy,
    network_sandbox_policy: NetworkSandboxPolicy,
    windows_sandbox_level: WindowsSandboxLevel,

    /// Working directory that should be treated as the *root* of the
    /// session. All relative paths supplied by the model as well as the
    /// execution sandbox are resolved against this directory **instead**
    /// of the process-wide current working directory. CLI front-ends are
    /// expected to expand this to an absolute path before sending the
    /// `ConfigureSession` operation so that the business-logic layer can
    /// operate deterministically.
    cwd: PathBuf,
    /// Directory containing all Chaos state for this session.
    chaos_home: PathBuf,
    /// Optional user-facing name for the thread, updated during the session.
    process_name: Option<String>,

    // TODO(pakrym): Remove config from here
    original_config_do_not_use: Arc<Config>,
    /// Optional service name tag for session metrics.
    metrics_service_name: Option<String>,
    app_server_client_name: Option<String>,
    /// Source of the session (cli, vscode, exec, mcp, ...)
    session_source: SessionSource,
    dynamic_tools: Vec<DynamicToolSpec>,
    persist_extended_history: bool,
    inherited_shell_snapshot: Option<Arc<ShellSnapshot>>,
}

impl SessionConfiguration {
    pub(crate) fn chaos_home(&self) -> &PathBuf {
        &self.chaos_home
    }

    fn process_config_snapshot(&self) -> ProcessConfigSnapshot {
        ProcessConfigSnapshot {
            model: self.collaboration_mode.model().to_string(),
            model_provider_id: self.original_config_do_not_use.model_provider_id.clone(),
            service_tier: self.service_tier,
            approval_policy: self.approval_policy.value(),
            approvals_reviewer: self.approvals_reviewer,
            sandbox_policy: self.sandbox_policy.get().clone(),
            cwd: self.cwd.clone(),
            ephemeral: self.original_config_do_not_use.ephemeral,
            reasoning_effort: self.collaboration_mode.reasoning_effort(),
            personality: self.personality,
            session_source: self.session_source.clone(),
        }
    }

    pub(crate) fn apply(&self, updates: &SessionSettingsUpdate) -> ConstraintResult<Self> {
        let mut next_configuration = self.clone();
        let file_system_policy_matches_legacy = self.file_system_sandbox_policy
            == FileSystemSandboxPolicy::from_legacy_sandbox_policy(
                self.sandbox_policy.get(),
                &self.cwd,
            );
        if let Some(collaboration_mode) = updates.collaboration_mode.clone() {
            next_configuration.collaboration_mode = collaboration_mode;
        }
        if let Some(summary) = updates.reasoning_summary {
            next_configuration.model_reasoning_summary = Some(summary);
        }
        if let Some(service_tier) = updates.service_tier {
            next_configuration.service_tier = service_tier;
        }
        if let Some(personality) = updates.personality {
            next_configuration.personality = Some(personality);
        }
        if let Some(approval_policy) = updates.approval_policy {
            next_configuration.approval_policy.set(approval_policy)?;
        }
        if let Some(approvals_reviewer) = updates.approvals_reviewer {
            next_configuration.approvals_reviewer = approvals_reviewer;
        }
        let mut sandbox_policy_changed = false;
        if let Some(sandbox_policy) = updates.sandbox_policy.clone() {
            next_configuration.sandbox_policy.set(sandbox_policy)?;
            next_configuration.network_sandbox_policy =
                NetworkSandboxPolicy::from(next_configuration.sandbox_policy.get());
            sandbox_policy_changed = true;
        }
        if let Some(windows_sandbox_level) = updates.windows_sandbox_level {
            next_configuration.windows_sandbox_level = windows_sandbox_level;
        }
        let mut cwd_changed = false;
        if let Some(cwd) = updates.cwd.clone() {
            next_configuration.cwd = cwd;
            cwd_changed = true;
        }
        if sandbox_policy_changed || (cwd_changed && file_system_policy_matches_legacy) {
            // Preserve richer split policies across cwd-only updates; only
            // rederive when the session is already using the legacy bridge.
            next_configuration.file_system_sandbox_policy =
                FileSystemSandboxPolicy::from_legacy_sandbox_policy(
                    next_configuration.sandbox_policy.get(),
                    &next_configuration.cwd,
                );
        }
        if let Some(app_server_client_name) = updates.app_server_client_name.clone() {
            next_configuration.app_server_client_name = Some(app_server_client_name);
        }
        Ok(next_configuration)
    }
}

#[derive(Default, Clone)]
pub(crate) struct SessionSettingsUpdate {
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) approval_policy: Option<ApprovalPolicy>,
    pub(crate) approvals_reviewer: Option<ApprovalsReviewer>,
    pub(crate) sandbox_policy: Option<SandboxPolicy>,
    pub(crate) windows_sandbox_level: Option<WindowsSandboxLevel>,
    pub(crate) collaboration_mode: Option<CollaborationMode>,
    pub(crate) reasoning_summary: Option<ReasoningSummaryConfig>,
    pub(crate) service_tier: Option<Option<ServiceTier>>,
    pub(crate) final_output_json_schema: Option<Option<Value>>,
    pub(crate) personality: Option<Personality>,
    pub(crate) app_server_client_name: Option<String>,
}

impl Session {
    /// Builds the `x-codex-beta-features` header value for this session.
    ///
    /// With stages removed, there are no experimental-menu features to advertise.
    /// Kept as a named function so callers do not need to change.
    fn build_model_client_beta_features_header(_config: &Config) -> Option<String> {
        None
    }

    async fn start_managed_network_proxy(
        spec: &crate::config::NetworkProxySpec,
        exec_policy: &chaos_selinux::Policy,
        sandbox_policy: &SandboxPolicy,
        network_policy_decider: Option<Arc<dyn chaos_pf::NetworkPolicyDecider>>,
        blocked_request_observer: Option<Arc<dyn chaos_pf::BlockedRequestObserver>>,
        managed_network_requirements_enabled: bool,
        audit_metadata: NetworkProxyAuditMetadata,
    ) -> anyhow::Result<(StartedNetworkProxy, SessionNetworkProxyRuntime)> {
        let spec = spec
            .with_exec_policy_network_rules(exec_policy)
            .map_err(|err| {
                tracing::warn!(
                    "failed to apply execpolicy network rules to managed proxy; continuing with configured network policy: {err}"
                );
                err
            })
            .unwrap_or_else(|_| spec.clone());
        let network_proxy = spec
            .start_proxy(
                sandbox_policy,
                network_policy_decider,
                blocked_request_observer,
                managed_network_requirements_enabled,
                audit_metadata,
            )
            .await
            .map_err(|err| anyhow::anyhow!("failed to start managed network proxy: {err}"))?;
        let session_network_proxy = {
            let proxy = network_proxy.proxy();
            SessionNetworkProxyRuntime {
                http_addr: proxy.http_addr().to_string(),
                socks_addr: proxy.socks_addr().to_string(),
            }
        };
        Ok((network_proxy, session_network_proxy))
    }

    /// Don't expand the number of mutated arguments on config. We are in the process of getting rid of it.
    pub(crate) fn build_per_turn_config(session_configuration: &SessionConfiguration) -> Config {
        // todo(aibrahim): store this state somewhere else so we don't need to mut config
        let config = session_configuration.original_config_do_not_use.clone();
        let mut per_turn_config = (*config).clone();
        per_turn_config.cwd = session_configuration.cwd.clone();
        per_turn_config.model_reasoning_effort =
            session_configuration.collaboration_mode.reasoning_effort();
        per_turn_config.model_reasoning_summary = session_configuration.model_reasoning_summary;
        per_turn_config.service_tier = session_configuration.service_tier;
        per_turn_config.personality = session_configuration.personality;
        per_turn_config.approvals_reviewer = session_configuration.approvals_reviewer;
        let resolved_web_search_mode = resolve_web_search_mode_for_turn(
            &per_turn_config.web_search_mode,
            session_configuration.sandbox_policy.get(),
        );
        if let Err(err) = per_turn_config
            .web_search_mode
            .set(resolved_web_search_mode)
        {
            let fallback_value = per_turn_config.web_search_mode.value();
            tracing::warn!(
                error = %err,
                ?resolved_web_search_mode,
                ?fallback_value,
                "resolved web_search_mode is disallowed by requirements; keeping constrained value"
            );
        }
        per_turn_config.features = config.features.clone();
        per_turn_config
    }

    pub(crate) async fn chaos_home(&self) -> PathBuf {
        let state = self.state.lock().await;
        state.session_configuration.chaos_home().clone()
    }

    pub(crate) fn subscribe_out_of_band_elicitation_pause_state(&self) -> watch::Receiver<bool> {
        self.out_of_band_elicitation_paused.subscribe()
    }

    pub(crate) fn set_out_of_band_elicitation_pause_state(&self, paused: bool) {
        self.out_of_band_elicitation_paused.send_replace(paused);
    }

    fn start_file_watcher_listener(self: &Arc<Self>) {
        let mut rx = self.services.file_watcher.subscribe();
        let weak_sess = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(FileWatcherEvent::SkillsChanged { .. }) => {
                        let Some(sess) = weak_sess.upgrade() else {
                            break;
                        };
                        let event = Event {
                            id: sess.next_internal_sub_id(),
                            msg: EventMsg::SkillsUpdateAvailable,
                        };
                        sess.send_event_raw(event).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn make_turn_context(
        auth_manager: Option<Arc<AuthManager>>,
        session_telemetry: &SessionTelemetry,
        provider: ModelProviderInfo,
        session_configuration: &SessionConfiguration,
        user_shell: &shell::Shell,
        shell_zsh_path: Option<&PathBuf>,
        main_execve_wrapper_exe: Option<&PathBuf>,
        per_turn_config: Config,
        model_info: ModelInfo,
        models_manager: &ModelsManager,
        network: Option<NetworkProxy>,
        sub_id: String,
        skills_outcome: Arc<SkillLoadOutcome>,
    ) -> TurnContext {
        let reasoning_effort = session_configuration.collaboration_mode.reasoning_effort();
        let reasoning_summary = session_configuration
            .model_reasoning_summary
            .unwrap_or(model_info.default_reasoning_summary);
        let session_telemetry = session_telemetry.clone().with_model(
            session_configuration.collaboration_mode.model(),
            model_info.slug.as_str(),
        );
        let session_source = session_configuration.session_source.clone();
        let auth_manager_for_context = auth_manager;
        let provider_for_context = provider;
        let session_telemetry_for_context = session_telemetry;
        let per_turn_config = Arc::new(per_turn_config);

        let tools_config = ToolsConfig::new(&ToolsConfigParams {
            model_info: &model_info,
            available_models: &models_manager.try_list_models().unwrap_or_default(),
            features: &per_turn_config.features,
            web_search_mode: Some(per_turn_config.web_search_mode.value()),
            session_source: session_source.clone(),
            sandbox_policy: session_configuration.sandbox_policy.get(),
            windows_sandbox_level: session_configuration.windows_sandbox_level,
        })
        .with_unified_exec_shell_mode_for_session(
            user_shell,
            shell_zsh_path,
            main_execve_wrapper_exe,
        )
        .with_web_search_config(per_turn_config.web_search_config.clone())
        .with_allow_login_shell(per_turn_config.permissions.allow_login_shell)
        .with_agent_roles(per_turn_config.agent_roles.clone());

        let cwd = session_configuration.cwd.clone();
        let turn_metadata_state = Arc::new(TurnMetadataState::new(
            sub_id.clone(),
            cwd.clone(),
            session_configuration.sandbox_policy.get(),
        ));
        let (current_date, timezone) = local_time_context();
        TurnContext {
            sub_id,
            trace_id: current_span_trace_id(),
            config: per_turn_config.clone(),
            auth_manager: auth_manager_for_context,
            model_info: model_info.clone(),
            session_telemetry: session_telemetry_for_context,
            provider: provider_for_context,
            reasoning_effort,
            reasoning_summary,
            session_source,
            cwd,
            current_date: Some(current_date),
            timezone: Some(timezone),
            app_server_client_name: session_configuration.app_server_client_name.clone(),
            minion_instructions: session_configuration.minion_instructions.clone(),
            compact_prompt: session_configuration.compact_prompt.clone(),
            user_instructions: session_configuration.user_instructions.clone(),
            collaboration_mode: session_configuration.collaboration_mode.clone(),
            personality: session_configuration.personality,
            approval_policy: session_configuration.approval_policy.clone(),
            sandbox_policy: session_configuration.sandbox_policy.clone(),
            file_system_sandbox_policy: session_configuration.file_system_sandbox_policy.clone(),
            network_sandbox_policy: session_configuration.network_sandbox_policy,
            network,
            windows_sandbox_level: session_configuration.windows_sandbox_level,
            shell_environment_policy: per_turn_config.permissions.shell_environment_policy.clone(),
            tools_config,
            features: per_turn_config.features.clone(),
            ghost_snapshot: per_turn_config.ghost_snapshot.clone(),
            final_output_json_schema: None,
            alcatraz_macos_exe: per_turn_config.alcatraz_macos_exe.clone(),
            alcatraz_linux_exe: per_turn_config.alcatraz_linux_exe.clone(),
            alcatraz_freebsd_exe: per_turn_config.alcatraz_freebsd_exe.clone(),
            tool_call_gate: Arc::new(ReadinessFlag::new()),
            truncation_policy: model_info.truncation_policy.into(),
            dynamic_tools: session_configuration.dynamic_tools.clone(),
            turn_metadata_state,
            turn_skills: TurnSkillsContext::new(skills_outcome),
            turn_timing_state: Arc::new(TurnTimingState::default()),
        }
    }

    #[instrument(name = "session_init", level = "info", skip_all)]
    #[allow(clippy::too_many_arguments)]
    async fn new(
        mut session_configuration: SessionConfiguration,
        config: Arc<Config>,
        auth_manager: Arc<AuthManager>,
        models_manager: Arc<ModelsManager>,
        exec_policy: ExecPolicyManager,
        tx_event: Sender<Event>,
        agent_status: watch::Sender<AgentStatus>,
        initial_history: InitialHistory,
        session_source: SessionSource,
        skills_manager: Arc<SkillsManager>,
        mcp_manager: Arc<McpManager>,
        file_watcher: Arc<FileWatcher>,
        agent_control: AgentControl,
    ) -> anyhow::Result<Arc<Self>> {
        debug!(
            "Configuring session: model={}; provider={:?}",
            session_configuration.collaboration_mode.model(),
            session_configuration.provider
        );
        if !session_configuration.cwd.is_absolute() {
            return Err(anyhow::anyhow!(
                "cwd is not absolute: {:?}",
                session_configuration.cwd
            ));
        }

        let forked_from_id = initial_history.forked_from_id();

        let (conversation_id, rollout_params) = match &initial_history {
            InitialHistory::New | InitialHistory::Forked(_) => {
                let conversation_id = ProcessId::default();
                (
                    conversation_id,
                    RolloutRecorderParams::new(
                        conversation_id,
                        forked_from_id,
                        session_source,
                        BaseInstructions {
                            text: session_configuration.base_instructions.clone(),
                        },
                        session_configuration.dynamic_tools.clone(),
                        if session_configuration.persist_extended_history {
                            EventPersistenceMode::Extended
                        } else {
                            EventPersistenceMode::Limited
                        },
                    ),
                )
            }
            InitialHistory::Resumed(resumed_history) => (
                resumed_history.conversation_id,
                RolloutRecorderParams::resume(
                    resumed_history.conversation_id,
                    session_source,
                    if session_configuration.persist_extended_history {
                        EventPersistenceMode::Extended
                    } else {
                        EventPersistenceMode::Limited
                    },
                ),
            ),
        };
        let state_builder = match &initial_history {
            InitialHistory::Resumed(_) | InitialHistory::New | InitialHistory::Forked(_) => None,
        };

        // Kick off independent async setup tasks in parallel to reduce startup latency.
        //
        // - initialize RolloutRecorder with new or resumed session info
        // - perform default shell discovery
        // - load history metadata (skipped for subagents)
        let rollout_fut = async {
            if config.ephemeral {
                Ok::<_, anyhow::Error>((None, None))
            } else {
                let state_db_ctx = state_db::init(&config).await;
                let rollout_recorder = RolloutRecorder::new(
                    &config,
                    rollout_params,
                    state_db_ctx.clone(),
                    state_builder.clone(),
                )
                .await?;
                Ok((Some(rollout_recorder), state_db_ctx))
            }
        }
        .instrument(info_span!(
            "session_init.rollout",
            otel.name = "session_init.rollout",
            session_init.ephemeral = config.ephemeral,
        ));

        let is_subagent = matches!(
            session_configuration.session_source,
            SessionSource::SubAgent(_)
        );
        let auth_manager_clone = Arc::clone(&auth_manager);
        let config_for_mcp = Arc::clone(&config);
        let mcp_manager_for_mcp = Arc::clone(&mcp_manager);
        let auth_and_mcp_fut = async move {
            let auth = auth_manager_clone.auth().await;
            let mcp_servers = mcp_manager_for_mcp.effective_servers(&config_for_mcp);
            let auth_statuses = compute_auth_statuses(
                mcp_servers.iter(),
                config_for_mcp.mcp_oauth_credentials_store_mode,
            )
            .await;
            (auth, mcp_servers, auth_statuses)
        }
        .instrument(info_span!(
            "session_init.auth_mcp",
            otel.name = "session_init.auth_mcp",
        ));

        // Join all independent futures.
        let (rollout_recorder_and_state_db, (auth, mcp_servers, auth_statuses)) =
            tokio::join!(rollout_fut, auth_and_mcp_fut);

        let (rollout_recorder, state_db_ctx) = rollout_recorder_and_state_db.map_err(|e| {
            error!("failed to initialize rollout recorder: {e:#}");
            e
        })?;
        let (history_log_id, history_entry_count) = async {
            if is_subagent {
                (0, 0)
            } else {
                crate::message_history::history_metadata(state_db_ctx.as_deref()).await
            }
        }
        .instrument(info_span!(
            "session_init.history_metadata",
            otel.name = "session_init.history_metadata",
            session_init.is_subagent = is_subagent,
        ))
        .await;
        let mut post_session_configured_events = Vec::<Event>::new();

        for usage in config.features.legacy_feature_usages() {
            post_session_configured_events.push(Event {
                id: INITIAL_SUBMIT_ID.to_owned(),
                msg: EventMsg::DeprecationNotice(DeprecationNoticeEvent {
                    summary: usage.summary.clone(),
                    details: usage.details.clone(),
                }),
            });
        }
        if crate::config::uses_deprecated_instructions_file(&config.config_layer_stack) {
            post_session_configured_events.push(Event {
                id: INITIAL_SUBMIT_ID.to_owned(),
                msg: EventMsg::DeprecationNotice(DeprecationNoticeEvent {
                    summary: "`experimental_instructions_file` is deprecated and ignored. Use `model_instructions_file` instead."
                        .to_string(),
                    details: Some(
                        "Move the setting to `model_instructions_file` in config.toml (or under a profile) to load instructions from a file."
                            .to_string(),
                    ),
                }),
            });
        }
        for message in &config.startup_warnings {
            post_session_configured_events.push(Event {
                id: "".to_owned(),
                msg: EventMsg::Warning(WarningEvent {
                    message: message.clone(),
                }),
            });
        }

        let auth = auth.as_ref();
        let auth_mode = auth.map(ChaosAuth::auth_mode).map(TelemetryAuthMode::from);
        let account_id = auth.and_then(ChaosAuth::get_account_id);
        let account_email = auth.and_then(ChaosAuth::get_account_email);
        let originator = crate::default_client::originator().value;
        let terminal_type = terminal::user_agent();
        let session_model = session_configuration.collaboration_mode.model().to_string();
        let mut session_telemetry = SessionTelemetry::new(
            conversation_id,
            session_model.as_str(),
            session_model.as_str(),
            account_id.clone(),
            account_email.clone(),
            auth_mode,
            originator.clone(),
            config.otel.log_user_prompt,
            terminal_type.clone(),
            session_configuration.session_source.clone(),
        );
        if let Some(service_name) = session_configuration.metrics_service_name.as_deref() {
            session_telemetry = session_telemetry.with_metrics_service_name(service_name);
        }
        let network_proxy_audit_metadata = NetworkProxyAuditMetadata {
            conversation_id: Some(conversation_id.to_string()),
            app_version: Some(CHAOS_VERSION.to_string()),
            user_account_id: account_id,
            auth_mode: auth_mode.map(|mode| mode.to_string()),
            originator: Some(originator),
            user_email: account_email,
            terminal_type: Some(terminal_type),
            model: Some(session_model.clone()),
            slug: Some(session_model),
        };
        crate::features::emit_feature_metrics(&config.features, &session_telemetry);
        session_telemetry.counter(
            THREAD_STARTED_METRIC,
            /*inc*/ 1,
            &[(
                "is_git",
                if get_git_repo_root(&session_configuration.cwd).is_some() {
                    "true"
                } else {
                    "false"
                },
            )],
        );

        session_telemetry.conversation_starts(
            config.model_provider.name.as_str(),
            session_configuration.collaboration_mode.reasoning_effort(),
            config
                .model_reasoning_summary
                .unwrap_or(ReasoningSummaryConfig::Auto),
            config.model_context_window,
            config.model_auto_compact_token_limit,
            config.permissions.approval_policy.value(),
            config.permissions.sandbox_policy.get().clone(),
            mcp_servers.keys().map(String::as_str).collect(),
            config.active_profile.clone(),
        );

        let mut default_shell = shell::default_user_shell();
        // Create the mutable state for the Session.
        let shell_snapshot_tx = if config.features.enabled(Feature::ShellSnapshot) {
            if let Some(snapshot) = session_configuration.inherited_shell_snapshot.clone() {
                let (tx, rx) = watch::channel(Some(snapshot));
                default_shell.shell_snapshot = rx;
                tx
            } else {
                ShellSnapshot::start_snapshotting(
                    config.chaos_home.clone(),
                    conversation_id,
                    session_configuration.cwd.clone(),
                    &mut default_shell,
                    session_telemetry.clone(),
                )
            }
        } else {
            let (tx, rx) = watch::channel(None);
            default_shell.shell_snapshot = rx;
            tx
        };
        let process_name =
            match process_names::find_process_name_by_id(&config.chaos_home, &conversation_id)
                .instrument(info_span!(
                    "session_init.process_name_lookup",
                    otel.name = "session_init.process_name_lookup",
                ))
                .await
            {
                Ok(name) => name,
                Err(err) => {
                    warn!("Failed to read session index for process name: {err}");
                    None
                }
            };
        session_configuration.process_name = process_name.clone();
        let state = SessionState::new(session_configuration.clone());
        let managed_network_requirements_enabled = config.managed_network_requirements_enabled();
        let network_approval = Arc::new(NetworkApprovalService::default());
        // The managed proxy can call back into core for allowlist-miss decisions.
        let network_policy_decider_session = if managed_network_requirements_enabled {
            config
                .permissions
                .network
                .as_ref()
                .map(|_| Arc::new(RwLock::new(std::sync::Weak::<Session>::new())))
        } else {
            None
        };
        let blocked_request_observer = if managed_network_requirements_enabled {
            config
                .permissions
                .network
                .as_ref()
                .map(|_| build_blocked_request_observer(Arc::clone(&network_approval)))
        } else {
            None
        };
        let network_policy_decider =
            network_policy_decider_session
                .as_ref()
                .map(|network_policy_decider_session| {
                    build_network_policy_decider(
                        Arc::clone(&network_approval),
                        Arc::clone(network_policy_decider_session),
                    )
                });
        let (network_proxy, session_network_proxy) =
            if let Some(spec) = config.permissions.network.as_ref() {
                let current_exec_policy = exec_policy.current();
                let (network_proxy, session_network_proxy) = Self::start_managed_network_proxy(
                    spec,
                    current_exec_policy.as_ref(),
                    config.permissions.sandbox_policy.get(),
                    network_policy_decider.as_ref().map(Arc::clone),
                    blocked_request_observer.as_ref().map(Arc::clone),
                    managed_network_requirements_enabled,
                    network_proxy_audit_metadata,
                )
                .instrument(info_span!(
                    "session_init.network_proxy",
                    otel.name = "session_init.network_proxy",
                    session_init.managed_network_requirements_enabled =
                        managed_network_requirements_enabled,
                ))
                .await?;
                (Some(network_proxy), Some(session_network_proxy))
            } else {
                (None, None)
            };

        let mut hook_shell_argv =
            default_shell.derive_exec_args("", /*use_login_shell*/ false);
        let hook_shell_program = hook_shell_argv.remove(0);
        let _ = hook_shell_argv.pop();
        let hooks = Hooks::new(HooksConfig {
            legacy_notify_argv: config.notify.clone(),
            feature_enabled: config.features.enabled(Feature::CodexHooks),
            config_layer_stack: Some(config.config_layer_stack.clone()),
            shell_program: Some(hook_shell_program),
            shell_args: hook_shell_argv,
        });
        for warning in hooks.startup_warnings() {
            post_session_configured_events.push(Event {
                id: INITIAL_SUBMIT_ID.to_owned(),
                msg: EventMsg::Warning(WarningEvent {
                    message: warning.clone(),
                }),
            });
        }

        // Spawn the hallucinate scripting engine (Lua/WASM user scripts).
        // When `disable_user_scripts` is set (test environments), pass a
        // non-existent path as the user-layer override so real user scripts
        // never load into the session.
        let user_scripts_dir = session_configuration
            .original_config_do_not_use
            .disable_user_scripts
            .then(|| std::path::PathBuf::from("/dev/null/no_user_scripts"));
        let hallucinate = match chaos_hallucinate::spawn(chaos_hallucinate::SessionInfo {
            session_id: conversation_id.to_string(),
            cwd: session_configuration.cwd.to_string_lossy().to_string(),
            provider: session_configuration.provider.name.clone(),
            user_scripts_dir,
        }) {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::warn!("hallucinate engine failed to start: {e}");
                None
            }
        };

        let services = SessionServices {
            catalog: Arc::new(crate::catalog::CatalogSink::new(
                crate::catalog::Catalog::from_inventory(),
            )),
            // Initialize the MCP connection manager with an uninitialized
            // instance. It will be replaced with one created via
            // McpConnectionManager::new() once all its constructor args are
            // available. This also ensures `SessionConfigured` is emitted
            // before any MCP-related events. It is reasonable to consider
            // changing this to use Option or OnceCell, though the current
            // setup is straightforward enough and performs well.
            mcp_connection_manager: Arc::new(RwLock::new(McpConnectionManager::new_uninitialized(
                &config.permissions.approval_policy,
            ))),
            mcp_startup_cancellation_token: Mutex::new(CancellationToken::new()),
            unified_exec_manager: UnifiedExecProcessManager::new(
                config.background_terminal_max_timeout,
            ),
            shell_zsh_path: config.zsh_path.clone(),
            main_execve_wrapper_exe: config.main_execve_wrapper_exe.clone(),
            analytics_events_client: AnalyticsEventsClient::new(
                Arc::clone(&config),
                Arc::clone(&auth_manager),
            ),
            hooks,
            rollout: Mutex::new(rollout_recorder),
            user_shell: Arc::new(default_shell),
            shell_snapshot_tx,

            exec_policy,
            auth_manager: Arc::clone(&auth_manager),
            session_telemetry,
            models_manager: Arc::clone(&models_manager),
            tool_approvals: Mutex::new(ApprovalStore::default()),
            execve_session_approvals: RwLock::new(HashMap::new()),
            skills_manager,
            mcp_manager: Arc::clone(&mcp_manager),
            file_watcher,
            agent_control,
            network_proxy,
            network_approval: Arc::clone(&network_approval),
            state_db: state_db_ctx.clone(),
            hallucinate,
            model_client: ModelClient::new(
                Some(Arc::clone(&auth_manager)),
                conversation_id,
                session_configuration.provider.clone(),
                session_configuration.session_source.clone(),
                session_configuration.approval_policy.value(),
                config.model_verbosity,
                ws_version_from_features(config.as_ref()),
                config.features.enabled(Feature::EnableRequestCompression),
                true,
                Self::build_model_client_beta_features_header(config.as_ref()),
            ),
        };
        let (out_of_band_elicitation_paused, _out_of_band_elicitation_paused_rx) =
            watch::channel(false);

        let sess = Arc::new(Session {
            conversation_id,
            tx_event: tx_event.clone(),
            agent_status,
            out_of_band_elicitation_paused,
            state: Mutex::new(state),
            features: config.features.clone(),
            pending_mcp_server_refresh_config: Mutex::new(None),
            active_turn: Mutex::new(None),
            services,
            next_internal_sub_id: AtomicU64::new(0),
        });
        sess.services.model_client.bind_session(&sess);
        if let Some(network_policy_decider_session) = network_policy_decider_session {
            let mut guard = network_policy_decider_session.write().await;
            *guard = Arc::downgrade(&sess);
        }
        // Log a debug summary of the session configuration for debug.log.
        tracing::debug!(
            provider = %session_configuration.provider.name,
            model = %session_configuration.collaboration_mode.model(),
            sandbox_policy = ?session_configuration.sandbox_policy.get(),
            approval_policy = ?session_configuration.approval_policy.value(),
            reasoning_effort = ?session_configuration.collaboration_mode.reasoning_effort(),
            cwd = %session_configuration.cwd.display(),
            "session configured",
        );

        // Dispatch the SessionConfiguredEvent first and then report any errors.
        // If resuming, include converted initial messages in the payload so UIs can render them immediately.
        let initial_messages = initial_history.get_event_msgs();
        let events = std::iter::once(Event {
            id: INITIAL_SUBMIT_ID.to_owned(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: conversation_id,
                forked_from_id,
                process_name: session_configuration.process_name.clone(),
                model: session_configuration.collaboration_mode.model().to_string(),
                model_provider_id: config.model_provider_id.clone(),
                service_tier: session_configuration.service_tier,
                approval_policy: session_configuration.approval_policy.value(),
                approvals_reviewer: session_configuration.approvals_reviewer,
                sandbox_policy: session_configuration.sandbox_policy.get().clone(),
                cwd: session_configuration.cwd.clone(),
                reasoning_effort: session_configuration.collaboration_mode.reasoning_effort(),
                history_log_id,
                history_entry_count,
                initial_messages,
                network_proxy: session_network_proxy,
            }),
        })
        .chain(post_session_configured_events.into_iter());
        for event in events {
            sess.send_event_raw(event).await;
        }

        // Start the watcher after SessionConfigured so it cannot emit earlier events.
        sess.start_file_watcher_listener();
        // Construct sandbox_state before MCP startup so it can be sent to each
        // MCP server immediately after it becomes ready (avoiding blocking).
        let sandbox_state = SandboxState {
            sandbox_policy: session_configuration.sandbox_policy.get().clone(),
            alcatraz_macos_exe: config.alcatraz_macos_exe.clone(),
            alcatraz_linux_exe: config.alcatraz_linux_exe.clone(),
            alcatraz_freebsd_exe: config.alcatraz_freebsd_exe.clone(),
            sandbox_cwd: session_configuration.cwd.clone(),
        };
        let mut required_mcp_servers: Vec<String> = mcp_servers
            .iter()
            .filter(|(_, server)| server.enabled && server.required)
            .map(|(name, _)| name.clone())
            .collect();
        required_mcp_servers.sort();
        let enabled_mcp_server_count = mcp_servers.values().filter(|server| server.enabled).count();
        let required_mcp_server_count = required_mcp_servers.len();
        {
            let mut cancel_guard = sess.services.mcp_startup_cancellation_token.lock().await;
            cancel_guard.cancel();
            *cancel_guard = CancellationToken::new();
        }
        let (mcp_connection_manager, cancel_token) = McpConnectionManager::new(
            &mcp_servers,
            config.mcp_oauth_credentials_store_mode,
            auth_statuses.clone(),
            &session_configuration.approval_policy,
            tx_event.clone(),
            sandbox_state,
            config.chaos_home.clone(),
            Arc::clone(&sess.services.catalog) as Arc<dyn chaos_traits::McpCatalogSink>,
        )
        .instrument(info_span!(
            "session_init.mcp_manager_init",
            otel.name = "session_init.mcp_manager_init",
            session_init.enabled_mcp_server_count = enabled_mcp_server_count,
            session_init.required_mcp_server_count = required_mcp_server_count,
        ))
        .await;
        {
            let mut manager_guard = sess.services.mcp_connection_manager.write().await;
            *manager_guard = mcp_connection_manager;
        }
        // Sync MCP tools into the catalog.
        {
            let mcp_mgr = sess.services.mcp_connection_manager.read().await;
            let mcp_tools = mcp_mgr.list_all_tools().await;
            let mut catalog = sess
                .services
                .catalog
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for tool_info in mcp_tools.values() {
                catalog.register_mcp_tools(
                    &tool_info.server_name,
                    vec![chaos_mcp_runtime::catalog_conv::mcp_tool_info_to_catalog_tool(tool_info)],
                );
            }
        }
        {
            let mut cancel_guard = sess.services.mcp_startup_cancellation_token.lock().await;
            if cancel_guard.is_cancelled() {
                cancel_token.cancel();
            }
            *cancel_guard = cancel_token;
        }
        if !required_mcp_servers.is_empty() {
            let failures = sess
                .services
                .mcp_connection_manager
                .read()
                .await
                .required_startup_failures(&required_mcp_servers)
                .instrument(info_span!(
                    "session_init.required_mcp_wait",
                    otel.name = "session_init.required_mcp_wait",
                    session_init.required_mcp_server_count = required_mcp_server_count,
                ))
                .await;
            if !failures.is_empty() {
                let details = failures
                    .iter()
                    .map(|failure| format!("{}: {}", failure.server, failure.error))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(anyhow::anyhow!(
                    "required MCP servers failed to initialize: {details}"
                ));
            }
        }
        sess.schedule_startup_prewarm(session_configuration.base_instructions.clone())
            .await;
        let session_start_source = match &initial_history {
            InitialHistory::Resumed(_) => chaos_dtrace::SessionStartSource::Resume,
            InitialHistory::New | InitialHistory::Forked(_) => {
                chaos_dtrace::SessionStartSource::Startup
            }
        };

        // record_initial_history can emit events. We record only after the SessionConfiguredEvent is emitted.
        sess.record_initial_history(initial_history).await;
        {
            let mut state = sess.state.lock().await;
            state.set_pending_session_start_source(Some(session_start_source));
        }

        Ok(sess)
    }

    pub(crate) fn get_tx_event(&self) -> Sender<Event> {
        self.tx_event.clone()
    }

    pub(crate) fn state_db(&self) -> Option<state_db::StateDbHandle> {
        self.services.state_db.clone()
    }

    /// Ensure persisted session history writes are durably flushed.
    pub(crate) async fn flush_rollout(&self) {
        let recorder = {
            let guard = self.services.rollout.lock().await;
            guard.clone()
        };
        if let Some(rec) = recorder
            && let Err(e) = rec.flush().await
        {
            warn!("failed to flush rollout recorder: {e}");
        }
    }

    pub(crate) async fn ensure_rollout_materialized(&self) {
        let recorder = {
            let guard = self.services.rollout.lock().await;
            guard.clone()
        };
        if let Some(rec) = recorder
            && let Err(e) = rec.persist().await
        {
            warn!("failed to materialize rollout recorder: {e}");
        }
    }

    fn next_internal_sub_id(&self) -> String {
        let id = self
            .next_internal_sub_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("auto-compact-{id}")
    }

    pub(crate) async fn get_total_token_usage(&self) -> i64 {
        let state = self.state.lock().await;
        state.get_total_token_usage(state.server_reasoning_included())
    }

    pub(crate) async fn get_total_token_usage_breakdown(&self) -> TotalTokenUsageBreakdown {
        let state = self.state.lock().await;
        state.history.get_total_token_usage_breakdown()
    }

    pub(crate) async fn total_token_usage(&self) -> Option<TokenUsage> {
        let state = self.state.lock().await;
        state.token_info().map(|info| info.total_token_usage)
    }

    pub(crate) async fn get_estimated_token_count(
        &self,
        turn_context: &TurnContext,
    ) -> Option<i64> {
        let state = self.state.lock().await;
        state.history.estimate_token_count(turn_context)
    }

    pub(crate) async fn get_base_instructions(&self) -> BaseInstructions {
        let state = self.state.lock().await;
        BaseInstructions {
            text: state.session_configuration.base_instructions.clone(),
        }
    }

    async fn record_initial_history(&self, conversation_history: InitialHistory) {
        let turn_context = self.new_default_turn().await;
        let is_subagent = {
            let state = self.state.lock().await;
            matches!(
                state.session_configuration.session_source,
                SessionSource::SubAgent(_)
            )
        };
        match conversation_history {
            InitialHistory::New => {
                // Defer initial context insertion until the first real turn starts so
                // turn/start overrides can be merged before we write model-visible context.
                self.set_previous_turn_settings(/*previous_turn_settings*/ None)
                    .await;
            }
            InitialHistory::Resumed(resumed_history) => {
                let rollout_items = resumed_history.history;
                let previous_turn_settings = self
                    .apply_rollout_reconstruction(&turn_context, &rollout_items)
                    .await;

                // If resuming, warn when the last recorded model differs from the current one.
                let curr: &str = turn_context.model_info.slug.as_str();
                if let Some(prev) = previous_turn_settings
                    .as_ref()
                    .map(|settings| settings.model.as_str())
                    .filter(|model| *model != curr)
                {
                    warn!("resuming session with different model: previous={prev}, current={curr}");
                    self.send_event(
                        &turn_context,
                        EventMsg::Warning(WarningEvent {
                            message: format!(
                                "This session was recorded with model `{prev}` but is resuming with `{curr}`. \
                         Consider switching back to `{prev}` as it may affect Chaos performance."
                            ),
                        }),
                    )
                    .await;
                }

                // Seed usage info from the persisted session history so UIs can show token counts
                // immediately on resume/fork.
                if let Some(info) = Self::last_token_info_from_rollout(&rollout_items) {
                    let mut state = self.state.lock().await;
                    state.set_token_info(Some(info));
                }

                // Defer seeding the session's initial context until the first turn starts so
                // turn/start overrides can be merged before we write to the rollout.
                if !is_subagent {
                    self.flush_rollout().await;
                }
            }
            InitialHistory::Forked(rollout_items) => {
                self.apply_rollout_reconstruction(&turn_context, &rollout_items)
                    .await;

                // Seed usage info from the persisted session history so UIs can show token counts
                // immediately on resume/fork.
                if let Some(info) = Self::last_token_info_from_rollout(&rollout_items) {
                    let mut state = self.state.lock().await;
                    state.set_token_info(Some(info));
                }

                // If persisting, persist all rollout items as-is (recorder filters)
                if !rollout_items.is_empty() {
                    self.persist_rollout_items(&rollout_items).await;
                }

                // Append the current session's initial context after the reconstructed history.
                let initial_context = self.build_initial_context(&turn_context).await;
                self.record_conversation_items(&turn_context, &initial_context)
                    .await;
                {
                    let mut state = self.state.lock().await;
                    state.set_reference_context_item(Some(turn_context.to_turn_context_item()));
                }

                // Forked threads should remain file-backed immediately after startup.
                self.ensure_rollout_materialized().await;

                // Flush after seeding history and any persisted rollout copy.
                if !is_subagent {
                    self.flush_rollout().await;
                }
            }
        }
    }

    async fn apply_rollout_reconstruction(
        &self,
        turn_context: &TurnContext,
        rollout_items: &[RolloutItem],
    ) -> Option<PreviousTurnSettings> {
        let reconstructed_rollout = self
            .reconstruct_history_from_rollout(turn_context, rollout_items)
            .await;
        let previous_turn_settings = reconstructed_rollout.previous_turn_settings.clone();
        self.replace_history(
            reconstructed_rollout.history,
            reconstructed_rollout.reference_context_item,
        )
        .await;
        self.set_previous_turn_settings(previous_turn_settings.clone())
            .await;
        previous_turn_settings
    }

    fn last_token_info_from_rollout(rollout_items: &[RolloutItem]) -> Option<TokenUsageInfo> {
        rollout_items.iter().rev().find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::TokenCount(ev)) => ev.info.clone(),
            _ => None,
        })
    }

    async fn previous_turn_settings(&self) -> Option<PreviousTurnSettings> {
        let state = self.state.lock().await;
        state.previous_turn_settings()
    }

    pub(crate) async fn set_previous_turn_settings(
        &self,
        previous_turn_settings: Option<PreviousTurnSettings>,
    ) {
        let mut state = self.state.lock().await;
        state.set_previous_turn_settings(previous_turn_settings);
    }

    fn maybe_refresh_shell_snapshot_for_cwd(
        &self,
        previous_cwd: &Path,
        next_cwd: &Path,
        chaos_home: &Path,
        session_source: &SessionSource,
    ) {
        if previous_cwd == next_cwd {
            return;
        }

        if !self.features.enabled(Feature::ShellSnapshot) {
            return;
        }

        if matches!(
            session_source,
            SessionSource::SubAgent(SubAgentSource::ProcessSpawn { .. })
        ) {
            return;
        }

        ShellSnapshot::refresh_snapshot(
            chaos_home.to_path_buf(),
            self.conversation_id,
            next_cwd.to_path_buf(),
            self.services.user_shell.as_ref().clone(),
            self.services.shell_snapshot_tx.clone(),
            self.services.session_telemetry.clone(),
        );
    }

    pub(crate) async fn update_settings(
        &self,
        updates: SessionSettingsUpdate,
    ) -> ConstraintResult<()> {
        let mut state = self.state.lock().await;

        match state.session_configuration.apply(&updates) {
            Ok(updated) => {
                let previous_cwd = state.session_configuration.cwd.clone();
                let next_cwd = updated.cwd.clone();
                let chaos_home = updated.chaos_home.clone();
                let session_source = updated.session_source.clone();
                state.session_configuration = updated;
                drop(state);

                self.maybe_refresh_shell_snapshot_for_cwd(
                    &previous_cwd,
                    &next_cwd,
                    &chaos_home,
                    &session_source,
                );

                if previous_cwd != next_cwd
                    && let Err(e) = self
                        .services
                        .mcp_connection_manager
                        .read()
                        .await
                        .notify_roots_changed(&next_cwd)
                        .await
                {
                    warn!("Failed to notify MCP servers of roots change: {e:#}");
                }

                Ok(())
            }
            Err(err) => {
                warn!("rejected session settings update: {err}");
                Err(err)
            }
        }
    }

    pub(crate) async fn new_turn_with_sub_id(
        &self,
        sub_id: String,
        updates: SessionSettingsUpdate,
    ) -> ConstraintResult<Arc<TurnContext>> {
        let (
            session_configuration,
            sandbox_policy_changed,
            previous_cwd,
            chaos_home,
            session_source,
        ) = {
            let mut state = self.state.lock().await;
            match state.session_configuration.clone().apply(&updates) {
                Ok(next) => {
                    let previous_cwd = state.session_configuration.cwd.clone();
                    let sandbox_policy_changed =
                        state.session_configuration.sandbox_policy != next.sandbox_policy;
                    let chaos_home = next.chaos_home.clone();
                    let session_source = next.session_source.clone();
                    state.session_configuration = next.clone();
                    (
                        next,
                        sandbox_policy_changed,
                        previous_cwd,
                        chaos_home,
                        session_source,
                    )
                }
                Err(err) => {
                    drop(state);
                    self.send_event_raw(Event {
                        id: sub_id.clone(),
                        msg: EventMsg::Error(ErrorEvent {
                            message: err.to_string(),
                            codex_error_info: Some(CodexErrorInfo::BadRequest),
                        }),
                    })
                    .await;
                    return Err(err);
                }
            }
        };

        self.maybe_refresh_shell_snapshot_for_cwd(
            &previous_cwd,
            &session_configuration.cwd,
            &chaos_home,
            &session_source,
        );

        if previous_cwd != session_configuration.cwd
            && let Err(e) = self
                .services
                .mcp_connection_manager
                .read()
                .await
                .notify_roots_changed(&session_configuration.cwd)
                .await
        {
            warn!("Failed to notify MCP servers of roots change: {e:#}");
        }

        Ok(self
            .new_turn_from_configuration(
                sub_id,
                session_configuration,
                updates.final_output_json_schema,
                sandbox_policy_changed,
            )
            .await)
    }

    async fn new_turn_from_configuration(
        &self,
        sub_id: String,
        session_configuration: SessionConfiguration,
        final_output_json_schema: Option<Option<Value>>,
        sandbox_policy_changed: bool,
    ) -> Arc<TurnContext> {
        let per_turn_config = Self::build_per_turn_config(&session_configuration);
        self.services
            .mcp_connection_manager
            .read()
            .await
            .set_approval_policy(&session_configuration.approval_policy);

        if sandbox_policy_changed {
            let sandbox_state = SandboxState {
                sandbox_policy: per_turn_config.permissions.sandbox_policy.get().clone(),
                alcatraz_macos_exe: per_turn_config.alcatraz_macos_exe.clone(),
                alcatraz_linux_exe: per_turn_config.alcatraz_linux_exe.clone(),
                alcatraz_freebsd_exe: per_turn_config.alcatraz_freebsd_exe.clone(),
                sandbox_cwd: per_turn_config.cwd.clone(),
            };
            if let Err(e) = self
                .services
                .mcp_connection_manager
                .read()
                .await
                .notify_sandbox_state_change(&sandbox_state)
                .await
            {
                warn!("Failed to notify sandbox state change to MCP servers: {e:#}");
            }
        }

        let model_info = self
            .services
            .models_manager
            .get_model_info(
                session_configuration.collaboration_mode.model(),
                &per_turn_config,
            )
            .await;
        let skills_outcome = Arc::new(
            self.services
                .skills_manager
                .skills_for_config(&per_turn_config),
        );
        let mut turn_context: TurnContext = Self::make_turn_context(
            Some(Arc::clone(&self.services.auth_manager)),
            &self.services.session_telemetry,
            session_configuration.provider.clone(),
            &session_configuration,
            self.services.user_shell.as_ref(),
            self.services.shell_zsh_path.as_ref(),
            self.services.main_execve_wrapper_exe.as_ref(),
            per_turn_config,
            model_info,
            &self.services.models_manager,
            self.services
                .network_proxy
                .as_ref()
                .map(StartedNetworkProxy::proxy),
            sub_id,
            skills_outcome,
        );

        if let Some(final_schema) = final_output_json_schema {
            turn_context.final_output_json_schema = final_schema;
        }
        let turn_context = Arc::new(turn_context);
        turn_context.turn_metadata_state.spawn_git_enrichment_task();
        turn_context
    }

    pub(crate) async fn maybe_emit_unknown_model_warning_for_turn(&self, tc: &TurnContext) {
        if tc.model_info.used_fallback_model_metadata && !self.services.model_client.is_clamped() {
            self.send_event(
                tc,
                EventMsg::Warning(WarningEvent {
                    message: format!(
                        "Model metadata for `{}` not found. Defaulting to fallback metadata; this can degrade performance and cause issues.",
                        tc.model_info.slug
                    ),
                }),
            )
            .await;
        }
    }

    pub(crate) async fn new_default_turn(&self) -> Arc<TurnContext> {
        self.new_default_turn_with_sub_id(self.next_internal_sub_id())
            .await
    }

    pub(crate) async fn take_startup_regular_task(&self) -> Option<RegularTask> {
        let startup_regular_task = {
            let mut state = self.state.lock().await;
            state.take_startup_regular_task()
        };
        let startup_regular_task = startup_regular_task?;
        match startup_regular_task.await {
            Ok(Ok(regular_task)) => Some(regular_task),
            Ok(Err(err)) => {
                warn!("startup websocket prewarm setup failed: {err:#}");
                None
            }
            Err(err) => {
                warn!("startup websocket prewarm setup join failed: {err}");
                None
            }
        }
    }

    async fn schedule_startup_prewarm(self: &Arc<Self>, base_instructions: String) {
        let sess = Arc::clone(self);
        let startup_regular_task: JoinHandle<ChaosResult<RegularTask>> =
            tokio::spawn(
                async move { sess.schedule_startup_prewarm_inner(base_instructions).await },
            );
        let mut state = self.state.lock().await;
        state.set_startup_regular_task(startup_regular_task);
    }

    async fn schedule_startup_prewarm_inner(
        self: &Arc<Self>,
        base_instructions: String,
    ) -> ChaosResult<RegularTask> {
        let startup_turn_context = self
            .new_default_turn_with_sub_id(INITIAL_SUBMIT_ID.to_owned())
            .await;
        let startup_cancellation_token = CancellationToken::new();
        let startup_router = built_tools(
            self,
            startup_turn_context.as_ref(),
            &[],
            /*skills_outcome*/ None,
            &startup_cancellation_token,
        )
        .await?;
        let startup_prompt = build_prompt(
            Vec::new(),
            startup_router.as_ref(),
            startup_turn_context.as_ref(),
            BaseInstructions {
                text: base_instructions,
            },
        );
        let startup_turn_metadata_header = startup_turn_context
            .turn_metadata_state
            .current_header_value();
        RegularTask::with_startup_prewarm(
            self.services.model_client.clone(),
            startup_prompt,
            startup_turn_context,
            startup_turn_metadata_header,
        )
        .await
    }

    pub(crate) async fn get_config(&self) -> std::sync::Arc<Config> {
        let state = self.state.lock().await;
        state
            .session_configuration
            .original_config_do_not_use
            .clone()
    }

    pub(crate) async fn reload_user_config_layer(&self) {
        let config_toml_path = {
            let state = self.state.lock().await;
            state
                .session_configuration
                .chaos_home
                .join(CONFIG_TOML_FILE)
        };

        let user_config = match std::fs::read_to_string(&config_toml_path) {
            Ok(contents) => match toml::from_str::<toml::Value>(&contents) {
                Ok(config) => config,
                Err(err) => {
                    warn!("failed to parse user config while reloading layer: {err}");
                    return;
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                toml::Value::Table(Default::default())
            }
            Err(err) => {
                warn!("failed to read user config while reloading layer: {err}");
                return;
            }
        };

        let config_toml_path = match AbsolutePathBuf::try_from(config_toml_path) {
            Ok(path) => path,
            Err(err) => {
                warn!("failed to resolve user config path while reloading layer: {err}");
                return;
            }
        };

        let mut state = self.state.lock().await;
        let mut config = (*state.session_configuration.original_config_do_not_use).clone();
        config.config_layer_stack = config
            .config_layer_stack
            .with_user_config(&config_toml_path, user_config);
        state.session_configuration.original_config_do_not_use = Arc::new(config);
        self.services.skills_manager.clear_cache();
    }

    pub(crate) async fn reload_project_mcp_layer_and_refresh(&self, turn_context: &TurnContext) {
        let project_mcp_json_path = {
            let state = self.state.lock().await;
            let session_config = &state.session_configuration;
            crate::config_loader::project_mcp_json_path_for_stack(
                &session_config.original_config_do_not_use.config_layer_stack,
                &session_config.cwd,
            )
        };

        let layer = match std::fs::read_to_string(&project_mcp_json_path) {
            Ok(contents) => match crate::config_loader::parse_project_mcp_json(&contents) {
                Ok(config) => {
                    let Ok(file) = AbsolutePathBuf::try_from(project_mcp_json_path.clone()) else {
                        warn!(
                            "failed to resolve project MCP path while reloading layer: {}",
                            project_mcp_json_path.display()
                        );
                        return;
                    };
                    Some(crate::config_loader::ConfigLayerEntry::new(
                        chaos_ipc::api::ConfigLayerSource::ProjectMcp { file },
                        config,
                    ))
                }
                Err(err) => {
                    warn!("failed to parse project MCP config while reloading layer: {err}");
                    return;
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                warn!("failed to read project MCP config while reloading layer: {err}");
                return;
            }
        };

        let (mcp_servers, store_mode) = {
            let mut state = self.state.lock().await;
            let mut config = (*state.session_configuration.original_config_do_not_use).clone();
            config.config_layer_stack = config.config_layer_stack.with_project_mcp_layer(layer);
            if let Err(err) = config.reload_mcp_servers_from_layer_stack() {
                warn!("failed to reload MCP servers from updated config stack: {err}");
                return;
            }
            let mcp_servers = config.mcp_servers.get().clone();
            let store_mode = config.mcp_oauth_credentials_store_mode;
            state.session_configuration.original_config_do_not_use = Arc::new(config);
            (mcp_servers, store_mode)
        };

        self.refresh_mcp_servers_inner(turn_context, mcp_servers, store_mode)
            .await;
    }

    pub(crate) async fn new_default_turn_with_sub_id(&self, sub_id: String) -> Arc<TurnContext> {
        let session_configuration = {
            let state = self.state.lock().await;
            state.session_configuration.clone()
        };
        self.new_turn_from_configuration(
            sub_id,
            session_configuration,
            /*final_output_json_schema*/ None,
            /*sandbox_policy_changed*/ false,
        )
        .await
    }

    async fn build_settings_update_items(
        &self,
        reference_context_item: Option<&TurnContextItem>,
        current_context: &TurnContext,
    ) -> Vec<ResponseItem> {
        // TODO: Make context updates a pure diff of persisted previous/current TurnContextItem
        // state so replay/backtracking is deterministic. Runtime inputs that affect model-visible
        // context (shell, exec policy, feature gates, previous-turn bridge) should be persisted
        // state or explicit non-state replay events.
        let previous_turn_settings = {
            let state = self.state.lock().await;
            state.previous_turn_settings()
        };
        let shell = self.user_shell();
        let exec_policy = self.services.exec_policy.current();
        crate::context_manager::updates::build_settings_update_items(
            reference_context_item,
            previous_turn_settings.as_ref(),
            current_context,
            shell.as_ref(),
            exec_policy.as_ref(),
            self.features.enabled(Feature::Personality),
        )
    }

    /// Persist the event to rollout and send it to clients.
    pub(crate) async fn send_event(&self, turn_context: &TurnContext, msg: EventMsg) {
        let event = Event {
            id: turn_context.sub_id.clone(),
            msg,
        };
        self.send_event_raw(event).await;
    }

    pub(crate) async fn send_event_raw(&self, event: Event) {
        // Persist the event into rollout (recorder filters as needed)
        let rollout_items = vec![RolloutItem::EventMsg(event.msg.clone())];
        self.persist_rollout_items(&rollout_items).await;
        self.deliver_event_raw(event).await;
    }

    async fn deliver_event_raw(&self, event: Event) {
        // Record the last known agent status.
        if let Some(status) = agent_status_from_event(&event.msg) {
            self.agent_status.send_replace(status);
        }
        if let Err(e) = self.tx_event.send(event).await {
            debug!("dropping event because channel is closed: {e}");
        }
    }

    pub(crate) async fn emit_turn_item_started(&self, turn_context: &TurnContext, item: &TurnItem) {
        self.send_event(
            turn_context,
            EventMsg::ItemStarted(ItemStartedEvent {
                process_id: self.conversation_id,
                turn_id: turn_context.sub_id.clone(),
                item: item.clone(),
            }),
        )
        .await;
    }

    pub(crate) async fn emit_turn_item_completed(
        &self,
        turn_context: &TurnContext,
        item: TurnItem,
    ) {
        record_turn_ttfm_metric(turn_context, &item).await;
        self.send_event(
            turn_context,
            EventMsg::ItemCompleted(ItemCompletedEvent {
                process_id: self.conversation_id,
                turn_id: turn_context.sub_id.clone(),
                item,
            }),
        )
        .await;
    }

    /// Adds an execpolicy amendment to both the in-memory and on-disk policies so future
    /// commands can use the newly approved prefix.
    pub(crate) async fn persist_execpolicy_amendment(
        &self,
        amendment: &ExecPolicyAmendment,
    ) -> Result<(), ExecPolicyUpdateError> {
        let chaos_home = self
            .state
            .lock()
            .await
            .session_configuration
            .chaos_home()
            .clone();

        self.services
            .exec_policy
            .append_amendment_and_update(&chaos_home, amendment)
            .await?;

        Ok(())
    }

    pub(crate) async fn turn_context_for_sub_id(&self, sub_id: &str) -> Option<Arc<TurnContext>> {
        let active = self.active_turn.lock().await;
        active
            .as_ref()
            .and_then(|turn| turn.tasks.get(sub_id))
            .map(|task| Arc::clone(&task.turn_context))
    }

    async fn active_turn_context_and_cancellation_token(
        &self,
    ) -> Option<(Arc<TurnContext>, CancellationToken)> {
        let active = self.active_turn.lock().await;
        let (_, task) = active.as_ref()?.tasks.first()?;
        Some((
            Arc::clone(&task.turn_context),
            task.cancellation_token.child_token(),
        ))
    }

    pub(crate) async fn record_execpolicy_amendment_message(
        &self,
        sub_id: &str,
        amendment: &ExecPolicyAmendment,
    ) {
        let Some(prefixes) = format_allow_prefixes(vec![amendment.command.clone()]) else {
            warn!("execpolicy amendment for {sub_id} had no command prefix");
            return;
        };
        let text = format!("Approved command prefix saved:\n{prefixes}");
        let message: ResponseItem = DeveloperInstructions::new(text.clone()).into();

        if let Some(turn_context) = self.turn_context_for_sub_id(sub_id).await {
            self.record_conversation_items(&turn_context, std::slice::from_ref(&message))
                .await;
            return;
        }

        if self
            .inject_response_items(vec![ResponseInputItem::Message {
                role: "developer".to_string(),
                content: vec![ContentItem::InputText { text }],
            }])
            .await
            .is_err()
        {
            warn!("no active turn found to record execpolicy amendment message for {sub_id}");
        }
    }

    pub(crate) async fn persist_network_policy_amendment(
        &self,
        amendment: &NetworkPolicyAmendment,
        network_approval_context: &NetworkApprovalContext,
    ) -> anyhow::Result<()> {
        let host =
            Self::validated_network_policy_amendment_host(amendment, network_approval_context)?;
        let chaos_home = self
            .state
            .lock()
            .await
            .session_configuration
            .chaos_home()
            .clone();
        let execpolicy_amendment =
            execpolicy_network_rule_amendment(amendment, network_approval_context, &host);

        if let Some(started_network_proxy) = self.services.network_proxy.as_ref() {
            let proxy = started_network_proxy.proxy();
            match amendment.action {
                NetworkPolicyRuleAction::Allow => proxy
                    .add_allowed_domain(&host)
                    .await
                    .map_err(|err| anyhow::anyhow!("failed to update runtime allowlist: {err}"))?,
                NetworkPolicyRuleAction::Deny => proxy
                    .add_denied_domain(&host)
                    .await
                    .map_err(|err| anyhow::anyhow!("failed to update runtime denylist: {err}"))?,
            }
        }

        self.services
            .exec_policy
            .append_network_rule_and_update(
                &chaos_home,
                &host,
                execpolicy_amendment.protocol,
                execpolicy_amendment.decision,
                Some(execpolicy_amendment.justification),
            )
            .await
            .map_err(|err| {
                anyhow::anyhow!("failed to persist network policy amendment to execpolicy: {err}")
            })?;

        Ok(())
    }

    fn validated_network_policy_amendment_host(
        amendment: &NetworkPolicyAmendment,
        network_approval_context: &NetworkApprovalContext,
    ) -> anyhow::Result<String> {
        let approved_host = normalize_host(&network_approval_context.host);
        let amendment_host = normalize_host(&amendment.host);
        if amendment_host != approved_host {
            return Err(anyhow::anyhow!(
                "network policy amendment host '{}' does not match approved host '{}'",
                amendment.host,
                network_approval_context.host
            ));
        }
        Ok(approved_host)
    }

    pub(crate) async fn record_network_policy_amendment_message(
        &self,
        sub_id: &str,
        amendment: &NetworkPolicyAmendment,
    ) {
        let (action, list_name) = match amendment.action {
            NetworkPolicyRuleAction::Allow => ("Allowed", "allowlist"),
            NetworkPolicyRuleAction::Deny => ("Denied", "denylist"),
        };
        let text = format!(
            "{action} network rule saved in execpolicy ({list_name}): {}",
            amendment.host
        );
        let message: ResponseItem = DeveloperInstructions::new(text.clone()).into();

        if let Some(turn_context) = self.turn_context_for_sub_id(sub_id).await {
            self.record_conversation_items(&turn_context, std::slice::from_ref(&message))
                .await;
            return;
        }

        if self
            .inject_response_items(vec![ResponseInputItem::Message {
                role: "developer".to_string(),
                content: vec![ContentItem::InputText { text }],
            }])
            .await
            .is_err()
        {
            warn!("no active turn found to record network policy amendment message for {sub_id}");
        }
    }

    /// Emit an exec approval request event and await the user's decision.
    ///
    /// The request is keyed by `call_id` + `approval_id` so matching responses
    /// are delivered to the correct in-flight turn. If the pending approval is
    /// cleared before a response arrives, treat it as an abort so interrupted
    /// turns do not continue on a synthetic denial.
    ///
    /// Note that if `available_decisions` is `None`, then the other fields will
    /// be used to derive the available decisions via
    /// [ExecApprovalRequestEvent::default_available_decisions].
    #[allow(clippy::too_many_arguments)]
    pub async fn request_command_approval(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        approval_id: Option<String>,
        command: Vec<String>,
        cwd: PathBuf,
        reason: Option<String>,
        network_approval_context: Option<NetworkApprovalContext>,
        proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
        additional_permissions: Option<PermissionProfile>,
        skill_metadata: Option<ExecApprovalRequestSkillMetadata>,
        available_decisions: Option<Vec<ReviewDecision>>,
    ) -> ReviewDecision {
        //  command-level approvals use `call_id`.
        // `approval_id` is only present for subcommand callbacks (execve intercept)
        let effective_approval_id = approval_id.clone().unwrap_or_else(|| call_id.clone());
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_approval(effective_approval_id.clone(), tx_approve)
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending approval for call_id: {effective_approval_id}");
        }

        let parsed_cmd = parse_command(&command);
        let proposed_network_policy_amendments = network_approval_context.as_ref().map(|context| {
            vec![
                NetworkPolicyAmendment {
                    host: context.host.clone(),
                    action: NetworkPolicyRuleAction::Allow,
                },
                NetworkPolicyAmendment {
                    host: context.host.clone(),
                    action: NetworkPolicyRuleAction::Deny,
                },
            ]
        });
        let available_decisions = available_decisions.unwrap_or_else(|| {
            ExecApprovalRequestEvent::default_available_decisions(
                network_approval_context.as_ref(),
                proposed_execpolicy_amendment.as_ref(),
                proposed_network_policy_amendments.as_deref(),
                additional_permissions.as_ref(),
            )
        });
        let event = EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
            call_id,
            approval_id,
            turn_id: turn_context.sub_id.clone(),
            command,
            cwd,
            reason,
            network_approval_context,
            proposed_execpolicy_amendment,
            proposed_network_policy_amendments,
            additional_permissions,
            skill_metadata,
            available_decisions: Some(available_decisions),
            parsed_cmd,
        });
        self.send_event(turn_context, event).await;
        rx_approve.await.unwrap_or(ReviewDecision::Abort)
    }

    pub async fn request_patch_approval(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        changes: HashMap<PathBuf, FileChange>,
        reason: Option<String>,
        grant_root: Option<PathBuf>,
    ) -> oneshot::Receiver<ReviewDecision> {
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
        let approval_id = call_id.clone();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_approval(approval_id.clone(), tx_approve)
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending approval for call_id: {approval_id}");
        }

        let event = EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            changes,
            reason,
            grant_root,
        });
        self.send_event(turn_context, event).await;
        rx_approve
    }

    pub async fn request_permissions(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        args: RequestPermissionsArgs,
    ) -> Option<RequestPermissionsResponse> {
        match turn_context.approval_policy.value() {
            ApprovalPolicy::Headless => {
                return Some(RequestPermissionsResponse {
                    permissions: RequestPermissionProfile::default(),
                    scope: PermissionGrantScope::Turn,
                });
            }
            ApprovalPolicy::Granular(granular_config)
                if !granular_config.allows_request_permissions() =>
            {
                return Some(RequestPermissionsResponse {
                    permissions: RequestPermissionProfile::default(),
                    scope: PermissionGrantScope::Turn,
                });
            }
            ApprovalPolicy::Interactive
            | ApprovalPolicy::Supervised
            | ApprovalPolicy::Granular(_) => {}
        }

        let (tx_response, rx_response) = oneshot::channel();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_request_permissions(call_id.clone(), tx_response)
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending request_permissions for call_id: {call_id}");
        }

        // TODO(ccunningham): Support auto-review for request_permissions /
        // with_additional_permissions. V0 still routes this surface through
        // the existing manual RequestPermissions event flow.
        let event = EventMsg::RequestPermissions(RequestPermissionsEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            reason: args.reason,
            permissions: args.permissions,
        });
        self.send_event(turn_context, event).await;
        rx_response.await.ok()
    }

    pub async fn request_user_input(
        &self,
        turn_context: &TurnContext,
        call_id: String,
        args: RequestUserInputArgs,
    ) -> Option<RequestUserInputResponse> {
        let sub_id = turn_context.sub_id.clone();
        let (tx_response, rx_response) = oneshot::channel();
        let event_id = sub_id.clone();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_user_input(sub_id, tx_response)
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending user input for sub_id: {event_id}");
        }

        let event = EventMsg::RequestUserInput(RequestUserInputEvent {
            call_id,
            turn_id: turn_context.sub_id.clone(),
            questions: args.questions,
        });
        self.send_event(turn_context, event).await;
        rx_response.await.ok()
    }

    pub async fn request_mcp_server_elicitation(
        &self,
        turn_context: &TurnContext,
        request_id: RequestId,
        params: McpServerElicitationRequestParams,
    ) -> Option<ElicitationResponse> {
        let server_name = params.server_name.clone();
        let request = match params.request {
            McpServerElicitationRequest::Form {
                meta,
                message,
                requested_schema,
            } => {
                let requested_schema = match serde_json::to_value(requested_schema) {
                    Ok(requested_schema) => requested_schema,
                    Err(err) => {
                        warn!(
                            "failed to serialize MCP elicitation schema for server_name: {server_name}, request_id: {request_id}: {err:#}"
                        );
                        return None;
                    }
                };
                chaos_ipc::approvals::ElicitationRequest::Form {
                    meta,
                    message,
                    requested_schema,
                }
            }
            McpServerElicitationRequest::Url {
                meta,
                message,
                url,
                elicitation_id,
            } => chaos_ipc::approvals::ElicitationRequest::Url {
                meta,
                message,
                url,
                elicitation_id,
            },
        };

        let (tx_response, rx_response) = oneshot::channel();
        let prev_entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.insert_pending_elicitation(
                        server_name.clone(),
                        request_id.clone(),
                        tx_response,
                    )
                }
                None => None,
            }
        };
        if prev_entry.is_some() {
            warn!(
                "Overwriting existing pending elicitation for server_name: {server_name}, request_id: {request_id}"
            );
        }
        let id = match &request_id {
            RequestId::String(value) => chaos_ipc::mcp::RequestId::String(value.clone()),
            RequestId::Number(value) => chaos_ipc::mcp::RequestId::Integer(*value),
        };
        let event = EventMsg::ElicitationRequest(ElicitationRequestEvent {
            turn_id: params.turn_id,
            server_name,
            id,
            request,
        });
        self.send_event(turn_context, event).await;
        rx_response.await.ok()
    }

    pub async fn notify_user_input_response(
        &self,
        sub_id: &str,
        response: RequestUserInputResponse,
    ) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_user_input(sub_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_response) => {
                tx_response.send(response).ok();
            }
            None => {
                warn!("No pending user input found for sub_id: {sub_id}");
            }
        }
    }

    pub async fn notify_request_permissions_response(
        &self,
        call_id: &str,
        response: RequestPermissionsResponse,
    ) {
        let mut granted_for_session = None;
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    let entry = ts.remove_pending_request_permissions(call_id);
                    if entry.is_some() && !response.permissions.is_empty() {
                        match response.scope {
                            PermissionGrantScope::Turn => {
                                ts.record_granted_permissions(response.permissions.clone().into());
                            }
                            PermissionGrantScope::Session => {
                                granted_for_session = Some(response.permissions.clone());
                            }
                        }
                    }
                    entry
                }
                None => None,
            }
        };
        if let Some(permissions) = granted_for_session {
            let mut state = self.state.lock().await;
            state.record_granted_permissions(permissions.into());
        }
        match entry {
            Some(tx_response) => {
                tx_response.send(response).ok();
            }
            None => {
                warn!("No pending request_permissions found for call_id: {call_id}");
            }
        }
    }

    pub(crate) async fn granted_turn_permissions(&self) -> Option<PermissionProfile> {
        let active = self.active_turn.lock().await;
        let active = active.as_ref()?;
        let ts = active.turn_state.lock().await;
        ts.granted_permissions()
    }

    pub(crate) async fn granted_session_permissions(&self) -> Option<PermissionProfile> {
        let state = self.state.lock().await;
        state.granted_permissions()
    }

    pub async fn notify_dynamic_tool_response(&self, call_id: &str, response: DynamicToolResponse) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_dynamic_tool(call_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_response) => {
                tx_response.send(response).ok();
            }
            None => {
                warn!("No pending dynamic tool call found for call_id: {call_id}");
            }
        }
    }

    pub async fn notify_approval(&self, approval_id: &str, decision: ReviewDecision) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_approval(approval_id)
                }
                None => None,
            }
        };
        match entry {
            Some(tx_approve) => {
                tx_approve.send(decision).ok();
            }
            None => {
                warn!("No pending approval found for call_id: {approval_id}");
            }
        }
    }

    pub async fn resolve_elicitation(
        &self,
        server_name: String,
        id: RequestId,
        response: ElicitationResponse,
    ) -> anyhow::Result<()> {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(at) => {
                    let mut ts = at.turn_state.lock().await;
                    ts.remove_pending_elicitation(&server_name, &id)
                }
                None => None,
            }
        };
        if let Some(tx_response) = entry {
            tx_response
                .send(response)
                .map_err(|e| anyhow::anyhow!("failed to send elicitation response: {e:?}"))?;
            return Ok(());
        }

        self.services
            .mcp_connection_manager
            .read()
            .await
            .resolve_elicitation(server_name, id, response)
            .await
    }

    /// Records input items: always append to conversation history and
    /// persist these response items to rollout.
    pub(crate) async fn record_conversation_items(
        &self,
        turn_context: &TurnContext,
        items: &[ResponseItem],
    ) {
        self.record_into_history(items, turn_context).await;
        self.persist_rollout_response_items(items).await;
        self.send_raw_response_items(turn_context, items).await;
    }

    /// Append ResponseItems to the in-memory conversation history only.
    pub(crate) async fn record_into_history(
        &self,
        items: &[ResponseItem],
        turn_context: &TurnContext,
    ) {
        let mut state = self.state.lock().await;
        state.record_items(items.iter(), turn_context.truncation_policy);
    }

    pub(crate) async fn record_model_warning(&self, message: impl Into<String>, ctx: &TurnContext) {
        self.services
            .session_telemetry
            .counter("codex.model_warning", /*inc*/ 1, &[]);
        let item = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!("Warning: {}", message.into()),
            }],
            end_turn: None,
            phase: None,
        };

        self.record_conversation_items(ctx, &[item]).await;
    }

    async fn maybe_warn_on_server_model_mismatch(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        server_model: String,
    ) -> bool {
        let requested_model = turn_context.model_info.slug.clone();
        let server_model_normalized = server_model.to_ascii_lowercase();
        let requested_model_normalized = requested_model.to_ascii_lowercase();
        if server_model_normalized == requested_model_normalized {
            info!("server reported model {server_model} (matches requested model)");
            return false;
        }

        warn!("server reported model {server_model} while requested model was {requested_model}");

        let warning_message = format!(
            "Your account was flagged for potentially high-risk cyber activity and this request was routed to gpt-5.2 as a fallback. To regain access to gpt-5.3-codex, apply for trusted access: {CYBER_VERIFY_URL} or learn more: {CYBER_SAFETY_URL}"
        );

        self.send_event(
            turn_context,
            EventMsg::ModelReroute(ModelRerouteEvent {
                from_model: requested_model.clone(),
                to_model: server_model.clone(),
                reason: ModelRerouteReason::HighRiskCyberActivity,
            }),
        )
        .await;

        self.send_event(
            turn_context,
            EventMsg::Warning(WarningEvent {
                message: warning_message.clone(),
            }),
        )
        .await;
        self.record_model_warning(warning_message, turn_context)
            .await;
        true
    }

    pub(crate) async fn replace_history(
        &self,
        items: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
    ) {
        let mut state = self.state.lock().await;
        state.replace_history(items, reference_context_item);
    }

    pub(crate) async fn replace_compacted_history(
        &self,
        items: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
        compacted_item: CompactedItem,
    ) {
        self.replace_history(items, reference_context_item.clone())
            .await;

        self.persist_rollout_items(&[RolloutItem::Compacted(compacted_item)])
            .await;
        if let Some(turn_context_item) = reference_context_item {
            self.persist_rollout_items(&[RolloutItem::TurnContext(turn_context_item)])
                .await;
        }
    }

    async fn persist_rollout_response_items(&self, items: &[ResponseItem]) {
        let rollout_items: Vec<RolloutItem> = items
            .iter()
            .cloned()
            .map(RolloutItem::ResponseItem)
            .collect();
        self.persist_rollout_items(&rollout_items).await;
    }

    pub fn enabled(&self, feature: Feature) -> bool {
        self.features.enabled(feature)
    }

    pub(crate) fn features(&self) -> ManagedFeatures {
        self.features.clone()
    }

    pub(crate) async fn collaboration_mode(&self) -> CollaborationMode {
        let state = self.state.lock().await;
        state.session_configuration.collaboration_mode.clone()
    }

    async fn send_raw_response_items(&self, turn_context: &TurnContext, items: &[ResponseItem]) {
        for item in items {
            self.send_event(
                turn_context,
                EventMsg::RawResponseItem(RawResponseItemEvent { item: item.clone() }),
            )
            .await;
        }
    }

    pub(crate) async fn build_initial_context(
        &self,
        turn_context: &TurnContext,
    ) -> Vec<ResponseItem> {
        let mut developer_sections = Vec::<String>::with_capacity(8);
        let mut contextual_user_sections = Vec::<String>::with_capacity(2);
        let shell = self.user_shell();
        let (
            _reference_context_item,
            previous_turn_settings,
            collaboration_mode,
            base_instructions,
            _session_source,
        ) = {
            let state = self.state.lock().await;
            (
                state.reference_context_item(),
                state.previous_turn_settings(),
                state.session_configuration.collaboration_mode.clone(),
                state.session_configuration.base_instructions.clone(),
                state.session_configuration.session_source.clone(),
            )
        };
        if let Some(model_switch_message) =
            crate::context_manager::updates::build_model_instructions_update_item(
                previous_turn_settings.as_ref(),
                turn_context,
            )
        {
            developer_sections.push(model_switch_message.into_text());
        }
        developer_sections.push(
            DeveloperInstructions::from_policy(
                turn_context.sandbox_policy.get(),
                turn_context.approval_policy.value(),
                self.services.exec_policy.current().as_ref(),
                &turn_context.cwd,
                turn_context
                    .features
                    .enabled(Feature::ExecPermissionApprovals),
                turn_context
                    .features
                    .enabled(Feature::RequestPermissionsTool),
            )
            .into_text(),
        );
        if let Some(minion_instructions) = turn_context.minion_instructions.as_deref() {
            developer_sections.push(minion_instructions.to_string());
        }
        // Add developer instructions from collaboration_mode if they exist and are non-empty
        if let Some(collab_instructions) =
            DeveloperInstructions::from_collaboration_mode(&collaboration_mode)
        {
            developer_sections.push(collab_instructions.into_text());
        }
        if self.features.enabled(Feature::Personality)
            && let Some(personality) = turn_context.personality
        {
            let model_info = turn_context.model_info.clone();
            let has_baked_personality = model_info.supports_personality()
                && base_instructions == model_info.get_model_instructions(Some(personality));
            if !has_baked_personality
                && let Some(personality_message) =
                    crate::context_manager::updates::personality_message_for(
                        &model_info,
                        personality,
                    )
            {
                developer_sections.push(
                    DeveloperInstructions::personality_spec_message(personality_message)
                        .into_text(),
                );
            }
        }
        let implicit_skills = turn_context
            .turn_skills
            .outcome
            .allowed_skills_for_implicit_invocation();
        if let Some(skills_section) = render_skills_section(&implicit_skills) {
            developer_sections.push(skills_section);
        }
        if let Some(user_instructions) = turn_context.user_instructions.as_deref() {
            contextual_user_sections.push(
                UserInstructions {
                    text: user_instructions.to_string(),
                    directory: turn_context.cwd.to_string_lossy().into_owned(),
                }
                .serialize_to_text(),
            );
        }
        let subagents = self
            .services
            .agent_control
            .format_environment_context_subagents(self.conversation_id)
            .await;
        contextual_user_sections.push(
            EnvironmentContext::from_turn_context(turn_context, shell.as_ref())
                .with_subagents(subagents)
                .serialize_to_xml(),
        );

        let mut items = Vec::with_capacity(3);
        if let Some(developer_message) =
            crate::context_manager::updates::build_developer_update_item(developer_sections)
        {
            items.push(developer_message);
        }
        if let Some(contextual_user_message) =
            crate::context_manager::updates::build_contextual_user_message(contextual_user_sections)
        {
            items.push(contextual_user_message);
        }
        items
    }

    pub(crate) async fn persist_rollout_items(&self, items: &[RolloutItem]) {
        let recorder = {
            let guard = self.services.rollout.lock().await;
            guard.clone()
        };
        if let Some(rec) = recorder
            && let Err(e) = rec.record_items(items).await
        {
            error!("failed to record rollout items: {e:#}");
        }
    }

    pub(crate) async fn clone_history(&self) -> ContextManager {
        let state = self.state.lock().await;
        state.clone_history()
    }

    pub(crate) async fn reference_context_item(&self) -> Option<TurnContextItem> {
        let state = self.state.lock().await;
        state.reference_context_item()
    }

    /// Persist the latest turn context snapshot for the first real user turn and for
    /// steady-state turns that emit model-visible context updates.
    ///
    /// When the reference snapshot is missing, this injects full initial context. Otherwise, it
    /// emits only settings diff items.
    ///
    /// If full context is injected and a model switch occurred, this prepends the
    /// `<model_switch>` developer message so model-specific instructions are not lost.
    ///
    /// This is the normal runtime path that establishes a new `reference_context_item`.
    /// Mid-turn compaction is the other path that can re-establish that baseline when it
    /// reinjects full initial context into replacement history. Other non-regular tasks
    /// intentionally do not update the baseline.
    pub(crate) async fn record_context_updates_and_set_reference_context_item(
        &self,
        turn_context: &TurnContext,
    ) {
        let reference_context_item = {
            let state = self.state.lock().await;
            state.reference_context_item()
        };
        let should_inject_full_context = reference_context_item.is_none();
        let context_items = if should_inject_full_context {
            self.build_initial_context(turn_context).await
        } else {
            // Steady-state path: append only context diffs to minimize token overhead.
            self.build_settings_update_items(reference_context_item.as_ref(), turn_context)
                .await
        };
        let turn_context_item = turn_context.to_turn_context_item();
        if !context_items.is_empty() {
            self.record_conversation_items(turn_context, &context_items)
                .await;
        }
        // Persist one `TurnContextItem` per real user turn so resume/lazy replay can recover the
        // latest durable baseline even when this turn emitted no model-visible context diffs.
        self.persist_rollout_items(&[RolloutItem::TurnContext(turn_context_item.clone())])
            .await;

        // Advance the in-memory diff baseline even when this turn emitted no model-visible
        // context items. This keeps later runtime diffing aligned with the current turn state.
        let mut state = self.state.lock().await;
        state.set_reference_context_item(Some(turn_context_item));
    }

    pub(crate) async fn update_token_usage_info(
        &self,
        turn_context: &TurnContext,
        token_usage: Option<&TokenUsage>,
    ) {
        if let Some(token_usage) = token_usage {
            let mut state = self.state.lock().await;
            state.update_token_info_from_usage(token_usage, turn_context.model_context_window());
        }
        self.send_token_count_event(turn_context).await;
    }

    pub(crate) async fn recompute_token_usage(&self, turn_context: &TurnContext) {
        let history = self.clone_history().await;
        let base_instructions = self.get_base_instructions().await;
        let Some(estimated_total_tokens) =
            history.estimate_token_count_with_base_instructions(&base_instructions)
        else {
            return;
        };
        {
            let mut state = self.state.lock().await;
            let mut info = state.token_info().unwrap_or(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window: None,
            });

            info.last_token_usage = TokenUsage {
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: estimated_total_tokens.max(0),
            };

            if let Some(model_context_window) = turn_context.model_context_window() {
                info.model_context_window = Some(model_context_window);
            }

            state.set_token_info(Some(info));
        }
        self.send_token_count_event(turn_context).await;
    }

    pub(crate) async fn update_rate_limits(
        &self,
        turn_context: &TurnContext,
        new_rate_limits: RateLimitSnapshot,
    ) {
        {
            let mut state = self.state.lock().await;
            state.set_rate_limits(new_rate_limits);
        }
        self.send_token_count_event(turn_context).await;
    }

    pub async fn dependency_env(&self) -> HashMap<String, String> {
        let state = self.state.lock().await;
        state.dependency_env()
    }

    pub(crate) async fn set_server_reasoning_included(&self, included: bool) {
        let mut state = self.state.lock().await;
        state.set_server_reasoning_included(included);
    }

    async fn send_token_count_event(&self, turn_context: &TurnContext) {
        let (info, rate_limits) = {
            let state = self.state.lock().await;
            state.token_info_and_rate_limits()
        };
        let event = EventMsg::TokenCount(TokenCountEvent { info, rate_limits });
        self.send_event(turn_context, event).await;
    }

    pub(crate) async fn set_total_tokens_full(&self, turn_context: &TurnContext) {
        if let Some(context_window) = turn_context.model_context_window() {
            let mut state = self.state.lock().await;
            state.set_token_usage_full(context_window);
        }
        self.send_token_count_event(turn_context).await;
    }

    pub(crate) async fn record_response_item_and_emit_turn_item(
        &self,
        turn_context: &TurnContext,
        response_item: ResponseItem,
    ) {
        // Add to conversation history and persist response item to rollout.
        self.record_conversation_items(turn_context, std::slice::from_ref(&response_item))
            .await;

        // Derive a turn item and emit lifecycle events if applicable.
        if let Some(item) = parse_turn_item(&response_item) {
            self.emit_turn_item_started(turn_context, &item).await;
            self.emit_turn_item_completed(turn_context, item).await;
        }
    }

    pub(crate) async fn record_user_prompt_and_emit_turn_item(
        &self,
        turn_context: &TurnContext,
        input: &[UserInput],
        response_item: ResponseItem,
    ) {
        // Persist the user message to history, but emit the turn item from `UserInput` so
        // UI-only `text_elements` are preserved. `ResponseItem::Message` does not carry
        // those spans, and `record_response_item_and_emit_turn_item` would drop them.
        self.record_conversation_items(turn_context, std::slice::from_ref(&response_item))
            .await;
        let turn_item = TurnItem::UserMessage(UserMessageItem::new(input));
        self.emit_turn_item_started(turn_context, &turn_item).await;
        self.emit_turn_item_completed(turn_context, turn_item).await;
        self.ensure_rollout_materialized().await;
    }

    pub(crate) async fn notify_background_event(
        &self,
        turn_context: &TurnContext,
        message: impl Into<String>,
    ) {
        let event = EventMsg::BackgroundEvent(BackgroundEventEvent {
            message: message.into(),
        });
        self.send_event(turn_context, event).await;
    }

    pub(crate) async fn notify_stream_error(
        &self,
        turn_context: &TurnContext,
        message: impl Into<String>,
        codex_error: ChaosErr,
    ) {
        let additional_details = codex_error.to_string();
        let codex_error_info = CodexErrorInfo::ResponseStreamDisconnected {
            http_status_code: codex_error.http_status_code_value(),
        };
        let event = EventMsg::StreamError(StreamErrorEvent {
            message: message.into(),
            codex_error_info: Some(codex_error_info),
            additional_details: Some(additional_details),
        });
        self.send_event(turn_context, event).await;
    }

    async fn maybe_start_ghost_snapshot(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        cancellation_token: CancellationToken,
    ) {
        if !self.enabled(Feature::GhostCommit) {
            return;
        }
        let token = match turn_context.tool_call_gate.subscribe().await {
            Ok(token) => token,
            Err(err) => {
                warn!("failed to subscribe to ghost snapshot readiness: {err}");
                return;
            }
        };

        info!("spawning ghost snapshot task");
        let task = GhostSnapshotTask::new(token);
        Arc::new(task)
            .run(
                Arc::new(SessionTaskContext::new(self.clone())),
                turn_context.clone(),
                Vec::new(),
                cancellation_token,
            )
            .await;
    }

    /// Inject additional user input into the currently active turn.
    ///
    /// Returns the active turn id when accepted.
    pub async fn steer_input(
        &self,
        input: Vec<UserInput>,
        expected_turn_id: Option<&str>,
    ) -> Result<String, SteerInputError> {
        if input.is_empty() {
            return Err(SteerInputError::EmptyInput);
        }

        let mut active = self.active_turn.lock().await;
        let Some(active_turn) = active.as_mut() else {
            return Err(SteerInputError::NoActiveTurn(input));
        };

        let Some((active_turn_id, _)) = active_turn.tasks.first() else {
            return Err(SteerInputError::NoActiveTurn(input));
        };

        if let Some(expected_turn_id) = expected_turn_id
            && expected_turn_id != active_turn_id
        {
            return Err(SteerInputError::ExpectedTurnMismatch {
                expected: expected_turn_id.to_string(),
                actual: active_turn_id.clone(),
            });
        }

        let mut turn_state = active_turn.turn_state.lock().await;
        turn_state.push_pending_input(input.into());
        Ok(active_turn_id.clone())
    }

    /// Returns the input if there was no task running to inject into
    pub async fn inject_response_items(
        &self,
        input: Vec<ResponseInputItem>,
    ) -> Result<(), Vec<ResponseInputItem>> {
        let mut active = self.active_turn.lock().await;
        match active.as_mut() {
            Some(at) => {
                let mut ts = at.turn_state.lock().await;
                for item in input {
                    ts.push_pending_input(item);
                }
                Ok(())
            }
            None => Err(input),
        }
    }

    pub async fn get_pending_input(&self) -> Vec<ResponseInputItem> {
        let mut active = self.active_turn.lock().await;
        match active.as_mut() {
            Some(at) => {
                let mut ts = at.turn_state.lock().await;
                ts.take_pending_input()
            }
            None => Vec::with_capacity(0),
        }
    }

    /// Returns `true` when there is pending input AND the mailbox
    /// delivery phase allows consumption in the current turn.
    pub async fn has_deliverable_input(&self) -> bool {
        let active = self.active_turn.lock().await;
        match active.as_ref() {
            Some(at) => {
                let ts = at.turn_state.lock().await;
                ts.has_deliverable_input()
            }
            None => false,
        }
    }

    /// Get the `TurnState` mutex for a specific sub-task by id without
    /// holding the `active_turn` lock across await points.
    async fn turn_state_for_sub_id(
        &self,
        sub_id: &str,
    ) -> Option<Arc<tokio::sync::Mutex<crate::state::TurnState>>> {
        let active = self.active_turn.lock().await;
        active.as_ref().and_then(|at| {
            at.tasks
                .contains_key(sub_id)
                .then(|| Arc::clone(&at.turn_state))
        })
    }

    /// Defer mailbox delivery to the next turn because the model emitted
    /// a final answer. Guarded: if pending input already exists the phase
    /// stays `CurrentTurn` — the turn must stay open to consume it.
    /// The guard check and transition happen under one lock acquisition.
    pub async fn defer_mailbox_delivery_to_next_turn(&self, sub_id: &str) {
        let Some(turn_state) = self.turn_state_for_sub_id(sub_id).await else {
            return;
        };
        let mut ts = turn_state.lock().await;
        ts.record_answer_emitted();
    }

    /// Reopen mailbox delivery for the current turn. Called when a tool
    /// call is emitted so steered input arriving during tool execution is
    /// consumed in the same turn.
    pub async fn accept_mailbox_delivery_for_current_turn(&self, sub_id: &str) {
        let Some(turn_state) = self.turn_state_for_sub_id(sub_id).await else {
            return;
        };
        let mut ts = turn_state.lock().await;
        ts.record_tool_call_emitted();
    }

    pub async fn list_resources(
        &self,
        server: &str,
        params: Option<PaginatedRequestParams>,
    ) -> anyhow::Result<ListResourcesResult> {
        self.services
            .mcp_connection_manager
            .read()
            .await
            .list_resources(server, params)
            .await
    }

    pub async fn list_resource_templates(
        &self,
        server: &str,
        params: Option<PaginatedRequestParams>,
    ) -> anyhow::Result<ListResourceTemplatesResult> {
        self.services
            .mcp_connection_manager
            .read()
            .await
            .list_resource_templates(server, params)
            .await
    }

    pub async fn read_resource(
        &self,
        server: &str,
        params: ReadResourceRequestParams,
    ) -> anyhow::Result<ReadResourceResult> {
        self.services
            .mcp_connection_manager
            .read()
            .await
            .read_resource(server, params)
            .await
    }

    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
    ) -> anyhow::Result<CallToolResult> {
        self.services
            .mcp_connection_manager
            .read()
            .await
            .call_tool(server, tool, arguments, meta)
            .await
    }

    pub(crate) async fn parse_mcp_tool_name(
        &self,
        name: &str,
        namespace: &Option<String>,
    ) -> Option<(String, String)> {
        let tool_name = if let Some(namespace) = namespace {
            if name.starts_with(namespace.as_str()) {
                name
            } else {
                &format!("{namespace}{name}")
            }
        } else {
            name
        };
        self.services
            .mcp_connection_manager
            .read()
            .await
            .parse_tool_name(tool_name)
            .await
    }

    pub async fn interrupt_task(self: &Arc<Self>) {
        info!("interrupt received: abort current task, if any");
        let has_active_turn = { self.active_turn.lock().await.is_some() };
        if has_active_turn {
            self.abort_all_tasks(TurnAbortReason::Interrupted).await;
        } else {
            self.cancel_mcp_startup().await;
        }
    }

    pub(crate) fn hooks(&self) -> &Hooks {
        &self.services.hooks
    }

    pub(crate) fn user_shell(&self) -> Arc<shell::Shell> {
        Arc::clone(&self.services.user_shell)
    }

    pub(crate) async fn take_pending_session_start_source(
        &self,
    ) -> Option<chaos_dtrace::SessionStartSource> {
        let mut state = self.state.lock().await;
        state.take_pending_session_start_source()
    }

    async fn refresh_mcp_servers_inner(
        &self,
        turn_context: &TurnContext,
        mcp_servers: HashMap<String, McpServerConfig>,
        store_mode: OAuthCredentialsStoreMode,
    ) {
        let config = self.get_config().await;
        let auth_statuses = compute_auth_statuses(mcp_servers.iter(), store_mode).await;
        let sandbox_state = SandboxState {
            sandbox_policy: turn_context.sandbox_policy.get().clone(),
            alcatraz_macos_exe: turn_context.alcatraz_macos_exe.clone(),
            alcatraz_linux_exe: turn_context.alcatraz_linux_exe.clone(),
            alcatraz_freebsd_exe: turn_context.alcatraz_freebsd_exe.clone(),
            sandbox_cwd: turn_context.cwd.clone(),
        };
        {
            let mut guard = self.services.mcp_startup_cancellation_token.lock().await;
            guard.cancel();
            *guard = CancellationToken::new();
        }
        let (refreshed_manager, cancel_token) = McpConnectionManager::new(
            &mcp_servers,
            store_mode,
            auth_statuses,
            &turn_context.config.permissions.approval_policy,
            self.get_tx_event(),
            sandbox_state,
            config.chaos_home.clone(),
            Arc::clone(&self.services.catalog) as Arc<dyn chaos_traits::McpCatalogSink>,
        )
        .await;
        {
            let mut guard = self.services.mcp_startup_cancellation_token.lock().await;
            if guard.is_cancelled() {
                cancel_token.cancel();
            }
            *guard = cancel_token;
        }

        let mut manager = self.services.mcp_connection_manager.write().await;
        *manager = refreshed_manager;

        // Re-sync MCP tools into catalog after refresh.
        let mcp_tools = manager.list_all_tools().await;
        {
            let mut catalog = self
                .services
                .catalog
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Clear all previous MCP entries and re-register.
            catalog.clear_all_mcp();
            for tool_info in mcp_tools.values() {
                catalog.register_mcp_tools(
                    &tool_info.server_name,
                    vec![chaos_mcp_runtime::catalog_conv::mcp_tool_info_to_catalog_tool(tool_info)],
                );
            }
        }
    }

    async fn refresh_mcp_servers_if_requested(&self, turn_context: &TurnContext) {
        let refresh_config = { self.pending_mcp_server_refresh_config.lock().await.take() };
        let Some(refresh_config) = refresh_config else {
            return;
        };

        let McpServerRefreshConfig {
            mcp_servers,
            mcp_oauth_credentials_store_mode,
        } = refresh_config;

        let mcp_servers =
            match serde_json::from_value::<HashMap<String, McpServerConfig>>(mcp_servers) {
                Ok(servers) => servers,
                Err(err) => {
                    warn!("failed to parse MCP server refresh config: {err}");
                    return;
                }
            };
        let store_mode = match serde_json::from_value::<OAuthCredentialsStoreMode>(
            mcp_oauth_credentials_store_mode,
        ) {
            Ok(mode) => mode,
            Err(err) => {
                warn!("failed to parse MCP OAuth refresh config: {err}");
                return;
            }
        };

        self.refresh_mcp_servers_inner(turn_context, mcp_servers, store_mode)
            .await;
    }

    #[cfg(test)]
    #[expect(dead_code, reason = "test helper available for future tests")]
    async fn mcp_startup_cancellation_token(&self) -> CancellationToken {
        self.services
            .mcp_startup_cancellation_token
            .lock()
            .await
            .clone()
    }

    async fn cancel_mcp_startup(&self) {
        self.services
            .mcp_startup_cancellation_token
            .lock()
            .await
            .cancel();
    }
}

async fn submission_loop(sess: Arc<Session>, config: Arc<Config>, rx_sub: Receiver<Submission>) {
    // To break out of this loop, send Op::Shutdown.
    while let Ok(sub) = rx_sub.recv().await {
        debug!(?sub, "Submission");
        let dispatch_span = submission_dispatch_span(&sub);
        let should_exit = async {
            match sub.op.clone() {
                Op::Interrupt => {
                    handlers::interrupt(&sess).await;
                    false
                }
                Op::CleanBackgroundTerminals => {
                    handlers::clean_background_terminals(&sess).await;
                    false
                }
                Op::OverrideTurnContext {
                    cwd,
                    approval_policy,
                    approvals_reviewer,
                    sandbox_policy,
                    windows_sandbox_level,
                    model,
                    effort,
                    summary,
                    service_tier,
                    collaboration_mode,
                    personality,
                } => {
                    let collaboration_mode = if let Some(collab_mode) = collaboration_mode {
                        collab_mode
                    } else {
                        let state = sess.state.lock().await;
                        state.session_configuration.collaboration_mode.with_updates(
                            model.clone(),
                            effort,
                            /*minion_instructions*/ None,
                        )
                    };
                    // If clamped and model changed, forward to claude subprocess.
                    if sess.services.model_client.is_clamped()
                        && let Some(ref model_slug) = model
                        && let Err(e) = sess.services.model_client.set_clamp_model(model_slug).await
                    {
                        warn!("failed to set clamped model: {e}");
                    }
                    handlers::override_turn_context(
                        &sess,
                        sub.id.clone(),
                        SessionSettingsUpdate {
                            cwd,
                            approval_policy,
                            approvals_reviewer,
                            sandbox_policy,
                            windows_sandbox_level,
                            collaboration_mode: Some(collaboration_mode),
                            reasoning_summary: summary,
                            service_tier,
                            personality,
                            ..Default::default()
                        },
                    )
                    .await;
                    false
                }
                Op::SetClamped { enabled } => {
                    sess.services.model_client.set_clamped(enabled).await;
                    let mode = if enabled {
                        "clamped (Claude Code MAX)"
                    } else {
                        "direct API"
                    };
                    sess.send_event_raw(Event {
                        id: sub.id.clone(),
                        msg: EventMsg::AgentMessage(AgentMessageEvent {
                            message: format!("Transport switched to {mode}."),
                            phase: None,
                        }),
                    })
                    .await;
                    false
                }
                Op::UserInput { .. } | Op::UserTurn { .. } => {
                    handlers::user_input_or_turn(&sess, sub.id.clone(), sub.op).await;
                    false
                }
                Op::ExecApproval {
                    id: approval_id,
                    turn_id,
                    decision,
                } => {
                    handlers::exec_approval(&sess, approval_id, turn_id, decision).await;
                    false
                }
                Op::PatchApproval { id, decision } => {
                    handlers::patch_approval(&sess, id, decision).await;
                    false
                }
                Op::UserInputAnswer { id, response } => {
                    handlers::request_user_input_response(&sess, id, response).await;
                    false
                }
                Op::RequestPermissionsResponse { id, response } => {
                    handlers::request_permissions_response(&sess, id, response).await;
                    false
                }
                Op::DynamicToolResponse { id, response } => {
                    handlers::dynamic_tool_response(&sess, id, response).await;
                    false
                }
                Op::AddToHistory { text } => {
                    handlers::add_to_history(&sess, &config, text).await;
                    false
                }
                Op::GetHistoryEntryRequest { offset, log_id } => {
                    handlers::get_history_entry_request(
                        &sess,
                        &config,
                        sub.id.clone(),
                        offset,
                        log_id,
                    )
                    .await;
                    false
                }
                Op::ListMcpTools => {
                    handlers::list_mcp_tools(&sess, &config, sub.id.clone()).await;
                    false
                }
                Op::ListAllTools => {
                    handlers::list_all_tools(&sess, &config, sub.id.clone()).await;
                    false
                }
                Op::RefreshMcpServers { config } => {
                    handlers::refresh_mcp_servers(&sess, config).await;
                    false
                }
                Op::ReloadUserConfig => {
                    handlers::reload_user_config(&sess).await;
                    false
                }
                Op::ListCustomPrompts => {
                    handlers::list_custom_prompts(&sess, sub.id.clone()).await;
                    false
                }
                Op::ListSkills { cwds, force_reload } => {
                    handlers::list_skills(&sess, sub.id.clone(), cwds, force_reload).await;
                    false
                }
                Op::ListRemoteSkills {
                    hazelnut_scope,
                    product_surface,
                    enabled,
                } => {
                    handlers::list_remote_skills(
                        &sess,
                        &config,
                        sub.id.clone(),
                        hazelnut_scope,
                        product_surface,
                        enabled,
                    )
                    .await;
                    false
                }
                Op::DownloadRemoteSkill { hazelnut_id } => {
                    handlers::export_remote_skill(&sess, &config, sub.id.clone(), hazelnut_id)
                        .await;
                    false
                }
                Op::Undo => {
                    handlers::undo(&sess, sub.id.clone()).await;
                    false
                }
                Op::Compact => {
                    handlers::compact(&sess, sub.id.clone()).await;
                    false
                }
                Op::DropMemories | Op::UpdateMemories => {
                    // Memory subsystem evicted — these ops are now no-ops.
                    false
                }
                Op::ProcessRollback { num_turns } => {
                    handlers::process_rollback(&sess, sub.id.clone(), num_turns).await;
                    false
                }
                Op::SetProcessName { name } => {
                    handlers::set_process_name(&sess, sub.id.clone(), name).await;
                    false
                }
                Op::RunUserShellCommand { command } => {
                    handlers::run_user_shell_command(&sess, sub.id.clone(), command).await;
                    false
                }
                Op::ResolveElicitation {
                    server_name,
                    request_id,
                    decision,
                    content,
                    meta,
                } => {
                    handlers::resolve_elicitation(
                        &sess,
                        server_name,
                        request_id,
                        decision,
                        content,
                        meta,
                    )
                    .await;
                    false
                }
                Op::Shutdown => handlers::shutdown(&sess, sub.id.clone()).await,
                Op::Review { review_request } => {
                    handlers::review(&sess, &config, sub.id.clone(), review_request).await;
                    false
                }
                _ => false, // Ignore unknown ops; enum is non_exhaustive to allow extensions.
            }
        }
        .instrument(dispatch_span)
        .await;
        if should_exit {
            break;
        }
    }

    debug!("Agent loop exited");
}

fn submission_dispatch_span(sub: &Submission) -> tracing::Span {
    let op_name = sub.op.kind();
    let span_name = format!("op.dispatch.{op_name}");
    let dispatch_span = info_span!(
        "submission_dispatch",
        otel.name = span_name.as_str(),
        submission.id = sub.id.as_str(),
        codex.op = op_name
    );
    if let Some(trace) = sub.trace.as_ref()
        && !set_parent_from_w3c_trace_context(&dispatch_span, trace)
    {
        warn!(
            submission.id = sub.id.as_str(),
            "ignoring invalid submission trace carrier"
        );
    }
    dispatch_span
}

/// Operation handlers
mod handlers {
    use crate::chaos::Session;
    use crate::chaos::SessionSettingsUpdate;
    use crate::chaos::SteerInputError;

    use crate::chaos::spawn_review_thread;
    use crate::config::Config;

    use crate::mcp::auth::compute_auth_statuses;
    use crate::mcp::collect_mcp_snapshot_from_manager;
    use crate::review_prompts::resolve_review_request;
    use crate::rollout::RolloutRecorder;
    use crate::rollout::process_names;
    use crate::tasks::CompactTask;
    use crate::tasks::UndoTask;
    use crate::tasks::UserShellCommandMode;
    use crate::tasks::UserShellCommandTask;
    use crate::tasks::execute_user_shell_command;
    use chaos_ipc::custom_prompts::CustomPrompt;
    use chaos_ipc::protocol::CodexErrorInfo;
    use chaos_ipc::protocol::ErrorEvent;
    use chaos_ipc::protocol::Event;
    use chaos_ipc::protocol::EventMsg;
    use chaos_ipc::protocol::InitialHistory;
    use chaos_ipc::protocol::ListCustomPromptsResponseEvent;
    use chaos_ipc::protocol::ListRemoteSkillsResponseEvent;
    use chaos_ipc::protocol::ListSkillsResponseEvent;
    use chaos_ipc::protocol::McpServerRefreshConfig;
    use chaos_ipc::protocol::Op;
    use chaos_ipc::protocol::ProcessNameUpdatedEvent;
    use chaos_ipc::protocol::ProcessRolledBackEvent;
    use chaos_ipc::protocol::RemoteSkillDownloadedEvent;
    use chaos_ipc::protocol::RemoteSkillHazelnutScope;
    use chaos_ipc::protocol::RemoteSkillProductSurface;
    use chaos_ipc::protocol::RemoteSkillSummary;
    use chaos_ipc::protocol::ResumedHistory;
    use chaos_ipc::protocol::ReviewDecision;
    use chaos_ipc::protocol::ReviewRequest;
    use chaos_ipc::protocol::RolloutItem;
    use chaos_ipc::protocol::SkillsListEntry;
    use chaos_ipc::protocol::TurnAbortReason;
    use chaos_ipc::protocol::WarningEvent;
    use chaos_ipc::request_permissions::RequestPermissionsResponse;
    use chaos_ipc::request_user_input::RequestUserInputResponse;

    use crate::context_manager::is_user_turn_boundary;
    use chaos_ipc::config_types::CollaborationMode;
    use chaos_ipc::config_types::ModeKind;
    use chaos_ipc::config_types::Settings;
    use chaos_ipc::dynamic_tools::DynamicToolResponse;
    use chaos_ipc::mcp::RequestId as ProtocolRequestId;
    use chaos_ipc::user_input::UserInput;
    use chaos_mcp_runtime::ElicitationAction;
    use chaos_mcp_runtime::ElicitationResponse;
    use serde_json::Value;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tracing::info;
    use tracing::warn;

    pub async fn interrupt(sess: &Arc<Session>) {
        sess.interrupt_task().await;
    }

    pub async fn clean_background_terminals(sess: &Arc<Session>) {
        sess.close_unified_exec_processes().await;
    }

    pub async fn override_turn_context(
        sess: &Session,
        sub_id: String,
        updates: SessionSettingsUpdate,
    ) {
        if let Err(err) = sess.update_settings(updates).await {
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: err.to_string(),
                    codex_error_info: Some(CodexErrorInfo::BadRequest),
                }),
            })
            .await;
        }
    }

    pub async fn user_input_or_turn(sess: &Arc<Session>, sub_id: String, op: Op) {
        let (items, updates) = match op {
            Op::UserTurn {
                cwd,
                approval_policy,
                sandbox_policy,
                model,
                effort,
                summary,
                service_tier,
                final_output_json_schema,
                items,
                collaboration_mode,
                personality,
            } => {
                let collaboration_mode = collaboration_mode.or_else(|| {
                    Some(CollaborationMode {
                        mode: ModeKind::Default,
                        settings: Settings {
                            model: model.clone(),
                            reasoning_effort: effort,
                            minion_instructions: None,
                        },
                    })
                });
                (
                    items,
                    SessionSettingsUpdate {
                        cwd: Some(cwd),
                        approval_policy: Some(approval_policy),
                        approvals_reviewer: None,
                        sandbox_policy: Some(sandbox_policy),
                        windows_sandbox_level: None,
                        collaboration_mode,
                        reasoning_summary: summary,
                        service_tier,
                        final_output_json_schema: Some(final_output_json_schema),
                        personality,
                        app_server_client_name: None,
                    },
                )
            }
            Op::UserInput {
                items,
                final_output_json_schema,
            } => (
                items,
                SessionSettingsUpdate {
                    final_output_json_schema: Some(final_output_json_schema),
                    ..Default::default()
                },
            ),
            _ => unreachable!(),
        };

        let Ok(current_context) = sess.new_turn_with_sub_id(sub_id, updates).await else {
            // new_turn_with_sub_id already emits the error event.
            return;
        };
        sess.maybe_emit_unknown_model_warning_for_turn(current_context.as_ref())
            .await;
        current_context.session_telemetry.user_prompt(&items);

        // Attempt to inject input into current task.
        if let Err(SteerInputError::NoActiveTurn(items)) =
            sess.steer_input(items, /*expected_turn_id*/ None).await
        {
            sess.refresh_mcp_servers_if_requested(&current_context)
                .await;
            let regular_task = sess.take_startup_regular_task().await.unwrap_or_default();
            sess.spawn_task(Arc::clone(&current_context), items, regular_task)
                .await;
        }
    }

    pub async fn run_user_shell_command(sess: &Arc<Session>, sub_id: String, command: String) {
        if let Some((turn_context, cancellation_token)) =
            sess.active_turn_context_and_cancellation_token().await
        {
            let session = Arc::clone(sess);
            tokio::spawn(async move {
                execute_user_shell_command(
                    session,
                    turn_context,
                    command,
                    cancellation_token,
                    UserShellCommandMode::ActiveTurnAuxiliary,
                )
                .await;
            });
            return;
        }

        let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;
        sess.spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            UserShellCommandTask::new(command),
        )
        .await;
    }

    pub async fn resolve_elicitation(
        sess: &Arc<Session>,
        server_name: String,
        request_id: ProtocolRequestId,
        decision: chaos_ipc::approvals::ElicitationAction,
        content: Option<Value>,
        meta: Option<Value>,
    ) {
        let action = match decision {
            chaos_ipc::approvals::ElicitationAction::Accept => ElicitationAction::Accept,
            chaos_ipc::approvals::ElicitationAction::Decline => ElicitationAction::Decline,
            chaos_ipc::approvals::ElicitationAction::Cancel => ElicitationAction::Cancel,
        };
        let content = match action {
            // Preserve the legacy fallback for clients that only send an action.
            ElicitationAction::Accept => Some(content.unwrap_or_else(|| serde_json::json!({}))),
            ElicitationAction::Decline | ElicitationAction::Cancel => None,
        };
        let response = ElicitationResponse {
            action,
            content,
            meta,
        };
        let request_id = chaos_mcp_runtime::manager::protocol_request_id_to_guest(&request_id);
        if let Err(err) = sess
            .resolve_elicitation(server_name, request_id, response)
            .await
        {
            warn!(
                error = %err,
                "failed to resolve elicitation request in session"
            );
        }
    }

    /// Propagate a user's exec approval decision to the session.
    /// Also optionally applies an execpolicy amendment.
    pub async fn exec_approval(
        sess: &Arc<Session>,
        approval_id: String,
        turn_id: Option<String>,
        decision: ReviewDecision,
    ) {
        let event_turn_id = turn_id.unwrap_or_else(|| approval_id.clone());
        if let ReviewDecision::ApprovedExecpolicyAmendment {
            proposed_execpolicy_amendment,
        } = &decision
        {
            match sess
                .persist_execpolicy_amendment(proposed_execpolicy_amendment)
                .await
            {
                Ok(()) => {
                    sess.record_execpolicy_amendment_message(
                        &event_turn_id,
                        proposed_execpolicy_amendment,
                    )
                    .await;
                }
                Err(err) => {
                    let message = format!("Failed to apply execpolicy amendment: {err}");
                    tracing::warn!("{message}");
                    let warning = EventMsg::Warning(WarningEvent { message });
                    sess.send_event_raw(Event {
                        id: event_turn_id.clone(),
                        msg: warning,
                    })
                    .await;
                }
            }
        }
        match decision {
            ReviewDecision::Abort => {
                sess.interrupt_task().await;
            }
            other => sess.notify_approval(&approval_id, other).await,
        }
    }

    pub async fn patch_approval(sess: &Arc<Session>, id: String, decision: ReviewDecision) {
        match decision {
            ReviewDecision::Abort => {
                sess.interrupt_task().await;
            }
            other => sess.notify_approval(&id, other).await,
        }
    }

    pub async fn request_user_input_response(
        sess: &Arc<Session>,
        id: String,
        response: RequestUserInputResponse,
    ) {
        sess.notify_user_input_response(&id, response).await;
    }

    pub async fn request_permissions_response(
        sess: &Arc<Session>,
        id: String,
        response: RequestPermissionsResponse,
    ) {
        sess.notify_request_permissions_response(&id, response)
            .await;
    }

    pub async fn dynamic_tool_response(
        sess: &Arc<Session>,
        id: String,
        response: DynamicToolResponse,
    ) {
        sess.notify_dynamic_tool_response(&id, response).await;
    }

    pub async fn add_to_history(sess: &Arc<Session>, config: &Arc<Config>, text: String) {
        let id = sess.conversation_id;
        let config = Arc::clone(config);
        let state_db = sess.services.state_db.clone();
        tokio::spawn(async move {
            if let Err(e) =
                crate::message_history::append_entry(&text, &id, state_db.as_deref(), &config).await
            {
                warn!("failed to append to message history: {e}");
            }
        });
    }

    pub async fn get_history_entry_request(
        sess: &Arc<Session>,
        config: &Arc<Config>,
        sub_id: String,
        offset: usize,
        log_id: u64,
    ) {
        let sess_clone = Arc::clone(sess);
        let state_db = sess.services.state_db.clone();
        let _config = Arc::clone(config);

        tokio::spawn(async move {
            let entry_opt =
                crate::message_history::lookup(log_id, offset, state_db.as_deref()).await;

            let event = Event {
                id: sub_id,
                msg: EventMsg::GetHistoryEntryResponse(
                    crate::protocol::GetHistoryEntryResponseEvent {
                        offset,
                        log_id,
                        entry: entry_opt,
                    },
                ),
            };

            sess_clone.send_event_raw(event).await;
        });
    }

    pub async fn refresh_mcp_servers(sess: &Arc<Session>, refresh_config: McpServerRefreshConfig) {
        let mut guard = sess.pending_mcp_server_refresh_config.lock().await;
        *guard = Some(refresh_config);
    }

    pub async fn reload_user_config(sess: &Arc<Session>) {
        sess.reload_user_config_layer().await;
    }

    pub async fn list_all_tools(sess: &Session, _config: &Arc<Config>, sub_id: String) {
        use chaos_ipc::protocol::AllToolsResponseEvent;
        use chaos_ipc::protocol::ToolSummary;

        let mut tools: Vec<ToolSummary> = {
            let catalog = sess
                .services
                .catalog
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            catalog
                .tools()
                .iter()
                .map(|(source, tool)| {
                    let source_str = match source {
                        crate::catalog::CatalogSource::Module(name) => name.clone(),
                        crate::catalog::CatalogSource::Mcp(name) => format!("mcp:{name}"),
                    };
                    let annotation_labels = tool
                        .annotations
                        .as_ref()
                        .and_then(|v| {
                            serde_json::from_value::<chaos_mcp_runtime::ToolAnnotations>(v.clone())
                                .ok()
                        })
                        .map(|ann| {
                            let mut labels = crate::tools::spec::annotation_labels(&ann);
                            let has_read_semantics =
                                labels.iter().any(|label| label == "read-only" || label == "writes");
                            if !has_read_semantics
                                && let Some(read_only) = tool.read_only_hint
                            {
                                labels.push(
                                    if read_only { "read-only" } else { "writes" }.to_string()
                                );
                            }
                            labels
                        })
                        .or_else(|| {
                            tool.read_only_hint.map(|read_only| {
                                vec![if read_only { "read-only" } else { "writes" }.to_string()]
                            })
                        })
                        .unwrap_or_default();
                    ToolSummary {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        annotation_labels,
                        annotations: tool.annotations.clone(),
                        source: source_str,
                    }
                })
                .collect()
        };

        // Include script-defined tools from the hallucinate engine.
        if let Some(ref handle) = sess.services.hallucinate {
            for tool in handle.list_tools().await {
                tools.push(ToolSummary {
                    name: tool.name,
                    description: tool.description,
                    annotation_labels: Vec::new(),
                    annotations: None,
                    source: "hallucinate".to_string(),
                });
            }
        }

        let event = Event {
            id: sub_id,
            msg: EventMsg::AllToolsResponse(AllToolsResponseEvent { tools }),
        };
        sess.send_event_raw(event).await;
    }

    pub async fn list_mcp_tools(sess: &Session, _config: &Arc<Config>, sub_id: String) {
        let mcp_connection_manager = sess.services.mcp_connection_manager.read().await;
        let _auth = sess.services.auth_manager.auth().await;
        let config = sess.get_config().await;
        let mcp_servers = sess.services.mcp_manager.effective_servers(&config);
        let snapshot = collect_mcp_snapshot_from_manager(
            &mcp_connection_manager,
            compute_auth_statuses(mcp_servers.iter(), config.mcp_oauth_credentials_store_mode)
                .await,
        )
        .await;
        let event = Event {
            id: sub_id,
            msg: EventMsg::McpListToolsResponse(snapshot),
        };
        sess.send_event_raw(event).await;
    }

    pub async fn list_custom_prompts(sess: &Session, sub_id: String) {
        let custom_prompts: Vec<CustomPrompt> =
            if let Some(dir) = crate::custom_prompts::default_prompts_dir() {
                crate::custom_prompts::discover_prompts_in(&dir).await
            } else {
                Vec::new()
            };

        let event = Event {
            id: sub_id,
            msg: EventMsg::ListCustomPromptsResponse(ListCustomPromptsResponseEvent {
                custom_prompts,
            }),
        };
        sess.send_event_raw(event).await;
    }

    pub async fn list_skills(
        sess: &Session,
        sub_id: String,
        cwds: Vec<PathBuf>,
        force_reload: bool,
    ) {
        let cwds = if cwds.is_empty() {
            let state = sess.state.lock().await;
            vec![state.session_configuration.cwd.clone()]
        } else {
            cwds
        };

        let skills_manager = &sess.services.skills_manager;
        let mut skills = Vec::new();
        for cwd in cwds {
            let outcome = skills_manager.skills_for_cwd(&cwd, force_reload).await;
            let errors = super::errors_to_info(&outcome.errors);
            let skills_metadata = super::skills_to_info(&outcome.skills, &outcome.disabled_paths);
            skills.push(SkillsListEntry {
                cwd,
                skills: skills_metadata,
                errors,
            });
        }

        let event = Event {
            id: sub_id,
            msg: EventMsg::ListSkillsResponse(ListSkillsResponseEvent { skills }),
        };
        sess.send_event_raw(event).await;
    }

    pub async fn list_remote_skills(
        sess: &Session,
        config: &Arc<Config>,
        sub_id: String,
        hazelnut_scope: RemoteSkillHazelnutScope,
        product_surface: RemoteSkillProductSurface,
        enabled: Option<bool>,
    ) {
        let auth = sess.services.auth_manager.auth().await;
        let response = crate::skills::remote::list_remote_skills(
            config,
            auth.as_ref(),
            hazelnut_scope,
            product_surface,
            enabled,
        )
        .await
        .map(|skills| {
            skills
                .into_iter()
                .map(|skill| RemoteSkillSummary {
                    id: skill.id,
                    name: skill.name,
                    description: skill.description,
                })
                .collect::<Vec<_>>()
        });

        match response {
            Ok(skills) => {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::ListRemoteSkillsResponse(ListRemoteSkillsResponseEvent {
                        skills,
                    }),
                };
                sess.send_event_raw(event).await;
            }
            Err(err) => {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(ErrorEvent {
                        message: format!("failed to list remote skills: {err}"),
                        codex_error_info: Some(CodexErrorInfo::Other),
                    }),
                };
                sess.send_event_raw(event).await;
            }
        }
    }

    pub async fn export_remote_skill(
        sess: &Session,
        config: &Arc<Config>,
        sub_id: String,
        hazelnut_id: String,
    ) {
        let auth = sess.services.auth_manager.auth().await;
        match crate::skills::remote::export_remote_skill(
            config,
            auth.as_ref(),
            hazelnut_id.as_str(),
        )
        .await
        {
            Ok(result) => {
                let id = result.id;
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::RemoteSkillDownloaded(RemoteSkillDownloadedEvent {
                        id: id.clone(),
                        name: id,
                        path: result.path,
                    }),
                };
                sess.send_event_raw(event).await;
            }
            Err(err) => {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(ErrorEvent {
                        message: format!("failed to export remote skill {hazelnut_id}: {err}"),
                        codex_error_info: Some(CodexErrorInfo::Other),
                    }),
                };
                sess.send_event_raw(event).await;
            }
        }
    }

    pub async fn undo(sess: &Arc<Session>, sub_id: String) {
        let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;
        sess.spawn_task(turn_context, Vec::new(), UndoTask::new())
            .await;
    }

    pub async fn compact(sess: &Arc<Session>, sub_id: String) {
        let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;

        sess.spawn_task(
            Arc::clone(&turn_context),
            vec![UserInput::Text {
                text: turn_context.compact_prompt().to_string(),
                // Compaction prompt is synthesized; no UI element ranges to preserve.
                text_elements: Vec::new(),
            }],
            CompactTask,
        )
        .await;
    }

    pub async fn process_rollback(sess: &Arc<Session>, sub_id: String, num_turns: u32) {
        if num_turns == 0 {
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: "num_turns must be >= 1".to_string(),
                    codex_error_info: Some(CodexErrorInfo::ProcessRollbackFailed),
                }),
            })
            .await;
            return;
        }

        let has_active_turn = { sess.active_turn.lock().await.is_some() };
        if has_active_turn {
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: "Cannot rollback while a turn is in progress.".to_string(),
                    codex_error_info: Some(CodexErrorInfo::ProcessRollbackFailed),
                }),
            })
            .await;
            return;
        }

        let turn_context = sess.new_default_turn_with_sub_id(sub_id).await;
        let recorder = {
            let guard = sess.services.rollout.lock().await;
            guard.clone()
        };
        let Some(recorder) = recorder else {
            sess.send_event_raw(Event {
                id: turn_context.sub_id.clone(),
                msg: EventMsg::Error(ErrorEvent {
                    message: "thread rollback requires persisted session history".to_string(),
                    codex_error_info: Some(CodexErrorInfo::ProcessRollbackFailed),
                }),
            })
            .await;
            return;
        };
        if let Err(err) = recorder.flush().await {
            sess.send_event_raw(Event {
                id: turn_context.sub_id.clone(),
                msg: EventMsg::Error(ErrorEvent {
                    message: format!(
                        "failed to flush persisted session history for rollback replay: {err}"
                    ),
                    codex_error_info: Some(CodexErrorInfo::ProcessRollbackFailed),
                }),
            })
            .await;
            return;
        }

        let initial_history = match RolloutRecorder::get_rollout_history_for_process(
            sess.conversation_id,
        )
        .await
        {
            Ok(history) => history,
            Err(err) => {
                let live_rollout_items = recorder.snapshot_rollout_items();
                if live_rollout_items.is_empty() {
                    sess.send_event_raw(Event {
                            id: turn_context.sub_id.clone(),
                            msg: EventMsg::Error(ErrorEvent {
                                message: format!(
                                    "failed to load persisted session history for rollback replay: {err}"
                                ),
                                codex_error_info: Some(CodexErrorInfo::ProcessRollbackFailed),
                            }),
                        })
                        .await;
                    return;
                }
                InitialHistory::Resumed(ResumedHistory {
                    conversation_id: sess.conversation_id,
                    history: live_rollout_items,
                })
            }
        };

        let rollback_event = ProcessRolledBackEvent { num_turns };
        let rollback_msg = EventMsg::ProcessRolledBack(rollback_event.clone());
        let replay_items = initial_history
            .get_rollout_items()
            .into_iter()
            .chain(std::iter::once(RolloutItem::EventMsg(rollback_msg.clone())))
            .collect::<Vec<_>>();
        sess.persist_rollout_items(&[RolloutItem::EventMsg(rollback_msg.clone())])
            .await;
        sess.flush_rollout().await;
        sess.apply_rollout_reconstruction(turn_context.as_ref(), replay_items.as_slice())
            .await;
        sess.recompute_token_usage(turn_context.as_ref()).await;

        sess.deliver_event_raw(Event {
            id: turn_context.sub_id.clone(),
            msg: rollback_msg,
        })
        .await;
    }

    /// Persists the explicit process name in SQLite, updates in-memory state, and emits
    /// a `ProcessNameUpdated` event on success.
    /// It then updates `SessionConfiguration::process_name`.
    /// Returns an error event if the name is empty or session persistence is disabled.
    pub async fn set_process_name(sess: &Arc<Session>, sub_id: String, name: String) {
        let Some(name) = crate::util::normalize_process_name(&name) else {
            let event = Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: "Process name cannot be empty.".to_string(),
                    codex_error_info: Some(CodexErrorInfo::BadRequest),
                }),
            };
            sess.send_event_raw(event).await;
            return;
        };

        let persistence_enabled = {
            let rollout = sess.services.rollout.lock().await;
            rollout.is_some()
        };
        if !persistence_enabled {
            let event = Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: "Session persistence is disabled; cannot rename process.".to_string(),
                    codex_error_info: Some(CodexErrorInfo::Other),
                }),
            };
            sess.send_event_raw(event).await;
            return;
        };

        let chaos_home = sess.chaos_home().await;
        if let Err(e) =
            process_names::append_process_name(&chaos_home, sess.conversation_id, &name).await
        {
            let event = Event {
                id: sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: format!("Failed to set process name: {e}"),
                    codex_error_info: Some(CodexErrorInfo::Other),
                }),
            };
            sess.send_event_raw(event).await;
            return;
        }

        {
            let mut state = sess.state.lock().await;
            state.session_configuration.process_name = Some(name.clone());
        }

        sess.send_event_raw(Event {
            id: sub_id,
            msg: EventMsg::ProcessNameUpdated(ProcessNameUpdatedEvent {
                process_id: sess.conversation_id,
                process_name: Some(name),
            }),
        })
        .await;
    }

    pub async fn shutdown(sess: &Arc<Session>, sub_id: String) -> bool {
        sess.abort_all_tasks(TurnAbortReason::Interrupted).await;
        sess.services
            .unified_exec_manager
            .terminate_all_processes()
            .await;
        info!("Shutting down Chaos instance");
        let history = sess.clone_history().await;
        let turn_count = history
            .raw_items()
            .iter()
            .filter(|item| is_user_turn_boundary(item))
            .count();
        sess.services.session_telemetry.counter(
            "codex.conversation.turn.count",
            i64::try_from(turn_count).unwrap_or(0),
            &[],
        );

        // Gracefully flush and shutdown the session history recorder on session end.
        let recorder_opt = {
            let mut guard = sess.services.rollout.lock().await;
            guard.take()
        };
        if let Some(rec) = recorder_opt
            && let Err(e) = rec.shutdown().await
        {
            warn!("failed to shutdown rollout recorder: {e}");
            let event = Event {
                id: sub_id.clone(),
                msg: EventMsg::Error(ErrorEvent {
                    message: "Failed to shutdown rollout recorder".to_string(),
                    codex_error_info: Some(CodexErrorInfo::Other),
                }),
            };
            sess.send_event_raw(event).await;
        }

        let event = Event {
            id: sub_id,
            msg: EventMsg::ShutdownComplete,
        };
        sess.send_event_raw(event).await;
        true
    }

    pub async fn review(
        sess: &Arc<Session>,
        config: &Arc<Config>,
        sub_id: String,
        review_request: ReviewRequest,
    ) {
        let turn_context = sess.new_default_turn_with_sub_id(sub_id.clone()).await;
        sess.maybe_emit_unknown_model_warning_for_turn(turn_context.as_ref())
            .await;
        sess.refresh_mcp_servers_if_requested(&turn_context).await;
        match resolve_review_request(review_request, turn_context.cwd.as_path()) {
            Ok(resolved) => {
                spawn_review_thread(
                    Arc::clone(sess),
                    Arc::clone(config),
                    turn_context.clone(),
                    sub_id,
                    resolved,
                )
                .await;
            }
            Err(err) => {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(ErrorEvent {
                        message: err.to_string(),
                        codex_error_info: Some(CodexErrorInfo::Other),
                    }),
                };
                sess.send_event(&turn_context, event.msg).await;
            }
        }
    }
}

/// Spawn a review thread using the given prompt.
async fn spawn_review_thread(
    sess: Arc<Session>,
    config: Arc<Config>,
    parent_turn_context: Arc<TurnContext>,
    sub_id: String,
    resolved: crate::review_prompts::ResolvedReviewRequest,
) {
    let model = config
        .review_model
        .clone()
        .unwrap_or_else(|| parent_turn_context.model_info.slug.clone());
    let review_model_info = sess
        .services
        .models_manager
        .get_model_info(&model, &config)
        .await;
    // For reviews, disable web_search and view_image regardless of global settings.
    let review_features = sess.features.clone();
    let review_web_search_mode = WebSearchMode::Disabled;
    let tools_config = ToolsConfig::new(&ToolsConfigParams {
        model_info: &review_model_info,
        available_models: &sess
            .services
            .models_manager
            .list_models(RefreshStrategy::OnlineIfUncached)
            .await,
        features: &review_features,
        web_search_mode: Some(review_web_search_mode),
        session_source: parent_turn_context.session_source.clone(),
        sandbox_policy: parent_turn_context.sandbox_policy.get(),
        windows_sandbox_level: parent_turn_context.windows_sandbox_level,
    })
    .with_unified_exec_shell_mode_for_session(
        sess.services.user_shell.as_ref(),
        sess.services.shell_zsh_path.as_ref(),
        sess.services.main_execve_wrapper_exe.as_ref(),
    )
    .with_web_search_config(/*web_search_config*/ None)
    .with_allow_login_shell(config.permissions.allow_login_shell)
    .with_agent_roles(config.agent_roles.clone());

    let review_prompt = resolved.prompt.clone();
    let provider = parent_turn_context.provider.clone();
    let auth_manager = parent_turn_context.auth_manager.clone();
    let model_info = review_model_info.clone();

    // Build per‑turn client with the requested model/family.
    let mut per_turn_config = (*config).clone();
    per_turn_config.model = Some(model.clone());
    per_turn_config.features = review_features.clone();
    if let Err(err) = per_turn_config.web_search_mode.set(review_web_search_mode) {
        let fallback_value = per_turn_config.web_search_mode.value();
        tracing::warn!(
            error = %err,
            ?review_web_search_mode,
            ?fallback_value,
            "review web_search_mode is disallowed by requirements; keeping constrained value"
        );
    }

    let session_telemetry = parent_turn_context
        .session_telemetry
        .clone()
        .with_model(model.as_str(), review_model_info.slug.as_str());
    let auth_manager_for_context = auth_manager.clone();
    let provider_for_context = provider.clone();
    let session_telemetry_for_context = session_telemetry.clone();
    let reasoning_effort = per_turn_config.model_reasoning_effort;
    let reasoning_summary = per_turn_config
        .model_reasoning_summary
        .unwrap_or(model_info.default_reasoning_summary);
    let session_source = parent_turn_context.session_source.clone();

    let per_turn_config = Arc::new(per_turn_config);
    let review_turn_id = sub_id.to_string();
    let turn_metadata_state = Arc::new(TurnMetadataState::new(
        review_turn_id.clone(),
        parent_turn_context.cwd.clone(),
        parent_turn_context.sandbox_policy.get(),
    ));

    let review_turn_context = TurnContext {
        sub_id: review_turn_id,
        trace_id: current_span_trace_id(),
        config: per_turn_config,
        auth_manager: auth_manager_for_context,
        model_info: model_info.clone(),
        session_telemetry: session_telemetry_for_context,
        provider: provider_for_context,
        reasoning_effort,
        reasoning_summary,
        session_source,
        tools_config,
        features: parent_turn_context.features.clone(),
        ghost_snapshot: parent_turn_context.ghost_snapshot.clone(),
        current_date: parent_turn_context.current_date.clone(),
        timezone: parent_turn_context.timezone.clone(),
        app_server_client_name: parent_turn_context.app_server_client_name.clone(),
        minion_instructions: None,
        user_instructions: None,
        compact_prompt: parent_turn_context.compact_prompt.clone(),
        collaboration_mode: parent_turn_context.collaboration_mode.clone(),
        personality: parent_turn_context.personality,
        approval_policy: parent_turn_context.approval_policy.clone(),
        sandbox_policy: parent_turn_context.sandbox_policy.clone(),
        file_system_sandbox_policy: parent_turn_context.file_system_sandbox_policy.clone(),
        network_sandbox_policy: parent_turn_context.network_sandbox_policy,
        network: parent_turn_context.network.clone(),
        windows_sandbox_level: parent_turn_context.windows_sandbox_level,
        shell_environment_policy: parent_turn_context.shell_environment_policy.clone(),
        cwd: parent_turn_context.cwd.clone(),
        final_output_json_schema: None,
        alcatraz_macos_exe: parent_turn_context.alcatraz_macos_exe.clone(),
        alcatraz_linux_exe: parent_turn_context.alcatraz_linux_exe.clone(),
        alcatraz_freebsd_exe: parent_turn_context.alcatraz_freebsd_exe.clone(),
        tool_call_gate: Arc::new(ReadinessFlag::new()),
        dynamic_tools: parent_turn_context.dynamic_tools.clone(),
        truncation_policy: model_info.truncation_policy.into(),
        turn_metadata_state,
        turn_skills: TurnSkillsContext::new(parent_turn_context.turn_skills.outcome.clone()),
        turn_timing_state: Arc::new(TurnTimingState::default()),
    };

    // Seed the child task with the review prompt as the initial user message.
    let input: Vec<UserInput> = vec![UserInput::Text {
        text: review_prompt,
        // Review prompt is synthesized; no UI element ranges to preserve.
        text_elements: Vec::new(),
    }];
    let tc = Arc::new(review_turn_context);
    tc.turn_metadata_state.spawn_git_enrichment_task();
    // TODO(ccunningham): Review turns currently rely on `spawn_task` for TurnComplete but do not
    // emit a parent TurnStarted. Consider giving review a full parent turn lifecycle
    // (TurnStarted + TurnComplete) for consistency with other standalone tasks.
    sess.spawn_task(tc.clone(), input, ReviewTask::new()).await;

    // Announce entering review mode so UIs can switch modes.
    let review_request = ReviewRequest {
        target: resolved.target,
        user_facing_hint: Some(resolved.user_facing_hint),
    };
    sess.send_event(&tc, EventMsg::EnteredReviewMode(review_request))
        .await;
}

fn skills_to_info(
    skills: &[SkillMetadata],
    disabled_paths: &HashSet<PathBuf>,
) -> Vec<ProtocolSkillMetadata> {
    skills
        .iter()
        .map(|skill| ProtocolSkillMetadata {
            name: skill.name.clone(),
            description: skill.description.clone(),
            short_description: skill.short_description.clone(),
            interface: skill
                .interface
                .clone()
                .map(|interface| ProtocolSkillInterface {
                    display_name: interface.display_name,
                    short_description: interface.short_description,
                    icon_small: interface.icon_small,
                    icon_large: interface.icon_large,
                    brand_color: interface.brand_color,
                    default_prompt: interface.default_prompt,
                }),
            dependencies: skill.dependencies.clone().map(|dependencies| {
                ProtocolSkillDependencies {
                    tools: dependencies
                        .tools
                        .into_iter()
                        .map(|tool| ProtocolSkillToolDependency {
                            r#type: tool.r#type,
                            value: tool.value,
                            description: tool.description,
                            transport: tool.transport,
                            command: tool.command,
                            url: tool.url,
                        })
                        .collect(),
                }
            }),
            path: skill.path_to_skills_md.clone(),
            scope: skill.scope,
            enabled: !disabled_paths.contains(&skill.path_to_skills_md),
        })
        .collect()
}

fn errors_to_info(errors: &[SkillError]) -> Vec<SkillErrorInfo> {
    errors
        .iter()
        .map(|err| SkillErrorInfo {
            path: err.path.clone(),
            message: err.message.clone(),
        })
        .collect()
}

/// Takes a user message as input and runs a loop where, at each sampling request, the model
/// replies with either:
///
/// - requested function calls
/// - an assistant message
///
/// While it is possible for the model to return multiple of these items in a
/// single sampling request, in practice, we generally one item per sampling request:
///
/// - If the model requests a function call, we execute it and send the output
///   back to the model in the next sampling request.
/// - If the model sends only an assistant message, we record it in the
///   conversation history and consider the turn complete.
///
pub(crate) async fn run_turn(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    input: Vec<UserInput>,
    prewarmed_client_session: Option<ModelClientSession>,
    cancellation_token: CancellationToken,
) -> Option<String> {
    if input.is_empty() {
        return None;
    }

    let model_info = turn_context.model_info.clone();
    let auto_compact_limit = model_info.auto_compact_token_limit().unwrap_or(i64::MAX);

    let event = EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: turn_context.sub_id.clone(),
        model_context_window: turn_context.model_context_window(),
        collaboration_mode_kind: turn_context.collaboration_mode.mode,
    });
    sess.send_event(&turn_context, event).await;
    // TODO(ccunningham): Pre-turn compaction runs before context updates and the
    // new user message are recorded. Estimate pending incoming items (context
    // diffs/full reinjection + user input) and trigger compaction preemptively
    // when they would push the thread over the compaction threshold.
    if run_pre_sampling_compact(&sess, &turn_context)
        .await
        .is_err()
    {
        error!("Failed to run pre-sampling compact");
        return None;
    }

    let skills_outcome = Some(turn_context.turn_skills.outcome.as_ref());

    sess.record_context_updates_and_set_reference_context_item(turn_context.as_ref())
        .await;

    let _mcp_tools = if turn_context.apps_enabled() {
        match sess
            .services
            .mcp_connection_manager
            .read()
            .await
            .list_all_tools()
            .or_cancel(&cancellation_token)
            .await
        {
            Ok(mcp_tools) => mcp_tools,
            Err(_) => return None,
        }
    } else {
        HashMap::new()
    };
    let mentioned_skills = skills_outcome.as_ref().map_or_else(Vec::new, |outcome| {
        collect_explicit_skill_mentions(
            &input,
            &outcome.skills,
            &outcome.disabled_paths,
            &HashMap::new(),
        )
    });
    let config = turn_context.config.clone();
    if config
        .features
        .enabled(Feature::SkillEnvVarDependencyPrompt)
    {
        let env_var_dependencies = collect_env_var_dependencies(&mentioned_skills);
        resolve_skill_dependencies_for_turn(&sess, &turn_context, &env_var_dependencies).await;
    }

    maybe_prompt_and_install_mcp_dependencies(
        sess.as_ref(),
        turn_context.as_ref(),
        &cancellation_token,
        &mentioned_skills,
    )
    .await;

    let session_telemetry = turn_context.session_telemetry.clone();
    let process_id = sess.conversation_id.to_string();
    let tracking = build_track_events_context(
        turn_context.model_info.slug.clone(),
        process_id,
        turn_context.sub_id.clone(),
    );
    let SkillInjections {
        items: skill_items,
        warnings: skill_warnings,
    } = build_skill_injections(
        &mentioned_skills,
        Some(&session_telemetry),
        &sess.services.analytics_events_client,
        tracking.clone(),
    )
    .await;

    for message in skill_warnings {
        sess.send_event(&turn_context, EventMsg::Warning(WarningEvent { message }))
            .await;
    }

    let initial_input_for_turn: ResponseInputItem = ResponseInputItem::from(input.clone());
    let response_item: ResponseItem = initial_input_for_turn.clone().into();
    sess.record_user_prompt_and_emit_turn_item(turn_context.as_ref(), &input, response_item)
        .await;
    // Track the previous-turn baseline from the regular user-turn path only so
    // standalone tasks (compact/shell/review/undo) cannot suppress future
    // model injections.
    sess.set_previous_turn_settings(Some(PreviousTurnSettings {
        model: turn_context.model_info.slug.clone(),
    }))
    .await;

    if !skill_items.is_empty() {
        sess.record_conversation_items(&turn_context, &skill_items)
            .await;
    }
    sess.maybe_start_ghost_snapshot(Arc::clone(&turn_context), cancellation_token.child_token())
        .await;
    let mut last_agent_message: Option<String> = None;
    let mut stop_hook_active = false;
    // Although from the perspective of codex.rs, TurnDiffTracker has the lifecycle of a Task which contains
    // many turns, from the perspective of the user, it is a single turn.
    let turn_diff_tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    let mut server_model_warning_emitted_for_turn = false;

    // `ModelClientSession` is turn-scoped and caches WebSocket + sticky routing state, so we reuse
    // one instance across retries within this turn.
    let mut client_session =
        prewarmed_client_session.unwrap_or_else(|| sess.services.model_client.new_session());

    loop {
        if let Some(session_start_source) = sess.take_pending_session_start_source().await {
            let session_start_permission_mode = match turn_context.approval_policy.value() {
                ApprovalPolicy::Headless => "bypassPermissions",
                ApprovalPolicy::Supervised
                | ApprovalPolicy::Interactive
                | ApprovalPolicy::Granular(_) => "default",
            }
            .to_string();
            let session_start_request = chaos_dtrace::SessionStartRequest {
                session_id: sess.conversation_id,
                cwd: turn_context.cwd.clone(),
                transcript_path: None,
                model: turn_context.model_info.slug.clone(),
                permission_mode: session_start_permission_mode,
                source: session_start_source,
            };
            for run in sess.hooks().preview_session_start(&session_start_request) {
                sess.send_event(
                    &turn_context,
                    EventMsg::HookStarted(crate::protocol::HookStartedEvent {
                        turn_id: Some(turn_context.sub_id.clone()),
                        run,
                    }),
                )
                .await;
            }
            let session_start_outcome = sess
                .hooks()
                .run_session_start(session_start_request, Some(turn_context.sub_id.clone()))
                .await;
            for completed in session_start_outcome.hook_events {
                sess.send_event(&turn_context, EventMsg::HookCompleted(completed))
                    .await;
            }
            if session_start_outcome.should_stop {
                break;
            }
            if let Some(additional_context) = session_start_outcome.additional_context {
                let developer_message: ResponseItem =
                    DeveloperInstructions::new(additional_context).into();
                sess.record_conversation_items(
                    &turn_context,
                    std::slice::from_ref(&developer_message),
                )
                .await;
            }
        }

        // Note that pending_input would be something like a message the user
        // submitted through the UI while the model was running. Though the UI
        // may support this, the model might not.
        let pending_response_items = sess
            .get_pending_input()
            .await
            .into_iter()
            .map(ResponseItem::from)
            .collect::<Vec<ResponseItem>>();

        if !pending_response_items.is_empty() {
            for response_item in pending_response_items {
                if let Some(TurnItem::UserMessage(user_message)) = parse_turn_item(&response_item) {
                    // TODO: move pending input to be UserInput only to keep TextElements.
                    sess.record_user_prompt_and_emit_turn_item(
                        turn_context.as_ref(),
                        &user_message.content,
                        response_item,
                    )
                    .await;
                } else {
                    sess.record_conversation_items(
                        &turn_context,
                        std::slice::from_ref(&response_item),
                    )
                    .await;
                }
            }
        }

        // Construct the input that we will send to the model.
        let sampling_request_input: Vec<ResponseItem> = {
            sess.clone_history()
                .await
                .for_prompt(&turn_context.model_info.input_modalities)
        };

        let sampling_request_input_messages = sampling_request_input
            .iter()
            .filter_map(|item| match parse_turn_item(item) {
                Some(TurnItem::UserMessage(user_message)) => Some(user_message),
                _ => None,
            })
            .map(|user_message| user_message.message())
            .collect::<Vec<String>>();
        let turn_metadata_header = turn_context.turn_metadata_state.current_header_value();
        match run_sampling_request(
            Arc::clone(&sess),
            Arc::clone(&turn_context),
            Arc::clone(&turn_diff_tracker),
            &mut client_session,
            turn_metadata_header.as_deref(),
            sampling_request_input,
            skills_outcome,
            &mut server_model_warning_emitted_for_turn,
            cancellation_token.child_token(),
        )
        .await
        {
            Ok(sampling_request_output) => {
                let SamplingRequestResult {
                    needs_follow_up,
                    last_agent_message: sampling_request_last_agent_message,
                } = sampling_request_output;
                let total_usage_tokens = sess.get_total_token_usage().await;
                let token_limit_reached = total_usage_tokens >= auto_compact_limit;

                let estimated_token_count =
                    sess.get_estimated_token_count(turn_context.as_ref()).await;

                trace!(
                    turn_id = %turn_context.sub_id,
                    total_usage_tokens,
                    estimated_token_count = ?estimated_token_count,
                    auto_compact_limit,
                    token_limit_reached,
                    needs_follow_up,
                    "post sampling token usage"
                );

                // as long as compaction works well in getting us way below the token limit, we shouldn't worry about being in an infinite loop.
                if token_limit_reached && needs_follow_up {
                    if run_auto_compact(
                        &sess,
                        &turn_context,
                        InitialContextInjection::BeforeLastUserMessage,
                    )
                    .await
                    .is_err()
                    {
                        return None;
                    }
                    continue;
                }

                if !needs_follow_up {
                    last_agent_message = sampling_request_last_agent_message;
                    let stop_hook_permission_mode = match turn_context.approval_policy.value() {
                        ApprovalPolicy::Headless => "bypassPermissions",
                        ApprovalPolicy::Supervised
                        | ApprovalPolicy::Interactive
                        | ApprovalPolicy::Granular(_) => "default",
                    }
                    .to_string();
                    let stop_request = chaos_dtrace::StopRequest {
                        session_id: sess.conversation_id,
                        turn_id: turn_context.sub_id.clone(),
                        cwd: turn_context.cwd.clone(),
                        transcript_path: None,
                        model: turn_context.model_info.slug.clone(),
                        permission_mode: stop_hook_permission_mode,
                        stop_hook_active,
                        last_assistant_message: last_agent_message.clone(),
                    };
                    for run in sess.hooks().preview_stop(&stop_request) {
                        sess.send_event(
                            &turn_context,
                            EventMsg::HookStarted(crate::protocol::HookStartedEvent {
                                turn_id: Some(turn_context.sub_id.clone()),
                                run,
                            }),
                        )
                        .await;
                    }
                    let stop_outcome = sess.hooks().run_stop(stop_request).await;
                    for completed in stop_outcome.hook_events {
                        sess.send_event(&turn_context, EventMsg::HookCompleted(completed))
                            .await;
                    }
                    if stop_outcome.should_block {
                        if let Some(continuation_prompt) = stop_outcome.continuation_prompt.clone()
                        {
                            let developer_message: ResponseItem =
                                DeveloperInstructions::new(continuation_prompt).into();
                            sess.record_conversation_items(
                                &turn_context,
                                std::slice::from_ref(&developer_message),
                            )
                            .await;
                            stop_hook_active = true;
                            continue;
                        } else {
                            sess.send_event(
                                &turn_context,
                                EventMsg::Warning(WarningEvent {
                                    message: "Stop hook requested continuation without a prompt; ignoring the block.".to_string(),
                                }),
                            )
                            .await;
                        }
                    }
                    if stop_outcome.should_stop {
                        break;
                    }
                    let hook_outcomes = sess
                        .hooks()
                        .dispatch(HookPayload {
                            session_id: sess.conversation_id,
                            cwd: turn_context.cwd.clone(),
                            client: turn_context.app_server_client_name.clone(),
                            triggered_at: Timestamp::now(),
                            hook_event: HookEvent::AfterAgent {
                                event: HookEventAfterAgent {
                                    process_id: sess.conversation_id,
                                    turn_id: turn_context.sub_id.clone(),
                                    input_messages: sampling_request_input_messages,
                                    last_assistant_message: last_agent_message.clone(),
                                },
                            },
                        })
                        .await;

                    let mut abort_message = None;
                    for hook_outcome in hook_outcomes {
                        let hook_name = hook_outcome.hook_name;
                        match hook_outcome.result {
                            HookResult::Success => {}
                            HookResult::FailedContinue(error) => {
                                warn!(
                                    turn_id = %turn_context.sub_id,
                                    hook_name = %hook_name,
                                    error = %error,
                                    "after_agent hook failed; continuing"
                                );
                            }
                            HookResult::FailedAbort(error) => {
                                let message = format!(
                                    "after_agent hook '{hook_name}' failed and aborted turn completion: {error}"
                                );
                                warn!(
                                    turn_id = %turn_context.sub_id,
                                    hook_name = %hook_name,
                                    error = %error,
                                    "after_agent hook failed; aborting operation"
                                );
                                if abort_message.is_none() {
                                    abort_message = Some(message);
                                }
                            }
                        }
                    }
                    if let Some(message) = abort_message {
                        sess.send_event(
                            &turn_context,
                            EventMsg::Error(ErrorEvent {
                                message,
                                codex_error_info: None,
                            }),
                        )
                        .await;
                        return None;
                    }
                    break;
                }
                continue;
            }
            Err(ChaosErr::TurnAborted) => {
                // Aborted turn is reported via a different event.
                break;
            }
            Err(ChaosErr::InvalidImageRequest()) => {
                let mut state = sess.state.lock().await;
                error_or_panic(
                    "Invalid image detected; sanitizing tool output to prevent poisoning",
                );
                if state.history.replace_last_turn_images("Invalid image") {
                    continue;
                }
                let event = EventMsg::Error(ErrorEvent {
                    message: "Invalid image in your last message. Please remove it and try again."
                        .to_string(),
                    codex_error_info: Some(CodexErrorInfo::BadRequest),
                });
                sess.send_event(&turn_context, event).await;
                break;
            }
            Err(e) => {
                info!("Turn error: {e:#}");
                let event = EventMsg::Error(e.to_error_event(/*message_prefix*/ None));
                sess.send_event(&turn_context, event).await;
                // let the user continue the conversation
                break;
            }
        }
    }

    last_agent_message
}

async fn run_pre_sampling_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
) -> ChaosResult<()> {
    let total_usage_tokens_before_compaction = sess.get_total_token_usage().await;
    maybe_run_previous_model_inline_compact(
        sess,
        turn_context,
        total_usage_tokens_before_compaction,
    )
    .await?;
    let total_usage_tokens = sess.get_total_token_usage().await;
    let auto_compact_limit = turn_context
        .model_info
        .auto_compact_token_limit()
        .unwrap_or(i64::MAX);
    // Compact if the total usage tokens are greater than the auto compact limit
    if total_usage_tokens >= auto_compact_limit {
        run_auto_compact(sess, turn_context, InitialContextInjection::DoNotInject).await?;
    }
    Ok(())
}

/// Runs pre-sampling compaction against the previous model when switching to a smaller
/// context-window model.
///
/// Returns `Ok(true)` when compaction ran successfully, `Ok(false)` when compaction was skipped
/// because the model/context-window preconditions were not met, and `Err(_)` only when compaction
/// was attempted and failed.
async fn maybe_run_previous_model_inline_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    total_usage_tokens: i64,
) -> ChaosResult<bool> {
    let Some(previous_turn_settings) = sess.previous_turn_settings().await else {
        return Ok(false);
    };
    let previous_model_turn_context = Arc::new(
        turn_context
            .with_model(previous_turn_settings.model, &sess.services.models_manager)
            .await,
    );

    let Some(old_context_window) = previous_model_turn_context.model_context_window() else {
        return Ok(false);
    };
    let Some(new_context_window) = turn_context.model_context_window() else {
        return Ok(false);
    };
    let new_auto_compact_limit = turn_context
        .model_info
        .auto_compact_token_limit()
        .unwrap_or(i64::MAX);
    let should_run = total_usage_tokens > new_auto_compact_limit
        && previous_model_turn_context.model_info.slug != turn_context.model_info.slug
        && old_context_window > new_context_window;
    if should_run {
        run_auto_compact(
            sess,
            &previous_model_turn_context,
            InitialContextInjection::DoNotInject,
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

async fn run_auto_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    initial_context_injection: InitialContextInjection,
) -> ChaosResult<()> {
    if should_use_remote_compact_task(&turn_context.provider) {
        run_inline_remote_auto_compact_task(
            Arc::clone(sess),
            Arc::clone(turn_context),
            initial_context_injection,
        )
        .await?;
    } else {
        run_inline_auto_compact_task(
            Arc::clone(sess),
            Arc::clone(turn_context),
            initial_context_injection,
        )
        .await?;
    }
    Ok(())
}

fn build_prompt(
    input: Vec<ResponseItem>,
    router: &ToolRouter,
    turn_context: &TurnContext,
    base_instructions: BaseInstructions,
) -> Prompt {
    let deferred_dynamic_tools = turn_context
        .dynamic_tools
        .iter()
        .filter(|tool| tool.defer_loading)
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    let tools = if deferred_dynamic_tools.is_empty() {
        router.model_visible_specs()
    } else {
        router
            .model_visible_specs()
            .into_iter()
            .filter(|spec| !deferred_dynamic_tools.contains(spec.name()))
            .collect()
    };

    Prompt {
        input,
        tools,
        parallel_tool_calls: turn_context.model_info.supports_parallel_tool_calls,
        base_instructions,
        personality: turn_context.personality,
        output_schema: turn_context.final_output_json_schema.clone(),
    }
}
#[allow(clippy::too_many_arguments)]
#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %turn_context.sub_id,
        model = %turn_context.model_info.slug,
        cwd = %turn_context.cwd.display()
    )
)]
async fn run_sampling_request(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    turn_diff_tracker: SharedTurnDiffTracker,
    client_session: &mut ModelClientSession,
    turn_metadata_header: Option<&str>,
    input: Vec<ResponseItem>,
    skills_outcome: Option<&SkillLoadOutcome>,
    server_model_warning_emitted_for_turn: &mut bool,
    cancellation_token: CancellationToken,
) -> ChaosResult<SamplingRequestResult> {
    let router = built_tools(
        sess.as_ref(),
        turn_context.as_ref(),
        &input,
        skills_outcome,
        &cancellation_token,
    )
    .await?;

    let base_instructions = sess.get_base_instructions().await;

    let prompt = build_prompt(
        input,
        router.as_ref(),
        turn_context.as_ref(),
        base_instructions,
    );
    let tool_runtime = ToolCallRuntime::new(
        Arc::clone(&router),
        Arc::clone(&sess),
        Arc::clone(&turn_context),
        Arc::clone(&turn_diff_tracker),
    );
    let mut retries = 0;
    let mut last_server_model: Option<String> = None;
    loop {
        let err = match try_run_sampling_request(
            tool_runtime.clone(),
            Arc::clone(&sess),
            Arc::clone(&turn_context),
            client_session,
            turn_metadata_header,
            Arc::clone(&turn_diff_tracker),
            server_model_warning_emitted_for_turn,
            &mut last_server_model,
            &prompt,
            cancellation_token.child_token(),
        )
        .await
        {
            Ok(output) => {
                return Ok(output);
            }
            Err(ChaosErr::ContextWindowExceeded) => {
                sess.set_total_tokens_full(&turn_context).await;
                return Err(ChaosErr::ContextWindowExceeded);
            }
            Err(ChaosErr::UsageLimitReached(e)) => {
                let rate_limits = e.rate_limits.clone();
                if let Some(rate_limits) = rate_limits {
                    sess.update_rate_limits(&turn_context, *rate_limits).await;
                }
                return Err(ChaosErr::UsageLimitReached(e));
            }
            Err(err) => err,
        };

        if !err.is_retryable() {
            return Err(err);
        }

        // Use the configured provider-specific stream retry budget.
        let max_retries = turn_context.provider.stream_max_retries();
        if retries >= max_retries
            && client_session.try_switch_fallback_transport(
                &turn_context.session_telemetry,
                &turn_context.model_info,
            )
        {
            sess.send_event(
                &turn_context,
                EventMsg::Warning(WarningEvent {
                    message: format!("Falling back from WebSockets to HTTPS transport. {err:#}"),
                }),
            )
            .await;
            retries = 0;
            continue;
        }
        if retries < max_retries {
            retries += 1;
            let delay = match &err {
                ChaosErr::Stream(_, requested_delay) => {
                    requested_delay.unwrap_or_else(|| backoff(retries))
                }
                _ => backoff(retries),
            };
            warn!(
                "stream disconnected - retrying sampling request ({retries}/{max_retries} in {delay:?})...",
            );

            // In release builds, hide the first websocket retry notification to reduce noisy
            // transient reconnect messages. In debug builds, keep full visibility for diagnosis.
            let report_error = retries > 1
                || cfg!(debug_assertions)
                || !sess
                    .services
                    .model_client
                    .responses_websocket_enabled(&turn_context.model_info);
            if report_error {
                // Surface retry information to any UI/front‑end so the
                // user understands what is happening instead of staring
                // at a seemingly frozen screen.
                sess.notify_stream_error(
                    &turn_context,
                    format!("Reconnecting... {retries}/{max_retries}"),
                    err,
                )
                .await;
            }
            tokio::time::sleep(delay).await;
        } else {
            return Err(err);
        }
    }
}

pub(crate) async fn built_tools(
    sess: &Session,
    turn_context: &TurnContext,
    _input: &[ResponseItem],
    _skills_outcome: Option<&SkillLoadOutcome>,
    cancellation_token: &CancellationToken,
) -> ChaosResult<Arc<ToolRouter>> {
    let mcp_connection_manager = sess.services.mcp_connection_manager.read().await;
    let has_mcp_servers = mcp_connection_manager.has_servers();
    let mcp_tools = mcp_connection_manager
        .list_all_tools()
        .or_cancel(cancellation_token)
        .await?;
    drop(mcp_connection_manager);

    // Read static module tools from the catalog.
    let mut catalog_tools = {
        let catalog = sess
            .services
            .catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        catalog
            .tools()
            .iter()
            .filter(|(s, _)| matches!(s, crate::catalog::CatalogSource::Module(_)))
            .map(|(s, t)| {
                let source_name = match s {
                    crate::catalog::CatalogSource::Module(name) => name.clone(),
                    crate::catalog::CatalogSource::Mcp(name) => name.clone(),
                };
                (
                    source_name,
                    chaos_traits::catalog::CatalogTool {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        input_schema: t.input_schema.clone(),
                        annotations: t.annotations.clone(),
                        read_only_hint: t.read_only_hint,
                        supports_parallel_tool_calls: t.supports_parallel_tool_calls,
                    },
                )
            })
            .collect::<Vec<_>>()
    };

    // Add script tools from the hallucinate engine (Lua/WASM user scripts).
    if let Some(ref handle) = sess.services.hallucinate {
        let script_tools = handle.list_tools().await;
        for tool in script_tools {
            catalog_tools.push((
                "hallucinate".to_string(),
                chaos_traits::catalog::CatalogTool {
                    name: tool.name,
                    description: tool.description,
                    input_schema: tool.input_schema,
                    annotations: None,
                    read_only_hint: None,
                    supports_parallel_tool_calls: true,
                },
            ));
        }
    }

    let plan_mode = turn_context.collaboration_mode.mode == ModeKind::Plan;
    Ok(Arc::new(ToolRouter::from_config(
        &turn_context.tools_config,
        ToolRouterParams {
            mcp_tools: has_mcp_servers.then(|| {
                mcp_tools
                    .into_iter()
                    .map(|(name, tool)| (name, tool.tool))
                    .collect()
            }),
            app_tools: None,
            dynamic_tools: turn_context.dynamic_tools.as_slice(),
            catalog_tools,
            hallucinate: sess.services.hallucinate.clone(),
            plan_mode,
        },
    )))
}

#[derive(Debug)]
struct SamplingRequestResult {
    needs_follow_up: bool,
    last_agent_message: Option<String>,
}

/// Ephemeral per-response state for streaming a single proposed plan.
/// This is intentionally not persisted or stored in session/state since it
/// only exists while a response is actively streaming. The final plan text
/// is extracted from the completed assistant message.
/// Tracks a single proposed plan item across a streaming response.
struct ProposedPlanItemState {
    item_id: String,
    started: bool,
    completed: bool,
}

/// Aggregated state used only while streaming a plan-mode response.
/// Includes per-item parsers, deferred agent message bookkeeping, and the plan item lifecycle.
struct PlanModeStreamState {
    /// Agent message items started by the model but deferred until we see non-plan text.
    pending_agent_message_items: HashMap<String, TurnItem>,
    /// Agent message items whose start notification has been emitted.
    started_agent_message_items: HashSet<String>,
    /// Leading whitespace buffered until we see non-whitespace text for an item.
    leading_whitespace_by_item: HashMap<String, String>,
    /// Tracks plan item lifecycle while streaming plan output.
    plan_item_state: ProposedPlanItemState,
}

impl PlanModeStreamState {
    fn new(turn_id: &str) -> Self {
        Self {
            pending_agent_message_items: HashMap::new(),
            started_agent_message_items: HashSet::new(),
            leading_whitespace_by_item: HashMap::new(),
            plan_item_state: ProposedPlanItemState::new(turn_id),
        }
    }
}

#[derive(Debug, Default)]
struct AssistantMessageStreamParsers {
    plan_mode: bool,
    parsers_by_item: HashMap<String, AssistantTextStreamParser>,
}

type ParsedAssistantTextDelta = AssistantTextChunk;

impl AssistantMessageStreamParsers {
    fn new(plan_mode: bool) -> Self {
        Self {
            plan_mode,
            parsers_by_item: HashMap::new(),
        }
    }

    fn parser_mut(&mut self, item_id: &str) -> &mut AssistantTextStreamParser {
        let plan_mode = self.plan_mode;
        self.parsers_by_item
            .entry(item_id.to_string())
            .or_insert_with(|| AssistantTextStreamParser::new(plan_mode))
    }

    fn seed_item_text(&mut self, item_id: &str, text: &str) -> ParsedAssistantTextDelta {
        if text.is_empty() {
            return ParsedAssistantTextDelta::default();
        }
        self.parser_mut(item_id).push_str(text)
    }

    fn parse_delta(&mut self, item_id: &str, delta: &str) -> ParsedAssistantTextDelta {
        self.parser_mut(item_id).push_str(delta)
    }

    fn finish_item(&mut self, item_id: &str) -> ParsedAssistantTextDelta {
        let Some(mut parser) = self.parsers_by_item.remove(item_id) else {
            return ParsedAssistantTextDelta::default();
        };
        parser.finish()
    }

    fn drain_finished(&mut self) -> Vec<(String, ParsedAssistantTextDelta)> {
        let parsers_by_item = std::mem::take(&mut self.parsers_by_item);
        parsers_by_item
            .into_iter()
            .map(|(item_id, mut parser)| (item_id, parser.finish()))
            .collect()
    }
}

impl ProposedPlanItemState {
    fn new(turn_id: &str) -> Self {
        Self {
            item_id: format!("{turn_id}-plan"),
            started: false,
            completed: false,
        }
    }

    async fn start(&mut self, sess: &Session, turn_context: &TurnContext) {
        if self.started || self.completed {
            return;
        }
        self.started = true;
        let item = TurnItem::Plan(PlanItem {
            id: self.item_id.clone(),
            text: String::new(),
        });
        sess.emit_turn_item_started(turn_context, &item).await;
    }

    async fn push_delta(&mut self, sess: &Session, turn_context: &TurnContext, delta: &str) {
        if self.completed {
            return;
        }
        if delta.is_empty() {
            return;
        }
        let event = PlanDeltaEvent {
            process_id: sess.conversation_id.to_string(),
            turn_id: turn_context.sub_id.clone(),
            item_id: self.item_id.clone(),
            delta: delta.to_string(),
        };
        sess.send_event(turn_context, EventMsg::PlanDelta(event))
            .await;
    }

    async fn complete_with_text(
        &mut self,
        sess: &Session,
        turn_context: &TurnContext,
        text: String,
    ) {
        if self.completed || !self.started {
            return;
        }
        self.completed = true;
        let item = TurnItem::Plan(PlanItem {
            id: self.item_id.clone(),
            text,
        });
        sess.emit_turn_item_completed(turn_context, item).await;
    }
}

/// In plan mode we defer agent message starts until the parser emits non-plan
/// text. The parser buffers each line until it can rule out a tag prefix, so
/// plan-only outputs never show up as empty assistant messages.
async fn maybe_emit_pending_agent_message_start(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item_id: &str,
) {
    if state.started_agent_message_items.contains(item_id) {
        return;
    }
    if let Some(item) = state.pending_agent_message_items.remove(item_id) {
        sess.emit_turn_item_started(turn_context, &item).await;
        state
            .started_agent_message_items
            .insert(item_id.to_string());
    }
}

/// Agent messages are text-only today; concatenate all text entries.
fn agent_message_text(item: &chaos_ipc::items::AgentMessageItem) -> String {
    item.content
        .iter()
        .map(|entry| match entry {
            chaos_ipc::items::AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect()
}

/// Split the stream into normal assistant text vs. proposed plan content.
/// Normal text becomes AgentMessage deltas; plan content becomes PlanDelta +
/// TurnItem::Plan.
async fn handle_plan_segments(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item_id: &str,
    segments: Vec<ProposedPlanSegment>,
) {
    for segment in segments {
        match segment {
            ProposedPlanSegment::Normal(delta) => {
                if delta.is_empty() {
                    continue;
                }
                let has_non_whitespace = delta.chars().any(|ch| !ch.is_whitespace());
                if !has_non_whitespace && !state.started_agent_message_items.contains(item_id) {
                    let entry = state
                        .leading_whitespace_by_item
                        .entry(item_id.to_string())
                        .or_default();
                    entry.push_str(&delta);
                    continue;
                }
                let delta = if !state.started_agent_message_items.contains(item_id) {
                    if let Some(prefix) = state.leading_whitespace_by_item.remove(item_id) {
                        format!("{prefix}{delta}")
                    } else {
                        delta
                    }
                } else {
                    delta
                };
                maybe_emit_pending_agent_message_start(sess, turn_context, state, item_id).await;

                let event = AgentMessageContentDeltaEvent {
                    process_id: sess.conversation_id.to_string(),
                    turn_id: turn_context.sub_id.clone(),
                    item_id: item_id.to_string(),
                    delta,
                };
                sess.send_event(turn_context, EventMsg::AgentMessageContentDelta(event))
                    .await;
            }
            ProposedPlanSegment::ProposedPlanStart => {
                if !state.plan_item_state.completed {
                    state.plan_item_state.start(sess, turn_context).await;
                }
            }
            ProposedPlanSegment::ProposedPlanDelta(delta) => {
                if !state.plan_item_state.completed {
                    if !state.plan_item_state.started {
                        state.plan_item_state.start(sess, turn_context).await;
                    }
                    state
                        .plan_item_state
                        .push_delta(sess, turn_context, &delta)
                        .await;
                }
            }
            ProposedPlanSegment::ProposedPlanEnd => {}
        }
    }
}

async fn emit_streamed_assistant_text_delta(
    sess: &Session,
    turn_context: &TurnContext,
    plan_mode_state: Option<&mut PlanModeStreamState>,
    item_id: &str,
    parsed: ParsedAssistantTextDelta,
) {
    if parsed.is_empty() {
        return;
    }
    if !parsed.citations.is_empty() {
        // Citation extraction is intentionally local for now; we strip citations from display text
        // but do not yet surface them in protocol events.
        let _citations = parsed.citations;
    }
    if let Some(state) = plan_mode_state {
        if !parsed.plan_segments.is_empty() {
            handle_plan_segments(sess, turn_context, state, item_id, parsed.plan_segments).await;
        }
        return;
    }
    if parsed.visible_text.is_empty() {
        return;
    }
    let event = AgentMessageContentDeltaEvent {
        process_id: sess.conversation_id.to_string(),
        turn_id: turn_context.sub_id.clone(),
        item_id: item_id.to_string(),
        delta: parsed.visible_text,
    };
    sess.send_event(turn_context, EventMsg::AgentMessageContentDelta(event))
        .await;
}

/// Flush buffered assistant text parser state when an assistant message item ends.
async fn flush_assistant_text_segments_for_item(
    sess: &Session,
    turn_context: &TurnContext,
    plan_mode_state: Option<&mut PlanModeStreamState>,
    parsers: &mut AssistantMessageStreamParsers,
    item_id: &str,
) {
    let parsed = parsers.finish_item(item_id);
    emit_streamed_assistant_text_delta(sess, turn_context, plan_mode_state, item_id, parsed).await;
}

/// Flush any remaining buffered assistant text parser state at response completion.
async fn flush_assistant_text_segments_all(
    sess: &Session,
    turn_context: &TurnContext,
    mut plan_mode_state: Option<&mut PlanModeStreamState>,
    parsers: &mut AssistantMessageStreamParsers,
) {
    for (item_id, parsed) in parsers.drain_finished() {
        emit_streamed_assistant_text_delta(
            sess,
            turn_context,
            plan_mode_state.as_deref_mut(),
            &item_id,
            parsed,
        )
        .await;
    }
}

/// Emit completion for plan items by parsing the finalized assistant message.
async fn maybe_complete_plan_item_from_message(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item: &ResponseItem,
) {
    if let ResponseItem::Message { role, content, .. } = item
        && role == "assistant"
    {
        let mut text = String::new();
        for entry in content {
            if let ContentItem::OutputText { text: chunk } = entry {
                text.push_str(chunk);
            }
        }
        if let Some(plan_text) = extract_proposed_plan_text(&text) {
            let (plan_text, _citations) = strip_citations(&plan_text);
            if !state.plan_item_state.started {
                state.plan_item_state.start(sess, turn_context).await;
            }
            state
                .plan_item_state
                .complete_with_text(sess, turn_context, plan_text)
                .await;
        }
    }
}

/// Emit a completed agent message in plan mode, respecting deferred starts.
async fn emit_agent_message_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    agent_message: chaos_ipc::items::AgentMessageItem,
    state: &mut PlanModeStreamState,
) {
    let agent_message_id = agent_message.id.clone();
    let text = agent_message_text(&agent_message);
    if text.trim().is_empty() {
        state.pending_agent_message_items.remove(&agent_message_id);
        state.started_agent_message_items.remove(&agent_message_id);
        return;
    }

    maybe_emit_pending_agent_message_start(sess, turn_context, state, &agent_message_id).await;

    if !state
        .started_agent_message_items
        .contains(&agent_message_id)
    {
        let start_item = state
            .pending_agent_message_items
            .remove(&agent_message_id)
            .unwrap_or_else(|| {
                TurnItem::AgentMessage(chaos_ipc::items::AgentMessageItem {
                    id: agent_message_id.clone(),
                    content: Vec::new(),
                    phase: None,
                })
            });
        sess.emit_turn_item_started(turn_context, &start_item).await;
        state
            .started_agent_message_items
            .insert(agent_message_id.clone());
    }

    sess.emit_turn_item_completed(turn_context, TurnItem::AgentMessage(agent_message))
        .await;
    state.started_agent_message_items.remove(&agent_message_id);
}

/// Emit completion for a plan-mode turn item, handling agent messages specially.
async fn emit_turn_item_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    turn_item: TurnItem,
    previously_active_item: Option<&TurnItem>,
    state: &mut PlanModeStreamState,
) {
    match turn_item {
        TurnItem::AgentMessage(agent_message) => {
            emit_agent_message_in_plan_mode(sess, turn_context, agent_message, state).await;
        }
        _ => {
            if previously_active_item.is_none() {
                sess.emit_turn_item_started(turn_context, &turn_item).await;
            }
            sess.emit_turn_item_completed(turn_context, turn_item).await;
        }
    }
}

/// Handle a completed assistant response item in plan mode, returning true if handled.
async fn handle_assistant_item_done_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    item: &ResponseItem,
    state: &mut PlanModeStreamState,
    previously_active_item: Option<&TurnItem>,
    last_agent_message: &mut Option<String>,
) -> bool {
    if let ResponseItem::Message { role, .. } = item
        && role == "assistant"
    {
        maybe_complete_plan_item_from_message(sess, turn_context, state, item).await;

        if let Some(turn_item) =
            handle_non_tool_response_item(sess, turn_context, item, /*plan_mode*/ true).await
        {
            emit_turn_item_in_plan_mode(
                sess,
                turn_context,
                turn_item,
                previously_active_item,
                state,
            )
            .await;
        }

        record_completed_response_item(sess, turn_context, item).await;
        if let Some(agent_message) = last_assistant_message_from_item(item, /*plan_mode*/ true) {
            *last_agent_message = Some(agent_message);
        }
        return true;
    }
    false
}

async fn drain_in_flight(
    in_flight: &mut FuturesOrdered<BoxFuture<'static, ChaosResult<ResponseInputItem>>>,
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
) -> ChaosResult<()> {
    while let Some(res) = in_flight.next().await {
        match res {
            Ok(response_input) => {
                sess.record_conversation_items(&turn_context, &[response_input.into()])
                    .await;
            }
            Err(err) => {
                error_or_panic(format!("in-flight tool future failed during drain: {err}"));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %turn_context.sub_id,
        model = %turn_context.model_info.slug
    )
)]
async fn try_run_sampling_request(
    tool_runtime: ToolCallRuntime,
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    client_session: &mut ModelClientSession,
    turn_metadata_header: Option<&str>,
    turn_diff_tracker: SharedTurnDiffTracker,
    server_model_warning_emitted_for_turn: &mut bool,
    last_server_model: &mut Option<String>,
    prompt: &Prompt,
    cancellation_token: CancellationToken,
) -> ChaosResult<SamplingRequestResult> {
    feedback_tags!(
        model = turn_context.model_info.slug.clone(),
        approval_policy = turn_context.approval_policy.value(),
        sandbox_policy = turn_context.sandbox_policy.get(),
        effort = turn_context.reasoning_effort,
        auth_mode = sess.services.auth_manager.auth_mode(),
        features = sess.features.enabled_features(),
    );
    let mut stream = client_session
        .stream(
            prompt,
            &turn_context.model_info,
            &turn_context.session_telemetry,
            turn_context.reasoning_effort,
            turn_context.reasoning_summary,
            turn_context.config.service_tier,
            turn_metadata_header,
        )
        .instrument(trace_span!("stream_request"))
        .or_cancel(&cancellation_token)
        .await??;
    let mut in_flight: FuturesOrdered<BoxFuture<'static, ChaosResult<ResponseInputItem>>> =
        FuturesOrdered::new();
    let mut needs_follow_up = false;
    let mut last_agent_message: Option<String> = None;
    let mut active_item: Option<TurnItem> = None;
    let mut should_emit_turn_diff = false;
    let plan_mode = turn_context.collaboration_mode.mode == ModeKind::Plan;
    let mut assistant_message_stream_parsers = AssistantMessageStreamParsers::new(plan_mode);
    let mut plan_mode_state = plan_mode.then(|| PlanModeStreamState::new(&turn_context.sub_id));
    let receiving_span = trace_span!("receiving_stream");
    let outcome: ChaosResult<SamplingRequestResult> = loop {
        let handle_responses = trace_span!(
            parent: &receiving_span,
            "handle_responses",
            otel.name = field::Empty,
            tool_name = field::Empty,
            from = field::Empty,
        );

        let event = match stream
            .next()
            .instrument(trace_span!(parent: &handle_responses, "receiving"))
            .or_cancel(&cancellation_token)
            .await
        {
            Ok(event) => event,
            Err(chaos_epoll::CancelErr::Cancelled) => break Err(ChaosErr::TurnAborted),
        };

        let event = match event {
            Some(res) => res?,
            None => {
                break Err(ChaosErr::Stream(
                    "stream closed before response.completed".into(),
                    None,
                ));
            }
        };

        sess.services
            .session_telemetry
            .record_responses(&handle_responses, &event);
        record_turn_ttft_metric(&turn_context, &event).await;

        match event {
            ResponseEvent::Created => {}
            ResponseEvent::OutputItemDone(item) => {
                let previously_active_item = active_item.take();
                if let Some(previous) = previously_active_item.as_ref()
                    && matches!(previous, TurnItem::AgentMessage(_))
                {
                    let item_id = previous.id();
                    flush_assistant_text_segments_for_item(
                        &sess,
                        &turn_context,
                        plan_mode_state.as_mut(),
                        &mut assistant_message_stream_parsers,
                        &item_id,
                    )
                    .await;
                }
                if let Some(state) = plan_mode_state.as_mut()
                    && handle_assistant_item_done_in_plan_mode(
                        &sess,
                        &turn_context,
                        &item,
                        state,
                        previously_active_item.as_ref(),
                        &mut last_agent_message,
                    )
                    .await
                {
                    continue;
                }

                let mut ctx = HandleOutputCtx {
                    sess: sess.clone(),
                    turn_context: turn_context.clone(),
                    tool_runtime: tool_runtime.clone(),
                    cancellation_token: cancellation_token.child_token(),
                };

                let output_result = handle_output_item_done(&mut ctx, item, previously_active_item)
                    .instrument(handle_responses)
                    .await?;
                if let Some(tool_future) = output_result.tool_future {
                    in_flight.push_back(tool_future);
                }
                if let Some(agent_message) = output_result.last_agent_message {
                    last_agent_message = Some(agent_message);
                }
                needs_follow_up |= output_result.needs_follow_up;
            }
            ResponseEvent::OutputItemAdded(item) => {
                if let Some(turn_item) = handle_non_tool_response_item(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &item,
                    plan_mode,
                )
                .await
                {
                    let mut turn_item = turn_item;
                    let mut seeded_parsed: Option<ParsedAssistantTextDelta> = None;
                    let mut seeded_item_id: Option<String> = None;
                    if matches!(turn_item, TurnItem::AgentMessage(_))
                        && let Some(raw_text) = raw_assistant_output_text_from_item(&item)
                    {
                        let item_id = turn_item.id();
                        let mut seeded =
                            assistant_message_stream_parsers.seed_item_text(&item_id, &raw_text);
                        if let TurnItem::AgentMessage(agent_message) = &mut turn_item {
                            agent_message.content =
                                vec![chaos_ipc::items::AgentMessageContent::Text {
                                    text: if plan_mode {
                                        String::new()
                                    } else {
                                        std::mem::take(&mut seeded.visible_text)
                                    },
                                }];
                        }
                        seeded_parsed = plan_mode.then_some(seeded);
                        seeded_item_id = Some(item_id);
                    }
                    if let Some(state) = plan_mode_state.as_mut()
                        && matches!(turn_item, TurnItem::AgentMessage(_))
                    {
                        let item_id = turn_item.id();
                        state
                            .pending_agent_message_items
                            .insert(item_id, turn_item.clone());
                    } else {
                        sess.emit_turn_item_started(&turn_context, &turn_item).await;
                    }
                    if let (Some(state), Some(item_id), Some(parsed)) = (
                        plan_mode_state.as_mut(),
                        seeded_item_id.as_deref(),
                        seeded_parsed,
                    ) {
                        emit_streamed_assistant_text_delta(
                            &sess,
                            &turn_context,
                            Some(state),
                            item_id,
                            parsed,
                        )
                        .await;
                    }
                    active_item = Some(turn_item);
                }
            }
            ResponseEvent::ServerModel(server_model) => {
                *last_server_model = Some(server_model.clone());
                if !*server_model_warning_emitted_for_turn
                    && sess
                        .maybe_warn_on_server_model_mismatch(&turn_context, server_model)
                        .await
                {
                    *server_model_warning_emitted_for_turn = true;
                }
            }
            ResponseEvent::ServerReasoningIncluded(included) => {
                sess.set_server_reasoning_included(included).await;
            }
            ResponseEvent::RateLimits(snapshot) => {
                // Update internal state with latest rate limits, but defer sending until
                // token usage is available to avoid duplicate TokenCount events.
                sess.update_rate_limits(&turn_context, snapshot).await;
            }
            ResponseEvent::ModelsEtag(etag) => {
                // Update internal state with latest models etag
                sess.services.models_manager.refresh_if_new_etag(etag).await;
            }
            ResponseEvent::Completed {
                response_id,
                token_usage,
            } => {
                if let Some(usage) = &token_usage {
                    let model_name = last_server_model
                        .as_deref()
                        .unwrap_or(turn_context.model_info.slug.as_str());
                    tracing::info!(
                        provider = turn_context.provider.name.as_str(),
                        model = model_name,
                        response_id = %response_id,
                        input_tokens = usage.input_tokens,
                        cached_input_tokens = usage.cached_input_tokens,
                        output_tokens = usage.output_tokens,
                        reasoning_output_tokens = usage.reasoning_output_tokens,
                        total_tokens = usage.total_tokens,
                        "ration: turn completed",
                    );
                }
                flush_assistant_text_segments_all(
                    &sess,
                    &turn_context,
                    plan_mode_state.as_mut(),
                    &mut assistant_message_stream_parsers,
                )
                .await;
                sess.update_token_usage_info(&turn_context, token_usage.as_ref())
                    .await;
                should_emit_turn_diff = true;

                // Use the phase-aware check: pending input that was
                // deferred to the next turn does not extend the current
                // turn even if it exists in the mailbox.
                needs_follow_up |= sess.has_deliverable_input().await;

                break Ok(SamplingRequestResult {
                    needs_follow_up,
                    last_agent_message,
                });
            }
            ResponseEvent::OutputTextDelta(delta) => {
                // In review child threads, suppress assistant text deltas; the
                // UI will show a selection popup from the final ReviewOutput.
                if let Some(active) = active_item.as_ref() {
                    let item_id = active.id();
                    if matches!(active, TurnItem::AgentMessage(_)) {
                        let parsed = assistant_message_stream_parsers.parse_delta(&item_id, &delta);
                        emit_streamed_assistant_text_delta(
                            &sess,
                            &turn_context,
                            plan_mode_state.as_mut(),
                            &item_id,
                            parsed,
                        )
                        .await;
                    } else {
                        let event = AgentMessageContentDeltaEvent {
                            process_id: sess.conversation_id.to_string(),
                            turn_id: turn_context.sub_id.clone(),
                            item_id,
                            delta,
                        };
                        sess.send_event(&turn_context, EventMsg::AgentMessageContentDelta(event))
                            .await;
                    }
                } else {
                    error_or_panic("OutputTextDelta without active item".to_string());
                }
            }
            ResponseEvent::ReasoningSummaryDelta {
                delta,
                summary_index,
            } => {
                if let Some(active) = active_item.as_ref() {
                    let event = ReasoningContentDeltaEvent {
                        process_id: sess.conversation_id.to_string(),
                        turn_id: turn_context.sub_id.clone(),
                        item_id: active.id(),
                        delta,
                        summary_index,
                    };
                    sess.send_event(&turn_context, EventMsg::ReasoningContentDelta(event))
                        .await;
                } else {
                    error_or_panic("ReasoningSummaryDelta without active item".to_string());
                }
            }
            ResponseEvent::ReasoningSummaryPartAdded { summary_index } => {
                if let Some(active) = active_item.as_ref() {
                    let event =
                        EventMsg::AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent {
                            item_id: active.id(),
                            summary_index,
                        });
                    sess.send_event(&turn_context, event).await;
                } else {
                    error_or_panic("ReasoningSummaryPartAdded without active item".to_string());
                }
            }
            ResponseEvent::ReasoningContentDelta {
                delta,
                content_index,
            } => {
                if let Some(active) = active_item.as_ref() {
                    let event = ReasoningRawContentDeltaEvent {
                        process_id: sess.conversation_id.to_string(),
                        turn_id: turn_context.sub_id.clone(),
                        item_id: active.id(),
                        delta,
                        content_index,
                    };
                    sess.send_event(&turn_context, EventMsg::ReasoningRawContentDelta(event))
                        .await;
                } else {
                    error_or_panic("ReasoningRawContentDelta without active item".to_string());
                }
            }
        }
    };

    flush_assistant_text_segments_all(
        &sess,
        &turn_context,
        plan_mode_state.as_mut(),
        &mut assistant_message_stream_parsers,
    )
    .await;

    drain_in_flight(&mut in_flight, sess.clone(), turn_context.clone()).await?;

    if cancellation_token.is_cancelled() {
        return Err(ChaosErr::TurnAborted);
    }

    if should_emit_turn_diff {
        let unified_diff = {
            let mut tracker = turn_diff_tracker.lock().await;
            tracker.get_unified_diff()
        };
        if let Ok(Some(unified_diff)) = unified_diff {
            let msg = EventMsg::TurnDiff(TurnDiffEvent { unified_diff });
            sess.clone().send_event(&turn_context, msg).await;
        }
    }

    outcome
}

pub(super) fn get_last_assistant_message_from_turn(responses: &[ResponseItem]) -> Option<String> {
    for item in responses.iter().rev() {
        if let Some(message) = last_assistant_message_from_item(item, /*plan_mode*/ false) {
            return Some(message);
        }
    }
    None
}

#[cfg(test)]
#[path = "chaos_tests.rs"]
mod tests;
#[cfg(test)]
pub(crate) use tests::make_session_and_context;
#[cfg(test)]
pub(crate) use tests::make_session_and_context_with_rx;
#[cfg(test)]
pub(crate) use tests::make_session_configuration_for_tests;
