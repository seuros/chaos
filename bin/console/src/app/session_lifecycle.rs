use super::{
    AgentNavigationState, App, AppEvent, AppEventSender, AppExitInfo, AppRunControl, Arc,
    AtomicBool, AuthManager, BacktrackState, Cell, ChatWidget, Config, ConfigOverrides, Duration,
    Event, EventMsg, ExitMode, FileSearchManager, HashMap, Line, LogPanelState,
    PROCESS_EVENT_CHANNEL_CAPACITY, ProcessEventChannel, ProcessEventSnapshot, ProcessId,
    ProcessTable, Rc, RefCell, RefreshStrategy, Result, SessionConfiguredEvent, SessionSelection,
    SessionSource, SessionTelemetry, StateRuntime, Stylize, TelemetryAuthMode, TileManager,
    TomlValue, ToolListPane, VecDeque, WrapErr, emit_project_config_warnings,
    normalize_harness_overrides_for_cwd, select, session_summary, tui, unbounded_channel,
};
use chaos_ipc::protocol::Op;
use chaos_kern::ChaosAuth;
use chaos_kern::models_manager::CollaborationModesConfig;
use std::path::PathBuf;
use tokio::sync::broadcast;

impl App {
    pub(super) fn chatwidget_init_for_forked_or_resumed_process(
        &self,
        tui: &mut tui::Tui,
        cfg: chaos_kern::config::Config,
    ) -> crate::chatwidget::ChatWidgetInit {
        crate::chatwidget::ChatWidgetInit {
            config: cfg,
            frame_requester: tui.frame_requester(),
            app_event_tx: self.app_event_tx.clone(),
            // Fork/resume bootstraps here don't carry any prefilled message content.
            initial_user_message: None,
            enhanced_keys_supported: self.enhanced_keys_supported,
            auth_manager: self.auth_manager.clone(),
            models_manager: self.server.get_models_manager(),
            is_first_run: false,
            model: Some(self.chat_widget.current_model().to_string()),
            status_line_invalid_items_warned: self.status_line_invalid_items_warned.clone(),
            session_telemetry: self.session_telemetry.clone(),
        }
    }

    pub(super) async fn shutdown_current_process(&mut self) {
        if let Some(process_id) = self.chat_widget.process_id() {
            // Clear any in-flight rollback guard when switching processes.
            self.backtrack.pending_rollback = None;
            self.suppress_shutdown_complete = true;
            self.chat_widget.submit_op(Op::Shutdown);
            self.server.remove_process(&process_id).await;
            self.abort_process_event_listener(process_id);
        }
    }

    pub(super) fn reset_process_event_state(&mut self) {
        self.abort_all_process_event_listeners();
        self.process_event_channels.clear();
        self.agent_navigation.clear();
        self.active_process_id = None;
        self.active_process_rx = None;
        self.primary_process_id = None;
        self.pending_primary_events.clear();
        self.chat_widget.set_pending_process_approvals(Vec::new());
        self.sync_active_agent_label();
    }

    pub(crate) async fn start_fresh_session_with_summary_hint(&mut self, tui: &mut tui::Tui) {
        // Start a fresh in-memory session while preserving resumability via persisted rollout
        // history.
        let previous_provider_id = self.config.model_provider_id.clone();
        self.refresh_in_memory_config_from_disk_best_effort("starting a new process")
            .await;
        let provider_changed = self.config.model_provider_id != previous_provider_id;
        if provider_changed {
            self.server = Arc::new(ProcessTable::new(
                &self.config,
                self.auth_manager.clone(),
                self.server.session_source(),
                CollaborationModesConfig {
                    default_mode_request_user_input: true,
                },
            ));
        }
        let model = if provider_changed {
            self.server
                .get_models_manager()
                .get_default_model(&None, RefreshStrategy::Offline)
                .await
        } else {
            self.chat_widget.current_model().to_string()
        };
        let config = self.fresh_session_config();
        let summary = session_summary(
            self.chat_widget.token_usage(),
            self.chat_widget.process_id(),
            self.chat_widget.process_name(),
        );
        self.shutdown_current_process().await;
        let report = self
            .server
            .shutdown_all_processes_bounded(Duration::from_secs(10))
            .await;
        if !report.submit_failed.is_empty() || !report.timed_out.is_empty() {
            tracing::warn!(
                submit_failed = report.submit_failed.len(),
                timed_out = report.timed_out.len(),
                "failed to close all processes"
            );
        }
        let init = crate::chatwidget::ChatWidgetInit {
            config,
            frame_requester: tui.frame_requester(),
            app_event_tx: self.app_event_tx.clone(),
            // New sessions start without prefilled message content.
            initial_user_message: None,
            enhanced_keys_supported: self.enhanced_keys_supported,
            auth_manager: self.auth_manager.clone(),
            models_manager: self.server.get_models_manager(),
            is_first_run: false,
            model: Some(model),
            status_line_invalid_items_warned: self.status_line_invalid_items_warned.clone(),
            session_telemetry: self.session_telemetry.clone(),
        };
        self.chat_widget = ChatWidget::new(init, self.server.clone());
        self.reset_process_event_state();
        if let Some(summary) = summary {
            let mut lines: Vec<Line<'static>> = vec![summary.usage_line.clone().into()];
            if let Some(command) = summary.resume_command {
                let spans = vec!["To continue this session, run ".into(), command.cyan()];
                lines.push(spans.into());
            }
            self.chat_widget.add_plain_history_lines(lines);
        }
        tui.frame_requester().schedule_frame();
    }

