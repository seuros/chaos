use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use async_channel::Sender;
use chaos_dtrace::Hooks;
use chaos_dtrace::HooksConfig;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::CollaborationMode;
use chaos_ipc::dynamic_tools::DynamicToolResponse;
use chaos_ipc::items::TurnItem;
use chaos_ipc::items::UserMessageItem;
use chaos_ipc::models::BaseInstructions;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseInputItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::ChaosErrorInfo;
use chaos_ipc::protocol::CompactedItem;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::InitialHistory;
use chaos_ipc::protocol::ItemCompletedEvent;
use chaos_ipc::protocol::ItemStartedEvent;
use chaos_ipc::protocol::McpServerRefreshConfig;
use chaos_ipc::protocol::RawResponseItemEvent;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::TokenCountEvent;
use chaos_ipc::protocol::TokenUsage;
use chaos_ipc::protocol::TokenUsageInfo;
use chaos_ipc::protocol::TurnAbortReason;
use chaos_ipc::protocol::TurnContextItem;
use chaos_ipc::protocol::WarningEvent;
use chaos_ipc::user_input::UserInput;
use chaos_mcp_runtime::manager::McpConnectionManager;
use chaos_pf::NetworkProxyAuditMetadata;
use chaos_ready::Readiness;
use chaos_syslog::SessionTelemetry;
use chaos_syslog::TelemetryAuthMode;
use chaos_syslog::metrics::names::THREAD_STARTED_METRIC;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::info_span;
use tracing::instrument;
use tracing::warn;

use crate::AuthManager;
use crate::ChaosAuth;
use crate::SandboxState;
use crate::config::Config;
use crate::config::ManagedFeatures;
use crate::config::StartedNetworkProxy;
use crate::config::resolve_web_search_mode_for_turn;
use crate::context_manager::ContextManager;
use crate::context_manager::TotalTokenUsageBreakdown;
use crate::exec_policy::ExecPolicyManager;
use crate::features::Feature;
use crate::file_watcher::FileWatcher;
use crate::file_watcher::FileWatcherEvent;
use crate::git_info::get_git_repo_root;
use crate::mcp::McpManager;
use crate::mcp::auth::compute_auth_statuses;
use crate::minions::AgentControl;
use crate::minions::AgentStatus;
use crate::minions::agent_status_from_event;
use crate::models_manager::manager::ModelsManager;
use crate::parse_turn_item;
use crate::rollout::RolloutRecorder;
use crate::rollout::RolloutRecorderParams;
use crate::rollout::policy::EventPersistenceMode;
use crate::rollout::process_names;
use crate::runtime_db;
use crate::shell;
use crate::shell_snapshot::ShellSnapshot;
use crate::skills::SkillsManager;
use crate::state::ActiveTurn;
use crate::state::SessionServices;
use crate::state::SessionState;
use crate::tasks::GhostSnapshotTask;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskContext;
use crate::tools::network_approval::NetworkApprovalService;
use crate::tools::network_approval::build_blocked_request_observer;
use crate::tools::network_approval::build_network_policy_decider;
use crate::tools::sandboxing::ApprovalStore;
use crate::turn_timing::record_turn_ttfm_metric;

use super::INITIAL_SUBMIT_ID;
use super::PreviousTurnSettings;
use super::SessionConfiguration;
use super::SessionSettingsUpdate;
use super::SteerInputError;
use super::TurnContext;
use super::turn_context::make_turn_context;

/// Context for an initialized model agent
///
/// A session has at most 1 running task at a time, and can be interrupted by
/// user input.
pub(crate) struct Session {
    pub(crate) conversation_id: ProcessId,
    pub(crate) tx_event: Sender<Event>,
    pub(super) agent_status: watch::Sender<AgentStatus>,
    pub(super) out_of_band_elicitation_paused: watch::Sender<bool>,
    pub(crate) state: Mutex<SessionState>,
    /// The set of enabled features should be invariant for the lifetime of the
    /// session.
    pub(crate) features: ManagedFeatures,
    pub(crate) pending_mcp_server_refresh_config: Mutex<Option<McpServerRefreshConfig>>,
    pub(crate) active_turn: Mutex<Option<ActiveTurn>>,

