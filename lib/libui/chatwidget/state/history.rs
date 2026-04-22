//! History management: message submission, queueing, cell insertion, notifications,
//! connectors popup, and related helpers.
use super::super::*;
use chaos_chassis::turn::TurnContext;
use chaos_chassis::turn::TurnSubmission;

impl ChatWidget {
    pub(crate) fn flush_active_cell(&mut self) {
        if let Some(active) = self.active_cell.take() {
            self.needs_final_message_separator = true;
            self.app_event_tx.send(AppEvent::InsertHistoryCell(active));
        }
    }

    pub fn add_to_history(&mut self, cell: impl HistoryCell + 'static) {
        self.add_boxed_history(Box::new(cell));
    }

    pub(crate) fn add_boxed_history(&mut self, cell: Box<dyn HistoryCell>) {
        if !cell.display_lines(u16::MAX).is_empty() {
            // Only break exec grouping if the cell renders visible lines.
            self.flush_active_cell();
            self.needs_final_message_separator = true;
        }
        self.app_event_tx.send(AppEvent::InsertHistoryCell(cell));
    }

    pub(crate) fn queue_user_message(&mut self, user_message: UserMessage) {
        if !self.is_session_configured()
            || self.bottom_pane.is_task_running()
            || self.is_review_mode
        {
            self.queued_user_messages.push_back(user_message);
            self.refresh_pending_input_preview();
        } else {
            self.submit_user_message(user_message);
        }
    }

