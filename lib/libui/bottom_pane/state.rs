//! State mutation and accessor methods for `BottomPane`.
//!
//! All `impl BottomPane` and `impl Renderable for BottomPane` blocks live here
//! to keep the parent module file focused on type definitions, constants, and
//! re-exports.

use super::*;

impl BottomPane {
    pub fn new(params: BottomPaneParams) -> Self {
        let BottomPaneParams {
            app_event_tx,
            frame_requester,
            has_input_focus,
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
            animations_enabled,
        } = params;
        let mut composer = ChatComposer::new(
            has_input_focus,
            app_event_tx.clone(),
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
        );
        composer.set_frame_requester(frame_requester.clone());
        Self {
            composer,
            view_stack: Vec::new(),
            app_event_tx,
            frame_requester,
            has_input_focus,
            enhanced_keys_supported,
            disable_paste_burst,
            is_task_running: false,
            status: None,
            unified_exec_footer: UnifiedExecFooter::new(),
            pending_input_preview: PendingInputPreview::new(),
            pending_process_approvals: PendingProcessApprovals::new(),
            esc_backtrack_hint: false,
            animations_enabled,
            context_window_percent: None,
            context_window_used_tokens: None,
        }
    }

    /// Update image-paste behavior for the active composer and repaint immediately.
    ///
    /// Callers use this to keep composer affordances aligned with model capabilities.
    pub fn set_image_paste_enabled(&mut self, enabled: bool) {
        self.composer.set_image_paste_enabled(enabled);
        self.request_redraw();
    }

    pub fn set_connectors_snapshot(&mut self, snapshot: Option<ConnectorsSnapshot>) {
        self.composer.set_connector_mentions(snapshot);
        self.request_redraw();
    }

    pub fn take_mention_bindings(&mut self) -> Vec<MentionBinding> {
        self.composer.take_mention_bindings()
    }

    pub fn take_recent_submission_mention_bindings(&mut self) -> Vec<MentionBinding> {
        self.composer.take_recent_submission_mention_bindings()
    }

    /// Clear pending attachments and mention bindings e.g. when a slash command doesn't submit text.
    pub fn drain_pending_submission_state(&mut self) {
        let _ = self.take_recent_submission_images_with_placeholders();
        let _ = self.take_remote_image_urls();
        let _ = self.take_recent_submission_mention_bindings();
        let _ = self.take_mention_bindings();
    }

    pub fn set_collaboration_modes_enabled(&mut self, enabled: bool) {
        self.composer.set_collaboration_modes_enabled(enabled);
        self.request_redraw();
    }

    pub fn set_connectors_enabled(&mut self, enabled: bool) {
        self.composer.set_connectors_enabled(enabled);
    }

    pub fn set_collaboration_mode_indicator(
        &mut self,
        indicator: Option<CollaborationModeIndicator>,
    ) {
        self.composer.set_collaboration_mode_indicator(indicator);
        self.request_redraw();
    }

    pub fn set_personality_command_enabled(&mut self, enabled: bool) {
        self.composer.set_personality_command_enabled(enabled);
        self.request_redraw();
    }

    /// Update the key hint shown next to queued messages so it matches the
    /// binding that `ChatWidget` actually listens for.
    pub fn set_queued_message_edit_binding(&mut self, binding: KeyBinding) {
        self.pending_input_preview.set_edit_binding(binding);
        self.request_redraw();
    }

    pub fn status_widget(&self) -> Option<&StatusIndicatorWidget> {
        self.status.as_ref()
    }

    #[cfg(test)]
    pub fn context_window_percent(&self) -> Option<i64> {
        self.context_window_percent
    }

    #[cfg(test)]
    pub fn context_window_used_tokens(&self) -> Option<i64> {
        self.context_window_used_tokens
    }

    fn active_view(&self) -> Option<&dyn BottomPaneView> {
        self.view_stack.last().map(std::convert::AsRef::as_ref)
    }

    pub(super) fn push_view(&mut self, view: Box<dyn BottomPaneView>) {
        self.view_stack.push(view);
        self.request_redraw();
    }