    pub(crate) services: SessionServices,
    pub(super) next_internal_sub_id: AtomicU64,
}

impl Session {
    /// Builds the `x-chaos-beta-features` header value for this session.
    ///
    /// With stages removed, there are no experimental-menu features to advertise.
    /// Kept as a named function so callers do not need to change.
    pub(crate) fn build_model_client_beta_features_header(_config: &Config) -> Option<String> {
        None
    }

    pub(crate) async fn start_managed_network_proxy(
        spec: &crate::config::NetworkProxySpec,
        exec_policy: &chaos_selinux::Policy,
        sandbox_policy: &crate::protocol::SandboxPolicy,
        network_policy_decider: Option<Arc<dyn chaos_pf::NetworkPolicyDecider>>,
        blocked_request_observer: Option<Arc<dyn chaos_pf::BlockedRequestObserver>>,
        managed_network_requirements_enabled: bool,
        audit_metadata: NetworkProxyAuditMetadata,
    ) -> anyhow::Result<(
        StartedNetworkProxy,
        crate::protocol::SessionNetworkProxyRuntime,
    )> {
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
            crate::protocol::SessionNetworkProxyRuntime {
                http_addr: proxy.http_addr().to_string(),
                socks_addr: proxy.socks_addr().to_string(),
            }
        };
        Ok((network_proxy, session_network_proxy))
    }

    /// Don't expand the number of mutated arguments on config. We are in the
    /// process of getting rid of it.
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

    #[instrument(name = "session_init", level = "info", skip_all)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn new(
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
                let runtime_db_ctx = runtime_db::init(&config).await;
                let rollout_recorder = RolloutRecorder::new(
                    &config,
                    rollout_params,
                    runtime_db_ctx.clone(),
                    state_builder.clone(),
                )
                .await?;
                Ok((Some(rollout_recorder), runtime_db_ctx))
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
                msg: EventMsg::DeprecationNotice(crate::protocol::DeprecationNoticeEvent {
                    summary: usage.summary.clone(),
                    details: usage.details.clone(),
                }),
            });
        }
        if crate::config::uses_deprecated_instructions_file(&config.config_layer_stack) {
            post_session_configured_events.push(Event {
                id: INITIAL_SUBMIT_ID.to_owned(),
                msg: EventMsg::DeprecationNotice(crate::protocol::DeprecationNoticeEvent {
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
        // Self-authenticated providers (xAI, DeepSeek, Mistral, etc.) use
        // `env_key` and never touch the cached ChatGPT login. Reporting
        // the global auth mode on those sessions leaks ChatGPT account
        // identity into telemetry for requests that were actually signed
        // with a third-party API key. Gate on `requires_openai_auth`:
        // only surface the cached auth when the active provider is the
        // one that owns it.
        let provider_owns_auth = session_configuration.provider.requires_openai_auth;
        let auth_mode = if provider_owns_auth {
            auth.map(ChaosAuth::auth_mode).map(TelemetryAuthMode::from)
        } else {
            Some(TelemetryAuthMode::ApiKey)
        };
        let originator = crate::default_client::originator().value.as_str();
        let terminal_type = crate::terminal::user_agent();
        let session_model = session_configuration.collaboration_mode.model().to_string();
        let mut session_telemetry = SessionTelemetry::new(
            conversation_id,
            session_model.as_str(),
            session_model.as_str(),
            auth_mode,
            originator,
            config.otel.log_user_prompt,
            terminal_type.as_str(),
            session_configuration.session_source.clone(),
        );
        if let Some(service_name) = session_configuration.metrics_service_name.as_deref() {
            session_telemetry = session_telemetry.with_metrics_service_name(service_name);
        }
        let network_proxy_audit_metadata = NetworkProxyAuditMetadata {
            conversation_id: Some(conversation_id.to_string()),
            app_version: Some(chaos_ipc::product::CHAOS_VERSION.to_string()),
            auth_mode: auth_mode.map(|mode| mode.to_string()),
            originator: Some(originator.to_string()),
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
                .unwrap_or(chaos_ipc::config_types::ReasoningSummary::Auto),
            config.model_context_window,
            config.model_auto_compact_token_limit,
            config.permissions.approval_policy.value(),
            config.permissions.sandbox_policy.get().clone(),
            mcp_servers.keys().map(String::as_str).collect(),
            config.active_profile.clone(),
        );

        let mut default_shell = shell::default_user_shell();
        // Create the mutable state for the Session.
        let shell_snapshot_tx =
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
            unified_exec_manager: crate::unified_exec::UnifiedExecProcessManager::new(
                config.background_terminal_max_timeout,
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
            skills_manager,
            mcp_manager: Arc::clone(&mcp_manager),
            file_watcher,
            agent_control,
            network_proxy,
            network_approval: Arc::clone(&network_approval),
            runtime_db: state_db_ctx.clone(),
            hallucinate,
            model_client: crate::client::ModelClient::new(
                Some(Arc::clone(&auth_manager)),
                conversation_id,
                session_configuration.provider.clone(),
                session_configuration.session_source.clone(),
                session_configuration.approval_policy.value(),
                config.model_verbosity,
                true, // request compression always enabled
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
        let initial_messages =
            super::submission_loop::initial_replay_event_msgs(&initial_history, conversation_id);
        let events = std::iter::once(Event {
            id: INITIAL_SUBMIT_ID.to_owned(),
            msg: EventMsg::SessionConfigured(crate::protocol::SessionConfiguredEvent {
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

    pub(crate) fn runtime_db(&self) -> Option<runtime_db::RuntimeDbHandle> {
        self.services.runtime_db.clone()
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

    pub(super) fn next_internal_sub_id(&self) -> String {
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

    pub(crate) async fn record_initial_history(&self, conversation_history: InitialHistory) {
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

    pub(crate) async fn apply_rollout_reconstruction(
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

    pub(crate) async fn previous_turn_settings(&self) -> Option<PreviousTurnSettings> {
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

    pub(crate) async fn new_turn_with_sub_id(
        &self,
        sub_id: String,
        updates: SessionSettingsUpdate,
    ) -> crate::config::ConstraintResult<Arc<TurnContext>> {
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
                            chaos_error_info: Some(ChaosErrorInfo::BadRequest),
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

    pub(crate) async fn new_turn_from_configuration(
        &self,
        sub_id: String,
        session_configuration: SessionConfiguration,
        final_output_json_schema: Option<Option<serde_json::Value>>,
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
        let mut turn_context: TurnContext = make_turn_context(
            Some(Arc::clone(&self.services.auth_manager)),
            &self.services.session_telemetry,
            session_configuration.provider.clone(),
            &session_configuration,
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

    /// Associated-function shim so callers (including tests) can still write
    /// `Session::make_turn_context(...)` after the logic was extracted to
    /// `turn_context::make_turn_context`. Only used by test code.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn make_turn_context(
        auth_manager: Option<Arc<AuthManager>>,
        session_telemetry: &chaos_syslog::SessionTelemetry,
        provider: crate::ModelProviderInfo,
        session_configuration: &SessionConfiguration,
        per_turn_config: crate::config::Config,
        model_info: chaos_ipc::openai_models::ModelInfo,
        models_manager: &crate::models_manager::manager::ModelsManager,
        network: Option<chaos_pf::NetworkProxy>,
        sub_id: String,
        skills_outcome: Arc<crate::skills::SkillLoadOutcome>,
    ) -> TurnContext {
        make_turn_context(
            auth_manager,
            session_telemetry,
            provider,
            session_configuration,
            per_turn_config,
            model_info,
            models_manager,
            network,
            sub_id,
            skills_outcome,
        )
    }

    pub(crate) async fn new_default_turn(&self) -> Arc<TurnContext> {
        self.new_default_turn_with_sub_id(self.next_internal_sub_id())
            .await
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

    pub(crate) async fn build_settings_update_items(
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

    pub(crate) async fn deliver_event_raw(&self, event: Event) {
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

    pub(crate) async fn turn_context_for_sub_id(&self, sub_id: &str) -> Option<Arc<TurnContext>> {
        let active = self.active_turn.lock().await;
        active
            .as_ref()
            .and_then(|turn| turn.tasks.get(sub_id))
            .map(|task| Arc::clone(&task.turn_context))
    }

    pub(crate) async fn active_turn_context_and_cancellation_token(
        &self,
    ) -> Option<(Arc<TurnContext>, CancellationToken)> {
        let active = self.active_turn.lock().await;
        let (_, task) = active.as_ref()?.tasks.first()?;
        Some((
            Arc::clone(&task.turn_context),
            task.cancellation_token.child_token(),
        ))
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
            .counter("chaos.model_warning", /*inc*/ 1, &[]);
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

    pub(crate) async fn maybe_warn_on_server_model_mismatch(
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
            "Upstream rerouted this request from {requested_model} to {server_model}. The vendor did not honor your model selection."
        );

        self.send_event(
            turn_context,
            EventMsg::ModelReroute(crate::protocol::ModelRerouteEvent {
                from_model: requested_model.clone(),
                to_model: server_model.clone(),
                reason: crate::protocol::ModelRerouteReason::VendorDeclinedSelection,
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
            chaos_ipc::models::DeveloperInstructions::from_policy(
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
            chaos_ipc::models::DeveloperInstructions::from_collaboration_mode(&collaboration_mode)
        {
            developer_sections.push(collab_instructions.into_text());
        }
        if let Some(personality) = turn_context.personality {
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
                    chaos_ipc::models::DeveloperInstructions::personality_spec_message(
                        personality_message,
                    )
                    .into_text(),
                );
            }
        }
        let implicit_skills = turn_context
            .turn_skills
            .outcome
            .allowed_skills_for_implicit_invocation();
        if let Some(skills_section) = crate::skills::render_skills_section(&implicit_skills) {
            developer_sections.push(skills_section);
        }
        if let Some(user_instructions) = turn_context.user_instructions.as_deref() {
            contextual_user_sections.push(
                crate::instructions::UserInstructions {
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
            crate::environment_context::EnvironmentContext::from_turn_context(
                turn_context,
                shell.as_ref(),
            )
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
        token_usage: Option<&crate::protocol::TokenUsage>,
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
        new_rate_limits: crate::protocol::RateLimitSnapshot,
    ) {
        // GCRA rate-gate: warn when callers are repeatedly hitting the same
        // rate-limit window faster than 1 Hz.  TAT per limit_id is stored in a
        // process-wide table rather than on SessionState to keep it lightweight
        // and lock-contention free.
        if let Some(ref id) = new_rate_limits.limit_id {
            use std::sync::LazyLock;
            use std::sync::Mutex;
            static RATE_TATS: LazyLock<Mutex<HashMap<String, f64>>> =
                LazyLock::new(|| Mutex::new(HashMap::new()));

            let now = jiff::Timestamp::now().as_second() as f64;
            let emission_interval = 1.0_f64;
            let tat = RATE_TATS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(id)
                .copied()
                .unwrap_or(0.0);
            let result = throttle_machines::gcra::check(tat, now, emission_interval, 0.0);
            RATE_TATS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.clone(), result.new_tat);
            if !result.allowed {
                warn!(
                    limit_id = %id,
                    retry_after = result.retry_after,
                    "rate limit snapshot arriving faster than 1 Hz"
                );
            }
        }

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
        let event = EventMsg::BackgroundEvent(crate::protocol::BackgroundEventEvent {
            message: message.into(),
        });
        self.send_event(turn_context, event).await;
    }

    pub(crate) async fn notify_stream_error(
        &self,
        turn_context: &TurnContext,
        message: impl Into<String>,
        codex_error: crate::error::ChaosErr,
    ) {
        let additional_details = codex_error.to_string();
        let chaos_error_info = ChaosErrorInfo::ResponseStreamDisconnected {
            http_status_code: codex_error.http_status_code_value(),
        };
        let event = EventMsg::StreamError(crate::protocol::StreamErrorEvent {
            message: message.into(),
            chaos_error_info: Some(chaos_error_info),
            additional_details: Some(additional_details),
        });
        self.send_event(turn_context, event).await;
    }

    pub(crate) async fn maybe_start_ghost_snapshot(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        cancellation_token: CancellationToken,
    ) {
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

    pub(crate) fn user_shell(&self) -> Arc<crate::shell::Shell> {
        Arc::clone(&self.services.user_shell)
    }

    pub(crate) async fn take_pending_session_start_source(
        &self,
    ) -> Option<chaos_dtrace::SessionStartSource> {
        let mut state = self.state.lock().await;
        state.take_pending_session_start_source()
    }
}
