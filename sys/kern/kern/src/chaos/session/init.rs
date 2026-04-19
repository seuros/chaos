use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use async_channel::Sender;
use chaos_dtrace::Hooks;
use chaos_dtrace::HooksConfig;
use chaos_ipc::ProcessId;
use chaos_ipc::models::BaseInstructions;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::InitialHistory;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::WarningEvent;
use chaos_mcp_runtime::manager::McpConnectionManager;
use chaos_pf::NetworkProxyAuditMetadata;
use chaos_syslog::SessionTelemetry;
use chaos_syslog::TelemetryAuthMode;
use chaos_syslog::metrics::names::THREAD_STARTED_METRIC;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::error;
use tracing::info_span;
use tracing::instrument;
use tracing::warn;

use crate::AuthManager;
use crate::ChaosAuth;
use crate::SandboxState;
use crate::config::Config;
use crate::config::StartedNetworkProxy;
use crate::config::resolve_web_search_mode_for_turn;
use crate::exec_policy::ExecPolicyManager;
use crate::file_watcher::FileWatcher;
use crate::git_info::get_git_repo_root;
use crate::mcp::McpManager;
use crate::mcp::auth::compute_auth_statuses;
use crate::minions::AgentControl;
use crate::minions::AgentStatus;
use crate::models_manager::manager::ModelsManager;
use crate::rollout::RolloutRecorder;
use crate::rollout::RolloutRecorderParams;
use crate::rollout::policy::EventPersistenceMode;
use crate::rollout::process_names;
use crate::runtime_db;
use crate::shell;
use crate::shell_snapshot::ShellSnapshot;
use crate::state::SessionServices;
use crate::state::SessionState;
use crate::tools::network_approval::NetworkApprovalService;
use crate::tools::network_approval::build_blocked_request_observer;
use crate::tools::network_approval::build_network_policy_decider;
use crate::tools::sandboxing::ApprovalStore;

use super::Session;
use crate::chaos::INITIAL_SUBMIT_ID;
use crate::chaos::SessionConfiguration;
use crate::chaos::submission_loop;

impl Session {
    /// Builds the `x-chaos-beta-features` header value for this session.
    pub(crate) fn build_model_client_beta_features_header(_config: &Config) -> Option<String> {
        None
    }

    pub(crate) async fn start_managed_network_proxy(
        spec: &crate::config::NetworkProxySpec,
        exec_policy: &chaos_selinux::Policy,
        vfs_policy: &chaos_ipc::permissions::VfsPolicy,
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
                vfs_policy,
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
            &session_configuration.vfs_policy,
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

    pub(super) fn start_file_watcher_listener(self: &Arc<Self>) {
        let mut rx = self.services.file_watcher.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Ok(_) => continue,
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
        mcp_manager: Arc<McpManager>,
        file_watcher: Arc<FileWatcher>,
        agent_control: AgentControl,
    ) -> anyhow::Result<Arc<Self>> {
        tracing::debug!(
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
                crate::message_history::history_metadata(state_db_ctx.as_ref()).await
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
                    &config.permissions.vfs_policy,
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
            mcp_connection_manager: Arc::new(RwLock::new(McpConnectionManager::new_uninitialized(
                &config.permissions.approval_policy,
            ))),
            mcp_startup_cancellation_token: Mutex::new(CancellationToken::new()),
            internal_task_store: crate::internal_tasks::InternalTaskStore::default(),
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
        tracing::debug!(
            provider = %session_configuration.provider.name,
            model = %session_configuration.collaboration_mode.model(),
            vfs_policy = ?session_configuration.vfs_policy,
            socket_policy = ?session_configuration.socket_policy,
            approval_policy = ?session_configuration.approval_policy.value(),
            reasoning_effort = ?session_configuration.collaboration_mode.reasoning_effort(),
            cwd = %session_configuration.cwd.display(),
            "session configured",
        );

        let initial_messages =
            submission_loop::initial_replay_event_msgs(&initial_history, conversation_id);
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
                vfs_policy: session_configuration.vfs_policy.clone(),
                socket_policy: session_configuration.socket_policy,
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

        sess.start_file_watcher_listener();
        let sandbox_state = SandboxState {
            vfs_policy: session_configuration.vfs_policy.clone(),
            socket_policy: session_configuration.socket_policy,
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

        sess.record_initial_history(initial_history).await;
        {
            let mut state = sess.state.lock().await;
            state.set_pending_session_start_source(Some(session_start_source));
        }

        Ok(sess)
    }
}