    /// Forward a key event to the active view or the composer.
    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> InputResult {
        // If a modal/view is active, handle it here; otherwise forward to composer.
        if !self.view_stack.is_empty() {
            if key_event.kind == KeyEventKind::Release {
                return InputResult::None;
            }

            // We need three pieces of information after routing the key:
            // whether Esc completed the view, whether the view finished for any
            // reason, and whether a paste-burst timer should be scheduled.
            let (ctrl_c_completed, view_complete, view_in_paste_burst) = {
                let last_index = self.view_stack.len() - 1;
                let view = &mut self.view_stack[last_index];
                let prefer_esc =
                    key_event.code == KeyCode::Esc && view.prefer_esc_to_handle_key_event();
                let ctrl_c_completed = key_event.code == KeyCode::Esc
                    && !prefer_esc
                    && matches!(view.on_ctrl_c(), CancellationEvent::Handled)
                    && view.is_complete();
                if ctrl_c_completed {
                    (true, true, false)
                } else {
                    view.handle_key_event(key_event);
                    (false, view.is_complete(), view.is_in_paste_burst())
                }
            };

            if ctrl_c_completed {
                self.view_stack.pop();
                self.on_active_view_complete();
                if let Some(next_view) = self.view_stack.last()
                    && next_view.is_in_paste_burst()
                {
                    self.request_redraw_in(ChatComposer::recommended_paste_flush_delay());
                }
            } else if view_complete {
                self.view_stack.clear();
                self.on_active_view_complete();
            } else if view_in_paste_burst {
                self.request_redraw_in(ChatComposer::recommended_paste_flush_delay());
            }
            self.request_redraw();
            InputResult::None
        } else {
            let is_agent_command = self
                .composer_text()
                .lines()
                .next()
                .and_then(parse_slash_name)
                .is_some_and(|(name, _, _)| name == "agent");

            // If a task is running and a status line is visible, allow Esc to
            // send an interrupt even while the composer has focus.
            // When a popup is active, prefer dismissing it over interrupting the task.
            if key_event.code == KeyCode::Esc
                && matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                && self.is_task_running
                && !is_agent_command
                && !self.composer.popup_active()
                && let Some(status) = &self.status
            {
                // Send Op::Interrupt
                status.interrupt();
                self.request_redraw();
                return InputResult::None;
            }
            let (input_result, needs_redraw) = self.composer.handle_key_event(key_event);
            if needs_redraw {
                self.request_redraw();
            }
            if self.composer.is_in_paste_burst() {
                self.request_redraw_in(ChatComposer::recommended_paste_flush_delay());
            }
            input_result
        }
    }

    /// Handles a Ctrl+C press within the bottom pane.
    ///
    /// An active modal view is given the first chance to consume the key (typically to dismiss
    /// itself). If no view is active, Ctrl+C clears draft composer input.
    ///
    /// This method may show the quit shortcut hint as a user-visible acknowledgement that Ctrl+C
    /// was received, but it does not decide whether the process should exit; `ChatWidget` owns the
    /// quit/interrupt state machine and uses the result to decide what happens next.
    pub fn on_ctrl_c(&mut self) -> CancellationEvent {
        if let Some(view) = self.view_stack.last_mut() {
            let event = view.on_ctrl_c();
            if matches!(event, CancellationEvent::Handled) {
                if view.is_complete() {
                    self.view_stack.pop();
                    self.on_active_view_complete();
                }
                self.show_quit_shortcut_hint(key_hint::ctrl(KeyCode::Char('c')));
                self.request_redraw();
            }
            event
        } else if self.composer_is_empty() {
            CancellationEvent::NotHandled
        } else {
            self.view_stack.pop();
            self.clear_composer_for_ctrl_c();
            self.show_quit_shortcut_hint(key_hint::ctrl(KeyCode::Char('c')));
            self.request_redraw();
            CancellationEvent::Handled
        }
    }

    pub fn handle_paste(&mut self, pasted: String) {
        if let Some(view) = self.view_stack.last_mut() {
            let needs_redraw = view.handle_paste(pasted);
            if view.is_complete() {
                self.on_active_view_complete();
            }
            if needs_redraw {
                self.request_redraw();
            }
        } else {
            let needs_redraw = self.composer.handle_paste(pasted);
            self.composer.sync_popups();
            if needs_redraw {
                self.request_redraw();
            }
        }
    }

    pub fn insert_str(&mut self, text: &str) {
        self.composer.insert_str(text);
        self.composer.sync_popups();
        self.request_redraw();
    }