    /// Returns `(closed_process_id, primary_process_id)` when a non-primary active
    /// thread has died and we should fail over to the primary thread.
    ///
    /// A user-requested shutdown (`ExitMode::ShutdownFirst`) sets
    /// `pending_shutdown_exit_process_id`; matching shutdown completions are ignored
    /// here so Ctrl+C-like exits don't accidentally resurrect the main thread.
    ///
    /// Failover is only eligible when all of these are true:
    /// 1. the event is `ShutdownComplete`;
    /// 2. the active thread differs from the primary thread;
    /// 3. the active thread is not the pending shutdown-exit thread.
    pub(super) fn active_non_primary_shutdown_target(
        &self,
        msg: &EventMsg,
    ) -> Option<(ProcessId, ProcessId)> {
        if !matches!(msg, EventMsg::ShutdownComplete) {
            return None;
        }
        let active_process_id = self.active_process_id?;
        let primary_process_id = self.primary_process_id?;
        if self.pending_shutdown_exit_process_id == Some(active_process_id) {
            return None;
        }
        (active_process_id != primary_process_id).then_some((active_process_id, primary_process_id))
    }

    pub(super) fn replay_process_snapshot(
        &mut self,
        snapshot: ProcessEventSnapshot,
        resume_restored_queue: bool,
    ) {
        if let Some(event) = snapshot.session_configured {
            self.handle_codex_event_replay(event);
        }
        self.chat_widget
            .set_queue_autosend_suppressed(/*suppressed*/ true);
        self.chat_widget
            .restore_process_input_state(snapshot.input_state);
        for event in snapshot.events {
            self.handle_codex_event_replay(event);
        }
        self.chat_widget
            .set_queue_autosend_suppressed(/*suppressed*/ false);
        if resume_restored_queue {
            self.chat_widget.maybe_send_next_queued_input();
        }
        self.refresh_status_line();
    }

    pub(super) fn should_wait_for_initial_session(session_selection: &SessionSelection) -> bool {
        matches!(
            session_selection,
            SessionSelection::StartFresh | SessionSelection::Exit
        )
    }

    pub(super) fn should_handle_active_process_events(
        waiting_for_initial_session_configured: bool,
        has_active_process_receiver: bool,
    ) -> bool {
        has_active_process_receiver && !waiting_for_initial_session_configured
    }