    pub(crate) fn submit_user_message(&mut self, user_message: UserMessage) {
        if !self.is_session_configured() {
            tracing::warn!("cannot submit user message before session is configured; queueing");
            self.queued_user_messages.push_front(user_message);
            self.refresh_pending_input_preview();
            return;
        }
        if self.is_review_mode {
            self.queued_user_messages.push_back(user_message);
            self.refresh_pending_input_preview();
            return;
        }

        let UserMessage {
            text,
            local_images,
            remote_image_urls,
            text_elements,
            mention_bindings,
        } = user_message;
        if text.is_empty() && local_images.is_empty() && remote_image_urls.is_empty() {
            return;
        }
        if (!local_images.is_empty() || !remote_image_urls.is_empty())
            && !self.current_model_supports_images()
        {
            self.restore_blocked_image_submission(
                text,
                text_elements,
                local_images,
                mention_bindings,
                remote_image_urls,
            );
            return;
        }

        let render_in_history = !self.agent_turn_running;
        let mut items: Vec<UserInput> = Vec::new();

        // Special-case: "!cmd" executes a local shell command instead of sending to the model.
        if let Some(stripped) = text.strip_prefix('!') {
            let cmd = stripped.trim();
            if cmd.is_empty() {
                self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_info_event(
                        USER_SHELL_COMMAND_HELP_TITLE.to_string(),
                        Some(USER_SHELL_COMMAND_HELP_HINT.to_string()),
                    ),
                )));
                return;
            }
            self.submit_op(Op::RunUserShellCommand {
                command: cmd.to_string(),
            });
            return;
        }

        for image_url in &remote_image_urls {
            items.push(UserInput::Image {
                image_url: image_url.clone(),
            });
        }

        for image in &local_images {
            items.push(UserInput::LocalImage {
                path: image.path.clone(),
            });
        }

        if !text.is_empty() {
            items.push(UserInput::Text {
                text: text.clone(),
                text_elements: text_elements.clone(),
            });
        }

        let mut selected_app_ids: HashSet<String> = HashSet::new();
        if let Some(apps) = self.connectors_for_mentions() {
            for binding in &mention_bindings {
                let Some(app_id) = binding
                    .path
                    .strip_prefix("app://")
                    .filter(|id| !id.is_empty())
                else {
                    continue;
                };
                if !selected_app_ids.insert(app_id.to_string()) {
                    continue;
                }
                if let Some(app) = apps.iter().find(|app| app.id == app_id && app.is_enabled) {
                    items.push(UserInput::Mention {
                        name: app.name.clone(),
                        path: binding.path.clone(),
                    });
                }
            }
        }

        let effective_mode = self.effective_collaboration_mode();
        let collaboration_mode = if self.collaboration_modes_enabled() {
            self.active_collaboration_mask
                .as_ref()
                .map(|_| effective_mode.clone())
        } else {
            None
        };
        let pending_steer = (!render_in_history).then(|| PendingSteer {
            user_message: UserMessage {
                text: text.clone(),
                local_images: local_images.clone(),
                remote_image_urls: remote_image_urls.clone(),
                text_elements: text_elements.clone(),
                mention_bindings: mention_bindings.clone(),
            },
            compare_key: Self::pending_steer_compare_key_from_items(&items),
        });
        let personality = self
            .config
            .personality
            .filter(|_| self.current_model_supports_personality());
        let service_tier = self.config.service_tier.map(Some);
        let op = TurnSubmission::new(
            items,
            TurnContext {
                cwd: self.config.cwd.clone(),
                approval_policy: self.config.permissions.approval_policy.value(),
                sandbox_policy: self.config.permissions.sandbox_policy.get().clone(),
                model: effective_mode.model().to_string(),
                effort: effective_mode.reasoning_effort(),
                summary: None,
                service_tier,
                final_output_json_schema: None,
                collaboration_mode,
                personality,
            },
        )
        .into_op();

        if !self.submit_op(op) {
            return;
        }

        // Persist the text to cross-session message history.
        if !text.is_empty() {
            let encoded_mentions = mention_bindings
                .iter()
                .map(|binding| LinkedMention {
                    mention: binding.mention.clone(),
                    path: binding.path.clone(),
                })
                .collect::<Vec<_>>();
            let history_text = encode_history_mentions(&text, &encoded_mentions);
            self.chaos_op_tx
                .send(Op::AddToHistory { text: history_text })
                .unwrap_or_else(|e| {
                    tracing::error!("failed to send AddHistory op: {e}");
                });
        }

        if let Some(pending_steer) = pending_steer {
            self.pending_steers.push_back(pending_steer);
            self.saw_plan_item_this_turn = false;
            self.refresh_pending_input_preview();
        }

        // Show replayable user content in conversation history.
        if render_in_history && !text.is_empty() {
            let local_image_paths = local_images
                .into_iter()
                .map(|img| img.path)
                .collect::<Vec<_>>();
            self.last_rendered_user_message_event =
                Some(Self::rendered_user_message_event_from_parts(
                    text.clone(),
                    text_elements.clone(),
                    local_image_paths.clone(),
                    remote_image_urls.clone(),
                ));
            self.add_to_history(history_cell::new_user_prompt(
                text,
                text_elements,
                local_image_paths,
                remote_image_urls,
            ));
        } else if render_in_history && !remote_image_urls.is_empty() {
            self.last_rendered_user_message_event =
                Some(Self::rendered_user_message_event_from_parts(
                    String::new(),
                    Vec::new(),
                    Vec::new(),
                    remote_image_urls.clone(),
                ));
            self.add_to_history(history_cell::new_user_prompt(
                String::new(),
                Vec::new(),
                Vec::new(),
                remote_image_urls,
            ));
        }

        self.needs_final_message_separator = false;
    }

    /// Restore the blocked submission draft without losing mention resolution state.
    ///
    /// The blocked-image path intentionally keeps the draft in the composer so
    /// users can remove attachments and retry. We must restore
    /// mention bindings alongside visible text; restoring only `$name` tokens
    /// makes the draft look correct while degrading mention resolution to
    /// name-only heuristics on retry.
    pub(crate) fn restore_blocked_image_submission(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_images: Vec<LocalImageAttachment>,
        mention_bindings: Vec<MentionBinding>,
        remote_image_urls: Vec<String>,
    ) {
        // Preserve the user's composed payload so they can retry after changing models.
        let local_image_paths = local_images.iter().map(|img| img.path.clone()).collect();
        self.set_remote_image_urls(remote_image_urls);
        self.bottom_pane.set_composer_text_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
        self.add_to_history(history_cell::new_warning_event(
            self.image_inputs_not_supported_message(),
        ));
        self.request_redraw();
    }

    /// Exit the UI immediately without waiting for shutdown.
    ///
    /// Prefer [`Self::request_quit_without_confirmation`] for user-initiated exits;
    /// this is mainly a fallback for shutdown completion or emergency exits.
    pub(crate) fn request_immediate_exit(&self) {
        self.app_event_tx.send(AppEvent::Exit(ExitMode::Immediate));
    }

    /// Request a shutdown-first quit.
    ///
    /// This is used for explicit quit commands (`/quit`, `/exit`, `/logout`) and for
    /// the double-press Ctrl+C/Ctrl+D quit shortcut.
    pub(crate) fn request_quit_without_confirmation(&self) {
        self.app_event_tx
            .send(AppEvent::Exit(ExitMode::ShutdownFirst));
    }

    pub(crate) fn request_redraw(&mut self) {
        self.frame_requester.schedule_frame();
    }

    pub(crate) fn bump_active_cell_revision(&mut self) {
        // Wrapping avoids overflow; wraparound would require 2^64 bumps and at
        // worst causes a one-time cache-key collision.
        self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
    }

    pub(in crate::chatwidget) fn notify(&mut self, notification: Notification) {
        if !notification.allowed_for(&self.config.tui_notifications) {
            return;
        }
        if let Some(existing) = self.pending_notification.as_ref()
            && existing.priority() > notification.priority()
        {
            return;
        }
        self.pending_notification = Some(notification);
        self.request_redraw();
    }

    pub fn maybe_post_pending_notification(&mut self, tui: &mut crate::tui::Tui) {
        if let Some(notif) = self.pending_notification.take() {
            tui.notify(notif.display());
        }
    }

    /// Mark the active cell as failed (✗) and flush it into history.
    pub(crate) fn finalize_active_cell_as_failed(&mut self) {
        if let Some(mut cell) = self.active_cell.take() {
            // Insert finalized cell into history and keep grouping consistent.
            if let Some(exec) = cell.as_any_mut().downcast_mut::<ExecCell>() {
                exec.mark_failed();
            } else if let Some(tool) = cell.as_any_mut().downcast_mut::<McpToolCallCell>() {
                tool.mark_failed();
            }
            self.add_boxed_history(cell);
        }
    }

    pub fn set_queue_autosend_suppressed(&mut self, suppressed: bool) {
        self.suppress_queue_autosend = suppressed;
    }

    // If idle and there are queued inputs, submit exactly one to start the next turn.
    pub fn maybe_send_next_queued_input(&mut self) {
        if self.suppress_queue_autosend {
            return;
        }
        if self.bottom_pane.is_task_running() {
            return;
        }
        if let Some(user_message) = self.queued_user_messages.pop_front() {
            self.submit_user_message(user_message);
        }
        // Update the list to reflect the remaining queued messages (if any).
        self.refresh_pending_input_preview();
    }

    /// Rebuild and update the bottom-pane pending-input preview.
    pub(crate) fn refresh_pending_input_preview(&mut self) {
        let queued_messages: Vec<String> = self
            .queued_user_messages
            .iter()
            .map(|m| m.text.clone())
            .collect();
        let pending_steers: Vec<String> = self
            .pending_steers
            .iter()
            .map(|steer| steer.user_message.text.clone())
            .collect();
        self.bottom_pane
            .set_pending_input_preview(queued_messages, pending_steers);
    }

    pub fn set_pending_process_approvals(&mut self, threads: Vec<String>) {
        self.bottom_pane.set_pending_process_approvals(threads);
    }

    pub fn add_diff_in_progress(&mut self) {
        self.request_redraw();
    }

    pub fn on_diff_complete(&mut self) {
        self.request_redraw();
    }

    pub fn add_status_output(&mut self) {
        let default_usage = TokenUsage::default();
        let token_info = self.token_info.as_ref();
        let total_usage = token_info
            .map(|ti| &ti.total_token_usage)
            .unwrap_or(&default_usage);
        let collaboration_mode = self.collaboration_mode_label();
        let reasoning_effort_override = Some(self.effective_reasoning_effort());
        let rate_limit_snapshots: Vec<RateLimitSnapshotDisplay> = self
            .rate_limit_snapshots_by_limit_id
            .values()
            .cloned()
            .collect();
        self.add_to_history(crate::status::new_status_output_with_rate_limits(
            &self.config,
            self.auth_manager.as_ref(),
            token_info,
            total_usage,
            &self.process_id,
            self.process_name.clone(),
            self.forked_from,
            rate_limit_snapshots.as_slice(),
            self.plan_type,
            Timestamp::now(),
            self.model_display_name(),
            collaboration_mode,
            reasoning_effort_override,
        ));
    }

    pub fn add_debug_config_output(&mut self) {
        self.add_to_history(crate::debug_config::new_debug_config_output(
            &self.config,
            self.session_network_proxy.as_ref(),
        ));
    }

    pub fn add_ps_output(&mut self) {
        let processes = self
            .unified_exec_processes
            .iter()
            .map(|process| history_cell::UnifiedExecProcessDetails {
                command_display: process.command_display.clone(),
                recent_chunks: process.recent_chunks.clone(),
            })
            .collect();
        self.add_to_history(history_cell::new_unified_exec_processes_output(processes));
    }

    pub(crate) fn clean_background_terminals(&mut self) {
        self.submit_op(Op::CleanBackgroundTerminals);
        self.add_info_message(
            "Stopping all background terminals.".to_string(),
            /*hint*/ None,
        );
    }

    pub fn refresh_connectors(&mut self, force_refetch: bool) {
        self.prefetch_connectors_with_options(force_refetch);
    }

    pub(crate) fn prefetch_connectors(&mut self) {
        self.prefetch_connectors_with_options(/*force_refetch*/ false);
    }

    pub(crate) fn prefetch_connectors_with_options(&mut self, force_refetch: bool) {
        if !self.connectors_enabled() {
            return;
        }
        if self.connectors_prefetch_in_flight {
            if force_refetch {
                self.connectors_force_refetch_pending = true;
            }
            return;
        }

        // Connectors infrastructure removed — return empty results immediately.
        self.connectors_prefetch_in_flight = false;
        let snapshot = ConnectorsSnapshot {
            connectors: Vec::new(),
        };
        self.connectors_cache = ConnectorsCacheState::Ready(snapshot);
    }

    pub(crate) fn connectors_enabled(&self) -> bool {
        false
    }

    pub(crate) fn connectors_for_mentions(&self) -> Option<&[AppInfo]> {
        if !self.connectors_enabled() {
            return None;
        }

        if let Some(snapshot) = &self.connectors_partial_snapshot {
            return Some(snapshot.connectors.as_slice());
        }

        match &self.connectors_cache {
            ConnectorsCacheState::Ready(snapshot) => Some(snapshot.connectors.as_slice()),
            _ => None,
        }
    }

    /// Build a placeholder cell while the session is configuring.
    pub(crate) fn placeholder_session_header_cell(_config: &Config) -> Box<dyn HistoryCell> {
        Box::new(history_cell::PlainHistoryCell::new(Vec::new()))
    }

    /// Apply the real session info cell.
    pub(crate) fn apply_session_info_cell(&mut self, cell: history_cell::SessionInfoCell) {
        // Replace any placeholder active cell with the real session info.
        self.active_cell.take();
        let cell = Box::new(cell) as Box<dyn HistoryCell>;
        if !cell.display_lines(u16::MAX).is_empty() {
            self.add_boxed_history(cell);
        }
    }

    pub fn add_info_message(&mut self, message: String, hint: Option<String>) {
        self.add_to_history(history_cell::new_info_event(message, hint));
        self.request_redraw();
    }

    pub fn add_plain_history_lines(&mut self, lines: Vec<Line<'static>>) {
        self.add_boxed_history(Box::new(PlainHistoryCell::new(lines)));
        self.request_redraw();
    }

    pub fn add_error_message(&mut self, message: String) {
        self.add_to_history(history_cell::new_error_event(message));
        self.request_redraw();
    }

    pub(crate) fn rename_confirmation_cell(
        name: &str,
        process_id: Option<ProcessId>,
    ) -> PlainHistoryCell {
        let resume_cmd = chaos_kern::util::resume_command(Some(name), process_id)
            .unwrap_or_else(|| format!("chaos resume {name}"));
        let name = name.to_string();
        let line = vec![
            "• ".into(),
            "Thread renamed to ".into(),
            name.cyan(),
            ", to resume this thread run ".into(),
            resume_cmd.cyan(),
        ];
        PlainHistoryCell::new(vec![line.into()])
    }

    pub fn add_mcp_output(&mut self) {
        let mcp_manager = McpManager::new();
        if mcp_manager.effective_servers(&self.config).is_empty() {
            self.add_to_history(history_cell::empty_mcp_output());
        } else {
            self.submit_op(Op::ListMcpTools);
        }
    }

    pub fn open_mcp_add_form(&mut self) {
        let cwd = self.config.cwd.clone();
        let view = crate::bottom_pane::McpAddForm::new(
            cwd,
            self.config.config_layer_stack.clone(),
            self.process_id,
            self.app_event_tx.clone(),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    #[allow(dead_code)]
    pub fn add_connectors_output(&mut self) {
        if !self.connectors_enabled() {
            self.add_info_message(
                "Apps are disabled.".to_string(),
                Some("Enable the apps feature to use $ or /apps.".to_string()),
            );
            return;
        }

        let connectors_cache = self.connectors_cache.clone();
        let should_force_refetch = !self.connectors_prefetch_in_flight
            || matches!(connectors_cache, ConnectorsCacheState::Ready(_));
        self.prefetch_connectors_with_options(should_force_refetch);

        match connectors_cache {
            ConnectorsCacheState::Ready(snapshot) => {
                if snapshot.connectors.is_empty() {
                    self.add_info_message("No apps available.".to_string(), /*hint*/ None);
                } else {
                    self.open_connectors_popup(&snapshot.connectors);
                }
            }
            ConnectorsCacheState::Failed(err) => {
                self.add_to_history(history_cell::new_error_event(err));
            }
            ConnectorsCacheState::Loading | ConnectorsCacheState::Uninitialized => {
                self.open_connectors_loading_popup();
            }
        }
        self.request_redraw();
    }

    #[allow(dead_code)]
    fn open_connectors_loading_popup(&mut self) {
        if !self.bottom_pane.replace_selection_view_if_active(
            CONNECTORS_SELECTION_VIEW_ID,
            self.connectors_loading_popup_params(),
        ) {
            self.bottom_pane
                .show_selection_view(self.connectors_loading_popup_params());
        }
    }

    #[allow(dead_code)]
    fn open_connectors_popup(&mut self, connectors: &[AppInfo]) {
        self.bottom_pane.show_selection_view(
            self.connectors_popup_params(connectors, /*selected_connector_id*/ None),
        );
    }

    #[allow(dead_code)]
    fn connectors_loading_popup_params(&self) -> SelectionViewParams {
        let mut header = ColumnRenderable::new();
        header.push(Line::from("Apps".bold()));
        header.push(Line::from("Loading installed and available apps...".dim()));

        SelectionViewParams {
            view_id: Some(CONNECTORS_SELECTION_VIEW_ID),
            header: Box::new(header),
            items: vec![SelectionItem {
                name: "Loading apps...".to_string(),
                description: Some("This updates when the full list is ready.".to_string()),
                is_disabled: true,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn connectors_popup_params(
        &self,
        connectors: &[AppInfo],
        selected_connector_id: Option<&str>,
    ) -> SelectionViewParams {
        let total = connectors.len();
        let installed = connectors
            .iter()
            .filter(|connector| connector.is_accessible)
            .count();
        let mut header = ColumnRenderable::new();
        header.push(Line::from("Apps".bold()));
        header.push(Line::from(
            "Use $ to insert an installed app into your prompt.".dim(),
        ));
        header.push(Line::from(
            format!("Installed {installed} of {total} available apps.").dim(),
        ));
        let initial_selected_idx = selected_connector_id.and_then(|selected_connector_id| {
            connectors
                .iter()
                .position(|connector| connector.id == selected_connector_id)
        });
        let mut items: Vec<SelectionItem> = Vec::with_capacity(connectors.len());
        for connector in connectors {
            let connector_label = connector.name.clone();
            let connector_title = connector_label.clone();
            let link_description = Self::connector_description(connector);
            let description = Self::connector_brief_description(connector);
            let status_label = Self::connector_status_label(connector);
            let search_value = format!("{connector_label} {}", connector.id);
            let mut item = SelectionItem {
                name: connector_label,
                description: Some(description),
                search_value: Some(search_value),
                ..Default::default()
            };
            let is_installed = connector.is_accessible;
            let selected_label = if is_installed {
                format!(
                    "{status_label}. Press Enter to open the app page to install, manage, or enable/disable this app."
                )
            } else {
                format!("{status_label}. Press Enter to open the app page to install this app.")
            };
            let missing_label = format!("{status_label}. App link unavailable.");
            let instructions = if connector.is_accessible {
                "Manage this app in your browser."
            } else {
                "Install this app in your browser, then reload Chaos."
            };
            if let Some(install_url) = connector.install_url.clone() {
                let app_id = connector.id.clone();
                let is_enabled = connector.is_enabled;
                let title = connector_title.clone();
                let instructions = instructions.to_string();
                let description = link_description.clone();
                item.actions = vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenAppLink {
                        app_id: app_id.clone(),
                        title: title.clone(),
                        description: description.clone(),
                        instructions: instructions.clone(),
                        url: install_url.clone(),
                        is_installed,
                        is_enabled,
                    });
                })];
                item.dismiss_on_select = true;
                item.selected_description = Some(selected_label);
            } else {
                let missing_label_for_action = missing_label.clone();
                item.actions = vec![Box::new(move |tx| {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_info_event(
                            missing_label_for_action.clone(),
                            /*hint*/ None,
                        ),
                    )));
                })];
                item.dismiss_on_select = true;
                item.selected_description = Some(missing_label);
            }
            items.push(item);
        }

        SelectionViewParams {
            view_id: Some(CONNECTORS_SELECTION_VIEW_ID),
            header: Box::new(header),
            footer_hint: Some(Self::connectors_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search apps".to_string()),
            col_width_mode: ColumnWidthMode::AutoAllRows,
            initial_selected_idx,
            ..Default::default()
        }
    }

    pub(crate) fn refresh_connectors_popup_if_open(&mut self, connectors: &[AppInfo]) {
        let selected_connector_id =
            if let (Some(selected_index), ConnectorsCacheState::Ready(snapshot)) = (
                self.bottom_pane
                    .selected_index_for_active_view(CONNECTORS_SELECTION_VIEW_ID),
                &self.connectors_cache,
            ) {
                snapshot
                    .connectors
                    .get(selected_index)
                    .map(|connector| connector.id.as_str())
            } else {
                None
            };
        let _ = self.bottom_pane.replace_selection_view_if_active(
            CONNECTORS_SELECTION_VIEW_ID,
            self.connectors_popup_params(connectors, selected_connector_id),
        );
    }

    fn connectors_popup_hint_line() -> Line<'static> {
        Line::from(vec![
            "Press ".into(),
            key_hint::plain(KeyCode::Esc).into(),
            " to close.".into(),
        ])
    }

    fn connector_brief_description(connector: &AppInfo) -> String {
        let status_label = Self::connector_status_label(connector);
        match Self::connector_description(connector) {
            Some(description) => format!("{status_label} · {description}"),
            None => status_label.to_string(),
        }
    }

    fn connector_status_label(connector: &AppInfo) -> &'static str {
        if connector.is_accessible {
            if connector.is_enabled {
                "Installed"
            } else {
                "Installed · Disabled"
            }
        } else {
            "Can be installed"
        }
    }

    fn connector_description(connector: &AppInfo) -> Option<String> {
        connector
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_string)
    }

    /// Forward file-search results to the bottom pane.
    pub fn apply_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        self.bottom_pane.on_file_search_result(query, matches);
    }

    pub fn token_usage(&self) -> TokenUsage {
        self.token_info
            .as_ref()
            .map(|ti| ti.total_token_usage.clone())
            .unwrap_or_default()
    }

    pub fn process_id(&self) -> Option<ProcessId> {
        self.process_id
    }

    pub fn process_name(&self) -> Option<String> {
        self.process_name.clone()
    }

    /// Returns a cache key describing the current in-flight active cell for the transcript overlay.
    ///
    /// `Ctrl+T` renders committed transcript cells plus a render-only live tail derived from the
    /// current active cell, and the overlay caches that tail; this key is what it uses to decide
    /// whether it must recompute. When there is no active cell, this returns `None` so the overlay
    /// can drop the tail entirely.
    ///
    /// If callers mutate the active cell's transcript output without bumping the revision (or
    /// providing an appropriate animation tick), the overlay will keep showing a stale tail while
    /// the main viewport updates.
    pub fn active_cell_transcript_key(&self) -> Option<ActiveCellTranscriptKey> {
        let cell = self.active_cell.as_ref()?;
        Some(ActiveCellTranscriptKey {
            revision: self.active_cell_revision,
            is_stream_continuation: cell.is_stream_continuation(),
            animation_tick: cell.transcript_animation_tick(),
        })
    }

    /// Returns the active cell's transcript lines for a given terminal width.
    ///
    /// This is a convenience for the transcript overlay live-tail path, and it intentionally
    /// filters out empty results so the overlay can treat "nothing to render" as "no tail". Callers
    /// should pass the same width the overlay uses; using a different width will cause wrapping
    /// mismatches between the main viewport and the transcript overlay.
    pub fn active_cell_transcript_lines(&self, width: u16) -> Option<Vec<Line<'static>>> {
        let cell = self.active_cell.as_ref()?;
        let lines = cell.transcript_lines(width);
        (!lines.is_empty()).then_some(lines)
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn status_line_text(&self) -> Option<String> {
        self.bottom_pane.status_line_text()
    }

    pub fn clear_token_usage(&mut self) {
        self.token_info = None;
    }
}