    pub fn pre_draw_tick(&mut self) {
        self.composer.sync_popups();
    }

    /// Replace the composer text with `text`.
    ///
    /// This is intended for fresh input where mention linkage does not need to
    /// survive; it routes to `ChatComposer::set_text_content`, which resets
    /// mention bindings.
    pub fn set_composer_text(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
    ) {
        self.composer
            .set_text_content(text, text_elements, local_image_paths);
        self.composer.move_cursor_to_end();
        self.request_redraw();
    }

    /// Replace the composer text while preserving mention link targets.
    ///
    /// Use this when rehydrating a draft after a local validation/gating
    /// failure (for example unsupported image submit) so previously selected
    /// mention targets remain stable across retry.
    pub fn set_composer_text_with_mention_bindings(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
        mention_bindings: Vec<MentionBinding>,
    ) {
        self.composer.set_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
        self.request_redraw();
    }

    #[allow(dead_code)]
    pub fn set_composer_input_enabled(&mut self, enabled: bool, placeholder: Option<String>) {
        self.composer.set_input_enabled(enabled, placeholder);
        self.request_redraw();
    }

    pub fn clear_composer_for_ctrl_c(&mut self) {
        self.composer.clear_for_ctrl_c();
        self.request_redraw();
    }

    /// Get the current composer text (for tests and programmatic checks).
    pub fn composer_text(&self) -> String {
        self.composer.current_text()
    }

    pub fn composer_text_elements(&self) -> Vec<TextElement> {
        self.composer.text_elements()
    }

    pub fn composer_local_images(&self) -> Vec<LocalImageAttachment> {
        self.composer.local_images()
    }

    pub fn composer_mention_bindings(&self) -> Vec<MentionBinding> {
        self.composer.mention_bindings()
    }

    #[cfg(test)]
    pub fn composer_local_image_paths(&self) -> Vec<PathBuf> {
        self.composer.local_image_paths()
    }

    pub fn composer_text_with_pending(&self) -> String {
        self.composer.current_text_with_pending()
    }

    pub fn composer_pending_pastes(&self) -> Vec<(String, String)> {
        self.composer.pending_pastes()
    }

    pub fn apply_external_edit(&mut self, text: String) {
        self.composer.apply_external_edit(text);
        self.request_redraw();
    }

    pub fn set_footer_hint_override(&mut self, items: Option<Vec<(String, String)>>) {
        self.composer.set_footer_hint_override(items);
        self.request_redraw();
    }

    pub fn set_remote_image_urls(&mut self, urls: Vec<String>) {
        self.composer.set_remote_image_urls(urls);
        self.request_redraw();
    }

    pub fn remote_image_urls(&self) -> Vec<String> {
        self.composer.remote_image_urls()
    }

    pub fn take_remote_image_urls(&mut self) -> Vec<String> {
        let urls = self.composer.take_remote_image_urls();
        self.request_redraw();
        urls
    }

    pub fn set_composer_pending_pastes(&mut self, pending_pastes: Vec<(String, String)>) {
        self.composer.set_pending_pastes(pending_pastes);
        self.request_redraw();
    }

    /// Update the status indicator header (defaults to "Working") and details below it.
    ///
    /// Passing `None` clears any existing details. No-ops if the status indicator is not active.
    pub fn update_status(
        &mut self,
        header: String,
        details: Option<String>,
        details_capitalization: StatusDetailsCapitalization,
        details_max_lines: usize,
    ) {
        if let Some(status) = self.status.as_mut() {
            status.update_header(header);
            status.update_details(details, details_capitalization, details_max_lines.max(1));
            self.request_redraw();
        }
    }