    pub(super) fn should_stop_waiting_for_initial_session(
        waiting_for_initial_session_configured: bool,
        primary_process_id: Option<ProcessId>,
    ) -> bool {
        waiting_for_initial_session_configured && primary_process_id.is_some()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        tui: &mut tui::Tui,
        auth_manager: Arc<AuthManager>,
        process_table: Arc<ProcessTable>,
        log_state_db: Option<Arc<StateRuntime>>,
        config: Config,
        cli_kv_overrides: Vec<(String, TomlValue)>,
        harness_overrides: ConfigOverrides,
        active_profile: Option<String>,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
        session_selection: SessionSelection,
        is_first_run: bool,
        start_clamped: bool,
    ) -> Result<AppExitInfo> {
        use tokio_stream::StreamExt;
        let (app_event_tx, mut app_event_rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);
        emit_project_config_warnings(&app_event_tx, &config);
        tui.set_notification_method(config.tui_notification_method);

        let harness_overrides =
            normalize_harness_overrides_for_cwd(harness_overrides, &config.cwd)?;
        let auth_manager = auth_manager.clone();
        let process_table = process_table.clone();
        let model = process_table
            .get_models_manager()
            .get_default_model(&config.model, RefreshStrategy::Offline)
            .await;
        let auth = if config.model_provider_id == chaos_kern::auth::DEFAULT_AUTH_PROVIDER_ID {
            auth_manager.auth().await
        } else {
            auth_manager.auth_for_provider(&config.model_provider_id)
        };
        let auth_ref = auth.as_ref();
        let auth_mode = auth_ref
            .map(ChaosAuth::auth_mode)
            .map(TelemetryAuthMode::from);
        let session_telemetry = SessionTelemetry::new(
            ProcessId::new(),
            model.as_str(),
            model.as_str(),
            auth_mode,
            chaos_kern::default_client::originator().value.as_str(),
            config.otel.log_user_prompt,
            chaos_kern::terminal::user_agent(),
            SessionSource::Cli,
        );
        if config
            .tui_status_line
            .as_ref()
            .is_some_and(|cmd| !cmd.is_empty())
        {
            session_telemetry.counter("chaos.status_line", /*inc*/ 1, &[]);
        }

        let status_line_invalid_items_warned = Arc::new(AtomicBool::new(false));

        let enhanced_keys_supported = tui.enhanced_keys_supported();
        let wait_for_initial_session_configured =
            Self::should_wait_for_initial_session(&session_selection);
        let chat_widget = match session_selection {
            SessionSelection::StartFresh | SessionSelection::Exit => {
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: process_table.get_models_manager(),
                    is_first_run,
                    model: Some(model.clone()),
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                ChatWidget::new(init, process_table.clone())
            }
            SessionSelection::Resume(target_session) => {
                let resumed = process_table
                    .resume_process(
                        config.clone(),
                        target_session.process_id,
                        auth_manager.clone(),
                        /*parent_trace*/ None,
                    )
                    .await
                    .wrap_err_with(|| {
                        format!("Failed to resume session {}", target_session.process_id)
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: process_table.get_models_manager(),
                    is_first_run,
                    model: config.model.clone(),
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                let (_, process, session_configured) = resumed.into_parts();
                ChatWidget::new_from_existing(init, process, session_configured)
            }
            SessionSelection::Fork(target_session) => {
                session_telemetry.counter(
                    "chaos.thread.fork",
                    /*inc*/ 1,
                    &[("source", "cli_subcommand")],
                );
                let forked = process_table
                    .fork_process_by_id(
                        usize::MAX,
                        config.clone(),
                        target_session.process_id,
                        /*persist_extended_history*/ false,
                        /*parent_trace*/ None,
                    )
                    .await
                    .wrap_err_with(|| {
                        format!("Failed to fork session {}", target_session.process_id)
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    auth_manager: auth_manager.clone(),
                    models_manager: process_table.get_models_manager(),
                    is_first_run,
                    model: config.model.clone(),
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                let (_, process, session_configured) = forked.into_parts();
                ChatWidget::new_from_existing(init, process, session_configured)
            }
        };

        let file_search = FileSearchManager::new(config.cwd.clone(), app_event_tx.clone());
        let tool_list_pane = Rc::new(RefCell::new(ToolListPane::new()));
        let tool_list_close = Rc::new(Cell::new(false));
        let mut app = Self {
            server: process_table.clone(),
            session_telemetry: session_telemetry.clone(),
            app_event_tx,
            chat_widget,
            auth_manager: auth_manager.clone(),
            config,
            active_profile,
            cli_kv_overrides,
            harness_overrides,
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            tile_manager: TileManager::new(tool_list_pane.clone(), tool_list_close.clone()),
            tool_list_pane,
            tool_list_close,
            file_search,
            log_state_db,
            log_state_db_init_error: None,
            log_panel: LogPanelState::default(),
            enhanced_keys_supported,
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            suppress_shutdown_complete: false,
            pending_shutdown_exit_process_id: None,

            process_event_channels: HashMap::new(),
            process_event_listener_tasks: HashMap::new(),
            agent_navigation: AgentNavigationState::default(),
            active_process_id: None,
            active_process_rx: None,
            primary_process_id: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
        };

        if start_clamped {
            app.chat_widget.activate_clamp();
        }

        let tui_events = tui.event_stream();
        tokio::pin!(tui_events);

        tui.frame_requester().schedule_frame();

        let mut process_created_rx = process_table.subscribe_process_created();
        let mut listen_for_threads = true;
        let mut waiting_for_initial_session_configured = wait_for_initial_session_configured;

        let exit_reason_result = {
            loop {
                let control = select! {
                    Some(event) = app_event_rx.recv() => {
                        match app.handle_event(tui, event).await {
                            Ok(control) => control,
                            Err(err) => break Err(err),
                        }
                    }
                    active = async {
                        if let Some(rx) = app.active_process_rx.as_mut() {
                            rx.recv().await
                        } else {
                            None
                        }
                    }, if App::should_handle_active_process_events(
                        waiting_for_initial_session_configured,
                        app.active_process_rx.is_some()
                    ) => {
                        if let Some(event) = active {
                            if let Err(err) = app.handle_active_process_event(tui, event).await {
                                break Err(err);
                            }
                        } else {
                            app.clear_active_thread().await;
                        }
                        AppRunControl::Continue
                    }
                    event = tui_events.next() => {
                        match event {
                            Some(event) => match app.handle_tui_event(tui, event).await {
                                Ok(control) => control,
                                Err(err) => break Err(err),
                            },
                            None => {
                                tracing::warn!(
                                    "terminal input stream closed; shutting down active thread"
                                );
                                app.handle_exit_mode(ExitMode::ShutdownFirst)
                            }
                        }
                    }
                    // Listen on new thread creation due to collab tools.
                    created = process_created_rx.recv(), if listen_for_threads => {
                        match created {
                            Ok(process_id) => {
                                if let Err(err) = app.handle_process_created(process_id).await {
                                    break Err(err);
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                tracing::warn!("process_created receiver lagged; skipping resync");
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                listen_for_threads = false;
                            }
                        }
                        AppRunControl::Continue
                    }
                };
                if App::should_stop_waiting_for_initial_session(
                    waiting_for_initial_session_configured,
                    app.primary_process_id,
                ) {
                    waiting_for_initial_session_configured = false;
                }
                match control {
                    AppRunControl::Continue => {}
                    AppRunControl::Exit(reason) => break Ok(reason),
                }
            }
        };
        let clear_result = tui.terminal.clear();
        let exit_reason = match exit_reason_result {
            Ok(exit_reason) => {
                clear_result?;
                exit_reason
            }
            Err(err) => {
                if let Err(clear_err) = clear_result {
                    tracing::warn!(error = %clear_err, "failed to clear terminal UI");
                }
                return Err(err);
            }
        };
        Ok(AppExitInfo {
            token_usage: app.token_usage(),
            process_id: app.chat_widget.process_id(),
            process_name: app.chat_widget.process_name(),
            exit_reason,
        })
    }

    pub(super) async fn handle_process_created(&mut self, process_id: ProcessId) -> Result<()> {
        if self.process_event_channels.contains_key(&process_id) {
            return Ok(());
        }
        let thread = match self.server.get_process(process_id).await {
            Ok(thread) => thread,
            Err(err) => {
                tracing::warn!("failed to attach listener for process {process_id}: {err}");
                return Ok(());
            }
        };
        let config_snapshot = thread.config_snapshot().await;
        self.upsert_agent_picker_thread(
            process_id,
            config_snapshot.session_source.get_nickname(),
            config_snapshot.session_source.get_agent_role(),
            /*is_closed*/ false,
        );
        let event = Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: process_id,
                forked_from_id: None,
                process_name: None,
                model: config_snapshot.model,
                model_provider_id: config_snapshot.model_provider_id,
                service_tier: config_snapshot.service_tier,
                approval_policy: config_snapshot.approval_policy,
                approvals_reviewer: config_snapshot.approvals_reviewer,
                vfs_policy: config_snapshot.vfs_policy,
                socket_policy: config_snapshot.socket_policy,
                cwd: config_snapshot.cwd,
                reasoning_effort: config_snapshot.reasoning_effort,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
            }),
        };
        let channel =
            ProcessEventChannel::new_with_session_configured(PROCESS_EVENT_CHANNEL_CAPACITY, event);
        let app_event_tx = self.app_event_tx.clone();
        self.process_event_channels.insert(process_id, channel);
        let listener_handle = tokio::spawn(async move {
            loop {
                let event = match thread.next_event().await {
                    Ok(event) => event,
                    Err(err) => {
                        tracing::debug!("external process {process_id} listener stopped: {err}");
                        break;
                    }
                };
                app_event_tx.send(AppEvent::ProcessEvent { process_id, event });
            }
        });
        self.process_event_listener_tasks
            .insert(process_id, listener_handle);
        Ok(())
    }
}
