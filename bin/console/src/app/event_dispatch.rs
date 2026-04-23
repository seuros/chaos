use super::{
    App, AppEvent, AppRunControl, ApprovalRequest, COMMIT_ANIMATION_TICK, ChatWidget, ConfigEdit,
    ConfigEditsBuilder, CwdPromptAction, DiffSummary, Event, EventMsg, ExitMode, ExitReason,
    ExternalEditorState, HistoryCell, Line, Op, Ordering, Overlay, PaneId, Paragraph,
    ProcessEventStore, Renderable, Result, SessionSelection, Stylize, TuiEvent, Wrap,
    ansi_escape_line, highlight_bash_to_lines, session_summary, strip_bash_lc_and_escape, thread,
    tui,
};
use crate::onboarding::auth::AccountsWidget;
use std::sync::Arc;

impl App {
    pub(crate) async fn handle_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        event: TuiEvent,
    ) -> Result<AppRunControl> {
        if matches!(event, TuiEvent::Draw) {
            let size = tui.terminal.size()?;
            if size != tui.terminal.last_known_screen_size {
                self.refresh_status_line();
            }
        }

        if self.overlay.is_some() {
            let _ = self.handle_backtrack_overlay_event(tui, event).await?;
        } else {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_key_event(tui, key_event).await;
                }
                TuiEvent::Mouse(_) => {}

                TuiEvent::Paste(pasted) => {
                    // Only paste into chat when chat is focused — do not leak
                    // clipboard content into the composer from auxiliary panes.
                    let chat_focused = self
                        .tile_manager
                        .focused()
                        .is_none_or(|id| id == PaneId::ROOT);
                    if chat_focused {
                        // Many terminals convert newlines to \r when pasting (e.g., iTerm2),
                        // but tui-textarea expects \n. Normalize CR to LF.
                        let pasted = pasted.replace("\r", "\n");
                        self.chat_widget.handle_paste(pasted);
                    }
                }
                TuiEvent::Draw => {
                    if self.backtrack_render_pending {
                        self.backtrack_render_pending = false;
                        self.render_transcript_once(tui);
                    }
                    self.chat_widget.maybe_post_pending_notification(tui);
                    if self
                        .chat_widget
                        .handle_paste_burst_tick(tui.frame_requester())
                    {
                        return Ok(AppRunControl::Continue);
                    }
                    // Allow widgets to process any pending timers before rendering.
                    self.chat_widget.pre_draw_tick();
                    let terminal_size = tui.terminal.size()?;
                    // When tiled, the viewport must be tall enough for the
                    // auxiliary panes, not just the chat content.
                    let chat_area: ratatui::layout::Rect = terminal_size.into();
                    let desired = self.chat_widget.desired_height(chat_area.width);
                    let draw_height = if self.tile_manager.is_single_pane() {
                        desired
                    } else {
                        desired.max(terminal_size.height)
                    };
                    tui.draw(draw_height, |frame| {
                        let main_area = frame.area();

                        if self.tile_manager.is_single_pane() {
                            // Fast path: no tiling overhead, identical to pre-hypertile.
                            self.chat_widget.render(main_area, frame.buffer);
                            if let Some((x, y)) = self.chat_widget.cursor_pos(main_area) {
                                frame.set_cursor_position((x, y));
                            }
                        } else {
                            self.tile_manager.render(main_area, frame.buffer);

                            if let Some(chat_rect) = self.tile_manager.pane_rect(PaneId::ROOT) {
                                self.chat_widget.render(chat_rect, frame.buffer);
                            }

                            // Place cursor in chat pane.
                            if self.tile_manager.focused() == Some(PaneId::ROOT)
                                && let Some(chat_rect) = self.tile_manager.pane_rect(PaneId::ROOT)
                                && let Some((x, y)) = self.chat_widget.cursor_pos(chat_rect)
                            {
                                frame.set_cursor_position((x, y));
                            }
                        }
                    })?;
                    if self.chat_widget.external_editor_state() == ExternalEditorState::Requested {
                        self.chat_widget
                            .set_external_editor_state(ExternalEditorState::Active);
                        self.app_event_tx.send(AppEvent::LaunchExternalEditor);
                    }
                }
            }
        }
        Ok(AppRunControl::Continue)
    }

    pub(super) async fn handle_event(
        &mut self,
        tui: &mut tui::Tui,
        event: AppEvent,
    ) -> Result<AppRunControl> {
        match event {
            AppEvent::NewSession => {
                self.start_fresh_session_with_summary_hint(tui).await;
            }
            AppEvent::ClearUi => {
                self.clear_terminal_ui(tui, /*redraw_header*/ false)?;
                self.reset_app_ui_state_after_clear();

                self.start_fresh_session_with_summary_hint(tui).await;
            }
            AppEvent::OpenResumePicker => {
                match crate::resume_picker::run_resume_picker(
                    tui,
                    &self.config,
                    /*show_all*/ false,
                )
                .await?
                {
                    SessionSelection::Resume(target_session) => {
                        let current_cwd = self.config.cwd.clone();
                        let resume_cwd = match crate::resolve_cwd_for_resume_or_fork(
                            tui,
                            &self.config,
                            &current_cwd,
                            target_session.process_id,
                            CwdPromptAction::Resume,
                            /*allow_prompt*/ true,
                        )
                        .await?
                        {
                            crate::ResolveCwdOutcome::Continue(Some(cwd)) => cwd,
                            crate::ResolveCwdOutcome::Continue(None) => current_cwd.clone(),
                            crate::ResolveCwdOutcome::Exit => {
                                return Ok(AppRunControl::Exit(ExitReason::UserRequested));
                            }
                        };
                        let mut resume_config = match self
                            .rebuild_config_for_resume_or_fallback(&current_cwd, resume_cwd)
                            .await
                        {
                            Ok(cfg) => cfg,
                            Err(err) => {
                                self.chat_widget.add_error_message(format!(
                                    "Failed to rebuild configuration for resume: {err}"
                                ));
                                return Ok(AppRunControl::Continue);
                            }
                        };
                        self.apply_runtime_policy_overrides(&mut resume_config);
                        let summary = session_summary(
                            self.chat_widget.token_usage(),
                            self.chat_widget.process_id(),
                            self.chat_widget.process_name(),
                        );
                        match self
                            .server
                            .resume_process(
                                resume_config.clone(),
                                target_session.process_id,
                                self.auth_manager.clone(),
                                /*parent_trace*/ None,
                            )
                            .await
                        {
                            Ok(resumed) => {
                                self.shutdown_current_process().await;
                                self.config = resume_config;
                                tui.set_notification_method(self.config.tui_notification_method);
                                self.file_search.update_search_dir(self.config.cwd.clone());
                                let init = self.chatwidget_init_for_forked_or_resumed_process(
                                    tui,
                                    self.config.clone(),
                                );
                                let (_, process, session_configured) = resumed.into_parts();
                                self.chat_widget = ChatWidget::new_from_existing(
                                    init,
                                    process,
                                    session_configured,
                                );
                                self.reset_process_event_state();
                                if let Some(summary) = summary {
                                    let mut lines: Vec<Line<'static>> =
                                        vec![summary.usage_line.clone().into()];
                                    if let Some(command) = summary.resume_command {
                                        let spans = vec![
                                            "To continue this session, run ".into(),
                                            command.cyan(),
                                        ];
                                        lines.push(spans.into());
                                    }
                                    self.chat_widget.add_plain_history_lines(lines);
                                }
                            }
                            Err(err) => {
                                self.chat_widget.add_error_message(format!(
                                    "Failed to resume session {}: {err}",
                                    target_session.process_id
                                ));
                            }
                        }
                    }
                    SessionSelection::Exit
                    | SessionSelection::StartFresh
                    | SessionSelection::Fork(_) => {}
                }

                // Leaving alt-screen may blank the inline viewport; force a redraw either way.
                tui.frame_requester().schedule_frame();
            }
            AppEvent::ForkCurrentSession => {
                self.session_telemetry.counter(
                    "chaos.thread.fork",
                    /*inc*/ 1,
                    &[("source", "slash_command")],
                );
                let summary = session_summary(
                    self.chat_widget.token_usage(),
                    self.chat_widget.process_id(),
                    self.chat_widget.process_name(),
                );
                self.chat_widget
                    .add_plain_history_lines(vec!["/fork".magenta().into()]);
                if let Some(process_id) = self.chat_widget.process_id() {
                    self.refresh_in_memory_config_from_disk_best_effort("forking the process")
                        .await;
                    match self
                        .server
                        .fork_process_by_id(
                            usize::MAX,
                            self.config.clone(),
                            process_id,
                            /*persist_extended_history*/ false,
                            /*parent_trace*/ None,
                        )
                        .await
                    {
                        Ok(forked) => {
                            self.shutdown_current_process().await;
                            let init = self.chatwidget_init_for_forked_or_resumed_process(
                                tui,
                                self.config.clone(),
                            );
                            let (_, process, session_configured) = forked.into_parts();
                            self.chat_widget =
                                ChatWidget::new_from_existing(init, process, session_configured);
                            self.reset_process_event_state();
                            if let Some(summary) = summary {
                                let mut lines: Vec<Line<'static>> =
                                    vec![summary.usage_line.clone().into()];
                                if let Some(command) = summary.resume_command {
                                    let spans = vec![
                                        "To continue this session, run ".into(),
                                        command.cyan(),
                                    ];
                                    lines.push(spans.into());
                                }
                                self.chat_widget.add_plain_history_lines(lines);
                            }
                        }
                        Err(err) => {
                            self.chat_widget.add_error_message(format!(
                                "Failed to fork current session {process_id}: {err}"
                            ));
                        }
                    }
                } else {
                    self.chat_widget.add_error_message(
                        "A process must contain at least one turn before it can be forked."
                            .to_string(),
                    );
                }

                tui.frame_requester().schedule_frame();
            }
            AppEvent::InsertHistoryCell(cell) => {
                let cell: Arc<dyn HistoryCell> = cell.into();
                if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                    t.insert_cell(cell.clone());
                    tui.frame_requester().schedule_frame();
                }
                self.transcript_cells.push(cell.clone());
                let mut display = cell.display_lines(tui.terminal.last_known_screen_size.width);
                if !display.is_empty() {
                    // Only insert a separating blank line for new cells that are not
                    // part of an ongoing stream. Streaming continuations should not
                    // accrue extra blank lines between chunks.
                    if !cell.is_stream_continuation() {
                        if self.has_emitted_history_lines {
                            display.insert(0, Line::from(""));
                        } else {
                            self.has_emitted_history_lines = true;
                        }
                    }
                    if self.overlay.is_some() {
                        self.deferred_history_lines.extend(display);
                    } else {
                        tui.insert_history_lines(display);
                    }
                }
            }
            AppEvent::ApplyProcessRollback { num_turns } => {
                if self.apply_non_pending_process_rollback(num_turns) {
                    tui.frame_requester().schedule_frame();
                }
            }
            AppEvent::StartCommitAnimation => {
                if self
                    .commit_anim_running
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    let tx = self.app_event_tx.clone();
                    let running = self.commit_anim_running.clone();
                    thread::spawn(move || {
                        while running.load(Ordering::Relaxed) {
                            thread::sleep(COMMIT_ANIMATION_TICK);
                            tx.send(AppEvent::CommitTick);
                        }
                    });
                }
            }
            AppEvent::StopCommitAnimation => {
                self.commit_anim_running.store(false, Ordering::Release);
            }
            AppEvent::CommitTick => {
                self.chat_widget.on_commit_tick();
            }
            AppEvent::ChaosEvent(event) => {
                self.enqueue_primary_event(event).await?;
            }
            AppEvent::ProcessEvent { process_id, event } => {
                self.handle_routed_process_event(process_id, event).await?;
            }
            AppEvent::Exit(mode) => {
                return Ok(self.handle_exit_mode(mode));
            }
            AppEvent::FatalExitRequest(message) => {
                return Ok(AppRunControl::Exit(ExitReason::Fatal(message)));
            }
            AppEvent::ChaosOp(op) => {
                let replay_state_op =
                    ProcessEventStore::op_can_change_pending_replay_state(&op).then(|| op.clone());
                let submitted = self.chat_widget.submit_op(op);
                if submitted && let Some(op) = replay_state_op.as_ref() {
                    self.note_active_process_outbound_op(op).await;
                    self.refresh_pending_process_approvals().await;
                }
            }
            AppEvent::SubmitProcessOp { process_id, op } => {
                self.submit_op_to_process(process_id, op).await;
            }
            AppEvent::DiffResult(text) => {
                // Clear the in-progress state in the bottom pane
                self.chat_widget.on_diff_complete();
                // Enter alternate screen using TUI helper and build pager lines
                let _ = tui.enter_alt_screen();
                let pager_lines: Vec<ratatui::text::Line<'static>> = if text.trim().is_empty() {
                    vec!["No changes detected.".italic().into()]
                } else {
                    text.lines().map(ansi_escape_line).collect()
                };
                self.overlay = Some(Overlay::new_static_with_lines(
                    pager_lines,
                    "D I F F".to_string(),
                ));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenAppLink {
                app_id,
                title,
                description,
                instructions,
                url,
                is_installed,
                is_enabled,
            } => {
                self.chat_widget
                    .open_app_link_view(crate::bottom_pane::AppLinkViewParams {
                        app_id,
                        title,
                        description,
                        instructions,
                        url,
                        is_installed,
                        is_enabled,
                        suggest_reason: None,
                        suggestion_type: None,
                        elicitation_target: None,
                    });
            }
            AppEvent::OpenUrlInBrowser { url } => {
                let _ = self.open_url_in_browser(url);
            }
            AppEvent::OpenUrlElicitationInBrowser {
                process_id,
                server_name,
                request_id,
                url,
                on_open,
                on_error,
            } => {
                let decision = if self.open_url_in_browser(url) {
                    on_open
                } else {
                    on_error
                };
                self.submit_op_to_process(
                    process_id,
                    Op::ResolveElicitation {
                        server_name,
                        request_id,
                        decision,
                        content: None,
                        meta: None,
                    },
                )
                .await;
            }
            AppEvent::RefreshConnectors { force_refetch } => {
                self.chat_widget.refresh_connectors(force_refetch);
            }
            AppEvent::StartFileSearch(query) => {
                self.file_search.on_user_query(query);
            }
            AppEvent::FileSearchResult { query, matches } => {
                self.chat_widget.apply_file_search_result(query, matches);
            }
            AppEvent::ConnectorsLoaded { result, is_final } => {
                self.chat_widget.on_connectors_loaded(result, is_final);
            }
            AppEvent::UpdateReasoningEffort(effort) => {
                self.on_update_reasoning_effort(effort);
                self.refresh_status_line();
            }
            AppEvent::UpdateModel(model) => {
                self.chat_widget.set_model(&model);
                self.refresh_status_line();
            }
            AppEvent::UpdateCollaborationMode(mask) => {
                self.chat_widget.set_collaboration_mask(mask);
                self.refresh_status_line();
            }
            AppEvent::UpdatePersonality(personality) => {
                self.on_update_personality(personality);
            }
            AppEvent::OpenReasoningPopup { model } => {
                self.chat_widget.open_reasoning_popup(model);
            }
            AppEvent::OpenPlanReasoningScopePrompt { model, effort } => {
                self.chat_widget
                    .open_plan_reasoning_scope_prompt(model, effort);
            }
            AppEvent::OpenAllModelsPopup { models } => {
                self.chat_widget.open_all_models_popup(models);
            }
            AppEvent::OpenFullAccessConfirmation {
                preset,
                return_to_permissions,
            } => {
                self.chat_widget
                    .open_full_access_confirmation(preset, return_to_permissions);
            }
            AppEvent::LaunchExternalEditor => {
                if self.chat_widget.external_editor_state() == ExternalEditorState::Active {
                    self.launch_external_editor(tui).await;
                }
            }
            AppEvent::PersistModelSelection { model, effort } => {
                if crate::theme::is_clamped() {
                    tracing::debug!(%model, "skipping model persistence while clamped");
                    return Ok(AppRunControl::Continue);
                }

                let profile = self.active_profile.as_deref();
                match ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_profile(profile)
                    .set_model(Some(model.as_str()), effort)
                    .apply()
                    .await
                {
                    Ok(()) => {
                        let effort_label = effort
                            .map(|selected_effort| selected_effort.to_string())
                            .unwrap_or_else(|| "default".to_string());
                        tracing::info!("Selected model: {model}, Selected effort: {effort_label}");
                        let mut message = format!("Model changed to {model}");
                        if let Some(label) = Self::reasoning_label_for(&model, effort) {
                            message.push(' ');
                            message.push_str(label);
                        }
                        if let Some(profile) = profile {
                            message.push_str(" for ");
                            message.push_str(profile);
                            message.push_str(" profile");
                        }
                        self.chat_widget.add_info_message(message, /*hint*/ None);
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to persist model selection"
                        );
                        if let Some(profile) = profile {
                            self.chat_widget.add_error_message(format!(
                                "Failed to save model for profile `{profile}`: {err}"
                            ));
                        } else {
                            self.chat_widget
                                .add_error_message(format!("Failed to save default model: {err}"));
                        }
                    }
                }
            }
            AppEvent::PersistPersonalitySelection { personality } => {
                let profile = self.active_profile.as_deref();
                match ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_profile(profile)
                    .set_personality(Some(personality))
                    .apply()
                    .await
                {
                    Ok(()) => {
                        let label = Self::personality_label(personality);
                        let mut message = format!("Personality set to {label}");
                        if let Some(profile) = profile {
                            message.push_str(" for ");
                            message.push_str(profile);
                            message.push_str(" profile");
                        }
                        self.chat_widget.add_info_message(message, /*hint*/ None);
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "failed to persist personality selection"
                        );
                        if let Some(profile) = profile {
                            self.chat_widget.add_error_message(format!(
                                "Failed to save personality for profile `{profile}`: {err}"
                            ));
                        } else {
                            self.chat_widget.add_error_message(format!(
                                "Failed to save default personality: {err}"
                            ));
                        }
                    }
                }
            }
            AppEvent::UpdateApprovalPolicy(policy) => {
                let mut config = self.config.clone();
                if !self.try_set_approval_policy_on_config(
                    &mut config,
                    policy,
                    "Failed to set approval policy",
                    "failed to set approval policy on app config",
                ) {
                    return Ok(AppRunControl::Continue);
                }
                self.config = config;
                self.runtime_approval_policy_override =
                    Some(self.config.permissions.approval_policy.value());
                self.chat_widget
                    .set_approval_policy(self.config.permissions.approval_policy.value());
            }
            AppEvent::UpdateSandboxPolicy(policy) => {
                let policy_for_chat = policy.clone();

                let mut config = self.config.clone();
                if !self.try_set_sandbox_policy_on_config(
                    &mut config,
                    policy,
                    "Failed to set sandbox policy",
                    "failed to set sandbox policy on app config",
                ) {
                    return Ok(AppRunControl::Continue);
                }
                self.config = config;
                if let Err(err) = self.chat_widget.set_sandbox_policy(policy_for_chat) {
                    tracing::warn!(%err, "failed to set sandbox policy on chat config");
                    self.chat_widget
                        .add_error_message(format!("Failed to set sandbox policy: {err}"));
                    return Ok(AppRunControl::Continue);
                }
                self.runtime_sandbox_policy_override =
                    Some(self.config.permissions.sandbox_policy.get().clone());
            }
            AppEvent::UpdateApprovalsReviewer(policy) => {
                self.config.approvals_reviewer = policy;
                self.chat_widget.set_approvals_reviewer(policy);
                let profile = self.active_profile.as_deref();
                let segments = if let Some(profile) = profile {
                    vec![
                        "profiles".to_string(),
                        profile.to_string(),
                        "approvals_reviewer".to_string(),
                    ]
                } else {
                    vec!["approvals_reviewer".to_string()]
                };
                if let Err(err) = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_profile(profile)
                    .with_edits([ConfigEdit::SetPath {
                        segments,
                        value: policy.to_string().into(),
                    }])
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist approvals reviewer update"
                    );
                    self.chat_widget
                        .add_error_message(format!("Failed to save approvals reviewer: {err}"));
                }
            }
            AppEvent::UpdateFeatureFlags { updates } => {
                self.update_feature_flags(updates).await;
            }
            AppEvent::UpdateFullAccessWarningAcknowledged(ack) => {
                self.chat_widget.set_full_access_warning_acknowledged(ack);
            }
            AppEvent::UpdateRateLimitSwitchPromptHidden(hidden) => {
                self.chat_widget.set_rate_limit_switch_prompt_hidden(hidden);
            }
            AppEvent::UpdatePlanModeReasoningEffort(effort) => {
                self.config.plan_mode_reasoning_effort = effort;
                self.chat_widget.set_plan_mode_reasoning_effort(effort);
                self.refresh_status_line();
            }
            AppEvent::PersistFullAccessWarningAcknowledged => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .set_hide_full_access_warning(/*acknowledged*/ true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist full access warning acknowledgement"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save full access confirmation preference: {err}"
                    ));
                }
            }
            AppEvent::PersistRateLimitSwitchPromptHidden => {
                if let Err(err) = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .set_hide_rate_limit_model_nudge(/*acknowledged*/ true)
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist rate limit switch prompt preference"
                    );
                    self.chat_widget.add_error_message(format!(
                        "Failed to save rate limit reminder preference: {err}"
                    ));
                }
            }
            AppEvent::PersistPlanModeReasoningEffort(effort) => {
                let profile = self.active_profile.as_deref();
                let segments = if let Some(profile) = profile {
                    vec![
                        "profiles".to_string(),
                        profile.to_string(),
                        "plan_mode_reasoning_effort".to_string(),
                    ]
                } else {
                    vec!["plan_mode_reasoning_effort".to_string()]
                };
                let edit = if let Some(effort) = effort {
                    ConfigEdit::SetPath {
                        segments,
                        value: effort.to_string().into(),
                    }
                } else {
                    ConfigEdit::ClearPath { segments }
                };
                if let Err(err) = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_edits([edit])
                    .apply()
                    .await
                {
                    tracing::error!(
                        error = %err,
                        "failed to persist plan mode reasoning effort"
                    );
                    if let Some(profile) = profile {
                        self.chat_widget.add_error_message(format!(
                            "Failed to save Plan mode reasoning effort for profile `{profile}`: {err}"
                        ));
                    } else {
                        self.chat_widget.add_error_message(format!(
                            "Failed to save Plan mode reasoning effort: {err}"
                        ));
                    }
                }
            }
            AppEvent::OpenApprovalsPopup => {
                self.chat_widget.open_approvals_popup();
            }
            AppEvent::OpenAccountsPopup => {
                let _ = tui.enter_alt_screen();
                self.overlay = Some(Overlay::new_accounts(AccountsWidget::new(
                    tui.frame_requester(),
                    self.config.chaos_home.clone(),
                    self.config.cli_auth_credentials_store_mode,
                    self.auth_manager.clone(),
                    &self.config.model_providers,
                    self.config.forced_chatgpt_workspace_id.clone(),
                    self.config.forced_login_method,
                    self.config.animations,
                )));
                tui.frame_requester().schedule_frame();
            }
            AppEvent::OpenAgentPicker => {
                self.open_agent_picker().await;
            }
            AppEvent::SelectAgentProcess(process_id) => {
                self.select_agent_process(tui, process_id).await?;
            }
            AppEvent::SetAppEnabled { id, enabled } => {
                let edits = if enabled {
                    vec![
                        ConfigEdit::ClearPath {
                            segments: vec!["apps".to_string(), id.clone(), "enabled".to_string()],
                        },
                        ConfigEdit::ClearPath {
                            segments: vec![
                                "apps".to_string(),
                                id.clone(),
                                "disabled_reason".to_string(),
                            ],
                        },
                    ]
                } else {
                    vec![
                        ConfigEdit::SetPath {
                            segments: vec!["apps".to_string(), id.clone(), "enabled".to_string()],
                            value: false.into(),
                        },
                        ConfigEdit::SetPath {
                            segments: vec![
                                "apps".to_string(),
                                id.clone(),
                                "disabled_reason".to_string(),
                            ],
                            value: "user".into(),
                        },
                    ]
                };
                match ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_edits(edits)
                    .apply()
                    .await
                {
                    Ok(()) => {
                        self.chat_widget.update_connector_enabled(&id, enabled);
                        if let Err(err) = self.refresh_in_memory_config_from_disk().await {
                            tracing::warn!(error = %err, "failed to refresh config after app toggle");
                        }
                        self.chat_widget.submit_op(Op::ReloadUserConfig);
                    }
                    Err(err) => {
                        self.chat_widget.add_error_message(format!(
                            "Failed to update app config for {id}: {err}"
                        ));
                    }
                }
            }
            AppEvent::OpenPermissionsPopup => {
                self.chat_widget.open_permissions_popup();
            }
            AppEvent::OpenReviewBranchPicker(cwd) => {
                self.chat_widget.show_review_branch_picker(&cwd).await;
            }
            AppEvent::OpenReviewCommitPicker(cwd) => {
                self.chat_widget.show_review_commit_picker(&cwd).await;
            }
            AppEvent::OpenReviewCustomPrompt => {
                self.chat_widget.show_review_custom_prompt();
            }
            AppEvent::SubmitUserMessageWithMode {
                text,
                collaboration_mode,
            } => {
                self.chat_widget
                    .submit_user_message_with_mode(text, collaboration_mode);
            }
            AppEvent::FullScreenApprovalRequest(request) => match request {
                ApprovalRequest::ApplyPatch { cwd, changes, .. } => {
                    let _ = tui.enter_alt_screen();
                    let diff_summary = DiffSummary::new(changes, cwd);
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![diff_summary.into()],
                        "P A T C H".to_string(),
                    ));
                }
                ApprovalRequest::Exec { command, .. } => {
                    let _ = tui.enter_alt_screen();
                    let full_cmd = strip_bash_lc_and_escape(&command);
                    let full_cmd_lines = highlight_bash_to_lines(&full_cmd);
                    self.overlay = Some(Overlay::new_static_with_lines(
                        full_cmd_lines,
                        "E X E C".to_string(),
                    ));
                }
                ApprovalRequest::Permissions {
                    permissions,
                    reason,
                    ..
                } => {
                    let _ = tui.enter_alt_screen();
                    let mut lines = Vec::new();
                    if let Some(reason) = reason {
                        lines.push(Line::from(vec!["Reason: ".into(), reason.italic()]));
                        lines.push(Line::from(""));
                    }
                    if let Some(rule_line) =
                        crate::bottom_pane::format_requested_permissions_rule(&permissions)
                    {
                        lines.push(Line::from(vec![
                            "Permission rule: ".into(),
                            rule_line.cyan(),
                        ]));
                    }
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![Box::new(Paragraph::new(lines).wrap(Wrap { trim: false }))],
                        "P E R M I S S I O N S".to_string(),
                    ));
                }
                ApprovalRequest::McpElicitation {
                    server_name,
                    message,
                    url,
                    ..
                } => {
                    let _ = tui.enter_alt_screen();
                    let mut lines = vec![
                        Line::from(vec!["Server: ".into(), server_name.bold()]),
                        Line::from(""),
                    ];
                    if let Some(url) = url {
                        lines.push(Line::from(vec!["URL: ".into(), url.cyan().underlined()]));
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(message));
                    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
                    self.overlay = Some(Overlay::new_static_with_renderables(
                        vec![Box::new(paragraph)],
                        "E L I C I T A T I O N".to_string(),
                    ));
                }
            },
            AppEvent::StatusLineSetup { items } => {
                let ids = items.iter().map(ToString::to_string).collect::<Vec<_>>();
                let edit = chaos_kern::config::edit::status_line_items_edit(&ids);
                let apply_result = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_edits([edit])
                    .apply()
                    .await;
                match apply_result {
                    Ok(()) => {
                        self.config.tui_status_line = Some(ids.clone());
                        self.chat_widget.setup_status_line(items);
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "failed to persist status line items; keeping previous selection");
                        self.chat_widget
                            .add_error_message(format!("Failed to save status line items: {err}"));
                    }
                }
            }
            AppEvent::StatusLineBranchUpdated { cwd, branch } => {
                self.chat_widget.set_status_line_branch(cwd, branch);
                self.refresh_status_line();
            }
            AppEvent::StatusLineSetupCancelled => {
                self.chat_widget.cancel_status_line_setup();
            }
            AppEvent::SyntaxThemeSelected { name } => {
                let edit = chaos_kern::config::edit::syntax_theme_edit(&name);
                let apply_result = ConfigEditsBuilder::new(&self.config.chaos_home)
                    .with_edits([edit])
                    .apply()
                    .await;
                match apply_result {
                    Ok(()) => {
                        // Ensure the selected theme is active in the current
                        // session.  The preview callback covers arrow-key
                        // navigation, but if the user presses Enter without
                        // navigating, the runtime theme must still be applied.
                        if let Some(theme) = crate::render::highlight::resolve_theme_by_name(
                            &name,
                            Some(&self.config.chaos_home),
                        ) {
                            crate::render::highlight::set_syntax_theme(theme);
                        }
                        self.sync_tui_theme_selection(name);
                    }
                    Err(err) => {
                        self.restore_runtime_theme_from_config();
                        tracing::error!(error = %err, "failed to persist theme selection");
                        self.chat_widget
                            .add_error_message(format!("Failed to save theme: {err}"));
                    }
                }
            }
            AppEvent::AllToolsReceived(ev) => {
                self.on_all_tools_received(tui, ev);
            }
            AppEvent::ReloadProjectMcpForProcess(process_id) => {
                if let Err(err) = self.server.reload_project_mcp_for_process(process_id).await {
                    tracing::warn!(
                        error = %err,
                        %process_id,
                        "failed to reload project MCP layer for process"
                    );
                }
            }
        }
        Ok(AppRunControl::Continue)
    }

    pub(super) fn handle_exit_mode(&mut self, mode: ExitMode) -> AppRunControl {
        match mode {
            ExitMode::ShutdownFirst => {
                // Mark the thread we are explicitly shutting down for exit so
                // its shutdown completion does not trigger agent failover.
                self.pending_shutdown_exit_process_id =
                    self.active_process_id.or(self.chat_widget.process_id());
                if self.chat_widget.submit_op(Op::Shutdown) {
                    AppRunControl::Continue
                } else {
                    self.pending_shutdown_exit_process_id = None;
                    AppRunControl::Exit(ExitReason::UserRequested)
                }
            }
            ExitMode::Immediate => {
                self.pending_shutdown_exit_process_id = None;
                AppRunControl::Exit(ExitReason::UserRequested)
            }
        }
    }

    pub(super) fn handle_codex_event_now(&mut self, event: Event) {
        let needs_refresh = matches!(
            event.msg,
            EventMsg::SessionConfigured(_) | EventMsg::TurnStarted(_) | EventMsg::TokenCount(_)
        );
        // This guard is only for intentional thread-switch shutdowns.
        // App-exit shutdowns are tracked by `pending_shutdown_exit_process_id`
        // and resolved in `handle_active_process_event`.
        if self.suppress_shutdown_complete && matches!(event.msg, EventMsg::ShutdownComplete) {
            self.suppress_shutdown_complete = false;
            return;
        }
        self.handle_backtrack_event(&event.msg);
        self.chat_widget.handle_codex_event(event);

        if needs_refresh {
            self.refresh_status_line();
        }
    }

    pub(super) fn handle_codex_event_replay(&mut self, event: Event) {
        self.chat_widget.handle_codex_event_replay(event);
    }

    /// Handles an event emitted by the currently active thread.
    ///
    /// This function enforces shutdown intent routing: unexpected non-primary
    /// thread shutdowns fail over to the primary thread, while user-requested
    /// app exits consume only the tracked shutdown completion and then proceed.
    pub(super) async fn handle_active_process_event(
        &mut self,
        tui: &mut tui::Tui,
        event: Event,
    ) -> Result<()> {
        // Capture this before any potential thread switch: we only want to clear
        // the exit marker when the currently active thread acknowledges shutdown.
        let pending_shutdown_exit_completed = matches!(&event.msg, EventMsg::ShutdownComplete)
            && self.pending_shutdown_exit_process_id == self.active_process_id;

        // Processing order matters:
        //
        // 1. handle unexpected non-primary shutdown failover first;
        // 2. clear pending exit marker for matching shutdown;
        // 3. forward the event through normal handling.
        //
        // This preserves the mental model that user-requested exits do not trigger
        // failover, while true sub-agent deaths still do.
        if let Some((closed_process_id, primary_process_id)) =
            self.active_non_primary_shutdown_target(&event.msg)
        {
            self.mark_agent_picker_process_closed(closed_process_id);
            self.select_agent_process(tui, primary_process_id).await?;
            if self.active_process_id == Some(primary_process_id) {
                self.chat_widget.add_info_message(
                    format!(
                        "Agent process {closed_process_id} closed. Switched back to the main process."
                    ),
                    /*hint*/ None,
                );
            } else {
                self.clear_active_thread().await;
                self.chat_widget.add_error_message(format!(
                    "Agent process {closed_process_id} closed. Failed to switch back to the main process {primary_process_id}.",
                ));
            }
            return Ok(());
        }

        if pending_shutdown_exit_completed {
            // Clear only after seeing the shutdown completion for the tracked
            // thread, so unrelated shutdowns cannot consume this marker.
            self.pending_shutdown_exit_process_id = None;
        }
        self.handle_codex_event_now(event);
        if self.backtrack_render_pending {
            tui.frame_requester().schedule_frame();
        }
        Ok(())
    }
}