    /// Show the transient "press again to quit" hint for `key`.
    ///
    /// `ChatWidget` owns the quit shortcut state machine (it decides when quit is
    /// allowed), while the bottom pane owns rendering. We also schedule a redraw
    /// after [`QUIT_SHORTCUT_TIMEOUT`] so the hint disappears even if the user
    /// stops typing and no other events trigger a draw.
    pub fn show_quit_shortcut_hint(&mut self, key: KeyBinding) {
        if !DOUBLE_PRESS_QUIT_SHORTCUT_ENABLED {
            return;
        }

        self.composer
            .show_quit_shortcut_hint(key, self.has_input_focus);
        let frame_requester = self.frame_requester.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tokio::time::sleep(QUIT_SHORTCUT_TIMEOUT).await;
                frame_requester.schedule_frame();
            });
        } else {
            // In tests (and other non-Tokio contexts), fall back to a thread so
            // the hint can still expire without requiring an explicit draw.
            std::thread::spawn(move || {
                std::thread::sleep(QUIT_SHORTCUT_TIMEOUT);
                frame_requester.schedule_frame();
            });
        }
        self.request_redraw();
    }

    /// Clear the "press again to quit" hint immediately.
    pub fn clear_quit_shortcut_hint(&mut self) {
        self.composer.clear_quit_shortcut_hint(self.has_input_focus);
        self.request_redraw();
    }

    #[cfg(test)]
    pub fn quit_shortcut_hint_visible(&self) -> bool {
        self.composer.quit_shortcut_hint_visible()
    }

    #[cfg(test)]
    pub fn status_indicator_visible(&self) -> bool {
        self.status.is_some()
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn status_line_text(&self) -> Option<String> {
        self.composer.status_line_text()
    }

    pub fn show_esc_backtrack_hint(&mut self) {
        self.esc_backtrack_hint = true;
        self.composer.set_esc_backtrack_hint(/*show*/ true);
        self.request_redraw();
    }

    pub fn clear_esc_backtrack_hint(&mut self) {
        if self.esc_backtrack_hint {
            self.esc_backtrack_hint = false;
            self.composer.set_esc_backtrack_hint(/*show*/ false);
            self.request_redraw();
        }
    }

    // esc_backtrack_hint_visible removed; hints are controlled internally.

    pub fn set_task_running(&mut self, running: bool) {
        let was_running = self.is_task_running;
        self.is_task_running = running;
        self.composer.set_task_running(running);

        if running {
            if !was_running {
                if self.status.is_none() {
                    self.status = Some(StatusIndicatorWidget::new(
                        self.app_event_tx.clone(),
                        self.frame_requester.clone(),
                        self.animations_enabled,
                    ));
                }
                if let Some(status) = self.status.as_mut() {
                    status.set_interrupt_hint_visible(/*visible*/ true);
                }
                self.sync_status_inline_message();
                self.request_redraw();
            }
        } else {
            // Hide the status indicator when a task completes, but keep other modal views.
            self.hide_status_indicator();
        }
    }

    /// Hide the status indicator while leaving task-running state untouched.
    pub fn hide_status_indicator(&mut self) {
        if self.status.take().is_some() {
            self.request_redraw();
        }
    }

    pub fn ensure_status_indicator(&mut self) {
        if self.status.is_none() {
            self.status = Some(StatusIndicatorWidget::new(
                self.app_event_tx.clone(),
                self.frame_requester.clone(),
                self.animations_enabled,
            ));
            self.sync_status_inline_message();
            self.request_redraw();
        }
    }

    pub fn set_interrupt_hint_visible(&mut self, visible: bool) {
        if let Some(status) = self.status.as_mut() {
            status.set_interrupt_hint_visible(visible);
            self.request_redraw();
        }
    }

    pub fn set_context_window(&mut self, percent: Option<i64>, used_tokens: Option<i64>) {
        if self.context_window_percent == percent && self.context_window_used_tokens == used_tokens
        {
            return;
        }

        self.context_window_percent = percent;
        self.context_window_used_tokens = used_tokens;
        self.composer
            .set_context_window(percent, self.context_window_used_tokens);
        self.request_redraw();
    }

    /// Show a generic list selection view with the provided items.
    pub fn show_selection_view(&mut self, params: list_selection_view::SelectionViewParams) {
        let view = list_selection_view::ListSelectionView::new(params, self.app_event_tx.clone());
        self.push_view(Box::new(view));
    }

    /// Replace the active selection view when it matches `view_id`.
    pub fn replace_selection_view_if_active(
        &mut self,
        view_id: &'static str,
        params: list_selection_view::SelectionViewParams,
    ) -> bool {
        let is_match = self
            .view_stack
            .last()
            .is_some_and(|view| view.view_id() == Some(view_id));
        if !is_match {
            return false;
        }

        self.view_stack.pop();
        let view = list_selection_view::ListSelectionView::new(params, self.app_event_tx.clone());
        self.push_view(Box::new(view));
        true
    }

    pub fn selected_index_for_active_view(&self, view_id: &'static str) -> Option<usize> {
        self.view_stack
            .last()
            .filter(|view| view.view_id() == Some(view_id))
            .and_then(|view| view.selected_index())
    }

    /// Update the pending-input preview shown above the composer.
    pub fn set_pending_input_preview(&mut self, queued: Vec<String>, pending_steers: Vec<String>) {
        self.pending_input_preview.pending_steers = pending_steers;
        self.pending_input_preview.queued_messages = queued;
        self.request_redraw();
    }

    /// Update the inactive-thread approval list shown above the composer.
    pub fn set_pending_process_approvals(&mut self, processes: Vec<String>) {
        if self.pending_process_approvals.set_processes(processes) {
            self.request_redraw();
        }
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn pending_process_approvals(&self) -> &[String] {
        self.pending_process_approvals.processes()
    }

    /// Update the unified-exec process set and refresh whichever summary surface is active.
    ///
    /// The summary may be displayed inline in the status row or as a dedicated
    /// footer row depending on whether a status indicator is currently visible.
    pub fn set_unified_exec_processes(&mut self, processes: Vec<String>) {
        if self.unified_exec_footer.set_processes(processes) {
            self.sync_status_inline_message();
            self.request_redraw();
        }
    }

    /// Copy unified-exec summary text into the active status row, if any.
    ///
    /// This keeps status-line inline text synchronized without forcing the
    /// standalone unified-exec footer row to be visible.
    fn sync_status_inline_message(&mut self) {
        if let Some(status) = self.status.as_mut() {
            status.update_inline_message(self.unified_exec_footer.summary_text());
        }
    }

    /// Update custom prompts available for the slash popup.
    pub fn set_custom_prompts(&mut self, prompts: Vec<CustomPrompt>) {
        self.composer.set_custom_prompts(prompts);
        self.request_redraw();
    }

    pub fn composer_is_empty(&self) -> bool {
        self.composer.is_empty()
    }

    pub fn is_task_running(&self) -> bool {
        self.is_task_running
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn has_active_view(&self) -> bool {
        !self.view_stack.is_empty()
    }

    /// Return true when the pane is in the regular composer state without any
    /// overlays or popups and not running a task. This is the safe context to
    /// use Esc-Esc for backtracking from the main view.
    pub fn is_normal_backtrack_mode(&self) -> bool {
        !self.is_task_running && self.view_stack.is_empty() && !self.composer.popup_active()
    }

    /// Return true when no popups or modal views are active, regardless of task state.
    pub fn can_launch_external_editor(&self) -> bool {
        self.view_stack.is_empty() && !self.composer.popup_active()
    }

    /// Returns true when the bottom pane has no active modal view and no active composer popup.
    ///
    /// This is the UI-level definition of "no modal/popup is active" for key routing decisions.
    /// It intentionally does not include task state, since some actions are safe while a task is
    /// running and some are not.
    pub fn no_modal_or_popup_active(&self) -> bool {
        self.can_launch_external_editor()
    }

    pub fn show_view(&mut self, view: Box<dyn BottomPaneView>) {
        self.push_view(view);
    }

    /// Called when the agent requests user approval.
    pub fn push_approval_request(&mut self, request: ApprovalRequest, features: &Features) {
        let request = if let Some(view) = self.view_stack.last_mut() {
            match view.try_consume_approval_request(request) {
                Some(request) => request,
                None => {
                    self.request_redraw();
                    return;
                }
            }
        } else {
            request
        };

        // Otherwise create a new approval modal overlay.
        let modal = ApprovalOverlay::new(request, self.app_event_tx.clone(), features.clone());
        self.pause_status_timer_for_modal();
        self.push_view(Box::new(modal));
    }

    /// Called when the agent requests user input.
    pub fn push_user_input_request(&mut self, request: RequestUserInputEvent) {
        let request = if let Some(view) = self.view_stack.last_mut() {
            match view.try_consume_user_input_request(request) {
                Some(request) => request,
                None => {
                    self.request_redraw();
                    return;
                }
            }
        } else {
            request
        };

        let modal = RequestUserInputOverlay::new(
            request,
            self.app_event_tx.clone(),
            self.has_input_focus,
            self.enhanced_keys_supported,
            self.disable_paste_burst,
        );
        self.pause_status_timer_for_modal();
        self.set_composer_input_enabled(
            /*enabled*/ false,
            Some("Answer the questions to continue.".to_string()),
        );
        self.push_view(Box::new(modal));
    }

    pub fn push_mcp_server_elicitation_request(
        &mut self,
        request: McpServerElicitationFormRequest,
    ) {
        let request = if let Some(view) = self.view_stack.last_mut() {
            match view.try_consume_mcp_server_elicitation_request(request) {
                Some(request) => request,
                None => {
                    self.request_redraw();
                    return;
                }
            }
        } else {
            request
        };

        if let Some(tool_suggestion) = request.tool_suggestion() {
            let suggestion_type = match tool_suggestion.suggest_type {
                mcp_server_elicitation::ToolSuggestionType::Install => {
                    AppLinkSuggestionType::Install
                }
                mcp_server_elicitation::ToolSuggestionType::Enable => AppLinkSuggestionType::Enable,
            };
            let is_installed = matches!(
                tool_suggestion.suggest_type,
                mcp_server_elicitation::ToolSuggestionType::Enable
            );
            let view = AppLinkView::new(
                AppLinkViewParams {
                    app_id: tool_suggestion.tool_id.clone(),
                    title: tool_suggestion.tool_name.clone(),
                    description: None,
                    instructions: match suggestion_type {
                        AppLinkSuggestionType::Install => {
                            "Install this app in your browser, then return here.".to_string()
                        }
                        AppLinkSuggestionType::Enable => {
                            "Enable this app to use it for the current request.".to_string()
                        }
                    },
                    url: tool_suggestion.install_url.clone(),
                    is_installed,
                    is_enabled: false,
                    suggest_reason: Some(tool_suggestion.suggest_reason.clone()),
                    suggestion_type: Some(suggestion_type),
                    elicitation_target: Some(AppLinkElicitationTarget {
                        process_id: request.process_id(),
                        server_name: request.server_name().to_string(),
                        request_id: request.request_id().clone(),
                    }),
                },
                self.app_event_tx.clone(),
            );
            self.pause_status_timer_for_modal();
            self.set_composer_input_enabled(
                /*enabled*/ false,
                Some("Respond to the tool suggestion to continue.".to_string()),
            );
            self.push_view(Box::new(view));
            return;
        }

        let modal = McpServerElicitationOverlay::new(
            request,
            self.app_event_tx.clone(),
            self.has_input_focus,
            self.enhanced_keys_supported,
            self.disable_paste_burst,
        );
        self.pause_status_timer_for_modal();
        self.set_composer_input_enabled(
            /*enabled*/ false,
            Some("Respond to the MCP server request to continue.".to_string()),
        );
        self.push_view(Box::new(modal));
    }

    fn on_active_view_complete(&mut self) {
        self.resume_status_timer_after_modal();
        self.set_composer_input_enabled(/*enabled*/ true, /*placeholder*/ None);
    }

    fn pause_status_timer_for_modal(&mut self) {
        if let Some(status) = self.status.as_mut() {
            status.pause_timer();
        }
    }

    fn resume_status_timer_after_modal(&mut self) {
        if let Some(status) = self.status.as_mut() {
            status.resume_timer();
        }
    }

    /// Height (terminal rows) required by the current bottom pane.
    pub fn request_redraw(&self) {
        self.frame_requester.schedule_frame();
    }

    pub fn request_redraw_in(&self, dur: Duration) {
        self.frame_requester.schedule_frame_in(dur);
    }

    // --- History helpers ---

    pub fn set_history_metadata(&mut self, log_id: u64, entry_count: usize) {
        self.composer.set_history_metadata(log_id, entry_count);
    }

    pub fn flush_paste_burst_if_due(&mut self) -> bool {
        // Give the active view the first chance to flush paste-burst state so
        // overlays that reuse the composer behave consistently.
        if let Some(view) = self.view_stack.last_mut()
            && view.flush_paste_burst_if_due()
        {
            return true;
        }
        self.composer.flush_paste_burst_if_due()
    }

    pub fn is_in_paste_burst(&self) -> bool {
        // A view can hold paste-burst state independently of the primary
        // composer, so check it first.
        self.view_stack
            .last()
            .is_some_and(|view| view.is_in_paste_burst())
            || self.composer.is_in_paste_burst()
    }

    pub fn on_history_entry_response(&mut self, log_id: u64, offset: usize, entry: Option<String>) {
        let updated = self
            .composer
            .on_history_entry_response(log_id, offset, entry);

        if updated {
            self.composer.sync_popups();
            self.request_redraw();
        }
    }

    pub fn on_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        self.composer.on_file_search_result(query, matches);
        self.request_redraw();
    }

    pub fn attach_image(&mut self, path: PathBuf) {
        if self.view_stack.is_empty() {
            self.composer.attach_image(path);
            self.request_redraw();
        }
    }

    #[cfg(test)]
    pub fn take_recent_submission_images(&mut self) -> Vec<PathBuf> {
        self.composer.take_recent_submission_images()
    }

    pub fn take_recent_submission_images_with_placeholders(&mut self) -> Vec<LocalImageAttachment> {
        self.composer
            .take_recent_submission_images_with_placeholders()
    }

    pub fn prepare_inline_args_submission(
        &mut self,
        record_history: bool,
    ) -> Option<(String, Vec<TextElement>)> {
        self.composer.prepare_inline_args_submission(record_history)
    }

    fn as_renderable(&'_ self) -> RenderableItem<'_> {
        if let Some(view) = self.active_view() {
            RenderableItem::Borrowed(view)
        } else {
            let mut flex = FlexRenderable::new();
            if let Some(status) = &self.status {
                flex.push(/*flex*/ 0, RenderableItem::Borrowed(status));
            }
            // Avoid double-surfacing the same summary and avoid adding an extra
            // row while the status line is already visible.
            if self.status.is_none() && !self.unified_exec_footer.is_empty() {
                flex.push(
                    /*flex*/ 0,
                    RenderableItem::Borrowed(&self.unified_exec_footer),
                );
            }
            let has_pending_process_approvals = !self.pending_process_approvals.is_empty();
            let has_pending_input = !self.pending_input_preview.queued_messages.is_empty()
                || !self.pending_input_preview.pending_steers.is_empty();
            let has_status_or_footer =
                self.status.is_some() || !self.unified_exec_footer.is_empty();
            let has_inline_previews = has_pending_process_approvals || has_pending_input;
            if has_inline_previews && has_status_or_footer {
                flex.push(/*flex*/ 0, RenderableItem::Owned("".into()));
            }
            flex.push(
                /*flex*/ 1,
                RenderableItem::Borrowed(&self.pending_process_approvals),
            );
            if has_pending_process_approvals && has_pending_input {
                flex.push(/*flex*/ 0, RenderableItem::Owned("".into()));
            }
            flex.push(
                /*flex*/ 1,
                RenderableItem::Borrowed(&self.pending_input_preview),
            );
            if !has_inline_previews && has_status_or_footer {
                flex.push(/*flex*/ 0, RenderableItem::Owned("".into()));
            }
            let mut flex2 = FlexRenderable::new();
            flex2.push(/*flex*/ 1, RenderableItem::Owned(flex.into()));
            flex2.push(/*flex*/ 0, RenderableItem::Borrowed(&self.composer));
            RenderableItem::Owned(Box::new(flex2))
        }
    }

    pub fn set_status_line(&mut self, status_line: Option<Line<'static>>) {
        if self.composer.set_status_line(status_line) {
            self.request_redraw();
        }
    }

    pub fn set_status_line_enabled(&mut self, enabled: bool) {
        if self.composer.set_status_line_enabled(enabled) {
            self.request_redraw();
        }
    }

    /// Updates the contextual footer label and requests a redraw only when it changed.
    ///
    /// This keeps the footer plumbing cheap during thread transitions where `App` may recompute
    /// the label several times while the visible thread settles.
    pub fn set_active_agent_label(&mut self, active_agent_label: Option<String>) {
        if self.composer.set_active_agent_label(active_agent_label) {
            self.request_redraw();
        }
    }
}

impl Renderable for BottomPane {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.as_renderable().render(area, buf);
    }
    fn desired_height(&self, width: u16) -> u16 {
        self.as_renderable().desired_height(width)
    }
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.as_renderable().cursor_pos(area)
    }
}
